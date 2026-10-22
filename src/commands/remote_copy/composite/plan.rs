//! Composite source inventory and mutation-free destination validation.
use super::super::endpoint::{Endpoint, Kind};
use crate::cli::Commands;
use crate::commands::copy::FileToOverwrite;
use crate::commands::copy::SymlinkKind;
use crate::core::error::BcmrError;
use crate::core::file_metadata::PortableFileMetadata;
use crate::core::remote::{self, backend, sftp_ops};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub(super) struct Entry {
    pub(super) source: Endpoint,
    pub(super) target: Endpoint,
    pub(super) kind: Kind,
    pub(super) size: u64,
    pub(super) metadata: Option<PortableFileMetadata>,
    pub(super) link: Option<String>,
    pub(super) link_kind: SymlinkKind,
}

pub(crate) async fn required(sources: &[PathBuf], dest: &Path) -> Result<bool> {
    let remote_dest = remote::parse_remote_path(&dest.to_string_lossy());
    let mut local = false;
    let mut remote = false;
    let mut sftp = false;
    for path in sources.iter().chain(std::iter::once(&dest.to_owned())) {
        if let Some(endpoint) = remote::parse_remote_path(&path.to_string_lossy()) {
            if sources.contains(path) {
                remote = true;
            }
            sftp |= backend::is_sftp(&endpoint.ssh_target()).await?;
        } else if sources.contains(path) {
            local = true;
        }
    }
    Ok(sftp || (local && remote) || (remote && remote_dest.is_some()))
}

