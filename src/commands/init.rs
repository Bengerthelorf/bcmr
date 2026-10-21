use crate::cli::Shell;
use std::path::PathBuf;

pub fn generate_init_script(
    shell: &Shell,
    cmd_compat: &str,
    prefix_arg: Option<&str>,
    suffix_arg: Option<&str>,
    path: Option<&PathBuf>,
    no_cmd: bool,
) -> String {
    let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bcmr"));
    // Homebrew removes old kegs during upgrades. Keep sourced wrappers on the
    // verified opt link so they follow the active version in the same shell.
    let exe_path = crate::app::installation::homebrew_from_executable(&exe_path)
        .and_then(|installation| installation.stable_executable)
        .unwrap_or(exe_path)
        .display()
        .to_string();

    match shell {
        Shell::Bash | Shell::Zsh => {
            let mut script = String::new();
            let shell_exe = shell_word(&exe_path);

            if let Some(path) = path {
                script.push_str(&format!(
                    r#"
# Add bcmr directory to PATH
export PATH={}:"$PATH"
"#,
                    shell_word(&path.to_string_lossy())
                ));
            }

            if !no_cmd {
                let prefix = prefix_arg.unwrap_or(if cmd_compat.is_empty() {
                    ""
                } else {
                    cmd_compat
                });
                let suffix = suffix_arg.unwrap_or("");

                script.push_str(&format!(
                    r#"
# bcmr shell integration for {shell_name}
function {prefix}cp{suffix}() {{
    {shell_exe} copy "$@"
}}

function {prefix}mv{suffix}() {{
    {shell_exe} move "$@"
}}

function {prefix}rm{suffix}() {{
    {shell_exe} remove "$@"
}}
"#,
                    shell_name = shell,
                    prefix = prefix,
                    suffix = suffix,
                    shell_exe = shell_exe
                ));

                if matches!(shell, Shell::Zsh) {
                    script.push_str(&format!(
                        "\n# Let bcmr expand source stars, including remote paths, before zsh NOMATCH.\nalias {prefix}cp{suffix}='noglob {prefix}cp{suffix}'\nalias {prefix}mv{suffix}='noglob {prefix}mv{suffix}'\n"
                    ));
                    script.push_str(&format!(
                        r#"
# Initialize completion only when necessary. Keep compinit's security checks.
# An anonymous function keeps temporary state out of the user's shell.
() {{
    local _bcmr_completion_script
    if (( ! $+functions[compdef] )); then
        if ! {{ autoload -Uz compinit && compinit; }}; then
            print -u2 -- 'bcmr: could not initialize zsh completions with compinit; command wrappers remain available. Check compinit/compaudit diagnostics, then retry bcmr init.'
            return 0
        fi
    fi
    if (( ! $+functions[compdef] )); then
        print -u2 -- 'bcmr: compinit did not provide compdef; command wrappers remain available.'
        return 0
    fi

    # Reload stale scripts even when sourced again after current-version init.
    # Matching cached definitions keep repeated initialization free.
    if [[ ${{_bcmr_completion_version-}} != "{completion_version}" ||
          "${{functions[_bcmr]-}}" != "${{_bcmr_completion_base_definition-}}" ||
          "${{functions[_bcmr_with_remote]-}}" != "${{_bcmr_completion_remote_definition-}}" ]] ||
        (( ! $+functions[_bcmr] || ! $+functions[_bcmr_with_remote] )); then
        if _bcmr_completion_script="$({shell_exe} completions zsh)"; then
            if ! eval "$_bcmr_completion_script"; then
                print -u2 -- 'bcmr: could not load zsh completion script; command wrappers remain available. Retry bcmr completions zsh to diagnose.'
                return 0
            fi
        else
            print -u2 -- 'bcmr: could not generate zsh completions; command wrappers remain available. Retry bcmr completions zsh to diagnose.'
            return 0
        fi
    fi
    if [[ ${{_bcmr_completion_version-}} != "{completion_version}" ||
          "${{functions[_bcmr]-}}" != "${{_bcmr_completion_base_definition-}}" ||
          "${{functions[_bcmr_with_remote]-}}" != "${{_bcmr_completion_remote_definition-}}" ]] ||
        (( ! $+functions[_bcmr] || ! $+functions[_bcmr_with_remote] )); then
        print -u2 -- 'bcmr: zsh completion script did not define the expected completion definitions or version; command wrappers remain available.'
        return 0
    fi
    compdef _bcmr_with_remote bcmr

    _{prefix}cp{suffix}() {{
        local -a words=("bcmr" "copy" "${{words[@]:1}}")
        local -i CURRENT=$(( CURRENT + 1 ))
        _bcmr_with_remote "$@"
    }}
    compdef _{prefix}cp{suffix} {prefix}cp{suffix}

    _{prefix}mv{suffix}() {{
        local -a words=("bcmr" "move" "${{words[@]:1}}")
        local -i CURRENT=$(( CURRENT + 1 ))
        _bcmr_with_remote "$@"
    }}
    compdef _{prefix}mv{suffix} {prefix}mv{suffix}

    _{prefix}rm{suffix}() {{
        local -a words=("bcmr" "remove" "${{words[@]:1}}")
        local -i CURRENT=$(( CURRENT + 1 ))
        _bcmr "$@"
    }}
    compdef _{prefix}rm{suffix} {prefix}rm{suffix}
}}
"#,
                        prefix = prefix,
                        suffix = suffix,
                        shell_exe = shell_exe,
                        completion_version = env!("CARGO_PKG_VERSION"),
                    ));
                }

                script.push_str(&format!(
                    r#"
# To initialize bcmr, add this to your shell rc file:
#   eval "$(bcmr init {shell_name})"
# For a custom prefix (e.g. 'b' creates bcp/bmv/brm):
#   eval "$(bcmr init {shell_name} --cmd b)"
"#,
                    shell_name = shell,
                ));
            }

            script
        }
        Shell::Fish => {
            let mut script = String::new();

            if let Some(path) = path {
                script.push_str(&format!(
                    r#"
# Add bcmr directory to PATH
fish_add_path "{}"
"#,
                    path.display()
                ));
            }

            if !no_cmd {
                let prefix = prefix_arg.unwrap_or(if cmd_compat.is_empty() {
                    ""
                } else {
                    cmd_compat
                });
                let suffix = suffix_arg.unwrap_or("");

                script.push_str(&format!(
                    r#"
# bcmr shell integration
function {prefix}cp{suffix}
    "{exe_path}" copy $argv
end

function {prefix}mv{suffix}
    "{exe_path}" move $argv
end

function {prefix}rm{suffix}
    "{exe_path}" remove $argv
end
"#,
                    prefix = prefix,
                    suffix = suffix,
                    exe_path = exe_path
                ));
            }

            script
        }
        Shell::Powershell => {
            let mut script = String::new();

            if let Some(path) = path {
                script.push_str(&format!(
                    r#"
# Add bcmr directory to PATH
$env:PATH = "{};" + $env:PATH
"#,
                    path.display()
                ));
            }

            if !no_cmd {
                let prefix = prefix_arg.unwrap_or(if cmd_compat.is_empty() {
                    ""
                } else {
                    cmd_compat
                });
                let suffix = suffix_arg.unwrap_or("");

                script.push_str(&format!(
                    r#"
# bcmr shell integration for powershell
function {prefix}cp{suffix} {{ & "{exe_path}" copy @args }}
function {prefix}mv{suffix} {{ & "{exe_path}" move @args }}
function {prefix}rm{suffix} {{ & "{exe_path}" remove @args }}
"#,
                    prefix = prefix,
                    suffix = suffix,
                    exe_path = exe_path
                ));
            }

            script
        }
    }
}

