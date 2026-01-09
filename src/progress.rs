use crate::config::CONFIG;
use crossterm::{
    cursor::{position, Hide, MoveTo, MoveToColumn, Show},
    event::{self, Event, KeyCode},
    execute,
    style::{Attribute, Color, SetAttribute, SetForegroundColor},
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};

/// Converts a byte count into a human-readable format
fn format_bytes(bytes: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;

    if bytes < KB {
        format!("{:.0} B", bytes)
    } else if bytes < MB {
        format!("{:.2} KiB", bytes / KB)
    } else if bytes < GB {
        format!("{:.2} MiB", bytes / MB)
    } else if bytes < TB {
        format!("{:.2} GiB", bytes / GB)
    } else {
        format!("{:.2} TiB", bytes / TB)
    }
}

/// Formats a duration in seconds into a human-readable HH:MM:SS or MM:SS string
fn format_eta(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, secs)
    } else {
        format!("{:02}:{:02}", minutes, secs)
    }
}

fn parse_hex_color(hex: &str) -> Color {
    if hex.starts_with('#') && hex.len() == 7 {
        let r = u8::from_str_radix(&hex[1..3], 16).unwrap_or(255);
        let g = u8::from_str_radix(&hex[3..5], 16).unwrap_or(255);
        let b = u8::from_str_radix(&hex[5..7], 16).unwrap_or(255);
        Color::Rgb { r, g, b }
    } else {
        match hex.to_lowercase().as_str() {
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "white" => Color::White,
            "reset" => Color::Reset,
            _ => Color::White,
        }
    }
}

fn interpolate_color(c1: Color, c2: Color, t: f32) -> Color {
    match (c1, c2) {
        (
            Color::Rgb {
                r: r1,
                g: g1,
                b: b1,
            },
            Color::Rgb {
                r: r2,
                g: g2,
                b: b2,
            },
        ) => {
            let r = (r1 as f32 + (r2 as f32 - r1 as f32) * t) as u8;
            let g = (g1 as f32 + (g2 as f32 - g1 as f32) * t) as u8;
            let b = (b1 as f32 + (b2 as f32 - b1 as f32) * t) as u8;
            Color::Rgb { r, g, b }
        }
        _ => c1,
    }
}

fn get_gradient_color(colors: &[String], progress: f32) -> Color {
    if colors.is_empty() {
        return Color::White;
    }
    if colors.len() == 1 {
        return parse_hex_color(&colors[0]);
    }

    let segments = colors.len() - 1;
    let segment_len = 1.0 / segments as f32;

    let segment_idx = (progress / segment_len).floor() as usize;
    let segment_idx = segment_idx.min(segments - 1);

    let t = (progress - (segment_idx as f32 * segment_len)) / segment_len;

    let c1 = parse_hex_color(&colors[segment_idx]);
    let c2 = parse_hex_color(&colors[segment_idx + 1]);

    interpolate_color(c1, c2, t)
}

struct ProgressData {
    total_bytes: u64,
    current_bytes: u64,
    current_file: String,
    current_file_size: u64,
    current_file_progress: u64,
    last_update: Instant,
    last_bytes: u64,
    last_speed: f64,
    operation_type: String,
    items_total: Option<usize>, // Total number of items to process
    items_processed: usize,     // Number of items processed
}

impl ProgressData {
    fn new(total_bytes: u64) -> Self {
        let now = Instant::now();
        Self {
            total_bytes,
            current_bytes: 0,
            current_file: String::new(),
            current_file_size: 0,
            current_file_progress: 0,
            last_update: now,
            last_bytes: 0,
            last_speed: 0.0,
            operation_type: String::new(),
            items_total: None,
            items_processed: 0,
        }
    }

    fn calculate_speed(&mut self) -> f64 {
        let elapsed = self.last_update.elapsed().as_secs_f64();
        if elapsed < 0.1 {
            return self.last_speed;
        }

        let bytes_per_sec = (self.current_bytes - self.last_bytes) as f64 / elapsed;
        let speed = bytes_per_sec / (1024.0 * 1024.0);

        self.last_speed = if self.last_speed > 0.0 {
            self.last_speed * 0.8 + speed * 0.2
        } else {
            speed
        };

        self.last_update = Instant::now();
        self.last_bytes = self.current_bytes;

        self.last_speed
    }

    fn estimate_eta(&self) -> Option<Duration> {
        if self.total_bytes == 0 || self.current_bytes >= self.total_bytes {
            return Some(Duration::from_secs(0));
        }
        // last_speed is in MiB/s
        if self.last_speed <= 0.0 {
            return None;
        }
        let remaining_bytes = self.total_bytes.saturating_sub(self.current_bytes) as f64;
        let bytes_per_sec = self.last_speed * 1024.0 * 1024.0; // convert MiB/s -> B/s
        if bytes_per_sec <= 0.0 {
            return None;
        }
        let secs = (remaining_bytes / bytes_per_sec).ceil() as u64;
        Some(Duration::from_secs(secs))
    }
}

// TUI progress using crossterm for box interface
struct TuiProgress {
    data: ProgressData,
    start_row: u16,
    start_col: u16,
    raw_mode_enabled: bool,
    initialized: bool,
    finished: bool,
}

