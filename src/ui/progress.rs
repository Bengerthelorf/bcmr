use crate::ui::inline::InlineProgress;
use crate::ui::json::JsonProgress;
use crate::ui::tui::TuiProgress;
use std::io;

pub trait ProgressRenderer: Send {
    fn set_total_items(&mut self, total: usize);
    fn inc_items_processed(&mut self);
    fn set_current_file(&mut self, file_name: &str, file_size: u64);
    fn inc_current(&mut self, delta: u64);
    fn inc_skipped(&mut self, delta: u64) {
        self.inc_current(delta);
    }
    fn set_operation_type(&mut self, operation: &str);
    fn set_total_bytes(&mut self, total: u64);
    fn set_scanning(&mut self, scanning: bool);
    fn set_files_found(&mut self, count: u64);
    fn set_parallel_mode(&mut self, worker_count: usize);
    fn update_worker(&mut self, slot: usize, file_name: &str, file_size: u64, progress: u64);
    fn finish_worker(&mut self, slot: usize);
    fn tick(&mut self);
    fn finish(&mut self) -> io::Result<()>;
}

struct SilentProgress;

impl ProgressRenderer for SilentProgress {
    fn set_total_items(&mut self, _total: usize) {}
    fn inc_items_processed(&mut self) {}
    fn set_current_file(&mut self, _file_name: &str, _file_size: u64) {}
    fn inc_current(&mut self, _delta: u64) {}
    fn set_operation_type(&mut self, _operation: &str) {}
    fn set_total_bytes(&mut self, _total: u64) {}
    fn set_scanning(&mut self, _scanning: bool) {}
    fn set_files_found(&mut self, _count: u64) {}
    fn set_parallel_mode(&mut self, _worker_count: usize) {}
    fn update_worker(&mut self, _slot: usize, _file_name: &str, _file_size: u64, _progress: u64) {}
    fn finish_worker(&mut self, _slot: usize) {}
    fn tick(&mut self) {}
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn create_renderer(
    total_bytes: u64,
    plain: bool,
    silent: bool,
    json: bool,
) -> io::Result<Box<dyn ProgressRenderer>> {
    if json {
        Ok(Box::new(JsonProgress::new(total_bytes)))
    } else if silent {
        Ok(Box::new(SilentProgress))
    } else if plain {
        Ok(Box::new(InlineProgress::new(total_bytes)?))
    } else {
        Ok(Box::new(TuiProgress::new(total_bytes)?))
    }
}
