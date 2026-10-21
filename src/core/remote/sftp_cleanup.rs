//! Captured move cleanup implemented only through the SFTP subsystem.
//!
//! SFTP v3 has no inode, device, or ctime fields. These guards therefore
//! compare kind/size/whole-second mtime/mode/owner plus a complete BLAKE3 hash
//! or exact symlink bytes, and pin the canonical parent spelling. An identical
//! replacement cannot be distinguished from the captured object. Directory
//! size and mtime are intentionally ignored: selected child removal and late
//! arrivals change them. This is a bounded content/metadata concurrency guard,
//! not snapshot isolation against a writer holding an open descriptor.
//!
//! Every captured entry is validated before the first leaf is removed. Each
//! leaf is then renamed without replacement into a private sibling directory,
//! validated again, and unlinked there. Failure preserves uncopied content,
//! restoring regular files with the no-overwrite hardlink extension when
//! possible or naming their recovery path. Links are restored by atomic symlink
//! creation; their quarantine is retained if exact metadata cannot be recreated.
//! OpenSSH's standard v3 rename uses a racy stat/rename sequence for links.

use std::collections::{HashMap, HashSet};

use super::backend::{invalid, session};
use super::{sftp_ops, sftp_stage, RemotePath};
use crate::core::error::BcmrError;
use crate::core::sftp::{flags, Attrs, SftpClient, MAX_IO_LEN};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Metadata {
    kind: Kind,
    size: u64,
    permissions: u32,
    uid: u32,
    gid: u32,
    mtime: u32,
}

impl Metadata {
    fn from_attrs(attrs: &Attrs) -> Result<Self, BcmrError> {
        let kind = if attrs.is_symlink() {
            Kind::Symlink
        } else if attrs.is_dir() {
            Kind::Directory
        } else if attrs.is_file() {
            Kind::File
        } else {
            return Err(invalid(
                "SFTP cleanup source has an unsupported or unknown file type",
            ));
        };
        Ok(Self {
            kind,
            size: attrs
                .size
                .ok_or_else(|| invalid("SFTP cleanup requires the source size"))?,
            permissions: attrs
                .permissions
                .ok_or_else(|| invalid("SFTP cleanup requires source permissions"))?,
            uid: attrs
                .uid
                .ok_or_else(|| invalid("SFTP cleanup requires the source uid"))?,
            gid: attrs
                .gid
                .ok_or_else(|| invalid("SFTP cleanup requires the source gid"))?,
            mtime: attrs
                .mtime
                .ok_or_else(|| invalid("SFTP cleanup requires the source modification time"))?,
        })
    }

