use serde::Serialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Generate a short hex job ID based on timestamp + pid.
pub fn new_job_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let pid = std::process::id();
    format!("{:x}{:x}", ts & 0xFFFF_FFFF, pid & 0xFFFF)
}

/// Directory where job log files are stored.
pub fn jobs_dir() -> PathBuf {
    let base = directories::BaseDirs::new()
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("bcmr").join("jobs")
}

/// Full path for a job's log file.
pub fn log_path(job_id: &str) -> PathBuf {
    jobs_dir().join(format!("{}.jsonl", job_id))
}

/// Ensure the jobs directory exists.
pub fn ensure_jobs_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(jobs_dir())
}

#[derive(Serialize)]
pub struct JobInfo {
    pub job_id: String,
    pub pid: u32,
    pub log: String,
}

/// Read the latest line from a job's log file.
pub fn read_latest_status(job_id: &str) -> Result<String, String> {
    let path = log_path(job_id);
    if !path.exists() {
        return Err(format!("job '{}' not found", job_id));
    }

    let content = std::fs::read_to_string(&path).map_err(|e| format!("cannot read log: {}", e))?;

    content
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .ok_or_else(|| "log file is empty".to_string())
}

/// Check if a process is still running.
pub fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// List all jobs (active and recent).
pub fn list_jobs() -> Vec<(String, String, bool)> {
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

        // Find the latest line
        let latest = content
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string();

        // Check if there's a PID line (first line should be job start info)
        let pid_alive = content
            .lines()
            .next()
            .and_then(|l| {
                let v: serde_json::Value = serde_json::from_str(l).ok()?;
                v.get("pid")?.as_u64().map(|p| p as u32)
            })
            .is_some_and(is_pid_alive);

        jobs.push((job_id, latest, pid_alive));
    }

    jobs
}

/// Clean up completed job logs older than max_age_secs.
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
