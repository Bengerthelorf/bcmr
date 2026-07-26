use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(not(windows))]
use std::time::{Duration, Instant};

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

#[cfg(unix)]
fn is_symlink(p: &Path) -> bool {
    p.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
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

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "copy should succeed: {}", stderr);

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

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "initial copy should succeed: {}", stderr);
    assert!(files_match(&src, &dst));

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

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "initial copy should succeed: {}", stderr);

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

    // Force mtime forward so the resume detector sees a source change;
    // wall-clock sleep is flaky on slow CI and adds 1 s per test run.
    create_random_file(&src, 80 * 1024 * 1024);
    let advanced = filetime::FileTime::from_unix_time((src_mtime + 1) as i64, 0);
    filetime::set_file_mtime(&src, advanced).unwrap();

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

    let (ok, _, stderr) = run_bcmr(&["copy", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok, "copy should succeed: {}", stderr);
    assert!(files_match(&src, &dst));

    assert!(!session_exists(&src, &dst));
}

#[test]
fn e2e_copy_verify_detects_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 80 * 1024 * 1024);

    let (ok, _, stderr) = run_bcmr(&["copy", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok, "initial copy should succeed: {}", stderr);

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

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "first resume should succeed: {}", stderr);
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

    let (ok, stdout, stderr) =
        run_bcmr(&["copy", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(
        !ok,
        "copy without -f should fail when target exists; stdout: {} stderr: {}",
        stdout, stderr
    );

    let dst_hash_after = checksum::calculate_hash(&dst).unwrap();
    assert_eq!(dst_hash_before, dst_hash_after);
}

#[test]
fn e2e_verified_force_corruption_keeps_existing_destination_and_cleans_stage() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"new verified content").unwrap();
    let old_bytes = b"old destination must survive";
    fs::write(&dst, old_bytes).unwrap();

    let (ok, _stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-f",
        "-y",
        "-V",
        "--test-mode",
        "corrupt_before_finalize",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);

    assert!(!ok, "corrupted verified copy must fail");
    assert!(
        stderr.contains("Verification failed"),
        "expected a verification error, got: {stderr}"
    );
    assert_eq!(fs::read(&dst).unwrap(), old_bytes);
    let remaining_stages: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".bcmr.stage.")
        })
        .collect();
    assert!(
        remaining_stages.is_empty(),
        "only the failed copy's own stage should be removed: {remaining_stages:?}"
    );
}

#[test]
fn e2e_verified_force_copy_replaces_existing_destination_after_staging() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"new verified content").unwrap();
    fs::write(&dst, b"old content").unwrap();

    let (ok, _stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-f",
        "-y",
        "-V",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);

    assert!(ok, "verified force copy should succeed: {stderr}");
    assert_eq!(fs::read(&dst).unwrap(), b"new verified content");
}

#[test]
fn e2e_forced_reflink_uses_the_unique_stage_before_verified_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"new reflinked content").unwrap();
    fs::write(&dst, b"old destination").unwrap();
    let capability_probe = dir.path().join("reflink-capability-probe.bin");
    if let Err(error) = reflink_copy::reflink(&src, &capability_probe) {
        eprintln!("skipping forced reflink test: filesystem does not support reflink: {error}");
        return;
    }
    fs::remove_file(&capability_probe).unwrap();

    let (ok, _stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-f",
        "-y",
        "-V",
        "--reflink",
        "force",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);

    assert!(
        ok,
        "forced reflink must not collide with bcmr's own stage: {stderr}"
    );
    assert_eq!(fs::read(&dst).unwrap(), b"new reflinked content");

    fs::write(&src, b"").unwrap();
    fs::write(&dst, b"old destination again").unwrap();
    let (ok, _stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-f",
        "-y",
        "-V",
        "--reflink",
        "force",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(
        ok,
        "forced reflink of an empty file should succeed: {stderr}"
    );
    assert_eq!(fs::metadata(&dst).unwrap().len(), 0);
}