    fn matches(&self, actual: &Self) -> bool {
        self.kind == actual.kind
            && self.permissions == actual.permissions
            && self.uid == actual.uid
            && self.gid == actual.gid
            && (self.kind == Kind::Directory
                || (self.size == actual.size && self.mtime == actual.mtime))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Content {
    File(blake3::Hash),
    Directory,
    Symlink(Vec<u8>),
}

#[derive(Debug, Clone)]
struct Observation {
    metadata: Metadata,
    content: Content,
}

impl Observation {
    fn matches(&self, actual: &Self) -> bool {
        self.metadata.matches(&actual.metadata) && self.content == actual.content
    }
}

struct Snapshot {
    source: RemotePath,
    original_parent: String,
    canonical_parent: String,
    parent_metadata: Metadata,
    physical_path: String,
    observation: Observation,
}

impl Snapshot {
    async fn capture(client: &mut SftpClient, source: &RemotePath) -> Result<Self, BcmrError> {
        let (parent, name) = parent_and_name(&source.path)?;
        let canonical_parent = canonical_parent(client, parent).await?;
        let parent_metadata = directory_metadata(client, &canonical_parent).await?;
        let physical_path = join_path(&canonical_parent, name);
        let observation = observe(client, &physical_path).await?;
        let snapshot = Self {
            source: source.clone(),
            original_parent: parent.to_owned(),
            canonical_parent,
            parent_metadata,
            physical_path,
            observation,
        };
        snapshot.validate_parent(client).await?;
        Ok(snapshot)
    }

    async fn validate_parent(&self, client: &mut SftpClient) -> Result<(), BcmrError> {
        let actual_parent = canonical_parent(client, &self.original_parent).await?;
        if actual_parent != self.canonical_parent {
            return Err(changed(self, "canonical source parent changed"));
        }
        let actual_metadata = directory_metadata(client, &self.canonical_parent).await?;
        if !self.parent_metadata.matches(&actual_metadata) {
            return Err(changed(
                self,
                "source parent type, owner or permissions changed",
            ));
        }
        Ok(())
    }
}

pub struct SftpCleanupPlan {
    entries: Vec<Snapshot>,
}

impl SftpCleanupPlan {
    pub async fn capture(
        sources: &[RemotePath],
        excludes: &[regex::Regex],
    ) -> Result<Self, BcmrError> {
        crate::core::interrupt::check()?;
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        for source in sources {
            crate::core::interrupt::check()?;
            if crate::core::traversal::is_remote_entry_excluded(&source.path, "", excludes) {
                continue;
            }
            let mut source = source.clone();
            source.expand_tilde().await?;
            source.path = source.path.trim_end_matches('/').to_owned();
            source.reject_unsafe()?;
            parent_and_name(&source.path)?;
            let inventory = sftp_ops::inventory(&source, true, excludes).await?;
            let session = session(&source.ssh_target()).await?;
            let mut client = session.lock().await;
            for entry in inventory {
                let remote = if entry.path.is_empty() {
                    source.clone()
                } else {
                    source.join(&entry.path)
                };
                let snapshot =
                    crate::core::interrupt::read_only(Snapshot::capture(&mut client, &remote))
                        .await?;
                // Overlapping source roots or parent aliases may select a leaf
                // twice. Cleanup uses each canonical namespace spelling once.
                if seen.insert((remote.ssh_target(), snapshot.physical_path.clone())) {
                    match &snapshot.observation.content {
                        Content::File(hash) => {
                            sftp_ops::remember_move_hash(&remote, hash.to_hex().as_str());
                        }
                        Content::Symlink(target) => sftp_ops::remember_move_link(&remote, target),
                        Content::Directory => {}
                    }
                    entries.push(snapshot);
                }
            }
        }
        Ok(Self { entries })
    }

    pub async fn validate(&self) -> Result<(), BcmrError> {
        crate::core::interrupt::read_only(self.validate_captured()).await
    }

    async fn validate_captured(&self) -> Result<(), BcmrError> {
        let mut by_target: HashMap<String, Vec<&Snapshot>> = HashMap::new();
        for entry in &self.entries {
            by_target
                .entry(entry.source.ssh_target())
                .or_default()
                .push(entry);
        }
        for (target, entries) in by_target {
            let session = session(&target).await?;
            let mut client = session.lock().await;
            for entry in entries {
                crate::core::interrupt::check()?;
                validate_entry(&mut client, entry).await?;
            }
        }
        Ok(())
    }

    pub async fn remove(&self) -> Result<(), BcmrError> {
        // This completes for every target before the first namespace mutation.
        self.validate().await?;
        for entry in self
            .entries
            .iter()
            .filter(|entry| entry.observation.metadata.kind != Kind::Directory)
        {
            crate::core::interrupt::check()?;
            let session = session(&entry.source.ssh_target()).await?;
            remove_leaf(&mut *session.lock().await, entry).await?;
        }
        let mut directories = self
            .entries
            .iter()
            .filter(|entry| entry.observation.metadata.kind == Kind::Directory)
            .collect::<Vec<_>>();
        directories
            .sort_by_key(|entry| std::cmp::Reverse(entry.physical_path.matches('/').count()));
        for entry in directories {
            crate::core::interrupt::check()?;
            let session = session(&entry.source.ssh_target()).await?;
            remove_directory(&mut *session.lock().await, entry).await?;
        }
        crate::core::interrupt::check()?;
        Ok(())
    }
}

async fn canonical_parent(client: &mut SftpClient, parent: &str) -> Result<String, BcmrError> {
    let bytes = client.realpath(parent).await?;
    let parent = sftp_ops::utf8(bytes, "cleanup parent")?;
    if parent == "/" {
        Ok(parent)
    } else {
        Ok(parent.trim_end_matches('/').to_owned())
    }
}

async fn directory_metadata(client: &mut SftpClient, path: &str) -> Result<Metadata, BcmrError> {
    let metadata = Metadata::from_attrs(&client.stat(path, false).await?)?;
    if metadata.kind != Kind::Directory {
        return Err(invalid(
            "SFTP cleanup parent or quarantine is no longer a directory",
        ));
    }
    Ok(metadata)
}

async fn observe(client: &mut SftpClient, path: &str) -> Result<Observation, BcmrError> {
    let metadata = Metadata::from_attrs(&client.stat(path, false).await?)?;
    let content = match metadata.kind {
        Kind::Directory => Content::Directory,
        Kind::Symlink => Content::Symlink(client.readlink(path).await?),
        Kind::File => Content::File(hash_exact(client, path, metadata.size).await?),
    };
    let after = Metadata::from_attrs(&client.stat(path, false).await?)?;
    if !metadata.matches(&after) {
        return Err(invalid(
            "SFTP source metadata changed while computing its cleanup fingerprint",
        ));
    }
    Ok(Observation { metadata, content })
}

async fn hash_exact(
    client: &mut SftpClient,
    path: &str,
    size: u64,
) -> Result<blake3::Hash, BcmrError> {
    let handle = client.open(path, flags::READ, &Attrs::default()).await?;
    let result = async {
        let mut hasher = blake3::Hasher::new();
        let mut offset = 0;
        while offset < size {
            let length = (size - offset).min(MAX_IO_LEN as u64) as usize;
            let bytes = client.read(&handle, offset, length).await?;
            if bytes.is_empty() {
                return Err(invalid(
                    "SFTP source shortened while computing its cleanup fingerprint",
                ));
            }
            offset += bytes.len() as u64;
            hasher.update(&bytes);
        }
        if !client.read(&handle, size, 1).await?.is_empty() {
            return Err(invalid(
                "SFTP source grew while computing its cleanup fingerprint",
            ));
        }
        Ok(hasher.finalize())
    }
    .await;
    let close = client.close(&handle).await;
    let hash = result?;
    close?;
    Ok(hash)
}

async fn validate_entry(client: &mut SftpClient, entry: &Snapshot) -> Result<(), BcmrError> {
    entry.validate_parent(client).await?;
    let actual = observe(client, &entry.physical_path)
        .await
        .map_err(|error| changed(entry, error))?;
    if !entry.observation.matches(&actual) {
        return Err(changed(
            entry,
            "captured content, link target, type, owner or metadata no longer matches",
        ));
    }
    entry.validate_parent(client).await
}

struct Quarantine {
    directory: RemotePath,
    payload: RemotePath,
    metadata: Metadata,
}

impl Quarantine {
    async fn create(client: &mut SftpClient, entry: &Snapshot) -> Result<Self, BcmrError> {
        entry.validate_parent(client).await?;
        let directory = RemotePath {
            path: join_path(
                &entry.canonical_parent,
                &format!(".bcmr.move.{}", sftp_stage::nonce()?),
            ),
            ..entry.source.clone()
        };
        client
            .mkdir(
                &directory.path,
                &Attrs {
                    permissions: Some(0o700),
                    ..Attrs::default()
                },
            )
            .await?;
        let metadata = directory_metadata(client, &directory.path).await?;
        let payload = directory.join("payload");
        let quarantine = Self {
            directory,
            payload,
            metadata,
        };
        quarantine.validate(client).await?;
        if quarantine.metadata.permissions & 0o777 != 0o700 {
            return Err(invalid(format!(
                "SFTP quarantine {:?} did not retain private permissions",
                quarantine.directory.display()
            )));
        }
        Ok(quarantine)
    }

    async fn validate(&self, client: &mut SftpClient) -> Result<(), BcmrError> {
        let actual = directory_metadata(client, &self.directory.path).await?;
        if !self.metadata.matches(&actual)
            || canonical_parent(client, &self.directory.path).await? != self.directory.path
        {
            return Err(invalid(format!(
                "SFTP quarantine {:?} changed; preserved data was not removed",
                self.directory.display()
            )));
        }
        Ok(())
    }

    async fn rename_source(
        &self,
        client: &mut SftpClient,
        entry: &Snapshot,
    ) -> Result<(), BcmrError> {
        validate_entry(client, entry).await?;
        self.validate(client).await?;
        crate::core::interrupt::check()?;
        client
            .rename(&entry.physical_path, &self.payload.path, false)
            .await
    }

    async fn remove_empty(&self, client: &mut SftpClient) -> Result<(), BcmrError> {
        self.validate(client).await?;
        // Never rediscover or recursively delete quarantine contents.
        client.rmdir(&self.directory.path).await
    }
}

async fn remove_leaf(client: &mut SftpClient, entry: &Snapshot) -> Result<(), BcmrError> {
    crate::core::interrupt::read_only(validate_entry(client, entry)).await?;
    crate::core::interrupt::check()?;
    let quarantine = Quarantine::create(client, entry).await?;
    if let Err(error) = crate::core::interrupt::check() {
        let _ = quarantine.remove_empty(client).await;
        return Err(error);
    }
    if let Err(error) = quarantine.rename_source(client, entry).await {
        let _ = quarantine.remove_empty(client).await;
        return Err(contextual_error(&error, format!(
            "SFTP cleanup refused source '{}': {error}. If its original name is absent, recover preserved data from {:?}; quarantine contents were not recursively removed",
            entry.source, quarantine.payload.display(),
        )));
    }
    finish_leaf(client, entry, &quarantine).await
}

async fn finish_leaf(
    client: &mut SftpClient,
    entry: &Snapshot,
    quarantine: &Quarantine,
) -> Result<(), BcmrError> {
    let validation = async {
        entry.validate_parent(client).await?;
        quarantine.validate(client).await?;
        let actual = observe(client, &quarantine.payload.path).await?;
        if !entry.observation.matches(&actual) {
            return Err(changed(entry, "source changed during quarantine"));
        }
        quarantine.validate(client).await?;
        super::move_delivery::validate(&entry.source, client).await?;
        crate::core::interrupt::check()
    }
    .await;
    if let Err(error) = validation {
        let restore = async {
            entry.validate_parent(client).await?;
            quarantine.validate(client).await?;
            let current = Metadata::from_attrs(&client.stat(&quarantine.payload.path, false).await?)?;
            match current.kind {
                Kind::File => {
                    client
                        .hardlink(&quarantine.payload.path, &entry.physical_path)
                        .await?;
                }
                Kind::Symlink => {
                    let target = client.readlink(&quarantine.payload.path).await?;
                    client.symlink(&target, &entry.physical_path).await?;
                    let original = observe(client, &quarantine.payload.path).await?;
                    let restored = observe(client, &entry.physical_path).await?;
                    if !original.matches(&restored) {
                        return Err(invalid("link target was restored without overwrite, but original link metadata remains in quarantine"));
                    }
                }
                Kind::Directory => {
                    return Err(invalid("source became a directory; recovery contents were retained without recursive deletion"));
                }
            }
            client.remove(&quarantine.payload.path).await
        }
        .await;
        if restore.is_ok() {
            let _ = quarantine.remove_empty(client).await;
            return Err(contextual_error(&error, format!(
                "SFTP move source '{}' changed: {error}; uncopied content was restored without replacing another source name",
                entry.source,
            )));
        }
        return Err(contextual_error(&error, format!(
            "SFTP move source '{}' changed: {error}; any retained uncopied content is at {:?}. Restoration could not be confirmed without overwrite: {}",
            entry.source, quarantine.payload.display(), restore.unwrap_err(),
        )));
    }
    if let Err(error) = client.remove(&quarantine.payload.path).await {
        return Err(invalid(format!(
            "SFTP cleanup could not confirm removal of captured source '{}': {error}; any preserved quarantine content is at {:?}",
            entry.source, quarantine.payload.display(),
        )));
    }
    quarantine.remove_empty(client).await.map_err(|error| invalid(format!(
        "SFTP captured source '{}' was removed, but its private cleanup directory {:?} could not be removed: {error}",
        entry.source, quarantine.directory.display(),
    )))
}

async fn remove_directory(client: &mut SftpClient, entry: &Snapshot) -> Result<(), BcmrError> {
    crate::core::interrupt::check()?;
    entry.validate_parent(client).await?;
    let current = directory_metadata(client, &entry.physical_path).await?;
    if !entry.observation.metadata.matches(&current) {
        return Err(changed(
            entry,
            "source directory type, owner or permissions changed",
        ));
    }
    if !client.readdir(&entry.physical_path).await?.is_empty() {
        return Ok(()); // Excluded or newly arrived entries remain untouched.
    }
    crate::core::interrupt::check()?;
    match client.rmdir(&entry.physical_path).await {
        Ok(()) => Ok(()),
        Err(error) => {
            // A writer may populate it between READDIR and RMDIR. Confirm that
            // precise non-destructive case; permissions and other errors remain.
            if let Ok(current) = directory_metadata(client, &entry.physical_path).await {
                if entry.observation.metadata.matches(&current)
                    && !client.readdir(&entry.physical_path).await?.is_empty()
                {
                    return Ok(());
                }
            }
            Err(error)
        }
    }
}

fn contextual_error(error: &BcmrError, message: String) -> BcmrError {
    if matches!(error, BcmrError::Io(error) if error.kind() == std::io::ErrorKind::Interrupted) {
        BcmrError::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            message,
        ))
    } else {
        invalid(message)
    }
}

fn parent_and_name(path: &str) -> Result<(&str, &str), BcmrError> {
    let (parent, name) = match path.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some(parts) => parts,
        None => (".", path),
    };
    if name.is_empty() || matches!(name, "." | "..") || name.contains('\0') {
        return Err(invalid(
            "SFTP move cleanup requires a named source, not a root or dot directory",
        ));
    }
    Ok((parent, name))
}

fn join_path(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches('/'))
}

fn changed(entry: &Snapshot, reason: impl std::fmt::Display) -> BcmrError {
    invalid(format!(
        "SFTP source '{}' changed after capture; move cleanup refused: {reason}",
        entry.source
    ))
}

#[cfg(test)]
mod tests;
