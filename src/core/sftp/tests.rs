use super::wire::{self, ExpectedReply, Reply};
use super::*;

// These fixtures are hand-encoded from draft-ietf-secsh-filexfer-02. In
// particular, the expected bytes do not use the encoder under test.
#[test]
fn decoder_reads_all_v3_attributes_and_skips_extended_pairs() {
    let packet = [
        105, 0, 0, 0, 7, 128, 0, 0, 15, // ATTRS, id, all flags + extended
        0, 0, 0, 1, 0, 0, 0, 2, // size = 2^32 + 2
        0, 0, 3, 232, 0, 0, 0, 20, // uid = 1000, gid = 20
        0, 0, 129, 164, // regular file, 0644
        0, 0, 0, 10, 0, 0, 0, 11, // atime, mtime
        0, 0, 0, 1, // one extension pair
        0, 0, 0, 1, b'x', 0, 0, 0, 1, b'y',
    ];
    let Reply::Attrs(attrs) = wire::decode_reply(&packet, 7, ExpectedReply::Attrs).unwrap() else {
        panic!("ATTRS reply required")
    };
    assert_eq!(attrs.size, Some(4_294_967_298));
    assert_eq!(attrs.uid, Some(1000));
    assert_eq!(attrs.gid, Some(20));
    assert_eq!(attrs.permissions, Some(0o100644));
    assert_eq!((attrs.atime, attrs.mtime), (Some(10), Some(11)));
    assert!(attrs.is_file());
    assert!(!attrs.is_dir());
    assert!(!attrs.is_symlink());
}

#[test]
fn encoder_preserves_attribute_pairs_and_rejects_partial_pairs() {
    let attrs = Attrs {
        size: Some(9),
        uid: Some(1000),
        gid: Some(20),
        permissions: Some(0o40755),
        atime: Some(10),
        mtime: Some(11),
    };
    let mut bytes = Vec::new();
    wire::encode_attrs(&attrs, &mut bytes).unwrap();
    assert_eq!(
        bytes,
        [
            0, 0, 0, 15, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 3, 232, 0, 0, 0, 20, 0, 0, 65, 237, 0, 0, 0,
            10, 0, 0, 0, 11,
        ]
    );
    assert!(attrs.is_dir());
    for attrs in [
        Attrs {
            uid: Some(1),
            ..Attrs::default()
        },
        Attrs {
            gid: Some(1),
            ..Attrs::default()
        },
        Attrs {
            atime: Some(1),
            ..Attrs::default()
        },
        Attrs {
            mtime: Some(1),
            ..Attrs::default()
        },
    ] {
        assert!(wire::encode_attrs(&attrs, &mut Vec::new()).is_err());
    }
}

#[test]
fn decoder_rejects_mismatched_ids_unknown_types_and_trailing_bytes() {
    let valid_attrs = [105, 0, 0, 0, 7, 0, 0, 0, 0];
    assert!(wire::decode_reply(&valid_attrs, 8, ExpectedReply::Attrs).is_err());
    assert!(wire::decode_reply(&valid_attrs, 7, ExpectedReply::Handle).is_err());
    assert!(wire::decode_reply(&[99, 0, 0, 0, 7], 7, ExpectedReply::Attrs).is_err());
    let mut trailing = valid_attrs.to_vec();
    trailing.push(0);
    assert!(wire::decode_reply(&trailing, 7, ExpectedReply::Attrs).is_err());
    assert!(wire::decode_reply(&[105, 0, 0, 0, 7, 0, 0, 0, 16], 7, ExpectedReply::Attrs,).is_err());
}

#[test]
fn decoder_rejects_truncated_fields_and_unbounded_counts() {
    let packets: &[&[u8]] = &[
        &[],
        &[105],
        &[105, 0, 0, 0, 7],
        &[105, 0, 0, 0, 7, 0, 0, 0, 1],         // missing size
        &[104, 0, 0, 0, 7, 255, 255, 255, 255], // impossible NAME count
        &[102, 0, 0, 0, 7, 255, 255, 255, 255], // impossible handle
        &[101, 0, 0, 0, 7, 0, 0, 0, 2],         // missing STATUS strings
        &[105, 0, 0, 0, 7, 128, 0, 0, 0, 255, 255, 255, 255],
    ];
    for packet in packets {
        assert!(wire::decode_reply(packet, 7, ExpectedReply::Attrs).is_err());
    }
    assert!(wire::decode_reply(
        &[104, 0, 0, 0, 7, 255, 255, 255, 255],
        7,
        ExpectedReply::Names,
    )
    .is_err());
}

