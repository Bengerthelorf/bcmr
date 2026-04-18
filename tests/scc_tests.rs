use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use bcmr::core::checksum;
use bcmr::core::io as durable_io;
use bcmr::core::session::{Session, COPY_BLOCK_SIZE};

fn create_test_file(path: &Path, size: usize) {
    let mut f = fs::File::create(path).unwrap();
    let block: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|i: usize| (i.wrapping_mul(7).wrapping_add(13)) as u8)
        .collect();
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(block.len());
        f.write_all(&block[..n]).unwrap();
        remaining -= n;
    }
    f.sync_all().unwrap();
}

fn simulate_copy_with_session(src: &Path, dst: &Path) -> Session {
    let src_meta = src.metadata().unwrap();
    let src_size = src_meta.len();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(src).unwrap_or(0);

    let mut session = Session::new(src, dst, src_size, src_mtime, src_inode);

    let mut src_file = fs::File::open(src).unwrap();
    let mut dst_file = fs::File::create(dst).unwrap();
    let mut buf = vec![0u8; COPY_BLOCK_SIZE as usize];
    let mut block_hasher = blake3::Hasher::new();
    let mut bytes_in_block = 0u64;

    loop {
        let n = src_file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        dst_file.write_all(&buf[..n]).unwrap();
        block_hasher.update(&buf[..n]);
        bytes_in_block += n as u64;

        if bytes_in_block >= COPY_BLOCK_SIZE {
            let hash = block_hasher.finalize();
            session.add_block(*hash.as_bytes(), COPY_BLOCK_SIZE);
            block_hasher = blake3::Hasher::new();
            bytes_in_block -= COPY_BLOCK_SIZE;
        }
    }

    if bytes_in_block > 0 {
        let hash = block_hasher.finalize();
        session.add_block(*hash.as_bytes(), bytes_in_block);
    }

    dst_file.sync_all().unwrap();
    session.save().unwrap();
    session
}

#[test]
fn test_session_created_and_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 8 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);
    assert_eq!(session.block_hashes.len(), 2);

    let session_path = Session::session_path(&src, &dst);
    assert!(
        session_path.exists(),
        "session file should exist after save"
    );

    Session::remove(&src, &dst);
    assert!(
        !session_path.exists(),
        "session file should be removed after cleanup"
    );
}

#[test]
fn test_session_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 12 * 1024 * 1024);

    let original = simulate_copy_with_session(&src, &dst);

    let loaded = Session::load(&src, &dst).expect("session should load");
    assert_eq!(loaded.block_hashes.len(), original.block_hashes.len());
    assert_eq!(loaded.bytes_written, original.bytes_written);
    assert_eq!(loaded.src_size, original.src_size);

    for (a, b) in loaded.block_hashes.iter().zip(original.block_hashes.iter()) {
        assert_eq!(a, b);
    }

    Session::remove(&src, &dst);
}

#[test]
fn test_session_source_change_detection() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 8 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);

    let src_meta = src.metadata().unwrap();
    let mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let inode = durable_io::get_inode(&src).unwrap_or(0);
    assert!(session.source_matches(src_meta.len(), mtime, inode));
    assert!(!session.source_matches(src_meta.len() + 1, mtime, inode));
    assert!(!session.source_matches(src_meta.len(), mtime + 1, inode));

    Session::remove(&src, &dst);
}

#[test]
fn test_tail_block_verify_intact() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 20 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);
    assert_eq!(session.block_hashes.len(), 5);

    let resume_offset = session.find_resume_offset(&dst);
    assert_eq!(resume_offset, 20 * 1024 * 1024);

    Session::remove(&src, &dst);
}

#[test]
fn test_tail_block_verify_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 20 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);

    let dst_file = fs::OpenOptions::new().write(true).open(&dst).unwrap();
    dst_file.set_len(14 * 1024 * 1024).unwrap();
    drop(dst_file);

    let resume_offset = session.find_resume_offset(&dst);
    assert_eq!(resume_offset, 12 * 1024 * 1024);

    Session::remove(&src, &dst);
}

#[test]
fn test_tail_block_verify_corrupted_last_block() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 20 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);

    let mut f = fs::OpenOptions::new().write(true).open(&dst).unwrap();
    use std::io::Seek;
    f.seek(std::io::SeekFrom::End(-1)).unwrap();
    f.write_all(&[0xFF]).unwrap();
    drop(f);

    let resume_offset = session.find_resume_offset(&dst);
    assert_eq!(resume_offset, 16 * 1024 * 1024);

    Session::remove(&src, &dst);
}

