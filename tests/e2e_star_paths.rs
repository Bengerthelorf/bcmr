use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bcmr"))
        .current_dir(root)
        .args(args)
        .args(["--progress", "off"])
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn quoted_star_sources_copy_matching_files_without_hidden_names() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("out")).unwrap();
    for name in ["report甲.txt", "report乙.txt", "other.bin", ".hidden.txt"] {
        fs::write(root.join(name), name.as_bytes()).unwrap();
    }
    let output = run(root, &["copy", "*.txt", "out"]);
    assert_success(&output);
    assert_eq!(
        fs::read(root.join("out/report甲.txt")).unwrap(),
        "report甲.txt".as_bytes()
    );
    assert_eq!(
        fs::read(root.join("out/report乙.txt")).unwrap(),
        "report乙.txt".as_bytes()
    );
    assert!(!root.join("out/.hidden.txt").exists());
    assert!(!root.join("out/other.bin").exists());
}

#[test]
fn stars_expand_per_component_and_keep_brackets_literal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("out")).unwrap();
    fs::create_dir(root.join("batch1")).unwrap();
    fs::create_dir(root.join("batch2")).unwrap();
    fs::write(root.join("batch1/file[1].txt"), b"brackets").unwrap();
    fs::write(root.join("batch2/other.txt"), b"not selected").unwrap();
    let output = run(root, &["copy", "batch*/file[1]*", "out"]);
    assert_success(&output);
    assert_eq!(fs::read(root.join("out/file[1].txt")).unwrap(), b"brackets");
    assert!(!root.join("out/other.txt").exists());
}

#[test]
fn an_unmatched_pattern_fails_before_any_source_is_copied() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("one.txt"), b"preserved").unwrap();
    fs::create_dir(root.join("out")).unwrap();
    let output = run(root, &["copy", "one.txt", "absent*", "out"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No matches"));
    assert_eq!(fs::read_dir(root.join("out")).unwrap().count(), 0);
}

#[test]
fn star_fanout_move_copies_to_all_targets_then_removes_matched_sources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for target in ["out1", "out2"] {
        fs::create_dir(root.join(target)).unwrap();
    }
    fs::write(root.join("source1.txt"), b"one").unwrap();
    fs::write(root.join("source2.txt"), b"two").unwrap();
    fs::write(root.join("keep.bin"), b"keep").unwrap();
    let output = run(root, &["move", "source*", "--to", "out1", "--to", "out2"]);
    assert_success(&output);
    for target in ["out1", "out2"] {
        assert_eq!(
            fs::read(root.join(target).join("source1.txt")).unwrap(),
            b"one"
        );
        assert_eq!(
            fs::read(root.join(target).join("source2.txt")).unwrap(),
            b"two"
        );
    }
    assert!(!root.join("source1.txt").exists());
    assert!(!root.join("source2.txt").exists());
    assert!(root.join("keep.bin").exists());
}

#[cfg(unix)]
#[test]
fn exact_existing_star_name_and_destination_remain_literal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("literal*.txt"), b"literal").unwrap();
    fs::write(root.join("literalOTHER.txt"), b"do not copy").unwrap();
    fs::create_dir(root.join("out*")).unwrap();
    let output = run(root, &["copy", "literal*.txt", "out*"]);
    assert_success(&output);
    assert_eq!(
        fs::read(root.join("out*/literal*.txt")).unwrap(),
        b"literal"
    );
    assert_eq!(fs::read_dir(root.join("out*")).unwrap().count(), 1);
}
