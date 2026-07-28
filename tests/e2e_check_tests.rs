use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

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

#[test]
fn e2e_check_redirected_human_output_has_no_ansi_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.bin");
    let destination = dir.path().join("destination.bin");
    fs::write(&source, b"same").unwrap();
    fs::write(&destination, b"same").unwrap();

    let (ok, stdout, stderr) = run_bcmr(&[
        "check",
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
    ]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("In sync."), "{stdout:?}");
    assert!(
        !stdout.contains('\u{1b}'),
        "redirected output leaked ANSI: {stdout:?}"
    );
}

#[test]
fn e2e_check_multi_source_into_dir_does_not_false_missing() {
    let dir = tempfile::tempdir().unwrap();
    let src_a = dir.path().join("a.txt");
    let src_b = dir.path().join("b.txt");
    let dst = dir.path().join("dst");
    fs::create_dir(&dst).unwrap();
    fs::write(&src_a, b"alpha").unwrap();
    fs::write(&src_b, b"beta").unwrap();
    fs::write(dst.join("a.txt"), b"alpha").unwrap();
    fs::write(dst.join("b.txt"), b"beta").unwrap();
    fs::write(dst.join("c.txt"), b"unrelated").unwrap();

    let (ok, stdout, _stderr) = run_bcmr(&[
        "check",
        src_a.to_str().unwrap(),
        src_b.to_str().unwrap(),
        dst.to_str().unwrap(),
        "--json",
    ]);
    assert!(ok);
    assert!(stdout.contains("\"in_sync\":true"), "got: {stdout}");
    assert!(!stdout.contains("c.txt"), "sibling leaked: {stdout}");
}

#[test]
fn e2e_check_multi_source_detects_real_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let src_a = dir.path().join("a.txt");
    let src_b = dir.path().join("b.txt");
    let dst = dir.path().join("dst");
    fs::create_dir(&dst).unwrap();
    fs::write(&src_a, b"alpha").unwrap();
    fs::write(&src_b, b"beta").unwrap();
    fs::write(dst.join("a.txt"), b"alpha-MODIFIED").unwrap();

    let (_, stdout, _stderr) = run_bcmr(&[
        "check",
        src_a.to_str().unwrap(),
        src_b.to_str().unwrap(),
        dst.to_str().unwrap(),
        "--json",
    ]);
    assert!(stdout.contains("\"in_sync\":false"));
    assert!(stdout.contains("\"modified\":[{\"path\":\"a.txt\""));
    assert!(stdout.contains("\"added\":[{\"path\":\"b.txt\""));
    assert!(!stdout.contains("c.txt"));
}

#[test]
fn e2e_check_same_content_drifted_mtime_into_dir_is_in_sync() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("x.bin");
    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();
    let dst = dst_dir.join("x.bin");
    fs::write(&src, b"1234567890").unwrap();
    fs::write(&dst, b"1234567890").unwrap();

    let old = SystemTime::now() - Duration::from_secs(3600);
    let ft = filetime::FileTime::from_system_time(old);
    filetime::set_file_mtime(&dst, ft).unwrap();

    let (ok, stdout, _stderr) = run_bcmr(&[
        "check",
        src.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(ok, "expected exit 0, got: {stdout}");
    assert!(stdout.contains("\"in_sync\":true"), "got: {stdout}");
}

#[test]
fn e2e_check_same_content_drifted_mtime_with_no_hash_reports_modified() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("x.bin");
    let dst_dir = dir.path().join("dst");
    fs::create_dir(&dst_dir).unwrap();
    let dst = dst_dir.join("x.bin");
    fs::write(&src, b"1234567890").unwrap();
    fs::write(&dst, b"1234567890").unwrap();

    let old = SystemTime::now() - Duration::from_secs(3600);
    let ft = filetime::FileTime::from_system_time(old);
    filetime::set_file_mtime(&dst, ft).unwrap();

    let (_, stdout, _stderr) = run_bcmr(&[
        "check",
        "--no-hash",
        src.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
        "--json",
    ]);
    assert!(stdout.contains("\"in_sync\":false"), "got: {stdout}");
    assert!(
        stdout.contains("\"modified\":[{\"path\":\"x.bin\""),
        "got: {stdout}"
    );
}

#[test]
fn e2e_check_file_to_file_same_size_diff_content_is_modified() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    fs::write(&a, b"AAAAAA").unwrap();
    fs::write(&b, b"BBBBBB").unwrap();

    let now = SystemTime::now();
    let ft = filetime::FileTime::from_system_time(now);
    filetime::set_file_mtime(&a, ft).unwrap();
    filetime::set_file_mtime(&b, ft).unwrap();

    let (_, stdout, _stderr) =
        run_bcmr(&["check", a.to_str().unwrap(), b.to_str().unwrap(), "--json"]);
    assert!(stdout.contains("\"in_sync\":false"), "got: {stdout}");
    assert!(
        stdout.contains("\"modified\":[{\"path\":\"a.bin\""),
        "got: {stdout}"
    );
}

#[test]
fn e2e_check_file_to_file_same_content_is_in_sync() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    fs::write(&a, b"hello world").unwrap();
    fs::write(&b, b"hello world").unwrap();

    let old = SystemTime::now() - Duration::from_secs(7200);
    let ft = filetime::FileTime::from_system_time(old);
    filetime::set_file_mtime(&b, ft).unwrap();

    let (ok, stdout, _stderr) =
        run_bcmr(&["check", a.to_str().unwrap(), b.to_str().unwrap(), "--json"]);
    assert!(ok, "expected exit 0, got: {stdout}");
    assert!(stdout.contains("\"in_sync\":true"), "got: {stdout}");
}

#[test]
fn e2e_check_file_to_file_dst_missing_is_added() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    fs::write(&a, b"content").unwrap();

    let (_, stdout, _stderr) =
        run_bcmr(&["check", a.to_str().unwrap(), b.to_str().unwrap(), "--json"]);
    assert!(stdout.contains("\"in_sync\":false"), "got: {stdout}");
    assert!(
        stdout.contains("\"added\":[{\"path\":\"a.bin\""),
        "got: {stdout}"
    );
    assert!(stdout.contains("\"modified\":0"), "got: {stdout}");
}

#[test]
fn e2e_check_dir_to_dir_size_match_drifted_mtime_is_in_sync() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_parent = dir.path().join("dst");
    fs::create_dir(&src_dir).unwrap();
    fs::create_dir(&dst_parent).unwrap();
    let dst_mirror = dst_parent.join("src");
    fs::create_dir(&dst_mirror).unwrap();
    fs::write(src_dir.join("f.bin"), b"same content").unwrap();
    fs::write(dst_mirror.join("f.bin"), b"same content").unwrap();

    let old = SystemTime::now() - Duration::from_secs(3600);
    let ft = filetime::FileTime::from_system_time(old);
    filetime::set_file_mtime(dst_mirror.join("f.bin"), ft).unwrap();

    let (ok, stdout, _stderr) = run_bcmr(&[
        "check",
        "-r",
        src_dir.to_str().unwrap(),
        dst_parent.to_str().unwrap(),
        "--json",
    ]);
    assert!(ok, "expected exit 0, got: {stdout}");
    assert!(stdout.contains("\"in_sync\":true"), "got: {stdout}");
}