async fn inventory(
    source: &Endpoint,
    base: &Endpoint,
    args: &Commands,
    excludes: &[regex::Regex],
) -> Result<Vec<Entry>> {
    let preserve_links = args.is_no_deref() || matches!(args, Commands::Move { .. });
    let mut entries = Vec::new();
    match source {
        Endpoint::Local(root) => {
            for selected in crate::core::traversal::copy_walk(
                root,
                args.is_recursive(),
                0,
                preserve_links,
                excludes,
            ) {
                let selected = selected?;
                let kind = if selected.file_type().is_symlink() {
                    Kind::Link
                } else if selected.file_type().is_dir() {
                    Kind::Directory
                } else if selected.file_type().is_file() {
                    Kind::File
                } else {
                    bail!("unsupported source '{}'", selected.path().display());
                };
                if kind == Kind::Directory && !args.is_recursive() {
                    bail!(
                        "Source '{}' is a directory. Use -r flag for recursive copy.",
                        root.display()
                    );
                }
                let relative = selected
                    .path()
                    .strip_prefix(root)?
                    .components()
                    .map(|part| {
                        part.as_os_str().to_str().ok_or_else(|| {
                            anyhow::anyhow!("non-UTF-8 local name cannot be transferred remotely")
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join("/");
                let target = if relative.is_empty() {
                    base.clone()
                } else {
                    base.join(&relative)?
                };
                let selected_source = Endpoint::Local(selected.path().to_owned());
                let metadata = if args.is_preserve() && kind != Kind::Link {
                    Some(selected_source.metadata().await?)
                } else {
                    None
                };
                let link = if kind == Kind::Link {
                    Some(
                        std::fs::read_link(selected.path())?
                            .to_str()
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "non-UTF-8 symbolic link target cannot be transferred remotely"
                                )
                            })?
                            .to_owned(),
                    )
                } else {
                    None
                };
                let link_kind = if kind == Kind::Link {
                    super::super::links::local_link_kind(&selected.path().symlink_metadata()?)
                } else {
                    SymlinkKind::File
                };
                entries.push(Entry {
                    source: selected_source,
                    target,
                    kind,
                    size: if kind == Kind::File {
                        selected.metadata()?.len()
                    } else {
                        0
                    },
                    metadata,
                    link,
                    link_kind,
                });
            }
        }
        Endpoint::Ssh(root) | Endpoint::Sftp(root) => {
            for selected in remote::inventory_remote(root, preserve_links, excludes).await? {
                let kind = if selected.link_target.is_some() {
                    Kind::Link
                } else if selected.is_dir {
                    Kind::Directory
                } else {
                    Kind::File
                };
                if kind == Kind::Directory && !args.is_recursive() {
                    bail!("Source '{root}' is a directory. Use -r flag for recursive copy.");
                }
                let selected_source = if selected.path.is_empty() {
                    source.clone()
                } else {
                    source.join(&selected.path)?
                };
                let target = if selected.path.is_empty() {
                    base.clone()
                } else {
                    base.join(&selected.path)?
                };
                let metadata = if args.is_preserve() && kind != Kind::Link {
                    Some(selected_source.metadata().await?)
                } else {
                    None
                };
                #[cfg(windows)]
                let link_kind = if kind == Kind::Link {
                    match &selected_source {
                        Endpoint::Ssh(remote) | Endpoint::Sftp(remote)
                            if remote::remote_path_is_directory(remote).await =>
                        {
                            SymlinkKind::Directory
                        }
                        _ => SymlinkKind::File,
                    }
                } else {
                    SymlinkKind::File
                };
                #[cfg(not(windows))]
                let link_kind = SymlinkKind::File;
                entries.push(Entry {
                    source: selected_source,
                    target,
                    kind,
                    size: selected.size,
                    metadata,
                    link: selected.link_target,
                    link_kind,
                });
            }
        }
    }
    Ok(entries)
}

fn normalize_remote_absolute(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

fn remote_of(endpoint: &Endpoint) -> Option<&remote::RemotePath> {
    match endpoint {
        Endpoint::Ssh(remote) | Endpoint::Sftp(remote) => Some(remote),
        _ => None,
    }
}

#[derive(Default)]
struct Location {
    leaf: String,
    referent: Option<String>,
    boot: Option<String>,
    root_id: Option<String>,
    entry_id: Option<String>,
    referent_id: Option<String>,
}

async fn sftp_canonical_directory(
    client: &mut crate::core::sftp::SftpClient,
    path: &str,
) -> Result<String> {
    let mut probe = if path.is_empty() {
        ".".to_owned()
    } else {
        path.trim_end_matches('/').to_owned()
    };
    if probe.is_empty() {
        probe = "/".to_owned();
    }
    let mut suffix = Vec::new();
    loop {
        match client.realpath(&probe).await {
            Ok(parent) => {
                let parent = sftp_ops::utf8(parent, "canonical remote parent")?;
                suffix.reverse();
                return Ok(normalize_remote_absolute(&format!(
                    "{}/{}",
                    parent.trim_end_matches('/'),
                    suffix.join("/")
                )));
            }
            Err(error) if backend::missing(&error) => {
                if probe == "/" || probe == "." {
                    return Err(error.into());
                }
                let (parent, leaf) = probe.rsplit_once('/').unwrap_or((".", &probe));
                if leaf.is_empty() {
                    return Err(error.into());
                }
                suffix.push(leaf.to_owned());
                probe = if parent.is_empty() {
                    "/".to_owned()
                } else {
                    parent.to_owned()
                };
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn location(endpoint: &Endpoint) -> Result<Location> {
    if let Endpoint::Local(path) = endpoint {
        let absolute = std::path::absolute(path)?;
        let component_name = |path: &Path| match path.components().next_back() {
            Some(std::path::Component::Normal(name)) => Some(name.to_owned()),
            Some(std::path::Component::ParentDir) => Some(std::ffi::OsString::from("..")),
            Some(std::path::Component::CurDir) => Some(std::ffi::OsString::from(".")),
            _ => None,
        };
        let mut parent = absolute.parent().unwrap_or(Path::new("/")).to_owned();
        let mut suffix = vec![component_name(&absolute).unwrap_or_default()];
        while !parent.exists() {
            suffix.push(
                component_name(&parent)
                    .ok_or_else(|| anyhow::anyhow!("cannot resolve destination parent"))?,
            );
            if !parent.pop() {
                bail!("cannot resolve destination parent");
            }
        }
        let mut leaf = parent.canonicalize()?;
        for part in suffix.into_iter().rev() {
            if part == ".." {
                leaf.pop();
            } else if part != "." {
                leaf.push(part);
            }
        }
        #[cfg(unix)]
        let identity = |path: &Path, follow: bool| {
            use std::os::unix::fs::MetadataExt;
            (if follow {
                path.metadata()
            } else {
                path.symlink_metadata()
            })
            .ok()
            .map(|metadata| format!("{}:{}", metadata.dev(), metadata.ino()))
        };
        #[cfg(unix)]
        let (root_id, entry_id, referent_id) = (
            identity(Path::new("/"), true),
            identity(path, false),
            identity(path, true),
        );
        #[cfg(not(unix))]
        let (root_id, entry_id, referent_id) = (None, None, None);
        #[cfg(target_os = "linux")]
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|value| value.trim().to_owned());
        #[cfg(target_os = "macos")]
        let boot = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", "kern.bootsessionuuid"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|value| value.trim().to_owned());
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let boot = None;
        return Ok(Location {
            leaf: leaf.to_string_lossy().into_owned(),
            referent: path
                .canonicalize()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            boot,
            root_id,
            entry_id,
            referent_id,
        });
    }
    let remote = remote_of(endpoint).unwrap();
    if matches!(endpoint, Endpoint::Sftp(_)) {
        let session = backend::session(&remote.ssh_target()).await?;
        let mut client = session.lock().await;
        let path = remote.path.trim_end_matches('/');
        let (parent, name) = path.rsplit_once('/').map_or((".", path), |(parent, name)| {
            (if parent.is_empty() { "/" } else { parent }, name)
        });
        let parent = sftp_canonical_directory(&mut client, parent).await?;
        let referent = match client.realpath(&remote.path).await {
            Ok(path) => Some(sftp_ops::utf8(path, "canonical remote referent")?),
            Err(_) => None,
        };
        return Ok(Location {
            leaf: normalize_remote_absolute(&format!("{}/{name}", parent.trim_end_matches('/'))),
            referent,
            ..Location::default()
        });
    }
    let script = r#"
path=$1
case "$path" in /*) ;; *) path=./$path ;; esac
path=${path%/}
if LC_ALL=C stat -c '%d' . >/dev/null 2>&1; then style=gnu
elif LC_ALL=C stat -f '%d' . >/dev/null 2>&1; then style=bsd
else exit 1; fi
entry_id() {
    if [ "$style" = gnu ]; then LC_ALL=C stat -c '%d:%i' "$1"
    else LC_ALL=C stat -f '%d:%i' "$1"; fi
}
follow_id() {
    if [ "$style" = gnu ]; then LC_ALL=C stat -L -c '%d:%i' "$1"
    else LC_ALL=C stat -L -f '%d:%i' "$1"; fi
}
canonical_leaf() {
    candidate=$1
    parent=${candidate%/*}; name=${candidate##*/}
    [ -n "$parent" ] || parent=/
    suffix=/$name
    while [ ! -d "$parent" ]; do
        [ "$parent" != / ] && [ "$parent" != . ] || return 1
        leaf=${parent##*/}; [ -n "$leaf" ] || return 1
        suffix=/$leaf$suffix
        case "$parent" in */*) parent=${parent%/*}; [ -n "$parent" ] || parent=/ ;; *) parent=. ;; esac
    done
    parent=$(CDPATH= cd -P "$parent" && pwd -P && printf .) || return 1
    parent=${parent%.}; parent=${parent%?}
    printf '%s%s.' "${parent%/}" "$suffix"
}
physical=$(canonical_leaf "$path") || exit 1
physical=${physical%.}
referent=$physical
count=0
while [ -L "$referent" ]; do
    count=$((count+1)); [ "$count" -le 64 ] || { referent=; break; }
    link=$(readlink "$referent" && printf .) || { referent=; break; }
    link=${link%.}; link=${link%?}
    case "$link" in /*) probe=$link ;; *) probe=${referent%/*}/$link ;; esac
    referent=$(canonical_leaf "$probe") || { referent=; break; }
    referent=${referent%.}
done
if [ -n "$referent" ] && [ -d "$referent" ]; then
    referent=$(CDPATH= cd -P "$referent" && pwd -P && printf .) || exit 1
    referent=${referent%.}; referent=${referent%?}
fi
boot=
if [ -r /proc/sys/kernel/random/boot_id ]; then IFS= read -r boot < /proc/sys/kernel/random/boot_id
elif command -v sysctl >/dev/null 2>&1; then boot=$(sysctl -n kern.bootsessionuuid 2>/dev/null) || boot=; fi
root_id=$(entry_id /) || exit 1
entry=$(entry_id "$path" 2>/dev/null) || entry=
follow=$(follow_id "$path" 2>/dev/null) || follow=
printf '%s\000%s\000%s\000%s\000%s\000%s\000' "$physical" "$referent" "$boot" "$root_id" "$entry" "$follow"
"#;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let response = remote::metadata_command(
        &remote.ssh_target(),
        format!(
            "sh -c {} bcmr-relay-identity {}",
            quote(script),
            quote(&remote.path)
        ),
    )
    .await?;
    let fields = response.split(|byte| *byte == 0).collect::<Vec<_>>();
    if fields.len() != 7 || !fields[6].is_empty() || response.len() > 64 * 1024 {
        bail!("invalid remote path identity response");
    }
    let value = |index: usize| -> Result<String> { Ok(String::from_utf8(fields[index].to_vec())?) };
    let optional = |index: usize| -> Result<Option<String>> {
        let text = value(index)?;
        Ok((!text.is_empty()).then_some(text))
    };
    Ok(Location {
        leaf: normalize_remote_absolute(&value(0)?),
        referent: optional(1)?.map(|path| normalize_remote_absolute(&path)),
        boot: optional(2)?,
        root_id: optional(3)?,
        entry_id: optional(4)?,
        referent_id: optional(5)?,
    })
}

async fn same_namespace(
    source: &remote::RemotePath,
    target: &remote::RemotePath,
    src: &Location,
    dst: &Location,
) -> bool {
    if source.ssh_target() == target.ssh_target() {
        return true;
    }
    if src.root_id.is_none() || src.root_id != dst.root_id {
        return false;
    }
    if src.boot.is_some() && src.boot == dst.boot {
        return true;
    }
    // Some SSH servers do not expose a boot UUID. Qualify their inode data
    // with the authenticated server identity, never an alias or hostname alone.
    if src.boot.is_none() && dst.boot.is_none() {
        let left = crate::core::ssh_options::peer_identity(&source.ssh_target()).await;
        return left.is_some()
            && left == crate::core::ssh_options::peer_identity(&target.ssh_target()).await;
    }
    false
}

async fn same_location_namespace(
    source: &Endpoint,
    target: &Endpoint,
    src: &Location,
    dst: &Location,
) -> bool {
    if let (Some(source), Some(target)) = (remote_of(source), remote_of(target)) {
        return same_namespace(source, target, src, dst).await;
    }
    matches!((source, target), (Endpoint::Local(_), Endpoint::Local(_)))
        || (src.boot.is_some()
            && src.boot == dst.boot
            && src.root_id.is_some()
            && src.root_id == dst.root_id)
}

async fn validate_relationship(
    source: &Endpoint,
    target: &Endpoint,
    is_directory: bool,
) -> Result<()> {
    match (source, target) {
        (Endpoint::Local(source), Endpoint::Local(target)) => {
            let same = if is_directory {
                crate::commands::copy::is_dst_self_or_under_src(source, target)?
            } else {
                std::path::absolute(source)? == std::path::absolute(target)?
            };
            if same {
                bail!(
                    "destination '{}' is the source or is inside it",
                    target.display()
                );
            }
        }
        _ => {
            if !is_directory
                && remote_of(source)
                    .zip(remote_of(target))
                    .is_some_and(|(left, right)| left.ssh_target() != right.ssh_target())
            {
                return Ok(());
            }
            let src = location(source).await?;
            let dst = location(target).await?;
            if same_location_namespace(source, target, &src, &dst).await {
                let src = if is_directory {
                    src.referent.as_ref().unwrap_or(&src.leaf)
                } else {
                    &src.leaf
                };
                let dst = if is_directory {
                    dst.referent.as_ref().unwrap_or(&dst.leaf)
                } else {
                    &dst.leaf
                };
                if src == dst
                    || (is_directory && dst.starts_with(&format!("{}/", src.trim_end_matches('/'))))
                {
                    bail!(
                        "destination '{}' resolves to the source or is inside it",
                        target.display()
                    );
                }
            }
        }
    }
    Ok(())
}

pub(super) async fn validate_move_leaf(entry: &Entry) -> Result<()> {
    validate_same_file(&entry.source, &entry.target, entry.kind, "move").await
}

pub(super) async fn validate_regular_file_pair(source: &Endpoint, target: &Endpoint) -> Result<()> {
    validate_same_file(source, target, Kind::File, "transfer").await
}

pub(super) async fn validate_copy_leaf(entry: &Entry) -> Result<()> {
    if let (Endpoint::Local(source), Endpoint::Local(target)) = (&entry.source, &entry.target) {
        crate::commands::copy::refuse_same_file_copy(source, target, entry.kind == Kind::Link)?;
        return Ok(());
    }
    validate_same_file(&entry.source, &entry.target, entry.kind, "copy").await
}

async fn validate_same_file(
    source: &Endpoint,
    target: &Endpoint,
    kind: Kind,
    operation: &str,
) -> Result<()> {
    match (source, target) {
        (Endpoint::Local(source), Endpoint::Local(target)) => {
            crate::commands::r#move::refuse_same_file_move(source, target)?
        }
        (source, target) => {
            let src = location(source).await?;
            let dst = location(target).await?;
            if same_location_namespace(source, target, &src, &dst).await {
                let both_links = kind == Kind::Link && target.kind().await? == Some(Kind::Link);
                let same_leaf = src.leaf == dst.leaf;
                let same_entry =
                    !both_links && src.entry_id.is_some() && src.entry_id == dst.entry_id;
                let same_referent = !both_links
                    && ((src.referent.is_some() && src.referent == dst.referent)
                        || (src.referent_id.is_some() && src.referent_id == dst.referent_id));
                if same_leaf || same_entry || same_referent {
                    bail!(
                        "cannot {operation} '{}' onto the same file '{}'",
                        source.display(),
                        target.display()
                    );
                }
            }
        }
    }
    Ok(())
}

pub(super) async fn validate_link_destination(entry: &Entry) -> Result<()> {
    let Some(link) = &entry.link else {
        return Ok(());
    };
    let referent = match &entry.target {
        Endpoint::Local(path) => {
            Endpoint::Local(path.parent().unwrap_or(Path::new(".")).join(link))
        }
        Endpoint::Ssh(remote) | Endpoint::Sftp(remote) => {
            let mut referent = remote.clone();
            let parent = remote.path.rsplit_once('/').map_or(".", |(parent, _)| {
                if parent.is_empty() {
                    "/"
                } else {
                    parent
                }
            });
            referent.path = if link.starts_with('/') {
                link.clone()
            } else {
                format!("{parent}/{link}")
            };
            if matches!(entry.target, Endpoint::Sftp(_)) {
                Endpoint::Sftp(referent)
            } else {
                Endpoint::Ssh(referent)
            }
        }
    };
    let target = location(&entry.target).await?;
    let referent = location(&referent).await?;
    if referent.leaf == target.leaf || referent.referent.as_ref() == Some(&target.leaf) {
        bail!("symbolic link destination '{}' would resolve to itself; refusing to replace its payload", entry.target.display());
    }
    Ok(())
}

async fn preflight(entries: &[Entry], args: &Commands) -> Result<()> {
    let mut overwrites = Vec::new();
    let overwrite = args.is_force() || args.is_resume() || args.is_append() || args.is_strict();
    for entry in entries {
        if matches!(args, Commands::Move { .. }) {
            if let Some(metadata) = entry.metadata {
                if (entry.kind == Kind::File && metadata.mode & 0o400 == 0)
                    || (entry.kind == Kind::Directory && metadata.mode & 0o100 == 0)
                {
                    bail!("cannot validate final move destination with preserved permissions {:o} for '{}'; use copy -p and review source removal separately",metadata.mode,entry.source.display());
                }
            }
        }
        validate_link_destination(entry).await?;
        if matches!(args, Commands::Move { .. }) && entry.kind != Kind::Directory {
            validate_move_leaf(entry).await?;
        }
        let existing = entry.target.kind().await?;
        if !matches!(args, Commands::Move { .. })
            && overwrite
            && existing.is_some()
            && entry.kind != Kind::Directory
        {
            validate_copy_leaf(entry).await?;
        }
        if matches!(entry.target, Endpoint::Sftp(_))
            && existing == Some(Kind::File)
            && entry.kind == Kind::File
            && (args.is_resume() || args.is_append() || args.is_strict())
            && entry.source.hash(None).await? == entry.target.hash(None).await?
        {
            continue;
        }
        if let Endpoint::Sftp(remote) = &entry.target {
            if entry.kind == Kind::File
                && !backend::session(&remote.ssh_target())
                    .await?
                    .lock()
                    .await
                    .supports_hardlink()
            {
                bail!("SFTP server on '{}' lacks hardlink@openssh.com; cannot guarantee no-clobber file publication",remote.ssh_target());
            }
        }
        if entry.kind == Kind::Directory {
            if existing.is_some() && !entry.target.is_container().await? {
                bail!(
                    "destination '{}' is not a directory",
                    entry.target.display()
                );
            }
        } else if let Some(existing) = existing {
            if existing == Kind::Directory {
                bail!("destination '{}' is a directory", entry.target.display());
            }
            if !overwrite {
                return Err(BcmrError::TargetExists(entry.target.display().into()).into());
            }
            if matches!(entry.target, Endpoint::Sftp(_)) && !args.is_dry_run() {
                // v3 has no ACL/xattr read/copy or conditional inode exchange.
                // Do not weaken the existing atomic security contract.
                bail!("SFTP v3 cannot preserve an existing file's ACL/xattrs during atomic replacement of '{}'; use --remote-backend=ssh or a new destination path",entry.target.display());
            }
            if args.should_prompt_for_overwrite() {
                overwrites.push(FileToOverwrite {
                    path: entry.target.display().into(),
                    is_dir: false,
                });
            }
        }
        if let (Endpoint::Sftp(_), Some(metadata)) = (&entry.target, entry.metadata) {
            sftp_ops::wire_attrs(metadata)?;
        }
        if args.is_sync() {
            if !matches!(entry.target, Endpoint::Local(_)) {
                bail!("--sync cannot guarantee durable remote namespace publication with the composite SSH/SFTP backend; refusing before transfer");
            }
            #[cfg(not(unix))]
            bail!("--sync cannot guarantee durable local namespace publication on this platform; refusing before transfer");
        }
    }
    if args.should_prompt_for_overwrite() && !args.is_dry_run() && !overwrites.is_empty() {
        overwrites.sort_by(|a, b| a.path.cmp(&b.path));
        overwrites.dedup_by(|a, b| a.path == b.path);
        if !crate::app::prompts::confirm_overwrite(&overwrites)? {
            return Err(BcmrError::Cancelled.into());
        }
    }
    Ok(())
}

pub(super) async fn build(
    args: &Commands,
    sources: &[PathBuf],
    dest: &Path,
    excludes: &[regex::Regex],
) -> Result<Vec<Entry>> {
    let destination = Endpoint::parse(dest).await?;
    let container = destination.is_container().await?;
    if sources.len() > 1 && !container {
        bail!("multiple sources require a directory destination (existing or ending in '/')");
    }
    let mut entries = Vec::new();
    for path in sources {
        let source = Endpoint::parse(path).await?;
        let base = if container {
            destination.join(&source.basename()?)?
        } else {
            destination.clone()
        };
        let selected = inventory(&source, &base, args, excludes).await?;
        validate_relationship(
            &source,
            &base,
            selected
                .first()
                .is_some_and(|entry| entry.kind == Kind::Directory),
        )
        .await?;
        entries.extend(selected);
    }
    preflight(&entries, args).await?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_directory_link_kind_uses_directory_attribute_with_reparse_bit() {
        assert_eq!(
            crate::commands::copy::windows_symlink_kind_from_attributes(0x410),
            SymlinkKind::Directory
        );
        assert_eq!(
            crate::commands::copy::windows_symlink_kind_from_attributes(0x420),
            SymlinkKind::File
        );
    }

    #[test]
    fn link_resolution_normalizes_parent_segments_without_losing_unusual_names() {
        assert_eq!(
            normalize_remote_absolute("/home/dir/../payload"),
            "/home/payload"
        );
        assert_eq!(
            normalize_remote_absolute("/home/./snow 雪\nname"),
            "/home/snow 雪\nname"
        );
    }
}
