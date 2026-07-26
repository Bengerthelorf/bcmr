#![cfg(unix)]

mod common;

#[test]
fn config_fail_fast_on_malformed_toml() {
    let dir = tempfile::tempdir().unwrap();
    let bad_config = dir.path().join("bad.toml");
    std::fs::write(&bad_config, b"not = valid = toml = !@#$\n[\nunterminated").unwrap();

    let output = std::process::Command::new(common::bcmr_bin())
        .args(["copy", "@bogus_alias_to_force_config_read", "/tmp/x"])
        .env("BCMR_CONFIG", &bad_config)
        .output()
        .unwrap();

    assert!(!output.status.success(), "must fail on malformed config");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code must be 2 on parse error; got {:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to parse configuration"),
        "stderr must explain the failure; got: {stderr}"
    );
}

#[test]
fn config_absent_file_loads_defaults_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does_not_exist.toml");

    let output = std::process::Command::new(common::bcmr_bin())
        .args(["copy", "--help"])
        .env("BCMR_CONFIG", &missing)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "absent config must not be a parse error; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn explicit_config_rejects_zero_parallel_transfers_before_copy_starts() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("zero-parallel.toml");
    std::fs::write(&config, "[scp]\nparallel_transfers = 0\n").unwrap();

    let output = std::process::Command::new(common::bcmr_bin())
        .args([
            "--config",
            config.to_str().unwrap(),
            "copy",
            "src",
            "host:dst",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success(), "zero parallel config must fail");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("parallel_transfers"),
        "error must identify invalid concurrency: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
