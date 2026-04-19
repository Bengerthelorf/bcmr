use anyhow::{bail, Result};
use tokio::process::Command;

pub async fn run(target: &str, remote_path: &str) -> Result<()> {
    let display_path = remote_path.replace("~", "$HOME");
    eprintln!("Deploying bcmr to {}:{}", target, display_path);

    let check = ssh(target, "bcmr --version 2>/dev/null || echo NOTFOUND").await?;
    if !check.contains("NOTFOUND") {
        eprintln!("  Remote already has: {}", check.trim());
        let local_version = env!("CARGO_PKG_VERSION");
        if check.contains(local_version) {
            eprintln!("  Same version as local. Nothing to do.");
            return Ok(());
        }
        eprintln!("  Upgrading to v{}...", local_version);
    }

    let remote_info = ssh(target, "uname -sm").await?;
    let remote_info = remote_info.trim();
    let (remote_os, remote_arch) = parse_uname(remote_info)?;
    eprintln!("  Remote: {} {}", remote_os, remote_arch);

    let local_os = std::env::consts::OS;
    let local_arch = std::env::consts::ARCH;
    eprintln!("  Local:  {} {}", local_os, local_arch);

    let dir = if remote_path.contains('/') {
        remote_path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".")
    } else {
        "."
    };
    ssh(target, &format!("mkdir -p {}", shell_escape(dir))).await?;

    if remote_os == local_os && remote_arch == local_arch {
        eprintln!("  Same platform. Transferring local binary...");
        let local_bin = std::env::current_exe()?;
        scp_to(target, &local_bin, remote_path).await?;
    } else {
        eprintln!("  Cross-platform. Downloading from GitHub Releases...");
        let asset_name = release_asset_name(remote_os, remote_arch)?;
        let url = format!(
            "https://github.com/Bengerthelorf/bcmr/releases/latest/download/{}",
            asset_name
        );
        let download_cmd = format!(
            "curl -fsSL '{}' | tar xz -C {} bcmr && mv {}/bcmr {}",
            url,
            shell_escape(dir),
            shell_escape(dir),
            shell_escape(remote_path)
        );
        let output = ssh(target, &download_cmd).await;
        if output.is_err() {
            eprintln!("  Direct download failed. Downloading locally...");
            let tmp_dir = std::env::temp_dir().join("bcmr-deploy-tmp");
            let _ = tokio::fs::create_dir_all(&tmp_dir).await;
            let tmp_archive = tmp_dir.join(&asset_name);
            let status = Command::new("curl")
                .args(["-fsSL", &url, "-o"])
                .arg(&tmp_archive)
                .status()
                .await?;
            if !status.success() {
                bail!(
                    "Failed to download {} from GitHub Releases.\n\
                     Install manually: cargo install bcmr",
                    asset_name
                );
            }
            let status = Command::new("tar")
                .args(["xzf"])
                .arg(&tmp_archive)
                .args(["-C"])
                .arg(&tmp_dir)
                .status()
                .await?;
            if !status.success() {
                bail!("Failed to extract {}", asset_name);
            }
            let extracted = tmp_dir.join("bcmr");
            scp_to(target, &extracted, remote_path).await?;
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        }
    }

    ssh(target, &format!("chmod +x {}", shell_escape(remote_path))).await?;
    let verify = ssh(target, &format!("{} --version", shell_escape(remote_path))).await?;
    eprintln!("  Installed: {}", verify.trim());
    eprintln!("  Done.");

    Ok(())
}

async fn ssh(target: &str, cmd: &str) -> Result<String> {
    let output = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg(target)
        .arg(cmd)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ssh command failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn scp_to(target: &str, local: &std::path::Path, remote_path: &str) -> Result<()> {
    // sftp instead of scp: pre-OpenSSH-9 scp is CVE-2019-6111 territory.
    use tokio::io::AsyncWriteExt;
    let mut child = Command::new("sftp")
        .args(["-o", "BatchMode=yes", "-b", "-", target])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("sftp stdin unavailable"))?;
    let cmd = format!(
        "put {} {}\n",
        sftp_quote(&local.to_string_lossy())?,
        sftp_quote(remote_path)?
    );
    stdin.write_all(cmd.as_bytes()).await?;
    drop(stdin);
    let status = child.wait().await?;
    if !status.success() {
        bail!(
            "sftp put failed: {} -> {}:{}",
            local.display(),
            target,
            remote_path
        );
    }
    Ok(())
}

fn sftp_quote(s: &str) -> Result<String> {
    if s.chars().any(|c| c == '\n' || c == '"') {
        bail!(
            "sftp path contains forbidden character (newline or quote): {:?}",
            s
        );
    }
    if s.chars().any(char::is_whitespace) {
        Ok(format!("\"{}\"", s))
    } else {
        Ok(s.to_string())
    }
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
    if s.contains('\'') {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        format!("'{}'", s)
    }
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
        assert_eq!(shell_escape("it's"), "\"it's\"");
    }
}
