use crate::config::CONFIG;
use crate::ui::progress::ProgressRenderer;
use crate::ui::state::ProgressData;
use crate::ui::utils::{format_bytes, format_eta, get_gradient_color, parse_hex_color};
use crossterm::{
    cursor::{position, Hide, MoveTo, Show},
    event::{self, Event, KeyCode},
    execute,
    style::{Attribute, SetAttribute, SetForegroundColor},
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use std::io::{self, stdout, Write};
use std::time::Duration;

// TUI progress
pub struct TuiProgress {
    data: ProgressData,
    start_row: u16,
    start_col: u16,
    raw_mode_enabled: bool,
    initialized: bool,
    finished: bool,
}

impl TuiProgress {
    pub fn new(total_bytes: u64) -> io::Result<Self> {
        let data = ProgressData::new(total_bytes);
        Ok(Self {
            data,
            start_row: 0,
            start_col: 0,
            raw_mode_enabled: false,
            initialized: false,
            finished: false,
        })
    }

    fn initialize(&mut self) -> io::Result<()> {
        if self.initialized {
            return Ok(());
        }

        // Calc height
        let required_height = if self.data.items_total.is_some() {
            10
        } else {
            8
        };

        // Ensure space
        let (_, term_height) = terminal_size::terminal_size()
            .map(|(w, h)| (w.0, h.0))
            .unwrap_or((80, 24));

        let (_col, mut row) = position().unwrap_or((0, 0));

        // Scroll if needed
        if row + required_height > term_height {
            let lines_to_scroll = (row + required_height).saturating_sub(term_height);
            for _ in 0..lines_to_scroll {
                println!();
            }
            let (_new_col, new_row) = position().unwrap_or((0, 0));
            row = new_row;
            if row + required_height > term_height {
                row = term_height.saturating_sub(required_height);
            }
        }

        self.start_col = 0; // Always start at column 0 for clean look
        self.start_row = row;

        // Try to enable raw mode
        let _ = enable_raw_mode();
        let _ = execute!(stdout(), Hide);

        self.raw_mode_enabled = true;
        self.initialized = true;

        Ok(())
    }

    fn redraw(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }

        if !self.initialized {
            self.initialize()?;
        }

        // Ensure we have some minimum data to display
        if self.data.current_file.is_empty() {
            self.data.current_file = "File".to_string();
        }

        // Poll Ctrl+C
        if event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(event::KeyModifiers::CONTROL)
                {
                    self.finish()?;
                    std::process::exit(130);
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

        // Use full width
        use terminal_size::{terminal_size, Height, Width};
        let (term_width, _) = terminal_size().unwrap_or((Width(80), Height(24)));
        let box_width = term_width.0 as usize;
        let right_border_col = (term_width.0).saturating_sub(2);

        // Load configuration
        let theme = &CONFIG.progress.theme;
        let layout = &CONFIG.progress.layout;

        // Box chars
        let (top_left, top_right, bottom_left, bottom_right, horizontal, vertical) =
            match layout.box_style.as_str() {
                "double" => ('╔', '╗', '╚', '╝', '═', '║'),
                "heavy" => ('┏', '┓', '┗', '┛', '━', '┃'),
                "single" => ('┌', '┐', '└', '┘', '─', '│'),
                _ => ('╭', '╮', '╰', '╯', '─', '│'), // rounded is default
            };

        let border_color = parse_hex_color(&theme.border_color);
        let title_color = parse_hex_color(&theme.title_color);
        let text_color = parse_hex_color(&theme.text_color);

        // Draw fancy box
        let current_row = self.start_row;

        // --- Top Border ---
        execute!(
            stdout,
            MoveTo(0, current_row),
            SetForegroundColor(border_color)
        )?;
        write!(stdout, "{}{} {}", top_left, horizontal, operation)?; // Start of title box

        // Title
        execute!(
            stdout,
            SetForegroundColor(title_color),
            SetAttribute(Attribute::Bold)
        )?;
        write!(stdout, "{}", operation)?;

        // Continue border
        execute!(
            stdout,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(border_color)
        )?;

        let title_len = operation.len();

        // Reset and just draw the top line with title embedded
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

        // Variables for content rendering
        let bar_width = (box_width.saturating_sub(20)).max(20);

        // Helper to draw line content
        let draw_line_content =
            |out: &mut io::Stdout, row_offset: u16, content: &str| -> io::Result<()> {
                execute!(
                    out,
                    MoveTo(0, current_row + row_offset),
                    SetForegroundColor(border_color)
                )?;
                write!(out, "{} ", vertical)?;
                execute!(out, SetForegroundColor(text_color))?;
                
                // box_width - 2 (borders) - 1 (left space) -> max content width
                let available_width = box_width.saturating_sub(3); // -2 borders, -1 left space
                let content_len = content.chars().count(); 
                let display_content = if content_len > available_width {
                    &content[..available_width]
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

        // --- L1: Main Bar ---
        execute!(
            stdout,
            MoveTo(0, current_row + 1),
            SetForegroundColor(border_color)
        )?;
        write!(stdout, "{} Total:   [", vertical)?;

        // Render Gradient Bar
        let filled_len = (bar_width * total_progress as usize / 100).min(bar_width);
        let empty_len = bar_width - filled_len;

        for i in 0..filled_len {
            let progress_fraction = i as f32 / bar_width as f32;
            let color = get_gradient_color(&theme.bar_gradient, progress_fraction);
            execute!(stdout, SetForegroundColor(color))?;
            write!(stdout, "{}", theme.bar_complete_char)?;
        }

        execute!(stdout, SetForegroundColor(parse_hex_color("#444444")))?; // Dim color for empty
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

        // --- L2: Details ---
        let eta_str = match eta_opt {
            Some(d) => format_eta(d.as_secs()),
            None => "--".to_string(),
        };

        let details = format!(
            "Detail:  {} / {} | {}/s | ETA: {}",
            format_bytes(self.data.current_bytes as f64),
            format_bytes(self.data.total_bytes as f64),
            format_bytes(speed * 1024.0 * 1024.0),
            eta_str
        );
        draw_line_content(&mut stdout, 2, &details)?;

        // --- L3: Spacer ---
        draw_line_content(&mut stdout, 3, "")?;

        // --- L4: Current File ---
        let file_info = format!("Current: {}", self.data.current_file);
        // Truncate if too long (account for borders and padding)
        let max_text_width = box_width.saturating_sub(4);
        let display_file_info = if file_info.len() > max_text_width {
            format!("{}...", &file_info[..max_text_width.saturating_sub(3)])
        } else {
            file_info
        };
        draw_line_content(&mut stdout, 4, &display_file_info)?;

        // --- L5: File Bar ---
        execute!(
            stdout,
            MoveTo(0, current_row + 5),
            SetForegroundColor(border_color)
        )?;
        write!(stdout, "{}          [", vertical)?; // Indent to match "Current: " roughly? No, let's align with main bar "Total:   " is 9 chars

        // For file bar, use a simpler single color or same gradient? Let's use same gradient logic for consistency
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

        // --- Bottom Border ---
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

        stdout.flush()?;
        Ok(())
    }
}

impl ProgressRenderer for TuiProgress {
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
        // Always redraw to show the initial progress display
        let _ = self.redraw();
    }

    fn inc_current(&mut self, delta: u64) {
        self.data.current_bytes += delta;
        self.data.current_file_progress += delta;
        // Only redraw every 1MB to reduce flicker and improve visibility
        if self.data.current_bytes % (1024 * 1024) == 0
            || self.data.current_bytes >= self.data.total_bytes
        {
            let _ = self.redraw();
        }
    }

    fn set_operation_type(&mut self, operation: &str) {
        self.data.operation_type = operation.to_string();
        let _ = self.redraw();
    }

    fn tick(&mut self) {
        let _ = self.redraw();
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }

        // Make sure to show final progress state
        let _ = self.redraw();

        if self.raw_mode_enabled {
            let lines_used = if self.data.items_total.is_some() {
                10
            } else {
                8
            };
            execute!(stdout(), Show, MoveTo(0, self.start_row + lines_used))?;
            disable_raw_mode()?;
            self.raw_mode_enabled = false;
            println!();
        }

        self.finished = true;
        Ok(())
    }
}
