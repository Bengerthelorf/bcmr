mod attrs;
pub mod backend;
mod cleanup;
#[allow(dead_code)] // Shared with the command binary composite move engine.
pub(crate) mod move_delivery;
mod ops;
mod patterns;
mod resume;
pub(crate) mod sftp_cleanup;
pub(crate) mod sftp_ops;
#[allow(dead_code)] // Used by the command binary composite transfer engine.
pub(crate) mod sftp_stage;
mod ssh_cmd;
pub mod staged_upload;
mod transfer;
mod tree;

#[allow(unused_imports)]
pub use attrs::{apply_remote_attrs_locally, preserve_remote_attrs, verify_remote_file};
#[allow(unused_imports)]
pub(crate) use attrs::{get_remote_attrs, set_remote_attrs};
pub use cleanup::RemoteCleanupPlan;
#[allow(unused_imports)]
pub use ops::{
    complete_remote_path, expand_remote_tilde, remote_file_hash, remote_file_size,
    remote_list_files, remote_list_shallow, remote_path_is_directory, remote_remove, remote_stat,
    remote_total_size, resolve_remote_home, validate_ssh_connection,
};
pub use patterns::expand_remote_star;
pub use resume::{check_resume_state, ResumeDecision};
pub use sftp_cleanup::SftpCleanupPlan;
#[allow(unused_imports)]
pub(crate) use ssh_cmd::metadata_command;
#[allow(unused_imports)] // Command binary relay streams.
pub(crate) use ssh_cmd::ssh_command;
#[allow(unused_imports)]
pub(crate) use ssh_cmd::ssh_error_message;
#[allow(unused_imports)]
pub use transfer::{
    download_directory, download_file, ensure_remote_directories, ensure_remote_tree,
    upload_directory, upload_file,
};
#[allow(unused_imports)]
pub use tree::{create_remote_symlink, inventory_remote, RemoteTreeEntry};

pub fn set_ssh_compression(enabled: bool) {
    ssh_cmd::SSH_COMPRESS.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Debug, Clone)]
pub struct RemotePath {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
}

impl RemotePath {
    pub fn ssh_target(&self) -> String {
        if self.requires_uri_authority() {
            return format!("ssh://{}", self.uri_authority());
        }
        match &self.user {
            Some(user) => format!("{}@{}", user, self.host),
            None => self.host.clone(),
        }
    }

    pub fn display(&self) -> String {
        if self.requires_uri_authority()
            || (self.user.is_none()
                && self.host.len() == 1
                && self.host.as_bytes()[0].is_ascii_alphabetic()
                && self.path.starts_with('/'))
        {
            return self.uri_display();
        }
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

    fn requires_uri_authority(&self) -> bool {
        self.port.is_some()
            || self.user.as_ref().is_some_and(|user| {
                user.starts_with('-') || user.contains(['@', '/', ':', '[', ']'])
            })
    }

    fn uri_authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host.replace('%', "%25"))
        } else {
            uri_encode(&self.host, false)
        };
        let user = self
            .user
            .as_ref()
            .map_or_else(String::new, |user| format!("{}@", uri_encode(user, false)));
        let port = self
            .port
            .map_or_else(String::new, |port| format!(":{port}"));
        format!("{user}{host}{port}")
    }

    /// Render an absolute URI while retaining decoded filename semantics.
    pub fn uri_display(&self) -> String {
        format!(
            "scp://{}{}",
            self.uri_authority(),
            uri_encode(&self.path, true)
        )
    }

    pub fn file_name(&self) -> &str {
        self.path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&self.path)
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
            port: self.port,
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
    // Never reinterpret a malformed explicit URI as a legacy `scp:path` host.
    if s.get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("scp://"))
    {
        return parse_scp_uri(s).ok();
    }
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
        if user_part.is_empty()
            || user_part.starts_with('-')
            || user_part.contains('/')
            || user_part.contains(' ')
        {
            return None;
        }
        (Some(user_part.to_string()), &s[at_pos + 1..])
    } else {
        (None, s)
    };

    if let Some(stripped) = rest.strip_prefix('[') {
        let bracket_close = stripped.find(']')?;
        let host = &stripped[..bracket_close];
        if host.is_empty()
            || host.starts_with('-')
            || host.chars().any(char::is_control)
            || host.chars().any(char::is_whitespace)
            || host.contains(['/', '@'])
        {
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
            port: None,
            path,
        });
    }

    let colon_pos = rest.find(':')?;
    if colon_pos == 0 {
        return None;
    }

    let host_part = &rest[..colon_pos];
    let path_part = &rest[colon_pos + 1..];

    if host_part.contains('/')
        || host_part.contains(' ')
        || host_part.contains('@')
        || host_part.starts_with('-')
    {
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
        port: None,
        path,
    })
}

