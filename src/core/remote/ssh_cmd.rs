use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::Command;

pub(super) static SSH_COMPRESS: AtomicBool = AtomicBool::new(false);

pub(super) fn control_path(target: &str) -> String {
    let dir = std::env::temp_dir().join("bcmr-ssh");
    let _ = std::fs::create_dir_all(&dir);
    // Hash to (a) avoid `a@b` vs `a:b` sanitizing to the same name and
    // (b) keep the path inside macOS's 104-byte sun_path budget — OpenSSH
    // appends ~17 chars (`.XXXXXXXXXXXXXXXX`) for the MUX listener, and
    // $TMPDIR on macOS can be ~50 chars by itself. 16 hex (64 bits) leaves
    // a comfortable buffer while still giving ~2^32 distinct targets
    // before a 50% birthday collision — far past anything personal use hits.
    let digest = blake3::hash(target.as_bytes());
    let short = &digest.to_hex()[..16];
    dir.join(format!("{}.sock", short))
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
    let mut args = if std::env::var_os("BCMR_SSH_NO_MULTIPLEX").is_some() {
        vec![
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "ControlPath=none".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
        ]
    } else {
        let cp = control_path(target);
        vec![
            "-o".into(),
            format!("ControlPath={}", cp),
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            "ControlPersist=300".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
        ]
    };
    if !is_interactive() {
        args.extend(["-o".into(), "BatchMode=yes".into()]);
    }
    if SSH_COMPRESS.load(Ordering::Relaxed) {
        args.extend(["-o".into(), "Compression=yes".into()]);
    }
    args
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

pub(crate) fn ssh_error_message(stderr: &str, context: &str) -> String {
    let stderr_lower = stderr.to_lowercase();
    if stderr_lower.contains("session open refused")
        || stderr_lower.contains("mux_client_request_session")
    {
        format!(
            "{}: SSH session open refused — the host's MaxSessions limit \
             (default 10) is exhausted by parallel bcmr processes. \
             Use `bcmr copy --to host:a --to host:b ... /src` for fan-out \
             from a single process (one TCP connection, server-side limits \
             apply once), or raise sshd's MaxSessions on the remote",
            context
        )
    } else if stderr_lower.contains("connection refused") {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_path_distinguishes_collision_pairs() {
        assert_ne!(control_path("user@host"), control_path("user:host"));
        assert_ne!(control_path("a@b"), control_path("a:b"));
        assert_ne!(control_path("a@b"), control_path("a/b"));
    }

    #[test]
    fn control_path_fits_unix_sun_path_with_mux_suffix() {
        // OpenSSH appends `.XXXXXXXXXXXXXXXX` (17 chars) to the configured
        // ControlPath when opening the MUX listener. macOS sun_path is 104
        // bytes — the tightest mainstream Unix; Linux is 108. Use the macOS
        // budget so the assertion holds on every supported platform.
        const SUN_PATH_LIMIT: usize = 104;
        const OPENSSH_MUX_SUFFIX: usize = 17;
        for target in [
            "host",
            "user@host",
            "user@host:port",
            "verylonguser_name@subdomain.example.com",
        ] {
            let path = control_path(target);
            let total = path.len() + OPENSSH_MUX_SUFFIX;
            assert!(
                total <= SUN_PATH_LIMIT,
                "control_path({target:?}) + mux suffix = {total} bytes, \
                 exceeds sun_path budget {SUN_PATH_LIMIT}: {path}"
            );
        }
    }
}
