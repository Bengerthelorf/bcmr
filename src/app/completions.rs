use crate::cli;
use crate::core::error::BcmrError;
use anyhow::Result;

pub(crate) fn validate_mode(mode: &str, name: &str) -> Result<()> {
    match mode.to_lowercase().as_str() {
        "force" | "disable" | "never" | "auto" => Ok(()),
        other => Err(BcmrError::InvalidInput(format!(
            "Invalid {} mode '{}'. Supported modes: force, disable, never, auto.",
            name, other
        ))
        .into()),
    }
}

pub(crate) const POWERSHELL_REMOTE_INJECT: &str = r#"    $tokens = $commandAst.ToString() -split '\s+'
    if ($wordToComplete -match '.+:.+' -and $tokens.Count -ge 2 -and ($tokens[1] -eq 'copy' -or $tokens[1] -eq 'move')) {
        $results = bcmr __complete-remote $wordToComplete 2>$null
        if ($results) {
            $results | ForEach-Object {
                [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
            }
            return
        }
    }"#;

pub(crate) fn build_completion_command() -> clap::Command {
    let full = <cli::Cli as clap::CommandFactory>::command();
    let visible: Vec<clap::Command> = full
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .cloned()
        .collect();
    let mut cmd = clap::Command::new("bcmr");
    for sub in visible {
        cmd = cmd.subcommand(sub);
    }
    cmd
}

pub(crate) fn remote_completion_script(shell: &clap_complete::Shell) -> &'static str {
    use clap_complete::Shell;
    match shell {
        Shell::Zsh => {
            r#"

_bcmr_with_remote() {
    local cur="${words[CURRENT]}"
    if [[ "$cur" == *:* ]] && [[ "${words[2]}" == "copy" || "${words[2]}" == "move" ]]; then
        local -a results
        results=("${(@f)$(bcmr __complete-remote "$cur" 2>/dev/null)}")
        if [[ ${#results[@]} -gt 0 && -n "${results[1]}" ]]; then
            compadd -U -S '' -- "${results[@]}"
            return
        fi
    fi
    _bcmr "$@"
}
compdef _bcmr_with_remote bcmr
"#
        }
        Shell::Bash => {
            r#"

_bcmr_with_remote() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    local cmd="${COMP_WORDS[1]}"
    if [[ "$cur" == *:* ]] && [[ "$cmd" == "copy" || "$cmd" == "move" ]]; then
        local IFS=$'\n'
        COMPREPLY=($(bcmr __complete-remote "$cur" 2>/dev/null))
        if [[ ${#COMPREPLY[@]} -gt 0 ]]; then
            compopt -o nospace
            return
        fi
    fi
    _bcmr "$@"
}
complete -F _bcmr_with_remote bcmr
"#
        }
        Shell::Fish => {
            r#"

complete -c bcmr -n '__fish_seen_subcommand_from copy move; and string match -q "*:*" -- (commandline -ct)' -f -a '(bcmr __complete-remote (commandline -ct) 2>/dev/null)'
"#
        }
        Shell::PowerShell => "",
        _ => "",
    }
}
