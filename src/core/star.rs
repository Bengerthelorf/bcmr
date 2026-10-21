//! Single-component `*` matching. No shell evaluation or other glob syntax.

use crate::core::error::BcmrError;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

pub(crate) fn matches_component(pattern: &OsStr, name: &OsStr) -> bool {
    let pattern = pattern.as_encoded_bytes();
    let name = name.as_encoded_bytes();
    if name.starts_with(b".") && !pattern.starts_with(b".") {
        return false;
    }
    let (mut p, mut n) = (0, 0);
    let mut star = None;
    let mut retry = 0;
    while n < name.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = n;
        } else if p < pattern.len() && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some(s) = star {
            retry += 1;
            n = retry;
            p = s + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn exists(path: &Path) -> Result<bool, BcmrError> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

/// Expand stars in local source components, retaining exact existing names.
pub fn expand_local(source: &Path) -> Result<Vec<PathBuf>, BcmrError> {
    if !source.as_os_str().as_encoded_bytes().contains(&b'*')
        // Windows forbids literal '*' names; its metadata API can reject a
        // pattern as an invalid filename before enumeration gets a chance.
        || (!cfg!(windows) && exists(source)?)
    {
        return Ok(vec![source.to_path_buf()]);
    }
    // Retain local identity for discovered names such as `host:file`; later
    // transfer dispatch must not reinterpret them as remote source arguments.
    let mut candidates = vec![PathBuf::from(".")];
    for component in source.components() {
        match component {
            Component::Normal(pattern) if pattern.as_encoded_bytes().contains(&b'*') => {
                let mut matched = Vec::new();
                for base in candidates {
                    let directory = if base.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        base.as_path()
                    };
                    let entries = match std::fs::read_dir(directory) {
                        Ok(entries) => entries,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                            ) =>
                        {
                            continue
                        }
                        Err(error) => return Err(error.into()),
                    };
                    for entry in entries {
                        let entry = entry?;
                        if matches_component(pattern, &entry.file_name()) {
                            matched.push(base.join(entry.file_name()));
                        }
                    }
                }
                candidates = matched;
            }
            component => {
                for base in &mut candidates {
                    base.push(component.as_os_str());
                }
            }
        }
    }
    let spelling = source.as_os_str().as_encoded_bytes();
    let directory_only = spelling
        .last()
        .is_some_and(|last| *last == b'/' || (cfg!(windows) && *last == b'\\'))
        || spelling.ends_with(b"/.")
        || (cfg!(windows) && spelling.ends_with(b"\\."));
    let mut matched = Vec::new();
    for candidate in candidates {
        if exists(&candidate)? && (!directory_only || candidate.is_dir()) {
            matched.push(candidate);
        }
    }
    matched.sort();
    Ok(matched)
}
