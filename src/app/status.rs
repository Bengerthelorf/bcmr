use crate::commands;
use crate::config::is_json_mode;
use anyhow::{anyhow, Result};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const STARTUP_GRACE: Duration = Duration::from_secs(2);

pub(crate) fn status_detail(latest: &str) -> String {
    serde_json::from_str::<serde_json::Value>(latest)
        .ok()
        .and_then(|v| {
            let ty = v.get("type")?.as_str()?;
            match ty {
                "progress" => v
                    .get("percent")
                    .and_then(|p| p.as_f64())
                    .map(|p| format!("{:.1}%", p)),
                "result" => match v.get("status").and_then(|s| s.as_str()) {
                    Some("success") => Some("complete".to_string()),
                    Some(_) => v
                        .get("error")
                        .and_then(|e| e.as_str())
                        .map(String::from)
                        .or_else(|| Some("error".to_string())),
                    None => None,
                },
                _ => None,
            }
        })
        .unwrap_or_default()
}

pub(crate) fn handle_status_command(job_id: &Option<String>, rm: bool, all: bool, gc: bool) {
    if gc {
        let removed = commands::jobs::cleanup_old_jobs(commands::jobs::DEFAULT_GC_RETENTION_SECS);
        if is_json_mode() {
            println!(
                "{}",
                serde_json::json!({"action": "gc", "removed": removed})
            );
        } else {
            println!("Removed {} old job log(s).", removed);
        }
        return;
    }

    if rm {
        if all {
            let removed = commands::jobs::remove_all_jobs();
            if is_json_mode() {
                println!(
                    "{}",
                    serde_json::json!({"action": "rm", "scope": "all", "removed": removed})
                );
            } else {
                println!("Removed {} job log(s).", removed);
            }
            return;
        }
        let Some(id) = job_id else {
            eprintln!("Error: --rm needs a job id (or pass --all to drop every job)");
            std::process::exit(2);
        };
        match commands::jobs::remove_job(id) {
            Ok(true) => {
                if is_json_mode() {
                    println!(
                        "{}",
                        serde_json::json!({"action": "rm", "job_id": id, "removed": true})
                    );
                } else {
                    println!("Removed job '{}'.", id);
                }
            }
            Ok(false) => {
                eprintln!("Error: job '{}' not found", id);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: cannot remove '{}': {}", id, e);
                std::process::exit(1);
            }
        }
        return;
    }

    match job_id {
        Some(id) => {
            let (state, latest) = match commands::jobs::job_state(id) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            };

            if is_json_mode() {
                let latest_val: serde_json::Value =
                    serde_json::from_str(&latest).unwrap_or(serde_json::Value::Null);
                let wrapper = serde_json::json!({
                    "job_id": id,
                    "state": state.as_str(),
                    "latest": latest_val,
                });
                println!("{}", wrapper);
            } else {
                println!("{}\t{}\t{}", id, state.as_str(), status_detail(&latest));
            }
        }
        None => {
            let jobs = commands::jobs::list_jobs();
            if jobs.is_empty() {
                if !is_json_mode() {
                    println!("No jobs found.");
                }
                return;
            }
            if is_json_mode() {
                let arr: Vec<_> = jobs
                    .iter()
                    .map(|j| {
                        let latest_val: serde_json::Value =
                            serde_json::from_str(&j.latest).unwrap_or(serde_json::Value::Null);
                        serde_json::json!({
                            "job_id": j.id,
                            "state": j.state.as_str(),
                            "latest": latest_val,
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(arr));
            } else {
                for j in &jobs {
                    println!(
                        "{}\t{}\t{}",
                        j.id,
                        j.state.as_str(),
                        status_detail(&j.latest)
                    );
                }
            }
        }
    }
}

pub(crate) async fn watch_job(job_id: &str) -> Result<()> {
    let log = commands::jobs::log_path(job_id);

    let deadline = Instant::now() + STARTUP_GRACE;
    while !log.exists() && Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    if !log.exists() {
        return Err(anyhow!("job '{}' not found", job_id));
    }

    let json = is_json_mode();
    let mut offset = 0u64;
    let mut buffer = String::new();
    let mut header_pid: Option<u32> = None;
    let signal = tokio::signal::ctrl_c();
    tokio::pin!(signal);

    loop {
        let new = read_new_lines(&log, offset, &mut buffer)?;
        offset = new.new_offset;
        for line in new.lines {
            if header_pid.is_none() {
                header_pid = parse_header_pid(&line);
            }
            print_log_line(&line, json);
            if is_terminal_event(&line) {
                return Ok(());
            }
        }
        if let Some(pid) = header_pid {
            if !commands::jobs::is_pid_alive(pid) {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "type": "watch",
                            "status": "writer_dead",
                            "pid": pid,
                        })
                    );
                } else {
                    eprintln!(
                        "Error: job pid {} no longer running and no result event was emitted",
                        pid
                    );
                }
                return Err(anyhow!(
                    "job '{}' writer died without emitting a result event",
                    job_id
                ));
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {},
            _ = &mut signal => {
                if !json {
                    eprintln!();
                    eprintln!("(watch interrupted; job continues in background)");
                }
                return Ok(());
            }
        }
    }
}

