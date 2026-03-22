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

        execute!(stdout, Clear(ClearType::CurrentLine))?;
        let op_label = format!("{}: ", operation);
        write!(stdout, "{}", op_label)?;

        let suffix = if self.data.parallel_total > 0 {
            format!(
                " {}% [{}/{}w]",
                total_progress,
                self.data.active_worker_count(),
                self.data.parallel_total
            )
        } else {
            format!(" {}%", total_progress)
        };
        let bar_width = term_width
            .saturating_sub(op_label.len() + suffix.len() + 2)
            .max(10);

        write!(stdout, "[")?;
        let filled = (bar_width * total_progress as usize / 100).min(bar_width);
        let empty = bar_width - filled;
        write!(stdout, "{}", "=".repeat(filled))?;
        write!(stdout, "{}", "-".repeat(empty))?;
        writeln!(stdout, "]{}", suffix)?;

        let speed_str = if speed > 0.0 {
            format!("{}/s", format_bytes(speed * 1024.0 * 1024.0))
        } else {
            "-- /s".to_string()
        };
        execute!(stdout, Clear(ClearType::CurrentLine))?;
        if self.data.scanning {
            let dots = ".".repeat((self.data.elapsed().as_millis() / 500 % 4) as usize);
            writeln!(
                stdout,
                "{} (scanning{} {} files found) | {}",
                format_bytes(self.data.current_bytes as f64),
                dots,
                self.data.files_found,
                speed_str,
            )?;
        } else {
            writeln!(
                stdout,
                "{} / {} | {} | ETA: {}",
                format_bytes(self.data.current_bytes as f64),
                format_bytes(self.data.total_bytes as f64),
                speed_str,
                eta_str
            )?;
        }

        execute!(stdout, Clear(ClearType::CurrentLine))?;
        if self.data.parallel_total > 0 {
            let mut parts = Vec::new();
            for (i, worker) in self.data.workers.iter().enumerate() {
                if worker.active {
                    let pct = if worker.file_size > 0 {
                        (worker.progress as f64 / worker.file_size as f64 * 100.0).min(100.0) as u16
                    } else {
                        0
                    };
                    // Dynamic name width: divide available space among active workers
                    let active = self.data.active_worker_count().max(1);
                    let per_worker = term_width.saturating_sub(active * 14) / active;
                    let name_max = per_worker.max(12);
                    let display_name = if worker.file_name.chars().count() > name_max {
                        let end = worker
                            .file_name
                            .floor_char_boundary(name_max.saturating_sub(3));
                        format!("{}...", &worker.file_name[..end])
                    } else {
                        worker.file_name.clone()
                    };
                    parts.push(format!("[{}] {} {}%", i + 1, display_name, pct));
                } else {
                    parts.push(format!("[{}] idle", i + 1));
                }
            }
            write!(stdout, "{}", parts.join(" | "))?;
        } else {
            let file_label = "File: ";
            write!(stdout, "{}", file_label)?;

            let file_suffix = format!(" {}%", current_progress);
            let avail = term_width.saturating_sub(file_label.len() + file_suffix.len() + 2);
            let bar_width_file = (avail / 2).max(10);
            let name_width = avail.saturating_sub(bar_width_file + 1);

            let file_info = &self.data.current_file;
            let display_file = if file_info.len() > name_width {
                let mut end = name_width.saturating_sub(3);
                if !file_info.is_char_boundary(end) {
                    end = file_info.floor_char_boundary(end);
                }
                format!("{}...", &file_info[..end])
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
        }

        stdout.flush()?;

        self.lines_printed = 3;
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
        if self.data.current_bytes.is_multiple_of(1024 * 1024)
            || self.data.current_bytes >= self.data.total_bytes
        {
            let _ = self.redraw();
        }
    }

    fn inc_skipped(&mut self, delta: u64) {
        self.data.inc_skipped(delta);
        if self.data.current_bytes.is_multiple_of(1024 * 1024)
            || self.data.current_bytes >= self.data.total_bytes
        {
            let _ = self.redraw();
        }
    }

    fn set_operation_type(&mut self, operation: &str) {
        self.data.operation_type = operation.to_string();
        let _ = self.redraw();
    }

    fn set_total_bytes(&mut self, total: u64) {
        self.data.total_bytes = total;
        let _ = self.redraw();
    }

    fn set_scanning(&mut self, scanning: bool) {
        self.data.scanning = scanning;
        let _ = self.redraw();
    }

    fn set_files_found(&mut self, count: u64) {
        self.data.files_found = count;
    }

    fn set_parallel_mode(&mut self, worker_count: usize) {
        self.data.init_workers(worker_count);
        let _ = self.redraw();
    }

    fn update_worker(&mut self, slot: usize, file_name: &str, file_size: u64, progress: u64) {
        self.data
            .update_worker(slot, file_name, file_size, progress);
    }

    fn finish_worker(&mut self, slot: usize) {
        self.data.finish_worker(slot);
        let _ = self.redraw();
    }

    fn tick(&mut self) {
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
