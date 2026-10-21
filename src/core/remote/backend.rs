//! Invocation backend preference and per-connection capability discovery.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

use crate::core::error::BcmrError;
use crate::core::sftp::SftpClient;

static PREFERENCE: AtomicU8 = AtomicU8::new(0);
type Session = Arc<Mutex<SftpClient>>;
static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
static DISCOVERY: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

pub fn configure(preference: &str) {
    PREFERENCE.store(
        match preference {
            "ssh" => 1,
            "sftp" => 2,
            _ => 0,
        },
        Ordering::Relaxed,
    );
}

pub async fn session(target: &str) -> Result<Session, BcmrError> {
    let mut sessions = SESSIONS.get_or_init(Default::default).lock().await;
    if let Some(session) = sessions.get(target) {
        if session.lock().await.is_usable() {
            return Ok(Arc::clone(session));
        }
    }
    let session = Arc::new(Mutex::new(SftpClient::connect(target).await?));
    sessions.insert(target.to_owned(), Arc::clone(&session));
    Ok(session)
}

/// Drop cached sessions after a failed composite operation. In particular,
/// abandoning a source reader must close its server-side handle, even when
/// cancellation happened between two protocol requests.
#[allow(dead_code)] // Used by the command binary composite transfer engine.
pub(crate) async fn forget(target: &str) {
    SESSIONS
        .get_or_init(Default::default)
        .lock()
        .await
        .remove(target);
}

/// Discover once, before source expansion as well as before payload planning.
/// A real subsystem handshake must succeed before classifying an exec failure
/// as SFTP-only; authentication and network errors remain visible.
pub async fn is_sftp(target: &str) -> Result<bool, BcmrError> {
    match PREFERENCE.load(Ordering::Relaxed) {
        1 => return Ok(false),
        2 => return Ok(true),
        _ => {}
    }
    let mut cache = DISCOVERY.get_or_init(Default::default).lock().await;
    if let Some(&result) = cache.get(target) {
        return Ok(result);
    }
    let probe = super::ssh_cmd::ssh_command(target)
        .arg("printf BCMR_EXEC_AVAILABLE")
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    let use_sftp = !(probe.status.success() && probe.stdout == b"BCMR_EXEC_AVAILABLE");
    if use_sftp {
        if let Err(error) = session(target).await {
            if probe.status.code() == Some(255) {
                return Err(invalid(super::ssh_cmd::ssh_error_message(
                    &String::from_utf8_lossy(&probe.stderr),
                    &format!("Cannot connect to '{target}'"),
                )));
            }
            return Err(invalid(format!(
                "SSH execution unavailable on '{target}' ({}); SFTP handshake also failed: {error}",
                String::from_utf8_lossy(&probe.stderr).trim()
            )));
        }
    }
    cache.insert(target.to_owned(), use_sftp);
    Ok(use_sftp)
}

pub(crate) fn invalid(message: impl Into<String>) -> BcmrError {
    BcmrError::InvalidInput(message.into())
}

pub(crate) fn missing(error: &BcmrError) -> bool {
    matches!(error, BcmrError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
}
