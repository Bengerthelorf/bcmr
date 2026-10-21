//! Captured, non-following remote move cleanup.
//!
//! This is a bounded concurrency contract, not a tree transaction: all selected
//! metadata is checked before cleanup, and each leaf is checked again after a
//! private rename. An uncooperative writer holding an open fd can still modify
//! the inode after that last check. No cleanup operation discovers more leaves.
//! Captured hardlink aliases must initially share the same full fingerprint.
//! After our own unlink, a surviving alias is checked for stable metadata and
//! supplies the inode's new ctime. An in-place edit with restored mtime during
//! that bounded rename/unlink-to-restat interval can be indistinguishable from
//! our ctime change; the next leaf again requires the full fingerprint.
use super::ssh_cmd::{shell_escape, ssh_command, ssh_error_message};
use super::{inventory_remote, RemotePath};
use crate::core::error::BcmrError;
use std::collections::{HashMap, HashSet};

const STAT_FUNCTIONS: &str = r#"
if LC_ALL=C stat -c '%d' . >/dev/null 2>&1; then
    style=gnu
elif LC_ALL=C stat -f '%d' . >/dev/null 2>&1; then
    style=bsd
else
    printf '%s\n' 'cannot read cleanup metadata with remote stat' >&2
    exit 1
fi
fingerprint() {
    if [ "$style" = gnu ]; then
        LC_ALL=C TZ=UTC stat -c '%d:%i:%f:%u:%g|%s|%y|%z' "$1"
    else
        LC_ALL=C TZ=UTC stat -f '%d:%i:%p:%u:%g|%z|%.9Fm|%.9Fc' "$1"
    fi
}
entry_id() {
    if [ "$style" = gnu ]; then
        LC_ALL=C stat -c '%d:%i' "$1"
    else
        LC_ALL=C stat -f '%d:%i' "$1"
    fi
}
kind_of() {
    if [ -L "$1" ]; then printf l
    elif [ -d "$1" ]; then printf d
    elif [ -f "$1" ]; then printf f
    else return 1
    fi
}
"#;

