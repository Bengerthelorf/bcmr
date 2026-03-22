use std::time::{Duration, Instant};

pub struct WorkerState {
    pub file_name: String,
    pub file_size: u64,
    pub progress: u64,
    pub active: bool,
    pub last_update: Instant,
    pub last_bytes: u64,
    pub speed: f64,
}

impl WorkerState {
    pub fn new() -> Self {
        Self {
            file_name: String::new(),
            file_size: 0,
            progress: 0,
            active: false,
            last_update: Instant::now(),
            last_bytes: 0,
            speed: 0.0,
        }
    }

    pub fn calculate_speed(&mut self) -> f64 {
        let elapsed = self.last_update.elapsed().as_secs_f64();
        if elapsed < 0.1 {
            return self.speed;
        }
        let bytes_per_sec = self.progress.saturating_sub(self.last_bytes) as f64 / elapsed;
        let speed = bytes_per_sec / (1024.0 * 1024.0);
        self.speed = if self.speed > 0.0 {
            self.speed * 0.8 + speed * 0.2
        } else {
            speed
        };
        self.last_update = Instant::now();
        self.last_bytes = self.progress;
        self.speed
    }
}

pub struct ProgressData {
    pub total_bytes: u64,
    pub current_bytes: u64,
    pub skipped_bytes: u64,
    pub current_file: String,
    pub current_file_size: u64,
    pub current_file_progress: u64,

    pub start_time: Instant,
    pub last_update: Instant,
    pub last_transferred: u64,
    pub last_speed: f64,

    pub operation_type: String,
    pub items_total: Option<usize>,
    pub items_processed: usize,
    pub scanning: bool,
    pub files_found: u64,
    pub workers: Vec<WorkerState>,
    pub parallel_total: usize,
}

impl ProgressData {
    pub fn new(total_bytes: u64) -> Self {
        let now = Instant::now();
        Self {
            total_bytes,
            current_bytes: 0,
            skipped_bytes: 0,
            current_file: String::new(),
            current_file_size: 0,
            current_file_progress: 0,

            start_time: now,
            last_update: now,
            last_transferred: 0,
            last_speed: 0.0,

            operation_type: String::new(),
            items_total: None,
            items_processed: 0,
            scanning: false,
            files_found: 0,
            workers: Vec::new(),
            parallel_total: 0,
        }
    }

    pub fn init_workers(&mut self, count: usize) {
        self.parallel_total = count;
        self.workers = (0..count).map(|_| WorkerState::new()).collect();
    }

    pub fn active_worker_count(&self) -> usize {
        self.workers.iter().filter(|w| w.active).count()
    }

    pub fn update_worker(&mut self, slot: usize, file_name: &str, file_size: u64, progress: u64) {
        let Some(w) = self.workers.get_mut(slot) else {
            return;
        };
        if !w.active || w.file_name != file_name {
            w.last_bytes = 0;
            w.last_update = Instant::now();
            w.speed = 0.0;
        }
        w.file_name = file_name.to_string();
        w.file_size = file_size;
        w.progress = progress;
        w.active = true;
    }

    pub fn finish_worker(&mut self, slot: usize) {
        let Some(w) = self.workers.get_mut(slot) else {
            return;
        };
        w.active = false;
        w.file_name.clear();
        w.progress = 0;
        w.file_size = 0;
        w.speed = 0.0;
        w.last_bytes = 0;
    }

    pub fn calculate_speed(&mut self) -> f64 {
        let elapsed = self.last_update.elapsed().as_secs_f64();
        if elapsed < 0.1 {
            return self.last_speed;
        }

        let transferred = self.current_bytes.saturating_sub(self.skipped_bytes);
        let bytes_per_sec = transferred.saturating_sub(self.last_transferred) as f64 / elapsed;
        let speed = bytes_per_sec / (1024.0 * 1024.0);

        self.last_speed = if self.last_speed > 0.0 {
            self.last_speed * 0.8 + speed * 0.2
        } else {
            speed
        };

        self.last_update = Instant::now();
        self.last_transferred = transferred;

        self.last_speed
    }

    pub fn inc_skipped(&mut self, delta: u64) {
        self.current_bytes += delta;
        self.skipped_bytes += delta;
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

    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn average_bytes_per_sec(&self) -> Option<f64> {
        let secs = self.elapsed().as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        Some(self.current_bytes as f64 / secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_data_new() {
        let pd = ProgressData::new(1000);
        assert_eq!(pd.total_bytes, 1000);
        assert_eq!(pd.current_bytes, 0);
        assert_eq!(pd.skipped_bytes, 0);
    }

    #[test]
    fn test_inc_skipped() {
        let mut pd = ProgressData::new(1000);
        pd.inc_skipped(100);
        assert_eq!(pd.current_bytes, 100);
        assert_eq!(pd.skipped_bytes, 100);
        pd.inc_skipped(200);
        assert_eq!(pd.current_bytes, 300);
        assert_eq!(pd.skipped_bytes, 300);
    }

    #[test]
    fn test_estimate_eta_complete() {
        let pd = ProgressData::new(0);
        assert_eq!(pd.estimate_eta(), Some(Duration::from_secs(0)));
    }

    #[test]
    fn test_estimate_eta_no_speed() {
        let pd = ProgressData::new(1000);
        assert_eq!(pd.estimate_eta(), None);
    }

    #[test]
    fn test_estimate_eta_with_speed() {
        let mut pd = ProgressData::new(1024 * 1024 * 100);
        pd.current_bytes = 1024 * 1024 * 50;
        pd.last_speed = 10.0; // 10 MiB/s
        let eta = pd.estimate_eta().unwrap();
        assert_eq!(eta.as_secs(), 5);
    }

    #[test]
    fn test_init_workers() {
        let mut pd = ProgressData::new(1000);
        pd.init_workers(4);
        assert_eq!(pd.workers.len(), 4);
        assert_eq!(pd.parallel_total, 4);
        assert_eq!(pd.active_worker_count(), 0);
    }

    #[test]
    fn test_update_and_finish_worker() {
        let mut pd = ProgressData::new(1000);
        pd.init_workers(2);

        pd.update_worker(0, "file.txt", 500, 100);
        assert_eq!(pd.active_worker_count(), 1);
        assert_eq!(pd.workers[0].file_name, "file.txt");
        assert_eq!(pd.workers[0].progress, 100);

        pd.finish_worker(0);
        assert_eq!(pd.active_worker_count(), 0);
        assert!(pd.workers[0].file_name.is_empty());
    }

    #[test]
    fn test_worker_calculate_speed() {
        let mut w = WorkerState::new();
        w.progress = 1024 * 1024;
        w.last_update = Instant::now() - Duration::from_secs(1);
        let speed = w.calculate_speed();
        assert!(speed > 0.0);
    }
}
