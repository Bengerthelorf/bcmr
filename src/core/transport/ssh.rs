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

#[allow(dead_code)]
pub async fn spawn_local(bcmr_path: &std::path::Path) -> Result<SshSpawn, BcmrError> {
    // `--root /` so integration tests can write under tempdirs outside
    // the default `$HOME` jail.
    let child = Command::new(bcmr_path)
        .args(["serve", "--root", "/"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    take_pipes(child)
}

async fn spawn(args: &[&str]) -> Result<SshSpawn, BcmrError> {
    // stderr goes to /dev/null because OpenSSH diagnostics
    // (`Could not request local forwarding`, key-probe chatter) would
    // interleave with the protocol stdout if passed through.
    let child = Command::new("ssh")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
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
