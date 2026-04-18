//! SSH-subprocess transport: spawn `bcmr serve` on the remote over
//! SSH, return the child handle plus stdin/stdout pipes for the
//! `ServeClient` to frame protocol messages over.
//!
//! Kept intentionally thin — this is just the spawn + take-pipes
//! plumbing that used to live inline in
//! `ServeClient::connect_with_caps`. Moving it here draws the "SSH
//! happens here" boundary cleanly so the rest of `ServeClient`
//! doesn't need to know what transport is underneath. Future
//! transports (direct TCP post-rendezvous) will sit next to this
//! file and produce compatible stream halves.

use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::core::error::BcmrError;

/// Handles to a live `bcmr serve` subprocess over SSH.
///
/// Owned by `ServeClient`: `child` keeps the process alive (Drop
/// kills it), `stdin` is the frame-writer side, `stdout` is the
/// frame-reader side. The pair acts as a bidirectional byte channel
/// carrying the `bcmr` wire protocol.
pub struct SshSpawn {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
}

/// Spawn `ssh <target> bcmr serve` with the BatchMode /
/// ConnectTimeout options the client has always used, and return the
/// handles the rest of `ServeClient` needs. The caller runs the
/// protocol `Hello`/`Welcome` handshake on top of these pipes.
///
/// `stderr` is dropped to `/dev/null` because SSH's diagnostic
/// chatter (like the `Could not request local forwarding` line seen
/// in CI) isn't actionable for the end user and would interleave
/// with our own stderr output.
pub async fn spawn_remote(ssh_target: &str) -> Result<SshSpawn, BcmrError> {
    let mut child = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            ssh_target,
            "bcmr",
            "serve",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;

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

/// Test-only: spawn the local-build `bcmr serve` binary directly
/// (no SSH hop). Used by the e2e tests that drive the protocol
/// against a real server without needing `ssh localhost` configured.
/// Passes `--root /` so tests can write to tempdirs outside `$HOME`.
#[allow(dead_code)]
pub async fn spawn_local(bcmr_path: &std::path::Path) -> Result<SshSpawn, BcmrError> {
    let mut child = Command::new(bcmr_path)
        .args(["serve", "--root", "/"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;

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
