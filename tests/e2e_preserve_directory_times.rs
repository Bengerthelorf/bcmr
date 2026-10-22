#![cfg(unix)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::process::Command;

#[test]
fn recursive_preserve_keeps_subsecond_directory_mtimes() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    fs::create_dir_all(source.join("nested/empty")).unwrap();
    fs::write(source.join("nested/file"), b"payload").unwrap();
    let stamps = [
        ("", 1_700_000_000, 123_456_789),
        ("nested", 1_700_000_001, 987_654_321),
        ("nested/empty", 1_700_000_002, 234_567_891),
    ];
    for (relative, seconds, nanoseconds) in stamps {
        let time = filetime::FileTime::from_unix_time(seconds, nanoseconds);
        filetime::set_file_times(source.join(relative), time, time).unwrap();
    }
    let destination = temporary.path().join("destination");
    let output = Command::new(env!("CARGO_BIN_EXE_bcmr"))
        .args(["copy", "-rp", "--progress=off"])
        .arg(&source)
        .arg(&destination)
        .env("HOME", temporary.path())
        .env("XDG_CONFIG_HOME", temporary.path().join("config"))
        .env_remove("BCMR_CONFIG")
        .env_remove("BCMR_PROFILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (relative, _, _) in stamps {
        let expected = fs::metadata(source.join(relative)).unwrap();
        let actual = fs::metadata(destination.join(relative)).unwrap();
        assert_eq!(actual.mtime(), expected.mtime(), "{relative}");
        assert_eq!(actual.mtime_nsec(), expected.mtime_nsec(), "{relative}");
    }
}
