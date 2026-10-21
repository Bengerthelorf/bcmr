use anyhow::{bail, Context, Result};
use std::io::IsTerminal;
use std::process::Stdio;
use tokio::process::Command;

#[derive(Clone, Copy)]
enum DeployOutcome {
    Installed,
    Skipped,
}

pub async fn run(targets: &[String], remote_path: &str, sudo: bool) -> Result<()> {
    let interactive = std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && !crate::config::is_json_mode();
    let mut results = Vec::with_capacity(targets.len());
    let mut installed = 0;
    let mut skipped = 0;
    let mut failed = 0;

    // Keep password prompts attached to one host at a time. Each failure is
    // recorded before advancing, so rerunning the command skips current hosts.
    for target in targets {
        let result = deploy_target(target, remote_path, sudo, interactive).await;
        match &result {
            Ok(DeployOutcome::Installed) => installed += 1,
            Ok(DeployOutcome::Skipped) => skipped += 1,
            Err(error) => {
                failed += 1;
                eprintln!("  {target}: failed: {error:#}");
            }
        }
        results.push((target, result));
    }

    eprintln!("\nDeployment summary:");
    for (target, result) in results {
        match result {
            Ok(DeployOutcome::Installed) => eprintln!("  {target}: installed"),
            Ok(DeployOutcome::Skipped) => eprintln!("  {target}: skipped (already current)"),
            Err(error) => eprintln!("  {target}: failed: {error:#}"),
        }
    }
    eprintln!("  {installed} installed, {skipped} skipped, {failed} failed");
    if failed > 0 {
        bail!("Deployment failed on {failed} of {} targets", targets.len());
    }
    Ok(())
}

async fn deploy_target(
    target: &str,
    remote_path: &str,
    sudo: bool,
    interactive: bool,
) -> Result<DeployOutcome> {
    validate_target(target)?;
    validate_path(remote_path)?;
    let remote_path = if remote_path.starts_with('~') {
        crate::core::remote::expand_remote_tilde(target, remote_path).await?
    } else if remote_path.starts_with('/') {
        remote_path.to_owned()
    } else {
        let working_dir = ssh(target, "pwd -P").await?;
        format!("{}/{}", working_dir.trim_end(), remote_path)
    };
    validate_path(&remote_path)?;
    if !remote_path.starts_with('/') {
        bail!("could not resolve an absolute installation path: {remote_path:?}");
    }
    eprintln!("\nDeploying bcmr to {target}:{remote_path}");

    let remote_info = ssh(target, "uname -sm").await?;
    let (remote_os, remote_arch) = parse_uname(remote_info.trim())?;
    eprintln!("  Remote: {remote_os} {remote_arch}");
    let expected_version = format!("bcmr {}", env!("CARGO_PKG_VERSION"));
    let current_version = probe_version(target, &remote_path).await?;
    if current_version == expected_version {
        report_installation(target, &remote_path, &current_version).await?;
        eprintln!("  Same version at the installation path. Nothing to do.");
        return Ok(DeployOutcome::Skipped);
    }
    if !current_version.is_empty() {
        eprintln!("  Installation path currently reports: {current_version}");
    }
    ssh(
        target,
        &format!(
            "if [ -d {} ]; then printf '%s\\n' 'installation path is a directory; expected an executable file' >&2; exit 1; fi",
            shell_escape(&remote_path)
        ),
    )
    .await?;

    // Upload into a private temporary file before touching an existing binary.
    // This is also necessary for sudo: SFTP itself runs as the SSH user.
    let staging = ssh(target, "mktemp -t bcmr-deploy.XXXXXX").await?;
    let staging = staging.trim();
    validate_path(staging).context("invalid remote staging path from mktemp")?;
    if !staging.starts_with('/') {
        bail!("mktemp did not return an absolute remote staging path");
    }
    let install_result: Result<()> = async {
        if remote_os == std::env::consts::OS && remote_arch == std::env::consts::ARCH {
            eprintln!("  Same platform. Transferring local binary...");
            scp_to(target, &std::env::current_exe()?, staging).await?;
        } else {
            eprintln!("  Cross-platform. Downloading matching GitHub release...");
            download_release(target, remote_os, remote_arch, staging).await?;
        }
        install_binary(target, staging, &remote_path, sudo, interactive).await?;
        let installed_version = probe_version(target, &remote_path).await?;
        if installed_version.is_empty() {
            bail!(
                "installation at {} failed --version or returned no usable version; expected {}",
                remote_path,
                expected_version
            );
        }
        if installed_version != expected_version {
            bail!(
                "installation at {} reports {:?}; expected {}",
                remote_path,
                installed_version,
                expected_version
            );
        }
        report_installation(target, &remote_path, &installed_version).await?;
        Ok(())
    }
    .await;

    // Cleanup is best effort and cannot replace the actual per-host result.
    let _ = ssh(target, &format!("rm -f -- {}", shell_escape(staging))).await;
    install_result?;
    Ok(DeployOutcome::Installed)
}