fn uri_decode(value: &str) -> Result<String, &'static str> {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position] == b'%' {
            let pair = bytes
                .get(position + 1..position + 3)
                .ok_or("incomplete percent escape")?;
            let high = (pair[0] as char)
                .to_digit(16)
                .ok_or("invalid percent escape")?;
            let low = (pair[1] as char)
                .to_digit(16)
                .ok_or("invalid percent escape")?;
            decoded.push(((high << 4) | low) as u8);
            position += 3;
        } else {
            decoded.push(bytes[position]);
            position += 1;
        }
    }
    if decoded.contains(&0) {
        return Err("NUL is not permitted");
    }
    String::from_utf8(decoded).map_err(|_| "percent escapes must contain valid UTF-8")
}

fn uri_encode(value: &str, path: bool) -> String {
    use std::fmt::Write;
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (path && *byte == b'/')
        {
            encoded.push(*byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn parse_scp_uri(input: &str) -> Result<RemotePath, &'static str> {
    let body = input.get(6..).ok_or("missing authority")?;
    if body.contains(['?', '#']) {
        return Err("query and fragment delimiters must be percent escaped");
    }
    let slash = body.find('/').ok_or("an absolute path is required")?;
    let authority = &body[..slash];
    let path = uri_decode(&body[slash..])?;
    let (user, host_port) = if let Some((user, host_port)) = authority.split_once('@') {
        if user.is_empty() || user.contains(':') || host_port.contains('@') {
            return Err("invalid username or unsupported password");
        }
        let user = uri_decode(user)?;
        if user.chars().any(char::is_control) || user.chars().any(char::is_whitespace) {
            return Err("invalid username");
        }
        (Some(user), host_port)
    } else {
        (None, authority)
    };
    let (host, port_text) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let close = bracketed.find(']').ok_or("unclosed IPv6 address")?;
        let host = uri_decode(&bracketed[..close])?;
        let address = host
            .split_once('%')
            .map_or(host.as_str(), |(address, _)| address);
        if address.parse::<std::net::Ipv6Addr>().is_err() || host.ends_with('%') {
            return Err("invalid bracketed IPv6 address");
        }
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or("invalid IPv6 authority suffix")?,
            )
        };
        (host, port)
    } else {
        if host_port.matches(':').count() > 1 || host_port.contains(['[', ']']) {
            return Err("IPv6 addresses must be bracketed");
        }
        let (host, port) = host_port
            .split_once(':')
            .map_or((host_port, None), |(host, port)| (host, Some(port)));
        (uri_decode(host)?, port)
    };
    if host.is_empty()
        || host.starts_with('-')
        || host.chars().any(char::is_whitespace)
        || host.chars().any(char::is_control)
        || host.contains(['/', '@', '?', '#', '[', ']'])
    {
        return Err("invalid host");
    }
    // Only the bracketed branch may produce an IPv6 host.
    if !host_port.starts_with('[') && host.contains(':') {
        return Err("IPv6 addresses must be bracketed");
    }
    let port = if let Some(text) = port_text {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("port must be an integer from 1 to 65535");
        }
        Some(
            text.parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or("port must be an integer from 1 to 65535")?,
        )
    } else {
        None
    };
    Ok(RemotePath {
        user,
        host,
        port,
        path,
    })
}

/// Validate all explicit endpoints before any source is expanded or contacted.
pub fn validate_explicit_remote_path(input: &str) -> Result<(), crate::core::error::BcmrError> {
    if input
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("scp://"))
    {
        parse_scp_uri(input).map_err(|reason| {
            crate::core::error::BcmrError::InvalidInput(format!(
                "invalid scp URI '{input}': {reason}"
            ))
        })?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct RemoteFileInfo {
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone, Default)]
pub struct TransferOptions {
    pub force: bool,
    pub preserve: bool,
    pub verify: bool,
    pub resume: bool,
    pub strict: bool,
    pub append: bool,
    pub sync: bool,
}

