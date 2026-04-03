use crate::core::error::BcmrError;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

static SSH_COMPRESS: AtomicBool = AtomicBool::new(false);

pub fn set_ssh_compression(enabled: bool) {
    SSH_COMPRESS.store(enabled, Ordering::Relaxed);
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

fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
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

/// Transfer options shared between local copy and remote operations.
#[derive(Clone, Default)]
pub struct TransferOptions {
    pub preserve: bool,
    pub verify: bool,
    pub resume: bool,
    pub strict: bool,
    pub append: bool,
}

pub type RemoteTransferOptions = TransferOptions;

struct ResumeDecision {
    skip_bytes: u64,
    use_append_mode: bool,
    skip_entirely: bool,
}

async fn check_resume_state(
    opts: &RemoteTransferOptions,
    existing_size: Option<u64>,
    source_size: u64,
    existing_full_hash: impl AsyncFnOnce() -> Result<String, BcmrError>,
    source_full_hash: impl AsyncFnOnce() -> Result<String, BcmrError>,
    existing_partial_hash: impl AsyncFnOnce(u64) -> Result<String, BcmrError>,
) -> Result<ResumeDecision, BcmrError> {
    if !(opts.resume || opts.append || opts.strict) {
        return Ok(ResumeDecision {
            skip_bytes: 0,
            use_append_mode: false,
            skip_entirely: false,
        });
    }

    let existing_size = match existing_size {
        Some(s) => s,
        None => {
            return Ok(ResumeDecision {
                skip_bytes: 0,
                use_append_mode: false,
                skip_entirely: false,
            })
        }
    };

    if existing_size == source_size {
        if opts.strict {
            let ex_hash = existing_full_hash().await?;
            let src_hash = source_full_hash().await?;
            if ex_hash == src_hash {
                return Ok(ResumeDecision {
                    skip_bytes: 0,
                    use_append_mode: false,
                    skip_entirely: true,
                });
            }
        } else {
            return Ok(ResumeDecision {
                skip_bytes: 0,
                use_append_mode: false,
                skip_entirely: true,
            });
        }
    } else if existing_size < source_size {
        if opts.strict {
            let ex_hash = existing_full_hash().await?;
            let partial = existing_partial_hash(existing_size).await?;
            if ex_hash == partial {
                return Ok(ResumeDecision {
                    skip_bytes: existing_size,
                    use_append_mode: true,
                    skip_entirely: false,
                });
            }
        } else {
            return Ok(ResumeDecision {
                skip_bytes: existing_size,
                use_append_mode: true,
                skip_entirely: false,
            });
        }
    }

    Ok(ResumeDecision {
        skip_bytes: 0,
        use_append_mode: false,
        skip_entirely: false,
    })
}

// ── SSH connection multiplexing ──
// Uses ControlMaster to reuse a single TCP connection for all SSH commands
// to the same host. The first connection may prompt for a password (if TTY
// is available); subsequent connections piggyback on the master socket.

fn control_path(target: &str) -> String {
    let dir = std::env::temp_dir().join("bcmr-ssh");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{}.sock", target.replace(['@', ':', '/'], "_")))
        .to_string_lossy()
        .to_string()
}

fn is_interactive() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(libc::STDIN_FILENO) != 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn ssh_base_args(target: &str) -> Vec<String> {
    let cp = control_path(target);
    let mut args = vec![
        "-o".into(),
        format!("ControlPath={}", cp),
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        "ControlPersist=300".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if !is_interactive() {
        args.extend(["-o".into(), "BatchMode=yes".into()]);
    }
    if SSH_COMPRESS.load(Ordering::Relaxed) {
        args.extend(["-o".into(), "Compression=yes".into()]);
    }
    args
}

/// Per-worker SSH args with separate ControlPath for true parallel TCP connections.
///
/// The default ControlMaster multiplexing serializes all SSH channels through
/// one TCP connection and one encryption context, bottlenecking parallel throughput.
/// By giving each worker its own control socket, each gets an independent TCP stream.
/// mscp (https://github.com/upa/mscp) demonstrated 5.98x speedup with 8 connections.
fn ssh_base_args_for_worker(target: &str, worker_id: usize) -> Vec<String> {
    let dir = std::env::temp_dir().join("bcmr-ssh");
    let _ = std::fs::create_dir_all(&dir);
    let cp = dir
        .join(format!(
            "w{}_{}.sock",
            worker_id,
            target.replace(['@', ':', '/'], "_")
        ))
        .to_string_lossy()
        .to_string();

    let mut args = vec![
        "-o".into(),
        format!("ControlPath={}", cp),
        "-o".into(),
        "ControlMaster=auto".into(),
        "-o".into(),
        "ControlPersist=60".into(), // shorter persist for worker sockets
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if !is_interactive() {
        args.extend(["-o".into(), "BatchMode=yes".into()]);
    }
    if SSH_COMPRESS.load(Ordering::Relaxed) {
        args.extend(["-o".into(), "Compression=yes".into()]);
    }
    args
}

/// Create an SSH command that uses a per-worker control socket for true
/// parallel TCP connections. Use this in parallel transfer workers.
fn ssh_command_for_worker(target: &str, worker_id: usize) -> Command {
    let args = ssh_base_args_for_worker(target, worker_id);
    let mut cmd = Command::new("ssh");
    for arg in &args {
        cmd.arg(arg);
    }
    cmd.arg(target);
    cmd
}

/// Create an SSH command with optional worker isolation.
/// When worker_id is Some, uses a per-worker control socket for independent
/// TCP connections (5-6x throughput improvement for parallel transfers).
/// When None, uses the shared ControlMaster connection.
fn make_ssh_cmd(target: &str, worker_id: Option<usize>) -> Command {
    match worker_id {
        Some(id) => ssh_command_for_worker(target, id),
        None => ssh_command(target),
    }
}

fn ssh_command(target: &str) -> Command {
    let args = ssh_base_args(target);
    let mut cmd = Command::new("ssh");
    for arg in &args {
        cmd.arg(arg);
    }
    cmd.arg(target);
    cmd
}

fn ssh_error_message(stderr: &str, context: &str) -> String {
    let stderr_lower = stderr.to_lowercase();
    if stderr_lower.contains("connection refused") {
        format!(
            "{}: SSH connection refused (is sshd running on the host?)",
            context
        )
    } else if stderr_lower.contains("no route to host")
        || stderr_lower.contains("network is unreachable")
    {
        format!("{}: host unreachable (check network connectivity)", context)
    } else if stderr_lower.contains("permission denied") {
        format!(
            "{}: SSH authentication failed (check credentials/keys)",
            context
        )
    } else if stderr_lower.contains("could not resolve")
        || stderr_lower.contains("name or service not known")
    {
        format!("{}: unknown host (check hostname)", context)
    } else if stderr_lower.contains("no such file") || stderr_lower.contains("not a regular file") {
        format!("{}: remote file not found", context)
    } else if stderr_lower.contains("timed out") || stderr_lower.contains("connection timed out") {
        format!("{}: SSH connection timed out", context)
    } else {
        format!("{}: {}", context, stderr.trim())
    }
}

pub async fn validate_ssh_connection(remote: &RemotePath) -> Result<(), BcmrError> {
    let target = remote.ssh_target();
    // Establishes the ControlMaster connection; may prompt for password if TTY
    let output = ssh_command(&target).arg("echo ok").output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot connect to '{}'", target),
        )));
    }
    Ok(())
}