#[test]
fn decoder_distinguishes_eof_missing_permission_and_unsupported_status() {
    for (code, expected, kind) in [
        (1, StatusCode::Eof, std::io::ErrorKind::UnexpectedEof),
        (2, StatusCode::NoSuchFile, std::io::ErrorKind::NotFound),
        (
            3,
            StatusCode::PermissionDenied,
            std::io::ErrorKind::PermissionDenied,
        ),
        (8, StatusCode::Unsupported, std::io::ErrorKind::Unsupported),
    ] {
        let packet = [101, 0, 0, 0, 7, 0, 0, 0, code, 0, 0, 0, 0, 0, 0, 0, 0];
        let Reply::Status(status) = wire::decode_reply(&packet, 7, ExpectedReply::Attrs).unwrap()
        else {
            panic!("STATUS reply required")
        };
        let error = status.into_error();
        assert_eq!(status_code(&error), Some(expected));
        assert!(matches!(&error, BcmrError::Io(error) if error.kind() == kind));
    }
}

#[test]
fn decoder_limits_data_to_requested_length_and_accepts_short_reads() {
    let short = [103, 0, 0, 0, 7, 0, 0, 0, 2, b'x', b'y'];
    let Reply::Data(data) =
        wire::decode_reply(&short, 7, ExpectedReply::Data { max_len: 10 }).unwrap()
    else {
        panic!("DATA reply required")
    };
    assert_eq!(data, b"xy");
    assert!(wire::decode_reply(&short, 7, ExpectedReply::Data { max_len: 1 }).is_err());
    let empty = [103, 0, 0, 0, 7, 0, 0, 0, 0];
    assert!(wire::decode_reply(&empty, 7, ExpectedReply::Data { max_len: 10 }).is_err());
    let mut handle = vec![102, 0, 0, 0, 7, 0, 0, 1, 1];
    handle.extend([0; 257]);
    assert!(wire::decode_reply(&handle, 7, ExpectedReply::Handle).is_err());
}

#[test]
fn name_decoder_keeps_raw_bytes_and_does_not_parse_longname() {
    let packet = [
        104, 0, 0, 0, 7, 0, 0, 0, 1, // NAME, id, one entry
        0, 0, 0, 2, 255, b'\n', // a non-UTF8 raw component
        0, 0, 0, 4, b'f', b'a', b'k', b'e', // longname has no semantic use
        0, 0, 0, 4, 0, 0, 161, 255, // symlink attrs
    ];
    let Reply::Names(names) = wire::decode_reply(&packet, 7, ExpectedReply::Names).unwrap() else {
        panic!("NAME reply required")
    };
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].0, [255, b'\n']);
    assert!(names[0].1.is_symlink());
}

#[test]
fn directory_components_cannot_escape_the_listed_directory() {
    for name in [b"".as_slice(), b"/absolute", b"one/two", b"nul\0byte"] {
        assert!(wire::validate_child_name(name).is_err(), "{name:?}");
    }
    for name in [b".".as_slice(), b"..", b" leading\ntrailing ", &[255, 254]] {
        assert!(wire::validate_child_name(name).is_ok(), "{name:?}");
    }
}