#[test]
fn e2e_forced_reflink_rejects_direct_modes_before_touching_the_destination() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"complete replacement bytes").unwrap();

    for mode in ["--resume", "--append", "--strict"] {
        fs::write(&dst, b"original destination").unwrap();

        let (ok, _stdout, stderr) = run_bcmr(&[
            "copy",
            "-t",
            "-f",
            "-y",
            mode,
            "--reflink",
            "force",
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
        ]);

        assert!(!ok, "{mode} with forced reflink must be rejected");
        assert!(
            stderr.contains("incompatible"),
            "{mode} should report an incompatible option combination: {stderr}"
        );
        assert_eq!(
            fs::read(&dst).unwrap(),
            b"original destination",
            "{mode} validation must run before overwrite handling"
        );
    }
}

#[test]
fn e2e_forced_reflink_rejects_forced_sparse_before_touching_the_destination() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"complete replacement bytes").unwrap();
    fs::write(&dst, b"original destination").unwrap();

    let (ok, _stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-f",
        "-y",
        "--sparse",
        "force",
        "--reflink",
        "force",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);

    assert!(!ok, "forced sparse with forced reflink must be rejected");
    assert!(
        stderr.contains("incompatible"),
        "conflicting force modes should report an incompatible option combination: {stderr}"
    );
    assert_eq!(
        fs::read(&dst).unwrap(),
        b"original destination",
        "force-mode validation must run before overwrite handling"
    );
}

#[cfg(unix)]
#[test]
fn e2e_preserve_and_sync_apply_metadata_to_the_committed_destination() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"preserved content").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o640)).unwrap();
    let expected_mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(&src, expected_mtime).unwrap();
    fs::write(&dst, b"old content").unwrap();

    let (ok, _stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-f",
        "-y",
        "-p",
        "--sync",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);

    assert!(ok, "preserve + sync copy should succeed: {stderr}");
    let metadata = fs::metadata(&dst).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    let actual_mtime = filetime::FileTime::from_last_modification_time(&metadata);
    assert_eq!(actual_mtime.seconds(), expected_mtime.seconds());
}

#[cfg(not(windows))]
#[test]
fn e2e_pipeline_copy_honors_jobs_concurrency() {
    const FILES: usize = 12;
    const DELAY_MS: u64 = 600;
    // Fully serial copies would take FILES * DELAY_MS; anything one delay short
    // of that proves overlap, while staying loose enough for loaded CI runners.
    const THRESHOLD_MS: u64 = FILES as u64 * DELAY_MS - DELAY_MS;

    let dir = tempfile::tempdir().unwrap();
    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();

    let mut args: Vec<String> = vec![
        "copy".to_string(),
        "--jobs".to_string(),
        "4".to_string(),
        "--test-mode".to_string(),
        format!("delay:{DELAY_MS}"),
    ];

    for i in 0..FILES {
        let src = dir.path().join(format!("src-{i}.txt"));
        fs::write(&src, b"x").unwrap();
        args.push(src.to_string_lossy().into_owned());
    }
    args.push(dst_dir.to_string_lossy().into_owned());

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let start = Instant::now();
    let (ok, _, stderr) = run_bcmr(&arg_refs);
    let elapsed = start.elapsed();

    assert!(ok, "copy with --jobs should succeed: {}", stderr);
    assert!(
        elapsed < Duration::from_millis(THRESHOLD_MS),
        "expected file copies to overlap with --jobs; serial would take {}ms, elapsed={elapsed:?}",
        FILES as u64 * DELAY_MS
    );

    for i in 0..FILES {
        assert!(
            dst_dir.join(format!("src-{i}.txt")).exists(),
            "destination file src-{i}.txt missing"
        );
    }
}

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

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "intermediate resume should succeed: {}", stderr);

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

