use crate::config::CONFIG;
use crate::ui::progress::ProgressRenderer;
use crate::ui::state::ProgressData;
use crate::ui::suspend::{install_suspend_handler, suspend_now};
use crate::ui::utils::{format_bytes, format_eta, get_gradient_color, parse_hex_color};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode},
    execute,
    style::{Attribute, SetAttribute, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use std::io::{self, stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
pub struct TuiProgress {
    data: ProgressData,
    start_row: u16,
    raw_mode_enabled: bool,
    initialized: bool,
    finished: bool,
    suspended: Arc<AtomicBool>,
    last_rendered_lines: u16,
    alternate_screen_enabled: bool,
    last_terminal_size: Option<(u16, u16)>,
    compact_layout: bool,
}

fn use_compact_layout(term_width: u16, term_height: u16, required_height: u16) -> bool {
    term_width < 50 || term_height < required_height
}

fn clip_line_to_width(line: &str, term_width: u16) -> String {
    let max_width = term_width.max(1) as usize;
    if line.chars().count() <= max_width {
        return line.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut clipped = line.chars().take(max_width - 1).collect::<String>();
    clipped.push('…');
    clipped
}

impl TuiProgress {
    pub fn new(total_bytes: u64) -> io::Result<Self> {
        let data = ProgressData::new(total_bytes);
        Ok(Self {
            data,
            start_row: 0,
            raw_mode_enabled: false,
            initialized: false,
            finished: false,
            suspended: Arc::new(AtomicBool::new(false)),
            last_rendered_lines: 0,
            alternate_screen_enabled: false,
            last_terminal_size: None,
            compact_layout: false,
        })
    }

    fn total_lines(&self) -> u16 {
        if self.data.parallel_total > 0 {
            5 + self.data.parallel_total as u16
        } else {
            7
        }
    }

    fn initialize(&mut self) -> io::Result<()> {
        if self.initialized {
            return Ok(());
        }

        self.suspended = install_suspend_handler()?;

        enable_raw_mode()?;
        if let Err(error) = execute!(
            stdout(),
            EnterAlternateScreen,
            Clear(ClearType::All),
            MoveTo(0, 0),
            Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        self.start_row = 0;
        self.raw_mode_enabled = true;
        self.alternate_screen_enabled = true;
        self.initialized = true;

        Ok(())
    }

    fn redraw_compact(&mut self, term_width: u16) -> io::Result<()> {
        let progress = (self.data.current_bytes as f64 / self.data.total_bytes.max(1) as f64
            * 100.0)
            .min(100.0) as u16;
        let operation = if self.data.operation_type.is_empty() {
            "Progress"
        } else {
            self.data.operation_type.as_str()
        };

        let mut line = format!(
            "{} {:>3}% · {} / {}",
            operation,
            progress,
            format_bytes(self.data.current_bytes as f64),
            format_bytes(self.data.total_bytes as f64)
        );
        line = clip_line_to_width(&line, term_width);

        let mut output = stdout();
        execute!(
            output,
            MoveTo(0, 0),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(parse_hex_color(&CONFIG.progress.theme.text_color))
        )?;
        write!(output, "{line}")?;
        execute!(
            output,
            Clear(ClearType::UntilNewLine),
            SetAttribute(Attribute::Reset)
        )?;
        output.flush()?;
        self.last_rendered_lines = 1;
        Ok(())
    }

    fn redraw(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }

        if self.suspended.load(Ordering::SeqCst) {
            return Ok(());
        }

        if !self.initialized {
            self.initialize()?;
        }

        let (term_width, term_height) = terminal_size::terminal_size()
            .map(|(width, height)| (width.0, height.0))
            .unwrap_or((80, 24));
        let compact_layout = use_compact_layout(term_width, term_height, self.total_lines());
        if self.last_terminal_size != Some((term_width, term_height))
            || self.compact_layout != compact_layout
        {
            execute!(stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
            self.start_row = 0;
            self.last_rendered_lines = 0;
        }
        self.last_terminal_size = Some((term_width, term_height));
        self.compact_layout = compact_layout;

        if compact_layout {
            return self.redraw_compact(term_width);
        }

        let new_lines = self.total_lines();
        if self.last_rendered_lines > new_lines {
            let mut stdout = stdout();
            for i in new_lines..self.last_rendered_lines {
                let _ = execute!(
                    stdout,
                    MoveTo(0, self.start_row + i),
                    Clear(ClearType::CurrentLine)
                );
            }
        }

        let display_file = if self.data.current_file.is_empty() {
            "File".to_string()
        } else {
            self.data.current_file.clone()
        };

        if event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                if key.modifiers.contains(event::KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('c') => {
                            self.finish()?;
                            std::process::exit(130);
                        }
                        KeyCode::Char('z') => {
                            suspend_now(&self.suspended);
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }

        let total_progress = (self.data.current_bytes as f64 / self.data.total_bytes.max(1) as f64
            * 100.0)
            .min(100.0) as u16;
        let current_progress = (self.data.current_file_progress as f64
            / self.data.current_file_size.max(1) as f64
            * 100.0)
            .min(100.0) as u16;
        let speed = self.data.calculate_speed();
        let eta_opt = self.data.estimate_eta();

        let operation = if self.data.operation_type.is_empty() {
            "Progress".to_string()
        } else {
            format!("{} Progress", self.data.operation_type)
        };

        let mut stdout = stdout();

        let box_width = term_width as usize;
        let right_border_col = term_width.saturating_sub(2);

        let theme = &CONFIG.progress.theme;
        let layout = &CONFIG.progress.layout;

        let (top_left, top_right, bottom_left, bottom_right, horizontal, vertical) =
            match layout.box_style.as_str() {
                "double" => ('╔', '╗', '╚', '╝', '═', '║'),
                "heavy" => ('┏', '┓', '┗', '┛', '━', '┃'),
                "single" => ('┌', '┐', '└', '┘', '─', '│'),
                _ => ('╭', '╮', '╰', '╯', '─', '│'),
            };

        let border_color = parse_hex_color(&theme.border_color);
        let title_color = parse_hex_color(&theme.title_color);
        let text_color = parse_hex_color(&theme.text_color);

        let current_row = self.start_row;

        let title_len = operation.len();

        execute!(
            stdout,
            MoveTo(0, current_row),
            SetForegroundColor(border_color)
        )?;
        write!(stdout, "{}{} ", top_left, horizontal)?;
        execute!(
            stdout,
            SetForegroundColor(title_color),
            SetAttribute(Attribute::Bold)
        )?;
        write!(stdout, "{}", operation)?;
        execute!(
            stdout,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(border_color)
        )?;

        let remaining_len = box_width.saturating_sub(title_len + 4);
        write!(
            stdout,
            " {}",
            horizontal
                .to_string()
                .repeat(remaining_len.saturating_sub(1))
        )?;
        execute!(stdout, MoveTo(right_border_col, current_row))?;
        write!(stdout, "{}", top_right)?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        let bar_width = (box_width.saturating_sub(20)).max(20);

        let draw_line_content =
            |out: &mut io::Stdout, row_offset: u16, content: &str| -> io::Result<()> {
                execute!(
                    out,
                    MoveTo(0, current_row + row_offset),
                    SetForegroundColor(border_color)
                )?;
                write!(out, "{} ", vertical)?;
                execute!(out, SetForegroundColor(text_color))?;

                let available_width = box_width.saturating_sub(3);
                let content_len = content.chars().count();
                let display_content = if content_len > available_width {
                    &content[..content.floor_char_boundary(available_width)]
                } else {
                    content
                };

                write!(out, "{}", display_content)?;

                let padding = available_width.saturating_sub(display_content.chars().count());
                if padding > 0 {
                    write!(out, "{}", " ".repeat(padding))?;
                }

                execute!(
                    out,
                    MoveTo(right_border_col, current_row + row_offset),
                    SetForegroundColor(border_color)
                )?;
                write!(out, "{}", vertical)?;
                execute!(out, Clear(ClearType::UntilNewLine))?;
                Ok(())
            };

        execute!(
            stdout,
            MoveTo(0, current_row + 1),
            SetForegroundColor(border_color)
        )?;
        write!(stdout, "{} Total:   [", vertical)?;

        let filled_len = (bar_width * total_progress as usize / 100).min(bar_width);
        let empty_len = bar_width - filled_len;

        for i in 0..filled_len {
            let progress_fraction = i as f32 / bar_width as f32;
            let color = get_gradient_color(&theme.bar_gradient, progress_fraction);
            execute!(stdout, SetForegroundColor(color))?;
            write!(stdout, "{}", theme.bar_complete_char)?;
        }

        execute!(stdout, SetForegroundColor(parse_hex_color("#444444")))?;
        write!(stdout, "{}", theme.bar_incomplete_char.repeat(empty_len))?;

        execute!(stdout, SetForegroundColor(text_color))?;
        write!(stdout, "] {:>3}%", total_progress)?;

        execute!(
            stdout,
            MoveTo(right_border_col, current_row + 1),
            SetForegroundColor(border_color)
        )?;
        write!(stdout, "{}", vertical)?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        let eta_str = match eta_opt {
            Some(d) => format_eta(d.as_secs()),
            None => "--".to_string(),
        };

        let speed_str = if speed > 0.0 {
            format!("{}/s", format_bytes(speed * 1024.0 * 1024.0))
        } else {
            "-- /s".to_string()
        };
        let details = if self.data.scanning {
            let dots = ".".repeat((self.data.elapsed().as_millis() / 500 % 4) as usize);
            format!(
                "Detail:  {} (scanning{} {} files found) | {}",
                format_bytes(self.data.current_bytes as f64),
                dots,
                self.data.files_found,
                speed_str,
            )
        } else {
            format!(
                "Detail:  {} / {} | {} | ETA: {}",
                format_bytes(self.data.current_bytes as f64),
                format_bytes(self.data.total_bytes as f64),
                speed_str,
                eta_str
            )
        };
        draw_line_content(&mut stdout, 2, &details)?;

        if self.data.parallel_total > 0 {
            let workers_header = format!(
                "Workers: {}/{}",
                self.data.active_worker_count(),
                self.data.parallel_total
            );
            draw_line_content(&mut stdout, 3, &workers_header)?;

            let num_width = if self.data.parallel_total >= 10 { 2 } else { 1 };
            let worker_bar_width = 20usize.min(box_width.saturating_sub(30));
            let fixed_chars = num_width + 26;
            let name_max = box_width
                .saturating_sub(worker_bar_width + fixed_chars)
                .max(8);

            for (i, worker) in self.data.workers.iter_mut().enumerate() {
                let row_offset = 4 + i as u16;
                if worker.active {
                    let pct = if worker.file_size > 0 {
                        (worker.progress as f64 / worker.file_size as f64 * 100.0).min(100.0) as u16
                    } else {
                        0
                    };
                    let spd = worker.calculate_speed();
                    let spd_str = if spd > 0.0 {
                        format!("{}/s", format_bytes(spd * 1024.0 * 1024.0))
                    } else {
                        "-- /s".to_string()
                    };
                    let display_name = if worker.file_name.chars().count() > name_max {
                        let end = worker
                            .file_name
                            .floor_char_boundary(name_max.saturating_sub(3));
                        format!("{}...", &worker.file_name[..end])
                    } else {
                        worker.file_name.clone()
                    };
                    let filled = (worker_bar_width * pct as usize / 100).min(worker_bar_width);
                    let empty = worker_bar_width - filled;

                    execute!(
                        stdout,
                        MoveTo(0, current_row + row_offset),
                        SetForegroundColor(border_color)
                    )?;
                    write!(stdout, "{} ", vertical)?;
                    execute!(stdout, SetForegroundColor(text_color))?;
                    write!(
                        stdout,
                        "[{:>width$}] {:name_w$} [",
                        i + 1,
                        display_name,
                        width = num_width,
                        name_w = name_max
                    )?;

                    for j in 0..filled {
                        let frac = j as f32 / worker_bar_width as f32;
                        let color = get_gradient_color(&theme.bar_gradient, frac);
                        execute!(stdout, SetForegroundColor(color))?;
                        write!(stdout, "{}", theme.bar_complete_char)?;
                    }
                    execute!(stdout, SetForegroundColor(parse_hex_color("#444444")))?;
                    write!(stdout, "{}", theme.bar_incomplete_char.repeat(empty))?;

                    execute!(stdout, SetForegroundColor(text_color))?;
                    write!(stdout, "] {:>3}% {}", pct, spd_str)?;

                    let content_len =
                        num_width + 3 + name_max + 2 + worker_bar_width + 2 + 4 + 1 + spd_str.len();
                    let padding = box_width.saturating_sub(content_len + 3);
                    write!(stdout, "{}", " ".repeat(padding))?;

                    execute!(
                        stdout,
                        MoveTo(right_border_col, current_row + row_offset),
                        SetForegroundColor(border_color)
                    )?;
                    write!(stdout, "{}", vertical)?;
                    execute!(stdout, Clear(ClearType::UntilNewLine))?;
                } else {
                    let line = format!("[{:>width$}] idle", i + 1, width = num_width);
                    draw_line_content(&mut stdout, row_offset, &line)?;
                }
            }

            let bottom_row = 4 + self.data.parallel_total as u16;
            execute!(
                stdout,
                MoveTo(0, current_row + bottom_row),
                SetForegroundColor(border_color)
            )?;
            write!(
                stdout,
                "{}{}{}",
                bottom_left,
                horizontal
                    .to_string()
                    .repeat(right_border_col.saturating_sub(1) as usize),
                bottom_right
            )?;
            execute!(stdout, SetAttribute(Attribute::Reset))?;
            execute!(stdout, Clear(ClearType::UntilNewLine))?;
        } else {
            let items_line = if let Some(total) = self.data.items_total {
                format!("Items:   {} / {}", self.data.items_processed, total)
            } else {
                String::new()
            };
            draw_line_content(&mut stdout, 3, &items_line)?;

            let file_info = format!("Current: {}", display_file);
            let max_text_width = box_width.saturating_sub(4);
            let display_file_info = if file_info.len() > max_text_width {
                let truncated =
                    &file_info[..file_info.floor_char_boundary(max_text_width.saturating_sub(3))];
                format!("{truncated}...")
            } else {
                file_info
            };
            draw_line_content(&mut stdout, 4, &display_file_info)?;

            execute!(
                stdout,
                MoveTo(0, current_row + 5),
                SetForegroundColor(border_color)
            )?;
            write!(stdout, "{}          [", vertical)?;
            let filled_len = (bar_width * current_progress as usize / 100).min(bar_width);
            let empty_len = bar_width - filled_len;

            for i in 0..filled_len {
                let progress_fraction = i as f32 / bar_width as f32;
                let color = get_gradient_color(&theme.bar_gradient, progress_fraction);
                execute!(stdout, SetForegroundColor(color))?;
                write!(stdout, "{}", theme.bar_complete_char)?;
            }
            execute!(stdout, SetForegroundColor(parse_hex_color("#444444")))?;
            write!(stdout, "{}", theme.bar_incomplete_char.repeat(empty_len))?;

            execute!(stdout, SetForegroundColor(text_color))?;
            write!(stdout, "] {:>3}%", current_progress)?;

            execute!(
                stdout,
                MoveTo(right_border_col, current_row + 5),
                SetForegroundColor(border_color)
            )?;
            write!(stdout, "{}", vertical)?;
            execute!(stdout, Clear(ClearType::UntilNewLine))?;

            execute!(
                stdout,
                MoveTo(0, current_row + 6),
                SetForegroundColor(border_color)
            )?;
            write!(
                stdout,
                "{}{}{}",
                bottom_left,
                horizontal
                    .to_string()
                    .repeat(right_border_col.saturating_sub(1) as usize),
                bottom_right
            )?;
            execute!(stdout, SetAttribute(Attribute::Reset))?;
            execute!(stdout, Clear(ClearType::UntilNewLine))?;
        }

        self.last_rendered_lines = self.total_lines();
        stdout.flush()?;
        Ok(())
    }
}

impl Drop for TuiProgress {
    fn drop(&mut self) {
        if !self.finished {
            if self.alternate_screen_enabled {
                let _ = execute!(stdout(), Show, LeaveAlternateScreen);
                self.alternate_screen_enabled = false;
            }
            if self.raw_mode_enabled {
                let _ = disable_raw_mode();
                self.raw_mode_enabled = false;
            }
        }
    }
}

impl ProgressRenderer for TuiProgress {
    fn set_total_items(&mut self, total: usize) {
        self.data.items_total = Some(total);
        let _ = self.redraw();
    }

    fn set_verify_mode(&mut self, on: bool) {
        self.data.verify_mode = on;
    }

    fn inc_reflink(&mut self) {
        self.data.reflink_count += 1;
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
        if self.finished {
            return Ok(());
        }

        let was_suspended = self.suspended.load(Ordering::SeqCst);

        let _ = self.redraw();

        if self.alternate_screen_enabled && !was_suspended {
            execute!(stdout(), Show, LeaveAlternateScreen)?;
            self.alternate_screen_enabled = false;
        }
        if self.raw_mode_enabled && !was_suspended {
            disable_raw_mode()?;
            self.raw_mode_enabled = false;
        }

        println!("{}", self.data.done_summary_line());

        self.finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{clip_line_to_width, use_compact_layout};

    #[test]
    fn compact_layout_handles_narrow_or_short_terminals() {
        assert!(!use_compact_layout(80, 24, 7));
        assert!(use_compact_layout(49, 24, 7));
        assert!(use_compact_layout(80, 6, 7));
        assert!(!use_compact_layout(50, 7, 7));
    }

    #[test]
    fn compact_line_never_exceeds_the_terminal_width() {
        let clipped = clip_line_to_width("Copying 42% · 8.00 MiB / 16.00 MiB", 20);
        assert_eq!(clipped.chars().count(), 20);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip_line_to_width("Copying", 20), "Copying");
        assert_eq!(clip_line_to_width("Copying", 1), "…");
    }
}
