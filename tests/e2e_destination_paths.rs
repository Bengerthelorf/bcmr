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
fn file_copy_distinguishes_missing_directory_file_and_rename_targets() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("report.txt"), b"report bytes").unwrap();
    assert_success(&run(root, &["copy", "report.txt", "parent/new/"]));
    assert_eq!(
        fs::read(root.join("parent/new/report.txt")).unwrap(),
        b"report bytes"
    );
    assert_success(&run(root, &["copy", "report.txt", "nested/renamed.txt"]));
    assert_eq!(
        fs::read(root.join("nested/renamed.txt")).unwrap(),
        b"report bytes"
    );
    assert_success(&run(root, &["copy", "report.txt", "bare-name"]));
    assert_eq!(fs::read(root.join("bare-name")).unwrap(), b"report bytes");
    assert!(root.join("report.txt").is_file());
}

#[test]
fn move_creates_missing_file_parents_and_explicit_directory_parents() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("one.txt"), b"one").unwrap();
    assert_success(&run(root, &["move", "one.txt", "new/parent/"]));
    assert_eq!(fs::read(root.join("new/parent/one.txt")).unwrap(), b"one");
    assert!(!root.join("one.txt").exists());
    fs::write(root.join("two.txt"), b"two").unwrap();
    assert_success(&run(root, &["move", "two.txt", "file/parent/renamed"]));
    assert_eq!(fs::read(root.join("file/parent/renamed")).unwrap(), b"two");
    assert!(!root.join("two.txt").exists());
    fs::create_dir(root.join("tree")).unwrap();
    fs::write(root.join("tree/leaf.txt"), b"leaf").unwrap();
    assert_success(&run(root, &["move", "-r", "tree", "new-tree-parent/"]));
    assert_eq!(
        fs::read(root.join("new-tree-parent/tree/leaf.txt")).unwrap(),
        b"leaf"
    );
    assert!(!root.join("tree").exists());
}

#[test]
fn multiple_sources_require_existing_or_explicit_directory_targets() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for operation in ["copy", "move"] {
        let source = root.join(operation);
        fs::create_dir(&source).unwrap();
        fs::write(source.join("one.txt"), b"one").unwrap();
        fs::write(source.join("two.txt"), b"two").unwrap();
        let rejected = run(&source, &[operation, "one.txt", "two.txt", "ambiguous"]);
        assert!(!rejected.status.success());
        assert!(!source.join("ambiguous").exists());
        assert_eq!(fs::read(source.join("one.txt")).unwrap(), b"one");
        assert_success(&run(&source, &[operation, "one.txt", "two.txt", "new/"]));
        assert_eq!(fs::read(source.join("new/one.txt")).unwrap(), b"one");
        assert_eq!(fs::read(source.join("new/two.txt")).unwrap(), b"two");
        assert_eq!(source.join("one.txt").exists(), operation == "copy");
        assert_eq!(source.join("two.txt").exists(), operation == "copy");
    }
}

#[test]
fn dry_runs_predict_file_and_directory_targets_without_creating_any_parent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("report.txt"), b"report").unwrap();
    fs::create_dir(root.join("tree")).unwrap();
    fs::write(root.join("tree/leaf.txt"), b"leaf").unwrap();
    for operation in ["copy", "move"] {
        let file = run(
            root,
            &[operation, "--dry-run", "report.txt", "uncreated/new/"],
        );
        assert_success(&file);
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&file.stdout),
            String::from_utf8_lossy(&file.stderr)
        );
        assert!(
            output.contains(
                &Path::new("uncreated/new/")
                    .join("report.txt")
                    .display()
                    .to_string()
            ),
            "{output}"
        );
        let tree = run(
            root,
            &[operation, "-r", "--dry-run", "tree", "uncreated/new/"],
        );
        assert_success(&tree);
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&tree.stdout),
            String::from_utf8_lossy(&tree.stderr)
        );
        assert!(
            output.contains(
                &Path::new("uncreated/new/")
                    .join("tree")
                    .join("leaf.txt")
                    .display()
                    .to_string()
            ),
            "{output}"
        );
        assert!(!root.join("uncreated").exists());
        assert_eq!(fs::read(root.join("report.txt")).unwrap(), b"report");
        assert_eq!(fs::read(root.join("tree/leaf.txt")).unwrap(), b"leaf");
    }
}

#[test]
fn existing_file_with_trailing_slash_is_never_replaced_or_removed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("source.txt"), b"source").unwrap();
    fs::write(root.join("blocker"), b"existing").unwrap();
    let mut destinations = vec!["blocker/"];
    if cfg!(windows) {
        destinations.push(r"blocker\");
    }
    for destination in destinations {
        for operation in ["copy", "move"] {
            for dry_run in [false, true] {
                let mut args = vec![operation, "-f", "-y", "source.txt", destination];
                if dry_run {
                    args.push("--dry-run");
                }
                let output = run(root, &args);
                assert!(
                    !output.status.success(),
                    "{args:?} accepted a file as a directory: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(fs::read(root.join("blocker")).unwrap(), b"existing");
                assert_eq!(fs::read(root.join("source.txt")).unwrap(), b"source");
            }
        }
    }
}