pub async fn remote_file_size(remote: &RemotePath) -> Result<Option<u64>, BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "stat -c '%s' '{}' 2>/dev/null || stat -f '%z' '{}'",
            shell_escape(&remote.path),
            shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.trim().parse::<u64>().ok())
}

pub async fn remote_stat(remote: &RemotePath) -> Result<RemoteFileInfo, BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "stat -c '%F %s' '{}' 2>/dev/null || stat -f '%HT %z' '{}'",
            shell_escape(&remote.path),
            shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot stat remote path '{}'", remote),
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();

    // Linux stat -c '%F %s': "regular file 12345" or "directory 4096"
    // macOS stat -f '%HT %z': "Regular File 12345" or "Directory 4096"
    let is_dir = line.to_lowercase().starts_with("directory");
    let size: u64 = line
        .rsplit_once(' ')
        .and_then(|(_, s)| s.parse().ok())
        .unwrap_or(0);

    Ok(RemoteFileInfo { is_dir, size })
}

pub async fn remote_total_size(remote: &RemotePath, recursive: bool) -> Result<u64, BcmrError> {
    let info = remote_stat(remote).await?;

    if !info.is_dir {
        return Ok(info.size);
    }

    if !recursive {
        return Err(BcmrError::InvalidInput(format!(
            "Remote source '{}' is a directory. Use -r flag for recursive copy.",
            remote
        )));
    }

    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "find '{}' -type f -exec stat -c '%s' {{}} + 2>/dev/null || find '{}' -type f -exec stat -f '%z' {{}} +",
            shell_escape(&remote.path), shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        return Ok(0);
    }

    let total: u64 = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .sum();

    Ok(total)
}

