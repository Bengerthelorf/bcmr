//! One invocation-wide limit on delivered, uncompressed application payload.
//!
//! Acquire before delivering each payload chunk, above compression and framing.
//! Metadata, hashes and validated resume prefixes are not transferred payload.
//! A shared gate serializes budget waits across asynchronous network workers and
//! blocking local-copy workers. A cancelled wait releases its gate without
//! reserving future bandwidth for bytes that will never be delivered.

use std::num::NonZeroU64;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const MAX_CHUNK_SIZE: usize = 64 * 1024;
const MAX_SAVED_BUDGET: usize = 4 * 1024 * 1024;
static LIMITER: RwLock<Option<Arc<Limiter>>> = RwLock::new(None);

struct Limiter {
    bytes_per_second: NonZeroU64,
    chunk_size: usize,
    gate: tokio::sync::Mutex<Option<Instant>>,
}

impl Limiter {
    fn new(bytes_per_second: u64) -> Self {
        let bytes_per_second =
            NonZeroU64::new(bytes_per_second).expect("bandwidth rate must be positive");
        // About 100 ms of payload at low rates; never buffer an entire file or
        // a large protocol/CAS block solely to enforce the limit.
        let chunk_size = (bytes_per_second.get() / 10)
            .max(1)
            .min(MAX_CHUNK_SIZE as u64) as usize;
        Self {
            bytes_per_second,
            chunk_size,
            gate: tokio::sync::Mutex::new(None),
        }
    }

    fn deadline(&self, previous: Option<Instant>, bytes: usize) -> Instant {
        let now = Instant::now();
        // Retain bounded credit for timer rounding / slow writes so a large
        // configured rate does not become one 64 KiB chunk per timer tick.
        // There is no credit on the first acquisition, and idle time cannot
        // accumulate an unbounded future payload burst.
        let saved_budget = (self.bytes_per_second.get() / 500)
            .max(self.chunk_size as u64)
            .min(MAX_SAVED_BUDGET as u64) as usize;
        let earliest = now
            .checked_sub(payload_duration(saved_budget, self.bytes_per_second))
            .unwrap_or(now);
        let start = previous.map_or(now, |previous| previous.max(earliest));
        start + payload_duration(bytes, self.bytes_per_second)
    }

    async fn acquire(&self, mut bytes: usize) {
        while bytes > 0 {
            let part = bytes.min(self.chunk_size);
            let mut gate = self.gate.lock().await;
            let deadline = self.deadline(*gate, part);
            // Both clock types are monotonic. Update only after waiting so
            // cancellation does not reserve any untransferred payload.
            if deadline > Instant::now() {
                tokio::time::sleep_until(deadline.into()).await;
            }
            *gate = Some(deadline);
            bytes -= part;
        }
    }

    #[allow(dead_code, reason = "used by the binary's blocking local-copy path")]
    fn acquire_blocking(&self, mut bytes: usize) {
        while bytes > 0 {
            let part = bytes.min(self.chunk_size);
            let mut gate = self.gate.blocking_lock();
            let deadline = self.deadline(*gate, part);
            if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            *gate = Some(deadline);
            bytes -= part;
        }
    }
}

fn active() -> Option<Arc<Limiter>> {
    LIMITER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Initialize once before starting the invocation's copy/move workers.
///
/// `None` disables the cap. A supplied rate must be positive; CLI callers use
/// [`parse_rate`] to reject invalid input before any transfer starts.
pub fn configure(bytes_per_second: Option<u64>) {
    let limiter = bytes_per_second.map(|rate| Arc::new(Limiter::new(rate)));
    *LIMITER
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = limiter;
}

pub fn is_enabled() -> bool {
    active().is_some()
}

/// Maximum delivery chunk while capped; no restriction when uncapped.
///
/// Transfer loops must bound their actual reads/writes with this value. Calling
/// `acquire` for a whole file and then publishing it in one write is not pacing.
pub fn chunk_size() -> usize {
    active().map_or(usize::MAX, |limiter| limiter.chunk_size)
}

pub async fn acquire(bytes: usize) {
    if let Some(limiter) = active() {
        limiter.acquire(bytes).await;
    }
}

/// For userspace local-copy loops already running in `spawn_blocking`.
#[allow(
    dead_code,
    reason = "the library does not compile the local-copy commands"
)]
pub(crate) fn acquire_blocking(bytes: usize) {
    if let Some(limiter) = active() {
        limiter.acquire_blocking(bytes);
    }
}

fn payload_duration(bytes: usize, bytes_per_second: NonZeroU64) -> Duration {
    let rate = bytes_per_second.get() as u128;
    let bytes = bytes as u128;
    let seconds = bytes / rate;
    // u128 keeps even usize::MAX * 1e9 safe. Round up so fractional
    // nanoseconds never grant more payload than the requested rate.
    let nanoseconds = ((bytes % rate) * 1_000_000_000).div_ceil(rate);
    Duration::new(seconds as u64, nanoseconds as u32)
}

