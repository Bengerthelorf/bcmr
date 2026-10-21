//! Sticky cancellation for one CLI invocation, including post-copy cleanup.
//!
//! Install once before operation I/O and retain the guard until all move-source
//! cleanup has completed. Critical namespace mutations must finish or recover;
//! only read-only validation may be dropped by `read_only`.

use crate::core::error::BcmrError;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static NOTIFY: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

#[must_use = "retain the interrupt guard through all operation and source cleanup phases"]
pub struct InterruptGuard(tokio::task::JoinHandle<()>);

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub fn request() {
    REQUESTED.store(true, Ordering::Release);
    NOTIFY.notify_waiters();
}

fn error() -> BcmrError {
    BcmrError::Io(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "operation interrupted",
    ))
}

pub fn check() -> Result<(), BcmrError> {
    if REQUESTED.load(Ordering::Acquire) {
        Err(error())
    } else {
        Ok(())
    }
}

pub async fn cancelled() {
    loop {
        let notified = NOTIFY.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if REQUESTED.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

/// Register the OS listener before returning, rather than leaving a race until
/// the spawned task receives its first poll. The caller keeps this guard for
/// the whole command, through every destination and captured source cleanup.
pub async fn install() -> std::io::Result<InterruptGuard> {
    let mut signal = Box::pin(tokio::signal::ctrl_c());
    let received = std::future::poll_fn(|context| {
        std::task::Poll::Ready(match signal.as_mut().poll(context) {
            std::task::Poll::Ready(Ok(())) => {
                request();
                Ok(true)
            }
            std::task::Poll::Ready(Err(error)) => Err(error),
            std::task::Poll::Pending => Ok(false),
        })
    })
    .await?;
    Ok(InterruptGuard(tokio::spawn(async move {
        if !received && signal.await.is_ok() {
            request();
        }
    })))
}

/// Cancel read-only validation before a mutation starts. Do not use this for
/// rename/unlink/quarantine futures, which must complete their recovery logic.
pub async fn read_only<T>(
    operation: impl Future<Output = Result<T, BcmrError>>,
) -> Result<T, BcmrError> {
    check()?;
    tokio::select! {
        biased;
        _ = cancelled() => Err(error()),
        result = operation => {
            check()?;
            result
        }
    }
}
