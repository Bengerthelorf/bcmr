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
    let exe_path = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("bcmr"))
        .display()
        .to_string();

    match shell {
        Shell::Bash | Shell::Zsh => {
            let mut script = String::new();

            if let Some(path) = path {
                script.push_str(&format!(
                    r#"
# =============================================================================
#
# Add bcmr directory to PATH
#
# =============================================================================

export PATH="{}:$PATH"
"#,
                    path.display()
                ));
            }

            if !no_cmd {
                let prefix = prefix_arg.unwrap_or(if cmd_compat.is_empty() { "" } else { cmd_compat });
                let suffix = suffix_arg.unwrap_or("");

                script.push_str(&format!(
                    r#"
# =============================================================================
#
# BCMR shell integration for {shell_name}
#
# This provides convenient shell functions that wrap bcmr commands with
# progress tracking and safety features.
#
# =============================================================================

function {prefix}cp{suffix}() {{
    "{exe_path}" copy "$@"
}}

function {prefix}mv{suffix}() {{
    "{exe_path}" move "$@"
}}

function {prefix}rm{suffix}() {{
    "{exe_path}" remove "$@"
}}

# =============================================================================
#
# To initialize bcmr, add this to your shell configuration file:
#
#   eval "$(bcmr init {shell_name})"
#
# For custom prefix (e.g., 'b' creates bcp, bmv, brm):
#
#   eval "$(bcmr init {shell_name} --cmd b)"
#
# =============================================================================
"#,
                    shell_name = shell,
                    prefix = prefix,
                    suffix = suffix,
                    exe_path = exe_path
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
                let prefix = prefix_arg.unwrap_or(if cmd_compat.is_empty() { "" } else { cmd_compat });
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
    }
}

#[allow(dead_code)]
pub fn generate_uninstall_script(shell: &Shell) -> String {
    match shell {
        Shell::Bash | Shell::Zsh => r#"
unset -f cp mv rm bcp bmv brm 2>/dev/null || true
"#
        .to_string(),

        Shell::Fish => r#"
        Shell::Fish => r#"
functions -e cp mv rm bcp bmv brm 2>/dev/null || true
"#
        .to_string(),
    }
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
        assert!(script.contains("export PATH=\"/some/path:$PATH\""));
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
        // cmd="b" acts as prefix, suffix="+" -> bcp+
        let script = generate_init_script(&Shell::Bash, "b", None, Some("+"), None, false);
        assert!(script.contains("function bcp+()"));
    }
}
