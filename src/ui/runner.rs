use crate::ui::progress::{self, ProgressRenderer};

use anyhow::{bail, Result};
use parking_lot::Mutex;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::signal::ctrl_c;
use tokio::time::Duration;

#[derive(Default)]
struct CancellationState {
    requested: AtomicBool,
    notify: tokio::sync::Notify,
}

/// A sticky UI cancellation request owned by the current operation.
#[derive(Clone, Default)]
pub struct ProgressCancellation {
    inner: Arc<CancellationState>,
}

impl ProgressCancellation {
    pub fn is_cancelled(&self) -> bool {
        self.inner.requested.load(Ordering::Acquire)
    }

    pub(crate) fn request(&self) {
        self.inner.requested.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            // Register before reading the flag so a concurrent key press
            // cannot be lost between the flag read and the await.
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Clone)]
pub struct ProgressHandle {
    inner: Arc<Mutex<Box<dyn ProgressRenderer>>>,
}

impl ProgressHandle {
    fn new(renderer: Box<dyn ProgressRenderer>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(renderer)),
        }
    }

    pub fn inc_current(&self, n: u64) {
        self.inner.lock().inc_current(n);
    }

    pub fn inc_skipped(&self, n: u64) {
        self.inner.lock().inc_skipped(n);
    }

    pub fn inc_reflink(&self) {
        self.inner.lock().inc_reflink();
    }

    pub fn set_current_file(&self, name: &str, size: u64) {
        self.inner.lock().set_current_file(name, size);
    }

    pub fn update_worker(&self, slot: usize, name: &str, size: u64, progress: u64) {
        self.inner.lock().update_worker(slot, name, size, progress);
    }

    pub fn finish_worker(&self, slot: usize) {
        self.inner.lock().finish_worker(slot);
    }

    pub fn set_total_items(&self, total: usize) {
        self.inner.lock().set_total_items(total);
    }

    pub fn inc_items_processed(&self) {
        self.inner.lock().inc_items_processed();
    }

    pub fn set_total_bytes(&self, total: u64) {
        self.inner.lock().set_total_bytes(total);
    }

    pub fn set_scanning(&self, scanning: bool) {
        self.inner.lock().set_scanning(scanning);
    }

    pub fn set_files_found(&self, count: u64) {
        self.inner.lock().set_files_found(count);
    }

    pub fn set_operation_type(&self, operation: &str) {
        self.inner.lock().set_operation_type(operation);
    }

    pub fn set_verify_mode(&self, on: bool) {
        self.inner.lock().set_verify_mode(on);
    }

    pub fn set_parallel_mode(&self, worker_count: usize) {
        self.inner.lock().set_parallel_mode(worker_count);
    }

    pub(super) fn tick(&self) {
        self.inner.lock().tick();
    }

    pub(super) fn finish(&self) -> std::io::Result<()> {
        self.inner.lock().finish()
    }

    pub(super) fn finish_err(&self, msg: &str) -> std::io::Result<()> {
        self.inner.lock().finish_err(msg)
    }

    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut dyn ProgressRenderer) -> R,
    {
        f(&mut **self.inner.lock())
    }
}

pub struct ProgressRunner {
    handle: ProgressHandle,
    ticker_handle: tokio::task::JoinHandle<()>,
    signal_handle: Option<tokio::task::JoinHandle<()>>,
    cancellation: ProgressCancellation,
}

impl ProgressRunner {
    pub fn new(
        total_size: u64,
        mode: crate::cli::ProgressMode,
        silent: bool,
        json: bool,
        on_interrupt: fn(),
    ) -> std::io::Result<Self> {
        Self::build(total_size, mode, silent, json, Some(on_interrupt))
    }

    /// Keep progress rendering while the caller owns asynchronous interrupt
    /// handling and cleanup. Raw TUI Ctrl-C requests `cancellation_handle()`.
    pub fn new_without_signal_handler(
        total_size: u64,
        mode: crate::cli::ProgressMode,
        silent: bool,
        json: bool,
    ) -> std::io::Result<Self> {
        Self::build(total_size, mode, silent, json, None)
    }