fn parse_header_pid(line: &str) -> Option<u32> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("pid")?.as_u64().map(|p| p as u32))
}

struct NewLines {
    lines: Vec<String>,
    new_offset: u64,
}

fn read_new_lines(path: &Path, from: u64, scratch: &mut String) -> Result<NewLines> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    if len <= from {
        return Ok(NewLines {
            lines: Vec::new(),
            new_offset: from,
        });
    }
    f.seek(SeekFrom::Start(from))?;
    scratch.clear();
    f.read_to_string(scratch)?;
    let mut lines = Vec::new();
    let mut consumed = from;
    let mut last_nl = 0;
    for (i, ch) in scratch.char_indices() {
        if ch == '\n' {
            let line = scratch[last_nl..i].trim().to_string();
            if !line.is_empty() {
                lines.push(line);
            }
            last_nl = i + 1;
            consumed = from + last_nl as u64;
        }
    }
    Ok(NewLines {
        lines,
        new_offset: consumed,
    })
}

fn print_log_line(line: &str, json: bool) {
    if json {
        println!("{line}");
        return;
    }
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            println!("{line}");
            return;
        }
    };
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match ty {
        "progress" => {
            let pct = v.get("percent").and_then(|p| p.as_f64()).unwrap_or(0.0);
            let bytes_done = v.get("bytes_done").and_then(|n| n.as_u64()).unwrap_or(0);
            let bytes_total = v.get("bytes_total").and_then(|n| n.as_u64()).unwrap_or(0);
            let speed = v.get("speed_bps").and_then(|n| n.as_u64()).unwrap_or(0);
            let file = v.get("file").and_then(|s| s.as_str()).unwrap_or("");
            println!(
                "{:>5.1}%  {} / {}  {}/s  {}",
                pct,
                crate::ui::utils::format_bytes(bytes_done as f64),
                crate::ui::utils::format_bytes(bytes_total as f64),
                crate::ui::utils::format_bytes(speed as f64),
                file
            );
        }
        "result" => {
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("?");
            let duration = v
                .get("duration_secs")
                .and_then(|n| n.as_f64())
                .unwrap_or(0.0);
            let bytes = v.get("bytes_total").and_then(|n| n.as_u64()).unwrap_or(0);
            let err = v.get("error").and_then(|s| s.as_str());
            match (status, err) {
                ("success", _) => println!(
                    "Done: {} in {:.1}s",
                    crate::ui::utils::format_bytes(bytes as f64),
                    duration
                ),
                (_, Some(msg)) => println!("Error: {} (after {:.1}s)", msg, duration),
                (s, None) => println!("Result: {} (after {:.1}s)", s, duration),
            }
        }
        _ => {
            // Header line ({"job_id":..,"pid":..,"log":..}) and unknown types pass through.
            if let Some(jid) = v.get("job_id").and_then(|s| s.as_str()) {
                println!(
                    "Job {} (pid {})",
                    jid,
                    v.get("pid").and_then(|p| p.as_u64()).unwrap_or(0)
                );
            } else {
                println!("{line}");
            }
        }
    }
}

fn is_terminal_event(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| {
            v.get("type")
                .and_then(|t| t.as_str())
                .map(|t| t == "result")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_terminal_event_recognises_result() {
        assert!(is_terminal_event(
            "{\"type\":\"result\",\"status\":\"success\"}"
        ));
        assert!(!is_terminal_event(
            "{\"type\":\"progress\",\"percent\":50.0}"
        ));
        assert!(!is_terminal_event("{\"job_id\":\"abc\"}"));
        assert!(!is_terminal_event("not json"));
    }

    #[test]
    fn parse_header_pid_extracts_pid_field() {
        assert_eq!(
            parse_header_pid(r#"{"job_id":"abc","pid":4321,"log":"/x"}"#),
            Some(4321)
        );
        assert_eq!(parse_header_pid(r#"{"job_id":"abc"}"#), None);
        assert_eq!(parse_header_pid(r#"{"type":"progress"}"#), None);
        assert_eq!(parse_header_pid("not json"), None);
    }

    #[test]
    fn read_new_lines_returns_appended_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log.jsonl");
        std::fs::write(&p, "{\"type\":\"progress\"}\n{\"type\":\"result\"}\n").unwrap();
        let mut buf = String::new();
        let r1 = read_new_lines(&p, 0, &mut buf).unwrap();
        assert_eq!(r1.lines.len(), 2);
        let r2 = read_new_lines(&p, r1.new_offset, &mut buf).unwrap();
        assert!(r2.lines.is_empty());
        std::fs::write(
            &p,
            "{\"type\":\"progress\"}\n{\"type\":\"result\"}\n{\"type\":\"extra\"}\n",
        )
        .unwrap();
        let r3 = read_new_lines(&p, r1.new_offset, &mut buf).unwrap();
        assert_eq!(r3.lines.len(), 1);
        assert!(r3.lines[0].contains("extra"));
    }
}
