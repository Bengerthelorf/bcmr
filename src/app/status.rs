use crate::commands;
use crate::config::is_json_mode;

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
