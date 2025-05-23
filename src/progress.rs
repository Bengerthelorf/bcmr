use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
    cursor::{Hide, Show, MoveTo, position},
    event::{self, Event, KeyCode},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
    Terminal, backend::CrosstermBackend,
    text::{Line, Span},
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
}

// ratatui
struct FancyProgress {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    data: ProgressData,
}

impl FancyProgress {
    fn new(total_bytes: u64) -> io::Result<Self> {
        let mut stdout = stdout();
        execute!(stdout, Hide)?;
        enable_raw_mode()?;
        
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        let data = ProgressData::new(total_bytes);
        
        Ok(Self { terminal, data })
    }

    fn redraw(&mut self) -> io::Result<()> {
        // Check Ctrl+C
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

        let operation = if self.data.operation_type.is_empty() {
            "Progress".to_string()
        } else {
            format!("{} Progress", self.data.operation_type)
        };

        let calculate_inner_rect = |rect: Rect| -> Rect {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1)])
                .margin(1)
                .split(rect)[0]
        };
        
        self.terminal.draw(|f| {
            let display_area = Rect {
                x: 0,
                y: 0,
                width: f.area().width,
                height: if self.data.items_total.is_some() { 8 } else { 7 },
            };

            let mut constraints = vec![
                Constraint::Length(3),  // Total progress
                Constraint::Length(1),  // Total progress details
                Constraint::Length(3),  // Current file
            ];

            if self.data.items_total.is_some() {
                constraints.push(Constraint::Length(1));  // Items progress
            }

            let main_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(display_area);

            // Render total progress
            let total_block = Block::default()
                .title(operation.clone())
                .borders(Borders::ALL);
            f.render_widget(total_block, main_layout[0]);

            let gauge = Gauge::default()
                .block(Block::default())
                .gauge_style(Style::default().fg(Color::Cyan))
                .percent(total_progress)
                .label(format!("{}%", total_progress));
            f.render_widget(gauge, calculate_inner_rect(main_layout[0]));

            // Render progress details
            let details = format!(
                "{} / {}    Speed: {}/s",
                format_bytes(self.data.current_bytes as f64),
                format_bytes(self.data.total_bytes as f64),
                format_bytes(speed * 1024.0 * 1024.0)
            );
            let total_detail = Paragraph::new(Line::from(vec![
                Span::raw(details)
            ]));
            f.render_widget(total_detail, main_layout[1]);

            // Render current file status
            let current_file_info = format!(
                "Current File: {} ({})",
                self.data.current_file,
                format_bytes(self.data.current_file_size as f64)
            );
            let current_block = Block::default()
                .title(current_file_info)
                .borders(Borders::ALL);
            f.render_widget(current_block, main_layout[2]);

            let current_gauge = Gauge::default()
                .block(Block::default())
                .gauge_style(Style::default().fg(Color::Cyan))
                .percent(current_progress)
                .label(format!("{}%", current_progress));
            f.render_widget(current_gauge, calculate_inner_rect(main_layout[2]));

            // Render items progress if available
            if let Some(total_items) = self.data.items_total {
                let items_progress = format!(
                    "Items processed: {} / {}",
                    self.data.items_processed,
                    total_items
                );
                let items_detail = Paragraph::new(Line::from(vec![
                    Span::raw(items_progress)
                ]));
                f.render_widget(items_detail, main_layout[3]);
            }
        })?;

        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        execute!(
            self.terminal.backend_mut(),
            Show,
            MoveTo(0, if self.data.items_total.is_some() { 8 } else { 7 })
        )?;
        disable_raw_mode()?;
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

        let operation = if self.data.operation_type.is_empty() {
            "Progress".to_string()
        } else {
            format!("{} Progress", self.data.operation_type)
        };

        execute!(stdout, MoveTo(self.start_col, self.start_row))?;

        let total_line = format!(
            "{}: [{}] {}% | {} / {} | Speed: {}/s",
            operation,
            self.create_progress_bar(total_progress, 30),
            total_progress,
            format_bytes(self.data.current_bytes as f64),
            format_bytes(self.data.total_bytes as f64),
            format_bytes(speed * 1024.0 * 1024.0)
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