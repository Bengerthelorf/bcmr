use serde::Serialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// `Interrupted` means the process died after the descriptor was
/// written but before any terminal event reached the log (crash,
/// SIGKILL, OOM).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Scanning,
    Running,
    Done,
    Failed,
    Interrupted,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Scanning => "scanning",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Interrupted => "interrupted",
        }
    }
}

pub fn new_job_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let pid = std::process::id();
    format!("{:x}{:x}", ts & 0xFFFF_FFFF, pid & 0xFFFF)
}

pub fn jobs_dir() -> PathBuf {
    let base = directories::BaseDirs::new()
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("bcmr").join("jobs")
}

pub fn log_path(job_id: &str) -> PathBuf {
    jobs_dir().join(format!("{}.jsonl", job_id))
}

pub fn ensure_jobs_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(jobs_dir())
}

#[derive(Serialize)]
pub struct JobInfo {
    pub job_id: String,
    pub pid: u32,
    pub log: String,
}

/// The descriptor line (first line, pid+job_id only) has no `type`
/// field, so it maps to the "no event yet" branch below.
pub fn classify_job(latest_line: &str, pid_alive: bool) -> JobState {
    let v: serde_json::Value = match serde_json::from_str(latest_line) {
        Ok(v) => v,
        Err(_) => {
            return if pid_alive {
                JobState::Scanning
            } else {
                JobState::Interrupted
            };
        }
    };

    let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match event_type {
        "result" => match v.get("status").and_then(|s| s.as_str()) {
            Some("success") => JobState::Done,
            _ => JobState::Failed,
        },
        "progress" => {
            let scanning = v.get("scanning").and_then(|s| s.as_bool()).unwrap_or(false);
            if pid_alive {
                if scanning {
                    JobState::Scanning
                } else {
                    JobState::Running
                }
            } else {
                JobState::Interrupted
            }
        }
        _ => {
            if pid_alive {
                JobState::Scanning
            } else {
                JobState::Interrupted
            }
        }
    }
}

pub fn job_state(job_id: &str) -> Result<(JobState, String), String> {
    let path = log_path(job_id);
    if !path.exists() {
        return Err(format!("job '{}' not found", job_id));
    }
    let content = std::fs::read_to_string(&path).map_err(|e| format!("cannot read log: {}", e))?;

    let pid = content.lines().next().and_then(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        v.get("pid")?.as_u64().map(|p| p as u32)
    });
    let alive = pid.is_some_and(is_pid_alive);

    let latest = content
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();

    Ok((classify_job(&latest, alive), latest))
}

pub fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

pub struct JobEntry {
    pub id: String,
    pub state: JobState,
    pub latest: String,
}

pub fn list_jobs() -> Vec<JobEntry> {
    let dir = jobs_dir();
    let mut jobs = Vec::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return jobs,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".jsonl") {
            continue;
        }
        let job_id = name.trim_end_matches(".jsonl").to_string();

        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let latest = content
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string();

        let pid_alive = content
            .lines()
            .next()
            .and_then(|l| {
                let v: serde_json::Value = serde_json::from_str(l).ok()?;
                v.get("pid")?.as_u64().map(|p| p as u32)
            })
            .is_some_and(is_pid_alive);

        let state = classify_job(&latest, pid_alive);
        jobs.push(JobEntry {
            id: job_id,
            state,
            latest,
        });
    }

    jobs
}

pub fn cleanup_old_jobs(max_age_secs: u64) {
    let dir = jobs_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(max_age_secs);

    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            if mtime < cutoff {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}
