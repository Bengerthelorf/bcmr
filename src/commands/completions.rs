use anyhow::{anyhow, bail, Result};
use std::io::Write;
use std::path::PathBuf;

pub fn install_path(shell: clap_complete::Shell) -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .ok_or_else(|| anyhow!("could not resolve $HOME"))?;
    let data = xdg_dir("XDG_DATA_HOME", &home, &[".local", "share"]);
    let config = xdg_dir("XDG_CONFIG_HOME", &home, &[".config"]);
    let path = match shell {
        clap_complete::Shell::Bash => data
            .join("bash-completion")
            .join("completions")
            .join("bcmr"),
        clap_complete::Shell::Zsh => data.join("zsh").join("site-functions").join("_bcmr"),
        clap_complete::Shell::Fish => config.join("fish").join("completions").join("bcmr.fish"),
        clap_complete::Shell::PowerShell => bail!(
            "PowerShell completions are not auto-installable; \
             append the output of 'bcmr completions powershell' to your $PROFILE"
        ),
        clap_complete::Shell::Elvish => config.join("elvish").join("lib").join("bcmr.elv"),
        other => bail!("unsupported shell for --install: {other:?}"),
    };
    Ok(path)
}

fn xdg_dir(var: &str, home: &std::path::Path, fallback_segments: &[&str]) -> PathBuf {
    if let Some(v) = std::env::var_os(var) {
        let p = PathBuf::from(v);
        if p.is_absolute() {
            return p;
        }
    }
    let mut p = home.to_path_buf();
    for seg in fallback_segments {
        p.push(seg);
    }
    p
}

pub fn run_install(
    shell: clap_complete::Shell,
    script: &str,
    print: bool,
    uninstall: bool,
) -> Result<()> {
    let path = install_path(shell)?;

    if print {
        println!("{}", path.display());
        return Ok(());
    }

    if uninstall {
        return remove_install(shell, &path);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("cannot create {}: {}", parent.display(), e))?;
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("install path has no parent: {}", path.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| anyhow!("cannot stage temp file in {}: {}", parent.display(), e))?;
    tmp.write_all(script.as_bytes())
        .map_err(|e| anyhow!("cannot write staged completion script: {}", e))?;
    tmp.persist(&path)
        .map_err(|e| anyhow!("cannot install {}: {}", path.display(), e))?;
    eprintln!("Installed {} ({})", path.display(), shell);
    shell_reload_hint(shell);
    Ok(())
}

fn remove_install(shell: clap_complete::Shell, path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            eprintln!("Removed {}", path.display());
            shell_reload_hint(shell);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Nothing to remove at {}", path.display());
            Ok(())
        }
        Err(e) => Err(anyhow!("cannot remove {}: {}", path.display(), e)),
    }
}

fn shell_reload_hint(shell: clap_complete::Shell) {
    let hint = match shell {
        clap_complete::Shell::Bash => {
            "Reload: open a new shell. \
             Requires the bash-completion package to be installed and sourced from your rc file."
        }
        clap_complete::Shell::Zsh => {
            "Reload: ensure $XDG_DATA_HOME/zsh/site-functions is on $fpath, then run 'compinit'"
        }
        clap_complete::Shell::Fish => "Fish loads it on next shell start.",
        clap_complete::Shell::Elvish => "Elvish loads it on next shell start.",
        _ => return,
    };
    eprintln!("{}", hint);
}
