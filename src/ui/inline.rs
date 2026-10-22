use crate::ui::progress::ProgressRenderer;
use crate::ui::state::ProgressData;
use crate::ui::utils::{format_bytes, format_eta};
use crossterm::{
    cursor::MoveToColumn,
    execute,
    terminal::{Clear, ClearType},
};
use std::io::{self, stdout, Write};

pub struct InlineProgress {
    data: ProgressData,
    finished: bool,
    lines_printed: u16,
    last_size: Option<(u16, u16)>,
}

impl InlineProgress {
    pub fn new(total_bytes: u64) -> io::Result<Self> {
        let data = ProgressData::new(total_bytes);
        Ok(Self {
            data,
            finished: false,
            lines_printed: 0,
            last_size: None,
        })
    }

    fn redraw(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        let size = terminal_size::terminal_size()
            .map(|(w, h)| (w.0, h.0))
            .unwrap_or((80, 24));
        if self.last_size.is_some_and(|previous| previous != size) {
            // Resizing can reflow the old block. Start below it rather than
            // guessing its physical height and overwriting command history.
            self.lines_printed = 0;
        }
        self.last_size = Some(size);
        let width = (size.0 as usize).saturating_sub(1);
        let percent = if let Some(total) = self.data.items_total {
            percentage(self.data.items_processed as u64, total as u64)
        } else {
            percentage(self.data.current_bytes, self.data.total_bytes)
        };
        let operation = if self.data.operation_type.is_empty() {
            "Progress"
        } else {
            &self.data.operation_type
        };
        let label = format!("{operation}: ");
        let suffix = format!(" {percent}%");
        let bar_width = width.saturating_sub(label.len() + suffix.len() + 2);
        let filled = bar_width * percent as usize / 100;
        let first = format!(
            "{label}[{}{}]{suffix}",
            "=".repeat(filled),
            "-".repeat(bar_width - filled)
        );
        let speed = self.data.calculate_speed();
        let eta = self
            .data
            .estimate_eta()
            .map(|d| format_eta(d.as_secs()))
            .unwrap_or_else(|| "--".into());
        let detail = if let Some(total) = self.data.items_total {
            format!(
                "{} / {total} items | {}",
                self.data.items_processed,
                format_bytes(self.data.current_bytes as f64)
            )
        } else if self.data.scanning {
            format!(
                "{} | scanning: {} files found",
                format_bytes(self.data.current_bytes as f64),
                self.data.files_found
            )
        } else {
            format!(
                "{} / {} | {}/s | ETA: {eta}",
                format_bytes(self.data.current_bytes as f64),
                format_bytes(self.data.total_bytes as f64),
                format_bytes(speed * 1024.0 * 1024.0)
            )
        };
        let current = if self.data.parallel_total > 0 {
            let workers = self
                .data
                .workers
                .iter()
                .enumerate()
                .filter(|(_, worker)| worker.active)
                .map(|(index, worker)| {
                    format!(
                        "[{}] {} {}%",
                        index + 1,
                        worker.file_name,
                        percentage(worker.progress, worker.file_size)
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            if workers.is_empty() {
                "Workers: idle".to_owned()
            } else {
                workers
            }
        } else if self.data.current_file.is_empty() {
            String::new()
        } else {
            format!("File: {}", self.data.current_file)
        };
        let mut out = stdout().lock();
        if self.lines_printed > 0 {
            execute!(
                out,
                crossterm::cursor::MoveUp(self.lines_printed),
                MoveToColumn(0)
            )?;
        }
        let line_count = size.1.saturating_sub(1).clamp(1, 3);
        for line in [first, detail, current]
            .into_iter()
            .take(line_count as usize)
        {
            execute!(out, Clear(ClearType::CurrentLine))?;
            // Reserve the last column to avoid terminal autowrap. Every tracked
            // line ends with a newline, so refresh never moves into shell history.
            writeln!(out, "{}", fit_line(&line, width))?;
        }
        self.lines_printed = line_count;
        out.flush()
    }
}

fn percentage(done: u64, total: u64) -> u16 {
    if total == 0 {
        100
    } else {
        (done as f64 / total as f64 * 100.0).min(100.0) as u16
    }
}

fn fit_line(line: &str, width: usize) -> String {
    let mut fitted = String::new();
    let mut cells = 0;
    for ch in line.chars() {
        let escaped = if ch.is_control() {
            ch.escape_default().to_string()
        } else {
            ch.to_string()
        };
        // Conservatively budget two cells for non-ASCII glyphs, including CJK
        // and emoji; overestimating is preferable to wrapping a live display.
        let needed: usize = escaped
            .chars()
            .map(|c| if c.is_ascii() { 1 } else { 2 })
            .sum();
        if cells + needed > width {
            break;
        }
        fitted.push_str(&escaped);
        cells += needed;
    }
    fitted
}

impl ProgressRenderer for InlineProgress {
    fn set_total_items(&mut self, total: usize) {
        self.data.items_total = Some(total);
    }
    fn set_verify_mode(&mut self, on: bool) {
        self.data.verify_mode = on;
    }
    fn inc_reflink(&mut self) {
        self.data.reflink_count += 1;
    }
    fn inc_items_processed(&mut self) {
        self.data.items_processed += 1;
    }
    fn set_current_file(&mut self, file_name: &str, file_size: u64) {
        self.data.current_file = file_name.to_string();
        self.data.current_file_size = file_size;
        self.data.current_file_progress = 0;
    }
    fn inc_current(&mut self, delta: u64) {
        self.data.current_bytes += delta;
        self.data.current_file_progress += delta;
    }
    fn inc_skipped(&mut self, delta: u64) {
        self.data.inc_skipped(delta);
    }
    fn set_operation_type(&mut self, operation: &str) {
        self.data.operation_type = operation.to_string();
    }
    fn set_total_bytes(&mut self, total: u64) {
        self.data.total_bytes = total;
    }
    fn set_scanning(&mut self, scanning: bool) {
        self.data.scanning = scanning;
    }
    fn set_files_found(&mut self, count: u64) {
        self.data.files_found = count;
    }
    fn set_parallel_mode(&mut self, worker_count: usize) {
        self.data.init_workers(worker_count);
    }
    fn update_worker(&mut self, slot: usize, file_name: &str, file_size: u64, progress: u64) {
        self.data
            .update_worker(slot, file_name, file_size, progress);
    }
    fn finish_worker(&mut self, slot: usize) {
        self.data.finish_worker(slot);
    }
    fn tick(&mut self) {
        let _ = self.redraw();
    }
    fn finish(&mut self) -> io::Result<()> {
        if !self.finished {
            self.redraw()?;
            self.finished = true;
            println!("{}", self.data.done_summary_line());
        }
        Ok(())
    }
    fn finish_err(&mut self, _message: &str) -> io::Result<()> {
        if !self.finished {
            self.finished = true;
            println!("{}", self.data.stopped_summary_line());
        }
        Ok(())
    }
}
