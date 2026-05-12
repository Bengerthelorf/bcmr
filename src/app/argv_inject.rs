use crate::config::Config;
use crate::core::remote::parse_remote_path;
use std::collections::HashSet;

const SUBCOMMANDS_THAT_TAKE_DEFAULTS: &[&str] = &["copy", "move", "check", "remove"];

pub fn inject_defaults(argv: Vec<String>, config: &Config) -> Result<Vec<String>, String> {
    let profile_name = profile_from_argv_or_env(&argv);
    let argv = strip_profile_flag(argv);

    let Some(sub_idx) = find_subcommand(&argv) else {
        return Ok(argv);
    };

    let profile_args: Vec<String> = if let Some(name) = profile_name.as_deref() {
        match config.profile.get(name) {
            Some(p) => p.default_args.clone(),
            None => {
                let mut known: Vec<&str> = config.profile.keys().map(String::as_str).collect();
                known.sort_unstable();
                let known_list = if known.is_empty() {
                    "(none configured)".to_string()
                } else {
                    known.join(", ")
                };
                return Err(format!("unknown profile '{name}' (known: {known_list})"));
            }
        }
    } else {
        Vec::new()
    };

    let host_args: Vec<String> = first_matching_host_args(&argv[sub_idx + 1..], config);

    let user_long_flags = collect_user_long_flags(&argv[sub_idx + 1..]);
    let profile_args = drop_overridden_flags(profile_args, &user_long_flags);
    let host_args = drop_overridden_flags(host_args, &user_long_flags);

    if profile_args.is_empty() && host_args.is_empty() {
        return Ok(argv);
    }

    let mut out = Vec::with_capacity(argv.len() + profile_args.len() + host_args.len());
    out.extend(argv[..=sub_idx].iter().cloned());
    out.extend(profile_args);
    out.extend(host_args);
    out.extend(argv[sub_idx + 1..].iter().cloned());
    Ok(out)
}

fn collect_user_long_flags(after_sub: &[String]) -> HashSet<&str> {
    after_sub
        .iter()
        .filter_map(|a| {
            let rest = a.strip_prefix("--")?;
            if rest.is_empty() {
                return None;
            }
            Some(rest.split('=').next().unwrap())
        })
        .collect()
}

// Also drop the value token when the form is `--flag value` (no `=`).
fn drop_overridden_flags(injected: Vec<String>, user_flags: &HashSet<&str>) -> Vec<String> {
    let mut out = Vec::with_capacity(injected.len());
    let mut i = 0;
    while i < injected.len() {
        let a = &injected[i];
        let name = a.strip_prefix("--").map(|s| s.split('=').next().unwrap());
        if name.is_some_and(|n| user_flags.contains(n)) {
            let consumes_value =
                !a.contains('=') && i + 1 < injected.len() && !injected[i + 1].starts_with('-');
            i += if consumes_value { 2 } else { 1 };
        } else {
            out.push(a.clone());
            i += 1;
        }
    }
    out
}

fn find_subcommand(argv: &[String]) -> Option<usize> {
    argv.iter()
        .position(|s| SUBCOMMANDS_THAT_TAKE_DEFAULTS.contains(&s.as_str()))
}

fn profile_from_argv_or_env(argv: &[String]) -> Option<String> {
    let mut iter = argv.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--profile" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--profile=") {
            return Some(rest.to_string());
        }
    }
    std::env::var("BCMR_PROFILE").ok().filter(|s| !s.is_empty())
}

