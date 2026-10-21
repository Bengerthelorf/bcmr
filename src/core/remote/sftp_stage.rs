//! Private, canonically pinned SFTP staging with guarded publication/cleanup.
//!
//! SFTP v3 supplies neither inode nor ctime. A canonical pathname plus owner,
//! mode, an unpredictable ownership marker, and an anchored payload fingerprint
//! is a bounded namespace/content guard, not proof of inode identity. An exact
//! replacement of all guards remains indistinguishable, and a writer retaining
//! an open descriptor can still act after the last check. Any observed mismatch
//! retains recovery data instead of deleting through an unverified namespace.
//! Publication uses hardlink or SYMLINK atomic creation, never v3 RENAME's
//! potentially racy refusal of an existing destination.

use super::backend::{invalid, missing, session};
use super::sftp_ops;
use super::RemotePath;
use crate::core::error::BcmrError;
use crate::core::file_metadata::PortableFileMetadata;
use crate::core::sftp::{flags, Attrs, Handle, SftpClient, MAX_IO_LEN};
use parking_lot::Mutex;
use ring::rand::{SecureRandom, SystemRandom};

#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnerMode {
    permissions: u32,
    uid: u32,
    gid: u32,
}

impl OwnerMode {
    fn capture(attrs: &Attrs) -> Result<Self, BcmrError> {
        Ok(Self {
            permissions: attrs
                .permissions
                .ok_or_else(|| invalid("SFTP staging requires permissions"))?,
            uid: attrs
                .uid
                .ok_or_else(|| invalid("SFTP staging requires uid"))?,
            gid: attrs
                .gid
                .ok_or_else(|| invalid("SFTP staging requires gid"))?,
        })
    }

    fn matches(&self, attrs: &Attrs) -> bool {
        attrs.permissions == Some(self.permissions)
            && attrs.uid == Some(self.uid)
            && attrs.gid == Some(self.gid)
    }

