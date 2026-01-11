use crate::cli::Shell;
use std::path::PathBuf;

pub fn generate_init_script(
    shell: &Shell,
    cmd: &str,
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
# Add bcmr directory to PATH
export PATH="{}:$PATH"
"#,
                    path.display()
                ));
            }

            if !no_cmd {
                let prefix = if cmd.is_empty() { "" } else { cmd };
                script.push_str(&format!(
                    r#"
# bcmr shell integration
{prefix}cp() {{
    "{exe_path}" copy "$@"
}}

{prefix}mv() {{
    "{exe_path}" move "$@"
}}

{prefix}rm() {{
    "{exe_path}" remove "$@"
}}
"#,
                    prefix = prefix,
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
                let prefix = if cmd.is_empty() { "" } else { cmd };
                script.push_str(&format!(
                    r#"
# bcmr shell integration
function {prefix}cp
    "{exe_path}" copy $argv
end

function {prefix}mv
    "{exe_path}" move $argv
end

function {prefix}rm
    "{exe_path}" remove $argv
end
"#,
                    prefix = prefix,
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
        let script = generate_init_script(&Shell::Bash, "b", None, false);
        assert!(script.contains("bcp()"));
        assert!(script.contains("bmv()"));
        assert!(script.contains("brm()"));
    }

    #[test]
    fn test_zsh_init_script() {
        let script = generate_init_script(&Shell::Zsh, "", None, false);
        assert!(script.contains("cp()"));
        assert!(script.contains("mv()"));
        assert!(script.contains("rm()"));
    }

    #[test]
    fn test_fish_init_script() {
        let script = generate_init_script(&Shell::Fish, "b", None, false);
        assert!(script.contains("function bcp"));
        assert!(script.contains("function bmv"));
        assert!(script.contains("function brm"));
    }

    #[test]
    fn test_with_path() {
        let path = PathBuf::from("/some/path");
        let script = generate_init_script(&Shell::Bash, "", Some(&path), false);
        assert!(script.contains("export PATH=\"/some/path:$PATH\""));
    }

    #[test]
    fn test_no_cmd() {
        let script = generate_init_script(&Shell::Bash, "b", None, true);
        assert!(!script.contains("bcp()"));
        assert!(!script.contains("bmv()"));
        assert!(!script.contains("brm()"));
    }
}