const SNAPSHOT: &str = r#"
for path do
    case "$path" in
        */*) parent=${path%/*}; name=${path##*/}; [ -n "$parent" ] || parent=/ ;;
        *) parent=.; name=$path ;;
    esac
    (
        case "$parent" in /*) ;; *) parent=./$parent ;; esac
        CDPATH= cd -P "$parent" || exit 1
        kind=$(kind_of "./$name") || { printf '%s\n' "cleanup source missing or unsupported: $path" >&2; exit 1; }
        metadata=$(fingerprint "./$name") || exit 1
        parent_id=$(entry_id .) || exit 1
        printf '%s\000%s\000%s\000' "$kind" "$metadata" "$parent_id"
        pwd -P || exit 1
        printf '\000'
    ) || exit 1
done
"#;

#[derive(Clone, Debug)]
struct Snapshot {
    source: RemotePath,
    kind: u8,
    metadata: String,
    parent_identity: String,
    physical_parent: String,
}

impl Snapshot {
    fn comparison(&self) -> &str {
        if self.kind == b'd' {
            self.metadata.split('|').next().unwrap()
        } else {
            &self.metadata
        }
    }

    fn stable_after_rename(&self) -> &str {
        self.metadata.rsplit_once('|').unwrap().0
    }

    fn inode_identity(&self) -> &str {
        let end = self.metadata.match_indices(':').nth(1).unwrap().0;
        &self.metadata[..end]
    }

    fn parent_and_name(&self) -> (&str, &str) {
        match self.source.path.rsplit_once('/') {
            Some(("", name)) => ("/", name),
            Some(parts) => parts,
            None => (".", &self.source.path),
        }
    }
}

pub struct RemoteCleanupPlan {
    entries: Vec<Snapshot>,
}

impl RemoteCleanupPlan {
    pub async fn capture(
        sources: &[RemotePath],
        excludes: &[regex::Regex],
    ) -> Result<Self, BcmrError> {
        crate::core::interrupt::check()?;
        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for source in sources {
            crate::core::interrupt::check()?;
            let mut source = source.clone();
            // Excluded roots are no-ops, including inaccessible ones.
            if crate::core::traversal::is_remote_entry_excluded(&source.path, "", excludes) {
                continue;
            }
            source.expand_tilde().await?;
            source.path = source.path.trim_end_matches('/').to_owned();
            source.reject_unsafe()?;
            if source.path.is_empty()
                || matches!(source.path.as_str(), "." | "./")
                || source.path.ends_with("/.")
            {
                return Err(BcmrError::InvalidInput(format!(
                    "remote move cleanup requires a named source, not '{}'",
                    source
                )));
            }
            for entry in inventory_remote(&source, true, excludes).await? {
                let remote = if entry.path.is_empty() {
                    source.clone()
                } else {
                    source.join(&entry.path)
                };
                if seen.insert((remote.ssh_target(), remote.path.clone())) {
                    selected.push(remote);
                }
            }
        }
        Ok(Self {
            entries: snapshots(&selected, "bcmr-cleanup-capture").await?,
        })
    }

    /// Validate the entire captured selection before deleting any entry.
    pub async fn validate(&self) -> Result<(), BcmrError> {
        crate::core::interrupt::read_only(self.validate_captured()).await
    }

    async fn validate_captured(&self) -> Result<(), BcmrError> {
        let sources = self
            .entries
            .iter()
            .map(|entry| entry.source.clone())
            .collect::<Vec<_>>();
        let observed = snapshots(&sources, "bcmr-cleanup-validate").await?;
        for (expected, actual) in self.entries.iter().zip(observed.iter()) {
            crate::core::interrupt::check()?;
            if expected.kind != actual.kind
                || expected.parent_identity != actual.parent_identity
                || expected.comparison() != actual.comparison()
            {
                return Err(BcmrError::InvalidInput(format!(
                    "source '{}' changed after it was captured; remote move cleanup refused",
                    expected.source
                )));
            }
        }
        Ok(())
    }

    pub async fn remove(&self) -> Result<(), BcmrError> {
        self.validate().await?;
        let mut next_alias = vec![None; self.entries.len()];
        let mut next_by_inode = HashMap::<_, usize>::new();
        for (index, entry) in self
            .entries
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| entry.kind != b'd')
        {
            crate::core::interrupt::check()?;
            let key = (entry.source.ssh_target(), entry.inode_identity());
            if let Some(next) = next_by_inode.insert(key, index) {
                let alias = &self.entries[next];
                // Never normalize a discrepancy already present at capture.
                if entry.kind != alias.kind || entry.metadata != alias.metadata {
                    return Err(BcmrError::InvalidInput(format!(
                        "captured hardlink sources '{}' and '{}' have different metadata; remote move cleanup refused",
                        entry.source, alias.source
                    )));
                }
                next_alias[index] = Some(next);
            }
        }
        let mut refreshed_metadata = HashMap::new();
        for (index, entry) in self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.kind != b'd')
        {
            let key = (entry.source.ssh_target(), entry.inode_identity());
            let expected = refreshed_metadata
                .get(&key)
                .map_or(entry.metadata.as_str(), String::as_str);
            let next = next_alias[index].map(|next| &self.entries[next]);
            if let Some(metadata) = remove_leaf(entry, expected, next).await? {
                refreshed_metadata.insert(key, metadata);
            } else {
                refreshed_metadata.remove(&key);
            }
        }
        for entry in self.entries.iter().rev().filter(|entry| entry.kind == b'd') {
            crate::core::interrupt::check()?;
            remove_directory(entry).await?;
        }
        crate::core::interrupt::check()?;
        Ok(())
    }
}

fn shell_command(script: &str, label: &str, arguments: &[&str]) -> String {
    let mut command = format!("sh -c '{}' {}", shell_escape(script), label);
    for argument in arguments {
        command.push_str(&format!(" '{}'", shell_escape(argument)));
    }
    command
}

fn valid_identity(value: &str) -> bool {
    let parts = value.split(':').collect::<Vec<_>>();
    parts.len() == 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_metadata(value: &str) -> bool {
    let fields = value.split('|').collect::<Vec<_>>();
    if fields.len() != 4
        || fields.iter().any(|field| field.is_empty())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii() && !byte.is_ascii_control())
    {
        return false;
    }
    let identity = fields[0].split(':').collect::<Vec<_>>();
    identity.len() == 5
        && identity.iter().enumerate().all(|(index, field)| {
            !field.is_empty()
                && field.bytes().all(|byte| {
                    if index == 2 {
                        byte.is_ascii_hexdigit()
                    } else {
                        byte.is_ascii_digit()
                    }
                })
        })
        && fields[1].bytes().all(|byte| byte.is_ascii_digit())
        && fields[2..].iter().all(|timestamp| {
            timestamp.rsplit_once('.').is_some_and(|(_, fraction)| {
                fraction.len() >= 9
                    && fraction.as_bytes()[..9].iter().all(u8::is_ascii_digit)
                    && !fraction.as_bytes().get(9).is_some_and(u8::is_ascii_digit)
            })
        })
}

async fn snapshots(sources: &[RemotePath], label: &str) -> Result<Vec<Snapshot>, BcmrError> {
    let mut snapshots = Vec::with_capacity(sources.len());
    let script = format!("{STAT_FUNCTIONS}\n{SNAPSHOT}");
    let mut start = 0;
    while start < sources.len() {
        crate::core::interrupt::check()?;
        let target = sources[start].ssh_target();
        let mut end = start + 1;
        while end < sources.len() && end - start < 64 && sources[end].ssh_target() == target {
            end += 1;
        }
        let batch = &sources[start..end];
        let arguments = batch
            .iter()
            .map(|source| source.path.as_str())
            .collect::<Vec<_>>();
        let output = ssh_command(&target)
            .kill_on_drop(true)
            .arg(shell_command(&script, label, &arguments))
            .stdin(std::process::Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            return Err(BcmrError::InvalidInput(ssh_error_message(
                &String::from_utf8_lossy(&output.stderr),
                &format!("Cannot validate captured move sources on '{target}'"),
            )));
        }
        let bytes = output.stdout.strip_suffix(&[0]).ok_or_else(|| {
            BcmrError::InvalidInput("cleanup metadata is missing its final NUL".into())
        })?;
        let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
        if fields.len() != batch.len() * 4 {
            return Err(BcmrError::InvalidInput(
                "cleanup metadata has an unexpected field count".into(),
            ));
        }
        for (source, fields) in batch.iter().zip(fields.as_chunks::<4>().0) {
            let text = |bytes: &[u8]| {
                String::from_utf8(bytes.to_vec())
                    .map_err(|_| BcmrError::InvalidInput("non-UTF-8 cleanup metadata".into()))
            };
            let kind = match fields[0] {
                b"d" => b'd',
                b"f" => b'f',
                b"l" => b'l',
                _ => {
                    return Err(BcmrError::InvalidInput(
                        "unsupported cleanup entry kind".into(),
                    ))
                }
            };
            let metadata = text(fields[1])?;
            let parent_identity = text(fields[2])?;
            let physical_parent = text(fields[3].strip_suffix(b"\n").ok_or_else(|| {
                BcmrError::InvalidInput("invalid cleanup parent directory framing".into())
            })?)?;
            if !valid_metadata(&metadata)
                || !valid_identity(&parent_identity)
                || !physical_parent.starts_with('/')
                || physical_parent.contains('\0')
            {
                return Err(BcmrError::InvalidInput(
                    "invalid captured cleanup fingerprint".into(),
                ));
            }
            snapshots.push(Snapshot {
                source: source.clone(),
                kind,
                metadata,
                parent_identity,
                physical_parent,
            });
        }
        start = end;
    }
    Ok(snapshots)
}

const REMOVE_LEAF: &str = r#"
parent=$1
name=$2
expected_parent=$3
expected_kind=$4
expected_metadata=$5
expected_stable=$6
shift 6
case "$parent" in /*) ;; *) parent=./$parent ;; esac
CDPATH= cd -P "$parent" || exit 1
[ "$(entry_id .)" = "$expected_parent" ] || { printf '%s\n' 'source parent changed; cleanup refused' >&2; exit 1; }
[ "$(kind_of "./$name")" = "$expected_kind" ] || { printf '%s\n' 'source kind changed; cleanup refused' >&2; exit 1; }
[ "$(fingerprint "./$name")" = "$expected_metadata" ] || { printf '%s\n' 'source changed; cleanup refused' >&2; exit 1; }
umask 077
quarantine=$(mktemp -d './.bcmr.move.XXXXXXXXXX') || exit 1
quarantine=${quarantine#./}
quarantine_id=$(entry_id "./$quarantine") || exit 1
printf 'quarantine\000%s\000' "$quarantine"
payload="./$quarantine/$name"
[ ! -L "./$quarantine" ] && [ -d "./$quarantine" ] && [ "$(entry_id "./$quarantine")" = "$quarantine_id" ] || exit 1
restore_or_retain() {
    # Restore a regular replacement using exact no-replace link(2). If its
    # namespace is occupied, or this is a directory/link with uncertain portable
    # restore semantics, retain the quarantine and report the recovery location.
    if [ ! -L "./$quarantine" ] && [ -d "./$quarantine" ] && [ "$(entry_id "./$quarantine")" = "$quarantine_id" ] && [ ! -L "$payload" ] && [ -f "$payload" ] && link "$payload" "./$name" 2>/dev/null; then
        rm -f "$payload" || exit 1
        rmdir "./$quarantine" || exit 1
        printf 'restored\000'
    else
        printf 'retained\000'
    fi
}
trap 'trap - HUP INT TERM PIPE; restore_or_retain; exit 1' HUP INT TERM PIPE
# The final operand is a new private directory, never a user-controlled target
# that mv can reinterpret as a container. The original parent is our pinned cwd.
if ! mv "./$name" "./$quarantine/"; then
    rmdir "./$quarantine" 2>/dev/null || true
    printf '%s\n' 'could not quarantine selected source; cleanup refused' >&2
    exit 1
fi
current_kind=$(kind_of "$payload") || current_kind=unknown
current_metadata=$(fingerprint "$payload") || current_metadata=
current_stable=${current_metadata%|*}
if [ "$current_kind" != "$expected_kind" ] || [ "$current_stable" != "$expected_stable" ]; then
    restore_or_retain
    printf '%s\n' 'source changed during quarantine; uncopied data was preserved' >&2
    exit 1
fi
printf 'ready\000'
IFS= read -r decision || decision=abort
if [ "$decision" != commit ]; then
    restore_or_retain
    printf '%s\n' 'source cleanup was not committed; uncopied data was preserved' >&2
    exit 1
fi
[ ! -L "./$quarantine" ] && [ -d "./$quarantine" ] && [ "$(entry_id "./$quarantine")" = "$quarantine_id" ] || exit 1
current_kind=$(kind_of "$payload") || current_kind=unknown
current_metadata=$(fingerprint "$payload") || current_metadata=
if [ "$current_kind" != "$expected_kind" ] || [ "${current_metadata%|*}" != "$expected_stable" ]; then
    restore_or_retain
    printf '%s\n' 'source changed while awaiting cleanup commit; uncopied data was preserved' >&2
    exit 1
fi
rm -f "$payload" || exit 1
rmdir "./$quarantine" || exit 1
printf 'removed\000'
if [ "$#" -ne 0 ]; then
    [ "$#" -eq 5 ] || exit 1
    next_parent=$1
    next_name=$2
    next_expected_parent=$3
    next_expected_kind=$4
    next_expected_stable=$5
    (
        case "$next_parent" in /*) ;; *) next_parent=./$next_parent ;; esac
        CDPATH= cd -P "$next_parent" || exit 1
        [ "$(entry_id .)" = "$next_expected_parent" ] || { printf '%s\n' 'hardlink parent changed; cleanup refused' >&2; exit 1; }
        [ "$(kind_of "./$next_name")" = "$next_expected_kind" ] || { printf '%s\n' 'hardlink kind changed; cleanup refused' >&2; exit 1; }
        next_metadata=$(fingerprint "./$next_name") || exit 1
        [ "${next_metadata%|*}" = "$next_expected_stable" ] || { printf '%s\n' 'hardlink source changed; cleanup refused' >&2; exit 1; }
        printf 'refreshed\000%s\000' "$next_metadata"
    ) || exit 1
fi
"#;

async fn remove_leaf(
    entry: &Snapshot,
    expected_metadata: &str,
    next: Option<&Snapshot>,
) -> Result<Option<String>, BcmrError> {
    crate::core::interrupt::check()?;
    let (parent, name) = entry.parent_and_name();
    let script = format!("{STAT_FUNCTIONS}\n{REMOVE_LEAF}");
    let kind = (entry.kind as char).to_string();
    let next_kind = next.map(|entry| (entry.kind as char).to_string());
    let mut arguments = vec![
        parent,
        name,
        &entry.parent_identity,
        &kind,
        expected_metadata,
        entry.stable_after_rename(),
    ];
    if let Some(next) = next {
        let (_, name) = next.parent_and_name();
        arguments.extend_from_slice(&[
            &next.physical_parent,
            name,
            &next.parent_identity,
            next_kind.as_deref().unwrap(),
            next.stable_after_rename(),
        ]);
    }
    let (output, decision_error) = guarded_leaf_command(entry, &script, &arguments).await?;
    let fields = output.stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
    let removed = fields.len() >= 3 && fields[0] == b"quarantine" && fields[2] == b"removed";
    if output.status.success() && removed && decision_error.is_none() {
        match next {
            None if fields.len() == 4 && fields[3].is_empty() => return Ok(None),
            Some(next)
                if fields.len() == 6 && fields[3] == b"refreshed" && fields[5].is_empty() =>
            {
                if let Ok(metadata) = std::str::from_utf8(fields[4]) {
                    if valid_metadata(metadata)
                        && metadata.rsplit_once('|').unwrap().0 == next.stable_after_rename()
                    {
                        return Ok(Some(metadata.to_owned()));
                    }
                }
            }
            _ => {}
        }
    }
    let recovery = if fields.first() == Some(&b"quarantine".as_slice()) {
        fields
            .get(1)
            .and_then(|name| std::str::from_utf8(name).ok())
            .filter(|name| {
                name.starts_with(".bcmr.move.")
                    && !name.contains('/')
                    && !name.chars().any(char::is_control)
            })
            .map(|name| format!("{}/{}", entry.physical_parent.trim_end_matches('/'), name))
    } else {
        None
    };
    let restored = fields.get(2) == Some(&b"restored".as_slice());
    let recovery_message = if removed {
        "This captured source entry was removed; cleanup stopped before the next entry.".to_owned()
    } else if restored {
        "The replacement source was restored without overwriting another entry.".to_owned()
    } else if let Some(recovery) = recovery {
        format!("If the source name is absent, recover preserved data from {recovery:?}; quarantine was not recursively removed.")
    } else {
        format!("The source was preserved, or recovery data may remain under {:?}/.bcmr.move.* if the SSH response was interrupted.", entry.physical_parent)
    };
    let message = format!(
        "Remote move cleanup failed for '{}': {}. {} {}",
        entry.source,
        String::from_utf8_lossy(&output.stderr).trim(),
        decision_error
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        recovery_message,
    );
    if matches!(decision_error, Some(BcmrError::Io(ref error)) if error.kind() == std::io::ErrorKind::Interrupted)
    {
        Err(BcmrError::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            message,
        )))
    } else {
        Err(BcmrError::InvalidInput(message))
    }
}

async fn guarded_leaf_command(
    entry: &Snapshot,
    script: &str,
    arguments: &[&str],
) -> Result<(std::process::Output, Option<BcmrError>), BcmrError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Keep the source shell alive through its restore/retain decision. Only
    // witness reads are cancellable while the shell waits at the commit gate.
    let mut child = ssh_command(&entry.source.ssh_target())
        .arg(shell_command(script, "bcmr-cleanup-leaf", arguments))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped cleanup stdin");
    let mut stdout = child.stdout.take().expect("piped cleanup stdout");
    let stderr = child.stderr.take().expect("piped cleanup stderr");
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let _ = stderr.take(64 * 1024).read_to_end(&mut bytes).await;
        bytes
    });
    let mut prefix = Vec::new();
    let mut fields = 0;
    let prefix_result: Result<(), std::io::Error> = async {
        while fields < 3 && prefix.len() <= 256 {
            let mut byte = [0];
            if stdout.read(&mut byte).await? == 0 {
                break;
            }
            prefix.push(byte[0]);
            fields += usize::from(byte[0] == 0);
        }
        Ok(())
    }
    .await;
    let ready = prefix_result.is_ok()
        && prefix.starts_with(b"quarantine\0")
        && prefix.ends_with(b"\0ready\0")
        && prefix.len() <= 256;
    let mut decision_error = if ready {
        prefix.truncate(prefix.len() - b"ready\0".len());
        crate::core::interrupt::read_only(super::move_delivery::validate_ssh(&entry.source))
            .await
            .err()
    } else {
        Some(BcmrError::InvalidInput(
            "remote cleanup did not reach its bounded commit gate".into(),
        ))
    };
    if decision_error.is_none() {
        decision_error = crate::core::interrupt::check().err();
    }
    let decision: &[u8] = if decision_error.is_none() {
        b"commit\n"
    } else {
        b"abort\n"
    };
    if let Err(error) = stdin.write_all(decision).await {
        if decision_error.is_none() {
            decision_error = Some(error.into());
        }
    }
    drop(stdin); // EOF is an abort if no complete decision reached the shell.
    let mut remainder = Vec::new();
    let read_result = stdout.take(64 * 1024 + 1).read_to_end(&mut remainder).await;
    if (read_result.is_err() || remainder.len() > 64 * 1024) && decision_error.is_none() {
        decision_error = Some(BcmrError::InvalidInput(
            "invalid cleanup response after decision".into(),
        ));
    }
    prefix.extend_from_slice(&remainder);
    let status = child.wait().await?;
    let stderr = stderr_task.await.unwrap_or_default();
    Ok((
        std::process::Output {
            status,
            stdout: prefix,
            stderr,
        },
        decision_error,
    ))
}

const REMOVE_DIRECTORY: &str = r#"
parent=$1
name=$2
expected_parent=$3
expected_identity=$4
case "$parent" in /*) ;; *) parent=./$parent ;; esac
CDPATH= cd -P "$parent" || exit 1
[ "$(entry_id .)" = "$expected_parent" ] || { printf '%s\n' 'source parent changed; directory cleanup refused' >&2; exit 1; }
[ ! -L "./$name" ] && [ -d "./$name" ] || { printf '%s\n' 'source directory changed; cleanup refused' >&2; exit 1; }
metadata=$(fingerprint "./$name") || exit 1
[ "${metadata%%|*}" = "$expected_identity" ] || { printf '%s\n' 'source directory changed; cleanup refused' >&2; exit 1; }
if diagnostic=$(LC_ALL=C rmdir "./$name" 2>&1); then
    printf 'removed\000'
else
    case "$diagnostic" in
        *': Directory not empty'|*': File exists') printf 'preserved\000' ;;
        *) printf '%s\n' "$diagnostic" >&2; exit 1 ;;
    esac
fi
"#;

async fn remove_directory(entry: &Snapshot) -> Result<(), BcmrError> {
    crate::core::interrupt::check()?;
    let (parent, name) = entry.parent_and_name();
    let script = format!("{STAT_FUNCTIONS}\n{REMOVE_DIRECTORY}");
    let arguments = [parent, name, &entry.parent_identity, entry.comparison()];
    let output = ssh_command(&entry.source.ssh_target())
        .arg(shell_command(&script, "bcmr-cleanup-directory", &arguments))
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    if output.status.success() && matches!(output.stdout.as_slice(), b"removed\0" | b"preserved\0")
    {
        return Ok(());
    }
    Err(BcmrError::InvalidInput(ssh_error_message(
        &String::from_utf8_lossy(&output.stderr),
        &format!("Cannot remove captured empty directory '{}'", entry.source),
    )))
}
