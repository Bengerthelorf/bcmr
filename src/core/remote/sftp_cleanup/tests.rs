use super::*;

fn metadata(kind: Kind) -> Metadata {
    Metadata {
        kind,
        size: 8,
        permissions: match kind {
            Kind::File => 0o100600,
            Kind::Directory => 0o40700,
            Kind::Symlink => 0o120777,
        },
        uid: 1000,
        gid: 20,
        mtime: 1_700_000_000,
    }
}

#[test]
fn file_guard_compares_content_even_if_size_and_mtime_are_restored() {
    let expected = Observation {
        metadata: metadata(Kind::File),
        content: Content::File(blake3::hash(b"original")),
    };
    let actual = Observation {
        metadata: metadata(Kind::File),
        content: Content::File(blake3::hash(b"modified")),
    };
    assert!(!expected.matches(&actual));
}

#[test]
fn directory_guard_preserves_new_entries_and_still_checks_owner_and_mode() {
    let expected = Observation {
        metadata: metadata(Kind::Directory),
        content: Content::Directory,
    };
    let mut actual = expected.clone();
    actual.metadata.size += 100;
    actual.metadata.mtime += 10;
    assert!(expected.matches(&actual));
    actual.metadata.uid += 1;
    assert!(!expected.matches(&actual));
    actual.metadata = metadata(Kind::Directory);
    actual.metadata.permissions = 0o40755;
    assert!(!expected.matches(&actual));
}

#[test]
fn link_guard_compares_raw_target_without_following_it() {
    let expected = Observation {
        metadata: metadata(Kind::Symlink),
        content: Content::Symlink(b"a/../b".to_vec()),
    };
    let actual = Observation {
        metadata: metadata(Kind::Symlink),
        content: Content::Symlink(b"b".to_vec()),
    };
    assert!(!expected.matches(&actual));
}

#[test]
fn cleanup_requires_named_paths_and_complete_v3_fingerprints() {
    for path in ["", ".", "./", "/", "dir/.", "dir/.."] {
        assert!(parent_and_name(path).is_err(), "{path:?}");
    }
    assert_eq!(parent_and_name("leaf").unwrap(), (".", "leaf"));
    assert_eq!(parent_and_name("/leaf").unwrap(), ("/", "leaf"));
    assert_eq!(
        parent_and_name("dir/雪\nname").unwrap(),
        ("dir", "雪\nname")
    );
    assert!(Metadata::from_attrs(&Attrs::default()).is_err());
    assert!(
        Metadata::from_attrs(&Attrs {
            permissions: Some(0o100600),
            size: Some(8),
            mtime: Some(1_700_000_000),
            ..Attrs::default()
        })
        .is_err(),
        "omitted owner fields cannot weaken a cleanup guard"
    );
}

