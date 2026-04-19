use std::fs;
use std::path::PathBuf;
use std::process::Command;

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
