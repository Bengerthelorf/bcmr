//! Breakdown of where time goes in a dedup PUT. Runs in-process
//! against bcmr serve over a pipe (no SSH, no network) so the numbers
//! isolate protocol/CAS/hash cost from network latency.
//!
//! Reality-check vs Experiment 11: if the AI's claim is right that 13 s
//! for a 64 MiB warm-cache dedup PUT should really be ~1 s, we should
//! see the bulk of time go to network (absent here) rather than to
//! local work.

use bcmr::core::serve_client::ServeClient;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cas_tmp = tempfile::tempdir()?;
    std::env::set_var("BCMR_CAS_DIR", cas_tmp.path());

    let dir = tempfile::tempdir()?;
    let src = dir.path().join("payload.bin");
    let dst1 = dir.path().join("dst1.bin");
    let dst2 = dir.path().join("dst2.bin");

    // 64 MiB pseudo-random to match Experiment 11.
    let size: usize = 64 * 1024 * 1024;
    let mut data = vec![0u8; size];
    let mut x: u64 = 0xdeadbeefcafebabe;
    for b in data.iter_mut() {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (x >> 33) as u8;
    }
    std::fs::write(&src, &data)?;

    println!("=== Run 1: cold cache (no CAS hits) ===");
    let t0 = Instant::now();
    let mut client = ServeClient::connect_local().await?;
    let t_conn = t0.elapsed();
    println!(
        "  connect_local + handshake:  {:>7.2} ms",
        t_conn.as_secs_f64() * 1000.0
    );

    let t1 = Instant::now();
    let _h = client.put(dst1.to_str().unwrap(), &src).await?;
    let t_put1 = t1.elapsed();
    println!(
        "  put (includes hashing):     {:>7.2} ms",
        t_put1.as_secs_f64() * 1000.0
    );

    client.close().await?;
    println!(
        "  TOTAL run 1:                {:>7.2} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );

    println!();
    println!("=== Run 2: warm cache (every block a CAS hit) ===");
    let t0 = Instant::now();
    let mut client = ServeClient::connect_local().await?;
    let t_conn2 = t0.elapsed();
    println!(
        "  connect_local + handshake:  {:>7.2} ms",
        t_conn2.as_secs_f64() * 1000.0
    );

    let t1 = Instant::now();
    let _h = client.put(dst2.to_str().unwrap(), &src).await?;
    let t_put2 = t1.elapsed();
    println!(
        "  put (includes re-hashing):  {:>7.2} ms",
        t_put2.as_secs_f64() * 1000.0
    );

    client.close().await?;
    println!(
        "  TOTAL run 2:                {:>7.2} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );

    println!();
    println!("=== Breakdown analysis ===");
    println!(
        "  Run 2 put saved vs Run 1:   {:>7.2} ms",
        (t_put1.as_secs_f64() - t_put2.as_secs_f64()) * 1000.0
    );
    println!("  Expected saving = data write + wire");

    println!();
    println!("=== For reference: cost components ===");
    // Time to read + hash 64 MiB client-side (done twice per put: for
    // HaveBlocks and then again for streaming). We don't stream in
    // run 2 but we still re-hash in run 2's dedup path — let's see.
    let t0 = Instant::now();
    use blake3::Hasher;
    let mut h = Hasher::new();
    h.update(&data);
    let _hash = h.finalize();
    println!(
        "  blake3 whole 64 MiB (1 pass): {:>5.2} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );

    let t0 = Instant::now();
    let _r = std::fs::read(&src)?;
    println!(
        "  read 64 MiB from src disk:    {:>5.2} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );

    Ok(())
}