fn validate_target(target: &str) -> Result<()> {
    if target.is_empty()
        || target.starts_with('-')
        || target
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        bail!("invalid SSH target: {target:?}");
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<()> {
    if path.is_empty() || path.chars().any(char::is_control) {
        bail!("invalid installation path: {path:?}");
    }
    Ok(())
}

async fn probe_version(target: &str, path: &str) -> Result<String> {
    // Missing or unusable installations need replacement. SSH failures still
    // propagate: the fallback is inside the remote command, not around SSH.
    let result = ssh(
        target,
        &format!(
            "if version=$({} --version 2>/dev/null); then printf '%s' \"$version\"; fi",
            shell_escape(path)
        ),
    )
    .await?;
    Ok(result.trim().to_owned())
}

async fn report_installation(target: &str, path: &str, version: &str) -> Result<()> {
    eprintln!("  Installed path: {path}");
    eprintln!("  Installed version: {version}");
    let resolved = ssh(target, "command -v bcmr 2>/dev/null || true").await?;
    let resolved = resolved.trim();
    if resolved.is_empty() {
        eprintln!("  PATH: bcmr not found on the remote's non-interactive SSH PATH.");
        eprintln!(
            "  Add the installation directory to the remote PATH to enable fast-path transfers."
        );
    } else {
        let path_version = probe_version(target, resolved).await?;
        let display_version = if path_version.is_empty() {
            "unavailable"
        } else {
            &path_version
        };
        eprintln!("  PATH resolves to: {resolved}");
        eprintln!("  PATH version: {display_version}");
        if resolved != path {
            eprintln!("  Warning: a different PATH entry shadows the selected installation.");
            eprintln!(
                "  To upgrade that installation, use --path {} (and --sudo if needed).",
                shell_escape(resolved)
            );
        }
    }
    Ok(())
}

async fn install_binary(
    target: &str,
    staging: &str,
    path: &str,
    sudo: bool,
    interactive: bool,
) -> Result<()> {
    let dir = path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .filter(|dir| !dir.is_empty())
        .unwrap_or("/");
    // Keep privileged operations in the same SSH/PTY session so normal
    // per-terminal sudo authentication can be reused without retaining a password.
    let prefix = match (sudo, interactive) {
        (true, true) => "sudo -- ",
        (true, false) => "sudo -n -- ",
        (false, _) => "",
    };
    let command = format!(
        "{prefix}mkdir -p -- {} && {prefix}test ! -d {} && {prefix}install -m 755 -- {} {}",
        shell_escape(dir),
        shell_escape(path),
        shell_escape(staging),
        shell_escape(path)
    );
    if sudo && interactive {
        eprintln!("  Installing with sudo on {target}; enter its password in the SSH terminal if requested.");
        let status = ssh_command(target)
            .arg("-t")
            .arg("--")
            .arg(crate::core::ssh_options::destination_arg(target))
            .arg(command)
            .stdin(Stdio::inherit())
            // SSH merges the remote PTY's output (including sudo prompts)
            // onto stdout. Keep it on the user's terminal when stdout is a log.
            .stdout(Stdio::from(std::io::stderr()))
            .stderr(Stdio::inherit())
            .status()
            .await?;
        if !status.success() {
            bail!("sudo installation failed on {target} (SSH exit {status})");
        }
        Ok(())
    } else {
        ssh(target, &command).await.with_context(|| {
            if sudo {
                "non-interactive --sudo requires passwordless permission for sudo -n mkdir/test/install; rerun from a terminal to enter the remote sudo password".to_string()
            } else {
                format!("cannot install at {path}; use --sudo for a protected directory or --path for a user-writable location")
            }
        })?;
        Ok(())
    }
}

async fn download_release(target: &str, os: &str, arch: &str, staging: &str) -> Result<()> {
    let asset_name = release_asset_name(os, arch)?;
    let url = format!(
        "https://github.com/Bengerthelorf/bcmr/releases/download/v{}/{}",
        env!("CARGO_PKG_VERSION"),
        asset_name
    );
    let command = format!(
        "work=$(mktemp -d -t bcmr-deploy.XXXXXX) || exit 1; \
         trap 'rm -rf -- \"$work\"' EXIT; \
         curl -fsSL --connect-timeout 10 --max-time 120 {} -o \"$work/archive.tar.gz\" && \
         tar xzf \"$work/archive.tar.gz\" -C \"$work\" bcmr && \
         cp -- \"$work/bcmr\" {}",
        shell_escape(&url),
        shell_escape(staging)
    );
    if ssh(target, &command).await.is_ok() {
        return Ok(());
    }
    eprintln!("  Direct download failed. Downloading locally...");
    let temporary = tempfile::tempdir()?;
    let archive = temporary.path().join(&asset_name);
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "10",
            "--max-time",
            "120",
            &url,
            "-o",
        ])
        .arg(&archive)
        .status()
        .await?;
    if !status.success() {
        bail!(
            "failed to download {asset_name} from release v{}",
            env!("CARGO_PKG_VERSION")
        );
    }
    let status = Command::new("tar")
        .arg("xzf")
        .arg(&archive)
        .arg("-C")
        .arg(temporary.path())
        .arg("bcmr")
        .status()
        .await?;
    if !status.success() {
        bail!("failed to extract {asset_name}");
    }
    scp_to(target, &temporary.path().join("bcmr"), staging).await
}

