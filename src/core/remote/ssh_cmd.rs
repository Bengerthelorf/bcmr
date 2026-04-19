use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::Command;

pub(super) static SSH_COMPRESS: AtomicBool = AtomicBool::new(false);

pub(super) fn control_path(target: &str) -> String {
    let dir = std::env::temp_dir().join("bcmr-ssh");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{}.sock", target.replace(['@', ':', '/'], "_")))
        .to_string_lossy()
        .to_string()
}

pub(super) fn is_interactive() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(libc::STDIN_FILENO) != 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(super) fn ssh_base_args(target: &str) -> Vec<String> {
    let cp = control_path(target);
    let mut args = vec![
        "-o".into(),
        format!("ControlPath={}", cp),
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        "ControlPersist=300".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if !is_interactive() {
        args.extend(["-o".into(), "BatchMode=yes".into()]);
    }
    if SSH_COMPRESS.load(Ordering::Relaxed) {
        args.extend(["-o".into(), "Compression=yes".into()]);
    }
    args
}

pub(super) fn ssh_base_args_for_worker(target: &str, worker_id: usize) -> Vec<String> {
    let dir = std::env::temp_dir().join("bcmr-ssh");
    let _ = std::fs::create_dir_all(&dir);
    let cp = dir
        .join(format!(
            "w{}_{}.sock",
            worker_id,
            target.replace(['@', ':', '/'], "_")
        ))
        .to_string_lossy()
        .to_string();

    let mut args = vec![
        "-o".into(),
        format!("ControlPath={}", cp),
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        "ControlPersist=60".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if !is_interactive() {
        args.extend(["-o".into(), "BatchMode=yes".into()]);
    }
    if SSH_COMPRESS.load(Ordering::Relaxed) {
        args.extend(["-o".into(), "Compression=yes".into()]);
    }
    args
}

pub(super) fn ssh_command_for_worker(target: &str, worker_id: usize) -> Command {
    let args = ssh_base_args_for_worker(target, worker_id);
    let mut cmd = Command::new("ssh");
    for arg in &args {
        cmd.arg(arg);
    }
    cmd.arg(target);
    cmd
}

pub(super) fn make_ssh_cmd(target: &str, worker_id: Option<usize>) -> Command {
    match worker_id {
        Some(id) => ssh_command_for_worker(target, id),
        None => ssh_command(target),
    }
}

pub(super) fn ssh_command(target: &str) -> Command {
    let args = ssh_base_args(target);
    let mut cmd = Command::new("ssh");
    for arg in &args {
        cmd.arg(arg);
    }
    cmd.arg(target);
    cmd
}

pub(super) fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
}

pub(super) fn ssh_error_message(stderr: &str, context: &str) -> String {
    let stderr_lower = stderr.to_lowercase();
    if stderr_lower.contains("connection refused") {
        format!(
            "{}: SSH connection refused (is sshd running on the host?)",
            context
        )
    } else if stderr_lower.contains("no route to host")
        || stderr_lower.contains("network is unreachable")
    {
        format!("{}: host unreachable (check network connectivity)", context)
    } else if stderr_lower.contains("permission denied") {
        format!(
            "{}: SSH authentication failed (check credentials/keys)",
            context
        )
    } else if stderr_lower.contains("could not resolve")
        || stderr_lower.contains("name or service not known")
    {
        format!("{}: unknown host (check hostname)", context)
    } else if stderr_lower.contains("no such file") || stderr_lower.contains("not a regular file") {
        format!("{}: remote file not found", context)
    } else if stderr_lower.contains("timed out") || stderr_lower.contains("connection timed out") {
        format!("{}: SSH connection timed out", context)
    } else {
        format!("{}: {}", context, stderr.trim())
    }
}