impl TuiProgress {
    fn new(total_bytes: u64) -> io::Result<Self> {
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

        // Calculate required height based on content
        let required_height = if self.data.items_total.is_some() {
            10
        } else {
            8
        };

        // Ensure we have enough space at the bottom
        let (_, term_height) = terminal_size::terminal_size()
            .map(|(w, h)| (w.0, h.0))
            .unwrap_or((80, 24));

        let (_col, mut row) = position().unwrap_or((0, 0));

        // If we are too close to the bottom, scroll up
        if row + required_height > term_height {
            let lines_to_scroll = (row + required_height).saturating_sub(term_height);
            for _ in 0..lines_to_scroll {
                println!();
            }
            // After printing newlines, our row effectively moves up relative to the viewport
            // But actually the cursor is now at the bottom.
            // We want to start drawing 'lines_to_scroll' lines above the current position?
            // No, simpler: Print required_height newlines to force scroll, then move cursor up.

            // Let's rely on standard scrolling behavior:
            let (_new_col, new_row) = position().unwrap_or((0, 0));
            // Recalculate if we are still at the bottom
            row = new_row;
            if row + required_height > term_height {
                // We are at the bottom. The space is effectively "above" us now because we scrolled.
                // We need to set start_row so that start_row + height <= term_height.
                // So start_row should be term_height - required_height.
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

        // Check for Ctrl+C
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

        // Use full terminal width for the progress display
        use terminal_size::{terminal_size, Height, Width};
        let (term_width, _) = terminal_size().unwrap_or((Width(80), Height(24)));
        let box_width = term_width.0 as usize;
        let right_border_col = (term_width.0).saturating_sub(2);

        // Load configuration
        let theme = &CONFIG.progress.theme;
        let layout = &CONFIG.progress.layout;

        // Define box characters based on style
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

        // Draw title
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
        // Calculate remaining length: total width - (corner + horiz + space + title + space + corner)
        // Actually we just overwrite the rest.
        // Let's keep it simple: Top border spans purely, title is inside or overlay?
        // Design mock showed: ╭─ Copying ──────────────────╮

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

        // Helper to draw a line with borders
        // Fix: Pass stdout as argument to avoid borrow checker issues
        let draw_line_content =
            |out: &mut io::Stdout, row_offset: u16, content: &str| -> io::Result<()> {
                execute!(
                    out,
                    MoveTo(0, current_row + row_offset),
                    SetForegroundColor(border_color)
                )?;
                write!(out, "{} ", vertical)?;
                execute!(out, SetForegroundColor(text_color))?;
                write!(out, "{}", content)?;
                execute!(
                    out,
                    MoveTo(right_border_col, current_row + row_offset),
                    SetForegroundColor(border_color)
                )?;
                write!(out, "{}", vertical)?;
                execute!(out, Clear(ClearType::UntilNewLine))?;
                Ok(())
            };

        // --- Line 1: Main Progress Bar ---
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
        write!(stdout, "] {}%", total_progress)?;

        execute!(
            stdout,
            MoveTo(right_border_col, current_row + 1),
            SetForegroundColor(border_color)
        )?;
        write!(stdout, "{}", vertical)?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        // --- Line 2: Details (Size, Speed, ETA) ---
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

        // --- Line 3: Spacer ---
        draw_line_content(&mut stdout, 3, "")?;

        // --- Line 4: Current File Name ---
        let file_info = format!("Current: {}", self.data.current_file);
        // Truncate if too long (account for borders and padding)
        let max_text_width = box_width.saturating_sub(4);
        let display_file_info = if file_info.len() > max_text_width {
            format!("{}...", &file_info[..max_text_width.saturating_sub(3)])
        } else {
            file_info
        };
        draw_line_content(&mut stdout, 4, &display_file_info)?;

        // --- Line 5: Current File Bar ---
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
        write!(stdout, "] {}%", current_progress)?;

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

// Inline progress using multi-line colored output
struct InlineProgress {
    data: ProgressData,
    initialized: bool,
    lines_printed: u16,
}

impl InlineProgress {
    fn new(total_bytes: u64) -> io::Result<Self> {
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

        // Cursor movement for overwriting
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

        // Line 1: Operation + Total Bar
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

        // Line 2: Stats
        execute!(stdout, Clear(ClearType::CurrentLine))?;
        writeln!(
            stdout,
            "{} / {} | {}/s | ETA: {}",
            format_bytes(self.data.current_bytes as f64),
            format_bytes(self.data.total_bytes as f64),
            format_bytes(speed * 1024.0 * 1024.0),
            eta_str
        )?;

        // Line 3: File + Bar
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

        self.lines_printed = 2; // We printed 2 newlines (L1->L2, L2->L3), cursor at end of L3
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        println!();
        Ok(())
    }
}

// Public interface
pub struct CopyProgress {
    inner: Box<dyn ProgressRenderer>,
}

trait ProgressRenderer: Send {
    fn set_total_items(&mut self, total: usize);
    fn inc_items_processed(&mut self);
    fn set_current_file(&mut self, file_name: &str, file_size: u64);
    fn inc_current(&mut self, delta: u64);
    fn set_operation_type(&mut self, operation: &str);
    fn tick(&mut self);
    fn finish(&mut self) -> io::Result<()>;
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
        self.finish()
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
        // Redraw on tick to update ETA/Speed
        let _ = self.redraw();
    }

    fn finish(&mut self) -> io::Result<()> {
        self.finish()
    }
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

impl Drop for CopyProgress {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