/// Parse positive integer bytes/s, with optional B, KiB/MiB/GiB, kB/MB/GB
/// suffixes and an optional `/s`. Unit spelling is deliberately case-sensitive.
pub fn parse_rate(value: &str) -> Result<u64, String> {
    let invalid = || {
        "bandwidth rate must be a positive integer in bytes/s, KiB, MiB, GiB, kB, MB, or GB (optionally /s)".to_owned()
    };
    let value = value.strip_suffix("/s").unwrap_or(value);
    let digits = value
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(value.len());
    if digits == 0 {
        return Err(invalid());
    }
    let amount = value[..digits].parse::<u64>().map_err(|_| invalid())?;
    let multiplier = match &value[digits..] {
        "" | "B" => 1,
        "KiB" => 1024,
        "MiB" => 1024 * 1024,
        "GiB" => 1024 * 1024 * 1024,
        "kB" => 1000,
        "MB" => 1000 * 1000,
        "GB" => 1000 * 1000 * 1000,
        _ => return Err(invalid()),
    };
    amount
        .checked_mul(multiplier)
        .filter(|rate| *rate > 0)
        .ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rates_have_exact_case_sensitive_decimal_and_binary_units() {
        for (input, expected) in [
            ("1", 1),
            ("12/s", 12),
            ("7B/s", 7),
            ("2KiB", 2048),
            ("3MiB/s", 3_145_728),
            ("4GiB", 4_294_967_296),
            ("5kB/s", 5000),
            ("6MB", 6_000_000),
            ("7GB/s", 7_000_000_000),
            ("18446744073709551615", u64::MAX),
        ] {
            assert_eq!(parse_rate(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn invalid_zero_fractional_and_overflowing_rates_are_rejected() {
        for input in [
            "",
            "0",
            "0KiB",
            "-1",
            "+1",
            "1.5MiB",
            "1kb",
            "1Mbps",
            "1KiB/s/s",
            " 1MiB",
            "1MiB ",
            "18446744073709551616",
            "18446744073709551615GiB",
        ] {
            assert!(parse_rate(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn payload_durations_round_up_without_overflow() {
        assert_eq!(
            payload_duration(1, NonZeroU64::new(3).unwrap()),
            Duration::new(0, 333_333_334)
        );
        assert_eq!(
            payload_duration(1, NonZeroU64::new(u64::MAX).unwrap()),
            Duration::from_nanos(1)
        );
        assert_eq!(
            payload_duration(usize::MAX, NonZeroU64::new(1).unwrap()),
            Duration::from_secs(usize::MAX as u64)
        );
        assert_eq!(
            payload_duration(0, NonZeroU64::new(1).unwrap()),
            Duration::ZERO
        );
    }

    #[test]
    fn idle_time_cannot_accumulate_an_unbounded_payload_credit() {
        let limiter = Limiter::new(1000);
        let started = Instant::now();
        let stale = started.checked_sub(Duration::from_secs(10)).unwrap();
        let deadline = limiter.deadline(Some(stale), 100);
        // At 1000 B/s the saved budget is one 100-byte chunk. Paying for
        // that chunk exhausts the credit even after ten idle seconds.
        assert!(deadline >= started);
    }

    #[tokio::test]
    async fn first_payload_waits_for_its_budget() {
        let limiter = Limiter::new(1000);
        let started = Instant::now();
        limiter.acquire(50).await;
        assert!(started.elapsed() >= Duration::from_millis(48));
    }

    #[tokio::test]
    async fn concurrent_workers_and_blocking_copies_share_one_budget() {
        let limiter = Arc::new(Limiter::new(1000));
        let started = Instant::now();
        let mut workers = Vec::new();
        for _ in 0..3 {
            let limiter = Arc::clone(&limiter);
            workers.push(tokio::spawn(async move { limiter.acquire(50).await }));
        }
        let blocking_limiter = Arc::clone(&limiter);
        let blocking = tokio::task::spawn_blocking(move || blocking_limiter.acquire_blocking(50));
        for worker in workers {
            worker.await.unwrap();
        }
        blocking.await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(190));
    }

    #[tokio::test]
    async fn a_cancelled_wait_does_not_reserve_untransferred_payload() {
        let limiter = Arc::new(Limiter::new(1000));
        let waiting_limiter = Arc::clone(&limiter);
        let worker = tokio::spawn(async move { waiting_limiter.acquire(1000).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        worker.abort();
        let _ = worker.await;
        let started = Instant::now();
        limiter.acquire(1).await;
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn empty_payload_needs_no_timer_or_gate() {
        use futures::FutureExt;
        let limiter = Limiter::new(1);
        assert_eq!(limiter.acquire(0).now_or_never(), Some(()));
    }
}