fn strip_profile_flag(argv: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--profile" {
            i += 2;
            continue;
        }
        if a.starts_with("--profile=") {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

fn first_matching_host_args(after_sub: &[String], config: &Config) -> Vec<String> {
    if config.host.is_empty() {
        return Vec::new();
    }
    for token in after_sub {
        let candidate: &str =
            if let Some(eq) = token.strip_prefix("--").and_then(|s| s.split_once('=')) {
                eq.1
            } else if token.starts_with('-') {
                continue;
            } else {
                token
            };
        let Some(rp) = parse_remote_path(candidate) else {
            continue;
        };
        if let Some(defaults) = config.host.get(&rp.host) {
            return defaults.default_args.clone();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HostDefaults, ProfileDefaults};

    fn cfg_with(hosts: &[(&str, &[&str])], profiles: &[(&str, &[&str])]) -> Config {
        let mut c = Config::default();
        for (name, args) in hosts {
            c.host.insert(
                (*name).into(),
                HostDefaults {
                    default_args: args.iter().map(|s| s.to_string()).collect(),
                },
            );
        }
        for (name, args) in profiles {
            c.profile.insert(
                (*name).into(),
                ProfileDefaults {
                    default_args: args.iter().map(|s| s.to_string()).collect(),
                },
            );
        }
        c
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn no_config_passthrough() {
        let cfg = Config::default();
        let argv = s(&["bcmr", "copy", "src", "host:dst"]);
        assert_eq!(inject_defaults(argv.clone(), &cfg).unwrap(), argv);
    }

    #[test]
    fn host_default_args_inject_after_subcommand() {
        let cfg = cfg_with(&[("lab", &["-p", "--compress", "zstd"])], &[]);
        let argv = s(&["bcmr", "copy", "src", "lab:dst/"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(
            out,
            s(&[
                "bcmr",
                "copy",
                "-p",
                "--compress",
                "zstd",
                "src",
                "lab:dst/",
            ])
        );
    }

    #[test]
    fn profile_via_flag_strips_then_injects() {
        let cfg = cfg_with(&[], &[("work", &["-V", "-p"])]);
        let argv = s(&["bcmr", "--profile", "work", "copy", "src", "dst"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(out, s(&["bcmr", "copy", "-V", "-p", "src", "dst"]));
    }

    #[test]
    fn profile_then_host_appended_in_order() {
        let cfg = cfg_with(&[("lab", &["--compress", "zstd"])], &[("work", &["-V"])]);
        let argv = s(&["bcmr", "--profile", "work", "copy", "src", "lab:dst/"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(
            out,
            s(&[
                "bcmr",
                "copy",
                "-V",
                "--compress",
                "zstd",
                "src",
                "lab:dst/",
            ])
        );
    }

    #[test]
    fn unknown_profile_errors() {
        let cfg = cfg_with(&[], &[("work", &["-V"])]);
        let argv = s(&["bcmr", "--profile", "missing", "copy", "src", "dst"]);
        let err = inject_defaults(argv, &cfg).unwrap_err();
        assert!(err.contains("unknown profile 'missing'"), "got: {err}");
        assert!(err.contains("work"), "should list known profiles: {err}");
    }

    #[test]
    fn no_subcommand_passthrough() {
        let cfg = cfg_with(&[("lab", &["-V"])], &[]);
        let argv = s(&["bcmr", "doctor", "lab"]);
        assert_eq!(inject_defaults(argv.clone(), &cfg).unwrap(), argv);
    }

    #[test]
    fn profile_stripped_on_non_defaulting_subcommand() {
        let cfg = cfg_with(&[], &[("work", &["-V"])]);
        let argv = s(&["bcmr", "--profile", "work", "doctor"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(out, s(&["bcmr", "doctor"]));
    }

    #[test]
    fn profile_value_as_subcommand_no_panic() {
        let cfg = cfg_with(&[], &[("copy", &["-V"])]);
        let argv = s(&["bcmr", "--profile", "copy", "x", "dst"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(out, s(&["bcmr", "x", "dst"]));
    }

    #[test]
    fn host_default_via_to_equals_form() {
        let cfg = cfg_with(&[("lab", &["-p"])], &[]);
        let argv = s(&["bcmr", "copy", "src", "--to=lab:dst/"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(out, s(&["bcmr", "copy", "-p", "src", "--to=lab:dst/"]));
    }

    #[test]
    fn flag_name_not_treated_as_host() {
        let cfg = cfg_with(&[("--to", &["BUG"])], &[]);
        let argv = s(&["bcmr", "copy", "src", "--to=other:dst"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(out, s(&["bcmr", "copy", "src", "--to=other:dst"]));
    }

    #[test]
    fn cli_long_flag_overrides_host_default() {
        let cfg = cfg_with(&[("lab", &["--compress", "zstd"])], &[]);
        let argv = s(&["bcmr", "copy", "--compress", "lz4", "src", "lab:dst/"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(
            out,
            s(&["bcmr", "copy", "--compress", "lz4", "src", "lab:dst/",])
        );
    }

    #[test]
    fn cli_eq_form_overrides_host_default() {
        let cfg = cfg_with(&[("lab", &["--compress", "zstd"])], &[]);
        let argv = s(&["bcmr", "copy", "--compress=lz4", "src", "lab:dst/"]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(
            out,
            s(&["bcmr", "copy", "--compress=lz4", "src", "lab:dst/"])
        );
    }

    #[test]
    fn cli_flag_overrides_profile_default() {
        let cfg = cfg_with(&[], &[("work", &["--compress", "zstd", "-p"])]);
        let argv = s(&[
            "bcmr",
            "--profile",
            "work",
            "copy",
            "--compress=lz4",
            "src",
            "dst",
        ]);
        let out = inject_defaults(argv, &cfg).unwrap();
        assert_eq!(
            out,
            s(&["bcmr", "copy", "-p", "--compress=lz4", "src", "dst"])
        );
    }
}
