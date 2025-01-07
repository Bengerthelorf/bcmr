use std::io::{self, stdout};
use std::time::{Duration, Instant};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
    cursor::{Hide, Show, MoveTo},
    event::{self, Event, KeyCode},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
    Terminal, backend::CrosstermBackend,
    text::{Line, Span},
};

pub struct CopyProgress {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    total_bytes: u64,
    current_bytes: u64,
    current_file: String,
    current_file_size: u64,
    current_file_progress: u64,
    last_update: Instant,
    last_bytes: u64,
    last_speed: f64,
    operation_type: String,
    items_total: Option<usize>,    // New: Total number of items to process
    items_processed: usize,        // New: Number of items processed
}

impl CopyProgress {
    pub fn new(total_bytes: u64) -> io::Result<Self> {
        let mut stdout = stdout();
        execute!(stdout, Hide)?;
        enable_raw_mode()?;
        
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        
        let now = Instant::now();
        Ok(Self {
            terminal,
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
        })
    }

    // New method to set total items
    pub fn set_total_items(&mut self, total: usize) {
        self.items_total = Some(total);
        self.items_processed = 0;
        self.redraw().unwrap();
    }

    // New method to increment processed items
    pub fn inc_items_processed(&mut self) {
        self.items_processed += 1;
        self.redraw().unwrap();
    }

    fn calculate_speed(&self) -> f64 {
        let elapsed = self.last_update.elapsed().as_secs_f64();
        if elapsed < 0.1 {
            return self.last_speed;
        }

        let bytes_per_sec = (self.current_bytes - self.last_bytes) as f64 / elapsed;
        let speed = bytes_per_sec / (1024.0 * 1024.0);

        // Use a smoother moving average
        if self.last_speed > 0.0 {
            self.last_speed * 0.8 + speed * 0.2
        } else {
            speed
        }
    }

    pub fn set_current_file(&mut self, file_name: &str, file_size: u64) {
        self.current_file = file_name.to_string();
        self.current_file_size = file_size;
        self.current_file_progress = 0;
        self.redraw().unwrap();
    }

    pub fn inc_current(&mut self, delta: u64) {
        self.current_bytes += delta;
        self.current_file_progress += delta;

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();

        // More frequent speed updates
        if elapsed >= 0.1 {
            let bytes_per_sec = (self.current_bytes - self.last_bytes) as f64 / elapsed;
            let speed = bytes_per_sec / (1024.0 * 1024.0);

            // Smoother speed updates
            self.last_speed = if self.last_speed > 0.0 {
                self.last_speed * 0.8 + speed * 0.2
            } else {
                speed
            };

            self.last_update = now;
            self.last_bytes = self.current_bytes;
        }

        self.redraw().unwrap();
    }

    pub fn set_operation_type(&mut self, operation: &str) {
        self.operation_type = operation.to_string();
        self.redraw().unwrap();
    }

    fn redraw(&mut self) -> io::Result<()> {
        // Check Ctrl+C
        if event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(event::KeyModifiers::CONTROL) {
                    self.finish()?;
                    std::process::exit(130);  // Standard exit code for Ctrl+C
                }
            }
        }

        // Pre-calculate all necessary values
        let total_bytes = self.total_bytes;
        let current_bytes = self.current_bytes;
        let current_file = self.current_file.clone();
        let current_file_size = self.current_file_size;
        let current_file_progress = self.current_file_progress;
        let speed = self.calculate_speed();
        let operation = if self.operation_type.is_empty() {
            "Progress".to_string()
        } else {
            format!("{} Progress", self.operation_type)
        };

        let total_progress = (current_bytes as f64 / total_bytes.max(1) as f64 * 100.0) as u16;
        let current_progress = (current_file_progress as f64 / current_file_size.max(1) as f64 * 100.0) as u16;

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
                width: f.size().width,
                height: if self.items_total.is_some() { 8 } else { 7 },
            };

            let mut constraints = vec![
                Constraint::Length(3),  // Total progress
                Constraint::Length(1),  // Total progress details
                Constraint::Length(3),  // Current file
            ];

            // Add extra space for items counter if needed
            if self.items_total.is_some() {
                constraints.push(Constraint::Length(1));  // Items progress
            }

            let main_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(display_area);

            // Render total progress
            let total_block = Block::default()
                .title(operation)
                .borders(Borders::ALL);
            f.render_widget(total_block, main_layout[0]);

            let gauge = Gauge::default()
                .block(Block::default())
                .gauge_style(Style::default().fg(Color::Cyan))
                .percent(total_progress)
                .label(format!("{}%", total_progress));
            f.render_widget(gauge, calculate_inner_rect(main_layout[0]));

            // Render progress details and speed on the same line
            let details = format!(
                "{:.2} MiB / {:.2} MiB    Speed: {:.2} MiB/s",
                current_bytes as f64 / 1024.0 / 1024.0,
                total_bytes as f64 / 1024.0 / 1024.0,
                speed
            );
            let total_detail = Paragraph::new(Line::from(vec![
                Span::raw(details)
            ]));
            f.render_widget(total_detail, main_layout[1]);

            // Render current file
            let current_block = Block::default()
                .title(format!("Current File: {}", current_file))
                .borders(Borders::ALL);
            f.render_widget(current_block, main_layout[2]);

            let current_gauge = Gauge::default()
                .block(Block::default())
                .gauge_style(Style::default().fg(Color::Cyan))
                .percent(current_progress)
                .label(format!("{}%", current_progress));
            f.render_widget(current_gauge, calculate_inner_rect(main_layout[2]));

            // Render items progress if available
            if let Some(total_items) = self.items_total {
                let items_progress = format!(
                    "Items processed: {} / {}",
                    self.items_processed,
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

    pub fn finish(&mut self) -> io::Result<()> {
        execute!(
            self.terminal.backend_mut(),
            Show,
            MoveTo(0, if self.items_total.is_some() { 8 } else { 7 })
        )?;
        disable_raw_mode()?;
        Ok(())
    }
}

impl Drop for CopyProgress {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}