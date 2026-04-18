//! Single-core AEAD throughput at 4 MiB chunks (the Data-frame size).
//! Run: `cargo run --release --example crypto_probe`
//! Compare against SSH loopback throughput to size the Path B ceiling.

use ring::aead::{self, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, CHACHA20_POLY1305};
use ring::rand::{SecureRandom, SystemRandom};
use std::time::Instant;

const CHUNK_SIZE: usize = 4 * 1024 * 1024; // matches bcmr serve's DATA frame size
const TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB total work per measurement
const N_CHUNKS: usize = (TOTAL_BYTES as usize) / CHUNK_SIZE;

struct Result {
    name: &'static str,
    encrypt_gbps: f64,
    decrypt_gbps: f64,
    roundtrip_gbps: f64,
}

fn bench(algo_name: &'static str, algo: &'static aead::Algorithm) -> Result {
    let rng = SystemRandom::new();
    let mut key_bytes = vec![0u8; algo.key_len()];
    rng.fill(&mut key_bytes).expect("rng");
    let enc_key = LessSafeKey::new(UnboundKey::new(algo, &key_bytes).expect("key"));
    let dec_key = LessSafeKey::new(UnboundKey::new(algo, &key_bytes).expect("key"));

    // Fresh buffer per chunk so we're not benchmarking cache behavior
    // on the same 4 MiB buffer. Fill with pseudo-random content that
    // AES-NI / AVX can't shortcut.
    let mut plain = vec![0u8; CHUNK_SIZE];
    rng.fill(&mut plain).expect("rng fill");

    // The ring AEAD API appends the 16-byte tag to the buffer, so we
    // pre-allocate with room.
    let mut ct_buf = Vec::with_capacity(CHUNK_SIZE + aead::MAX_TAG_LEN);
    let mut nonce_counter = 0u64;

    let t0 = Instant::now();
    for _ in 0..N_CHUNKS {
        ct_buf.clear();
        ct_buf.extend_from_slice(&plain);
        let nonce = make_nonce(&mut nonce_counter);
        enc_key
            .seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut ct_buf)
            .expect("encrypt");
    }
    let enc_elapsed = t0.elapsed().as_secs_f64();
    let enc_gbps = (TOTAL_BYTES as f64) / enc_elapsed / 1e9;

    let ct_sample = ct_buf.clone();

    let mut work_buf = Vec::with_capacity(CHUNK_SIZE + aead::MAX_TAG_LEN);
    // Decrypt N times using the SAME valid ciphertext/nonce pair —
    // otherwise we'd have to re-encrypt in the loop, which measures
    // encrypt not decrypt.
    let nonce_reuse_counter = nonce_counter - 1;
    let t0 = Instant::now();
    for _ in 0..N_CHUNKS {
        work_buf.clear();
        work_buf.extend_from_slice(&ct_sample);
        let mut cc = nonce_reuse_counter;
        let nonce = make_nonce(&mut cc);
        dec_key
            .open_in_place(nonce, aead::Aad::empty(), &mut work_buf)
            .expect("decrypt");
    }
    let dec_elapsed = t0.elapsed().as_secs_f64();
    let dec_gbps = (TOTAL_BYTES as f64) / dec_elapsed / 1e9;

    nonce_counter = 0;
    let t0 = Instant::now();
    for _ in 0..N_CHUNKS {
        ct_buf.clear();
        ct_buf.extend_from_slice(&plain);
        let nonce = make_nonce(&mut nonce_counter);
        enc_key
            .seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut ct_buf)
            .expect("encrypt");
        let mut cc = nonce_counter - 1;
        let nonce = make_nonce(&mut cc);
        dec_key
            .open_in_place(nonce, aead::Aad::empty(), &mut ct_buf)
            .expect("decrypt");
    }
    let rt_elapsed = t0.elapsed().as_secs_f64();
    let rt_gbps = (TOTAL_BYTES as f64) / rt_elapsed / 1e9;

    Result {
        name: algo_name,
        encrypt_gbps: enc_gbps,
        decrypt_gbps: dec_gbps,
        roundtrip_gbps: rt_gbps,
    }
}

fn make_nonce(counter: &mut u64) -> Nonce {
    let c = *counter;
    *counter += 1;
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(&c.to_le_bytes());
    Nonce::assume_unique_for_key(bytes)
}

fn main() {
    println!("bcmr crypto probe");
    println!(
        "chunk size = {} MiB, total = {} GiB, {} chunks per measurement",
        CHUNK_SIZE / (1024 * 1024),
        TOTAL_BYTES / (1024 * 1024 * 1024),
        N_CHUNKS
    );
    println!(
        "arch: {}, aes-ni likely: {}",
        std::env::consts::ARCH,
        cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64")
    );
    println!();
    println!(
        "{:<28} {:>12} {:>12} {:>12}",
        "cipher", "encrypt GB/s", "decrypt GB/s", "rt GB/s"
    );
    println!("{}", "-".repeat(68));

    for (name, algo) in &[
        ("ring::AES-256-GCM", &AES_256_GCM),
        ("ring::ChaCha20-Poly1305", &CHACHA20_POLY1305),
    ] {
        let r = bench(name, algo);
        println!(
            "{:<28} {:>12.3} {:>12.3} {:>12.3}",
            r.name, r.encrypt_gbps, r.decrypt_gbps, r.roundtrip_gbps
        );
    }

    println!();
    println!("Interpretation:");
    println!("  Path B single-stream ceiling ≈ roundtrip column (server encrypts,");
    println!("  client decrypts; they're disjoint cores in practice so server-");
    println!("  encrypt-only is the real ceiling for server-bound workloads).");
    println!();
    println!("  Compare against SSH loopback throughput:");
    println!("    dd if=/dev/zero bs=1M count=4096 | ssh localhost 'cat > /dev/null'");
    println!();
    println!("  Decision rule: Path B ships only if encrypt-GB/s ≥ 1.5× SSH loopback.");
    println!("  Otherwise we stay on Path A (parallel SSH) and close the Path B branch.");
}