pub async fn remote_list_files(remote: &RemotePath) -> Result<Vec<(String, u64, bool)>, BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "find '{}' -printf '%P\\0%s\\0%y\\0' 2>/dev/null || find '{}' ! -path '{}' -exec stat -f '%N\\0%z\\0%HT\\0' {{}} +",
            shell_escape(&remote.path), shell_escape(&remote.path), shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Cannot list remote directory '{}'", remote),
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut entries = Vec::new();

    let mut i = 0;
    while i + 2 < fields.len() {
        let rel_path = fields[i].to_string();
        let size: u64 = fields[i + 1].parse().unwrap_or(0);
        let is_dir = fields[i + 2] == "d" || fields[i + 2].to_lowercase().contains("directory");
        i += 3;

        if rel_path.is_empty() {
            continue;
        }
        entries.push((rel_path, size, is_dir));
    }

    Ok(entries)
}

#[allow(clippy::too_many_arguments)]
pub async fn download_file(
    remote: &RemotePath,
    local_dst: &Path,
    progress_callback: &impl Fn(u64),
    skip_callback: &impl Fn(u64),
    on_new_file: &impl Fn(&str, u64),
    file_size: u64,
    opts: &RemoteTransferOptions,
    worker_id: Option<usize>,
) -> Result<(), BcmrError> {
    let file_name = remote.path.rsplit('/').next().unwrap_or(&remote.path);
    on_new_file(file_name, file_size);

    if let Some(parent) = local_dst.parent() {
        if !parent.exists() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let existing_size = if local_dst.exists() {
        Some(local_dst.metadata()?.len())
    } else {
        None
    };
    let local_path_for_hash = local_dst.to_path_buf();
    let remote_for_hash = remote.clone();
    let remote_for_partial = remote.clone();

    let decision = check_resume_state(
        opts,
        existing_size,
        file_size,
        async move || {
            tokio::task::spawn_blocking(move || {
                crate::core::checksum::calculate_hash(&local_path_for_hash)
            })
            .await
            .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
            .map_err(BcmrError::Io)
        },
        async move || remote_file_hash(&remote_for_hash, None).await,
        async move |limit| remote_file_hash(&remote_for_partial, Some(limit)).await,
    )
    .await?;

    if decision.skip_entirely {
        skip_callback(file_size);
        return Ok(());
    }

    if decision.skip_bytes > 0 {
        skip_callback(decision.skip_bytes);
    }

    let ssh_cmd = if decision.use_append_mode {
        format!(
            "tail -c +{} '{}'",
            decision.skip_bytes + 1,
            shell_escape(&remote.path)
        )
    } else {
        format!("cat '{}'", shell_escape(&remote.path))
    };

    let mut child = make_ssh_cmd(&remote.ssh_target(), worker_id)
        .arg(ssh_cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("Failed to capture SSH stdout".to_string()))?;
    let mut stderr_pipe = child.stderr.take();

    let mut dst_file = if decision.use_append_mode {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(local_dst)
            .await?
    } else {
        tokio::fs::File::create(local_dst).await?
    };
    let mut buffer = vec![0u8; 4 * 1024 * 1024];

    let io_result: Result<(), BcmrError> = async {
        loop {
            let n = stdout.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            dst_file.write_all(&buffer[..n]).await?;
            progress_callback(n as u64);
        }
        Ok(())
    }
    .await;

    if let Err(e) = io_result {
        let _ = child.kill().await;
        if !decision.use_append_mode {
            let _ = tokio::fs::remove_file(local_dst).await;
        }
        return Err(e);
    }

    let status = child.wait().await?;
    if !status.success() {
        let mut stderr_buf = String::new();
        if let Some(ref mut pipe) = stderr_pipe {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            stderr_buf = String::from_utf8_lossy(&buf).to_string();
        }
        if !decision.use_append_mode {
            let _ = tokio::fs::remove_file(local_dst).await;
        }
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr_buf,
            &format!("Download failed for '{}'", remote),
        )));
    }

    if opts.verify {
        let local_path = local_dst.to_path_buf();
        let local_hash =
            tokio::task::spawn_blocking(move || crate::core::checksum::calculate_hash(&local_path))
                .await
                .map_err(|e| BcmrError::InvalidInput(e.to_string()))??;
        let remote_hash = remote_file_hash(remote, None).await?;
        if local_hash != remote_hash {
            return Err(BcmrError::InvalidInput(format!(
                "Verification failed: {} -> '{}'",
                remote,
                local_dst.display()
            )));
        }
    }

    if opts.preserve {
        apply_remote_attrs_locally(remote, local_dst).await?;
    }

    Ok(())
}

