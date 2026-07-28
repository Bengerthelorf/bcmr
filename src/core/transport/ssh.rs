use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::core::error::BcmrError;

pub(crate) const SSH_LIVENESS_ARGS: [&str; 4] = [
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=20",
];

pub struct SshSpawn {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

pub async fn spawn_remote(ssh_target: &str) -> Result<SshSpawn, BcmrError> {
    spawn(&remote_args(ssh_target)).await
}

fn remote_args(ssh_target: &str) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    args.extend(SSH_LIVENESS_ARGS.map(str::to_owned));
    args.extend([ssh_target.into(), "bcmr".into(), "serve".into()]);
    args
}

#[cfg(any(test, feature = "test-support"))]
#[allow(dead_code)]
pub async fn spawn_local(bcmr_path: &std::path::Path) -> Result<SshSpawn, BcmrError> {
    let child = Command::new(bcmr_path)
        .args(["serve", "--root", "/"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    take_pipes(child)
}

async fn spawn(args: &[String]) -> Result<SshSpawn, BcmrError> {
    let stderr_dest = if std::env::var("BCMR_DEBUG_SSH_STDERR").is_ok_and(|v| v == "1") {
        std::process::Stdio::inherit()
    } else {
        std::process::Stdio::null()
    };
    let child = Command::new("ssh")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(stderr_dest)
        .kill_on_drop(true)
        .spawn()?;
    take_pipes(child)
}

fn take_pipes(mut child: Child) -> Result<SshSpawn, BcmrError> {
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("failed to open child stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("failed to open child stdout".into()))?;
    Ok(SshSpawn {
        child,
        stdin,
        stdout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_serve_ssh_has_bounded_liveness_detection() {
        let args = remote_args("host");

        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ServerAliveInterval=15"]),
            "serve transport must probe a silent SSH connection"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ServerAliveCountMax=20"]),
            "serve transport must eventually close an unresponsive SSH connection"
        );
    }
}
