//! Filesystem operations implemented entirely through the SFTP subsystem.
use super::backend::{invalid, missing, session};
use super::{RemotePath, RemoteTreeEntry};
use crate::core::error::BcmrError;
use crate::core::file_metadata::PortableFileMetadata;
use crate::core::sftp::{flags, Attrs};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

type MetadataSnapshot = HashMap<(String, String), PortableFileMetadata>;
static SOURCE_METADATA: OnceLock<Mutex<MetadataSnapshot>> = OnceLock::new();
type MoveSnapshots = HashMap<(String, String), (Option<String>, Option<Vec<u8>>)>;
static MOVE_SNAPSHOTS: OnceLock<Mutex<MoveSnapshots>> = OnceLock::new();

pub(crate) fn remember_move_hash(remote: &RemotePath, hash: &str) {
    MOVE_SNAPSHOTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry((remote.ssh_target(), remote.path.clone()))
        .or_default()
        .0 = Some(hash.to_owned());
}
pub(crate) fn remember_move_link(remote: &RemotePath, target: &[u8]) {
    MOVE_SNAPSHOTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry((remote.ssh_target(), remote.path.clone()))
        .or_default()
        .1 = Some(target.to_owned());
}
#[allow(dead_code)] // Command binary verifies delivery against the captured source.
pub(crate) fn captured_move_hash(remote: &RemotePath) -> Option<String> {
    MOVE_SNAPSHOTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(remote.ssh_target(), remote.path.clone()))
        .and_then(|value| value.0.clone())
}
#[allow(dead_code)] // Command binary verifies preserved link content.
pub(crate) fn captured_move_link(remote: &RemotePath) -> Option<Vec<u8>> {
    MOVE_SNAPSHOTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(remote.ssh_target(), remote.path.clone()))
        .and_then(|value| value.1.clone())
}

fn remember_metadata(remote: &RemotePath, metadata: &Attrs) {
    if let Ok(metadata) = portable(metadata) {
        SOURCE_METADATA
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry((remote.ssh_target(), remote.path.clone()))
            .or_insert(metadata);
    }
}

pub(crate) fn captured_metadata(remote: &RemotePath) -> Option<PortableFileMetadata> {
    SOURCE_METADATA
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(remote.ssh_target(), remote.path.clone()))
        .copied()
}

pub(crate) fn utf8(bytes: Vec<u8>, context: &str) -> Result<String, BcmrError> {
    String::from_utf8(bytes).map_err(|_| invalid(format!("SFTP {context} is not UTF-8")))
}

pub(crate) async fn attrs(remote: &RemotePath, follow: bool) -> Result<Attrs, BcmrError> {
    session(&remote.ssh_target())
        .await?
        .lock()
        .await
        .stat(&remote.path, follow)
        .await
}

pub(crate) fn portable(attrs: &Attrs) -> Result<PortableFileMetadata, BcmrError> {
    Ok(PortableFileMetadata {
        atime_seconds: attrs
            .atime
            .ok_or_else(|| invalid("SFTP server omitted access time"))?
            as i64,
        atime_nanoseconds: 0,
        mtime_seconds: attrs
            .mtime
            .ok_or_else(|| invalid("SFTP server omitted modification time"))?
            as i64,
        mtime_nanoseconds: 0,
        mode: attrs
            .permissions
            .ok_or_else(|| invalid("SFTP server omitted permissions"))?
            & 0o777,
    })
}

#[allow(dead_code)] // Used by the command binary composite transfer engine.
pub(crate) fn wire_attrs(metadata: PortableFileMetadata) -> Result<Attrs, BcmrError> {
    Ok(Attrs {
        permissions: Some(metadata.mode & 0o777),
        atime: Some(
            u32::try_from(metadata.atime_seconds)
                .map_err(|_| invalid("SFTP v3 access time is outside 1970..2106"))?,
        ),
        mtime: Some(
            u32::try_from(metadata.mtime_seconds)
                .map_err(|_| invalid("SFTP v3 modification time is outside 1970..2106"))?,
        ),
        ..Default::default()
    })
}

pub(crate) async fn expand_tilde(target: &str, path: &str) -> Result<String, BcmrError> {
    let suffix = match path.strip_prefix('~') {
        Some("") => "",
        Some(suffix) if suffix.starts_with('/') => suffix,
        Some(_) => {
            return Err(invalid(
                "SFTP cannot resolve another user's ~user directory; use an absolute path",
            ))
        }
        None => return Ok(path.to_owned()),
    };
    let home = utf8(
        session(target).await?.lock().await.realpath(".").await?,
        "home directory",
    )?;
    Ok(format!("{}{suffix}", home.trim_end_matches('/')))
}

