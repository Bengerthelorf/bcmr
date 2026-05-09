use crate::ui::inline::InlineProgress;
use crate::ui::json::JsonProgress;
use crate::ui::state::ProgressData;
use crate::ui::tui::TuiProgress;
use crate::ui::utils::format_bytes;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

pub trait ProgressRenderer: Send {
    fn inc_current(&mut self, delta: u64);
    fn finish(&mut self) -> io::Result<()>;

    fn finish_err(&mut self, _msg: &str) -> io::Result<()> {
        self.finish()
    }

    fn set_total_items(&mut self, _total: usize) {}
    fn inc_items_processed(&mut self) {}
    fn set_current_file(&mut self, _file_name: &str, _file_size: u64) {}
    fn inc_skipped(&mut self, delta: u64) {
        self.inc_current(delta);
    }
    fn set_operation_type(&mut self, _operation: &str) {}
    fn set_total_bytes(&mut self, _total: u64) {}
    fn set_scanning(&mut self, _scanning: bool) {}
    fn set_files_found(&mut self, _count: u64) {}

    fn set_parallel_mode(&mut self, _worker_count: usize) {}
    fn update_worker(&mut self, _slot: usize, _file_name: &str, _file_size: u64, _progress: u64) {}
    fn finish_worker(&mut self, _slot: usize) {}

    fn tick(&mut self) {}
}

struct SilentProgress;

impl ProgressRenderer for SilentProgress {
    fn inc_current(&mut self, _delta: u64) {}
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct PlainTextProgress {
    data: ProgressData,
}

impl PlainTextProgress {
    fn new(total_bytes: u64) -> Self {
        Self {
            data: ProgressData::new(total_bytes),
        }
    }
}

impl ProgressRenderer for PlainTextProgress {
    fn inc_current(&mut self, delta: u64) {
        self.data.current_bytes += delta;
    }

    fn finish(&mut self) -> io::Result<()> {
        let elapsed = self.data.elapsed();
        let avg_bps = self.data.average_bytes_per_sec().unwrap_or(0.0);
        println!(
            "Done: {} in {:.1}s | avg {}/s",
            format_bytes(self.data.current_bytes as f64),
            elapsed.as_secs_f64(),
            format_bytes(avg_bps),
        );
        Ok(())
    }
}

pub fn create_renderer(
    total_bytes: u64,
    plain: bool,
    silent: bool,
    json: bool,
    log_file: Option<&PathBuf>,
) -> io::Result<Box<dyn ProgressRenderer>> {
    if json {
        match log_file {
            Some(path) => Ok(Box::new(JsonProgress::with_log_file(total_bytes, path)?)),
            None => Ok(Box::new(JsonProgress::new(total_bytes))),
        }
    } else if silent {
        Ok(Box::new(SilentProgress))
    } else if plain {
        Ok(Box::new(InlineProgress::new(total_bytes)?))
    } else if !std::io::stdout().is_terminal() {
        Ok(Box::new(PlainTextProgress::new(total_bytes)))
    } else {
        Ok(Box::new(TuiProgress::new(total_bytes)?))
    }
}
