use crate::ui::progress::{self, ProgressRenderer};

use anyhow::{bail, Result};
use parking_lot::Mutex;
use std::ops::Deref;
use std::sync::Arc;
use tokio::signal::ctrl_c;
use tokio::time::Duration;

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
}

impl ProgressRunner {
    pub fn new(
        total_size: u64,
        plain: bool,
        silent: bool,
        json: bool,
        on_interrupt: fn(),
    ) -> std::io::Result<Self> {
        let log = crate::config::log_file();
        let renderer = progress::create_renderer(total_size, plain, silent, json, log.as_ref())?;
        let handle = ProgressHandle::new(renderer);

        let ticker = handle.clone();
        let ticker_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;
                ticker.tick();
            }
        });

        let signal = handle.clone();
        tokio::spawn(async move {
            if let Ok(()) = ctrl_c().await {
                on_interrupt();
                let _ = signal.finish_err("interrupted");
                std::process::exit(130);
            }
        });

        Ok(Self {
            handle,
            ticker_handle,
        })
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

    pub fn finish_ok(self) -> Result<()> {
        self.ticker_handle.abort();
        self.handle.finish()?;
        Ok(())
    }

    pub fn finish_err(self, msg: String) -> Result<()> {
        self.ticker_handle.abort();
        let _ = self.handle.finish_err(&msg);
        bail!("{}", msg);
    }

    pub fn finish_with_error(self, msg: &str) {
        self.ticker_handle.abort();
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
        self.ticker_handle.abort();
    }
}
