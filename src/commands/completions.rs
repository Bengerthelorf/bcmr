use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;

pub fn install_path(shell: clap_complete::Shell) -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .ok_or_else(|| anyhow!("could not resolve $HOME"))?;
    let path = match shell {
        clap_complete::Shell::Bash => home
            .join(".local")
            .join("share")
            .join("bash-completion")
            .join("completions")
            .join("bcmr"),
        clap_complete::Shell::Zsh => home
            .join(".local")
            .join("share")
            .join("zsh")
            .join("site-functions")
            .join("_bcmr"),
        clap_complete::Shell::Fish => home
            .join(".config")
            .join("fish")
            .join("completions")
            .join("bcmr.fish"),
        clap_complete::Shell::PowerShell => bail!(
            "PowerShell completions are not auto-installable; \
             append the output of 'bcmr completions powershell' to your $PROFILE"
        ),
        clap_complete::Shell::Elvish => home
            .join(".config")
            .join("elvish")
            .join("lib")
            .join("bcmr.elv"),
        other => bail!("unsupported shell for --install: {other:?}"),
    };
    Ok(path)
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
        match std::fs::remove_file(&path) {
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
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("cannot create {}: {}", parent.display(), e))?;
        }
        std::fs::write(&path, script)
            .map_err(|e| anyhow!("cannot write {}: {}", path.display(), e))?;
        eprintln!("Installed {} ({})", path.display(), shell);
        shell_reload_hint(shell);
        Ok(())
    }
}

fn shell_reload_hint(shell: clap_complete::Shell) {
    let hint = match shell {
        clap_complete::Shell::Bash => "Reload: exec bash  (or open a new shell)",
        clap_complete::Shell::Zsh => {
            "Reload: ensure ~/.local/share/zsh/site-functions is on $fpath, then run 'compinit'"
        }
        clap_complete::Shell::Fish => "Fish loads it on next shell start.",
        clap_complete::Shell::Elvish => "Elvish loads it on next shell start.",
        _ => return,
    };
    eprintln!("{}", hint);
}