#[test]
fn e2e_resume_rewrites_preallocated_unverified_tail() {
    use std::io::Read;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    create_random_file(&src, (2 * block_size) as usize);

    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);
    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);

    let mut first_block = vec![0u8; block_size as usize];
    fs::File::open(&src)
        .unwrap()
        .read_exact(&mut first_block)
        .unwrap();
    let mut destination = fs::File::create(&dst).unwrap();
    destination.write_all(&first_block).unwrap();
    destination.set_len(src_meta.len()).unwrap();
    destination.sync_all().unwrap();
    session.add_block(*blake3::hash(&first_block).as_bytes(), block_size);
    session.save().unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "resume should repair the unverified tail: {stderr}");
    assert!(
        files_match(&src, &dst),
        "same length from preallocation is not proof of complete content"
    );
}

#[test]
fn e2e_resume_rejects_session_prefix_after_same_identity_source_rewrite() {
    use std::io::Read;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    create_random_file(&src, (2 * block_size) as usize);

    let src_meta = src.metadata().unwrap();
    let original_mtime = filetime::FileTime::from_last_modification_time(&src_meta);
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);
    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);

    let mut first_block = vec![0; block_size as usize];
    fs::File::open(&src)
        .unwrap()
        .read_exact(&mut first_block)
        .unwrap();
    fs::write(&dst, &first_block).unwrap();
    session.add_block(*blake3::hash(&first_block).as_bytes(), block_size);
    session.save().unwrap();

    let mut source = fs::OpenOptions::new().write(true).open(&src).unwrap();
    source.write_all(&[first_block[0] ^ 0xFF]).unwrap();
    source.sync_all().unwrap();
    drop(source);
    filetime::set_file_mtime(&src, original_mtime).unwrap();
    let current_meta = src.metadata().unwrap();
    let current_mtime = current_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(session.source_matches(
        current_meta.len(),
        current_mtime,
        durable_io::get_inode(&src).unwrap_or(0)
    ));

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(
        ok,
        "resume should restart from current source content: {stderr}"
    );
    assert!(files_match(&src, &dst));
}

#[test]
fn e2e_sparse_resume_zeroes_an_unverified_dirty_tail() {
    use std::io::Read;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    create_random_file(&src, block_size as usize);
    fs::OpenOptions::new()
        .append(true)
        .open(&src)
        .unwrap()
        .write_all(&vec![0; block_size as usize])
        .unwrap();

    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);
    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);
    let mut first_block = vec![0; block_size as usize];
    fs::File::open(&src)
        .unwrap()
        .read_exact(&mut first_block)
        .unwrap();
    let mut destination = fs::File::create(&dst).unwrap();
    destination.write_all(&first_block).unwrap();
    destination
        .write_all(&vec![0xA5; block_size as usize])
        .unwrap();
    destination.sync_all().unwrap();
    session.add_block(*blake3::hash(&first_block).as_bytes(), block_size);
    session.save().unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        "--sparse=force",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "sparse resume should recreate the zero tail: {stderr}");
    assert!(
        files_match(&src, &dst),
        "seeking over zeros must not preserve old destination bytes"
    );
}

#[test]
fn e2e_resume_without_session_does_not_trust_equal_length_and_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 1024 * 1024);
    fs::write(&dst, vec![0xA5; 1024 * 1024]).unwrap();

    let source_mtime = filetime::FileTime::from_last_modification_time(&src.metadata().unwrap());
    filetime::set_file_mtime(&dst, source_mtime).unwrap();
    assert_eq!(src.metadata().unwrap().len(), dst.metadata().unwrap().len());
    assert_eq!(
        src.metadata().unwrap().modified().unwrap(),
        dst.metadata().unwrap().modified().unwrap()
    );
    assert!(!session_exists(&src, &dst));
    assert!(!files_match(&src, &dst));

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "resume should safely restart: {stderr}");
    assert!(
        files_match(&src, &dst),
        "equal length and mtime are not content proof"
    );
}