#[cfg(unix)]
async fn native(root: &std::path::Path) -> Option<SftpClient> {
    let server = [
        "/usr/libexec/sftp-server",
        "/usr/lib/openssh/sftp-server",
        "/usr/lib/ssh/sftp-server",
    ]
    .into_iter()
    .map(std::path::PathBuf::from)
    .find(|path| path.is_file())?;
    let child = tokio::process::Command::new(server)
        .arg("-d")
        .arg(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    Some(SftpClient::from_child(child).await.unwrap())
}

#[cfg(unix)]
fn remote(path: &str) -> RemotePath {
    RemotePath {
        user: None,
        host: "unused.invalid".into(),
        port: None,
        path: path.into(),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn native_content_change_with_restored_mtime_prevents_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("file");
    std::fs::write(&file, b"original").unwrap();
    let Some(mut client) = native(temp.path()).await else {
        return;
    };
    let snapshot = Snapshot::capture(&mut client, &remote("file"))
        .await
        .unwrap();
    let stamp = std::fs::metadata(&file).unwrap();
    std::fs::write(&file, b"modified").unwrap();
    filetime::set_file_mtime(
        &file,
        filetime::FileTime::from_last_modification_time(&stamp),
    )
    .unwrap();
    assert!(validate_entry(&mut client, &snapshot).await.is_err());
    assert_eq!(std::fs::read(file).unwrap(), b"modified");
}

#[cfg(unix)]
#[tokio::test]
async fn native_cleanup_removes_selected_file_and_link_without_following_the_link() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("file"), b"original").unwrap();
    std::fs::write(temp.path().join("keep"), b"kept").unwrap();
    std::os::unix::fs::symlink("keep", temp.path().join("link")).unwrap();
    let Some(mut client) = native(temp.path()).await else {
        return;
    };
    let file = Snapshot::capture(&mut client, &remote("file"))
        .await
        .unwrap();
    let link = Snapshot::capture(&mut client, &remote("link"))
        .await
        .unwrap();
    remove_leaf(&mut client, &file).await.unwrap();
    remove_leaf(&mut client, &link).await.unwrap();
    assert!(!temp.path().join("file").exists());
    assert!(temp.path().join("link").symlink_metadata().is_err());
    assert_eq!(std::fs::read(temp.path().join("keep")).unwrap(), b"kept");
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn native_quarantine_change_is_restored_with_no_replace() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("file"), b"original").unwrap();
    let Some(mut client) = native(temp.path()).await else {
        return;
    };
    let snapshot = Snapshot::capture(&mut client, &remote("file"))
        .await
        .unwrap();
    let quarantine = Quarantine::create(&mut client, &snapshot).await.unwrap();
    quarantine
        .rename_source(&mut client, &snapshot)
        .await
        .unwrap();
    std::fs::write(&quarantine.payload.path, b"uncopied replacement").unwrap();
    let error = finish_leaf(&mut client, &snapshot, &quarantine)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("restored"));
    assert_eq!(
        std::fs::read(temp.path().join("file")).unwrap(),
        b"uncopied replacement"
    );
    assert!(!std::path::Path::new(&quarantine.directory.path).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn native_restore_does_not_overwrite_a_concurrent_source_name() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("file"), b"original").unwrap();
    let Some(mut client) = native(temp.path()).await else {
        return;
    };
    let snapshot = Snapshot::capture(&mut client, &remote("file"))
        .await
        .unwrap();
    let quarantine = Quarantine::create(&mut client, &snapshot).await.unwrap();
    quarantine
        .rename_source(&mut client, &snapshot)
        .await
        .unwrap();
    std::fs::write(&quarantine.payload.path, b"uncopied replacement").unwrap();
    std::fs::write(temp.path().join("file"), b"new source name").unwrap();
    let error = finish_leaf(&mut client, &snapshot, &quarantine)
        .await
        .unwrap_err();
    assert!(error.to_string().contains(&quarantine.payload.path));
    assert_eq!(
        std::fs::read(temp.path().join("file")).unwrap(),
        b"new source name"
    );
    assert_eq!(
        std::fs::read(&quarantine.payload.path).unwrap(),
        b"uncopied replacement"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_changed_link_is_retained_without_overwriting_a_dangling_source_link() {
    let temp = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("original-target", temp.path().join("link")).unwrap();
    let Some(mut client) = native(temp.path()).await else {
        return;
    };
    let snapshot = Snapshot::capture(&mut client, &remote("link"))
        .await
        .unwrap();
    let quarantine = Quarantine::create(&mut client, &snapshot).await.unwrap();
    quarantine
        .rename_source(&mut client, &snapshot)
        .await
        .unwrap();
    std::fs::remove_file(&quarantine.payload.path).unwrap();
    std::os::unix::fs::symlink("uncopied-target", &quarantine.payload.path).unwrap();
    std::os::unix::fs::symlink("new-name-target", temp.path().join("link")).unwrap();
    let error = finish_leaf(&mut client, &snapshot, &quarantine)
        .await
        .unwrap_err();
    assert!(error.to_string().contains(&quarantine.payload.path));
    assert_eq!(
        std::fs::read_link(temp.path().join("link")).unwrap(),
        std::path::Path::new("new-name-target")
    );
    assert_eq!(
        std::fs::read_link(&quarantine.payload.path).unwrap(),
        std::path::Path::new("uncopied-target")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_changed_link_target_can_be_restored_with_atomic_symlink_creation() {
    let temp = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("original-target", temp.path().join("link")).unwrap();
    let Some(mut client) = native(temp.path()).await else {
        return;
    };
    let snapshot = Snapshot::capture(&mut client, &remote("link"))
        .await
        .unwrap();
    let quarantine = Quarantine::create(&mut client, &snapshot).await.unwrap();
    quarantine
        .rename_source(&mut client, &snapshot)
        .await
        .unwrap();
    std::fs::remove_file(&quarantine.payload.path).unwrap();
    std::os::unix::fs::symlink("uncopied-target", &quarantine.payload.path).unwrap();
    let error = finish_leaf(&mut client, &snapshot, &quarantine)
        .await
        .unwrap_err();
    assert_eq!(
        std::fs::read_link(temp.path().join("link")).unwrap(),
        std::path::Path::new("uncopied-target")
    );
    if std::path::Path::new(&quarantine.payload.path)
        .symlink_metadata()
        .is_ok()
    {
        // A clock tick or different inherited group can prevent exact metadata
        // restoration. The original link then remains as explicit recovery data.
        assert!(error.to_string().contains(&quarantine.payload.path));
    } else {
        assert!(error.to_string().contains("restored"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn native_directory_cleanup_preserves_a_late_file() {
    let temp = tempfile::tempdir().unwrap();
    let tree = temp.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    let Some(mut client) = native(temp.path()).await else {
        return;
    };
    let snapshot = Snapshot::capture(&mut client, &remote("tree"))
        .await
        .unwrap();
    std::fs::write(tree.join("late"), b"not selected").unwrap();
    validate_entry(&mut client, &snapshot).await.unwrap();
    remove_directory(&mut client, &snapshot).await.unwrap();
    assert_eq!(std::fs::read(tree.join("late")).unwrap(), b"not selected");
}

#[cfg(unix)]
#[tokio::test]
async fn native_parent_alias_retargeting_preserves_both_source_names() {
    let temp = tempfile::tempdir().unwrap();
    for name in ["first", "second"] {
        std::fs::create_dir(temp.path().join(name)).unwrap();
        std::fs::write(temp.path().join(name).join("file"), name).unwrap();
    }
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink("first", &alias).unwrap();
    let Some(mut client) = native(temp.path()).await else {
        return;
    };
    let snapshot = Snapshot::capture(&mut client, &remote("alias/file"))
        .await
        .unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink("second", &alias).unwrap();
    assert!(remove_leaf(&mut client, &snapshot).await.is_err());
    assert_eq!(
        std::fs::read(temp.path().join("first/file")).unwrap(),
        b"first"
    );
    assert_eq!(
        std::fs::read(temp.path().join("second/file")).unwrap(),
        b"second"
    );
}
