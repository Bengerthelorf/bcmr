//! Bind remote-source move cleanup to the destinations actually delivered.
//! A destination that aliases the quarantined source disappears before unlink.
//! This works without assuming that SSH host keys prove filesystem separation.
use super::{backend, RemotePath};
use crate::core::error::BcmrError;
use crate::core::sftp::{flags, Attrs, SftpClient};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tokio::io::AsyncReadExt;

#[derive(Clone)]
pub(crate) enum Target {
    Local(PathBuf),
    Remote(RemotePath),
}
#[derive(Clone)]
pub(crate) enum Content {
    File(String),
    Link(Vec<u8>),
}
type Deliveries = HashMap<(String, String), Vec<(Target, Content, bool)>>;
static DELIVERIES: OnceLock<Mutex<Deliveries>> = OnceLock::new();
type LocalDeliveries = HashMap<PathBuf, Vec<(Target, Content, bool)>>;
static LOCAL_DELIVERIES: OnceLock<Mutex<LocalDeliveries>> = OnceLock::new();

pub(crate) fn remember_local(
    source: &std::path::Path,
    target: Target,
    content: Content,
    preserve_atime: bool,
) -> Result<(), BcmrError> {
    LOCAL_DELIVERIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(std::path::absolute(source)?)
        .or_default()
        .push((target, content, preserve_atime));
    Ok(())
}

pub(crate) fn has_local(source: &std::path::Path) -> bool {
    std::path::absolute(source).ok().is_some_and(|path| {
        LOCAL_DELIVERIES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&path)
    })
}

pub(crate) async fn validate_local(source: &std::path::Path) -> Result<(), BcmrError> {
    let records = LOCAL_DELIVERIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&std::path::absolute(source)?)
        .cloned()
        .unwrap_or_default();
    for (target, expected, preserve_atime) in records {
        crate::core::interrupt::check()?;
        let valid = match target {
            Target::Local(path) => local_content(&path, &expected, preserve_atime).await?,
            Target::Remote(remote) => remote_content(&remote, &expected, preserve_atime).await?,
        };
        if !valid {
            return Err(backend::invalid("published move destination changed or aliases the quarantined local source; source cleanup refused"));
        }
    }
    Ok(())
}

pub(crate) fn remember(
    source: &RemotePath,
    target: Target,
    content: Content,
    preserve_atime: bool,
) {
    DELIVERIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry((
            source.ssh_target(),
            source.path.trim_end_matches('/').to_owned(),
        ))
        .or_default()
        .push((target, content, preserve_atime));
}

fn witnesses(source: &RemotePath) -> Vec<(Target, Content, bool)> {
    DELIVERIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(
            source.ssh_target(),
            source.path.trim_end_matches('/').to_owned(),
        ))
        .cloned()
        .unwrap_or_default()
}

/// Windows timestamp restoration needs attribute access on the same read
/// handle. Ordinary witnesses must remain usable with read access alone.
pub(crate) fn open_local_witness(
    path: &std::path::Path,
    preserve_atime: bool,
) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    if preserve_atime {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_WRITE_ATTRIBUTES};
        options.access_mode(FILE_GENERIC_READ | FILE_WRITE_ATTRIBUTES);
    }
    #[cfg(not(windows))]
    let _ = preserve_atime;
    options.open(path)
}

async fn local_content(
    path: &std::path::Path,
    expected: &Content,
    preserve_atime: bool,
) -> Result<bool, BcmrError> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    match expected {
        Content::Link(expected) if metadata.is_symlink() => Ok(tokio::fs::read_link(path)
            .await?
            .as_os_str()
            .as_encoded_bytes()
            == expected),
        Content::File(expected) if metadata.is_file() => {
            let path_for_open = path.to_owned();
            let opened_file = tokio::task::spawn_blocking(move || {
                open_local_witness(&path_for_open, preserve_atime)
            })
            .await??;
            let mut file = tokio::fs::File::from_std(opened_file);
            let opened = if preserve_atime {
                Some(file.metadata().await?)
            } else {
                None
            };
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                crate::core::interrupt::check()?;
                let n = file.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            if let Some(opened) = opened {
                let file = file.into_std().await;
                filetime::set_file_handle_times(
                    &file,
                    Some(filetime::FileTime::from_last_access_time(&opened)),
                    None,
                )?;
            }
            Ok(hasher.finalize().to_hex().as_str() == expected)
        }
        _ => Ok(false),
    }
}

async fn sftp_content(
    client: &mut SftpClient,
    path: &str,
    expected: &Content,
    preserve_atime: bool,
) -> Result<bool, BcmrError> {
    let metadata = client.stat(path, false).await?;
    match expected {
        Content::Link(expected) if metadata.is_symlink() => {
            Ok(client.readlink(path).await? == *expected)
        }
        Content::File(expected) if metadata.is_file() => {
            let handle = client.open(path, flags::READ, &Attrs::default()).await?;
            let result = async {
                let mut hasher = blake3::Hasher::new();
                let mut offset = 0;
                loop {
                    crate::core::interrupt::check()?;
                    let data = client.read(&handle, offset, 32 * 1024).await?;
                    if data.is_empty() {
                        break;
                    }
                    offset += data.len() as u64;
                    hasher.update(&data);
                }
                Ok::<_, BcmrError>(hasher.finalize().to_hex().as_str() == expected)
            }
            .await;
            let restore = if preserve_atime {
                client
                    .fsetstat(
                        &handle,
                        &Attrs {
                            atime: metadata.atime,
                            mtime: metadata.mtime,
                            ..Default::default()
                        },
                    )
                    .await
            } else {
                Ok(())
            };
            let close = client.close(&handle).await;
            let result = result?;
            restore?;
            close?;
            Ok(result)
        }
        _ => Ok(false),
    }
}

