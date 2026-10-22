#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;

use bcmr::core::protocol::{read_message, write_message, Message, CAP_DEDUP, PROTOCOL_VERSION};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

async fn receive(stdout: &mut tokio::process::ChildStdout) -> Message {
    tokio::time::timeout(Duration::from_secs(10), read_message(stdout))
        .await
        .expect("serve reply timed out")
        .unwrap()
        .expect("serve closed before replying")
}

// Each call starts a real server process with the same cache and an explicit
// root. A peer that supplies only a digest must not recover another root's data.
async fn upload(
    root: &Path,
    cache: &Path,
    destination: &str,
    payload: &[u8],
    supply_missing: bool,
) -> (bool, Message) {
    let binary = std::env::var_os("BCMR_CAS_SCOPE_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(common::bcmr_bin);
    let mut child = tokio::process::Command::new(binary)
        .arg("serve")
        .arg("--root")
        .arg(root)
        .env("BCMR_CAS_DIR", cache)
        .env("BCMR_CAS_CAP_MB", "16")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    write_message(
        &mut stdin,
        &Message::Hello {
            version: PROTOCOL_VERSION,
            caps: CAP_DEDUP,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        receive(&mut stdout).await,
        Message::Welcome { .. }
    ));
    write_message(
        &mut stdin,
        &Message::Put {
            path: destination.into(),
            size: payload.len() as u64,
            offset: 0,
            overwrite: false,
        },
    )
    .await
    .unwrap();
    write_message(
        &mut stdin,
        &Message::HaveBlocks {
            block_size: 4 * 1024 * 1024,
            hashes: vec![*blake3::hash(payload).as_bytes()],
        },
    )
    .await
    .unwrap();
    let missing = match receive(&mut stdout).await {
        Message::MissingBlocks { bits } => bits[0] & 1 != 0,
        other => panic!("expected MissingBlocks, got {other:?}"),
    };
    if missing && supply_missing {
        write_message(
            &mut stdin,
            &Message::Data {
                payload: payload.to_vec(),
            },
        )
        .await
        .unwrap();
    }
    write_message(&mut stdin, &Message::Done).await.unwrap();
    let reply = receive(&mut stdout).await;
    drop(stdin);
    tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("serve did not exit")
        .unwrap();
    (missing, reply)
}

#[tokio::test]
async fn same_root_reuses_blocks_but_another_root_cannot_recover_them() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let payload = b"harmless data belonging to root A";
    let (missing, reply) = upload(root_a.path(), cache.path(), "seed", payload, true).await;
    assert!(missing);
    assert!(matches!(reply, Message::Ok { .. }));
    assert_eq!(std::fs::read(root_a.path().join("seed")).unwrap(), payload);

    let (missing, reply) = upload(root_a.path(), cache.path(), "repeat", payload, false).await;
    assert!(
        !missing,
        "same-root uploads must retain useful deduplication"
    );
    assert!(matches!(reply, Message::Ok { .. }));
    assert_eq!(
        std::fs::read(root_a.path().join("repeat")).unwrap(),
        payload
    );

    let (missing, reply) = upload(root_b.path(), cache.path(), "forbidden", payload, false).await;
    assert!(
        missing,
        "a root must not advertise another root's cached bytes"
    );
    assert!(matches!(reply, Message::Error { .. }));
    assert!(!root_b.path().join("forbidden").exists());
}

#[tokio::test]
async fn restricted_root_cannot_reuse_an_unrestricted_servers_blocks() {
    let source = tempfile::tempdir().unwrap();
    let restricted = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let payload = b"harmless unrestricted-server fixture";
    let seed = source.path().join("seed");
    let (missing, reply) = upload(
        Path::new("/"),
        cache.path(),
        seed.to_str().unwrap(),
        payload,
        true,
    )
    .await;
    assert!(missing);
    assert!(matches!(reply, Message::Ok { .. }));

    let (missing, reply) =
        upload(restricted.path(), cache.path(), "forbidden", payload, false).await;
    assert!(
        missing,
        "the unrestricted cache must not bypass a restricted root"
    );
    assert!(matches!(reply, Message::Error { .. }));
    assert!(!restricted.path().join("forbidden").exists());
}

#[tokio::test]
async fn replacing_a_root_directory_does_not_inherit_its_old_cache() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let payload = b"harmless old-root fixture";
    let (_, reply) = upload(&root, cache.path(), "seed", payload, true).await;
    assert!(matches!(reply, Message::Ok { .. }));
    std::fs::rename(&root, work.path().join("old-root")).unwrap();
    std::fs::create_dir(&root).unwrap();

    let (missing, reply) = upload(&root, cache.path(), "forbidden", payload, false).await;
    assert!(
        missing,
        "a new directory at the same spelling needs a new cache scope"
    );
    assert!(matches!(reply, Message::Error { .. }));
    assert!(!root.join("forbidden").exists());
}

#[tokio::test]
async fn unavailable_cache_keeps_uploads_functional_without_dedup_hits() {
    let root = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let unavailable = work.path().join("not-a-directory");
    std::fs::write(&unavailable, b"harmless unavailable-cache fixture").unwrap();
    let payload = b"transfer must remain available without a cache";
    let (missing, reply) = upload(root.path(), &unavailable, "uploaded", payload, true).await;
    assert!(missing);
    assert!(matches!(reply, Message::Ok { .. }));
    assert_eq!(
        std::fs::read(root.path().join("uploaded")).unwrap(),
        payload
    );
    let (missing, reply) = upload(root.path(), &unavailable, "repeat", payload, false).await;
    assert!(missing, "disabled caching must never fabricate a hit");
    assert!(matches!(reply, Message::Error { .. }));
    assert!(!root.path().join("repeat").exists());
}
