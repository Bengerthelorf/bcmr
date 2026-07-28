use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::Command;

pub(super) static SSH_COMPRESS: AtomicBool = AtomicBool::new(false);

const MACOS_SUN_PATH_LIMIT: usize = 104;
const OPENSSH_MUX_SUFFIX: usize = 17;

fn control_path_for(temp_dir: &std::path::Path, target: &str) -> String {
    let dir = temp_dir.join("bcmr-ssh");
    // 16 hex (64 bits) — enough to keep `a@b` vs `a:b` distinct, short
    // enough that $TMPDIR + `bcmr-ssh/` + name + `.sock` + OpenSSH's
    // `.XXXXXXXXXXXXXXXX` MUX suffix fits macOS's 104-byte sun_path.
    let digest = blake3::hash(target.as_bytes());
    let short = &digest.to_hex()[..16];
    dir.join(format!("{}.sock", short))
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
fn control_path(target: &str) -> String {
    let temp_dir = std::env::temp_dir();
    let path = control_path_for(&temp_dir, target);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    path
}

fn multiplex_path_fits(path: &str) -> bool {
    path.len().saturating_add(OPENSSH_MUX_SUFFIX) <= MACOS_SUN_PATH_LIMIT
}

pub(super) fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn ssh_base_args_for(
    target: &str,
    temp_dir: &std::path::Path,
    explicit_no_multiplex: bool,
) -> Vec<String> {
    let control_path = control_path_for(temp_dir, target);
    // Win32-OpenSSH has no mux support. On Unix, OpenSSH appends a random
    // suffix while creating a master socket; an external TMPDIR can be long
    // enough that this exceeds sun_path. Fall back to a normal connection
    // rather than failing before SSH reaches the host.
    let no_multiplex =
        cfg!(windows) || explicit_no_multiplex || !multiplex_path_fits(&control_path);
    if !no_multiplex {
        if let Some(parent) = std::path::Path::new(&control_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let mut args = if no_multiplex {
        vec![
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "ControlPath=none".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
        ]
    } else {
        vec![
            "-o".into(),
            format!("ControlPath={control_path}"),
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

pub(super) fn ssh_base_args(target: &str) -> Vec<String> {
    ssh_base_args_for(
        target,
        &std::env::temp_dir(),
        std::env::var_os("BCMR_SSH_NO_MULTIPLEX").is_some(),
    )
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
        // macOS is the tightest at 104 bytes (Linux is 108) — assert against
        // the macOS budget so this fires on every supported platform.
        for target in [
            "host",
            "user@host",
            "user@host:port",
            "verylonguser_name@subdomain.example.com",
        ] {
            let path = control_path(target);
            let total = path.len() + OPENSSH_MUX_SUFFIX;
            assert!(
                total <= MACOS_SUN_PATH_LIMIT,
                "control_path({target:?}) + mux suffix = {total} bytes, \
                 exceeds sun_path budget {MACOS_SUN_PATH_LIMIT}: {path}"
            );
        }
    }

    #[test]
    fn overlong_external_temp_path_disables_multiplexing() {
        let long_external_temp = std::path::Path::new(
            "/Volumes/External Drive/Developments/bcmr/remote-live-tests/very-long-run-name/tmp",
        );

        let args = ssh_base_args_for("host", long_external_temp, false);

        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ControlMaster=no"]),
            "an overlong external TMPDIR must fall back to non-multiplexed SSH"
        );
    }
}