#[test]
fn e2e_resume_truncates_bytes_beyond_fully_verified_source() {
    use std::io::Read;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    create_random_file(&src, block_size as usize);

    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);
    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);

    let mut source_bytes = vec![0u8; block_size as usize];
    fs::File::open(&src)
        .unwrap()
        .read_exact(&mut source_bytes)
        .unwrap();
    let mut destination = fs::File::create(&dst).unwrap();
    destination.write_all(&source_bytes).unwrap();
    destination.write_all(b"unverified trailing bytes").unwrap();
    destination.sync_all().unwrap();
    session.add_block(*blake3::hash(&source_bytes).as_bytes(), block_size);
    session.save().unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "-C",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "resume should remove the unverified suffix: {stderr}");
    assert_eq!(dst.metadata().unwrap().len(), src_meta.len());
    assert!(files_match(&src, &dst));
}

#[test]
fn e2e_append_refuses_to_truncate_a_longer_destination() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"source").unwrap();
    let original_destination = b"destination-is-longer".to_vec();
    fs::write(&dst, &original_destination).unwrap();

    let (ok, _, _) = run_bcmr(&[
        "copy",
        "-t",
        "--append",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(!ok, "append cannot establish a safe offset past source EOF");
    assert_eq!(
        fs::read(&dst).unwrap(),
        original_destination,
        "a refused append must leave the destination unchanged"
    );
}

#[test]
fn e2e_append_shorter_destination_preserves_prefix_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"verified-prefix-and-tail").unwrap();
    fs::write(&dst, b"verified-prefix").unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--append",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(
        ok,
        "append should complete from the explicit size offset: {stderr}"
    );
    assert_eq!(fs::read(&dst).unwrap(), fs::read(&src).unwrap());
}

#[test]
fn e2e_strict_shorter_destination_proves_prefix_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 1024 * 1024 + 123);
    let source = fs::read(&src).unwrap();
    fs::write(&dst, &source[..333_333]).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--strict",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(
        ok,
        "strict resume should complete a proven prefix: {stderr}"
    );
    assert!(files_match(&src, &dst));
}

#[test]
fn e2e_dry_run_resume_and_strict_use_content_proof() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 1024 * 1024);
    let corrupt_destination = vec![0x5A; 1024 * 1024];
    fs::write(&dst, &corrupt_destination).unwrap();
    let source_mtime = filetime::FileTime::from_last_modification_time(&src.metadata().unwrap());
    filetime::set_file_mtime(&dst, source_mtime).unwrap();

    for mode in ["--resume", "--strict"] {
        let (ok, stdout, stderr) = run_bcmr(&[
            "copy",
            "-n",
            mode,
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
        ]);
        assert!(ok, "{mode} dry-run should succeed: {stderr}");
        assert!(
            stdout.contains("OVERWRITE"),
            "{mode} dry-run must match the real content-based restart decision: {stdout}"
        );
        assert_eq!(
            fs::read(&dst).unwrap(),
            corrupt_destination,
            "dry-run must not change destination content"
        );
    }
}

#[test]
fn e2e_dry_run_forced_direct_modes_preview_the_forced_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    fs::write(&src, b"identical content").unwrap();
    fs::write(&dst, b"identical content").unwrap();

    for mode in ["--resume", "--strict", "--append"] {
        let (ok, stdout, stderr) = run_bcmr(&[
            "copy",
            "-n",
            "-f",
            "-y",
            mode,
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
        ]);
        assert!(ok, "{mode} forced dry-run should succeed: {stderr}");
        assert!(
            stdout.contains("OVERWRITE"),
            "{mode} dry-run must preview the existing forced direct-mode semantics: {stdout}"
        );
        assert_eq!(fs::read(&dst).unwrap(), b"identical content");
    }
}

