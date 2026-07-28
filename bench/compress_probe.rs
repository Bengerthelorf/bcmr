use std::time::Instant;

fn gen_random(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
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

/// Maximum effective link speed where a sequential
/// `encode + transmit` path still beats sending the raw bytes.
fn raw_break_even_mbs(enc_mbs: f64, ratio: f64) -> Option<f64> {
    (ratio < 1.0).then_some(enc_mbs * (1.0 - ratio))
}

/// Effective link speed where two sequential codecs have equal
/// `encode + transmit` cost per raw byte.
fn codec_cross_over_mbs(
    left_enc_mbs: f64,
    left_ratio: f64,
    right_enc_mbs: f64,
    right_ratio: f64,
) -> Option<f64> {
    let denominator = 1.0 / right_enc_mbs - 1.0 / left_enc_mbs;
    let numerator = left_ratio - right_ratio;
    (denominator > 0.0 && numerator > 0.0).then_some(numerator / denominator)
}

fn bench_once(name: &'static str, data: &[u8]) -> Vec<Result> {
    let n = data.len();
    let iters_mb = (256 * 1024 * 1024 / n).max(1);
    let mut out = Vec::new();

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
    const BLOCK: usize = 4 * 1024 * 1024;
    let mut rows = Vec::new();
    rows.extend(bench_once("random", &gen_random(BLOCK)));
    rows.extend(bench_once("text", &gen_text_like(BLOCK)));
    rows.extend(bench_once("mixed", &gen_mixed(BLOCK)));

    println!("| workload | algo   | ratio | enc MB/s | dec MB/s | raw break-even MB/s |");
    println!("|----------|--------|------:|---------:|---------:|--------------------:|");
    for r in &rows {
        let break_even = raw_break_even_mbs(r.enc_mbs, r.ratio)
            .map(|value| format!("{value:.1}"))
            .unwrap_or_else(|| "never".to_string());
        println!(
            "| {:<8} | {:<6} | {:>5.3} | {:>8.1} | {:>8.1} | {:>19} |",
            r.name, r.algo, r.ratio, r.enc_mbs, r.dec_mbs, break_even
        );
    }

    println!();
    println!("Sequential codec crossovers (lower link speed favors Zstd-3):");
    for workload in ["text", "mixed"] {
        let lz4 = rows
            .iter()
            .find(|row| row.name == workload && row.algo == "lz4")
            .unwrap();
        let zstd = rows
            .iter()
            .find(|row| row.name == workload && row.algo == "zstd-3")
            .unwrap();
        let crossover =
            codec_cross_over_mbs(lz4.enc_mbs, lz4.ratio, zstd.enc_mbs, zstd.ratio).unwrap();
        println!("- {workload}: Zstd-3 -> LZ4 at {crossover:.1} MB/s");
    }
}

#[cfg(test)]
mod tests {
    use super::{codec_cross_over_mbs, raw_break_even_mbs};

    #[test]
    fn raw_break_even_matches_the_sequential_cost_equation() {
        assert_eq!(raw_break_even_mbs(500.0, 0.2), Some(400.0));
        assert_eq!(raw_break_even_mbs(500.0, 1.0), None);
        assert_eq!(raw_break_even_mbs(500.0, 1.1), None);
    }

    #[test]
    fn codec_cross_over_matches_equal_total_cost() {
        let crossover = codec_cross_over_mbs(700.0, 0.4, 500.0, 0.2).unwrap();
        let left_cost = 1.0 / 700.0 + 0.4 / crossover;
        let right_cost = 1.0 / 500.0 + 0.2 / crossover;
        assert!((left_cost - right_cost).abs() < 1e-12);
    }
}
