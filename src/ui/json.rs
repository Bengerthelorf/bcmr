use crate::ui::progress::ProgressRenderer;
use crate::ui::state::ProgressData;

use serde::Serialize;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

/// NDJSON progress renderer.
///
/// Writes one JSON object per line:
/// - `{"type":"progress",...}` on every tick (throttled)
/// - `{"type":"result",...}`  on finish
///
/// Output goes to a log file (detach mode) or stdout (stream mode).
pub struct JsonProgress {
    data: ProgressData,
    last_emit: Option<Instant>,
    finished: bool,
    writer: JsonWriter,
}

enum JsonWriter {
    Stdout,
    File(BufWriter<fs::File>),
}

impl JsonWriter {
    fn write_line(&mut self, value: &impl Serialize) {
        match self {
            JsonWriter::Stdout => {
                let mut out = io::stdout().lock();
                let _ = serde_json::to_writer(&mut out, value);
                let _ = out.write_all(b"\n");
                let _ = out.flush();
            }
            JsonWriter::File(f) => {
                let _ = serde_json::to_writer(&mut *f, value);
                let _ = f.write_all(b"\n");
                let _ = f.flush();
            }
        }
    }

    fn write_line_strict(&mut self, value: &impl Serialize) -> io::Result<()> {
        match self {
            JsonWriter::Stdout => {
                let mut out = io::stdout().lock();
                serde_json::to_writer(&mut out, value).map_err(io::Error::other)?;
                out.write_all(b"\n")?;
                out.flush()
            }
            JsonWriter::File(f) => {
                serde_json::to_writer(&mut *f, value).map_err(io::Error::other)?;
                f.write_all(b"\n")?;
                f.flush()
            }
        }
    }
}

// ── NDJSON line types ──────────────────────────────────────────────────

#[derive(Serialize)]
struct ProgressLine<'a> {
    r#type: &'static str,
    operation: &'a str,
    bytes_done: u64,
    bytes_total: u64,
    percent: f64,
    speed_bps: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    eta_secs: Option<u64>,
    file: &'a str,
    file_size: u64,
    file_progress: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    items_done: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    items_total: Option<usize>,
    scanning: bool,
}

#[derive(Serialize)]
struct ResultLine<'a> {
    r#type: &'static str,
    status: &'static str,
    operation: &'a str,
    bytes_total: u64,
    duration_secs: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    avg_speed_bps: Option<u64>,
}

// ── Implementation ─────────────────────────────────────────────────────

const EMIT_INTERVAL_MS: u128 = 200;

impl JsonProgress {
    /// Stream mode: write NDJSON to stdout.
    pub fn new(total_bytes: u64) -> Self {
        Self {
            data: ProgressData::new(total_bytes),
            last_emit: None,
            finished: false,
            writer: JsonWriter::Stdout,
        }
    }

    /// Detach mode: write NDJSON to a log file.
    pub fn with_log_file(total_bytes: u64, path: &PathBuf) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            data: ProgressData::new(total_bytes),
            last_emit: None,
            finished: false,
            writer: JsonWriter::File(BufWriter::new(file)),
        })
    }

    fn emit_progress(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_emit {
            if now.duration_since(last).as_millis() < EMIT_INTERVAL_MS {
                return;
            }
        }
        self.last_emit = Some(now);

        let speed_mib = self.data.calculate_speed();
        let speed_bps = (speed_mib * 1024.0 * 1024.0) as u64;
        let percent = if self.data.total_bytes > 0 {
            self.data.current_bytes as f64 / self.data.total_bytes as f64 * 100.0
        } else {
            0.0
        };
        let eta_secs = self.data.estimate_eta().map(|d| d.as_secs());

        let line = ProgressLine {
            r#type: "progress",
            operation: &self.data.operation_type,
            bytes_done: self.data.current_bytes,
            bytes_total: self.data.total_bytes,
            percent,
            speed_bps,
            eta_secs,
            file: &self.data.current_file,
            file_size: self.data.current_file_size,
            file_progress: self.data.current_file_progress,
            items_done: Some(self.data.items_processed),
            items_total: self.data.items_total,
            scanning: self.data.scanning,
        };

        self.writer.write_line(&line);
    }
}

impl ProgressRenderer for JsonProgress {
    fn set_total_items(&mut self, total: usize) {
        self.data.items_total = Some(total);
    }

    fn inc_items_processed(&mut self) {
        self.data.items_processed += 1;
    }

    fn set_current_file(&mut self, file_name: &str, file_size: u64) {
        self.data.current_file = file_name.to_string();
        self.data.current_file_size = file_size;
        self.data.current_file_progress = 0;
    }

    fn inc_current(&mut self, delta: u64) {
        self.data.current_bytes += delta;
        self.data.current_file_progress += delta;
        self.emit_progress();
    }

    fn inc_skipped(&mut self, delta: u64) {
        self.data.inc_skipped(delta);
    }

    fn set_operation_type(&mut self, operation: &str) {
        self.data.operation_type = operation.to_string();
    }

    fn set_total_bytes(&mut self, total: u64) {
        self.data.total_bytes = total;
    }

    fn set_scanning(&mut self, scanning: bool) {
        self.data.scanning = scanning;
    }

    fn set_files_found(&mut self, count: u64) {
        self.data.files_found = count;
    }

    fn set_parallel_mode(&mut self, worker_count: usize) {
        self.data.init_workers(worker_count);
    }

    fn update_worker(&mut self, slot: usize, file_name: &str, file_size: u64, progress: u64) {
        self.data
            .update_worker(slot, file_name, file_size, progress);
    }

    fn finish_worker(&mut self, slot: usize) {
        self.data.finish_worker(slot);
    }

    fn tick(&mut self) {
        if !self.finished {
            self.emit_progress();
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;

        let elapsed = self.data.elapsed();
        let avg_bps = self.data.average_bytes_per_sec().map(|b| b as u64);

        let line = ResultLine {
            r#type: "result",
            status: "success",
            operation: &self.data.operation_type,
            bytes_total: self.data.current_bytes,
            duration_secs: elapsed.as_secs_f64(),
            avg_speed_bps: avg_bps,
        };

        self.writer.write_line_strict(&line)
    }
}
