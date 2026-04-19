use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::core::error::BcmrError;

pub struct SshSpawn {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

pub async fn spawn_remote(ssh_target: &str) -> Result<SshSpawn, BcmrError> {
    spawn(&[
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        ssh_target,
        "bcmr",
        "serve",
    ])
    .await
}

#[cfg(any(test, feature = "test-support"))]
#[allow(dead_code)]
pub async fn spawn_local(bcmr_path: &std::path::Path) -> Result<SshSpawn, BcmrError> {
    // `--root /` escapes the default `$HOME` jail for integration tests.
    let child = Command::new(bcmr_path)
        .args(["serve", "--root", "/"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    take_pipes(child)
}

async fn spawn(args: &[&str]) -> Result<SshSpawn, BcmrError> {
    // BCMR_DEBUG_SSH_STDERR=1 surfaces remote bcmr stderr.
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
