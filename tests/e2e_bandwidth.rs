#![cfg(all(unix, feature = "test-support"))]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bcmr::core::bandwidth;
use bcmr::core::protocol::{CompressionAlgo, CAP_ZSTD};
use bcmr::core::serve_client::{ServeClient, ServeClientPool};

struct ResetBandwidth;

impl Drop for ResetBandwidth {
    fn drop(&mut self) {
        bandwidth::configure(None);
    }
}

fn assert_one_delivery(started: Instant, operation: &str) {
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1850),
        "{operation} bypassed the aggregate logical-payload cap: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(3800),
        "{operation} appears to charge one delivery more than once: {elapsed:?}"
    );
}

// A dedicated integration-test process keeps its invocation-global cap from
// changing other concurrently executing serve integration tests.
#[tokio::test]
async fn compressed_direct_and_striped_transfers_share_the_payload_budget() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let remote = directory.path().join("remote");
    let striped_remote = directory.path().join("striped-remote");
    let striped_local = directory.path().join("striped-local");
    let payload = vec![b'x'; 512 * 1024];
    std::fs::write(&source, &payload).unwrap();

    let mut client = ServeClient::connect_direct_local_with_caps(CAP_ZSTD)
        .await
        .unwrap();
    assert!(client.is_aead_negotiated());
    assert_eq!(client.negotiated_algo(), CompressionAlgo::Zstd);
    let mut pool = ServeClientPool::connect_direct_local(4).await.unwrap();

    bandwidth::configure(Some(256 * 1024));
    let _reset = ResetBandwidth;
    let started = Instant::now();
    client.put(remote.to_str().unwrap(), &source).await.unwrap();
    assert_one_delivery(started, "compressed direct PUT");
    assert_eq!(std::fs::read(&remote).unwrap(), payload);

    let received = Arc::new(Mutex::new(Vec::new()));
    let received_callback = Arc::clone(&received);
    let started = Instant::now();
    client
        .get(remote.to_str().unwrap(), 0, move |chunk| {
            received_callback.lock().unwrap().extend_from_slice(chunk);
        })
        .await
        .unwrap();
    assert_one_delivery(started, "compressed direct GET");
    assert_eq!(*received.lock().unwrap(), payload);

    let started = Instant::now();
    pool.striped_put_file(&source, striped_remote.to_str().unwrap())
        .await
        .unwrap();
    assert_one_delivery(started, "four-connection striped PUT");
    assert_eq!(std::fs::read(&striped_remote).unwrap(), payload);

    let started = Instant::now();
    pool.striped_get_file(
        striped_remote.to_str().unwrap(),
        &striped_local,
        payload.len() as u64,
    )
    .await
    .unwrap();
    assert_one_delivery(started, "four-connection striped GET");
    assert_eq!(std::fs::read(&striped_local).unwrap(), payload);

    client.close().await.unwrap();
    pool.close().await.unwrap();
}
