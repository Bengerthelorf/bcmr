mod attrs;
mod ops;
mod resume;
mod ssh_cmd;
mod transfer;

#[allow(unused_imports)]
pub use attrs::{apply_remote_attrs_locally, preserve_remote_attrs, verify_remote_file};
#[allow(unused_imports)]
pub use ops::{
    complete_remote_path, remote_file_hash, remote_file_size, remote_list_files, remote_stat,
    remote_total_size, validate_ssh_connection,
};
pub use resume::{check_resume_state, ResumeDecision};
pub use transfer::{
    download_directory, download_file, ensure_remote_tree, upload_directory, upload_file,
};

pub fn set_ssh_compression(enabled: bool) {
    ssh_cmd::SSH_COMPRESS.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Debug, Clone)]
pub struct RemotePath {
    pub user: Option<String>,
    pub host: String,
    pub path: String,
}

impl RemotePath {
    pub fn ssh_target(&self) -> String {
        match &self.user {
            Some(user) => format!("{}@{}", user, self.host),
            None => self.host.clone(),
        }
    }

    pub fn display(&self) -> String {
        format!("{}:{}", self.ssh_target(), self.path)
    }

    pub fn join(&self, subpath: &str) -> Self {
        Self {
            user: self.user.clone(),
            host: self.host.clone(),
            path: format!("{}/{}", self.path, subpath),
        }
    }
}

impl std::fmt::Display for RemotePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display())
    }
}

pub fn parse_remote_path(s: &str) -> Option<RemotePath> {
    if s.starts_with('/')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('~')
        || s == "."
        || s == ".."
    {
        return None;
    }

    if s.len() >= 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':' {
        let colon_pos = s.find(':')?;
        if colon_pos == 1 {
            return None;
        }
    }

    let colon_pos = s.find(':')?;
    if colon_pos == 0 {
        return None;
    }

    let host_part = &s[..colon_pos];
    let path_part = &s[colon_pos + 1..];

    if host_part.contains('/') || host_part.contains(' ') {
        return None;
    }

    let (user, host) = if let Some(at_pos) = host_part.find('@') {
        let user = &host_part[..at_pos];
        let host = &host_part[at_pos + 1..];
        if user.is_empty() || host.is_empty() {
            return None;
        }
        (Some(user.to_string()), host.to_string())
    } else {
        (None, host_part.to_string())
    };

    let path = if path_part.is_empty() {
        ".".to_string()
    } else {
        path_part.to_string()
    };

    Some(RemotePath { user, host, path })
}

#[derive(Debug)]
pub struct RemoteFileInfo {
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone, Default)]
pub struct TransferOptions {
    pub preserve: bool,
    pub verify: bool,
    pub resume: bool,
    pub strict: bool,
    pub append: bool,
}

pub type RemoteTransferOptions = TransferOptions;

pub struct TransferCallbacks<'a> {
    pub on_progress: &'a (dyn Fn(u64) + Send + Sync),
    pub on_skip: &'a (dyn Fn(u64) + Send + Sync),
    pub on_new_file: &'a (dyn Fn(&str, u64) + Send + Sync),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_remote_path() {
        let r = parse_remote_path("user@host:/path/to/file").unwrap();
        assert_eq!(r.user, Some("user".to_string()));
        assert_eq!(r.host, "host");
        assert_eq!(r.path, "/path/to/file");

        let r = parse_remote_path("host:file.txt").unwrap();
        assert_eq!(r.user, None);
        assert_eq!(r.host, "host");
        assert_eq!(r.path, "file.txt");

        let r = parse_remote_path("user@192.168.1.1:").unwrap();
        assert_eq!(r.path, ".");

        assert!(parse_remote_path("/absolute/path").is_none());
        assert!(parse_remote_path("./relative/path").is_none());
        assert!(parse_remote_path("../parent/path").is_none());
        assert!(parse_remote_path("~/home/path").is_none());
        assert!(parse_remote_path(".").is_none());
        assert!(parse_remote_path("..").is_none());

        assert!(parse_remote_path("C:\\Users\\file").is_none());
        assert!(parse_remote_path("D:file").is_none());

        assert!(parse_remote_path(":path").is_none());
        assert!(parse_remote_path("@host:path").is_none());
        assert!(parse_remote_path("user@:path").is_none());
    }

    #[test]
    fn test_parse_remote_path_with_spaces_in_host() {
        assert!(parse_remote_path("host name:path").is_none());
    }

    #[test]
    fn test_parse_remote_path_with_slash_in_host() {
        assert!(parse_remote_path("host/name:path").is_none());
    }

    #[test]
    fn test_remote_path_ssh_target() {
        let r = RemotePath {
            user: Some("alice".to_string()),
            host: "example.com".to_string(),
            path: "/data".to_string(),
        };
        assert_eq!(r.ssh_target(), "alice@example.com");

        let r2 = RemotePath {
            user: None,
            host: "example.com".to_string(),
            path: "/data".to_string(),
        };
        assert_eq!(r2.ssh_target(), "example.com");
    }

    #[test]
    fn test_remote_path_display() {
        let r = RemotePath {
            user: Some("bob".to_string()),
            host: "srv".to_string(),
            path: "/tmp/f".to_string(),
        };
        assert_eq!(r.display(), "bob@srv:/tmp/f");
    }

    #[test]
    fn test_remote_path_join() {
        let r = RemotePath {
            user: None,
            host: "h".to_string(),
            path: "/base".to_string(),
        };
        let joined = r.join("sub/file.txt");
        assert_eq!(joined.path, "/base/sub/file.txt");
        assert_eq!(joined.host, "h");
    }

    #[test]
    fn test_shell_escape_no_quotes() {
        assert_eq!(super::ssh_cmd::shell_escape("simple"), "simple");
    }

    #[test]
    fn test_shell_escape_with_quotes() {
        assert_eq!(super::ssh_cmd::shell_escape("it's"), "it'\\''s");
    }
}
