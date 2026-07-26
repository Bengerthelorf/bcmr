use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use bcmr::core::checksum;

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

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(target, link).is_ok()
}

#[cfg(not(any(unix, windows)))]
fn create_file_symlink(_target: &Path, _link: &Path) -> bool {
    false
}

#[test]
fn e2e_move_single_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
    let dst = dir.path().join("dst.txt");
    fs::write(&src, b"move me").unwrap();

    let (ok, _, stderr) = run_bcmr(&["move", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok, "move should succeed: {}", stderr);
    assert!(dst.exists(), "destination should exist: {}", stderr);
    assert!(
        !src.exists(),
        "source should be gone after move: {}",
        stderr
    );
    assert_eq!(
        fs::read(&dst).unwrap(),
        b"move me",
        "destination content should match original"
    );
}

#[test]
fn e2e_move_directory_recursive() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("tree");
    let dst_dir = dir.path().join("moved");
    fs::create_dir_all(src_dir.join("sub")).unwrap();
    fs::write(src_dir.join("a.txt"), b"alpha").unwrap();
    fs::write(src_dir.join("sub").join("b.txt"), b"beta").unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-r",
        src_dir.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "recursive move should succeed: {}", stderr);
    assert!(
        !src_dir.exists(),
        "source tree should be gone after move: {}",
        stderr
    );
    assert_eq!(
        fs::read(dst_dir.join("a.txt")).unwrap(),
        b"alpha",
        "top-level file should arrive intact"
    );
    assert_eq!(
        fs::read(dst_dir.join("sub").join("b.txt")).unwrap(),
        b"beta",
        "nested file should arrive intact"
    );
}

#[test]
fn e2e_move_directory_without_recursive_fails() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("tree");
    let dst_dir = dir.path().join("moved");
    fs::create_dir(&src_dir).unwrap();
    fs::write(src_dir.join("a.txt"), b"alpha").unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        src_dir.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(!ok, "moving a directory without -r should fail: {}", stderr);
    assert!(
        src_dir.join("a.txt").exists(),
        "source tree should be untouched after refused move: {}",
        stderr
    );
    assert!(
        !dst_dir.exists(),
        "destination should not be created after refused move: {}",
        stderr
    );
}

#[test]
fn e2e_move_file_into_existing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
    let dst_dir = dir.path().join("hold");
    fs::write(&src, b"payload").unwrap();
    fs::create_dir(&dst_dir).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        src.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "move into existing dir should succeed: {}", stderr);
    assert!(!src.exists(), "source should be gone: {}", stderr);
    assert_eq!(
        fs::read(dst_dir.join("src.txt")).unwrap(),
        b"payload",
        "file should land under dest dir keeping its name"
    );
}

#[test]
fn e2e_move_dir_into_existing_dir_joins_source_name() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("tree");
    let dst_dir = dir.path().join("hold");
    fs::create_dir_all(src_dir.join("sub")).unwrap();
    fs::write(src_dir.join("sub").join("b.txt"), b"beta").unwrap();
    fs::create_dir(&dst_dir).unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-r",
        src_dir.to_str().unwrap(),
        dst_dir.to_str().unwrap(),
    ]);
    assert!(ok, "move dir into existing dir should succeed: {}", stderr);
    assert!(!src_dir.exists(), "source tree should be gone: {}", stderr);
    assert_eq!(
        fs::read(dst_dir.join("tree").join("sub").join("b.txt")).unwrap(),
        b"beta",
        "tree should land as hold/tree, not replace hold"
    );
}

#[test]
fn e2e_move_refuses_overwrite_without_force_succeeds_with_force_yes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
    let dst = dir.path().join("dst.txt");
    fs::write(&src, b"new content").unwrap();
    fs::write(&dst, b"old content").unwrap();

    let (ok, _, stderr) = run_bcmr(&["move", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(
        !ok,
        "move without -f should fail when target exists: {}",
        stderr
    );
    assert_eq!(
        fs::read(&dst).unwrap(),
        b"old content",
        "existing destination should be untouched after refused move"
    );
    assert!(
        src.exists(),
        "source should remain after refused move: {}",
        stderr
    );

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-f",
        "-y",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    assert!(ok, "move with -f -y should succeed: {}", stderr);
    assert_eq!(
        fs::read(&dst).unwrap(),
        b"new content",
        "destination should hold the moved content after forced overwrite"
    );
    assert!(
        !src.exists(),
        "source should be gone after move: {}",
        stderr
    );
}

#[test]
fn e2e_move_force_refuses_same_file_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    fs::write(&file, b"same file payload").unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-f",
        "-y",
        file.to_str().unwrap(),
        file.to_str().unwrap(),
    ]);

    assert!(!ok, "move onto itself must fail: {stderr}");
    assert_eq!(
        fs::read(&file).unwrap(),
        b"same file payload",
        "same-path refusal must preserve the file"
    );
}

#[test]
fn e2e_move_force_refuses_file_into_its_own_parent_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    fs::write(&file, b"parent payload").unwrap();

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-f",
        "-y",
        file.to_str().unwrap(),
        dir.path().to_str().unwrap(),
    ]);

    assert!(
        !ok,
        "move into its own parent must fail before overwrite: {stderr}"
    );
    assert_eq!(
        fs::read(&file).unwrap(),
        b"parent payload",
        "parent-directory refusal must preserve the file"
    );
}

