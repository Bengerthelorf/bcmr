//! SFTP v3 over the system OpenSSH subsystem transport.
//!
//! This module never sends a remote shell command. It serializes requests,
//! bounds every wire packet, and discards a session if an in-flight future is
//! cancelled: a partially consumed reply must never satisfy another request.
//!
//! Protocol sources:
//! - https://www.ietf.org/archive/id/draft-ietf-secsh-filexfer-02.txt
//! - https://github.com/openssh/openssh-portable/blob/master/PROTOCOL

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::core::error::BcmrError;

mod wire;

use wire::{ExpectedReply, Reply};

/// Conservative interoperable payload size. `read` may return fewer bytes;
/// `write` splits a larger caller buffer into requests of at most this size.
pub const MAX_IO_LEN: usize = 32 * 1024;
const STDERR_LIMIT: usize = 8 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 1_000_000;
const MAX_DIRECTORY_BYTES: usize = 64 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
// Matches the bounded OpenSSH keepalive policy, with time for its final probe.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(330);
static NEXT_HANDLE_TOKEN: AtomicU64 = AtomicU64::new(1);

/// SFTP v3 open flags (draft-ietf-secsh-filexfer-02 section 6.3).
pub mod flags {
    pub const READ: u32 = 0x0000_0001;
    pub const WRITE: u32 = 0x0000_0002;
    pub const APPEND: u32 = 0x0000_0004;
    pub const CREATE: u32 = 0x0000_0008;
    pub const TRUNCATE: u32 = 0x0000_0010;
    pub const EXCLUSIVE: u32 = 0x0000_0020;
}

/// Missing fields were not supplied by the peer. SFTP v3 encodes uid/gid
/// together and atime/mtime together; timestamps have whole-second precision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attrs {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub permissions: Option<u32>,
    pub atime: Option<u32>,
    pub mtime: Option<u32>,
}

impl Attrs {
    pub fn is_file(&self) -> bool {
        self.permissions
            .is_some_and(|mode| mode & 0o170000 == 0o100000)
    }

    pub fn is_dir(&self) -> bool {
        self.permissions
            .is_some_and(|mode| mode & 0o170000 == 0o040000)
    }

    pub fn is_symlink(&self) -> bool {
        self.permissions
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
    }
}

/// An opaque handle issued by one particular client. Clones cannot be used
/// after close or with another client, even if the server recycles its bytes.
#[derive(Debug, Clone)]
pub struct Handle {
    bytes: Vec<u8>,
    token: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    Eof,
    NoSuchFile,
    PermissionDenied,
    Failure,
    BadMessage,
    NoConnection,
    ConnectionLost,
    Unsupported,
    Other(u32),
}

impl StatusCode {
    fn from_u32(code: u32) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::Eof,
            2 => Self::NoSuchFile,
            3 => Self::PermissionDenied,
            4 => Self::Failure,
            5 => Self::BadMessage,
            6 => Self::NoConnection,
            7 => Self::ConnectionLost,
            8 => Self::Unsupported,
            other => Self::Other(other),
        }
    }

    pub fn as_u32(self) -> u32 {
        match self {
            Self::Ok => 0,
            Self::Eof => 1,
            Self::NoSuchFile => 2,
            Self::PermissionDenied => 3,
            Self::Failure => 4,
            Self::BadMessage => 5,
            Self::NoConnection => 6,
            Self::ConnectionLost => 7,
            Self::Unsupported => 8,
            Self::Other(code) => code,
        }
    }
}

