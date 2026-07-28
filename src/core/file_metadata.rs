use crate::core::error::BcmrError;
use std::fs::File;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortableFileMetadata {
    pub atime_seconds: i64,
    pub atime_nanoseconds: u32,
    pub mtime_seconds: i64,
    pub mtime_nanoseconds: u32,
    pub mode: u32,
}

impl PortableFileMetadata {
    pub(crate) fn apply_to(self, file: &File) -> Result<(), BcmrError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(self.mode & 0o0777))?;
        }

        let atime = filetime::FileTime::from_unix_time(self.atime_seconds, self.atime_nanoseconds);
        let mtime = filetime::FileTime::from_unix_time(self.mtime_seconds, self.mtime_nanoseconds);
        filetime::set_file_handle_times(file, Some(atime), Some(mtime))?;
        Ok(())
    }
}
