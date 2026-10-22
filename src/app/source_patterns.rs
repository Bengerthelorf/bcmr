use crate::cli::Commands;
use crate::core::remote::{expand_remote_star, parse_remote_path};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

#[derive(Eq, PartialEq, Hash)]
enum SourceIdentity {
    Local(PathBuf),
    Remote(String, String),
}

fn source_identity(source: &Path) -> SourceIdentity {
    if let Some(remote) = parse_remote_path(&source.to_string_lossy()) {
        let path = remote
            .path
            .split('/')
            .filter(|component| !component.is_empty() && *component != ".")
            .collect::<Vec<_>>()
            .join("/");
        let path = if remote.path.starts_with('/') {
            format!("/{path}")
        } else {
            path
        };
        SourceIdentity::Remote(remote.ssh_target(), path)
    } else {
        // Normalize lexical spellings only; canonicalizing would conflate
        // distinct symbolic links and change which source a move removes.
        SourceIdentity::Local(
            source
                .components()
                .filter(|component| !matches!(component, Component::CurDir))
                .collect(),
        )
    }
}

/// Remove terminal native separators without changing the remaining spelling.
/// In particular, './host:link' must retain its explicit local-path prefix.
fn source_entry_path(source: &Path) -> PathBuf {
    // Native path parsing recognizes Unix, Windows drive and UNC roots.
    if source.file_name().is_none() {
        return source.to_path_buf();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = source.as_os_str().as_bytes();
        let end = bytes
            .iter()
            .rposition(|byte| *byte != b'/')
            .map_or(bytes.len(), |index| index + 1);
        PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end]))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut units: Vec<_> = source.as_os_str().encode_wide().collect();
        while units.last().is_some_and(|unit| matches!(*unit, 47 | 92)) {
            units.pop();
        }
        PathBuf::from(std::ffi::OsString::from_wide(&units))
    }
    #[cfg(not(any(unix, windows)))]
    {
        source
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(source.file_name().unwrap())
    }
}

/// A terminal separator makes lstat/rename follow a directory symlink on Unix.
/// Resolve that spelling once, before copy planning or move cleanup captures it.
/// Keep ordinary local directory spellings, including their exclusion paths.
fn preserved_source_path(source: &Path, excludes: &[regex::Regex]) -> PathBuf {
    if let Some(mut remote) = parse_remote_path(&source.to_string_lossy()) {
        // Remote exclusion matching already ignores terminal source slashes.
        if crate::core::traversal::is_remote_entry_excluded(&remote.path, "", excludes) {
            return source.to_path_buf();
        }
        let path = remote.path.trim_end_matches('/');
        // '/' (or '//') must retain its root rather than become the login cwd.
        if path.is_empty() || path.len() == remote.path.len() {
            return source.to_path_buf();
        }
        remote.path = path.to_owned();
        return PathBuf::from(remote.display());
    }

    if !crate::core::destination::has_trailing_separator(source)
        || crate::core::traversal::is_copy_excluded(source, source, excludes)
    {
        return source.to_path_buf();
    }
    let entry = source_entry_path(source);
    if entry
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        entry
    } else {
        source.to_path_buf()
    }
}

/// Resolve one source snapshot for planning, fan-out and successful move cleanup.
pub(crate) async fn expand_copy_move_sources(command: &mut Commands) -> Result<()> {
    if !matches!(command, Commands::Copy { .. } | Commands::Move { .. }) {
        return Ok(());
    }
    let preserve_links = command.is_no_deref() || matches!(command, Commands::Move { .. });
    let excludes = command.compile_excludes()?;
    let args = match command {
        Commands::Copy { args, .. } | Commands::Move { args, .. } => args,
        _ => return Ok(()),
    };
    let count = if args.to.is_empty() {
        args.paths.len().saturating_sub(1)
    } else {
        args.paths.len()
    };
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();
    for source in &args.paths[..count] {
        let remote = parse_remote_path(&source.to_string_lossy());
        let has_star = remote.as_ref().map_or_else(
            || source.as_os_str().as_encoded_bytes().contains(&b'*'),
            |remote| remote.path.contains('*'),
        );
        let matches = if !has_star {
            vec![source.clone()]
        } else if let Some(remote) = remote {
            expand_remote_star(remote)
                .await
                .with_context(|| format!("Source pattern '{}'", source.display()))?
                .into_iter()
                .map(|remote| PathBuf::from(remote.display()))
                .collect()
        } else {
            crate::core::star::expand_local(source)
                .with_context(|| format!("Source pattern '{}'", source.display()))?
        };
        if matches.is_empty() {
            bail!("No matches for source pattern '{}'", source.display());
        }
        for matched in matches {
            let matched = if preserve_links {
                preserved_source_path(&matched, &excludes)
            } else {
                matched
            };
            if seen.insert(source_identity(&matched)) {
                expanded.push(matched);
            }
        }
    }
    expanded.extend_from_slice(&args.paths[count..]);
    args.paths = expanded;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn preserved_local_sources_keep_roots_and_ordinary_directory_spellings() {
        for spelling in ["/", "//", "///", "./", "../"] {
            assert_eq!(
                preserved_source_path(Path::new(spelling), &[]).as_os_str(),
                std::ffi::OsStr::new(spelling)
            );
        }
        let directory = tempfile::tempdir().unwrap();
        let spelling = format!("{}///", directory.path().display());
        assert_eq!(
            preserved_source_path(Path::new(&spelling), &[]).as_os_str(),
            std::ffi::OsStr::new(&spelling)
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_separator_trimming_preserves_explicit_local_and_raw_name_spellings() {
        use std::os::unix::ffi::OsStrExt;
        for (spelling, expected) in [
            (b"./host:link///".as_slice(), b"./host:link".as_slice()),
            (b"./tree//link/".as_slice(), b"./tree//link".as_slice()),
            (b"/", b"/"),
            (b"///", b"///"),
            (b"./name-\xff/", b"./name-\xff"),
        ] {
            assert_eq!(
                source_entry_path(Path::new(std::ffi::OsStr::from_bytes(spelling))).as_os_str(),
                std::ffi::OsStr::from_bytes(expected)
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn preserved_local_sources_keep_drive_and_unc_roots() {
        for spelling in [
            r"C:\",
            "C:/",
            r"\\server\share\",
            r"\\?\C:\",
            r"\\?\UNC\server\share\",
        ] {
            assert_eq!(
                source_entry_path(Path::new(spelling)).as_os_str(),
                std::ffi::OsStr::new(spelling)
            );
        }
    }

    #[test]
    fn preserved_remote_sources_keep_roots_and_trim_only_source_separators() {
        for (spelling, expected) in [
            ("host:/", "host:/"),
            ("host://", "host://"),
            ("host:~/", "host:~"),
            ("host:tree/link///", "host:tree/link"),
            ("host:/tree//link/", "host:/tree//link"),
            (
                "scp://user@host:2222/tree/link///",
                "scp://user@host:2222/tree/link",
            ),
        ] {
            assert_eq!(
                preserved_source_path(Path::new(spelling), &[]).as_os_str(),
                std::ffi::OsStr::new(expected)
            );
        }
    }
}