#[test]
fn e2e_dry_run_resume_distinguishes_dirty_tail_from_short_destination() {
    use std::io::Read;

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    let block_size = bcmr::core::session::COPY_BLOCK_SIZE;
    create_random_file(&src, (2 * block_size) as usize);

    let src_meta = src.metadata().unwrap();
    let src_mtime = src_meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src_inode = durable_io::get_inode(&src).unwrap_or(0);
    let mut session = Session::new(&src, &dst, src_meta.len(), src_mtime, src_inode);
    let mut first_block = vec![0; block_size as usize];
    fs::File::open(&src)
        .unwrap()
        .read_exact(&mut first_block)
        .unwrap();
    let mut destination = fs::File::create(&dst).unwrap();
    destination.write_all(&first_block).unwrap();
    destination.set_len(src_meta.len()).unwrap();
    destination.sync_all().unwrap();
    session.add_block(*blake3::hash(&first_block).as_bytes(), block_size);
    session.save().unwrap();

    let session_path = Session::session_path(&src, &dst);
    let session_before = fs::read(&session_path).unwrap();
    let destination_hash_before = checksum::calculate_hash(&dst).unwrap();
    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "-n",
        "--resume",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);

    assert!(ok, "session-aware dry-run should succeed: {stderr}");
    assert!(
        stdout.contains("OVERWRITE"),
        "repairing an unverified tail requires truncation, not a plain append: {stdout}"
    );
    assert_eq!(
        fs::read(&session_path).unwrap(),
        session_before,
        "dry-run must not rewrite or remove session state"
    );
    assert_eq!(
        checksum::calculate_hash(&dst).unwrap(),
        destination_hash_before,
        "dry-run must not truncate the unverified destination tail"
    );

    destination.set_len(block_size).unwrap();
    destination.sync_all().unwrap();
    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "-n",
        "--resume",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "short-prefix dry-run should succeed: {stderr}");
    assert!(
        stdout.contains("APPEND"),
        "a destination ending exactly at the verified prefix is a plain append: {stdout}"
    );
    assert_eq!(dst.metadata().unwrap().len(), block_size);
    assert_eq!(fs::read(&session_path).unwrap(), session_before);
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

    let (ok, _, stderr) = run_bcmr(&["copy", "-p", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok, "copy -p should succeed: {}", stderr);

    let got = xattr::get(&dst, "user.bcmr.bin").unwrap().unwrap();
    assert_eq!(got, binary_value);
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_replicates_top_level_symlink() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), b"target body").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--no-deref",
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "stderr={stderr}");

    let landed = dst_dir.join("link.txt");
    assert!(is_symlink(&landed));
    assert_eq!(
        std::fs::read_link(&landed).unwrap(),
        std::path::Path::new("target.txt")
    );
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_default_dereferences() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), b"derefed").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "default deref copy should succeed: {}", stderr);

    let landed = dst_dir.join("link.txt");
    assert!(landed.symlink_metadata().unwrap().file_type().is_file());
    assert_eq!(fs::read(&landed).unwrap(), b"derefed");
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_preserves_dangling_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("broken.txt");
    std::os::unix::fs::symlink("nonexistent", &link).unwrap();
    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--no-deref",
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "copy of dangling symlink should succeed: {}", stderr);

    let landed = dst_dir.join("broken.txt");
    assert!(landed.symlink_metadata().is_ok());
    assert_eq!(
        std::fs::read_link(&landed).unwrap(),
        std::path::Path::new("nonexistent")
    );
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_recursive_preserves_nested_links() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tree");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("a.txt"), b"a").unwrap();
    let sub = src.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("real.txt"), b"real").unwrap();
    std::os::unix::fs::symlink("real.txt", sub.join("rel_link.txt")).unwrap();
    std::os::unix::fs::symlink(src.join("a.txt"), src.join("abs_link.txt")).unwrap();

    let dst_root = dir.path().join("dst");
    fs::create_dir(&dst_root).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-rt",
        "--no-deref",
        src.to_str().unwrap(),
        dst_root.to_str().unwrap(),
    ]);
    assert!(ok, "stderr={stderr}");

    let landed = dst_root.join("tree");
    assert!(landed.join("a.txt").is_file());
    let rel = landed.join("sub/rel_link.txt");
    assert!(is_symlink(&rel));
    assert_eq!(
        std::fs::read_link(&rel).unwrap(),
        std::path::Path::new("real.txt")
    );
    let abs = landed.join("abs_link.txt");
    assert!(is_symlink(&abs));
    // Absolute targets stay verbatim — bcmr doesn't rewrite them relative to dst.
    assert_eq!(std::fs::read_link(&abs).unwrap(), src.join("a.txt"));
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_overwrite_gate_refuses_existing_dst() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), b"target").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();
    let collide = dst_dir.join("link.txt");
    fs::write(&collide, b"PRE").unwrap();
    let pre_hash = checksum::calculate_hash(&collide).unwrap();

    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--no-deref",
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(
        !ok,
        "copy onto existing dst without -f should fail; stdout: {} stderr: {}",
        stdout, stderr
    );
    assert!(stderr.contains("already exists") || stderr.contains("TargetExists"));
    assert!(collide.is_file());
    assert_eq!(checksum::calculate_hash(&collide).unwrap(), pre_hash);
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_overwrite_with_force_replaces() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), b"target").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();
    let collide = dst_dir.join("link.txt");
    fs::write(&collide, b"PRE").unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-tfy",
        "--no-deref",
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "stderr={stderr}");

    assert!(is_symlink(&collide));
    assert_eq!(
        std::fs::read_link(&collide).unwrap(),
        std::path::Path::new("target.txt")
    );
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_overwrite_gate_treats_dangling_link_as_existing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), b"new").unwrap();
    let src_link = dir.path().join("src_link.txt");
    std::os::unix::fs::symlink("target.txt", &src_link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();
    let dangling = dst_dir.join("src_link.txt");
    std::os::unix::fs::symlink("does_not_exist", &dangling).unwrap();

    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--no-deref",
        src_link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(
        !ok,
        "copy onto existing dangling link should fail; stdout: {} stderr: {}",
        stdout, stderr
    );
    assert!(stderr.contains("already exists") || stderr.contains("TargetExists"));
    assert_eq!(
        std::fs::read_link(&dangling).unwrap(),
        std::path::Path::new("does_not_exist")
    );
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_replicates_top_level_symlink_to_dir() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real_dir");
    fs::create_dir(&real_dir).unwrap();
    fs::write(real_dir.join("inside.txt"), b"x").unwrap();
    let link = dir.path().join("link_to_dir");
    std::os::unix::fs::symlink("real_dir", &link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--no-deref",
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "stderr={stderr}");

    let landed = dst_dir.join("link_to_dir");
    assert!(is_symlink(&landed));
    assert_eq!(
        std::fs::read_link(&landed).unwrap(),
        std::path::Path::new("real_dir")
    );
    // The link target's contents must NOT be walked into and copied.
    assert!(!dst_dir.join("link_to_dir/inside.txt").exists() || is_symlink(&landed));
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_multi_source_mix() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("real.txt"), b"real").unwrap();
    fs::write(dir.path().join("target.txt"), b"target").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--no-deref",
        dir.path().join("real.txt").to_str().unwrap(),
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "stderr={stderr}");

    let real_dst = dst_dir.join("real.txt");
    let link_dst = dst_dir.join("link.txt");
    assert!(real_dst.symlink_metadata().unwrap().file_type().is_file());
    assert_eq!(fs::read(&real_dst).unwrap(), b"real");
    assert!(is_symlink(&link_dst));
    assert_eq!(
        std::fs::read_link(&link_dst).unwrap(),
        std::path::Path::new("target.txt")
    );
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_refuses_overwriting_directory_even_with_force() {
    // cp -P semantics: --force overwrites files / symlinks, never directories.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), b"x").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();
    let collide = dst_dir.join("link.txt");
    fs::create_dir(&collide).unwrap();
    fs::write(collide.join("inside.txt"), b"keep").unwrap();

    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "-tfy",
        "--no-deref",
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(
        !ok,
        "overwriting a directory must fail even with force; stdout: {} stderr: {}",
        stdout, stderr
    );
    assert!(stderr.contains("cannot overwrite directory"));
    assert!(collide.is_dir());
    assert!(collide.join("inside.txt").exists());
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_dry_run_emits_overwrite_for_existing_dst() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), b"x").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();

    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();
    fs::write(dst_dir.join("link.txt"), b"PRE").unwrap();

    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "-tnfy",
        "--no-deref",
        link.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "stderr={stderr}");
    let lower = stdout.to_lowercase();
    assert!(
        lower.contains("overwrite"),
        "expected OVERWRITE in dry-run, got: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn e2e_no_deref_remote_target_refuses() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("target.txt"), b"x").unwrap();
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink("target.txt", &link).unwrap();

    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "--no-deref",
        link.to_str().unwrap(),
        "no-such-host-bcmr-test.invalid:dst/",
    ]);
    assert!(
        !ok,
        "--no-deref with remote target should fail; stdout: {} stderr: {}",
        stdout, stderr
    );
    assert!(stderr.contains("--no-deref is currently only supported for local"));
}

