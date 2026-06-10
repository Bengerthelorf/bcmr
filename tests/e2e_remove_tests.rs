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
fn e2e_remove_single_file_with_yes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    fs::write(&file, b"doomed").unwrap();

    let (ok, _, stderr) = run_bcmr(&["remove", "-t", "-y", file.to_str().unwrap()]);
    assert!(ok, "remove -y should succeed: {}", stderr);
    assert!(!file.exists(), "file should be removed: {}", stderr);
}

#[test]
fn e2e_remove_directory_requires_recursive_flag() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("d");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("f.txt"), b"x").unwrap();

    let (ok, _, stderr) = run_bcmr(&["remove", "-t", "-y", target.to_str().unwrap()]);
    assert!(
        !ok,
        "removing a directory without -r should fail: {}",
        stderr
    );
    assert!(
        stderr.contains("Is a directory"),
        "error should explain the directory refusal: {}",
        stderr
    );
    assert!(
        target.join("f.txt").exists(),
        "directory contents should be untouched: {}",
        stderr
    );
}

#[test]
fn e2e_remove_directory_tree_recursive() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("d");
    fs::create_dir_all(target.join("sub").join("deep")).unwrap();
    fs::write(target.join("f1.txt"), b"one").unwrap();
    fs::write(target.join("sub").join("f2.txt"), b"two").unwrap();
    fs::write(target.join("sub").join("deep").join("f3.txt"), b"three").unwrap();

    let (ok, _, stderr) = run_bcmr(&["remove", "-t", "-r", "-y", target.to_str().unwrap()]);
    assert!(ok, "recursive remove should succeed: {}", stderr);
    assert!(
        !target.exists(),
        "directory tree should be fully removed: {}",
        stderr
    );
}

#[test]
fn e2e_remove_dry_run_removes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("d");
    fs::create_dir_all(target.join("sub")).unwrap();
    fs::write(target.join("f1.txt"), b"one").unwrap();
    fs::write(target.join("sub").join("f2.txt"), b"two").unwrap();

    let (ok, stdout, stderr) =
        run_bcmr(&["remove", "-t", "-r", "-y", "-n", target.to_str().unwrap()]);
    assert!(ok, "dry-run remove should succeed: {}", stderr);
    assert!(
        stdout.contains("DRY RUN"),
        "dry-run should announce itself: {}",
        stdout
    );
    assert!(
        target.join("f1.txt").exists() && target.join("sub").join("f2.txt").exists(),
        "dry-run must leave every file in place: {}",
        stderr
    );
}

#[test]
fn e2e_remove_nonexistent_path_errors() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.txt");

    let (ok, _, stderr) = run_bcmr(&["remove", "-t", "-y", missing.to_str().unwrap()]);
    assert!(!ok, "removing a nonexistent path should fail: {}", stderr);
    assert!(
        stderr.contains("not found"),
        "error should name the missing source: {}",
        stderr
    );
}

#[test]
fn e2e_remove_force_skips_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.txt");

    let (ok, _, stderr) = run_bcmr(&["remove", "-t", "-f", missing.to_str().unwrap()]);
    assert!(
        ok,
        "remove -f should succeed on a nonexistent path (rm -f semantics): {}",
        stderr
    );
}

#[test]
fn e2e_remove_recursive_exclude_keeps_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("d");
    fs::create_dir_all(target.join("sub")).unwrap();
    let keep = target.join("sub").join("keep.log");
    fs::write(&keep, b"survivor").unwrap();
    fs::write(target.join("gone.txt"), b"doomed").unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "remove",
        "-t",
        "-r",
        "-y",
        "-e",
        r"\.log$",
        target.to_str().unwrap(),
    ]);
    assert!(
        ok,
        "recursive remove with exclude must not error on the surviving parent dirs: {}",
        stderr
    );
    assert!(
        keep.exists(),
        "excluded file should be left in place: {}",
        stderr
    );
    assert!(
        !target.join("gone.txt").exists(),
        "non-excluded file should be removed: {}",
        stderr
    );
}

#[test]
fn e2e_remove_exclude_leaves_matching_files() {
    let dir = tempfile::tempdir().unwrap();
    let keep = dir.path().join("keep.log");
    let gone = dir.path().join("gone.txt");
    fs::write(&keep, b"survivor").unwrap();
    fs::write(&gone, b"doomed").unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "remove",
        "-t",
        "-y",
        "-e",
        r"\.log$",
        keep.to_str().unwrap(),
        gone.to_str().unwrap(),
    ]);
    assert!(ok, "remove with exclude should succeed: {}", stderr);
    assert!(
        keep.exists(),
        "excluded file should be left in place: {}",
        stderr
    );
    assert!(
        !gone.exists(),
        "non-excluded file should be removed: {}",
        stderr
    );
}
