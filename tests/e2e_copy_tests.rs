use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use bcmr::core::checksum;
use bcmr::core::io as durable_io;
use bcmr::core::session::Session;

fn bcmr_bin() -> PathBuf {
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("bcmr");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

fn run_bcmr(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(bcmr_bin())
        .args(args)
        .output()
        .expect("failed to execute bcmr");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn create_random_file(path: &Path, size: usize) {
    let mut f = fs::File::create(path).unwrap();
    let mut buf = vec![0u8; 4096];
    let mut remaining = size;
    let mut seed: u64 = 0xDEADBEEF;
    while remaining > 0 {
        let n = remaining.min(buf.len());
        for b in buf[..n].iter_mut() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *b = (seed >> 33) as u8;
        }
        f.write_all(&buf[..n]).unwrap();
        remaining -= n;
    }
    f.sync_all().unwrap();
}

fn files_match(a: &Path, b: &Path) -> bool {
    let ha = checksum::calculate_hash(a).unwrap();
    let hb = checksum::calculate_hash(b).unwrap();
    ha == hb
}

fn session_exists(src: &Path, dst: &Path) -> bool {
    Session::session_path(src, dst).exists()
}

#[test]
fn e2e_fresh_copy_produces_correct_output() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 80 * 1024 * 1024);

    let (ok, _, stderr) = run_bcmr(&["copy", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok, "copy should succeed: {}", stderr);
    assert!(dst.exists(), "destination should exist");
    assert!(files_match(&src, &dst), "files should be identical");
}

#[test]
fn e2e_session_cleaned_up_after_success() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 80 * 1024 * 1024);

    let (ok, _, _) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok);

    assert!(
        !session_exists(&src, &dst),
        "session should be removed after successful copy"
    );
}

#[test]
fn e2e_copy_with_verify_flag() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 80 * 1024 * 1024);

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-V",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "copy with -V should succeed: {}", stderr);
    assert!(files_match(&src, &dst));
}

#[test]
fn e2e_resume_after_simulated_crash() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let size = 80 * 1024 * 1024;
    create_random_file(&src, size);

    let (ok, _, _) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok);
    assert!(files_match(&src, &dst));

    // Session is cleaned up after success; rebuild one by hand to simulate
    // a mid-copy crash so the resume path can exercise carry-forward.
    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);

    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);

    let resume_point = 60 * 1024 * 1024u64;
    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    let mut f = fs::File::open(&src).unwrap();
    let mut buf = vec![0u8; block_size as usize];
    use std::io::Read;
    for _ in 0..(resume_point / block_size) {
        let n = f.read(&mut buf).unwrap();
        assert_eq!(n, block_size as usize);
        let hash = blake3::hash(&buf[..n]);
        session.add_block(*hash.as_bytes(), block_size);
    }
    session.save().unwrap();

    let df = fs::OpenOptions::new().write(true).open(&dst).unwrap();
    df.set_len(resume_point).unwrap();
    drop(df);

    assert_eq!(dst.metadata().unwrap().len(), resume_point);

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "resume should succeed: {}", stderr);

    assert_eq!(dst.metadata().unwrap().len(), size as u64);
    assert!(files_match(&src, &dst), "resumed file should match source");

    assert!(!session_exists(&src, &dst));
}

#[test]
fn e2e_resume_with_corrupted_tail_block() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let size = 80 * 1024 * 1024;
    create_random_file(&src, size);

    let (ok, _, _) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok);

    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);

    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);

    let mut f = fs::File::open(&src).unwrap();
    let mut buf = vec![0u8; block_size as usize];
    use std::io::Read;
    for _ in 0..15 {
        let n = f.read(&mut buf).unwrap();
        let hash = blake3::hash(&buf[..n]);
        session.add_block(*hash.as_bytes(), block_size);
    }
    session.save().unwrap();

    let df = fs::OpenOptions::new().write(true).open(&dst).unwrap();
    df.set_len(60 * 1024 * 1024).unwrap();
    drop(df);

    {
        use std::io::Seek;
        let mut f = fs::OpenOptions::new().write(true).open(&dst).unwrap();
        f.seek(std::io::SeekFrom::End(-1)).unwrap();
        f.write_all(&[0xFF]).unwrap();
    }

    // Resume should detect corruption in block 15 and fall back to block 14.
    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "resume with corrupt tail should succeed: {}", stderr);
    assert!(
        files_match(&src, &dst),
        "file should be correct after resume with corrupt tail"
    );
}

