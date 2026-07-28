use std::process::Command;
use std::time::{Duration, Instant};

fn bcmr() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bcmr"))
}

#[test]
fn json_copy_runs_in_the_foreground_and_emits_a_terminal_result() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("destination.txt");
    std::fs::write(&source, b"foreground-json").unwrap();

    let output = bcmr()
        .args(["--json", "copy"])
        .arg(&source)
        .arg(&destination)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"foreground-json");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let last: serde_json::Value =
        serde_json::from_str(stdout.lines().last().expect("terminal JSON line")).unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["status"], "success");
    assert!(last.get("job_id").is_none(), "--json must not submit a job");
}

#[test]
fn json_remove_without_explicit_consent_is_structured_and_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let victim = temp.path().join("keep.txt");
    std::fs::write(&victim, b"keep").unwrap();

    let output = bcmr()
        .args(["--json", "remove"])
        .arg(&victim)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(victim.exists(), "JSON output must never authorize deletion");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let last: serde_json::Value =
        serde_json::from_str(stdout.lines().last().expect("structured error line")).unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["status"], "error");
    assert_eq!(last["error_kind"], "confirmation_required");
}

#[test]
fn explicit_background_job_emits_submitted_and_terminal_events() {
    let temp = tempfile::tempdir().unwrap();
    let jobs = temp.path().join("jobs");
    let victim = temp.path().join("remove.txt");
    std::fs::write(&victim, b"remove").unwrap();

    let output = bcmr()
        .env("BCMR_JOBS_DIR", &jobs)
        .args(["--json", "--background", "remove", "--force", "--yes"])
        .arg(&victim)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let submitted: serde_json::Value = serde_json::from_slice(output.stdout.trim_ascii()).unwrap();
    assert_eq!(submitted["type"], "submitted");
    let log = std::path::PathBuf::from(submitted["log"].as_str().unwrap());

    let deadline = Instant::now() + Duration::from_secs(5);
    let contents = loop {
        let contents = std::fs::read_to_string(&log).unwrap_or_default();
        if contents.lines().any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|event| event["type"] == "result")
        }) {
            break contents;
        }
        assert!(
            Instant::now() < deadline,
            "background job did not emit a terminal event:\n{contents}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };

    assert!(!victim.exists());
    let first: serde_json::Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
    assert_eq!(first["type"], "submitted");
    let last: serde_json::Value = serde_json::from_str(contents.lines().last().unwrap()).unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["status"], "success");
}
