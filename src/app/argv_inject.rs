use crate::config::Config;
use crate::core::remote::parse_remote_path;

const SUBCOMMANDS_THAT_TAKE_DEFAULTS: &[&str] = &["copy", "move", "check", "remove"];

pub fn inject_defaults(argv: Vec<String>, config: &Config) -> Vec<String> {
    let Some(sub_idx) = find_subcommand(&argv) else {
        return argv;
    };

    let profile_name = profile_from_argv_or_env(&argv);
    let profile_args: Vec<String> = profile_name
        .as_deref()
        .and_then(|n| config.profile.get(n))
        .map(|p| p.default_args.clone())
        .unwrap_or_default();

    let host_args: Vec<String> = first_matching_host_args(&argv[sub_idx..], config);

    if profile_args.is_empty() && host_args.is_empty() {
        return strip_profile_flag(argv);
    }

    let argv = strip_profile_flag(argv);
    let sub_idx = find_subcommand(&argv).expect("subcommand must still be present");

    let mut out = Vec::with_capacity(argv.len() + profile_args.len() + host_args.len());
    out.extend(argv[..=sub_idx].iter().cloned());
    out.extend(profile_args);
    out.extend(host_args);
    out.extend(argv[sub_idx + 1..].iter().cloned());
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
        let Some(rp) = parse_remote_path(token) else {
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
        assert_eq!(inject_defaults(argv.clone(), &cfg), argv);
    }

    #[test]
    fn host_default_args_inject_after_subcommand() {
        let cfg = cfg_with(&[("lab", &["-p", "--compress", "zstd"])], &[]);
        let argv = s(&["bcmr", "copy", "src", "lab:dst/"]);
        let out = inject_defaults(argv, &cfg);
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
        let out = inject_defaults(argv, &cfg);
        assert_eq!(out, s(&["bcmr", "copy", "-V", "-p", "src", "dst"]));
    }

    #[test]
    fn profile_then_host_appended_in_order() {
        let cfg = cfg_with(&[("lab", &["--compress", "zstd"])], &[("work", &["-V"])]);
        let argv = s(&["bcmr", "--profile", "work", "copy", "src", "lab:dst/"]);
        let out = inject_defaults(argv, &cfg);
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
    fn unknown_profile_is_silent_no_op() {
        let cfg = cfg_with(&[], &[("work", &["-V"])]);
        let argv = s(&["bcmr", "--profile", "missing", "copy", "src", "dst"]);
        let out = inject_defaults(argv, &cfg);
        assert_eq!(out, s(&["bcmr", "copy", "src", "dst"]));
    }

    #[test]
    fn no_subcommand_passthrough() {
        let cfg = cfg_with(&[("lab", &["-V"])], &[]);
        let argv = s(&["bcmr", "doctor", "lab"]);
        assert_eq!(inject_defaults(argv.clone(), &cfg), argv);
    }
}