#[test]
fn extensions_require_the_advertised_supported_version() {
    let version = [
        2, 0, 0, 0, 3, 0, 0, 0, 24, b'p', b'o', b's', b'i', b'x', b'-', b'r', b'e', b'n', b'a',
        b'm', b'e', b'@', b'o', b'p', b'e', b'n', b's', b's', b'h', b'.', b'c', b'o', b'm', 0, 0,
        0, 1, b'1', 0, 0, 0, 17, b'f', b's', b'y', b'n', b'c', b'@', b'o', b'p', b'e', b'n', b's',
        b's', b'h', b'.', b'c', b'o', b'm', 0, 0, 0, 1, b'2',
    ];
    let caps = wire::decode_version(&version).unwrap();
    assert!(caps.posix_rename);
    assert!(!caps.fsync);
    assert!(wire::decode_version(&[2, 0, 0, 0, 4]).is_err());
    assert!(wire::decode_version(&version[..version.len() - 1]).is_err());
    let hardlink = [
        2, 0, 0, 0, 3, 0, 0, 0, 20, b'h', b'a', b'r', b'd', b'l', b'i', b'n', b'k', b'@', b'o',
        b'p', b'e', b'n', b's', b's', b'h', b'.', b'c', b'o', b'm', 0, 0, 0, 1, b'1',
    ];
    assert!(wire::decode_version(&hardlink).unwrap().hardlink);
}

#[cfg(unix)]
#[tokio::test]
async fn native_hardlink_publication_never_replaces_a_dangling_symlink() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("source"), b"kept").unwrap();
    std::os::unix::fs::symlink("missing", temp.path().join("existing")).unwrap();
    let Some(mut client) = native_client(temp.path(), false).await else {
        return;
    };
    assert!(client.is_usable());
    if !client.supports_hardlink() {
        return;
    }
    assert!(client.hardlink("source", "existing").await.is_err());
    assert_eq!(
        std::fs::read_link(temp.path().join("existing")).unwrap(),
        std::path::Path::new("missing")
    );
    assert_eq!(std::fs::read(temp.path().join("source")).unwrap(), b"kept");
    client.hardlink("source", "new").await.unwrap();
    assert_eq!(std::fs::read(temp.path().join("new")).unwrap(), b"kept");
}

#[tokio::test]
async fn framing_rejects_oversized_and_truncated_packets_before_allocating_body() {
    let mut oversized = &b"\x7f\xff\xff\xff"[..];
    assert!(wire::read_frame(&mut oversized).await.is_err());
    let mut zero = &b"\0\0\0\0"[..];
    assert!(wire::read_frame(&mut zero).await.is_err());
    let mut truncated = &b"\0\0\0\x05\x02\0"[..];
    assert!(wire::read_frame(&mut truncated).await.is_err());
    let mut valid = &b"\0\0\0\x05\x02\0\0\0\x03"[..];
    assert_eq!(wire::read_frame(&mut valid).await.unwrap(), [2, 0, 0, 0, 3]);
}

#[test]
fn subsystem_arguments_keep_remote_paths_out_of_the_ssh_command() {
    let args = ssh_args_for("example.invalid", false).unwrap();
    assert_eq!(&args[args.len() - 3..], ["-s", "example.invalid", "sftp"]);
    for option in [
        "ControlMaster=no",
        "ControlPath=none",
        "ConnectTimeout=10",
        "BatchMode=yes",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=20",
    ] {
        assert!(args.windows(2).any(|pair| pair == ["-o", option]));
    }
    for target in ["-oProxyCommand=bad", "bad\0host", "bad\nhost", ""] {
        assert!(ssh_args_for(target, false).is_err());
    }
}

#[test]
fn interactive_subsystem_leaves_authentication_prompts_to_native_ssh() {
    let args = ssh_args_for("example.invalid", true).unwrap();
    assert!(!args.iter().any(|arg| arg.starts_with("BatchMode=")));
    assert_eq!(&args[args.len() - 3..], ["-s", "example.invalid", "sftp"]);
    assert!(args
        .windows(2)
        .any(|pair| pair == ["-o", "ConnectTimeout=10"]));
    assert!(args
        .windows(2)
        .any(|pair| pair == ["-o", "ServerAliveInterval=15"]));
}

#[test]
fn noninteractive_subsystem_prevents_unattended_authentication_prompts() {
    let args = ssh_args_for("example.invalid", false).unwrap();
    assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
}