pub async fn remote_file_hash(
    remote: &RemotePath,
    limit: Option<u64>,
) -> Result<String, BcmrError> {
    let cmd = match limit {
        Some(n) => format!("head -c {} '{}'", n, shell_escape(&remote.path)),
        None => format!("cat '{}'", shell_escape(&remote.path)),
    };

    let mut child = ssh_command(&remote.ssh_target())
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("Failed to capture SSH stdout".to_string()))?;

    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = stdout.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let status = child.wait().await?;
    if !status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "Failed to hash remote file '{}'",
            remote
        )));
    }

    Ok(hasher.finalize().to_hex().to_string())
}

pub async fn upload_file(
    local_src: &Path,
    remote: &RemotePath,
    progress_callback: &impl Fn(u64),
    skip_callback: &impl Fn(u64),
    on_new_file: &impl Fn(&str, u64),
    opts: &RemoteTransferOptions,
    worker_id: Option<usize>,
) -> Result<(), BcmrError> {
    let file_size = local_src.metadata()?.len();
    let file_name = local_src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    on_new_file(&file_name, file_size);

    if let Some(parent) = remote.path.rsplit_once('/') {
        if !parent.0.is_empty() {
            let mkdir_out = make_ssh_cmd(&remote.ssh_target(), worker_id)
                .arg(format!("mkdir -p '{}'", shell_escape(parent.0)))
                .output()
                .await?;
            if !mkdir_out.status.success() {
                let stderr = String::from_utf8_lossy(&mkdir_out.stderr);
                return Err(BcmrError::InvalidInput(ssh_error_message(
                    &stderr,
                    &format!("Failed to create remote directory '{}'", parent.0),
                )));
            }
        }
    }

    let existing_size = remote_file_size(remote).await.ok().flatten();
    let remote_for_hash = remote.clone();
    let local_path_for_hash = local_src.to_path_buf();
    let local_path_for_partial = local_src.to_path_buf();

    let decision = check_resume_state(
        opts,
        existing_size,
        file_size,
        async move || remote_file_hash(&remote_for_hash, None).await,
        async move || {
            tokio::task::spawn_blocking(move || {
                crate::core::checksum::calculate_hash(&local_path_for_hash)
            })
            .await
            .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
            .map_err(BcmrError::Io)
        },
        async move |limit| {
            tokio::task::spawn_blocking(move || {
                crate::core::checksum::calculate_partial_hash(&local_path_for_partial, limit)
            })
            .await
            .map_err(|e| BcmrError::InvalidInput(e.to_string()))?
            .map_err(BcmrError::Io)
        },
    )
    .await?;

    if decision.skip_entirely {
        skip_callback(file_size);
        return Ok(());
    }

    if decision.skip_bytes > 0 {
        skip_callback(decision.skip_bytes);
    }

    let cat_op = if decision.use_append_mode { ">>" } else { ">" };
    let mut child = make_ssh_cmd(&remote.ssh_target(), worker_id)
        .arg(format!("cat {} '{}'", cat_op, shell_escape(&remote.path)))
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| BcmrError::InvalidInput("Failed to capture SSH stdin".to_string()))?;

    let mut src_file = tokio::fs::File::open(local_src).await?;

    if decision.skip_bytes > 0 {
        use tokio::io::AsyncSeekExt;
        src_file
            .seek(std::io::SeekFrom::Start(decision.skip_bytes))
            .await?;
    }

    let mut buffer = vec![0u8; 4 * 1024 * 1024];

    let io_result: Result<(), BcmrError> = async {
        loop {
            let n = src_file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            stdin.write_all(&buffer[..n]).await?;
            progress_callback(n as u64);
        }
        Ok(())
    }
    .await;

    drop(stdin);

    if let Err(e) = io_result {
        let _ = child.kill().await;
        return Err(e);
    }

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Upload failed for '{}' -> {}", local_src.display(), remote),
        )));
    }

    if opts.verify && !verify_remote_file(local_src, remote).await? {
        return Err(BcmrError::InvalidInput(format!(
            "Verification failed: '{}' -> {}",
            local_src.display(),
            remote
        )));
    }

    if opts.preserve {
        preserve_remote_attrs(local_src, remote).await?;
    }

    Ok(())
}

