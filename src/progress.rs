use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
    cursor::{Hide, Show, MoveTo, position},
    event::{self, Event, KeyCode},
};

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
    items_total: Option<usize>,    // Total number of items to process
    items_processed: usize,        // Number of items processed
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

// Fancy progress using crossterm for better positioning
struct FancyProgress {
    data: ProgressData,
    start_row: u16,
    start_col: u16,
    raw_mode_enabled: bool,
    initialized: bool,
    finished: bool,
}

impl FancyProgress {
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

        // Try to get cursor position, fallback to (0,0) if not available
        match position() {
            Ok((col, row)) => {
                self.start_col = col;
                self.start_row = row;
            }
            Err(_) => {
                // Fallback for non-interactive environments
                self.start_col = 0;
                self.start_row = 0;
            }
        }

        // Try to enable raw mode, but don't fail if it's not available
        let _ = enable_raw_mode();
        let _ = execute!(stdout(), Hide);
        
        self.raw_mode_enabled = true;
        self.initialized = true;

        Ok(())
    }

    fn redraw(&mut self) -> io::Result<()> {
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
                if key.code == KeyCode::Char('c') && key.modifiers.contains(event::KeyModifiers::CONTROL) {
                    self.finish()?;
                    std::process::exit(130);
                }
            }
        }

        let total_progress = (self.data.current_bytes as f64 / self.data.total_bytes.max(1) as f64 * 100.0) as u16;
        let current_progress = (self.data.current_file_progress as f64 / self.data.current_file_size.max(1) as f64 * 100.0) as u16;
        let speed = self.data.calculate_speed();
        let eta_opt = self.data.estimate_eta();

        let operation = if self.data.operation_type.is_empty() {
            "Progress".to_string()
        } else {
            format!("{} Progress", self.data.operation_type)
        };

        let mut stdout = stdout();
        
        // Use full terminal width for the progress display
        use terminal_size::{Width, Height, terminal_size};
        let (term_width, _) = terminal_size().unwrap_or((Width(80), Height(24)));
        let box_width = term_width.0 as usize;
        // Some terminals don't handle the rightmost column well, so use width-2
        let right_border_col = (term_width.0).saturating_sub(2);
        
        // Calculate progress bar width based on available space
        let progress_bar_width = (box_width.saturating_sub(20)).max(20); // Reserve space for text
        let bar_width = progress_bar_width;

        // Draw fancy box with Unicode characters
        let current_row = self.start_row;
        
        // Top border of main progress
        execute!(stdout, MoveTo(0, current_row))?;
        write!(stdout, "┌{}┐", "─".repeat(right_border_col as usize - 1))?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        // Operation title
        execute!(stdout, MoveTo(0, current_row + 1))?;
        // Draw left border and title
        write!(stdout, "│ {}", operation)?;
        // Draw right border at safe position
        execute!(stdout, MoveTo(right_border_col, current_row + 1))?;
        write!(stdout, "│")?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        // Progress bar
        execute!(stdout, MoveTo(0, current_row + 2))?;
        let filled_width = (bar_width * total_progress as usize / 100).min(bar_width);
        let empty_width = bar_width - filled_width;
        let progress_content = format!(
            " [{}{}] {}% ",
            "█".repeat(filled_width),
            "░".repeat(empty_width),
            total_progress
        );
        // Draw left border and content
        write!(stdout, "│{}", progress_content)?;
        // Draw right border at terminal edge
        execute!(stdout, MoveTo(right_border_col, current_row + 2))?;
        write!(stdout, "│")?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        // Progress details
        execute!(stdout, MoveTo(0, current_row + 3))?;
        let eta_str = match eta_opt {
            Some(d) => {
                if d.as_secs() == 0 && self.data.current_bytes < self.data.total_bytes {
                    "--".to_string()
                } else {
                    format_eta(d.as_secs())
                }
            }
            None => "--".to_string(),
        };

        let details_content = format!(
            " {} / {} | Speed: {}/s | ETA: {}",
            format_bytes(self.data.current_bytes as f64),
            format_bytes(self.data.total_bytes as f64),
            format_bytes(speed * 1024.0 * 1024.0),
            eta_str
        );
        // Draw left border and details
        write!(stdout, "│{}", details_content)?;
        // Draw right border at terminal edge
        execute!(stdout, MoveTo(right_border_col, current_row + 3))?;
        write!(stdout, "│")?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        // Bottom border of main progress / top of file progress
        execute!(stdout, MoveTo(0, current_row + 4))?;
        write!(stdout, "├{}┤", "─".repeat(right_border_col as usize - 1))?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        // Current file info
        execute!(stdout, MoveTo(0, current_row + 5))?;
        let file_info = format!("Current: {} ({})", 
            self.data.current_file, 
            format_bytes(self.data.current_file_size as f64)
        );
        let truncated_info = if file_info.len() > box_width.saturating_sub(4) {
            format!("{}...", &file_info[..box_width.saturating_sub(7)])
        } else {
            file_info
        };
        // Draw left border and file info
        write!(stdout, "│ {}", truncated_info)?;
        // Draw right border at terminal edge
        execute!(stdout, MoveTo(right_border_col, current_row + 5))?;
        write!(stdout, "│")?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        // Current file progress bar - always show this
        execute!(stdout, MoveTo(0, current_row + 6))?;
        let file_filled_width = (bar_width * current_progress as usize / 100).min(bar_width);
        let file_empty_width = bar_width.saturating_sub(file_filled_width);
        let file_progress_content = format!(
            " [{}{}] {}% ",
            "█".repeat(file_filled_width),
            "░".repeat(file_empty_width),
            current_progress
        );
        // Draw left border and file progress
        write!(stdout, "│{}", file_progress_content)?;
        // Draw right border at terminal edge
        execute!(stdout, MoveTo(right_border_col, current_row + 6))?;
        write!(stdout, "│")?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        let mut last_row = current_row + 7;

        // Items progress if available  
        if let Some(total_items) = self.data.items_total {
            execute!(stdout, MoveTo(0, last_row))?;
            write!(stdout, "├{}┤", "─".repeat(right_border_col as usize - 1))?;
            execute!(stdout, Clear(ClearType::UntilNewLine))?;

            execute!(stdout, MoveTo(0, last_row + 1))?;
            let items_info = format!("Items: {} / {}", self.data.items_processed, total_items);
            // Draw left border and items info
            write!(stdout, "│ {}", items_info)?;
            // Draw right border at terminal edge
            execute!(stdout, MoveTo(right_border_col, last_row + 1))?;
            write!(stdout, "│")?;
            execute!(stdout, Clear(ClearType::UntilNewLine))?;
            last_row += 2;
        }

        // Bottom border
        execute!(stdout, MoveTo(0, last_row))?;
        write!(stdout, "└{}┘", "─".repeat(right_border_col as usize - 1))?;
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
            let lines_used = if self.data.items_total.is_some() { 10 } else { 8 };
            execute!(stdout(), Show, MoveTo(0, self.start_row + lines_used))?;
            disable_raw_mode()?;
            self.raw_mode_enabled = false;
            println!();
        }
        
        self.finished = true;
        Ok(())
    }
}

