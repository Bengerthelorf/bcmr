use crate::ui::progress::{self, ProgressRenderer};

use anyhow::{bail, Result};
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::signal::ctrl_c;
use tokio::time::Duration;

pub struct ProgressRunner {
    progress: Arc<Mutex<Box<dyn ProgressRenderer>>>,
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
        let progress = Arc::new(Mutex::new(renderer));

        let ticker = Arc::clone(&progress);
        let ticker_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;
                ticker.lock().tick();
            }
        });

        let signal = Arc::clone(&progress);
        tokio::spawn(async move {
            if let Ok(()) = ctrl_c().await {
                on_interrupt();
                let _ = signal.lock().finish_err("interrupted");
                std::process::exit(130);
            }
        });

        Ok(Self {
            progress,
            ticker_handle,
        })
    }

    pub fn progress(&self) -> &Arc<Mutex<Box<dyn ProgressRenderer>>> {
        &self.progress
    }

    pub fn inc_callback(&self) -> impl Fn(u64) + Send + Sync + Clone + 'static {
        let p = Arc::clone(&self.progress);
        move |n| p.lock().inc_current(n)
    }

    pub fn skip_callback(&self) -> impl Fn(u64) + Send + Sync + Clone + 'static {
        let p = Arc::clone(&self.progress);
        move |n| p.lock().inc_skipped(n)
    }

    pub fn file_callback(&self) -> impl Fn(&str, u64) + Send + Sync + Clone + 'static {
        let p = Arc::clone(&self.progress);
        move |name, size| p.lock().set_current_file(name, size)
    }

    pub fn set_parallel_mode(&self, worker_count: usize) {
        self.progress.lock().set_parallel_mode(worker_count);
    }

    pub fn finish_ok(self) -> Result<()> {
        self.ticker_handle.abort();
        self.progress.lock().finish()?;
        Ok(())
    }

    pub fn finish_err(self, msg: String) -> Result<()> {
        self.ticker_handle.abort();
        let _ = self.progress.lock().finish_err(&msg);
        bail!("{}", msg);
    }

    /// Emits the error terminal event without returning an Err, so the caller
    /// can re-raise the original error while the log file still gets a
    /// terminal "error" line.
    pub fn finish_with_error(self, msg: &str) {
        self.ticker_handle.abort();
        let _ = self.progress.lock().finish_err(msg);
    }
}

/// Tokio's `JoinHandle` detaches on drop; without aborting, two ticker tasks
/// could paint the same progress bar at once if the handler spawns a second
/// runner (e.g. serve fast path errors and we fall back to legacy SSH).
impl Drop for ProgressRunner {
    fn drop(&mut self) {
        self.ticker_handle.abort();
    }
}