pub struct TransferCallbacks<'a> {
    pub on_progress: &'a (dyn Fn(u64) + Send + Sync),
    pub on_skip: &'a (dyn Fn(u64) + Send + Sync),
    pub on_new_file: &'a (dyn Fn(&str, u64) + Send + Sync),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scp_uri_decodes_utf8_and_preserves_connection_across_render_and_join() {
        let endpoint =
            parse_remote_path("scp://ali%40ce@[fe80::1%25en0]:2244/a%20b/%E9%9B%AA%25%23.txt")
                .unwrap();
        assert_eq!(endpoint.user.as_deref(), Some("ali@ce"));
        assert_eq!(endpoint.host, "fe80::1%en0");
        assert_eq!(endpoint.port, Some(2244));
        assert_eq!(endpoint.path, "/a b/雪%#.txt");
        let rendered = endpoint.display();
        assert_eq!(
            rendered,
            "scp://ali%40ce@[fe80::1%25en0]:2244/a%20b/%E9%9B%AA%25%23.txt"
        );
        let reparsed = parse_remote_path(&rendered).unwrap();
        assert_eq!(reparsed.ssh_target(), endpoint.ssh_target());
        assert_eq!(reparsed.path, endpoint.path);
        let child = endpoint.join("child #%.txt");
        assert_eq!(child.port, Some(2244));
        assert_eq!(child.ssh_target(), endpoint.ssh_target());
        assert_eq!(
            parse_remote_path(&child.display()).unwrap().path,
            "/a b/雪%#.txt/child #%.txt"
        );
    }

    #[test]
    fn scp_uri_without_port_is_absolute_and_legacy_absolute_spelling_is_retained() {
        let endpoint = parse_remote_path("scp://fixture/absolute%20path").unwrap();
        assert_eq!(endpoint.port, None);
        assert_eq!(endpoint.path, "/absolute path");
        let short_host = parse_remote_path("scp://h/absolute%20path").unwrap();
        assert_eq!(
            parse_remote_path(&short_host.display()).unwrap().path,
            "/absolute path"
        );
        let unusual_user = parse_remote_path("scp://ali%2Fce@fixture/path").unwrap();
        assert_eq!(
            parse_remote_path(&unusual_user.display())
                .unwrap()
                .user
                .as_deref(),
            Some("ali/ce")
        );
        assert_eq!(
            parse_remote_path("fixture://absolute/path").unwrap().path,
            "//absolute/path"
        );
    }

    #[test]
    fn explicit_malformed_scp_uris_never_become_legacy_endpoints() {
        for input in [
            "scp://",
            "scp:///file",
            "scp://fixture",
            "scp://fixture:0/file",
            "scp://fixture:65536/file",
            "scp://fixture:abc/file",
            "scp://[::1/file",
            "scp://::1/file",
            "scp://fixture/%",
            "scp://fixture/%GG",
            "scp://fixture/%FF",
            "scp://fixture/%00",
            "scp://fixture/path?query",
            "scp://fixture/path#fragment",
            "scp://-oProxyCommand=bad/file",
            "scp://user:password@fixture/file",
        ] {
            assert!(parse_remote_path(input).is_none(), "must reject {input:?}");
            assert!(
                validate_explicit_remote_path(input).is_err(),
                "must reject explicitly before I/O: {input:?}"
            );
        }
    }

    #[test]
    fn explicit_uri_ports_separate_connection_identities() {
        let first = parse_remote_path("scp://fixture:2222/file").unwrap();
        let second = parse_remote_path("scp://fixture:2223/file").unwrap();
        assert_ne!(first.ssh_target(), second.ssh_target());
    }

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
            port: None,
            path: "/foo".into(),
        };
        assert_eq!(r.display(), "[::1]:/foo");

        let r2 = RemotePath {
            user: Some("me".into()),
            host: "fe80::1%eth0".into(),
            port: None,
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
            port: None,
            path: "/data".to_string(),
        };
        assert_eq!(r.ssh_target(), "alice@example.com");

        let r2 = RemotePath {
            user: None,
            host: "example.com".to_string(),
            port: None,
            path: "/data".to_string(),
        };
        assert_eq!(r2.ssh_target(), "example.com");
    }

    #[test]
    fn test_remote_path_display() {
        let r = RemotePath {
            user: Some("bob".to_string()),
            host: "srv".to_string(),
            port: None,
            path: "/tmp/f".to_string(),
        };
        assert_eq!(r.display(), "bob@srv:/tmp/f");
    }

    #[test]
    fn test_remote_path_join() {
        let r = RemotePath {
            user: None,
            host: "h".to_string(),
            port: None,
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
            port: None,
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
            port: None,
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