#[test]
fn e2e_help_long_form_lists_examples_env_and_exit_codes() {
    let (_ok, stdout, _stderr) = run_bcmr(&["--help"]);
    for token in ["EXAMPLES:", "ENVIRONMENT:", "EXIT CODES:", "BCMR_CAS_DIR"] {
        assert!(
            stdout.contains(token),
            "expected '{token}' in --help, got: {stdout}"
        );
    }

    let (_ok, short_stdout, _) = run_bcmr(&["-h"]);
    for token in ["EXAMPLES:", "ENVIRONMENT:", "EXIT CODES:"] {
        assert!(
            !short_stdout.contains(token),
            "did not expect '{token}' in -h short form, got: {short_stdout}"
        );
    }
}

#[test]
fn e2e_subcommand_help_shows_examples() {
    for sub in ["copy", "move", "check", "remove"] {
        let (_ok, stdout, _stderr) = run_bcmr(&[sub, "--help"]);
        assert!(
            stdout.contains("EXAMPLES:"),
            "{sub} --help missing EXAMPLES: section, got: {stdout}"
        );
    }
}

#[test]
fn e2e_cross_host_copy_refuses_with_clear_error() {
    let (ok, _stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "host-a-bcmr-test.invalid:src.bin",
        "host-b-bcmr-test.invalid:dst/",
    ]);
    assert!(!ok, "expected non-zero exit");
    assert!(
        stderr.contains("does not support remote-to-remote"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("local intermediate"), "stderr: {stderr}");
}

