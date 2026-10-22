use std::borrow::Cow;

use crate::core::error::BcmrError;
use crate::core::protocol::Message;

use super::{FileTransfer, ServeClient};

/// SSH sessions serve the filesystem root, but scp-style relative paths still
/// name files under the remote account's home directory.
pub(super) struct RemotePaths {
    target: String,
    home: Option<String>,
}

impl RemotePaths {
    pub(super) fn new(target: &str) -> Self {
        Self {
            target: target.to_owned(),
            home: None,
        }
    }

    async fn resolve(&mut self, path: &str) -> Result<String, BcmrError> {
        if self.home.is_none() {
            // This is already an SSH/serve connection. Preserve the exact HOME
            // spelling: whitespace can be part of a POSIX directory name.
            let output = crate::core::remote::metadata_command(
                &self.target,
                "printf '%s' \"$HOME\"".to_owned(),
            )
            .await?;
            let home = String::from_utf8(output)
                .map_err(|_| BcmrError::InvalidInput("remote home is not valid UTF-8".into()))?;
            if !home.starts_with('/') || home.contains('\0') {
                return Err(BcmrError::InvalidInput(
                    "remote home must be an absolute POSIX path without null bytes".into(),
                ));
            }
            self.home = Some(home);
        }
        let home = self.home.as_deref().expect("remote home was resolved");
        // Use remote POSIX syntax even when the client runs on Windows. Keep
        // `..` intact so the server's existing validation continues to reject it.
        Ok(format!("{}/{}", home.trim_end_matches('/'), path))
    }
}

impl ServeClient {
    pub(super) async fn normalize_message<'a>(
        &mut self,
        message: &'a Message,
    ) -> Result<Cow<'a, Message>, BcmrError> {
        let Some(paths) = self.remote_paths.as_mut() else {
            return Ok(Cow::Borrowed(message));
        };
        let path = match message {
            Message::List { path }
            | Message::Stat { path }
            | Message::Hash { path, .. }
            | Message::Get { path, .. }
            | Message::Put { path, .. }
            | Message::Mkdir { path }
            | Message::Resume { path }
            | Message::PutChunked { path, .. }
            | Message::GetChunked { path, .. }
            | Message::Truncate { path, .. } => path,
            _ => return Ok(Cow::Borrowed(message)),
        };
        if path.starts_with('/') {
            return Ok(Cow::Borrowed(message));
        }
        let resolved = paths.resolve(path).await?;
        // Only path requests are cloned; payload and control frames stay borrowed.
        let mut message = message.clone();
        match &mut message {
            Message::List { path }
            | Message::Stat { path }
            | Message::Hash { path, .. }
            | Message::Get { path, .. }
            | Message::Put { path, .. }
            | Message::Mkdir { path }
            | Message::Resume { path }
            | Message::PutChunked { path, .. }
            | Message::GetChunked { path, .. }
            | Message::Truncate { path, .. } => *path = resolved,
            _ => unreachable!("only path requests reach normalization"),
        }
        Ok(Cow::Owned(message))
    }

    pub(super) async fn normalize_transfer_paths(
        &mut self,
        files: &mut [FileTransfer],
    ) -> Result<(), BcmrError> {
        let Some(paths) = self.remote_paths.as_mut() else {
            return Ok(());
        };
        for file in files {
            if !file.remote.starts_with('/') {
                file.remote = paths.resolve(&file.remote).await?;
            }
        }
        Ok(())
    }
}
