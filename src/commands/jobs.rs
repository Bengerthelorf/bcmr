use serde::Serialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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

pub fn validate_job_id(job_id: &str) -> Result<(), String> {
    if job_id.is_empty()
        || !job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(
            "invalid job ID; use only ASCII letters, digits, hyphens, and underscores".to_string(),
        );
    }
    Ok(())
}

pub fn jobs_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os("BCMR_JOBS_DIR") {
        return PathBuf::from(custom);
    }
    let base = directories::BaseDirs::new()
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("bcmr").join("jobs")
}

pub fn log_path(job_id: &str) -> std::io::Result<PathBuf> {
    log_path_in(&jobs_dir(), job_id)
}

fn log_path_in(dir: &std::path::Path, job_id: &str) -> std::io::Result<PathBuf> {
    validate_job_id(job_id)
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
    Ok(dir.join(format!("{}.jsonl", job_id)))
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
    let path = log_path(job_id).map_err(|e| e.to_string())?;
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

pub fn remove_job(job_id: &str) -> std::io::Result<bool> {
    remove_job_in(&jobs_dir(), job_id)
}

pub fn remove_all_jobs() -> usize {
    remove_all_jobs_in(&jobs_dir())
}

fn remove_job_in(dir: &std::path::Path, job_id: &str) -> std::io::Result<bool> {
    let path = log_path_in(dir, job_id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn remove_all_jobs_in(dir: &std::path::Path) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        // Unlinking mid-write loses data the writer hadn't fsync'd yet.
        if job_is_active(&path) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn job_is_active(log_path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(log_path) else {
        return false;
    };
    let Some(first) = content.lines().next() else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(first) else {
        return false;
    };
    v.get("pid")
        .and_then(|p| p.as_u64())
        .map(|p| is_pid_alive(p as u32))
        .unwrap_or(false)
}

pub const DEFAULT_GC_RETENTION_SECS: u64 = 7 * 24 * 3600;

pub fn cleanup_old_jobs(max_age_secs: u64) -> usize {
    let dir = jobs_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(max_age_secs);

    let mut removed = 0usize;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            if mtime < cutoff && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn jobs_dir_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn test_remove_job_in_deletes_existing_log() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("abc.jsonl"), "{}").unwrap();
        assert!(remove_job_in(dir.path(), "abc").unwrap());
        assert!(!dir.path().join("abc.jsonl").exists());
    }

    #[test]
    fn test_remove_job_in_returns_false_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!remove_job_in(dir.path(), "nope").unwrap());
    }

    #[test]
    fn job_apis_reject_unsafe_ids_without_touching_logs_outside_jobs_dir() {
        let _guard = jobs_dir_env_lock();
        let root = tempfile::tempdir().unwrap();
        let jobs = root.path().join("jobs");
        std::fs::create_dir(&jobs).unwrap();
        let outside = root.path().join("outside.jsonl");
        std::fs::write(
            &outside,
            "{\"pid\":999999}\n{\"type\":\"result\",\"status\":\"success\"}\n",
        )
        .unwrap();

        let previous = std::env::var_os("BCMR_JOBS_DIR");
        std::env::set_var("BCMR_JOBS_DIR", &jobs);
        let results: Vec<_> = [
            "",
            "/absolute",
            "has/slash",
            "has\\backslash",
            ".",
            "..",
            "../outside",
            "\0",
        ]
        .into_iter()
        .map(|id| (id, job_state(id), remove_job(id)))
        .collect();
        match previous {
            Some(value) => std::env::set_var("BCMR_JOBS_DIR", value),
            None => std::env::remove_var("BCMR_JOBS_DIR"),
        }

        for (id, state, removed) in results {
            assert!(state.is_err(), "unsafe ID must not read a log: {id:?}");
            assert!(removed.is_err(), "unsafe ID must not remove a log: {id:?}");
        }
        assert!(outside.exists(), "outside log must remain untouched");
    }

    #[test]
    fn test_remove_all_jobs_in_drops_only_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jsonl"), "{}").unwrap();
        std::fs::write(dir.path().join("b.jsonl"), "{}").unwrap();
        std::fs::write(dir.path().join("readme.txt"), "keep").unwrap();
        let removed = remove_all_jobs_in(dir.path());
        assert_eq!(removed, 2);
        assert!(dir.path().join("readme.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_remove_all_jobs_in_skips_active_pid() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("active.jsonl");
        let stale = dir.path().join("stale.jsonl");
        std::fs::write(
            &active,
            format!("{{\"pid\":{},\"job_id\":\"x\"}}\n", std::process::id()),
        )
        .unwrap();
        std::fs::write(&stale, "{\"pid\":999999,\"job_id\":\"y\"}\n").unwrap();
        let removed = remove_all_jobs_in(dir.path());
        assert_eq!(removed, 1);
        assert!(active.exists(), "active log must survive");
        assert!(!stale.exists(), "stale log must be removed");
    }
}
