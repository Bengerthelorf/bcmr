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

#[cfg(not(unix))]
pub(super) async fn create_symlink_replacing(
    _dst: &Path,
    _target: &Path,
) -> std::result::Result<(), BcmrError> {
    Err(BcmrError::InvalidInput(
        "symlink replication is not supported on this platform".into(),
    ))
}