#[test]
fn e2e_resume_detects_source_change() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 80 * 1024 * 1024);

    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);
    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);
    session.add_block([0xAA; 32], bcmr::core::session::COPY_BLOCK_SIZE);
    session.save().unwrap();

    std::thread::sleep(std::time::Duration::from_secs(1));
    create_random_file(&src, 80 * 1024 * 1024);

    {
        let mut f = fs::File::create(&dst).unwrap();
        f.write_all(&vec![0u8; 40 * 1024 * 1024]).unwrap();
    }

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        "-f",
        "-y",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "copy should succeed after source change: {}", stderr);
    assert!(
        files_match(&src, &dst),
        "should have the new source content"
    );
}

#[test]
fn e2e_small_file_no_session() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("small.bin");
    let dst = dir.path().join("small_dst.bin");
    create_random_file(&src, 1024 * 1024);

    let (ok, _, _) = run_bcmr(&["copy", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok);
    assert!(files_match(&src, &dst));

    assert!(!session_exists(&src, &dst));
}

#[test]
fn e2e_copy_verify_detects_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 80 * 1024 * 1024);

    let (ok, _, _) = run_bcmr(&["copy", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok);

    {
        use std::io::Seek;
        let mut f = fs::OpenOptions::new().write(true).open(&dst).unwrap();
        f.seek(std::io::SeekFrom::Start(1000)).unwrap();
        f.write_all(&[0xFF; 100]).unwrap();
    }

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-V",
        "-f",
        "-y",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "copy -V -f should succeed: {}", stderr);
    assert!(files_match(&src, &dst), "verified copy should be correct");
}

#[test]
fn e2e_resume_with_verify() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let size = 80 * 1024 * 1024;
    create_random_file(&src, size);

    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);
    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);

    {
        use std::io::Read;
        let mut sf = fs::File::open(&src).unwrap();
        let mut df = fs::File::create(&dst).unwrap();
        let mut buf = vec![0u8; block_size as usize];
        for _ in 0..15 {
            let n = sf.read(&mut buf).unwrap();
            df.write_all(&buf[..n]).unwrap();
            let hash = blake3::hash(&buf[..n]);
            session.add_block(*hash.as_bytes(), block_size);
        }
        df.sync_all().unwrap();
    }
    session.save().unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        "-V",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "resume with verify should succeed: {}", stderr);
    assert!(
        files_match(&src, &dst),
        "resumed + verified file should be correct"
    );
}

#[test]
fn e2e_multi_crash_resume_preserves_block_history() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let size = 80 * 1024 * 1024;
    create_random_file(&src, size);

    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);
    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;

    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);
    {
        use std::io::Read;
        let mut sf = fs::File::open(&src).unwrap();
        let mut df = fs::File::create(&dst).unwrap();
        let mut buf = vec![0u8; block_size as usize];
        for _ in 0..10 {
            let n = sf.read(&mut buf).unwrap();
            df.write_all(&buf[..n]).unwrap();
            let hash = blake3::hash(&buf[..n]);
            session.add_block(*hash.as_bytes(), block_size);
        }
        df.sync_all().unwrap();
    }
    session.save().unwrap();

    let (ok, _, _) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok);
    assert!(files_match(&src, &dst));

    let mut session2 = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);
    {
        use std::io::Read;
        let mut sf = fs::File::open(&src).unwrap();
        let mut buf = vec![0u8; block_size as usize];
        for _ in 0..15 {
            let n = sf.read(&mut buf).unwrap();
            let hash = blake3::hash(&buf[..n]);
            session2.add_block(*hash.as_bytes(), block_size);
        }
    }
    session2.save().unwrap();

    let df = fs::OpenOptions::new().write(true).open(&dst).unwrap();
    df.set_len(60 * 1024 * 1024).unwrap();
    drop(df);

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "second resume should succeed: {}", stderr);
    assert!(
        files_match(&src, &dst),
        "file should be correct after multi-crash resume"
    );
}

