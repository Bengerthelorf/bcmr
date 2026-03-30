use anyhow::{bail, Result};
use tokio::process::Command;

/// Deploy bcmr binary to a remote host.
///
/// 1. Detect remote OS + architecture
/// 2. If same as local → scp the running binary
/// 3. If different → download matching binary from GitHub Releases
/// 4. Verify installation
pub async fn run(target: &str, remote_path: &str) -> Result<()> {
    // Expand ~ in remote path for display
    let display_path = remote_path.replace("~", "$HOME");
    eprintln!("Deploying bcmr to {}:{}", target, display_path);

    // Step 1: Check if bcmr is already installed
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

    // Step 2: Detect remote platform
    let remote_info = ssh(target, "uname -sm").await?;
    let remote_info = remote_info.trim();
    let (remote_os, remote_arch) = parse_uname(remote_info)?;
    eprintln!("  Remote: {} {}", remote_os, remote_arch);

    let local_os = std::env::consts::OS;
    let local_arch = std::env::consts::ARCH;
    eprintln!("  Local:  {} {}", local_os, local_arch);

    // Step 3: Ensure remote directory exists
    let dir = if remote_path.contains('/') {
        remote_path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".")
    } else {
        "."
    };
    ssh(target, &format!("mkdir -p {}", shell_escape(dir))).await?;

    // Step 4: Get binary and transfer
    if remote_os == local_os && remote_arch == local_arch {
        // Same platform — transfer local binary directly
        eprintln!("  Same platform. Transferring local binary...");
        let local_bin = std::env::current_exe()?;
        scp_to(target, &local_bin, remote_path).await?;
    } else {
        // Cross-platform — download from GitHub Releases
        eprintln!("  Cross-platform. Downloading from GitHub Releases...");
        let asset_name = release_asset_name(remote_os, remote_arch)?;
        let url = format!(
            "https://github.com/Bengerthelorf/bcmr/releases/latest/download/{}",
            asset_name
        );
        // Download on remote directly (avoids transferring through local)
        let download_cmd = format!(
            "curl -fsSL '{}' -o {} && chmod +x {}",
            url,
            shell_escape(remote_path),
            shell_escape(remote_path)
        );
        let output = ssh(target, &download_cmd).await;
        if output.is_err() {
            // Fallback: download locally, then scp
            eprintln!("  Direct download failed. Downloading locally...");
            let tmp = std::env::temp_dir().join("bcmr-deploy-tmp");
            let status = Command::new("curl")
                .args(["-fsSL", &url, "-o"])
                .arg(&tmp)
                .status()
                .await?;
            if !status.success() {
                bail!(
                    "Failed to download {} binary from GitHub Releases.\n\
                     You can install manually: cargo install bcmr",
                    asset_name
                );
            }
            scp_to(target, &tmp, remote_path).await?;
            let _ = tokio::fs::remove_file(&tmp).await;
        }
    }

    // Step 5: Make executable and verify
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
    let dest = format!("{}:{}", target, remote_path);
    let status = Command::new("scp")
        .args(["-o", "BatchMode=yes"])
        .arg(local)
        .arg(&dest)
        .status()
        .await?;
    if !status.success() {
        bail!("scp failed: {} -> {}", local.display(), dest);
    }
    Ok(())
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
        ("linux", "x86_64") => "bcmr-x86_64-unknown-linux-gnu.tar.gz",
        ("linux", "aarch64") => "bcmr-aarch64-unknown-linux-gnu.tar.gz",
        ("macos", "x86_64") => "bcmr-x86_64-apple-darwin.tar.gz",
        ("macos", "aarch64") => "bcmr-aarch64-apple-darwin.tar.gz",
        ("freebsd", "x86_64") => "bcmr-x86_64-unknown-freebsd.tar.gz",
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
        assert!(name.contains("x86_64-unknown-linux"));
    }

    #[test]
    fn test_release_asset_macos_arm() {
        let name = release_asset_name("macos", "aarch64").unwrap();
        assert!(name.contains("aarch64-apple-darwin"));
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
