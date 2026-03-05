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

struct SilentProgress;

impl ProgressRenderer for SilentProgress {
    fn set_total_items(&mut self, _total: usize) {}
    fn inc_items_processed(&mut self) {}
    fn set_current_file(&mut self, _file_name: &str, _file_size: u64) {}
    fn inc_current(&mut self, _delta: u64) {}
    fn set_operation_type(&mut self, _operation: &str) {}
    fn tick(&mut self) {}
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Creates the appropriate progress renderer based on display mode.
///
/// - `silent`: no output (dry-run mode)
/// - `plain`: inline 3-line text progress (--tui flag or config)
/// - default: fancy TUI box with gradient progress bar
pub fn create_renderer(
    total_bytes: u64,
    plain: bool,
    silent: bool,
) -> io::Result<Box<dyn ProgressRenderer>> {
    if silent {
        Ok(Box::new(SilentProgress))
    } else if plain {
        Ok(Box::new(InlineProgress::new(total_bytes)?))
    } else {
        Ok(Box::new(TuiProgress::new(total_bytes)?))
    }
}
