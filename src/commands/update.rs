use anyhow::Result;
use self_update::cargo_crate_version;

fn platform_target() -> Result<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => Ok("x86_64-linux"),
        ("aarch64", "linux") => Ok("aarch64-linux"),
        ("x86_64", "macos") => Ok("x86_64-macos"),
        ("aarch64", "macos") => Ok("aarch64-macos"),
        ("x86_64", "windows") => Ok("x86_64-windows"),
        ("aarch64", "windows") => Ok("aarch64-windows"),
        ("x86_64", "freebsd") => Ok("x86_64-freebsd"),
        (arch, os) => Err(anyhow::anyhow!("Unsupported platform: {}-{}", arch, os)),
    }
}

fn version_newer(latest: &str, current: &str) -> bool {
    let parse =
        |version: &str| semver::Version::parse(version.strip_prefix('v').unwrap_or(version)).ok();
    match (parse(latest), parse(current)) {
        (Some(latest), Some(current)) => latest.cmp_precedence(&current).is_gt(),
        _ => false,
    }
}

fn fetch_latest_version() -> Result<String> {
    let releases = self_update::backends::github::ReleaseList::configure()
        .repo_owner("Bengerthelorf")
        .repo_name("bcmr")
        .build()?
        .fetch()?;
    releases
        .latest()
        .map(|release| release.version().to_owned())
        .ok_or_else(|| anyhow::anyhow!("no releases found"))
}

pub fn check_for_update() -> Option<String> {
    let latest = fetch_latest_version().ok()?;
    let current = cargo_crate_version!();
    if version_newer(&latest, current) {
        Some(latest)
    } else {
        None
    }
}

pub fn run(check_only: bool) -> Result<()> {
    let current = cargo_crate_version!();
    let json = crate::config::is_json_mode();

    if check_only {
        let latest = fetch_latest_version()?;
        let available = version_newer(&latest, current);
        if json {
            let result = serde_json::json!({
                "type": "result",
                "status": "success",
                "operation": "update_check",
                "current_version": current,
                "latest_version": latest,
                "update_available": available,
            });
            println!("{result}");
            crate::config::mark_json_terminal_emitted();
        } else {
            println!("Current version: {}", current);
            if available {
                println!("Latest version:  {} (update available)", latest);
            } else {
                println!("Latest version:  {} (up to date)", latest);
            }
        }
        return Ok(());
    }

    if !json {
        println!("Current version: {}", current);
        println!("Checking for updates...");
    }

    let status = self_update::backends::github::Update::configure()
        .repo_owner("Bengerthelorf")
        .repo_name("bcmr")
        .bin_name("bcmr")
        .target(platform_target()?)
        .show_download_progress(!json)
        .current_version(current)
        .build()?
        .update()?;

    if json {
        let result = serde_json::json!({
            "type": "result",
            "status": "success",
            "operation": "update",
            "previous_version": current,
            "current_version": status.version(),
            "updated": status.is_updated(),
        });
        println!("{result}");
        crate::config::mark_json_terminal_emitted();
    } else if status.is_updated() {
        println!("Updated to version {}!", status.version());
    } else {
        println!("Already up to date.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::version_newer;

    #[test]
    fn version_newer_follows_semver_prerelease_ordering() {
        assert!(version_newer("v0.7.0", "0.7.0-rc.1"));
        assert!(version_newer("0.7.0-rc.2", "0.7.0-rc.1"));
        assert!(version_newer("0.7.0-alpha.10", "v0.7.0-alpha.2"));
        assert!(!version_newer("0.7.0-rc.1", "0.7.0"));
    }

    #[test]
    fn version_newer_ignores_build_metadata_and_rejects_invalid_versions() {
        assert!(!version_newer("1.2.3+build.2", "1.2.3+build.1"));
        assert!(!version_newer(
            "1.2.3-alpha.1+build.2",
            "1.2.3-alpha.1+build.1"
        ));
        assert!(!version_newer("not-a-version", "1.2.3"));
        assert!(!version_newer("1.2.3", "not-a-version"));
    }

    #[test]
    fn version_newer_handles_equal_and_plain_patch_versions() {
        assert!(version_newer("1.2.4", "v1.2.3"));
        assert!(!version_newer("1.2.3", "1.2.3"));
        assert!(!version_newer("1.2.2", "1.2.3"));
    }
}