fn ssh_command(target: &str) -> Command {
    let mut command = Command::new("ssh");
    command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
    command.args(crate::core::transport::ssh::SSH_LIVENESS_ARGS);
    command.args(crate::core::ssh_options::extra_args(target));
    command
}

async fn ssh(target: &str, cmd: &str) -> Result<String> {
    let output = ssh_command(target)
        .arg("-T")
        .arg("--")
        .arg(crate::core::ssh_options::destination_arg(target))
        .arg(cmd)
        .stdin(Stdio::null())
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ssh command failed on {target}: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn scp_to(target: &str, local: &std::path::Path, remote_path: &str) -> Result<()> {
    // SFTP keeps upload paths out of the remote shell, including on older SSH.
    use tokio::io::AsyncWriteExt;
    let mut child = Command::new("sftp")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .args(crate::core::transport::ssh::SSH_LIVENESS_ARGS)
        .args(crate::core::ssh_options::sftp_args(target))
        .args(["-b", "-", "--"])
        .arg(crate::core::ssh_options::destination_arg(target))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().context("sftp stdin unavailable")?;
    let command = format!(
        "put {} {}\n",
        sftp_quote(&local.to_string_lossy())?,
        sftp_quote(remote_path)?
    );
    stdin.write_all(command.as_bytes()).await?;
    drop(stdin);
    if !child.wait().await?.success() {
        bail!(
            "sftp put failed: {} -> {target}:{remote_path}",
            local.display()
        );
    }
    Ok(())
}

fn sftp_quote(path: &str) -> Result<String> {
    if path.chars().any(char::is_control) {
        bail!("sftp path contains a control character: {path:?}");
    }
    Ok(format!(
        "\"{}\"",
        path.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn parse_uname(info: &str) -> Result<(&str, &str)> {
    let parts: Vec<&str> = info.split_whitespace().collect();
    if parts.len() < 2 {
        bail!("unexpected uname output: {}", info);
    }
    let os = match parts[0].to_lowercase().as_str() {
        "linux" => "linux",
        "darwin" => "macos",
        "freebsd" => "freebsd",
        _ => bail!("unsupported remote OS: {}", parts[0]),
    };
    let arch = match parts[1] {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        _ => bail!("unsupported remote architecture: {}", parts[1]),
    };
    Ok((os, arch))
}

fn release_asset_name(os: &str, arch: &str) -> Result<String> {
    let name = match (os, arch) {
        ("linux", "x86_64") => "bcmr-x86_64-linux.tar.gz",
        ("linux", "aarch64") => "bcmr-aarch64-linux.tar.gz",
        ("macos", "x86_64") => "bcmr-x86_64-macos.tar.gz",
        ("macos", "aarch64") => "bcmr-aarch64-macos.tar.gz",
        ("freebsd", "x86_64") => "bcmr-x86_64-freebsd.tar.gz",
        _ => bail!("no pre-built binary for {} {}", os, arch),
    };
    Ok(name.to_string())
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_uname_linux_x86() {
        let (os, arch) = parse_uname("Linux x86_64").unwrap();
        assert_eq!(os, "linux");
        assert_eq!(arch, "x86_64");
    }

    #[test]
    fn test_parse_uname_darwin_arm() {
        let (os, arch) = parse_uname("Darwin arm64").unwrap();
        assert_eq!(os, "macos");
        assert_eq!(arch, "aarch64");
    }

    #[test]
    fn test_parse_uname_linux_aarch64() {
        let (os, arch) = parse_uname("Linux aarch64").unwrap();
        assert_eq!(os, "linux");
        assert_eq!(arch, "aarch64");
    }

    #[test]
    fn test_parse_uname_freebsd() {
        let (os, arch) = parse_uname("FreeBSD amd64").unwrap();
        assert_eq!(os, "freebsd");
        assert_eq!(arch, "x86_64");
    }

    #[test]
    fn test_parse_uname_unsupported_os() {
        assert!(parse_uname("SunOS sparc64").is_err());
    }

    #[test]
    fn test_parse_uname_empty() {
        assert!(parse_uname("").is_err());
    }

    #[test]
    fn test_release_asset_linux_x86() {
        let name = release_asset_name("linux", "x86_64").unwrap();
        assert_eq!(name, "bcmr-x86_64-linux.tar.gz");
    }

    #[test]
    fn test_release_asset_macos_arm() {
        let name = release_asset_name("macos", "aarch64").unwrap();
        assert_eq!(name, "bcmr-aarch64-macos.tar.gz");
    }

    #[test]
    fn test_release_asset_unsupported() {
        assert!(release_asset_name("windows", "x86_64").is_err());
    }

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape("/usr/bin/bcmr"), "'/usr/bin/bcmr'");
    }

    #[test]
    fn test_shell_escape_with_single_quote() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }
}
