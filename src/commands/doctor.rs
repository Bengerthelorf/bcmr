use anyhow::Result;
use serde::Serialize;
use std::io::IsTerminal;
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    fn glyph(self, ascii: bool) -> &'static str {
        match (self, ascii) {
            (CheckStatus::Ok, false) => "✓",
            (CheckStatus::Warn, false) => "⚠",
            (CheckStatus::Fail, false) => "✗",
            (CheckStatus::Ok, true) => "OK",
            (CheckStatus::Warn, true) => "WARN",
            (CheckStatus::Fail, true) => "FAIL",
        }
    }
}

#[derive(Serialize)]
pub struct Check {
    pub status: CheckStatus,
    pub label: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommend: Option<String>,
}

impl Check {
    fn ok(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Ok,
            label: label.into(),
            detail: detail.into(),
            recommend: None,
        }
    }
    fn warn(label: impl Into<String>, detail: impl Into<String>, recommend: &str) -> Self {
        Self {
            status: CheckStatus::Warn,
            label: label.into(),
            detail: detail.into(),
            recommend: Some(recommend.to_string()),
        }
    }
    fn fail(label: impl Into<String>, detail: impl Into<String>, recommend: &str) -> Self {
        Self {
            status: CheckStatus::Fail,
            label: label.into(),
            detail: detail.into(),
            recommend: Some(recommend.to_string()),
        }
    }
}

#[derive(Serialize)]
struct HostReport {
    host: String,
    checks: Vec<Check>,
}

pub async fn run(hosts: &[String], json: bool) -> Result<()> {
    let local = run_local_checks();
    let host_reports = run_all_host_checks(hosts).await;

    let ok = report_is_ok(&local, &host_reports);

    if json {
        let envelope = serde_json::json!({
            "bcmr_version": env!("CARGO_PKG_VERSION"),
            "local": local,
            "hosts": host_reports,
            "ok": ok,
        });
        println!("{}", envelope);
    } else {
        print_human(env!("CARGO_PKG_VERSION"), &local, &host_reports);
    }

    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

fn report_is_ok(local: &[Check], hosts: &[HostReport]) -> bool {
    !local.iter().any(|c| c.status == CheckStatus::Fail)
        && !hosts
            .iter()
            .any(|h| h.checks.iter().any(|c| c.status == CheckStatus::Fail))
}

async fn run_all_host_checks(hosts: &[String]) -> Vec<HostReport> {
    use tokio::task::JoinSet;

    if hosts.is_empty() {
        return Vec::new();
    }
    let mut set: JoinSet<(usize, HostReport)> = JoinSet::new();
    for (i, host) in hosts.iter().enumerate() {
        let host = host.clone();
        set.spawn(async move {
            let checks = run_host_checks(&host).await;
            (i, HostReport { host, checks })
        });
    }
    let mut slots: Vec<Option<HostReport>> = (0..hosts.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        if let Ok((i, rep)) = joined {
            slots[i] = Some(rep);
        }
    }
    slots.into_iter().flatten().collect()
}

fn print_human(version: &str, local: &[Check], hosts: &[HostReport]) {
    let ascii = use_ascii_glyphs();
    println!("bcmr {} — diagnostic report", version);
    println!();
    println!("Local:");
    for c in local {
        print_check(c, "  ", ascii);
    }
    if hosts.is_empty() {
        println!();
        println!("(Pass host arguments to probe remotes: bcmr doctor user@host ...)");
    } else {
        for h in hosts {
            println!();
            println!("{}:", h.host);
            for c in &h.checks {
                print_check(c, "  ", ascii);
            }
        }
    }
}

fn use_ascii_glyphs() -> bool {
    crate::ui::progress::ansi_disabled_by_env() || !std::io::stdout().is_terminal()
}

fn print_check(c: &Check, indent: &str, ascii: bool) {
    println!(
        "{}{} {}: {}",
        indent,
        c.status.glyph(ascii),
        c.label,
        c.detail
    );
    if let Some(rec) = &c.recommend {
        let arrow = if ascii { "->" } else { "→" };
        println!("{}  {} {}", indent, arrow, rec);
    }
}

fn run_local_checks() -> Vec<Check> {
    vec![check_config_file(), check_jobs_dir(), check_color_env()]
}

fn check_config_file() -> Check {
    let path = std::env::var_os("BCMR_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            directories::UserDirs::new().map(|u| {
                u.home_dir()
                    .join(".config")
                    .join("bcmr")
                    .join("config.toml")
            })
        });
    let Some(path) = path else {
        return Check::warn(
            "config file",
            "could not resolve $HOME and BCMR_CONFIG is unset",
            "set $HOME to a real directory or pass --config <PATH>",
        );
    };
    if !path.exists() {
        return Check::ok(
            "config file",
            format!("{} (none — using built-in defaults)", path.display()),
        );
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => match toml::from_str::<toml::Value>(&s) {
            Ok(_) => Check::ok("config file", format!("{} (valid TOML)", path.display())),
            Err(e) => Check::fail(
                "config file",
                format!("{} (parse error)", path.display()),
                &format!("fix or remove the file; error: {}", e),
            ),
        },
        Err(e) => Check::fail(
            "config file",
            format!("{} (unreadable)", path.display()),
            &format!("fix permissions; error: {}", e),
        ),
    }
}