#[test]
fn e2e_move_force_refuses_hard_link_alias_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    let alias = dir.path().join("alias.txt");
    fs::write(&file, b"hard-link payload").unwrap();
    if fs::hard_link(&file, &alias).is_err() {
        return;
    }

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-f",
        "-y",
        file.to_str().unwrap(),
        alias.to_str().unwrap(),
    ]);

    assert!(!ok, "move onto a hard-link alias must fail: {stderr}");
    for path in [&file, &alias] {
        assert_eq!(
            fs::read(path).unwrap(),
            b"hard-link payload",
            "hard-link refusal must preserve {}",
            path.display()
        );
    }
}

#[test]
fn e2e_move_force_refuses_file_onto_symlink_alias_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    let alias = dir.path().join("alias.txt");
    fs::write(&file, b"symlink payload").unwrap();
    if !create_file_symlink(&file, &alias) {
        return;
    }

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-f",
        "-y",
        file.to_str().unwrap(),
        alias.to_str().unwrap(),
    ]);

    assert!(!ok, "move onto a symlink alias must fail: {stderr}");
    for path in [&file, &alias] {
        assert_eq!(
            fs::read(path).unwrap(),
            b"symlink payload",
            "symlink refusal must preserve {}",
            path.display()
        );
    }
}

#[test]
fn e2e_move_force_refuses_symlink_onto_its_underlying_file_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    let alias = dir.path().join("alias.txt");
    fs::write(&file, b"symlink source payload").unwrap();
    if !create_file_symlink(&file, &alias) {
        return;
    }

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-f",
        "-y",
        alias.to_str().unwrap(),
        file.to_str().unwrap(),
    ]);

    assert!(
        !ok,
        "moving a symlink onto its underlying file must fail: {stderr}"
    );
    for path in [&file, &alias] {
        assert_eq!(
            fs::read(path).unwrap(),
            b"symlink source payload",
            "symlink-source refusal must preserve {}",
            path.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn e2e_move_force_rename_failure_preserves_existing_destination() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    let dst_dir = dir.path().join("dst");
    fs::create_dir(&src_dir).unwrap();
    fs::create_dir(&dst_dir).unwrap();
    let src = src_dir.join("file.txt");
    let dst = dst_dir.join("file.txt");
    fs::write(&src, b"new payload").unwrap();
    fs::write(&dst, b"old payload").unwrap();

    let original_permissions = fs::metadata(&src_dir).unwrap().permissions();
    let mut readonly = original_permissions.clone();
    readonly.set_mode(0o555);
    fs::set_permissions(&src_dir, readonly).unwrap();

    if fs::File::create(src_dir.join("permission-probe")).is_ok() {
        let _ = fs::remove_file(src_dir.join("permission-probe"));
        fs::set_permissions(&src_dir, original_permissions).unwrap();
        return;
    }

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-f",
        "-y",
        "-q",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
    ]);
    fs::set_permissions(&src_dir, original_permissions).unwrap();

    assert!(
        !ok,
        "rename must fail without source-dir write permission: {stderr}"
    );
    assert_eq!(
        fs::read(&dst).unwrap(),
        b"old payload",
        "failed forced move must preserve the existing destination"
    );
    assert_eq!(
        fs::read(&src).unwrap(),
        b"new payload",
        "failed forced move must preserve the source"
    );
}

#[test]
fn e2e_move_dry_run_leaves_everything_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.txt");
    let file_dst = dir.path().join("dst.txt");
    let src_dir = dir.path().join("tree");
    let dir_dst = dir.path().join("moved");
    fs::write(&src, b"payload").unwrap();
    fs::create_dir_all(src_dir.join("sub")).unwrap();
    fs::write(src_dir.join("sub").join("b.txt"), b"beta").unwrap();

    let (ok, stdout, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-n",
        src.to_str().unwrap(),
        file_dst.to_str().unwrap(),
    ]);
    assert!(ok, "dry-run file move should succeed: {}", stderr);
    assert!(
        stdout.contains("DRY RUN"),
        "dry-run should announce itself: {}",
        stdout
    );
    assert!(
        src.exists(),
        "dry-run must not remove the source: {}",
        stderr
    );
    assert!(
        !file_dst.exists(),
        "dry-run must not create the destination: {}",
        stderr
    );

    let (ok, _, stderr) = run_bcmr(&[
        "move",
        "-t",
        "-n",
        "-r",
        src_dir.to_str().unwrap(),
        dir_dst.to_str().unwrap(),
    ]);
    assert!(ok, "dry-run recursive move should succeed: {}", stderr);
    assert!(
        src_dir.join("sub").join("b.txt").exists(),
        "dry-run must leave the source tree intact: {}",
        stderr
    );
    assert!(
        !dir_dst.exists(),
        "dry-run must not create the destination tree: {}",
        stderr
    );
}

#[test]
fn e2e_move_preserves_content_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    let dst = dir.path().join("dst.bin");
    create_random_file(&src, 8 * 1024 * 1024);
    let src_hash = checksum::calculate_hash(&src).unwrap();

    let (ok, _, stderr) = run_bcmr(&["move", "-t", src.to_str().unwrap(), dst.to_str().unwrap()]);
    assert!(ok, "move should succeed: {}", stderr);
    assert!(
        !src.exists(),
        "source should be gone after move: {}",
        stderr
    );

    let dst_hash = checksum::calculate_hash(&dst).unwrap();
    assert_eq!(
        src_hash, dst_hash,
        "moved file must be byte-identical to the original"
    );
}
