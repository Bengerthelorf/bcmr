//! Standalone compression probe. Compile with:
//!   rustc --edition 2021 -O bench/compress_probe.rs -L target/release/deps \
//!         --extern lz4_flex --extern zstd -o /tmp/compress_probe
//! Or use the helper script bench/run_compress_probe.sh.
//!
//! Measures: compression ratio, encode throughput, decode throughput for
//! LZ4 and Zstd-1/3/9 on three realistic block contents:
//!   - incompressible: /dev/urandom bytes
//!   - text-like: repeating dictionary with noise (mimics logs, source)
//!   - mixed: half source-like, half random (mimics typical file mix)

use std::time::Instant;

fn gen_random(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    // Simple LCG for reproducibility; quality irrelevant, just want high entropy.
    let mut x: u64 = 0xdeadbeefcafebabe;
    for b in buf.iter_mut() {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (x >> 33) as u8;
    }
    buf
}

fn gen_text_like(n: usize) -> Vec<u8> {
    // Pool of words/tokens typical in source code and logs.
    let tokens: &[&[u8]] = &[
        b"function ",
        b"const ",
        b"return ",
        b"if (",
        b") {",
        b"} else {",
        b"import ",
        b"export ",
        b"await ",
        b"async ",
        b"=> ",
        b";\n",
        b"    ",
        b"\n",
        b"// ",
        b"/* ",
        b" */\n",
        b"Result<",
        b"Option<",
        b"Ok(",
        b"Err(",
        b"String",
        b"Vec<u8>",
        b"self.",
        b"None,",
        b"Some(",
    ];
    let mut buf = Vec::with_capacity(n);
    let mut x: u64 = 42;
    while buf.len() < n {
        x = x.wrapping_mul(2862933555777941757).wrapping_add(3037000493);
        let tok = tokens[(x as usize) % tokens.len()];
        buf.extend_from_slice(tok);
    }
    buf.truncate(n);
    buf
}

fn gen_mixed(n: usize) -> Vec<u8> {
    let mut out = gen_text_like(n / 2);
    out.extend(gen_random(n - out.len()));
    out
}

struct Result {
    name: &'static str,
    algo: &'static str,
    ratio: f64,
    enc_mbs: f64,
    dec_mbs: f64,
}

fn bench_once(name: &'static str, data: &[u8]) -> Vec<Result> {
    let n = data.len();
    let iters_mb = (256 * 1024 * 1024 / n).max(1);
    let mut out = Vec::new();

    // LZ4 (frame)
    {
        let encoded: Vec<u8> = lz4_flex::compress_prepend_size(data);
        let ratio = encoded.len() as f64 / n as f64;

        let t0 = Instant::now();
        for _ in 0..iters_mb {
            let _ = lz4_flex::compress_prepend_size(data);
        }
        let enc_mbs = (n * iters_mb) as f64 / t0.elapsed().as_secs_f64() / (1024.0 * 1024.0);

        let t0 = Instant::now();
        for _ in 0..iters_mb {
            let _ = lz4_flex::decompress_size_prepended(&encoded).unwrap();
        }
        let dec_mbs = (n * iters_mb) as f64 / t0.elapsed().as_secs_f64() / (1024.0 * 1024.0);

        out.push(Result {
            name,
            algo: "lz4",
            ratio,
            enc_mbs,
            dec_mbs,
        });
    }

    for level in &[1i32, 3, 9] {
        let encoded = zstd::bulk::compress(data, *level).unwrap();
        let ratio = encoded.len() as f64 / n as f64;

        let t0 = Instant::now();
        for _ in 0..iters_mb {
            let _ = zstd::bulk::compress(data, *level).unwrap();
        }
        let enc_mbs = (n * iters_mb) as f64 / t0.elapsed().as_secs_f64() / (1024.0 * 1024.0);

        let t0 = Instant::now();
        for _ in 0..iters_mb {
            let _ = zstd::bulk::decompress(&encoded, n).unwrap();
        }
        let dec_mbs = (n * iters_mb) as f64 / t0.elapsed().as_secs_f64() / (1024.0 * 1024.0);

        let algo: &'static str = match *level {
            1 => "zstd-1",
            3 => "zstd-3",
            9 => "zstd-9",
            _ => "zstd-?",
        };
        out.push(Result {
            name,
            algo,
            ratio,
            enc_mbs,
            dec_mbs,
        });
    }

    out
}

fn main() {
    // 4 MiB per block matches bcmr's COPY_BLOCK_SIZE.
    const BLOCK: usize = 4 * 1024 * 1024;
    let mut rows = Vec::new();
    rows.extend(bench_once("random", &gen_random(BLOCK)));
    rows.extend(bench_once("text", &gen_text_like(BLOCK)));
    rows.extend(bench_once("mixed", &gen_mixed(BLOCK)));

    println!("| workload | algo   | ratio | enc MB/s | dec MB/s |");
    println!("|----------|--------|------:|---------:|---------:|");
    for r in rows {
        println!(
            "| {:<8} | {:<6} | {:>5.3} | {:>8.1} | {:>8.1} |",
            r.name, r.algo, r.ratio, r.enc_mbs, r.dec_mbs
        );
    }
}