async fn remote_content(
    target: &RemotePath,
    expected: &Content,
    preserve_atime: bool,
) -> Result<bool, BcmrError> {
    if backend::is_sftp(&target.ssh_target()).await? {
        let session = backend::session(&target.ssh_target()).await?;
        return sftp_content(
            &mut *session.lock().await,
            &target.path,
            expected,
            preserve_atime,
        )
        .await;
    }
    let entries = super::inventory_remote(target, true, &[]).await?;
    let Some(entry) = entries.first() else {
        return Ok(false);
    };
    match expected {
        Content::Link(expected) => Ok(entry
            .link_target
            .as_ref()
            .is_some_and(|target| target.as_bytes() == expected)),
        Content::File(expected) if !entry.is_dir && entry.link_target.is_none() => {
            Ok(ssh_hash(target, preserve_atime).await? == *expected)
        }
        _ => Ok(false),
    }
}

/// Bind the timestamp restoration to the same open descriptor that is hashed.
/// The reference file stores timestamps only, never source payload bytes.
async fn ssh_hash(target: &RemotePath, preserve_atime: bool) -> Result<String, BcmrError> {
    let script = r#"
path=$1
preserve_atime=$2
case "$path" in /*) ;; *) path=./$path ;; esac
[ ! -L "$path" ] && [ -f "$path" ] || exit 1
exec 3< "$path" || exit 1
if [ "$preserve_atime" != 1 ]; then exec cat <&3; fi
reference=$(mktemp "${TMPDIR:-/tmp}/bcmr.move-atime.XXXXXXXXXX") || exit 1
recorded=0
finish() {
    rc=$?
    trap - EXIT HUP INT TERM
    if [ "$recorded" = 1 ]; then touch -a -r "$reference" /dev/fd/3 || rc=1; fi
    rm -f "$reference" || rc=1
    exit "$rc"
}
trap finish EXIT HUP INT TERM
touch -a -r /dev/fd/3 "$reference" || exit 1
recorded=1
cat <&3
"#;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let mut child = super::ssh_command(&target.ssh_target())
        .arg(format!(
            "sh -c {} bcmr-move-witness {} {}",
            quote(script),
            quote(&target.path),
            if preserve_atime { "1" } else { "0" },
        ))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| backend::invalid("missing SSH move-witness output"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| backend::invalid("missing SSH move-witness diagnostics"))?;
    let drain = tokio::spawn(async move {
        let mut retained = Vec::new();
        let mut buffer = [0; 4096];
        while let Ok(n) = stderr.read(&mut buffer).await {
            if n == 0 {
                break;
            }
            let keep = n.min(8192usize.saturating_sub(retained.len()));
            retained.extend_from_slice(&buffer[..keep]);
        }
        String::from_utf8_lossy(&retained).into_owned()
    });
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        crate::core::interrupt::check()?;
        let n = stdout.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let status = child.wait().await?;
    let diagnostic = drain.await.unwrap_or_default();
    if !status.success() {
        return Err(backend::invalid(format!(
            "cannot validate move destination '{}': {diagnostic}",
            target.display()
        )));
    }
    Ok(hash.finalize().to_hex().to_string())
}

pub(crate) async fn validate(
    source: &RemotePath,
    client: &mut SftpClient,
) -> Result<(), BcmrError> {
    for (target, expected, preserve_atime) in witnesses(source) {
        crate::core::interrupt::check()?;
        let valid = match &target {
            Target::Local(path) => local_content(path, &expected, preserve_atime).await,
            Target::Remote(remote) if remote.ssh_target() == source.ssh_target() => {
                sftp_content(client, &remote.path, &expected, preserve_atime).await
            }
            Target::Remote(remote) => remote_content(remote, &expected, preserve_atime).await,
        }?;
        if !valid {
            return Err(backend::invalid("published move destination changed or aliases the quarantined source; source cleanup refused"));
        }
    }
    Ok(())
}

pub(crate) async fn validate_ssh(source: &RemotePath) -> Result<(), BcmrError> {
    for (target, expected, preserve_atime) in witnesses(source) {
        crate::core::interrupt::check()?;
        let valid = match target {
            Target::Local(path) => local_content(&path, &expected, preserve_atime).await?,
            Target::Remote(remote) => remote_content(&remote, &expected, preserve_atime).await?,
        };
        if !valid {
            return Err(backend::invalid("published move destination changed or aliases the quarantined source; source cleanup refused"));
        }
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn preserved_witness_restores_atime_without_data_write_access() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("payload");
        std::fs::write(&path, b"read-only witness payload").unwrap();
        let mut file = open_local_witness(&path, true).unwrap();
        let mut payload = String::new();
        file.read_to_string(&mut payload).unwrap();
        assert_eq!(payload, "read-only witness payload");
        let expected_atime = filetime::FileTime::from_unix_time(1_500_000_001, 0);
        filetime::set_file_handle_times(&file, Some(expected_atime), None).unwrap();
        assert_eq!(
            filetime::FileTime::from_last_access_time(&file.metadata().unwrap()),
            expected_atime
        );
        assert_eq!(
            file.write_all(b"must not be writable").unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"read-only witness payload");
    }
}
