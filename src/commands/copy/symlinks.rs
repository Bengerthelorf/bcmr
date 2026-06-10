use crate::cli::Commands;
use crate::core::error::BcmrError;
use std::path::Path;

pub(super) fn check_symlink_overwrite(
    dst: &Path,
    cli: &Commands,
) -> std::result::Result<(), BcmrError> {
    let Ok(md) = dst.symlink_metadata() else {
        return Ok(());
    };
    // cp -P refuses to clobber a real directory with a symlink even under -f.
    let ft = md.file_type();
    if ft.is_dir() && !ft.is_symlink() {
        return Err(BcmrError::InvalidInput(format!(
            "cannot overwrite directory '{}' with a symlink",
            dst.display()
        )));
    }
    if !cli.is_force() {
        return Err(BcmrError::TargetExists(dst.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
pub(super) async fn create_symlink_replacing(
    dst: &Path,
    target: &Path,
) -> std::result::Result<(), BcmrError> {
    if dst.symlink_metadata().is_ok() {
        tokio::fs::remove_file(dst).await?;
    }
    let dst = dst.to_path_buf();
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || std::os::unix::fs::symlink(&target, &dst))
        .await
        .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
        .map_err(BcmrError::Io)
}

#[cfg(windows)]
pub(super) async fn create_symlink_replacing(
    dst: &Path,
    target: &Path,
) -> std::result::Result<(), BcmrError> {
    if let Ok(md) = dst.symlink_metadata() {
        // std's is_dir() is false for directory symlinks, but DeleteFile still
        // refuses them — branch on the raw FILE_ATTRIBUTE_DIRECTORY bit.
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        if md.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
            tokio::fs::remove_dir(dst).await?;
        } else {
            tokio::fs::remove_file(dst).await?;
        }
    }
    let dst = dst.to_path_buf();
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // Windows distinguishes file vs directory symlinks; resolve a relative
        // target against the link's parent to probe what it points at.
        let resolved = if target.is_absolute() {
            target.clone()
        } else {
            dst.parent()
                .map_or_else(|| target.clone(), |p| p.join(&target))
        };
        let result = if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(&target, &dst)
        } else {
            std::os::windows::fs::symlink_file(&target, &dst)
        };
        result.map_err(|e| {
            const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
            if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) {
                BcmrError::InvalidInput(format!(
                    "cannot create symlink '{}': enable Windows Developer Mode \
                     or run elevated (symlink creation requires privilege)",
                    dst.display()
                ))
            } else {
                BcmrError::Io(e)
            }
        })
    })
    .await
    .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
}

#[cfg(not(any(unix, windows)))]
pub(super) async fn create_symlink_replacing(
    _dst: &Path,
    _target: &Path,
) -> std::result::Result<(), BcmrError> {
    Err(BcmrError::InvalidInput(
        "symlink replication is not supported on this platform".into(),
    ))
}
