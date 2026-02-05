use crate::ui::progress::ProgressRenderer;
use crate::ui::state::ProgressData;
use crate::ui::utils::{format_bytes, format_eta};
use crossterm::{
    cursor::MoveToColumn,
    execute,
    terminal::{Clear, ClearType},
};
use std::io::{self, stdout, Write};

// Inline progress
pub struct InlineProgress {
    data: ProgressData,
    initialized: bool,
    lines_printed: u16,
}

impl InlineProgress {
    pub fn new(total_bytes: u64) -> io::Result<Self> {
        let data = ProgressData::new(total_bytes);
        Ok(Self {
            data,
            initialized: false,
            lines_printed: 0,
        })
    }

    fn initialize(&mut self) -> io::Result<()> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        Ok(())
    }

    fn redraw(&mut self) -> io::Result<()> {
        if !self.initialized {
            self.initialize()?;
        }

        let mut stdout = stdout();

        // Cursor move
        if self.lines_printed > 0 {
            execute!(
                stdout,
                crossterm::cursor::MoveUp(self.lines_printed),
                MoveToColumn(0)
            )?;
        }

        let total_progress =
            (self.data.current_bytes as f64 / self.data.total_bytes.max(1) as f64 * 100.0) as u16;
        let current_progress = (self.data.current_file_progress as f64
            / self.data.current_file_size.max(1) as f64
            * 100.0) as u16;
        let speed = self.data.calculate_speed();
        let eta_opt = self.data.estimate_eta();

        let operation = if self.data.operation_type.is_empty() {
            "Progress".to_string()
        } else {
            self.data.operation_type.clone()
        };

        let eta_str = match eta_opt {
            Some(d) => format_eta(d.as_secs()),
            None => "--".to_string(),
        };

        let (term_width, _) = terminal_size::terminal_size()
            .map(|(w, h)| (w.0, h.0))
            .unwrap_or((80, 24));
        let term_width = term_width as usize;

        // L1: Op + Total
        execute!(stdout, Clear(ClearType::CurrentLine))?;
        let op_label = format!("{}: ", operation);
        write!(stdout, "{}", op_label)?;

        let suffix = format!(" {}%", total_progress);
        let bar_width = term_width
            .saturating_sub(op_label.len() + suffix.len() + 2)
            .max(10);

        write!(stdout, "[")?;
        let filled = (bar_width * total_progress as usize / 100).min(bar_width);
        let empty = bar_width - filled;
        write!(stdout, "{}", "=".repeat(filled))?;
        write!(stdout, "{}", "-".repeat(empty))?;
        writeln!(stdout, "]{}", suffix)?;

        // L2: Stats
        execute!(stdout, Clear(ClearType::CurrentLine))?;
        writeln!(
            stdout,
            "{} / {} | {}/s | ETA: {}",
            format_bytes(self.data.current_bytes as f64),
            format_bytes(self.data.total_bytes as f64),
            format_bytes(speed * 1024.0 * 1024.0),
            eta_str
        )?;

        // L3: File
        execute!(stdout, Clear(ClearType::CurrentLine))?;
        let file_label = "File: ";
        write!(stdout, "{}", file_label)?;

        let file_suffix = format!(" {}%", current_progress);
        let avail = term_width.saturating_sub(file_label.len() + file_suffix.len() + 2);
        let bar_width_file = (avail / 2).max(10);
        let name_width = avail.saturating_sub(bar_width_file + 1);

        let file_info = &self.data.current_file;
        let display_file = if file_info.len() > name_width {
            format!("{}...", &file_info[..name_width.saturating_sub(3)])
        } else {
            format!("{:width$}", file_info, width = name_width)
        };

        write!(stdout, "{} ", display_file)?;

        write!(stdout, "[")?;
        let filled = (bar_width_file * current_progress as usize / 100).min(bar_width_file);
        let empty = bar_width_file - filled;
        write!(stdout, "{}", "=".repeat(filled))?;
        write!(stdout, "{}", "-".repeat(empty))?;
        write!(stdout, "]{}", file_suffix)?;

        stdout.flush()?;

        self.lines_printed = 2;
        Ok(())
    }
}

impl ProgressRenderer for InlineProgress {
    fn set_total_items(&mut self, total: usize) {
        self.data.items_total = Some(total);
        let _ = self.redraw();
    }

    fn inc_items_processed(&mut self) {
        self.data.items_processed += 1;
        let _ = self.redraw();
    }

    fn set_current_file(&mut self, file_name: &str, file_size: u64) {
        self.data.current_file = file_name.to_string();
        self.data.current_file_size = file_size;
        self.data.current_file_progress = 0;
        let _ = self.redraw();
    }

    fn inc_current(&mut self, delta: u64) {
        self.data.current_bytes += delta;
        self.data.current_file_progress += delta;
        let _ = self.redraw();
    }

    fn set_operation_type(&mut self, operation: &str) {
        self.data.operation_type = operation.to_string();
        let _ = self.redraw();
    }

    fn tick(&mut self) {
        // Redraw on tick
        let _ = self.redraw();
    }

    fn finish(&mut self) -> io::Result<()> {
        // Print a final overall summary (useful for logs / scrolling terminals)
        let elapsed = self.data.elapsed();
        let avg_bps = self.data.average_bytes_per_sec().unwrap_or(0.0);
        println!();
        println!(
            "Done: {} in {:.1}s | avg {}/s",
            format_bytes(self.data.current_bytes as f64),
            elapsed.as_secs_f64(),
            format_bytes(avg_bps)
        );
        Ok(())
    }
}