#[test]
fn e2e_plain_flag_and_legacy_tui_alias_both_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("s.bin");
    fs::write(&src, b"plain-test").unwrap();

    for (flag, tag) in [("--plain", "long"), ("--tui", "alias"), ("-t", "short")] {
        let dst = dir.path().join(format!("d_{tag}.bin"));
        let (ok, _stdout, stderr) =
            run_bcmr(&["copy", flag, src.to_str().unwrap(), dst.to_str().unwrap()]);
        assert!(ok, "{flag} should be accepted, stderr: {stderr}");
        assert!(dst.exists(), "{flag} did not produce output");
    }
}

#[test]
fn e2e_help_advertises_plain_not_tui() {
    let (_ok, stdout, _stderr) = run_bcmr(&["copy", "--help"]);
    assert!(
        stdout.contains("--plain"),
        "expected --plain in help, got: {stdout}"
    );
    assert!(
        !stdout.contains("--tui"),
        "did not expect --tui in help (kept as hidden alias), got: {stdout}"
    );
}

#[test]
fn e2e_quiet_suppresses_done_line() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("s.bin");
    let dst = dir.path().join("d.bin");
    fs::write(&src, b"quiet-test").unwrap();

    let (ok, stdout, _stderr) =
        run_bcmr(&["copy", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok, "copy should succeed");
    assert!(dst.exists(), "dst should exist");
    assert!(
        !stdout.contains("Done:"),
        "expected no Done line under -q, got: {stdout}"
    );
}