// Plain progress using simple text
struct PlainProgress {
    data: ProgressData,
    start_row: u16,
    start_col: u16,
    raw_mode_enabled: bool,
    initialized: bool,
}

impl PlainProgress {
    fn new(total_bytes: u64) -> io::Result<Self> {
        let data = ProgressData::new(total_bytes);
        Ok(Self {
            data,
            start_row: 0,
            start_col: 0,
            raw_mode_enabled: false,
            initialized: false,
        })
    }

    fn initialize(&mut self) -> io::Result<()> {
        if self.initialized {
            return Ok(());
        }

        let (col, row) = position()?;
        self.start_col = col;
        self.start_row = row;

        enable_raw_mode()?;
        execute!(stdout(), Hide)?;
        self.raw_mode_enabled = true;
        self.initialized = true;

        Ok(())
    }

    fn create_progress_bar(&self, percent: u16, width: usize) -> String {
        let filled = (width * percent as usize / 100).min(width);
        let empty = width - filled;
        
        let mut bar = String::with_capacity(width);
        for _ in 0..filled {
            bar.push('=');
        }
        for _ in 0..empty {
            bar.push('-');
        }
        bar
    }

    fn redraw(&mut self) -> io::Result<()> {
        if !self.initialized {
            self.initialize()?;
        }

        // Check for Ctrl+C
        if event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(event::KeyModifiers::CONTROL) {
                    self.finish()?;
                    std::process::exit(130);
                }
            }
        }

        let mut stdout = stdout();
        
        let total_progress = (self.data.current_bytes as f64 / self.data.total_bytes.max(1) as f64 * 100.0) as u16;
        let current_progress = (self.data.current_file_progress as f64 / self.data.current_file_size.max(1) as f64 * 100.0) as u16;
        let speed = self.data.calculate_speed();
        let eta_opt = self.data.estimate_eta();

        let operation = if self.data.operation_type.is_empty() {
            "Progress".to_string()
        } else {
            format!("{} Progress", self.data.operation_type)
        };

        execute!(stdout, MoveTo(self.start_col, self.start_row))?;

        let eta_str = match eta_opt {
            Some(d) => {
                if d.as_secs() == 0 && self.data.current_bytes < self.data.total_bytes {
                    "--".to_string()
                } else {
                    format_eta(d.as_secs())
                }
            }
            None => "--".to_string(),
        };

        let total_line = format!(
            "{}: [{}] {}% | {} / {} | Speed: {}/s | ETA: {}",
            operation,
            self.create_progress_bar(total_progress, 30),
            total_progress,
            format_bytes(self.data.current_bytes as f64),
            format_bytes(self.data.total_bytes as f64),
            format_bytes(speed * 1024.0 * 1024.0),
            eta_str
        );
        write!(stdout, "{}", total_line)?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        execute!(stdout, MoveTo(self.start_col, self.start_row + 1))?;
        let file_line = format!(
            "File: {} [{}] {}%",
            self.data.current_file,
            self.create_progress_bar(current_progress, 30),
            current_progress
        );
        write!(stdout, "{}", file_line)?;
        execute!(stdout, Clear(ClearType::UntilNewLine))?;

        if let Some(total_items) = self.data.items_total {
            execute!(stdout, MoveTo(self.start_col, self.start_row + 2))?;
            let items_line = format!(
                "Items: {} / {}",
                self.data.items_processed,
                total_items
            );
            write!(stdout, "{}", items_line)?;
            execute!(stdout, Clear(ClearType::UntilNewLine))?;
        }

        stdout.flush()?;
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.raw_mode_enabled {
            let lines_used = if self.data.items_total.is_some() { 3 } else { 2 };
            execute!(stdout(), Show, MoveTo(0, self.start_row + lines_used))?;
            disable_raw_mode()?;
            self.raw_mode_enabled = false;
            println!();
        }
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
    fn finish(&mut self) -> io::Result<()>;
}

impl ProgressRenderer for FancyProgress {
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
        if self.data.current_bytes % (1024 * 1024) == 0 || 
           self.data.current_bytes >= self.data.total_bytes {
            let _ = self.redraw();
        }
    }

    fn set_operation_type(&mut self, operation: &str) {
        self.data.operation_type = operation.to_string();
        let _ = self.redraw();
    }

    fn finish(&mut self) -> io::Result<()> {
        self.finish()
    }
}

impl ProgressRenderer for PlainProgress {
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

    fn finish(&mut self) -> io::Result<()> {
        self.finish()
    }
}

impl CopyProgress {
    pub fn new(total_bytes: u64, plain_mode: bool) -> io::Result<Self> {
        let inner: Box<dyn ProgressRenderer> = if plain_mode {
            Box::new(PlainProgress::new(total_bytes)?)
        } else {
            Box::new(FancyProgress::new(total_bytes)?)
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

    pub fn finish(&mut self) -> io::Result<()> {
        self.inner.finish()
    }
}

impl Drop for CopyProgress {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
