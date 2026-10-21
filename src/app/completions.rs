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

pub(crate) const POWERSHELL_REMOTE_INJECT: &str = r#"    $tokens = @($commandAst.CommandElements | ForEach-Object {
        if ($_ -is [System.Management.Automation.Language.StringConstantExpressionAst]) { $_.Value } else { $_.Extent.Text }
    })
    $connectionOptions = @()
    $operation = ''
    for ($i = 1; $i -lt $tokens.Count - 1; $i++) {
        $token = $tokens[$i]
        if ($token -eq '--') { break }
        if ($token -in @('--ssh-port', '--ssh-identity', '--ssh-config', '--ssh-jump', '--remote-backend', '--config', '--profile')) {
            if ($i + 1 -lt $tokens.Count - 1) { $connectionOptions += $token; $i++; $connectionOptions += $tokens[$i] }
        } elseif ($token -match '^--(ssh-port|ssh-identity|ssh-config|ssh-jump|remote-backend|config|profile)=') {
            $connectionOptions += $token
        } elseif ($token -in @('copy', 'move')) { $operation = $token }
    }
    if ($wordToComplete -match '.+:.+' -and $operation -in @('copy', 'move')) {
        if ($env:BCMR_COMPLETION_DEBUG -eq '1') {
            $results = bcmr @connectionOptions __complete-remote $wordToComplete
        } else {
            $results = bcmr @connectionOptions __complete-remote $wordToComplete 2>$null
        }
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
    for arg in full.get_arguments().filter(|arg| arg.is_global_set()) {
        cmd = cmd.arg(arg.clone());
    }
    for sub in visible {
        cmd = cmd.subcommand(sub);
    }
    cmd
}

pub(crate) fn remote_completion_script(shell: &clap_complete::Shell) -> &'static str {
    use clap_complete::Shell;
    match shell {
        Shell::Zsh => {
            concat!(
                r#"

_bcmr_with_remote() {
    local cur="${words[CURRENT]}"
    local operation='' token
    local -a connection_options
    local i=2
    while (( i < CURRENT )); do
        token="${words[i]}"
        case "$token" in
            --) break ;;
            --ssh-port|--ssh-identity|--ssh-config|--ssh-jump|--remote-backend|--config|--profile)
                if (( i + 1 < CURRENT )); then
                    connection_options+=("$token" "${words[i+1]}")
                fi
                (( i += 2 )) ;;
            --ssh-port=*|--ssh-identity=*|--ssh-config=*|--ssh-jump=*|--remote-backend=*|--config=*|--profile=*)
                connection_options+=("$token")
                (( i += 1 )) ;;
            copy|move) operation="$token"; (( i += 1 )) ;;
            *) (( i += 1 )) ;;
        esac
    done
    if [[ "$cur" == *:* ]] && [[ "$operation" == "copy" || "$operation" == "move" ]]; then
        local -a results
        if [[ "${BCMR_COMPLETION_DEBUG:-}" == 1 ]]; then
            results=("${(@f)$(bcmr "${connection_options[@]}" __complete-remote "$cur")}")
        else
            results=("${(@f)$(bcmr "${connection_options[@]}" __complete-remote "$cur" 2>/dev/null)}")
        fi
        if [[ ${#results[@]} -gt 0 && -n "${results[1]}" ]]; then
            compadd -U -S '' -- "${results[@]}"
            return
        fi
    fi
    _bcmr "$@"
}
compdef _bcmr_with_remote bcmr
"#,
                "\ntypeset -g _bcmr_completion_version='",
                env!("CARGO_PKG_VERSION"),
                "'\n",
                "typeset -g _bcmr_completion_base_definition=\"${functions[_bcmr]}\"\n",
                "typeset -g _bcmr_completion_remote_definition=\"${functions[_bcmr_with_remote]}\"\n"
            )
        }
        Shell::Bash => {
            r#"

_bcmr_with_remote() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    local operation='' token i=1
    local -a connection_options=()
    while (( i < COMP_CWORD )); do
        token="${COMP_WORDS[i]}"
        case "$token" in
            --) break ;;
            --ssh-port|--ssh-identity|--ssh-config|--ssh-jump|--remote-backend|--config|--profile)
                if (( i + 1 < COMP_CWORD )); then
                    connection_options+=("$token" "${COMP_WORDS[i+1]}")
                fi
                (( i += 2 )) ;;
            --ssh-port=*|--ssh-identity=*|--ssh-config=*|--ssh-jump=*|--remote-backend=*|--config=*|--profile=*)
                connection_options+=("$token")
                (( i += 1 )) ;;
            copy|move) operation="$token"; (( i += 1 )) ;;
            *) (( i += 1 )) ;;
        esac
    done
    if [[ "$cur" == *:* ]] && [[ "$operation" == "copy" || "$operation" == "move" ]]; then
        local IFS=$'\n'
        if [[ "${BCMR_COMPLETION_DEBUG:-}" == 1 ]]; then
            COMPREPLY=($(bcmr "${connection_options[@]}" __complete-remote "$cur"))
        else
            COMPREPLY=($(bcmr "${connection_options[@]}" __complete-remote "$cur" 2>/dev/null))
        fi
        if [[ ${#COMPREPLY[@]} -gt 0 ]]; then
            if type compopt >/dev/null 2>&1; then
                compopt -o nospace
            fi
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

function __bcmr_complete_remote
    set -l tokens (commandline -opc)
    set -l connection_options
    set -l i 2
    while test $i -le (count $tokens)
        switch $tokens[$i]
            case --
                break
            case --ssh-port --ssh-identity --ssh-config --ssh-jump --remote-backend --config --profile
                if test (math $i + 1) -le (count $tokens)
                    set -a connection_options $tokens[$i] $tokens[(math $i + 1)]
                end
                set i (math $i + 2)
            case '--ssh-port=*' '--ssh-identity=*' '--ssh-config=*' '--ssh-jump=*' '--remote-backend=*' '--config=*' '--profile=*'
                set -a connection_options $tokens[$i]
                set i (math $i + 1)
            case '*'
                set i (math $i + 1)
        end
    end
    if test "$BCMR_COMPLETION_DEBUG" = 1
        bcmr $connection_options __complete-remote (commandline -ct)
    else
        bcmr $connection_options __complete-remote (commandline -ct) 2>/dev/null
    end
end
complete -c bcmr -n '__fish_seen_subcommand_from copy move; and string match -q "*:*" -- (commandline -ct)' -f -a '(__bcmr_complete_remote)'
"#
        }
        Shell::PowerShell => "",
        _ => "",
    }
}
