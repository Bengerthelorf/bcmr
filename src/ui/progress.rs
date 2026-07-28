use crate::cli::ProgressMode;
use crate::ui::inline::InlineProgress;
use crate::ui::json::JsonProgress;
use crate::ui::state::ProgressData;
use crate::ui::tui::TuiProgress;
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

    fn set_verify_mode(&mut self, _on: bool) {}
    fn inc_reflink(&mut self) {}

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
        println!("{}", self.data.done_summary_line());
        Ok(())
    }

    fn set_verify_mode(&mut self, on: bool) {
        self.data.verify_mode = on;
    }

    fn inc_reflink(&mut self) {
        self.data.reflink_count += 1;
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RendererKind {
    Json,
    Silent,
    Plain,
    Inline,
    Tui,
}

pub(crate) fn color_disabled_by_env() -> bool {
    std::env::var_os("NO_COLOR")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

pub(crate) fn terminal_controls_disabled_by_env() -> bool {
    matches!(std::env::var("TERM").as_deref(), Ok("dumb"))
}

pub(crate) fn color_enabled_for_stdout() -> bool {
    std::io::stdout().is_terminal()
        && !color_disabled_by_env()
        && !terminal_controls_disabled_by_env()
}

fn select_renderer(
    mode: ProgressMode,
    silent: bool,
    json: bool,
    is_terminal: bool,
    no_color: bool,
    controls_disabled: bool,
    terminal_size: (u16, u16),
) -> RendererKind {
    if json {
        return RendererKind::Json;
    }
    if silent || mode == ProgressMode::Off {
        return RendererKind::Silent;
    }
    if !is_terminal || controls_disabled {
        return RendererKind::Plain;
    }

    let inline_fits = terminal_size.0 >= 40 && terminal_size.1 >= 4;
    let tui_fits = terminal_size.0 >= 50 && terminal_size.1 >= 8;
    match mode {
        ProgressMode::Plain => RendererKind::Plain,
        ProgressMode::Inline => {
            if inline_fits {
                RendererKind::Inline
            } else {
                RendererKind::Plain
            }
        }
        ProgressMode::Tui | ProgressMode::Auto if tui_fits && !no_color => RendererKind::Tui,
        ProgressMode::Tui | ProgressMode::Auto if inline_fits => RendererKind::Inline,
        ProgressMode::Tui | ProgressMode::Auto => RendererKind::Plain,
        ProgressMode::Off => RendererKind::Silent,
    }
}

pub(crate) fn renderer_kind(mode: ProgressMode, silent: bool, json: bool) -> RendererKind {
    let size = terminal_size::terminal_size()
        .map(|(width, height)| (width.0, height.0))
        .unwrap_or((80, 24));
    select_renderer(
        mode,
        silent,
        json,
        std::io::stdout().is_terminal(),
        color_disabled_by_env(),
        terminal_controls_disabled_by_env(),
        size,
    )
}

pub fn create_renderer(
    total_bytes: u64,
    mode: ProgressMode,
    silent: bool,
    json: bool,
    log_file: Option<&PathBuf>,
) -> io::Result<Box<dyn ProgressRenderer>> {
    match renderer_kind(mode, silent, json) {
        RendererKind::Json => match log_file {
            Some(path) => Ok(Box::new(JsonProgress::with_log_file(total_bytes, path)?)),
            None => Ok(Box::new(JsonProgress::new(total_bytes))),
        },
        RendererKind::Silent => Ok(Box::new(SilentProgress)),
        RendererKind::Plain => Ok(Box::new(PlainTextProgress::new(total_bytes))),
        RendererKind::Inline => Ok(Box::new(InlineProgress::new(total_bytes)?)),
        RendererKind::Tui => Ok(Box::new(TuiProgress::new(total_bytes)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::{select_renderer, RendererKind};
    use crate::cli::ProgressMode;

    #[test]
    fn json_and_silent_have_stable_precedence() {
        assert_eq!(
            select_renderer(ProgressMode::Off, true, true, false, true, true, (1, 1)),
            RendererKind::Json
        );
        assert_eq!(
            select_renderer(
                ProgressMode::Auto,
                true,
                false,
                true,
                false,
                false,
                (80, 24)
            ),
            RendererKind::Silent
        );
    }

    #[test]
    fn auto_degrades_by_capability_without_losing_all_progress_for_no_color() {
        assert_eq!(
            select_renderer(
                ProgressMode::Auto,
                false,
                false,
                true,
                false,
                false,
                (80, 24)
            ),
            RendererKind::Tui
        );
        assert_eq!(
            select_renderer(
                ProgressMode::Auto,
                false,
                false,
                true,
                true,
                false,
                (80, 24)
            ),
            RendererKind::Inline
        );
        assert_eq!(
            select_renderer(
                ProgressMode::Auto,
                false,
                false,
                false,
                false,
                false,
                (80, 24)
            ),
            RendererKind::Plain
        );
    }

    #[test]
    fn explicit_modes_do_what_their_names_say() {
        assert_eq!(
            select_renderer(
                ProgressMode::Plain,
                false,
                false,
                true,
                false,
                false,
                (80, 24)
            ),
            RendererKind::Plain
        );
        assert_eq!(
            select_renderer(
                ProgressMode::Inline,
                false,
                false,
                true,
                false,
                false,
                (80, 24)
            ),
            RendererKind::Inline
        );
        assert_eq!(
            select_renderer(
                ProgressMode::Tui,
                false,
                false,
                true,
                false,
                false,
                (80, 24)
            ),
            RendererKind::Tui
        );
    }
}