/// The typed inner error of a `BcmrError::Io` produced by an SFTP STATUS.
#[derive(Debug, Clone)]
pub struct StatusError {
    pub code: StatusCode,
    pub message: String,
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "SFTP status {} ({:?})",
            self.code.as_u32(),
            self.code
        )?;
        if !self.message.is_empty() {
            write!(formatter, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for StatusError {}

impl StatusError {
    fn into_error(self) -> BcmrError {
        use std::io::ErrorKind;
        let kind = match self.code {
            StatusCode::Eof => ErrorKind::UnexpectedEof,
            StatusCode::NoSuchFile => ErrorKind::NotFound,
            StatusCode::PermissionDenied => ErrorKind::PermissionDenied,
            StatusCode::BadMessage => ErrorKind::InvalidData,
            StatusCode::NoConnection => ErrorKind::NotConnected,
            StatusCode::ConnectionLost => ErrorKind::ConnectionAborted,
            StatusCode::Unsupported => ErrorKind::Unsupported,
            _ => ErrorKind::Other,
        };
        std::io::Error::new(kind, self).into()
    }
}

#[allow(dead_code)] // Public protocol API; the binary uses standard IO error kinds.
pub fn status_code(error: &BcmrError) -> Option<StatusCode> {
    let BcmrError::Io(error) = error else {
        return None;
    };
    error
        .get_ref()?
        .downcast_ref::<StatusError>()
        .map(|status| status.code)
}

#[derive(Default)]
struct Capabilities {
    posix_rename: bool,
    fsync: bool,
    hardlink: bool,
}

pub struct SftpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_task: tokio::task::JoinHandle<()>,
    capabilities: Capabilities,
    next_id: u32,
    handles: HashMap<u64, Vec<u8>>,
    poisoned: bool,
}

impl SftpClient {
    pub async fn connect(target: &str) -> Result<Self, BcmrError> {
        let child = Command::new("ssh")
            .args(ssh_args(target)?)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        Self::from_child(child).await
    }

    // Separating subprocess ownership from initialization also permits native
    // protocol tests to launch the actual local OpenSSH sftp-server executable.
    pub(crate) async fn from_child(mut child: Child) -> Result<Self, BcmrError> {
        let Some(stdin) = child.stdin.take() else {
            let _ = child.start_kill();
            return Err(BcmrError::InvalidInput(
                "SFTP transport has no stdin pipe".into(),
            ));
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.start_kill();
            return Err(BcmrError::InvalidInput(
                "SFTP transport has no stdout pipe".into(),
            ));
        };
        let Some(mut child_stderr) = child.stderr.take() else {
            let _ = child.start_kill();
            return Err(BcmrError::InvalidInput(
                "SFTP transport has no stderr pipe".into(),
            ));
        };
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&stderr);
        let stderr_task = tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            while let Ok(length) = child_stderr.read(&mut buffer).await {
                if length == 0 {
                    break;
                }
                let mut captured = capture.lock();
                let excess = captured
                    .len()
                    .saturating_add(length)
                    .saturating_sub(STDERR_LIMIT);
                if excess != 0 {
                    captured.drain(..excess);
                }
                captured.extend_from_slice(&buffer[..length]);
            }
        });
        let mut client = Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr,
            stderr_task,
            capabilities: Capabilities::default(),
            next_id: 1,
            handles: HashMap::new(),
            poisoned: false,
        };
        client.capabilities = client
            .exchange(
                &[wire::INIT, 0, 0, 0, 3],
                HANDSHAKE_TIMEOUT,
                wire::decode_version,
            )
            .await?;
        Ok(client)
    }

    #[allow(dead_code)] // Public protocol API; composite namespace sync is refused.
    pub fn supports_fsync(&self) -> bool {
        self.capabilities.fsync
    }

    pub fn supports_posix_rename(&self) -> bool {
        self.capabilities.posix_rename
    }

    pub fn supports_hardlink(&self) -> bool {
        self.capabilities.hardlink
    }

    pub(crate) fn is_usable(&self) -> bool {
        !self.poisoned && self.stdin.is_some()
    }

    pub async fn stat(&mut self, path: &str, follow: bool) -> Result<Attrs, BcmrError> {
        let payload = path_payload(path)?;
        let kind = if follow { wire::STAT } else { wire::LSTAT };
        match self.request(kind, &payload, ExpectedReply::Attrs).await? {
            Reply::Attrs(attrs) => Ok(attrs),
            Reply::Status(status) => Err(self.status_failure(status)),
            _ => unreachable!("reply decoder enforces the expected packet type"),
        }
    }

    /// Returns raw path bytes so the caller can make an explicit UTF-8 policy.
    pub async fn realpath(&mut self, path: &str) -> Result<Vec<u8>, BcmrError> {
        let name = self.single_name(wire::REALPATH, path).await?;
        if !name.starts_with(b"/") {
            return Err(self.protocol_error("REALPATH did not return an absolute path"));
        }
        Ok(name)
    }

    pub async fn readlink(&mut self, path: &str) -> Result<Vec<u8>, BcmrError> {
        self.single_name(wire::READLINK, path).await
    }

    async fn single_name(&mut self, kind: u8, path: &str) -> Result<Vec<u8>, BcmrError> {
        let payload = path_payload(path)?;
        match self.request(kind, &payload, ExpectedReply::Names).await? {
            Reply::Names(mut names) if names.len() == 1 => {
                let name = names.pop().expect("one returned name").0;
                if name.is_empty() || name.len() > wire::MAX_PATH_LEN || name.contains(&0) {
                    return Err(
                        self.protocol_error("returned path is empty, oversized, or contains NUL")
                    );
                }
                Ok(name)
            }
            Reply::Names(_) => {
                Err(self.protocol_error("path operation returned more than one name"))
            }
            Reply::Status(status) => Err(self.status_failure(status)),
            _ => unreachable!("reply decoder enforces the expected packet type"),
        }
    }

    /// Validates each raw component before returning it. Dot entries are
    /// skipped; non-UTF8 names remain raw for caller-side exclusion decisions.
    pub async fn readdir(&mut self, path: &str) -> Result<Vec<(Vec<u8>, Attrs)>, BcmrError> {
        let payload = path_payload(path)?;
        let reply = self
            .request(wire::OPENDIR, &payload, ExpectedReply::Handle)
            .await?;
        let handle = self.take_handle(reply)?;
        let result = self.directory_entries(&handle).await;
        let closed = self.close(&handle).await;
        match (result, closed) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(entries), Ok(())) => Ok(entries),
        }
    }

    async fn directory_entries(
        &mut self,
        handle: &Handle,
    ) -> Result<Vec<(Vec<u8>, Attrs)>, BcmrError> {
        let mut payload = Vec::new();
        wire::put_string(self.handle_bytes(handle)?, &mut payload);
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        let mut raw_count = 0usize;
        let mut accounted_bytes = 0usize;
        loop {
            match self
                .request(wire::READDIR, &payload, ExpectedReply::Names)
                .await?
            {
                Reply::Status(status) if status.code == StatusCode::Eof => return Ok(entries),
                Reply::Status(status) => return Err(self.status_failure(status)),
                Reply::Names(names) => {
                    raw_count = raw_count.saturating_add(names.len());
                    if raw_count > MAX_DIRECTORY_ENTRIES {
                        return Err(self.protocol_error("directory exceeds the entry limit"));
                    }
                    for (name, attrs) in names {
                        if let Err(error) = wire::validate_child_name(&name) {
                            self.terminate();
                            return Err(error);
                        }
                        accounted_bytes = accounted_bytes
                            .saturating_add(name.len())
                            .saturating_add(64);
                        if accounted_bytes > MAX_DIRECTORY_BYTES {
                            return Err(self
                                .protocol_error("directory exceeds the enumeration memory limit"));
                        }
                        if name == b"." || name == b".." {
                            continue;
                        }
                        if !seen.insert(name.clone()) {
                            return Err(
                                self.protocol_error("READDIR returned a duplicate component")
                            );
                        }
                        entries.push((name, attrs));
                    }
                }
                _ => unreachable!("reply decoder enforces the expected packet type"),
            }
        }
    }

    pub async fn open(
        &mut self,
        path: &str,
        flags: u32,
        attrs: &Attrs,
    ) -> Result<Handle, BcmrError> {
        if flags & !0x3f != 0
            || flags & (flags::READ | flags::WRITE) == 0
            || flags & flags::EXCLUSIVE != 0 && flags & flags::CREATE == 0
            || flags & (flags::TRUNCATE | flags::APPEND) != 0 && flags & flags::WRITE == 0
        {
            return Err(BcmrError::InvalidInput("Invalid SFTP v3 open flags".into()));
        }
        let mut payload = path_payload(path)?;
        payload.extend_from_slice(&flags.to_be_bytes());
        wire::encode_attrs(attrs, &mut payload)?;
        let reply = self
            .request(wire::OPEN, &payload, ExpectedReply::Handle)
            .await?;
        self.take_handle(reply)
    }

    fn take_handle(&mut self, reply: Reply) -> Result<Handle, BcmrError> {
        match reply {
            Reply::Handle(bytes) => {
                if self.handles.values().any(|live| live == &bytes) {
                    return Err(self.protocol_error("server reused a still-open handle"));
                }
                let token = NEXT_HANDLE_TOKEN.fetch_add(1, Ordering::Relaxed);
                self.handles.insert(token, bytes.clone());
                Ok(Handle { bytes, token })
            }
            Reply::Status(status) => Err(self.status_failure(status)),
            _ => unreachable!("reply decoder enforces the expected packet type"),
        }
    }

    pub async fn read(
        &mut self,
        handle: &Handle,
        offset: u64,
        max_len: usize,
    ) -> Result<Vec<u8>, BcmrError> {
        if max_len == 0 {
            return Err(BcmrError::InvalidInput(
                "SFTP read length must be positive".into(),
            ));
        }
        let max_len = max_len.min(MAX_IO_LEN);
        let mut payload = Vec::new();
        wire::put_string(self.handle_bytes(handle)?, &mut payload);
        payload.extend_from_slice(&offset.to_be_bytes());
        payload.extend_from_slice(&(max_len as u32).to_be_bytes());
        match self
            .request(wire::READ, &payload, ExpectedReply::Data { max_len })
            .await?
        {
            Reply::Data(data) => Ok(data),
            Reply::Status(status) if status.code == StatusCode::Eof => Ok(Vec::new()),
            Reply::Status(status) => Err(self.status_failure(status)),
            _ => unreachable!("reply decoder enforces the expected packet type"),
        }
    }

    pub async fn write(
        &mut self,
        handle: &Handle,
        offset: u64,
        data: &[u8],
    ) -> Result<(), BcmrError> {
        self.handle_bytes(handle)?;
        if offset.checked_add(data.len() as u64).is_none() {
            return Err(BcmrError::InvalidInput(
                "SFTP write offset overflows u64".into(),
            ));
        }
        let mut position = offset;
        for chunk in data.chunks(MAX_IO_LEN) {
            let mut payload = Vec::with_capacity(chunk.len() + wire::MAX_HANDLE_LEN + 16);
            wire::put_string(self.handle_bytes(handle)?, &mut payload);
            payload.extend_from_slice(&position.to_be_bytes());
            wire::put_string(chunk, &mut payload);
            let reply = self
                .request(wire::WRITE, &payload, ExpectedReply::Status)
                .await?;
            self.expect_ok(reply)?;
            position += chunk.len() as u64;
        }
        Ok(())
    }

    pub async fn close(&mut self, handle: &Handle) -> Result<(), BcmrError> {
        let mut payload = Vec::new();
        wire::put_string(self.handle_bytes(handle)?, &mut payload);
        // Per v3, a handle is invalid immediately when CLOSE is sent, even if
        // the server reports a delayed write/flush error in its close reply.
        self.handles.remove(&handle.token);
        let reply = self
            .request(wire::CLOSE, &payload, ExpectedReply::Status)
            .await?;
        self.expect_ok(reply)
    }

    pub async fn mkdir(&mut self, path: &str, attrs: &Attrs) -> Result<(), BcmrError> {
        let mut payload = path_payload(path)?;
        wire::encode_attrs(attrs, &mut payload)?;
        let reply = self
            .request(wire::MKDIR, &payload, ExpectedReply::Status)
            .await?;
        self.expect_ok(reply)
    }

    pub async fn remove(&mut self, path: &str) -> Result<(), BcmrError> {
        self.path_status(wire::REMOVE, path).await
    }

    pub async fn rmdir(&mut self, path: &str) -> Result<(), BcmrError> {
        self.path_status(wire::RMDIR, path).await
    }

    async fn path_status(&mut self, kind: u8, path: &str) -> Result<(), BcmrError> {
        let payload = path_payload(path)?;
        let reply = self.request(kind, &payload, ExpectedReply::Status).await?;
        self.expect_ok(reply)
    }

    /// Request a v3 rename, or negotiated POSIX replacement when `overwrite` is
    /// true. OpenSSH's v3 implementation may use a racy stat/rename fallback;
    /// `overwrite = false` is not an atomic NOREPLACE guarantee. Safety-critical
    /// no-overwrite creation must use hardlink or SYMLINK instead.
    pub async fn rename(&mut self, old: &str, new: &str, overwrite: bool) -> Result<(), BcmrError> {
        if overwrite && !self.supports_posix_rename() {
            return Err(unsupported(
                "atomic replacement requires posix-rename@openssh.com version 1",
            ));
        }
        let mut payload = Vec::new();
        let kind = if overwrite {
            wire::put_string(b"posix-rename@openssh.com", &mut payload);
            wire::EXTENDED
        } else {
            wire::RENAME
        };
        wire::put_string(valid_path(old.as_bytes())?, &mut payload);
        wire::put_string(valid_path(new.as_bytes())?, &mut payload);
        let reply = self.request(kind, &payload, ExpectedReply::Status).await?;
        self.expect_ok(reply)
    }

    pub async fn symlink(&mut self, target: &[u8], link: &str) -> Result<(), BcmrError> {
        let mut payload = Vec::new();
        // OpenSSH PROTOCOL section 4.1 documents its deployed target/link
        // argument order, reversed from the original v3 draft.
        wire::put_string(valid_path(target)?, &mut payload);
        wire::put_string(valid_path(link.as_bytes())?, &mut payload);
        let reply = self
            .request(wire::SYMLINK, &payload, ExpectedReply::Status)
            .await?;
        self.expect_ok(reply)
    }

    /// Atomic no-overwrite creation using OpenSSH's link(2) extension. Unlike
    /// standard v3 RENAME, its server implementation has no stat/rename fallback.
    pub async fn hardlink(&mut self, old: &str, new: &str) -> Result<(), BcmrError> {
        if !self.supports_hardlink() {
            return Err(unsupported(
                "safe no-overwrite publication requires hardlink@openssh.com version 1",
            ));
        }
        let mut payload = Vec::new();
        wire::put_string(b"hardlink@openssh.com", &mut payload);
        wire::put_string(valid_path(old.as_bytes())?, &mut payload);
        wire::put_string(valid_path(new.as_bytes())?, &mut payload);
        let reply = self
            .request(wire::EXTENDED, &payload, ExpectedReply::Status)
            .await?;
        self.expect_ok(reply)
    }

    pub async fn setstat(&mut self, path: &str, attrs: &Attrs) -> Result<(), BcmrError> {
        let mut payload = path_payload(path)?;
        wire::encode_attrs(attrs, &mut payload)?;
        let reply = self
            .request(wire::SETSTAT, &payload, ExpectedReply::Status)
            .await?;
        self.expect_ok(reply)
    }

    /// Apply attributes to the open object, even if its pathname was replaced.
    pub async fn fsetstat(&mut self, handle: &Handle, attrs: &Attrs) -> Result<(), BcmrError> {
        let mut payload = Vec::new();
        wire::put_string(self.handle_bytes(handle)?, &mut payload);
        wire::encode_attrs(attrs, &mut payload)?;
        let reply = self
            .request(wire::FSETSTAT, &payload, ExpectedReply::Status)
            .await?;
        self.expect_ok(reply)
    }

    #[allow(dead_code)] // Public protocol API; composite namespace sync is refused.
    pub async fn fsync(&mut self, handle: &Handle) -> Result<(), BcmrError> {
        if !self.supports_fsync() {
            return Err(unsupported(
                "durable file publication requires fsync@openssh.com version 1",
            ));
        }
        let mut payload = Vec::new();
        wire::put_string(b"fsync@openssh.com", &mut payload);
        wire::put_string(self.handle_bytes(handle)?, &mut payload);
        let reply = self
            .request(wire::EXTENDED, &payload, ExpectedReply::Status)
            .await?;
        self.expect_ok(reply)
    }

    fn handle_bytes(&self, handle: &Handle) -> Result<&[u8], BcmrError> {
        self.ensure_usable()?;
        match self.handles.get(&handle.token) {
            Some(bytes) if bytes == &handle.bytes => Ok(bytes),
            _ => Err(BcmrError::InvalidInput(
                "SFTP handle is closed or belongs to another client".into(),
            )),
        }
    }

    fn expect_ok(&mut self, reply: Reply) -> Result<(), BcmrError> {
        match reply {
            Reply::Status(status) if status.code == StatusCode::Ok => Ok(()),
            Reply::Status(status) => Err(status.into_error()),
            _ => unreachable!("reply decoder enforces STATUS for this request"),
        }
    }

    fn status_failure(&mut self, status: StatusError) -> BcmrError {
        if status.code == StatusCode::Ok {
            self.protocol_error("server returned OK without the required result payload")
        } else {
            status.into_error()
        }
    }

    fn ensure_usable(&self) -> Result<(), BcmrError> {
        if !self.is_usable() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "SFTP session was closed after cancellation or a protocol/transport error",
            )
            .into());
        }
        Ok(())
    }

    async fn request(
        &mut self,
        kind: u8,
        payload: &[u8],
        expected: ExpectedReply,
    ) -> Result<Reply, BcmrError> {
        self.ensure_usable()?;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let mut packet = Vec::with_capacity(payload.len() + 5);
        packet.push(kind);
        packet.extend_from_slice(&id.to_be_bytes());
        packet.extend_from_slice(payload);
        self.exchange(&packet, REQUEST_TIMEOUT, move |bytes| {
            wire::decode_reply(bytes, id, expected)
        })
        .await
    }

    async fn exchange<T: Send, F>(
        &mut self,
        packet: &[u8],
        deadline: Duration,
        decode: F,
    ) -> Result<T, BcmrError>
    where
        F: FnOnce(&[u8]) -> Result<T, BcmrError> + Send,
    {
        self.ensure_usable()?;
        let mut guard = RequestGuard {
            client: self,
            complete: false,
        };
        let result = tokio::time::timeout(deadline, async {
            let writer = guard.client.stdin.as_mut().expect("usable stdin pipe");
            wire::write_frame(writer, packet).await?;
            let bytes = wire::read_frame(&mut guard.client.stdout).await?;
            decode(&bytes)
        })
        .await;
        match result {
            Ok(Ok(value)) => {
                guard.complete = true;
                Ok(value)
            }
            Ok(Err(error)) => {
                // Give the bounded stderr reader a chance to capture the reason
                // OpenSSH closed stdout (auth/subsystem/configuration failures).
                tokio::task::yield_now().await;
                Err(guard.client.with_diagnostics(error))
            }
            Err(_) => Err(guard.client.with_diagnostics(
                std::io::Error::new(std::io::ErrorKind::TimedOut, "SFTP request timed out").into(),
            )),
        }
    }

    fn with_diagnostics(&self, error: BcmrError) -> BcmrError {
        let captured = self.stderr.lock();
        if captured.is_empty() {
            return error;
        }
        let diagnostic = safe_diagnostic(String::from_utf8_lossy(&captured).trim());
        match error {
            BcmrError::Io(error) => {
                std::io::Error::new(error.kind(), format!("{error}; ssh: {diagnostic}")).into()
            }
            other => other,
        }
    }

    fn protocol_error(&mut self, message: &str) -> BcmrError {
        self.terminate();
        wire::invalid(message)
    }

    fn terminate(&mut self) {
        self.poisoned = true;
        self.stdin.take();
        self.handles.clear();
        let _ = self.child.start_kill();
    }
}

