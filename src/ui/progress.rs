use crate::ui::inline::InlineProgress;
use crate::ui::tui::TuiProgress;
use std::io;

pub trait ProgressRenderer: Send {
    fn set_total_items(&mut self, total: usize);
    fn inc_items_processed(&mut self);
    fn set_current_file(&mut self, file_name: &str, file_size: u64);
    fn inc_current(&mut self, delta: u64);
    fn set_operation_type(&mut self, operation: &str);
    fn tick(&mut self);
    fn finish(&mut self) -> io::Result<()>;
}

// Public interface
pub struct CopyProgress {
    inner: Box<dyn ProgressRenderer>,
}

impl CopyProgress {
    pub fn new(total_bytes: u64, tui_mode: bool) -> io::Result<Self> {
        let inner: Box<dyn ProgressRenderer> = if tui_mode {
            Box::new(InlineProgress::new(total_bytes)?)
        } else {
            Box::new(TuiProgress::new(total_bytes)?)
        };

        Ok(Self { inner })
    }

    pub fn set_total_items(&mut self, total: usize) {
        self.inner.set_total_items(total);
    }

    pub fn inc_items_processed(&mut self) {
        self.inner.inc_items_processed();
    }

    pub fn set_current_file(&mut self, file_name: &str, file_size: u64) {
        self.inner.set_current_file(file_name, file_size);
    }

    pub fn inc_current(&mut self, delta: u64) {
        self.inner.inc_current(delta);
    }

    pub fn set_operation_type(&mut self, operation: &str) {
        self.inner.set_operation_type(operation);
    }

    pub fn tick(&mut self) {
        self.inner.tick();
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.inner.finish()
    }
}
