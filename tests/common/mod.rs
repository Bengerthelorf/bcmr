// Each integration test is its own binary, so `dead_code` warnings would
// fire for items used only by other test files.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Stdio;

pub fn bcmr_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };
    exe.parent().unwrap().parent().unwrap().join(bin_name)
}

pub struct ServeChild {
    pub child: tokio::process::Child,
    pub stdin: tokio::process::ChildStdin,
    pub stdout: tokio::process::ChildStdout,
}

pub fn spawn_serve(root: &str) -> ServeChild {
    spawn_serve_with_env(root, &[])
}

pub fn spawn_serve_with_env(root: &str, env: &[(&str, &str)]) -> ServeChild {
    let mut cmd = tokio::process::Command::new(bcmr_bin());
    cmd.args(["serve", "--root", root])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    ServeChild {
        child,
        stdin,
        stdout,
    }
}

use std::fs;
use std::io::Write;
use std::path::Path;

/// Deterministic pseudo-random bytes (LCG) so hash collisions are detectable.
pub fn create_file(path: &Path, size: usize) {
    let mut file = fs::File::create(path).unwrap();
    let mut state: u64 = 0xdeadbeef_cafebabe;
    let mut buf = Vec::with_capacity(size.min(65536));
    let mut remaining = size;
    while remaining > 0 {
        let chunk = remaining.min(65536);
        buf.clear();
        for _ in 0..chunk {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            buf.push((state >> 33) as u8);
        }
        file.write_all(&buf).unwrap();
        remaining -= chunk;
    }
    file.flush().unwrap();
}

pub fn bytes_to_hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Serialises tests that mutate `BCMR_CAS_CAP_MB` so unrelated concurrent
/// PUTs don't inherit a capped-vs-uncapped env at spawn time.
pub fn cas_test_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