#[test]
fn e2e_quiet_long_form_also_suppresses() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("s.bin");
    let dst = dir.path().join("d.bin");
    fs::write(&src, b"quiet-long").unwrap();

    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "--quiet",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "copy --quiet should succeed: {}", stderr);
    assert!(dst.exists());
    assert!(!stdout.contains("Done:"), "got: {stdout}");
}

#[test]
fn e2e_config_override_errors_loud_on_missing() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("s.bin");
    let dst = dir.path().join("d.bin");
    fs::write(&src, b"x").unwrap();

    let (ok, _stdout, stderr) = run_bcmr(&[
        "--config",
        "/nonexistent/bcmr-test.toml",
        "copy",
        "-q",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(
        !ok,
        "explicit --config to a missing path must fail (silent fallback would disable bookmarks/profiles)"
    );
    assert!(
        stderr.contains("not found"),
        "stderr should name the missing-file reason, got: {stderr}"
    );
    assert!(!dst.exists());
}

#[test]
fn e2e_config_override_layers_on_top_of_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("override.toml");
    fs::write(&cfg, "[progress]\nstyle = \"plain\"\n").unwrap();

    let src = dir.path().join("s.bin");
    let dst = dir.path().join("d.bin");
    fs::write(&src, b"y").unwrap();

    let (ok, _stdout, _stderr) = run_bcmr(&[
        "--config",
        cfg.to_str().unwrap(),
        "copy",
        "-q",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "override config should be honored");
    assert!(dst.exists());
}

#[test]
fn e2e_doctor_no_args_emits_local_checks() {
    let (ok, stdout, _stderr) = run_bcmr(&["doctor"]);
    assert!(ok, "doctor with no host should exit 0 on a healthy local");
    assert!(stdout.contains("Local:"), "got: {stdout}");
    assert!(stdout.contains("config file"), "got: {stdout}");
    assert!(stdout.contains("jobs dir"), "got: {stdout}");
    assert!(stdout.contains("color env"), "got: {stdout}");
    assert!(
        stdout.contains("Pass host arguments"),
        "missing host hint, got: {stdout}"
    );
}

#[test]
fn e2e_doctor_unreachable_host_fails_with_exit_1() {
    let (ok, _stdout, _stderr) = run_bcmr(&["doctor", "this-host-does-not-resolve.invalid"]);
    assert!(!ok, "doctor against unreachable host should exit non-zero");
}

#[test]
fn e2e_doctor_json_emits_structured_report() {
    let (ok, stdout, stderr) = run_bcmr(&["--json", "doctor"]);
    assert!(ok, "doctor --json should succeed: {}", stderr);
    let trimmed = stdout.trim();
    assert!(trimmed.starts_with('{'), "got: {stdout}");
    for token in ["\"bcmr_version\"", "\"local\"", "\"hosts\"", "\"ok\":true"] {
        assert!(trimmed.contains(token), "missing {token} in JSON: {stdout}");
    }
}

#[test]
fn e2e_same_host_remote_to_remote_copy_refuses() {
    let (ok, stdout, stderr) = run_bcmr(&[
        "copy",
        "-t",
        "host-x-bcmr-test.invalid:src.bin",
        "host-x-bcmr-test.invalid:dst/",
    ]);
    assert!(
        !ok,
        "same-host remote-to-remote should fail; stdout: {} stderr: {}",
        stdout, stderr
    );
    assert!(
        stderr.contains("does not support remote-to-remote"),
        "stderr: {stderr}"
    );
}

#[test]
fn e2e_cross_host_move_refuses_with_clear_error() {
    let (ok, _stdout, stderr) = run_bcmr(&[
        "move",
        "-t",
        "host-a-bcmr-test.invalid:src.bin",
        "host-b-bcmr-test.invalid:dst/",
    ]);
    assert!(!ok, "expected non-zero exit");
    assert!(
        stderr.contains("does not support remote-to-remote"),
        "stderr: {stderr}"
    );
}
