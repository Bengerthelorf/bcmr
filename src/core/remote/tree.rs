use super::ssh_cmd::{shell_escape, ssh_command, ssh_error_message};
use super::RemotePath;
use crate::core::error::BcmrError;
use crate::core::traversal::is_remote_entry_excluded;

#[derive(Clone, Debug)]
pub struct RemoteTreeEntry {
    /// Source-relative '/' path; the selected root is the empty string.
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    /// Present only for a preserved symlink; such entries are always leaves.
    pub link_target: Option<String>,
}

// A directory response carries non-following metadata for its immediate
// children. Regular leaves need no additional SSH process. Directory and link
// children are probed only after client-side exclusions select them. NUL
// framing preserves quotes, newlines and shell syntax on both find variants.
const PROBE: &str = r#"
case "$1" in /*) path=$1 ;; *) path=./$1 ;; esac
if [ "$2" = preserve ] && [ -L "$path" ]; then
    printf 'l\0000\000\000'
    readlink "$path" || exit 1
    printf '\000'
    exit 0
fi
if [ -d "$path" ]; then
    kind=d
elif [ -f "$path" ]; then
    kind=f
else
    printf '%s\n' "missing, broken, cyclic, or unsupported source: $path" >&2
    exit 1
fi
if size=$(LC_ALL=C stat -L -c '%s' "$path" 2>/dev/null); then stat_style=gnu
elif size=$(LC_ALL=C stat -L -f '%z' "$path"); then stat_style=bsd
else
    printf '%s\n' "cannot read source metadata: $path" >&2
    exit 1
fi
printf '%s\000%s\000' "$kind" "$size"
if [ "$kind" = d ]; then
    CDPATH= cd -P "$path" || exit 1
    pwd -P || exit 1
    printf '\000\000'
    if LC_ALL=C find . -maxdepth 0 -printf '' >/dev/null 2>&1; then
        # GNU find reads lstat metadata without following child links. Probe
        # support before emitting records: never fall back after partial output.
        LC_ALL=C find . -mindepth 1 -maxdepth 1 -printf '%P\0%y\0%s\0'
    else
        LC_ALL=C find . ! -name . -prune -exec sh -c '
            style=$1
            shift
            for child do
                if [ "$style" = gnu ]; then
                    metadata=$(LC_ALL=C stat -c "%s %F" "$child" 2>&1)
                    rc=$?
                else
                    metadata=$(LC_ALL=C stat -f "%z %HT" "$child" 2>&1)
                    rc=$?
                fi
                if [ "$rc" != 0 ]; then
                    # A failed/unsupported excluded entry must not reject the
                    # selection before the client can prune it.
                    printf "%s\000e\000%s\000" "${child#./}" "$metadata"
                    continue
                fi
                size=${metadata%% *}
                kind=${metadata#* }
                case "$kind" in
                    directory|Directory) kind=d ;;
                    "regular file"|"regular empty file"|"Regular File") kind=f ;;
                    "symbolic link"|"Symbolic Link") kind=l ;;
                    *) kind=o ;;
                esac
                printf "%s\000%s\000%s\000" "${child#./}" "$kind" "$size"
            done
        ' bcmr-tree-children "$stat_style" {} +
    fi
else
    printf '\000\000'
fi
"#;

struct Probe {
    size: u64,
    is_dir: bool,
    physical: String,
    link_target: Option<String>,
    children: Vec<ChildRecord>,
}

struct ChildRecord {
    name: Vec<u8>,
    kind: Vec<u8>,
    // Parse a regular leaf's size only after exclusions. For an 'e' record
    // this contains the portable stat diagnostic instead of a byte count.
    size: Vec<u8>,
}

fn invalid_listing(remote: &RemotePath, detail: &str) -> BcmrError {
    BcmrError::InvalidInput(format!("invalid source inventory for '{remote}': {detail}"))
}

async fn probe(remote: &RemotePath, no_deref: bool) -> Result<Probe, BcmrError> {
    let command = format!(
        "sh -c '{}' bcmr-tree '{}' {}",
        shell_escape(PROBE),
        shell_escape(&remote.path),
        if no_deref { "preserve" } else { "follow" },
    );
    let output = ssh_command(&remote.ssh_target())
        .arg(command)
        .output()
        .await?;
    if !output.status.success() {
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &String::from_utf8_lossy(&output.stderr),
            &format!("Cannot inventory source '{remote}'"),
        )));
    }
    let bytes = output
        .stdout
        .strip_suffix(&[0])
        .ok_or_else(|| invalid_listing(remote, "missing final NUL"))?;
    let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    if fields.len() < 4 {
        return Err(invalid_listing(remote, "incomplete entry"));
    }
    let text = |value: &[u8]| -> Result<String, BcmrError> {
        String::from_utf8(value.to_vec())
            .map_err(|_| invalid_listing(remote, "non-UTF-8 source name or link target"))
    };
    let size = text(fields[1])?
        .parse::<u64>()
        .map_err(|_| invalid_listing(remote, "invalid source size"))?;
    let is_dir = match fields[0] {
        b"d" => true,
        b"f" | b"l" => false,
        _ => return Err(invalid_listing(remote, "invalid source kind")),
    };
    let physical = if is_dir {
        text(
            fields[2]
                .strip_suffix(b"\n")
                .ok_or_else(|| invalid_listing(remote, "invalid physical directory"))?,
        )?
    } else {
        String::new()
    };
    let link_target = if fields[0] == b"l" {
        Some(text(fields[3].strip_suffix(b"\n").ok_or_else(|| {
            invalid_listing(remote, "invalid link target")
        })?)?)
    } else {
        None
    };
    if !(fields.len() - 4).is_multiple_of(3) {
        return Err(invalid_listing(remote, "incomplete child metadata"));
    }
    let mut children = Vec::new();
    for record in fields[4..].as_chunks::<3>().0 {
        let name = record[0];
        if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
            return Err(invalid_listing(remote, "invalid child name"));
        }
        children.push(ChildRecord {
            name: name.to_vec(),
            kind: record[1].to_vec(),
            size: record[2].to_vec(),
        });
    }
    if !is_dir && !children.is_empty() {
        return Err(invalid_listing(remote, "children on a leaf entry"));
    }
    children.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(Probe {
        size,
        is_dir,
        physical,
        link_target,
        children,
    })
}

pub async fn inventory_remote(
    root: &RemotePath,
    no_deref: bool,
    excludes: &[regex::Regex],
) -> Result<Vec<RemoteTreeEntry>, BcmrError> {
    if is_remote_entry_excluded(&root.path, "", excludes) {
        return Ok(Vec::new());
    }
    root.reject_unsafe()?;
    let mut entries = Vec::new();
    let mut pending = vec![(String::new(), Vec::<String>::new(), None::<ChildRecord>)];
    while let Some((relative, mut ancestors, cached)) = pending.pop() {
        if is_remote_entry_excluded(&root.path, &relative, excludes) {
            continue;
        }
        let remote = if relative.is_empty() {
            root.clone()
        } else {
            root.join(&relative)
        };
        if let Some(cached) = cached {
            // Match the same lossy spelling as local exclusions first. A
            // selected SSH path must then be exactly representable as UTF-8.
            std::str::from_utf8(&cached.name)
                .map_err(|_| invalid_listing(&remote, "non-UTF-8 source name"))?;
            match cached.kind.as_slice() {
                b"f" => {
                    let size = std::str::from_utf8(&cached.size)
                        .map_err(|_| invalid_listing(&remote, "invalid cached file size"))?
                        .parse::<u64>()
                        .map_err(|_| invalid_listing(&remote, "invalid cached file size"))?;
                    entries.push(RemoteTreeEntry {
                        path: relative,
                        size,
                        is_dir: false,
                        link_target: None,
                    });
                    continue;
                }
                b"d" | b"l" => {}
                b"e" => {
                    return Err(BcmrError::InvalidInput(format!(
                        "Cannot read source metadata for '{remote}': {}",
                        String::from_utf8_lossy(&cached.size)
                    )))
                }
                _ => {
                    return Err(BcmrError::InvalidInput(format!(
                        "unsupported source entry '{remote}'"
                    )))
                }
            }
        }
        let info = probe(&remote, no_deref).await?;
        if info.is_dir {
            if ancestors.contains(&info.physical) {
                return Err(BcmrError::InvalidInput(format!(
                    "symbolic-link directory cycle at '{remote}'"
                )));
            }
            ancestors.push(info.physical);
            for child in info.children.into_iter().rev() {
                let name = String::from_utf8_lossy(&child.name);
                let path = if relative.is_empty() {
                    name.into_owned()
                } else {
                    format!("{relative}/{name}")
                };
                pending.push((path, ancestors.clone(), Some(child)));
            }
        }
        entries.push(RemoteTreeEntry {
            path: relative,
            size: info.size,
            is_dir: info.is_dir,
            link_target: info.link_target,
        });
    }
    Ok(entries)
}

/// Publish a link entry without reading, modifying, or copying its target.
pub async fn create_remote_symlink(
    destination: &RemotePath,
    link_target: &str,
    overwrite: bool,
) -> Result<(), BcmrError> {
    let mut staging = super::staged_upload::StagedUpload::create(destination, overwrite).await?;
    let result = async {
        let output = ssh_command(&staging.remote().ssh_target())
            .arg(format!(
                "ln -s -- '{}' '{}'",
                shell_escape(link_target),
                shell_escape(&staging.remote().path)
            ))
            .output()
            .await?;
        if !output.status.success() {
            return Err(BcmrError::InvalidInput(ssh_error_message(
                &String::from_utf8_lossy(&output.stderr),
                "Cannot create staged symbolic link",
            )));
        }
        staging.prepare_symlink().await?;
        staging.commit().await
    }
    .await;
    staging.cleanup().await;
    result
}