// Quote one literal POSIX-shell word, including quotes and substitutions.
fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bash_init_script() {
        let script = generate_init_script(&Shell::Bash, "b", None, None, None, false);
        assert!(script.contains("function bcp()"));
        assert!(script.contains("function bmv()"));
        assert!(script.contains("function brm()"));
    }

    #[test]
    fn test_zsh_init_script() {
        let script = generate_init_script(&Shell::Zsh, "", None, None, None, false);
        assert!(script.contains("function cp()"));
        assert!(script.contains("function mv()"));
        assert!(script.contains("function rm()"));
    }

    #[test]
    fn test_fish_init_script() {
        let script = generate_init_script(&Shell::Fish, "b", None, None, None, false);
        assert!(script.contains("function bcp"));
        assert!(script.contains("function bmv"));
        assert!(script.contains("function brm"));
    }

    #[test]
    fn test_with_path() {
        let path = PathBuf::from("/some/path");
        let script = generate_init_script(&Shell::Bash, "", None, None, Some(&path), false);
        assert!(script.contains("export PATH='/some/path':\"$PATH\""));
    }

    #[test]
    fn test_no_cmd() {
        let script = generate_init_script(&Shell::Bash, "b", None, None, None, true);
        assert!(!script.contains("function bcp()"));
        assert!(!script.contains("function bmv()"));
        assert!(!script.contains("function brm()"));
    }

    #[test]
    fn test_suffix_only() {
        let script = generate_init_script(&Shell::Bash, "", None, Some("+"), None, false);
        assert!(script.contains("function cp+()"));
    }

    #[test]
    fn test_prefix_and_suffix() {
        let script = generate_init_script(&Shell::Bash, "", Some("b"), Some("+"), None, false);
        assert!(script.contains("function bcp+()"));
    }

    #[test]
    fn test_compat_cmd_with_suffix() {
        let script = generate_init_script(&Shell::Bash, "b", None, Some("+"), None, false);
        assert!(script.contains("function bcp+()"));
    }

    #[test]
    fn test_powershell_init_script() {
        let script = generate_init_script(&Shell::Powershell, "b", None, None, None, false);
        assert!(script.contains("function bcp"));
        assert!(script.contains("function bmv"));
        assert!(script.contains("function brm"));
        assert!(script.contains("@args"));
    }

    #[test]
    fn test_powershell_with_path() {
        let path = PathBuf::from(r"C:\bcmr");
        let script = generate_init_script(&Shell::Powershell, "", None, None, Some(&path), false);
        assert!(script.contains(r#"$env:PATH = "C:\bcmr;" + $env:PATH"#));
    }
}