#[test]
fn test_tail_block_verify_empty_dst() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 8 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);

    fs::remove_file(&dst).unwrap();

    let resume_offset = session.find_resume_offset(&dst);
    assert_eq!(resume_offset, 0);

    Session::remove(&src, &dst);
}

#[test]
fn test_tail_block_verify_multiple_corrupt_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 20 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);

    let mut f = fs::OpenOptions::new().write(true).open(&dst).unwrap();
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(12 * 1024 * 1024)).unwrap();
    f.write_all(&[0xFF]).unwrap();
    f.seek(std::io::SeekFrom::Start(16 * 1024 * 1024)).unwrap();
    f.write_all(&[0xFF]).unwrap();
    drop(f);

    let resume_offset = session.find_resume_offset(&dst);
    assert_eq!(resume_offset, 12 * 1024 * 1024);

    Session::remove(&src, &dst);
}

#[test]
fn test_inline_hash_matches_standalone() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    create_test_file(&src, 17 * 1024 * 1024);

    let standalone_hash = checksum::calculate_hash(&src).unwrap();

    let mut file = fs::File::open(&src).unwrap();
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; COPY_BLOCK_SIZE as usize];
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let inline_hash = hasher.finalize().to_hex().to_string();

    assert_eq!(
        standalone_hash, inline_hash,
        "inline streaming hash must match standalone hash"
    );
}

#[test]
fn test_session_expired() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 4 * 1024 * 1024);

    let mut session = simulate_copy_with_session(&src, &dst);

    session.updated_at = session.updated_at.saturating_sub(8 * 24 * 3600);
    session.save().unwrap();

    let loaded = Session::load(&src, &dst);
    assert!(loaded.is_none(), "expired session should not load");
}

#[test]
fn test_block_hashes_are_correct() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 12 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);
    assert_eq!(session.block_hashes.len(), 3);

    let mut file = fs::File::open(&src).unwrap();
    let mut buf = vec![0u8; COPY_BLOCK_SIZE as usize];

    for (i, expected_hash) in session.block_hashes.iter().enumerate() {
        let n = file.read(&mut buf).unwrap();
        assert_eq!(n, COPY_BLOCK_SIZE as usize, "block {} should be full", i);
        let hash = blake3::hash(&buf[..n]);
        assert_eq!(hash.as_bytes(), expected_hash, "block {} hash mismatch", i);
    }

    Session::remove(&src, &dst);
}

#[test]
fn test_resume_offset_no_blocks() {
    let session = Session::new(Path::new("/a"), Path::new("/b"), 1000, 0, 0);
    let resume = session.find_resume_offset(Path::new("/nonexistent"));
    assert_eq!(resume, 0);
}

#[test]
fn test_resume_offset_single_block_intact() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, COPY_BLOCK_SIZE as usize);

    let session = simulate_copy_with_session(&src, &dst);
    assert_eq!(session.block_hashes.len(), 1);

    let resume = session.find_resume_offset(&dst);
    assert_eq!(resume, COPY_BLOCK_SIZE);

    Session::remove(&src, &dst);
}

#[test]
fn test_session_corrupted_file_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 8 * 1024 * 1024);

    let session = simulate_copy_with_session(&src, &dst);
    let session_path = Session::session_path(&src, &dst);
    assert!(session_path.exists());
    drop(session);

    let mut data = fs::read(&session_path).unwrap();
    let mid = data.len() / 2;
    data[mid] ^= 0xFF;
    fs::write(&session_path, &data).unwrap();

    let loaded = Session::load(&src, &dst);
    assert!(loaded.is_none(), "corrupted session should not load");

    Session::remove(&src, &dst);
}

#[test]
fn test_session_truncated_file_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_test_file(&src, 8 * 1024 * 1024);

    simulate_copy_with_session(&src, &dst);
    let session_path = Session::session_path(&src, &dst);

    let data = fs::read(&session_path).unwrap();
    fs::write(&session_path, &data[..data.len() / 2]).unwrap();

    let loaded = Session::load(&src, &dst);
    assert!(loaded.is_none(), "truncated session should not load");

    Session::remove(&src, &dst);
}