async fn preserve_remote_attrs(local_src: &Path, remote: &RemotePath) -> Result<(), BcmrError> {
    let meta = local_src.metadata()?;

    let mode = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o7777
        }
        #[cfg(not(unix))]
        {
            0o644u32
        }
    };

    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let atime_secs = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            meta.atime() as u64
        }
        #[cfg(not(unix))]
        {
            mtime_secs
        }
    };

    let mtime_ts = unix_to_touch_ts(mtime_secs as i64);
    let atime_ts = unix_to_touch_ts(atime_secs as i64);
    // touch -t sets both atime+mtime; use -a/-m to set them independently
    let cmd = format!(
        "TZ=UTC touch -m -t '{}' '{}'; TZ=UTC touch -a -t '{}' '{}'; chmod {:o} '{}'",
        mtime_ts,
        shell_escape(&remote.path),
        atime_ts,
        shell_escape(&remote.path),
        mode,
        shell_escape(&remote.path)
    );
    let attr_out = ssh_command(&remote.ssh_target()).arg(cmd).output().await?;
    if !attr_out.status.success() {
        let stderr = String::from_utf8_lossy(&attr_out.stderr);
        return Err(BcmrError::InvalidInput(ssh_error_message(
            &stderr,
            &format!("Failed to set attributes on '{}'", remote),
        )));
    }
    Ok(())
}