#[cfg(unix)]
fn sftp_server() -> Option<std::path::PathBuf> {
    [
        "/usr/libexec/sftp-server",
        "/usr/lib/openssh/sftp-server",
        "/usr/lib/ssh/sftp-server",
        "/opt/homebrew/libexec/sftp-server",
    ]
    .into_iter()
    .map(std::path::PathBuf::from)
    .chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|path| path.join("sftp-server")),
    )
    .find(|path| path.is_file())
}

#[cfg(unix)]
async fn native_client(root: &std::path::Path, read_only: bool) -> Option<SftpClient> {
    let Some(server) = sftp_server() else {
        eprintln!("local OpenSSH sftp-server unavailable; skipping native SFTP test");
        return None;
    };
    let mut command = tokio::process::Command::new(server);
    command.arg("-d").arg(root);
    if read_only {
        command.arg("-R");
    }
    let child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    Some(SftpClient::from_child(child).await.unwrap())
}

#[cfg(unix)]
#[tokio::test]
async fn native_sftp_roundtrip_preserves_names_links_attributes_and_no_clobber() {
    use std::os::unix::ffi::OsStringExt;
    let temp = tempfile::tempdir().unwrap();
    let Some(mut client) = native_client(temp.path(), false).await else {
        return;
    };
    assert_eq!(
        client.realpath(".").await.unwrap(),
        temp.path()
            .canonicalize()
            .unwrap()
            .as_os_str()
            .as_encoded_bytes()
    );
    client
        .mkdir(
            "nested",
            &Attrs {
                permissions: Some(0o700),
                ..Attrs::default()
            },
        )
        .await
        .unwrap();
    let file = "nested/雪 'quoted'\nfile";
    let handle = client
        .open(
            file,
            flags::READ | flags::WRITE | flags::CREATE | flags::EXCLUSIVE,
            &Attrs {
                permissions: Some(0o600),
                ..Attrs::default()
            },
        )
        .await
        .unwrap();
    client
        .write(&handle, 0, b"\0first\xffsecond")
        .await
        .unwrap();
    client.write(&handle, 20, b"tail").await.unwrap();
    assert_eq!(client.read(&handle, 0, 6).await.unwrap(), b"\0first");
    assert_eq!(client.read(&handle, 20, 99).await.unwrap(), b"tail");
    assert!(client.read(&handle, 24, 99).await.unwrap().is_empty());
    if client.supports_fsync() {
        client.fsync(&handle).await.unwrap();
    }
    client.close(&handle).await.unwrap();
    assert!(
        client.read(&handle, 0, 1).await.is_err(),
        "closed handle must not be sent"
    );
    let attrs = client.stat(file, true).await.unwrap();
    assert_eq!(attrs.size, Some(24));
    assert!(attrs.is_file());
    client
        .symlink("雪 'quoted'\nfile".as_bytes(), "nested/link")
        .await
        .unwrap();
    assert!(client
        .stat("nested/link", false)
        .await
        .unwrap()
        .is_symlink());
    assert_eq!(
        client.stat("nested/link", true).await.unwrap().size,
        Some(24)
    );
    assert_eq!(
        client.readlink("nested/link").await.unwrap(),
        "雪 'quoted'\nfile".as_bytes()
    );
    let raw = std::ffi::OsString::from_vec(vec![255, b'\n']);
    let raw_created = match std::fs::write(temp.path().join("nested").join(raw), b"raw") {
        Ok(()) => true,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EILSEQ) | Some(libc::EINVAL)
            ) =>
        {
            // APFS rejects filenames that are not valid UTF-8. Keep all other
            // native assertions; raw-byte decoding is tested independently.
            false
        }
        Err(error) => panic!("creating the native raw-name fixture: {error}"),
    };
    let names = client.readdir("nested").await.unwrap();
    assert_eq!(names.len(), if raw_created { 3 } else { 2 });
    if raw_created {
        assert!(names
            .iter()
            .any(|(name, attrs)| name == &[255, b'\n'] && attrs.is_file()));
    }
    assert!(names
        .iter()
        .any(|(name, attrs)| name == b"link" && attrs.is_symlink()));
    client
        .setstat(
            file,
            &Attrs {
                permissions: Some(0o640),
                atime: Some(1_700_000_000),
                mtime: Some(1_700_000_001),
                ..Attrs::default()
            },
        )
        .await
        .unwrap();
    let attrs = client.stat(file, false).await.unwrap();
    assert_eq!(attrs.permissions.unwrap() & 0o7777, 0o640);
    assert_eq!(attrs.mtime, Some(1_700_000_001));
    std::fs::write(temp.path().join("existing"), b"keep").unwrap();
    assert!(client.rename(file, "existing", false).await.is_err());
    assert_eq!(
        std::fs::read(temp.path().join("existing")).unwrap(),
        b"keep"
    );
    assert!(client.stat(file, true).await.is_ok());
    if client.supports_posix_rename() {
        client.rename(file, "existing", true).await.unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("existing")).unwrap().len(),
            24
        );
    }
    let missing = client.stat("missing", true).await.unwrap_err();
    assert_eq!(status_code(&missing), Some(StatusCode::NoSuchFile));
    client.remove("nested/link").await.unwrap();
    client.remove("existing").await.unwrap();
    // Raw non-UTF8 names stay available to the caller; this test removes one locally.
    if raw_created {
        std::fs::remove_file(
            temp.path()
                .join("nested")
                .join(std::ffi::OsString::from_vec(vec![255, b'\n'])),
        )
        .unwrap();
    }
    client.rmdir("nested").await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn native_read_only_subsystem_reports_permission_denied_and_keeps_connection_usable() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("file"), b"keep").unwrap();
    let Some(mut client) = native_client(temp.path(), true).await else {
        return;
    };
    let error = client.remove("file").await.unwrap_err();
    assert_eq!(status_code(&error), Some(StatusCode::PermissionDenied));
    assert_eq!(client.stat("file", true).await.unwrap().size, Some(4));
}

