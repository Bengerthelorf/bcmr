use std::time::{Duration, Instant};

pub struct ProgressData {
    pub total_bytes: u64,
    pub current_bytes: u64,
    pub current_file: String,
    pub current_file_size: u64,
    pub current_file_progress: u64,
    pub last_update: Instant,
    pub last_bytes: u64,
    pub last_speed: f64,
    pub operation_type: String,
    pub items_total: Option<usize>, // Total number of items to process
    pub items_processed: usize,     // Number of items processed
}

impl ProgressData {
    pub fn new(total_bytes: u64) -> Self {
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

    pub fn calculate_speed(&mut self) -> f64 {
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

    pub fn estimate_eta(&self) -> Option<Duration> {
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