async fn get_remote_attrs(remote: &RemotePath) -> Result<(i64, i64, u32), BcmrError> {
    let output = ssh_command(&remote.ssh_target())
        .arg(format!(
            "stat -c '%X %Y %a' '{}' 2>/dev/null || stat -f '%a %m %Lp' '{}'",
            shell_escape(&remote.path),
            shell_escape(&remote.path)
        ))
        .output()
        .await?;

    if !output.status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "Cannot stat remote file '{}'",
            remote
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    let atime_secs: i64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mtime_secs: i64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mode: u32 = parts
        .get(2)
        .and_then(|s| u32::from_str_radix(s, 8).ok())
        .unwrap_or(0o644);

    Ok((atime_secs, mtime_secs, mode))
}

fn apply_local_attrs(
    local_path: &Path,
    atime_secs: i64,
    mtime_secs: i64,
    _mode: u32,
) -> Result<(), BcmrError> {
    let atime = filetime::FileTime::from_unix_time(atime_secs, 0);
    let mtime = filetime::FileTime::from_unix_time(mtime_secs, 0);
    filetime::set_file_times(local_path, atime, mtime)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(local_path, std::fs::Permissions::from_mode(_mode))?;
    }

    Ok(())
}

async fn apply_remote_attrs_locally(
    remote: &RemotePath,
    local_path: &Path,
) -> Result<(), BcmrError> {
    let (atime_secs, mtime_secs, mode) = get_remote_attrs(remote).await?;
    apply_local_attrs(local_path, atime_secs, mtime_secs, mode)?;
    Ok(())
}

pub async fn verify_remote_file(local_src: &Path, remote: &RemotePath) -> Result<bool, BcmrError> {
    use crate::core::checksum;

    let local_path = local_src.to_path_buf();
    let local_hash = tokio::task::spawn_blocking(move || checksum::calculate_hash(&local_path))
        .await
        .map_err(|e| BcmrError::InvalidInput(e.to_string()))??;

    let remote_hash = remote_file_hash(remote, None).await?;

    Ok(local_hash == remote_hash)
}

fn unix_to_touch_ts(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;

    // Civil days from epoch to y/m/d (Howard Hinnant's algorithm)
    let z = days as i32 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}{:02}{:02}{:02}{:02}.{:02}",
        y, m, d, hours, minutes, seconds
    )
}

pub async fn download_directory(
    remote: &RemotePath,
    local_dst: &Path,
    progress_callback: &impl Fn(u64),
    skip_callback: &impl Fn(u64),
    on_new_file: &impl Fn(&str, u64),
    excludes: &[regex::Regex],
    opts: &RemoteTransferOptions,
) -> Result<(), BcmrError> {
    let entries = remote_list_files(remote).await?;

    let entries: Vec<_> = entries
        .into_iter()
        .filter(|(rel_path, _, _)| {
            !crate::core::traversal::is_excluded(std::path::Path::new(rel_path), excludes)
        })
        .collect();

    for (rel_path, _, is_dir) in &entries {
        if *is_dir {
            let dir_path = local_dst.join(rel_path);
            if !dir_path.exists() {
                tokio::fs::create_dir_all(&dir_path).await?;
            }
        }
    }

    for (rel_path, size, is_dir) in &entries {
        if *is_dir {
            continue;
        }
        let file_remote = RemotePath {
            user: remote.user.clone(),
            host: remote.host.clone(),
            path: format!("{}/{}", remote.path, rel_path),
        };
        let local_file = local_dst.join(rel_path);
        download_file(
            &file_remote,
            &local_file,
            progress_callback,
            skip_callback,
            on_new_file,
            *size,
            opts,
            None,
        )
        .await?;
    }

    Ok(())
}

