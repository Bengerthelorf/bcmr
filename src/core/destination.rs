use crate::core::error::BcmrError;
use std::path::{Path, PathBuf};

fn validate_remote_relative(relative: &str, windows: bool) -> Result<(), BcmrError> {
    for component in relative.split('/') {
        let invalid = component.is_empty()
            || matches!(component, "." | "..")
            || (windows && (component.contains(['\\', ':']) || component.ends_with(['.', ' '])));
        let reserved = if windows {
            let stem = component
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        } else {
            false
        };
        if invalid || reserved {
            return Err(BcmrError::InvalidInput(format!(
                "Remote filename '{relative}' cannot be represented safely as a local relative path"
            )));
        }
    }
    Ok(())
}

/// Validate remote POSIX names before joining them with native local paths.
pub fn remote_entry_path(base: &Path, relative: &str) -> Result<PathBuf, BcmrError> {
    validate_remote_relative(relative, cfg!(windows))?;
    Ok(base.join(relative))
}

/// Whether the spelling explicitly requests a directory on this platform.
/// Inspect the original spelling because Path components discard a final slash.
pub fn has_trailing_separator(path: &Path) -> bool {
    let last = path.as_os_str().as_encoded_bytes().last().copied();
    #[cfg(windows)]
    {
        matches!(last, Some(b'/' | b'\\'))
    }
    #[cfg(not(windows))]
    {
        last == Some(b'/')
    }
}

/// Existing directories and explicitly requested missing directories are parents.
/// An existing non-directory with a final separator remains an invalid target.
pub fn local_is_directory(path: &Path) -> Result<bool, BcmrError> {
    if path.is_dir() {
        return Ok(true);
    }
    if !has_trailing_separator(path) {
        return Ok(false);
    }
    // A trailing slash on a regular file can make metadata(path) report ENOTDIR.
    // Normalize components only for this probe, retaining the original CLI path.
    let without_trailing_separator: PathBuf = path.components().collect();
    match without_trailing_separator.metadata() {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(BcmrError::InvalidInput(format!(
            "Destination '{}' ends with a directory separator but is not a directory",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(BcmrError::Io(error)),
    }
}

#[cfg(test)]
mod remote_name_tests {
    use super::*;

    #[test]
    fn absolute_and_parent_entries_cannot_escape_on_any_platform() {
        for name in [
            "/outside",
            "../outside",
            "dir/../outside",
            "dir//file",
            "dir/./file",
        ] {
            assert!(validate_remote_relative(name, false).is_err(), "{name}");
            assert!(validate_remote_relative(name, true).is_err(), "{name}");
        }
    }

    #[test]
    fn windows_rejects_native_escape_and_device_names() {
        for name in [
            r"..\outside",
            r"C:\outside",
            "file:stream",
            "NUL",
            "dir/CON.txt",
            "LPT1",
            "file.",
            "file ",
        ] {
            assert!(validate_remote_relative(name, true).is_err(), "{name}");
        }
        assert!(validate_remote_relative("dir/普通文件.txt", true).is_ok());
    }

    #[test]
    fn unix_keeps_literal_backslash_colon_and_unicode_names() {
        for name in [
            r"..\outside",
            r"C:\outside",
            "dir/普通文件.txt",
            "line\nquote'file",
        ] {
            assert!(validate_remote_relative(name, false).is_ok(), "{name}");
        }
    }
}