impl Drop for SftpClient {
    fn drop(&mut self) {
        self.terminate();
        self.stderr_task.abort();
    }
}

struct RequestGuard<'a> {
    client: &'a mut SftpClient,
    complete: bool,
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.client.terminate();
        }
    }
}

fn ssh_args(target: &str) -> Result<Vec<String>, BcmrError> {
    use std::io::IsTerminal;
    ssh_args_for(target, std::io::stdin().is_terminal())
}

fn ssh_args_for(target: &str, interactive: bool) -> Result<Vec<String>, BcmrError> {
    if target.is_empty()
        || target.starts_with('-')
        || target.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(BcmrError::InvalidInput(
            "Invalid SFTP SSH destination".into(),
        ));
    }
    let destination = crate::core::ssh_options::destination_arg(target);
    if destination.is_empty()
        || destination.starts_with('-')
        || destination
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(BcmrError::InvalidInput(
            "Invalid SFTP SSH destination".into(),
        ));
    }
    // Command-line -o values precede configuration and enforce an independent
    // bounded-liveness connection even if ~/.ssh/config requests multiplexing.
    let mut args = vec![
        "-o".into(),
        "ControlMaster=no".into(),
        "-o".into(),
        "ControlPath=none".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=20".into(),
    ];
    if !interactive {
        args.extend(["-o".into(), "BatchMode=yes".into()]);
    }
    args.extend(crate::core::ssh_options::extra_args(target));
    args.extend(["-s".into(), destination, "sftp".into()]);
    Ok(args)
}

fn valid_path(path: &[u8]) -> Result<&[u8], BcmrError> {
    if path.len() > wire::MAX_PATH_LEN || path.contains(&0) {
        return Err(BcmrError::InvalidInput(
            "SFTP path exceeds the length limit or contains NUL".into(),
        ));
    }
    Ok(path)
}

fn path_payload(path: &str) -> Result<Vec<u8>, BcmrError> {
    let mut payload = Vec::with_capacity(path.len().min(wire::MAX_PATH_LEN) + 4);
    wire::put_string(valid_path(path.as_bytes())?, &mut payload);
    Ok(payload)
}

fn unsupported(message: &str) -> BcmrError {
    StatusError {
        code: StatusCode::Unsupported,
        message: message.into(),
    }
    .into_error()
}

fn safe_diagnostic(message: &str) -> String {
    message
        .chars()
        .take(STDERR_LIMIT)
        .flat_map(|character| {
            if character.is_control() {
                character.escape_default().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