pub async fn ensure_remote_tree(local_src: &Path, remote: &RemotePath) -> Result<(), BcmrError> {
    use crate::core::traversal;

    ssh_command(&remote.ssh_target())
        .arg(format!("mkdir -p '{}'", shell_escape(&remote.path)))
        .output()
        .await?;

    let excludes: Vec<regex::Regex> = Vec::new();
    let mut dirs = Vec::new();

    for entry in traversal::walk(local_src, true, false, 1, &excludes) {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let rel = path.strip_prefix(local_src)?;
            dirs.push(rel.to_path_buf());
        }
    }

    if !dirs.is_empty() {
        let mkdir_cmd = dirs
            .iter()
            .map(|d| {
                format!(
                    "'{}/{}'",
                    shell_escape(&remote.path),
                    shell_escape(&d.display().to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(" ");

        ssh_command(&remote.ssh_target())
            .arg(format!("mkdir -p {}", mkdir_cmd))
            .output()
            .await?;
    }

    Ok(())
}

pub async fn upload_directory(
    local_src: &Path,
    remote: &RemotePath,
    progress_callback: &impl Fn(u64),
    skip_callback: &impl Fn(u64),
    on_new_file: &impl Fn(&str, u64),
    excludes: &[regex::Regex],
    opts: &RemoteTransferOptions,
) -> Result<(), BcmrError> {
    use crate::core::traversal;

    let output = ssh_command(&remote.ssh_target())
        .arg(format!("mkdir -p '{}'", shell_escape(&remote.path)))
        .output()
        .await?;

    if !output.status.success() {
        return Err(BcmrError::InvalidInput(format!(
            "Failed to create remote directory '{}'",
            remote
        )));
    }

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in traversal::walk(local_src, true, false, 1, excludes) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(local_src)?;

        if path.is_dir() {
            dirs.push(rel.to_path_buf());
        } else if path.is_file() {
            files.push((path.to_path_buf(), rel.to_path_buf()));
        }
    }

    if !dirs.is_empty() {
        let mkdir_cmd = dirs
            .iter()
            .map(|d| {
                format!(
                    "'{}/{}'",
                    shell_escape(&remote.path),
                    shell_escape(&d.display().to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(" ");

        ssh_command(&remote.ssh_target())
            .arg(format!("mkdir -p {}", mkdir_cmd))
            .output()
            .await?;
    }

    for (local_path, rel_path) in &files {
        let file_remote = RemotePath {
            user: remote.user.clone(),
            host: remote.host.clone(),
            path: format!("{}/{}", remote.path, rel_path.display()),
        };
        upload_file(
            local_path,
            &file_remote,
            progress_callback,
            skip_callback,
            on_new_file,
            opts,
            None,
        )
        .await?;
    }

    Ok(())
}

pub async fn complete_remote_path(partial: &str) -> Vec<String> {
    let remote = match parse_remote_path(partial) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let (dir, prefix) = if remote.path.ends_with('/') || remote.path == "." {
        (remote.path.clone(), String::new())
    } else if let Some(pos) = remote.path.rfind('/') {
        (
            remote.path[..=pos].to_string(),
            remote.path[pos + 1..].to_string(),
        )
    } else {
        (".".to_string(), remote.path.clone())
    };

    let target = remote.ssh_target();
    let output = match ssh_command(&target)
        .arg(format!("ls -1ap '{}' 2>/dev/null", shell_escape(&dir)))
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let base = if dir == "." { String::new() } else { dir };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| *l != "./" && *l != "../")
        .filter(|l| prefix.is_empty() || l.starts_with(&prefix))
        .map(|l| format!("{}:{}{}", target, base, l))
        .collect()
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
        assert_eq!(shell_escape("simple"), "simple");
    }

    #[test]
    fn test_shell_escape_with_quotes() {
        assert_eq!(shell_escape("it's"), "it'\\''s");
    }

    #[test]
    fn test_unix_to_touch_ts_epoch() {
        assert_eq!(unix_to_touch_ts(0), "197001010000.00");
    }

    #[test]
    fn test_unix_to_touch_ts_known_date() {
        // 2020-01-01 12:00:00 UTC = 1577880000
        let ts = unix_to_touch_ts(1577880000);
        assert_eq!(ts, "202001011200.00");
    }

    #[test]
    fn test_unix_to_touch_ts_with_seconds() {
        // 2020-06-15 08:30:45 UTC = 1592210445 (approx, computed manually)
        let ts = unix_to_touch_ts(1592210445);
        assert!(ts.ends_with(".45"));
    }
}