pub(crate) async fn inventory(
    root: &RemotePath,
    no_deref: bool,
    excludes: &[regex::Regex],
) -> Result<Vec<RemoteTreeEntry>, BcmrError> {
    let session = session(&root.ssh_target()).await?;
    let mut client = session.lock().await;
    let mut pending = vec![(String::new(), Vec::<Vec<u8>>::new())];
    let mut result = Vec::new();
    while let Some((relative, mut ancestors)) = pending.pop() {
        if crate::core::traversal::is_remote_entry_excluded(&root.path, &relative, excludes) {
            continue;
        }
        let path = if relative.is_empty() {
            root.clone()
        } else {
            root.join(&relative)
        };
        let mut metadata = client.stat(&path.path, false).await?;
        let link = metadata.is_symlink();
        let link_target = if link && no_deref {
            Some(utf8(
                client.readlink(&path.path).await?,
                "symbolic link target",
            )?)
        } else {
            None
        };
        if link && !no_deref {
            metadata = client.stat(&path.path, true).await?;
        }
        // Hashing a move source or enumerating its directory can advance
        // access time. Preserve the source metadata seen before either read.
        if link_target.is_none() {
            remember_metadata(&path, &metadata);
        }
        if !metadata.is_dir() && !metadata.is_file() && link_target.is_none() {
            return Err(invalid(format!(
                "SFTP source '{path}' is not a regular file, directory or selected link"
            )));
        }
        if metadata.is_dir() {
            let physical = client.realpath(&path.path).await?;
            if ancestors.contains(&physical) {
                return Err(invalid(format!(
                    "symbolic-link directory cycle at '{path}'"
                )));
            }
            ancestors.push(physical);
            let mut children = client.readdir(&path.path).await?;
            children.sort_by(|a, b| a.0.cmp(&b.0));
            for (raw, _) in children.into_iter().rev() {
                let selected = if relative.is_empty() {
                    String::from_utf8_lossy(&raw).into_owned()
                } else {
                    format!("{relative}/{}", String::from_utf8_lossy(&raw))
                };
                if crate::core::traversal::is_remote_entry_excluded(&root.path, &selected, excludes)
                {
                    continue;
                }
                let name = utf8(raw, "directory entry")?;
                let selected = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                pending.push((selected, ancestors.clone()));
            }
        }
        result.push(RemoteTreeEntry {
            path: relative,
            size: metadata.size.unwrap_or(0),
            is_dir: metadata.is_dir(),
            link_target,
        });
    }
    Ok(result)
}

#[allow(dead_code)] // Used by the command binary composite transfer engine.
pub(crate) async fn mkdir_all(remote: &RemotePath) -> Result<(), BcmrError> {
    let session = session(&remote.ssh_target()).await?;
    let mut client = session.lock().await;
    let mut current = if remote.path.starts_with('/') {
        "/".to_owned()
    } else {
        String::new()
    };
    for part in remote
        .path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if part == ".." {
            return Err(invalid("SFTP parent directory component refused"));
        }
        if !current.is_empty() && !current.ends_with('/') {
            current.push('/');
        }
        current.push_str(part);
        match client.stat(&current, true).await {
            Ok(attrs) if attrs.is_dir() => {}
            Ok(_) => {
                return Err(invalid(format!(
                    "SFTP directory '{current}' is not a directory"
                )))
            }
            Err(error) if missing(&error) => {
                if let Err(error) = client
                    .mkdir(
                        &current,
                        &Attrs {
                            permissions: Some(0o777),
                            ..Default::default()
                        },
                    )
                    .await
                {
                    // Another writer may create this parent concurrently.
                    if !client.stat(&current, true).await?.is_dir() {
                        return Err(error);
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(crate) async fn hash(remote: &RemotePath, length: Option<u64>) -> Result<String, BcmrError> {
    let session = session(&remote.ssh_target()).await?;
    let mut client = session.lock().await;
    let handle = client
        .open(&remote.path, flags::READ, &Attrs::default())
        .await?;
    let result = async {
        let mut hasher = blake3::Hasher::new();
        let mut offset = 0u64;
        loop {
            let remaining = length.unwrap_or(u64::MAX).saturating_sub(offset);
            if remaining == 0 {
                break;
            }
            let data = client
                .read(&handle, offset, remaining.min(64 * 1024) as usize)
                .await?;
            if data.is_empty() {
                break;
            }
            offset += data.len() as u64;
            hasher.update(&data);
        }
        if length.is_some_and(|length| offset != length) {
            return Err(invalid("SFTP file shortened during hashing"));
        }
        Ok(hasher.finalize().to_hex().to_string())
    }
    .await;
    let close = client.close(&handle).await;
    let hash = result?;
    close?;
    Ok(hash)
}

pub(crate) async fn names(remote: &RemotePath) -> Result<Vec<String>, BcmrError> {
    let entries = session(&remote.ssh_target())
        .await?
        .lock()
        .await
        .readdir(&remote.path)
        .await?;
    let mut names = entries
        .into_iter()
        .map(|(name, _)| utf8(name, "directory entry"))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    Ok(names)
}
