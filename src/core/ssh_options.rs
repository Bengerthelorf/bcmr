//! Temporary, invocation-wide OpenSSH connection overrides.
//!
//! Targets retain their full connection identity in pools and caches. At the
//! process boundary URI authorities are decomposed, since OpenSSH versions
//! differ in their support for bracketed IPv6 URI destinations.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshOptions {
    pub port: Option<u16>,
    pub identities: Vec<PathBuf>,
    pub config: Option<PathBuf>,
    pub jump: Option<String>,
}

static OPTIONS: LazyLock<RwLock<SshOptions>> = LazyLock::new(|| RwLock::new(SshOptions::default()));

/// Configure once at CLI startup, before any bookmark or source expansion.
pub fn configure(options: SshOptions) {
    *OPTIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = options;
}

fn snapshot() -> SshOptions {
    OPTIONS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn uri_endpoint(target: &str) -> Option<super::remote::RemotePath> {
    let authority = target.strip_prefix("ssh://")?;
    super::remote::parse_remote_path(&format!("scp://{authority}/"))
}

fn args_for(target: &str, options: &SshOptions) -> Vec<String> {
    let endpoint = uri_endpoint(target);
    let mut args = Vec::new();
    if let Some(port) = endpoint
        .as_ref()
        .and_then(|remote| remote.port)
        .or(options.port)
    {
        args.extend(["-p".into(), port.to_string()]);
    }
    for identity in &options.identities {
        args.extend(["-i".into(), identity.to_string_lossy().into_owned()]);
    }
    if let Some(config) = &options.config {
        args.extend(["-F".into(), config.to_string_lossy().into_owned()]);
    }
    if let Some(jump) = &options.jump {
        args.extend(["-J".into(), jump.clone()]);
    }
    if let Some(user) = endpoint.and_then(|remote| remote.user) {
        args.extend(["-l".into(), user]);
    }
    args
}

/// Options only: add these before `destination_arg(target)` and the command.
/// An explicit endpoint port takes precedence over the temporary global port.
pub fn extra_args(target: &str) -> Vec<String> {
    args_for(target, &snapshot())
}

/// Destination for the actual OpenSSH process; pool/cache keys keep `target`.
pub fn destination_arg(target: &str) -> String {
    uri_endpoint(target).map_or_else(|| target.to_owned(), |remote| remote.host)
}

/// System sftp uses -P for ports and -l for rate limits, unlike ssh.
#[allow(dead_code)] // The system-sftp deployment path is binary-only.
pub(crate) fn sftp_args(target: &str) -> Vec<String> {
    let mut converted = Vec::new();
    let args = extra_args(target);
    for pair in args.as_chunks::<2>().0 {
        match pair[0].as_str() {
            "-p" => converted.extend(["-o".into(), format!("Port={}", pair[1])]),
            "-l" => converted.extend(["-o".into(), format!("User={}", pair[1])]),
            _ => converted.extend_from_slice(pair),
        }
    }
    converted
}

fn identity_for(target: &str, options: &SshOptions) -> String {
    let mut hash = blake3::Hasher::new();
    for component in std::iter::once(target.to_owned()).chain(args_for(target, options)) {
        hash.update(&(component.len() as u64).to_le_bytes());
        hash.update(component.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

/// Every connection override contributes to the legacy multiplex socket key.
pub(crate) fn connection_identity(target: &str) -> String {
    identity_for(target, &snapshot())
}

/// Positive evidence of one authenticated SSH server and login policy. This
/// does not prove that two *different* server keys expose disjoint filesystems.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerIdentity {
    host_key: String,
    user: String,
    authentication: Vec<(String, String)>,
}

static PEERS: LazyLock<tokio::sync::Mutex<HashMap<String, Option<PeerIdentity>>>> =
    LazyLock::new(Default::default);

async fn bounded_ssh_output(mut command: tokio::process::Command) -> Option<(Vec<u8>, Vec<u8>)> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    const LIMIT: u64 = 64 * 1024;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?.take(LIMIT + 1);
    let mut stderr = child.stderr.take()?.take(LIMIT + 1);
    let collect = async move {
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let result = tokio::try_join!(
            stdout.read_to_end(&mut output),
            stderr.read_to_end(&mut errors)
        );
        if result.is_err() || output.len() as u64 > LIMIT || errors.len() as u64 > LIMIT {
            let _ = child.kill().await;
            return None;
        }
        let _ = child.wait().await.ok()?;
        Some((output, errors))
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), collect)
        .await
        .ok()
        .flatten()
}

fn parse_login_policy(output: &[u8]) -> Option<(String, Vec<(String, String)>)> {
    let text = std::str::from_utf8(output).ok()?;
    let mut user = None;
    let mut authentication = Vec::new();
    for line in text.lines() {
        let (key, value) = line.split_once(' ')?;
        if key == "user" {
            if user.replace(value.to_owned()).is_some() || value.is_empty() {
                return None;
            }
        } else if matches!(
            key,
            "identityfile"
                | "certificatefile"
                | "identityagent"
                | "identitiesonly"
                | "preferredauthentications"
                | "pubkeyauthentication"
                | "passwordauthentication"
        ) {
            authentication.push((key.to_owned(), value.to_owned()));
        }
    }
    Some((user?, authentication))
}

fn parse_server_key(output: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(output).ok()?;
    let mut key = None;
    for line in text.lines() {
        let Some(value) = line
            .strip_prefix("debug1: Server host key: ")
            .or_else(|| line.strip_prefix("debug1: Server host certificate: "))
        else {
            continue;
        };
        let mut parts = value.split_whitespace();
        let algorithm = parts.next()?;
        let fingerprint = parts.next()?;
        if !fingerprint.starts_with("SHA256:") || fingerprint.len() < 30 {
            return None;
        }
        let observed = format!("{algorithm} {fingerprint}");
        if key.as_ref().is_some_and(|key| key != &observed) {
            return None;
        }
        key = Some(observed);
    }
    key
}

/// Probe the actual SSH host key with a read-only subsystem request. Setting
/// LogLevel rather than -v avoids enabling jump-child verbosity and confusing
/// a bastion's host key with the destination's key. No remote shell is used.
#[allow(dead_code)] // Used by binary-only remote move planning.
pub(crate) async fn peer_identity(target: &str) -> Option<PeerIdentity> {
    let cache_key = connection_identity(target);
    let mut peers = PEERS.lock().await;
    if let Some(identity) = peers.get(&cache_key) {
        return identity.clone();
    }
    let probe = async {
        let mut config = tokio::process::Command::new("ssh");
        config
            .arg("-G")
            .args(extra_args(target))
            .arg(destination_arg(target));
        let (output, _) = bounded_ssh_output(config).await?;
        let (user, authentication) = parse_login_policy(&output)?;
        let mut connection = tokio::process::Command::new("ssh");
        connection
            .args([
                "-T",
                "-o",
                "LogLevel=DEBUG1",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=none",
            ])
            .args(extra_args(target))
            .arg("-s")
            .arg(destination_arg(target))
            .arg("sftp");
        let (_, errors) = bounded_ssh_output(connection).await?;
        Some(PeerIdentity {
            host_key: parse_server_key(&errors)?,
            user,
            authentication,
        })
    }
    .await;
    peers.insert(cache_key, probe.clone());
    probe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_key_probe_rejects_mixed_peer_transcripts() {
        let first = "debug1: Server host key: ssh-ed25519 SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";
        let second = "debug1: Server host key: ssh-ed25519 SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\n";
        assert_eq!(
            parse_server_key(first.as_bytes()).as_deref(),
            Some("ssh-ed25519 SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        );
        assert!(parse_server_key(format!("{first}{second}").as_bytes()).is_none());
    }

    #[test]
    fn login_identity_keeps_distinct_users_and_configured_credentials_separate() {
        let first = parse_login_policy(
            b"hostname server\nuser alice\nidentityfile first key\nidentityfile second key\n",
        )
        .unwrap();
        assert_eq!(first.0, "alice");
        assert_eq!(
            first.1,
            [
                ("identityfile".to_owned(), "first key".to_owned()),
                ("identityfile".to_owned(), "second key".to_owned())
            ]
        );
        assert_ne!(
            first,
            parse_login_policy(b"hostname server\nuser bob\nidentityfile first key\n").unwrap()
        );
    }

    #[test]
    fn endpoint_port_and_decoded_user_override_global_connection_values() {
        let options = SshOptions {
            port: Some(2244),
            identities: vec!["first key".into(), "second key".into()],
            config: Some("config path".into()),
            jump: Some("jump@alias:2299".into()),
        };
        assert_eq!(
            args_for("ssh://ali%40ce@[::1]:2255", &options),
            [
                "-p",
                "2255",
                "-i",
                "first key",
                "-i",
                "second key",
                "-F",
                "config path",
                "-J",
                "jump@alias:2299",
                "-l",
                "ali@ce"
            ]
        );
        assert_eq!(destination_arg("ssh://ali%40ce@[::1]:2255"), "::1");
        assert_eq!(destination_arg("user@alias"), "user@alias");
    }

    #[test]
    fn multiplex_identity_includes_every_override_and_endpoint_port() {
        let base = SshOptions::default();
        let variants = [
            SshOptions {
                port: Some(2222),
                ..base.clone()
            },
            SshOptions {
                identities: vec!["key".into()],
                ..base.clone()
            },
            SshOptions {
                config: Some("config".into()),
                ..base.clone()
            },
            SshOptions {
                jump: Some("jump".into()),
                ..base.clone()
            },
        ];
        let baseline = identity_for("host", &base);
        for options in variants {
            assert_ne!(baseline, identity_for("host", &options));
        }
        assert_ne!(
            identity_for("ssh://host:2222", &base),
            identity_for("ssh://host:2223", &base)
        );
        assert_ne!(
            identity_for(
                "host",
                &SshOptions {
                    identities: vec!["one".into(), "two".into()],
                    ..base.clone()
                }
            ),
            identity_for(
                "host",
                &SshOptions {
                    identities: vec!["two".into(), "one".into()],
                    ..base
                }
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_openssh_gets_ipv6_host_user_and_inline_port_without_uri_support() {
        let target = "ssh://ali%40ce@[::1]:2255";
        let options = SshOptions {
            port: Some(2244),
            config: Some("/dev/null".into()),
            ..SshOptions::default()
        };
        let output = match std::process::Command::new("ssh")
            .arg("-G")
            .args(args_for(target, &options))
            .arg(destination_arg(target))
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("cannot invoke native OpenSSH: {error}"),
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = String::from_utf8(output.stdout).unwrap();
        for expected in ["user ali@ce", "hostname ::1", "port 2255"] {
            assert!(
                output.lines().any(|line| line == expected),
                "missing {expected:?} in {output}"
            );
        }
    }
}