    fn same_owner(&self, attrs: &Attrs) -> bool {
        attrs.uid == Some(self.uid) && attrs.gid == Some(self.gid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StableAttrs {
    owner: OwnerMode,
    size: u64,
    mtime: u32,
}

impl StableAttrs {
    fn capture(attrs: &Attrs) -> Result<Self, BcmrError> {
        Ok(Self {
            owner: OwnerMode::capture(attrs)?,
            size: attrs
                .size
                .ok_or_else(|| invalid("SFTP staging requires size"))?,
            mtime: attrs
                .mtime
                .ok_or_else(|| invalid("SFTP staging requires modification time"))?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    attrs: StableAttrs,
    hash: blake3::Hash,
}

#[derive(Clone)]
enum PayloadGuard {
    File { owner: OwnerMode, anchor: String },
    Link { attrs: StableAttrs, target: Vec<u8> },
}

#[derive(Clone, Default)]
struct StageState {
    payload: Option<PayloadGuard>,
    sealed: Option<FileStamp>,
    payload_removed: bool,
    anchor_removed: bool,
    marker_removed: bool,
    published: bool,
    finished: bool,
}

pub(crate) struct SftpStage {
    target: RemotePath,
    pub(crate) payload: RemotePath,
    canonical_target: String,
    directory: String,
    original_parent: String,
    canonical_parent: String,
    parent_guard: OwnerMode,
    directory_guard: OwnerMode,
    marker: String,
    marker_contents: Vec<u8>,
    marker_guard: StableAttrs,
    anchor: String,
    state: Mutex<StageState>,
}

pub(crate) fn nonce() -> Result<String, BcmrError> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| invalid("cannot generate private transaction name"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl SftpStage {
    pub(crate) async fn create(target: &RemotePath, overwrite: bool) -> Result<Self, BcmrError> {
        let (parent, name) = target.path.rsplit_once('/').unwrap_or((".", &target.path));
        let parent = if parent.is_empty() { "/" } else { parent };
        if name.is_empty() || matches!(name, "." | "..") {
            return Err(invalid("SFTP publication requires a named file"));
        }
        let session = session(&target.ssh_target()).await?;
        {
            let mut client = session.lock().await;
            reject_existing(&mut client, &target.path, target, overwrite).await?;
        }
        sftp_ops::mkdir_all(&RemotePath {
            path: parent.to_owned(),
            ..target.clone()
        })
        .await?;
        let mut client = session.lock().await;
        let canonical_parent = sftp_ops::utf8(client.realpath(parent).await?, "staging parent")?;
        let canonical_parent = if canonical_parent == "/" {
            canonical_parent
        } else {
            canonical_parent.trim_end_matches('/').to_owned()
        };
        let canonical_target = join(&canonical_parent, name);
        reject_existing(&mut client, &canonical_target, target, overwrite).await?;
        let parent_attrs = client.stat(&canonical_parent, false).await?;
        if !parent_attrs.is_dir() {
            return Err(invalid("SFTP staging parent changed kind"));
        }
        let parent_guard = OwnerMode::capture(&parent_attrs)?;
        let directory = join(&canonical_parent, &format!(".bcmr.upload.{}", nonce()?));
        client
            .mkdir(
                &directory,
                &Attrs {
                    permissions: Some(0o700),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| retained_at(&directory, error))?;
        let directory_attrs = client
            .stat(&directory, false)
            .await
            .map_err(|error| retained_at(&directory, error))?;
        let directory_guard =
            OwnerMode::capture(&directory_attrs).map_err(|error| retained_at(&directory, error))?;
        if !directory_attrs.is_dir() || directory_guard.permissions & 0o777 != 0o700 {
            return Err(retained_at(
                &directory,
                "private staging directory was not created with mode 0700",
            ));
        }
        let marker_name = reserved_name(".bcmr-owner", name);
        let anchor_name = reserved_name(".bcmr-anchor", name);
        let marker = join(&directory, &marker_name);
        let anchor = join(&directory, &anchor_name);
        let marker_contents = nonce()
            .map_err(|error| retained_at(&directory, error))?
            .into_bytes();
        let handle = client
            .open(
                &marker,
                flags::WRITE | flags::CREATE | flags::EXCLUSIVE,
                &Attrs {
                    permissions: Some(0o600),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| retained_at(&directory, error))?;
        let written = client.write(&handle, 0, &marker_contents).await;
        let closed = client.close(&handle).await;
        written
            .and(closed)
            .map_err(|error| retained_at(&directory, error))?;
        let marker_attrs = client
            .stat(&marker, false)
            .await
            .map_err(|error| retained_at(&directory, error))?;
        let marker_guard =
            StableAttrs::capture(&marker_attrs).map_err(|error| retained_at(&directory, error))?;
        let stage = Self {
            target: target.clone(),
            payload: RemotePath {
                path: join(&directory, name),
                ..target.clone()
            },
            canonical_target,
            directory,
            original_parent: parent.to_owned(),
            canonical_parent,
            parent_guard,
            directory_guard,
            marker,
            marker_contents,
            marker_guard,
            anchor,
            state: Mutex::new(StageState::default()),
        };
        stage
            .validate_namespace(&mut client)
            .await
            .map_err(|error| retained_at(&stage.directory, error))?;
        Ok(stage)
    }

    /// Call immediately after the exclusive OPEN or SYMLINK succeeds, before
    /// writing data. A hardlink anchor follows our open file through unlink or
    /// replacement of the working pathname; size/mtime may legitimately evolve.
    pub(crate) async fn capture_payload(&self) -> Result<(), BcmrError> {
        let session = session(&self.target.ssh_target())
            .await
            .map_err(|error| retained_at(&self.directory, error))?;
        let mut client = session.lock().await;
        let result = async {
            self.validate_namespace(&mut client).await?;
            if self.state.lock().payload.is_some() {
                return Err(invalid("SFTP payload ownership was already captured"));
            }
            let attrs = client.stat(&self.payload.path, false).await?;
            let guard = if attrs.is_file() {
                client.hardlink(&self.payload.path, &self.anchor).await?;
                if StableAttrs::capture(&client.stat(&self.anchor, false).await?)?
                    != StableAttrs::capture(&attrs)?
                {
                    return Err(invalid("SFTP payload changed while anchoring it"));
                }
                PayloadGuard::File {
                    owner: OwnerMode::capture(&attrs)?,
                    anchor: self.anchor.clone(),
                }
            } else if attrs.is_symlink() {
                PayloadGuard::Link {
                    attrs: StableAttrs::capture(&attrs)?,
                    target: client.readlink(&self.payload.path).await?,
                }
            } else {
                return Err(invalid("SFTP staging payload has an unsupported kind"));
            };
            self.state.lock().payload = Some(guard);
            self.validate_namespace(&mut client).await?;
            self.validate_entries(&mut client).await
        }
        .await;
        result.map_err(|error| retained_at(&self.directory, error))
    }

    /// Verify the owned staging namespace and payload before reading content.
    /// Callers must not hash the public payload pathname directly: a replaced
    /// symlink would otherwise read an unrelated file before commit refuses it.
    pub(crate) async fn payload_hash(&self) -> Result<String, BcmrError> {
        let session = session(&self.target.ssh_target())
            .await
            .map_err(|error| retained_at(&self.directory, error))?;
        let mut client = session.lock().await;
        let result = async {
            if self.state.lock().finished {
                return Err(invalid("SFTP stage was already completed"));
            }
            self.validate_namespace(&mut client).await?;
            self.validate_payload(&mut client).await?;
            self.state
                .lock()
                .sealed
                .as_ref()
                .map(|stamp| stamp.hash.to_hex().to_string())
                .ok_or_else(|| invalid("SFTP staging has no sealed regular-file payload"))
        }
        .await;
        result.map_err(|error| retained_at(&self.directory, error))
    }

    /// Seal and publish while the staging file remains readable. Final metadata
    /// is applied only after guarded cleanup, through a retained, fingerprinted
    /// handle. The published entry is briefly visible with staging mode 0600;
    /// an unreadable final mode cannot disable cleanup or redirect SETSTAT.
    pub(crate) async fn commit(
        &self,
        metadata: Option<PortableFileMetadata>,
    ) -> Result<(), BcmrError> {
        let final_attrs = metadata
            .map(sftp_ops::wire_attrs)
            .transpose()
            .map_err(|error| retained_at(&self.directory, error))?;
        let session = session(&self.target.ssh_target())
            .await
            .map_err(|error| retained_at(&self.directory, error))?;
        let mut client = session.lock().await;
        let mut retained = None;
        let result = async {
            if self.state.lock().finished {
                return Err(invalid("SFTP stage was already completed"));
            }
            self.validate_namespace(&mut client).await?;
            self.validate_payload(&mut client).await?;
            let payload = self
                .state
                .lock()
                .payload
                .clone()
                .ok_or_else(|| invalid("SFTP payload ownership was never captured"))?;
            match payload {
                PayloadGuard::File { anchor, .. } => {
                    let (stamp, handle) = fingerprint_open(&mut client, &anchor).await?;
                    retained = Some(handle);
                    if self.state.lock().sealed.as_ref() != Some(&stamp) {
                        return Err(invalid(
                            "SFTP retained payload handle did not match its seal",
                        ));
                    }
                    self.validate_namespace(&mut client).await?;
                    self.validate_entries(&mut client).await?;
                    client.hardlink(&anchor, &self.canonical_target).await?
                }
                PayloadGuard::Link { target, .. } => {
                    if final_attrs.is_some() {
                        return Err(invalid("SFTP staged link metadata is not supported"));
                    }
                    client.symlink(&target, &self.canonical_target).await?
                }
            }
            self.state.lock().published = true;
            self.cleanup_with_client(&mut client).await?;
            if let Some(attrs) = &final_attrs {
                let handle = retained.as_ref().ok_or_else(|| {
                    invalid("SFTP final metadata requires a retained file handle")
                })?;
                // No reads follow this operation: preserved access time remains
                // exact to v3 seconds, including final mode 000 or write-only.
                client.fsetstat(handle, attrs).await?;
            }
            Ok(())
        }
        .await;
        let closed = match retained {
            Some(handle) => client.close(&handle).await,
            None => Ok(()),
        };
        result.and(closed).map_err(|error| {
            let state = self.state.lock();
            if state.published && state.finished {
                invalid(format!(
                    "SFTP destination content was published at {:?}, but final metadata or handle completion failed: {error}",
                    self.canonical_target
                ))
            } else if state.published {
                invalid(format!(
                    "SFTP destination content was published at {:?}; {}",
                    self.canonical_target,
                    retained_at(&self.directory, error)
                ))
            } else {
                retained_at(&self.directory, error)
            }
        })
    }

    pub(crate) async fn cleanup(&self) -> Result<(), BcmrError> {
        if self.state.lock().finished {
            return Ok(());
        }
        let session = session(&self.target.ssh_target())
            .await
            .map_err(|error| retained_at(&self.directory, error))?;
        let mut client = session.lock().await;
        self.cleanup_with_client(&mut client)
            .await
            .map_err(|error| retained_at(&self.directory, error))
    }

    async fn validate_namespace(&self, client: &mut SftpClient) -> Result<(), BcmrError> {
        if client.realpath(&self.original_parent).await? != self.canonical_parent.as_bytes()
            || client.realpath(&self.canonical_parent).await? != self.canonical_parent.as_bytes()
        {
            return Err(invalid("SFTP destination parent changed during transfer"));
        }
        let parent = client.stat(&self.canonical_parent, false).await?;
        if !parent.is_dir() || !self.parent_guard.matches(&parent) {
            return Err(invalid(
                "SFTP destination parent owner, mode or kind changed",
            ));
        }
        let directory = client.stat(&self.directory, false).await?;
        if !directory.is_dir()
            || !self.directory_guard.matches(&directory)
            || client.realpath(&self.directory).await? != self.directory.as_bytes()
        {
            return Err(invalid("SFTP private staging namespace changed"));
        }
        if !self.state.lock().marker_removed {
            let marker = client.stat(&self.marker, false).await?;
            if !marker.is_file()
                || StableAttrs::capture(&marker)? != self.marker_guard
                || read_small(client, &self.marker, self.marker_contents.len()).await?
                    != self.marker_contents
                || StableAttrs::capture(&client.stat(&self.marker, false).await?)?
                    != self.marker_guard
            {
                return Err(invalid("SFTP private staging ownership marker changed"));
            }
        }
        Ok(())
    }

    async fn validate_entries(&self, client: &mut SftpClient) -> Result<(), BcmrError> {
        let state = self.state.lock().clone();
        let mut expected = Vec::new();
        if !state.marker_removed {
            expected.push(basename(&self.marker).as_bytes().to_vec());
        }
        if state.payload.is_some() && !state.payload_removed {
            expected.push(basename(&self.payload.path).as_bytes().to_vec());
        }
        if matches!(state.payload, Some(PayloadGuard::File { .. })) && !state.anchor_removed {
            expected.push(basename(&self.anchor).as_bytes().to_vec());
        }
        let mut actual = client
            .readdir(&self.directory)
            .await?
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        if actual != expected {
            return Err(invalid(
                "SFTP private staging contents changed or payload ownership is unknown",
            ));
        }
        Ok(())
    }

    async fn validate_payload(&self, client: &mut SftpClient) -> Result<(), BcmrError> {
        self.validate_entries(client).await?;
        let state = self.state.lock().clone();
        match state.payload {
            Some(PayloadGuard::File { owner, anchor }) if !state.payload_removed => {
                let anchored = client.stat(&anchor, false).await?;
                let working = client.stat(&self.payload.path, false).await?;
                if !anchored.is_file()
                    || !working.is_file()
                    || !owner.same_owner(&anchored)
                    || StableAttrs::capture(&anchored)? != StableAttrs::capture(&working)?
                {
                    return Err(invalid(
                        "SFTP working payload no longer matches its owned anchor",
                    ));
                }
                let stamp = fingerprint(client, &anchor).await?;
                if fingerprint(client, &self.payload.path).await? != stamp
                    || state.sealed.as_ref().is_some_and(|sealed| sealed != &stamp)
                {
                    return Err(invalid(
                        "SFTP working payload content changed or was replaced",
                    ));
                }
                self.state.lock().sealed = Some(stamp);
            }
            Some(PayloadGuard::Link { attrs, target }) if !state.payload_removed => {
                let before = client.stat(&self.payload.path, false).await?;
                if !before.is_symlink()
                    || StableAttrs::capture(&before)? != attrs
                    || client.readlink(&self.payload.path).await? != target
                    || StableAttrs::capture(&client.stat(&self.payload.path, false).await?)?
                        != attrs
                {
                    return Err(invalid("SFTP staged link changed or was replaced"));
                }
            }
            _ => {}
        }
        self.validate_namespace(client).await
    }

    async fn cleanup_with_client(&self, client: &mut SftpClient) -> Result<(), BcmrError> {
        if self.state.lock().finished {
            return Ok(());
        }
        self.validate_namespace(client).await?;
        self.validate_payload(client).await?;
        let state = self.state.lock().clone();
        if state.payload.is_some() && !state.payload_removed {
            client.remove(&self.payload.path).await?;
            self.state.lock().payload_removed = true;
        }
        if let Some(PayloadGuard::File { anchor, .. }) = state.payload {
            if !state.anchor_removed {
                self.validate_namespace(client).await?;
                let sealed = self.state.lock().sealed.clone().ok_or_else(|| {
                    invalid("owned SFTP anchor has no confirmed content fingerprint")
                })?;
                if fingerprint(client, &anchor).await? != sealed {
                    return Err(invalid("SFTP owned anchor changed before cleanup"));
                }
                client.remove(&anchor).await?;
                self.state.lock().anchor_removed = true;
            }
        }
        self.validate_namespace(client).await?;
        self.validate_entries(client).await?;
        if !self.state.lock().marker_removed {
            client.remove(&self.marker).await?;
            self.state.lock().marker_removed = true;
        }
        // RMDIR is always nonrecursive, including retries after a lost reply.
        client.rmdir(&self.directory).await?;
        self.state.lock().finished = true;
        Ok(())
    }
}

async fn reject_existing(
    client: &mut SftpClient,
    path: &str,
    target: &RemotePath,
    overwrite: bool,
) -> Result<(), BcmrError> {
    match client.stat(path, false).await {
        Ok(_) if !overwrite => Err(BcmrError::TargetExists(target.display().into())),
        Ok(_) => Err(invalid(format!("SFTP v3 cannot preserve existing ACL/xattrs or conditionally replace an entry at '{target}'; use --remote-backend=ssh or a new destination path"))),
        Err(error) if missing(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

async fn read_small(
    client: &mut SftpClient,
    path: &str,
    expected: usize,
) -> Result<Vec<u8>, BcmrError> {
    let handle = client.open(path, flags::READ, &Attrs::default()).await?;
    let result = client.read(&handle, 0, expected + 1).await;
    let closed = client.close(&handle).await;
    let bytes = result?;
    closed?;
    if bytes.len() != expected {
        return Err(invalid("SFTP ownership marker has an unexpected length"));
    }
    Ok(bytes)
}

async fn fingerprint(client: &mut SftpClient, path: &str) -> Result<FileStamp, BcmrError> {
    let (stamp, handle) = fingerprint_open(client, path).await?;
    client.close(&handle).await?;
    Ok(stamp)
}

async fn fingerprint_open(
    client: &mut SftpClient,
    path: &str,
) -> Result<(FileStamp, Handle), BcmrError> {
    let before = client.stat(path, false).await?;
    if !before.is_file() {
        return Err(invalid("SFTP owned payload is no longer a regular file"));
    }
    let attrs = StableAttrs::capture(&before)?;
    let atime = before
        .atime
        .ok_or_else(|| invalid("SFTP staging validation requires access time"))?;
    let handle = client.open(path, flags::READ, &Attrs::default()).await?;
    let result = async {
        let mut hash = blake3::Hasher::new();
        let mut offset = 0u64;
        while offset < attrs.size {
            let bytes = client
                .read(
                    &handle,
                    offset,
                    (attrs.size - offset).min(MAX_IO_LEN as u64) as usize,
                )
                .await?;
            if bytes.is_empty() {
                return Err(invalid("SFTP owned payload shortened while checking it"));
            }
            offset += bytes.len() as u64;
            hash.update(&bytes);
        }
        if !client.read(&handle, attrs.size, 1).await?.is_empty() {
            return Err(invalid("SFTP owned payload grew while checking it"));
        }
        if StableAttrs::capture(&client.stat(path, false).await?)? != attrs {
            return Err(invalid(
                "SFTP owned payload metadata changed while checking it",
            ));
        }
        // Validation reads must not destroy preserved atime. Apply this through
        // the already-open handle so a replaced pathname cannot redirect the
        // metadata update to an unrelated object.
        client
            .fsetstat(
                &handle,
                &Attrs {
                    atime: Some(atime),
                    mtime: Some(attrs.mtime),
                    ..Default::default()
                },
            )
            .await?;
        Ok(FileStamp {
            attrs,
            hash: hash.finalize(),
        })
    }
    .await;
    match result {
        Ok(stamp) => Ok((stamp, handle)),
        Err(error) => {
            let _ = client.close(&handle).await;
            Err(error)
        }
    }
}

fn reserved_name(base: &str, payload: &str) -> String {
    if base == payload {
        format!("{base}.guard")
    } else {
        base.to_owned()
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn join(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches('/'))
}

fn retained_at(path: &str, reason: impl std::fmt::Display) -> BcmrError {
    invalid(format!("{reason}. Unverified SFTP staging cleanup was refused; recovery data may remain at the original canonical staging location {path:?} (or where it was renamed)"))
}