fn check_jobs_dir() -> Check {
    let dir: PathBuf = crate::commands::jobs::jobs_dir();
    if !dir.exists() {
        return Check::ok(
            "jobs dir",
            format!("{} (empty — created on first --json run)", dir.display()),
        );
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            return Check::fail(
                "jobs dir",
                format!("{} (unreadable)", dir.display()),
                &format!("fix permissions; error: {}", e),
            );
        }
    };
    let (mut count, mut bytes) = (0u64, 0u64);
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl") {
            count += 1;
            if let Ok(meta) = entry.metadata() {
                bytes += meta.len();
            }
        }
    }
    let detail = format!(
        "{} ({} job{}, {})",
        dir.display(),
        count,
        if count == 1 { "" } else { "s" },
        crate::ui::utils::format_bytes(bytes as f64)
    );
    if count > 50 {
        Check::warn(
            "jobs dir",
            detail,
            "consider running 'bcmr status --gc' to drop logs older than 7 days",
        )
    } else {
        Check::ok("jobs dir", detail)
    }
}

fn check_color_env() -> Check {
    if crate::ui::progress::ansi_disabled_by_env() {
        let no_color = std::env::var_os("NO_COLOR")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        let term = std::env::var("TERM").unwrap_or_default();
        Check::ok(
            "color env",
            format!("ANSI suppressed (NO_COLOR={}, TERM={:?})", no_color, term),
        )
    } else {
        Check::ok(
            "color env",
            "ANSI enabled (NO_COLOR unset, TERM not 'dumb')",
        )
    }
}

async fn run_host_checks(host: &str) -> Vec<Check> {
    let mut checks = Vec::new();

    match ssh_probe(host).await {
        Ok(probe) => {
            checks.push(Check::ok("ssh", format!("reachable as {}", host)));
            checks.push(classify_remote_bcmr(&probe));
        }
        Err(stderr) => {
            checks.push(Check::fail(
                "ssh",
                crate::core::remote::ssh_error_message(&stderr, host),
                "verify ~/.ssh/config and key auth (BatchMode=yes is used)",
            ));
        }
    }

    checks
}

struct RemoteProbe {
    bcmr_path: Option<String>,
    bcmr_version: Option<String>,
}

async fn ssh_probe(host: &str) -> std::result::Result<RemoteProbe, String> {
    let cmd = "command -v bcmr 2>/dev/null && bcmr --version 2>/dev/null || true";
    let output = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host, cmd])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut path = None;
    let mut version = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('/') {
            path = Some(line.to_string());
        } else if line.starts_with("bcmr ") {
            version = Some(line.trim_start_matches("bcmr ").to_string());
        }
    }
    Ok(RemoteProbe {
        bcmr_path: path,
        bcmr_version: version,
    })
}

fn classify_remote_bcmr(probe: &RemoteProbe) -> Check {
    let local = env!("CARGO_PKG_VERSION");
    match (&probe.bcmr_path, &probe.bcmr_version) {
        (None, _) => Check::fail(
            "remote bcmr",
            "not on PATH",
            "run 'bcmr deploy <host>' (or '--path /usr/local/bin/bcmr <host>' if ~/.local/bin is not in non-interactive PATH)",
        ),
        (Some(path), None) => Check::warn(
            "remote bcmr",
            format!("{} (version unknown)", path),
            "the binary did not respond to --version; try 'ssh <host> bcmr --version' to debug",
        ),
        (Some(path), Some(v)) if v == local => {
            Check::ok("remote bcmr", format!("{} v{} (matches local)", path, v))
        }
        (Some(path), Some(v)) => Check::warn(
            "remote bcmr",
            format!("{} v{} (local v{})", path, v, local),
            "consider 'bcmr deploy <host>' to upgrade — protocol may auto-fall-back on mismatch",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_ok_is_true_when_no_fails() {
        let local = vec![Check::ok("a", "x"), Check::warn("b", "y", "z")];
        let hosts = vec![HostReport {
            host: "h".into(),
            checks: vec![Check::ok("ssh", "ok")],
        }];
        assert!(report_is_ok(&local, &hosts));
    }

    #[test]
    fn report_ok_is_false_on_local_fail() {
        let local = vec![Check::fail("a", "x", "fix")];
        assert!(!report_is_ok(&local, &[]));
    }

    #[test]
    fn report_ok_is_false_on_host_fail() {
        let hosts = vec![HostReport {
            host: "h".into(),
            checks: vec![Check::fail("ssh", "x", "fix")],
        }];
        assert!(!report_is_ok(&[], &hosts));
    }

    #[test]
    fn glyph_ascii_fallback() {
        assert_eq!(CheckStatus::Ok.glyph(true), "OK");
        assert_eq!(CheckStatus::Warn.glyph(true), "WARN");
        assert_eq!(CheckStatus::Fail.glyph(true), "FAIL");
        assert_eq!(CheckStatus::Ok.glyph(false), "✓");
    }
}
