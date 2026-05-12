mod attrs;
mod ops;
mod resume;
mod ssh_cmd;
mod transfer;

#[allow(unused_imports)]
pub use attrs::{apply_remote_attrs_locally, preserve_remote_attrs, verify_remote_file};
#[allow(unused_imports)]
pub use ops::{
    complete_remote_path, expand_remote_tilde, remote_file_hash, remote_file_size,
    remote_list_files, remote_list_shallow, remote_path_is_directory, remote_remove, remote_stat,
    remote_total_size, resolve_remote_home, validate_ssh_connection,
};
pub use resume::{check_resume_state, ResumeDecision};
#[allow(unused_imports)]
pub(crate) use ssh_cmd::ssh_error_message;
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
        let host_display = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let target = match &self.user {
            Some(user) => format!("{}@{}", user, host_display),
            None => host_display,
        };
        format!("{}:{}", target, self.path)
    }

    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn join(&self, subpath: &str) -> Self {
        let path = if self.path.ends_with('/') {
            format!("{}{}", self.path, subpath)
        } else {
            format!("{}/{}", self.path, subpath)
        };
        Self {
            user: self.user.clone(),
            host: self.host.clone(),
            path,
        }
    }

    pub async fn expand_tilde(&mut self) -> Result<(), crate::core::error::BcmrError> {
        if !self.path.starts_with('~') {
            return Ok(());
        }
        let target = self.ssh_target();
        let expanded = ops::expand_remote_tilde(&target, &self.path).await?;
        self.path = expanded;
        Ok(())
    }

    pub fn reject_unsafe(&self) -> Result<(), crate::core::error::BcmrError> {
        let p = std::path::Path::new(&self.path);
        let mut has_named = false;
        let mut has_root = false;
        for c in p.components() {
            match c {
                std::path::Component::ParentDir => {
                    return Err(crate::core::error::BcmrError::InvalidInput(format!(
                        "remote path '{}' contains '..' — refusing for safety; \
                         this is a hard error on every transport, including the legacy SSH fallback",
                        self
                    )));
                }
                std::path::Component::Normal(_) => has_named = true,
                std::path::Component::RootDir => has_root = true,
                _ => {}
            }
        }
        // Reject bare filesystem root (`host:/`); allow `.`/`./` (scp's
        // shorthand for the login dir).
        if has_root && !has_named {
            return Err(crate::core::error::BcmrError::InvalidInput(format!(
                "remote path '{}' resolves to filesystem root — refusing for safety \
                 (use a named path under it)",
                self
            )));
        }
        Ok(())
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
        return None;
    }

    let userinfo_split = {
        let lbrack = s.find('[');
        let colon = s.find(':');
        let cap = match (lbrack, colon) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return None,
        };
        s[..cap].find('@')
    };

    let (user, rest) = if let Some(at_pos) = userinfo_split {
        let user_part = &s[..at_pos];
        if user_part.is_empty() || user_part.contains('/') || user_part.contains(' ') {
            return None;
        }
        (Some(user_part.to_string()), &s[at_pos + 1..])
    } else {
        (None, s)
    };

    if let Some(stripped) = rest.strip_prefix('[') {
        let bracket_close = stripped.find(']')?;
        let host = &stripped[..bracket_close];
        if host.is_empty() {
            return None;
        }
        let after = &stripped[bracket_close + 1..];
        let path_part = after.strip_prefix(':')?;
        let path = if path_part.is_empty() {
            ".".to_string()
        } else {
            path_part.to_string()
        };
        return Some(RemotePath {
            user,
            host: host.to_string(),
            path,
        });
    }

    let colon_pos = rest.find(':')?;
    if colon_pos == 0 {
        return None;
    }

    let host_part = &rest[..colon_pos];
    let path_part = &rest[colon_pos + 1..];

    if host_part.contains('/') || host_part.contains(' ') || host_part.contains('@') {
        return None;
    }

    let path = if path_part.is_empty() {
        ".".to_string()
    } else {
        path_part.to_string()
    };

    Some(RemotePath {
        user,
        host: host_part.to_string(),
        path,
    })
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
    pub sync: bool,
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
    fn parse_bracketed_ipv6() {
        let r = parse_remote_path("[::1]:dst").unwrap();
        assert_eq!(r.user, None);
        assert_eq!(r.host, "::1");
        assert_eq!(r.path, "dst");

        let r = parse_remote_path("me@[::1]:dst").unwrap();
        assert_eq!(r.user, Some("me".to_string()));
        assert_eq!(r.host, "::1");
        assert_eq!(r.path, "dst");

        let r = parse_remote_path("[2001:db8::1]:/abs").unwrap();
        assert_eq!(r.host, "2001:db8::1");
        assert_eq!(r.path, "/abs");

        let r = parse_remote_path("[fe80::1%eth0]:foo").unwrap();
        assert_eq!(r.host, "fe80::1%eth0");

        let r = parse_remote_path("[::1]:").unwrap();
        assert_eq!(r.path, ".");
    }

    #[test]
    fn parse_bracketed_ipv6_rejections() {
        assert!(parse_remote_path("[]:foo").is_none());
        assert!(parse_remote_path("[::1]").is_none());
        assert!(parse_remote_path("@[::1]:foo").is_none());
    }

    #[test]
    fn display_brackets_ipv6_host() {
        let r = RemotePath {
            user: None,
            host: "::1".into(),
            path: "/foo".into(),
        };
        assert_eq!(r.display(), "[::1]:/foo");

        let r2 = RemotePath {
            user: Some("me".into()),
            host: "fe80::1%eth0".into(),
            path: "/foo".into(),
        };
        assert_eq!(r2.display(), "me@[fe80::1%eth0]:/foo");

        let parsed = parse_remote_path(&r.display()).unwrap();
        assert_eq!(parsed.host, r.host);
        assert_eq!(parsed.path, r.path);
        let parsed2 = parse_remote_path(&r2.display()).unwrap();
        assert_eq!(parsed2.user, r2.user);
        assert_eq!(parsed2.host, r2.host);
        assert_eq!(parsed2.path, r2.path);
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
    fn join_does_not_double_slash_when_base_has_trailing_slash() {
        let r = RemotePath {
            user: None,
            host: "h".to_string(),
            path: "dst/".to_string(),
        };
        assert_eq!(r.join("file.txt").path, "dst/file.txt");
    }

    #[test]
    fn test_shell_escape_no_quotes() {
        assert_eq!(super::ssh_cmd::shell_escape("simple"), "simple");
    }

    #[test]
    fn test_shell_escape_with_quotes() {
        assert_eq!(super::ssh_cmd::shell_escape("it's"), "it'\\''s");
    }

    fn rp(path: &str) -> RemotePath {
        RemotePath {
            user: None,
            host: "h".into(),
            path: path.into(),
        }
    }

    #[test]
    fn reject_unsafe_blocks_parent_dir_components() {
        assert!(rp("..").reject_unsafe().is_err());
        assert!(rp("foo/..").reject_unsafe().is_err());
        assert!(rp("foo/../bar").reject_unsafe().is_err());
        assert!(rp("../etc/passwd").reject_unsafe().is_err());
    }

    #[test]
    fn reject_unsafe_blocks_root_only() {
        assert!(rp("/").reject_unsafe().is_err());
        rp("").reject_unsafe().expect("empty allowed");
        rp(".").reject_unsafe().expect(". allowed");
        rp("./").reject_unsafe().expect("./ allowed");
    }

    #[test]
    fn reject_unsafe_allows_login_dir_shorthand() {
        for s in ["host:", "host:.", "host:./"] {
            let r = parse_remote_path(s).unwrap_or_else(|| panic!("{s} should parse"));
            r.reject_unsafe()
                .unwrap_or_else(|e| panic!("{s} should pass reject_unsafe: {e}"));
        }
        let r = parse_remote_path("host:/").unwrap();
        assert!(r.reject_unsafe().is_err());
    }

    #[test]
    fn reject_unsafe_allows_normal_paths() {
        rp("foo").reject_unsafe().expect("plain name");
        rp("foo/bar.txt").reject_unsafe().expect("nested name");
        rp("./foo").reject_unsafe().expect("cur-prefixed name");
        rp("/etc/passwd").reject_unsafe().expect("absolute name");
    }
}