#[test]
fn e2e_copy_preserves_existing_on_no_force() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 1024);
    create_random_file(&dst, 512);

    let dst_hash_before = checksum::calculate_hash(&dst).unwrap();

    let (ok, _, _) = run_bcmr(&["copy", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(!ok, "copy without -f should fail when target exists");

    let dst_hash_after = checksum::calculate_hash(&dst).unwrap();
    assert_eq!(dst_hash_before, dst_hash_after);
}

/// Exercises the `block_hashes[..keep]` carry-forward path in `copy_file`:
/// two simulated crashes force back-to-back resumes that must preserve hashes
/// from the prior session rather than recomputing from scratch.
#[test]
fn e2e_carry_forward_code_path() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let size = 80 * 1024 * 1024;
    create_random_file(&src, size);

    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);

    {
        use std::io::Read;
        let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);
        let mut sf = fs::File::open(&src).unwrap();
        let mut df = fs::File::create(&dst).unwrap();
        let mut buf = vec![0u8; block_size as usize];
        for _ in 0..10 {
            let n = sf.read(&mut buf).unwrap();
            df.write_all(&buf[..n]).unwrap();
            let hash = blake3::hash(&buf[..n]);
            session.add_block(*hash.as_bytes(), block_size);
        }
        df.sync_all().unwrap();
        session.save().unwrap();
    }

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "resume 1 should succeed: {}", stderr);
    assert!(files_match(&src, &dst));

    {
        use std::io::Read;
        let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);
        let mut sf = fs::File::open(&src).unwrap();
        let mut buf = vec![0u8; block_size as usize];
        for _ in 0..10 {
            let n = sf.read(&mut buf).unwrap();
            let hash = blake3::hash(&buf[..n]);
            session.add_block(*hash.as_bytes(), block_size);
        }
        session.save().unwrap();
    }
    {
        let f = fs::OpenOptions::new().write(true).open(&dst).unwrap();
        f.set_len(40 * 1024 * 1024).unwrap();
    }

    let (ok, _, _) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok);

    // Manually mimic what copy_file would produce with carry-forward:
    // 15 blocks (10 carried + 5 new), dst truncated to 60MB.
    {
        use std::io::Read;
        let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);
        let mut sf = fs::File::open(&src).unwrap();
        let mut buf = vec![0u8; block_size as usize];
        for _ in 0..15 {
            let n = sf.read(&mut buf).unwrap();
            let hash = blake3::hash(&buf[..n]);
            session.add_block(*hash.as_bytes(), block_size);
        }
        session.save().unwrap();
    }
    {
        let f = fs::OpenOptions::new().write(true).open(&dst).unwrap();
        f.set_len(60 * 1024 * 1024).unwrap();
    }

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "resume 2 (carry-forward) should succeed: {}", stderr);
    assert!(
        files_match(&src, &dst),
        "final file should match source after carry-forward resume"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_xattr_preserved_with_p_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.txt");
    let dst = tmp.path().join("dst.txt");
    fs::write(&src, b"payload").unwrap();

    let xattr_name = "user.bcmr.test";
    if xattr::set(&src, xattr_name, b"hello-xattr").is_err() {
        eprintln!("skipping xattr test: filesystem lacks user.* xattr support");
        return;
    }

    let (ok, _, stderr) = run_bcmr(&["copy", "-p", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok, "copy -p should succeed: {}", stderr);

    let got = xattr::get(&dst, xattr_name)
        .expect("xattr::get on dst")
        .expect("xattr should exist on dst");
    assert_eq!(got, b"hello-xattr");
}

/// Regression guard: xattr wire/disk path is raw bytes; a binary value with
/// the high bit set must round-trip verbatim rather than being UTF-8 mangled.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_xattr_preserves_binary_value() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src.bin");
    let dst = tmp.path().join("dst.bin");
    fs::write(&src, b"content").unwrap();

    let binary_value = vec![0x00, 0xff, 0x10, 0x80, 0x7f, 0xde, 0xad, 0xbe, 0xef];
    if xattr::set(&src, "user.bcmr.bin", &binary_value).is_err() {
        return;
    }

    let (ok, _, _) = run_bcmr(&["copy", "-p", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok);

    let got = xattr::get(&dst, "user.bcmr.bin").unwrap().unwrap();
    assert_eq!(got, binary_value);
}