    fn build(
        total_size: u64,
        mode: crate::cli::ProgressMode,
        silent: bool,
        json: bool,
        on_interrupt: Option<fn()>,
    ) -> std::io::Result<Self> {
        let log = crate::config::log_file();
        let mut renderer = progress::create_renderer(total_size, mode, silent, json, log.as_ref())?;
        let cancellation = ProgressCancellation::default();
        renderer.set_cancellation(cancellation.clone());
        let handle = ProgressHandle::new(renderer);

        let ticker = handle.clone();
        let ticker_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;
                ticker.tick();
            }
        });

        let signal_handle = on_interrupt.map(|on_interrupt| {
            let signal = handle.clone();
            let key_cancellation = cancellation.clone();
            tokio::spawn(async move {
                tokio::select! {
                    result = ctrl_c() => if result.is_err() { return; },
                    () = key_cancellation.cancelled() => {},
                }
                on_interrupt();
                let _ = signal.finish_err("interrupted");
                std::process::exit(130);
            })
        });

        Ok(Self {
            handle,
            ticker_handle,
            signal_handle,
            cancellation,
        })
    }

    pub fn cancellation_handle(&self) -> ProgressCancellation {
        self.cancellation.clone()
    }

    pub fn handle(&self) -> ProgressHandle {
        self.handle.clone()
    }

    pub fn inc_callback(&self) -> impl Fn(u64) + Send + Sync + Clone + 'static {
        let h = self.handle.clone();
        move |n| h.inc_current(n)
    }

    pub fn skip_callback(&self) -> impl Fn(u64) + Send + Sync + Clone + 'static {
        let h = self.handle.clone();
        move |n| h.inc_skipped(n)
    }

    pub fn reflink_callback(&self) -> impl Fn() + Send + Sync + Clone + 'static {
        let h = self.handle.clone();
        move || h.inc_reflink()
    }

    pub fn file_callback(&self) -> impl Fn(&str, u64) + Send + Sync + Clone + 'static {
        let h = self.handle.clone();
        move |name, size| h.set_current_file(name, size)
    }

    fn stop_tasks(&mut self) {
        self.ticker_handle.abort();
        if let Some(signal) = self.signal_handle.take() {
            signal.abort();
        }
    }

    pub fn finish_ok(mut self) -> Result<()> {
        self.stop_tasks();
        self.handle.finish()?;
        Ok(())
    }

    pub fn finish_err(mut self, msg: String) -> Result<()> {
        self.stop_tasks();
        let _ = self.handle.finish_err(&msg);
        bail!("{}", msg);
    }

    pub fn finish_with_error(mut self, msg: &str) {
        self.stop_tasks();
        let _ = self.handle.finish_err(msg);
    }
}

impl Deref for ProgressRunner {
    type Target = ProgressHandle;

    fn deref(&self) -> &ProgressHandle {
        &self.handle
    }
}

impl Drop for ProgressRunner {
    fn drop(&mut self) {
        self.stop_tasks();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_wakes_existing_and_future_waiters() {
        use futures::FutureExt;

        let cancellation = ProgressCancellation::default();
        let second = cancellation.clone();
        let mut first_waiter = Box::pin(cancellation.cancelled());
        let mut second_waiter = Box::pin(second.cancelled());
        assert!(first_waiter.as_mut().now_or_never().is_none());
        assert!(second_waiter.as_mut().now_or_never().is_none());

        cancellation.request();
        tokio::time::timeout(Duration::from_secs(1), first_waiter)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), second_waiter)
            .await
            .unwrap();
        assert_eq!(cancellation.cancelled().now_or_never(), Some(()));
    }

    // A real SIGINT must run in a subprocess: the regression calls exit(130),
    // so testing it inside the parent test runner would kill unrelated tests.
    #[cfg(unix)]
    #[test]
    fn finished_runners_do_not_handle_future_interrupts() {
        const CASE: &str = "BCMR_TEST_FINISHED_RUNNER_SIGNAL";
        if let Ok(case) = std::env::var(CASE) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let runner = if case == "without_signal" {
                    ProgressRunner::new_without_signal_handler(
                        0,
                        crate::cli::ProgressMode::Off,
                        true,
                        false,
                    )
                } else {
                    ProgressRunner::new(0, crate::cli::ProgressMode::Off, true, false, || {})
                }
                .unwrap();
                // Let the signal task register its listener before finishing.
                tokio::task::yield_now().await;
                let held_runner = match case.as_str() {
                    "ok" => {
                        runner.finish_ok().unwrap();
                        None
                    }
                    "err" => {
                        assert!(runner.finish_err("expected error".into()).is_err());
                        None
                    }
                    "error" => {
                        runner.finish_with_error("expected error");
                        None
                    }
                    "drop" => {
                        drop(runner);
                        None
                    }
                    "without_signal" => Some(runner),
                    _ => panic!("unknown lifecycle case"),
                };
                let mut later_operation =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                        .unwrap();
                assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0);
                tokio::time::timeout(Duration::from_secs(2), later_operation.recv())
                    .await
                    .unwrap()
                    .expect("the later operation must receive SIGINT");
                // A detached old handler would be scheduled by that SIGINT.
                tokio::time::sleep(Duration::from_millis(10)).await;
                drop(held_runner);
            });
            println!("later SIGINT handled by operation ({case})");
            return;
        }

        let (_, module) = module_path!().split_once("::").unwrap();
        let test_name = format!("{module}::finished_runners_do_not_handle_future_interrupts");
        for case in ["ok", "err", "error", "drop", "without_signal"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test_name, "--nocapture"])
                .env(CASE, case)
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .contains(&format!("later SIGINT handled by operation ({case})")),
                "finished runner ({case}) intercepted a later SIGINT: {:?}\n{}\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