#[cfg(unix)]
#[tokio::test]
async fn native_fsetstat_changes_the_open_object_after_its_path_is_replaced() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("working"), b"original").unwrap();
    let Some(mut client) = native_client(temp.path(), false).await else {
        return;
    };
    let handle = client
        .open("working", flags::READ, &Attrs::default())
        .await
        .unwrap();
    std::fs::rename(temp.path().join("working"), temp.path().join("held")).unwrap();
    std::fs::write(temp.path().join("working"), b"unrelated replacement").unwrap();
    let replacement = std::fs::metadata(temp.path().join("working")).unwrap();
    client
        .fsetstat(
            &handle,
            &Attrs {
                permissions: Some(0o640),
                atime: Some(1_500_000_001),
                mtime: Some(1_500_000_002),
                ..Attrs::default()
            },
        )
        .await
        .unwrap();
    client.close(&handle).await.unwrap();
    use std::os::unix::fs::MetadataExt;
    let held = std::fs::metadata(temp.path().join("held")).unwrap();
    assert_eq!(held.mode() & 0o777, 0o640);
    assert_eq!((held.atime(), held.mtime()), (1_500_000_001, 1_500_000_002));
    let untouched = std::fs::metadata(temp.path().join("working")).unwrap();
    assert_eq!(
        (untouched.mode(), untouched.mtime(), untouched.mtime_nsec()),
        (
            replacement.mode(),
            replacement.mtime(),
            replacement.mtime_nsec()
        )
    );
    assert_eq!(
        std::fs::read(temp.path().join("working")).unwrap(),
        b"unrelated replacement"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_blocked_request_closes_stdin_kills_child_and_poisons_the_session() {
    let temp = tempfile::tempdir().unwrap();
    let fifo = temp.path().join("blocked");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let Some(mut client) = native_client(temp.path(), false).await else {
        return;
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(30),
        client.open("blocked", flags::READ, &Attrs::default()),
    )
    .await;
    assert!(
        result.is_err(),
        "opening an unread FIFO must remain blocked"
    );
    assert!(client.poisoned);
    assert!(client.stdin.is_none());
    assert!(!client.is_usable());
    assert!(client.stat(".", true).await.is_err());
    tokio::time::timeout(std::time::Duration::from_secs(2), client.child.wait())
        .await
        .expect("cancelled subsystem child must exit promptly")
        .unwrap();
}
