//! Identify a Homebrew-managed executable without invoking Homebrew or trusting PATH.
use std::path::{Path, PathBuf};

pub(crate) struct HomebrewInstallation {
    pub stable_executable: Option<PathBuf>,
    pub upgrade_command: String,
}

pub(crate) fn homebrew_from_executable(executable: &Path) -> Option<HomebrewInstallation> {
    let executable = std::fs::canonicalize(executable).ok()?;
    if executable.file_name()? != "bcmr" {
        return None;
    }
    let bin = executable.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let keg = bin.parent()?;
    let rack = keg.parent()?;
    let cellar = rack.parent()?;
    if rack.file_name()? != "bcmr" || cellar.file_name()? != "Cellar" {
        return None;
    }
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(keg.join("INSTALL_RECEIPT.json")).ok()?).ok()?;
    let source = receipt.get("source")?;
    let tap = source.get("tap")?.as_str()?;
    let components: Vec<_> = tap.split('/').collect();
    if components.len() != 2
        || components.iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-' | b'.'))
        })
        || Path::new(source.get("path")?.as_str()?).file_name()? != "bcmr.rb"
    {
        return None;
    }
    let opt_binary = cellar.parent()?.join("opt/bcmr/bin/bcmr");
    // A stale or unrelated opt link must never redirect a command elsewhere.
    let stable_executable = std::fs::canonicalize(&opt_binary)
        .ok()
        .filter(|target| target == &executable)
        .map(|_| opt_binary);
    Some(HomebrewInstallation {
        stable_executable,
        upgrade_command: format!("brew upgrade {tap}/bcmr"),
    })
}

pub(crate) fn current_homebrew() -> Option<HomebrewInstallation> {
    homebrew_from_executable(&std::env::current_exe().ok()?)
}

pub(crate) fn update_command() -> String {
    current_homebrew()
        .map(|installation| installation.upgrade_command)
        .unwrap_or_else(|| "bcmr update".to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn receipt_and_matching_opt_link_identify_managed_executable() {
        let root = tempfile::tempdir().unwrap();
        let keg = root.path().join("Cellar/bcmr/0.7.0");
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        let binary = keg.join("bin/bcmr");
        std::fs::write(&binary, b"fixture").unwrap();
        let receipt = keg.join("INSTALL_RECEIPT.json");
        let valid = br#"{"source":{"tap":"bengerthelorf/tap","path":"/tap/Formula/bcmr.rb"}}"#;
        assert!(homebrew_from_executable(&binary).is_none());
        std::fs::write(&receipt, valid).unwrap();
        assert!(homebrew_from_executable(&binary)
            .unwrap()
            .stable_executable
            .is_none());
        let opt = root.path().join("opt");
        std::fs::create_dir(&opt).unwrap();
        symlink(&keg, opt.join("bcmr")).unwrap();
        let installation = homebrew_from_executable(&binary).unwrap();
        assert_eq!(
            installation.stable_executable,
            Some(opt.join("bcmr/bin/bcmr"))
        );
        assert_eq!(
            installation.upgrade_command,
            "brew upgrade bengerthelorf/tap/bcmr"
        );

        // Invalid JSON, another formula, and unsafe tap text are not receipts.
        for invalid in [
            "not json",
            r#"{"source":{"tap":"bengerthelorf/tap","path":"/tap/other.rb"}}"#,
            r#"{"source":{"tap":"owner/tap;echo bad","path":"/tap/bcmr.rb"}}"#,
        ] {
            std::fs::write(&receipt, invalid).unwrap();
            assert!(homebrew_from_executable(&binary).is_none());
        }
        std::fs::write(receipt, valid).unwrap();
        std::fs::remove_file(opt.join("bcmr")).unwrap();
        let other_keg = root.path().join("Cellar/bcmr/0.7.1");
        std::fs::create_dir_all(other_keg.join("bin")).unwrap();
        std::fs::write(other_keg.join("bin/bcmr"), b"other").unwrap();
        symlink(other_keg, opt.join("bcmr")).unwrap();
        assert!(homebrew_from_executable(&binary)
            .unwrap()
            .stable_executable
            .is_none());
    }
}
