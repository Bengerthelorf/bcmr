use crate::config::Config;
use crate::core::remote::parse_remote_path;
use clap::CommandFactory;
use std::collections::{HashMap, HashSet};

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

    let schema = FlagSchema::new(&argv[sub_idx]);
    let host_args = first_matching_host_args(&schema.split(&argv[sub_idx + 1..]), config);
    let user_flags = option_names(&schema.split(&argv[1..]));
    let host_options = schema.split(&host_args);
    let mut profile_overrides = user_flags.clone();
    profile_overrides.extend(option_names(&host_options));
    let profile_args = drop_overridden_flags(schema.split(&profile_args), &profile_overrides);
    let host_args = drop_overridden_flags(host_options, &user_flags);

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

struct FlagSpec {
    name: String,
    takes_value: bool,
}

struct FlagSchema {
    long: HashMap<String, FlagSpec>,
    short: HashMap<char, FlagSpec>,
}

struct OptionTokens {
    name: Option<String>,
    tokens: Vec<String>,
}

impl FlagSchema {
    fn new(subcommand: &str) -> Self {
        let command = crate::cli::Cli::command();
        let subcommand = command
            .get_subcommands()
            .find(|command| command.get_name() == subcommand)
            .expect("defaulted subcommand is declared in the CLI");
        let mut schema = Self {
            long: HashMap::new(),
            short: HashMap::new(),
        };
        for argument in command.get_arguments().chain(subcommand.get_arguments()) {
            let spec = || FlagSpec {
                name: argument.get_id().as_str().to_owned(),
                takes_value: argument.get_action().takes_values(),
            };
            if let Some(long) = argument.get_long() {
                schema.long.insert(long.to_owned(), spec());
            }
            if let Some(short) = argument.get_short() {
                schema.short.insert(short, spec());
            }
        }
        schema
    }

    // Split short groups so overriding -p in a default -rp keeps -r. Preserve
    // every occurrence within a layer, including repeatable --to/--ssh-identity.
    // Option values and everything after -- must never become override flags.
    fn split(&self, arguments: &[String]) -> Vec<OptionTokens> {
        let mut out = Vec::new();
        let mut index = 0;
        while index < arguments.len() {
            let token = &arguments[index];
            if token == "--" {
                out.push(OptionTokens {
                    name: None,
                    tokens: arguments[index..].to_vec(),
                });
                break;
            }
            if let Some(long) = token.strip_prefix("--") {
                let name = long.split('=').next().unwrap_or(long);
                let spec = self.long.get(name);
                let mut tokens = vec![token.clone()];
                if !long.contains('=') && spec.is_some_and(|spec| spec.takes_value) {
                    take_option_value(arguments, &mut index, &mut tokens);
                }
                out.push(OptionTokens {
                    name: Some(spec.map_or_else(|| name.to_owned(), |spec| spec.name.clone())),
                    tokens,
                });
            } else if let Some(shorts) = token.strip_prefix('-').filter(|value| !value.is_empty()) {
                for (offset, short) in shorts.char_indices() {
                    let Some(spec) = self.short.get(&short) else {
                        out.push(OptionTokens {
                            name: None,
                            tokens: vec![format!("-{}", &shorts[offset..])],
                        });
                        break;
                    };
                    let mut tokens = vec![format!("-{short}")];
                    if spec.takes_value {
                        let attached = &shorts[offset + short.len_utf8()..];
                        if attached.is_empty() {
                            take_option_value(arguments, &mut index, &mut tokens);
                        } else {
                            tokens[0].push_str(attached);
                        }
                    }
                    out.push(OptionTokens {
                        name: Some(spec.name.clone()),
                        tokens,
                    });
                    if spec.takes_value {
                        break;
                    }
                }
            } else {
                out.push(OptionTokens {
                    name: None,
                    tokens: vec![token.clone()],
                });
            }
            index += 1;
        }
        out
    }
}

fn take_option_value(arguments: &[String], index: &mut usize, tokens: &mut Vec<String>) {
    if let Some(value) = arguments
        .get(*index + 1)
        .filter(|value| !value.starts_with('-'))
    {
        tokens.push(value.clone());
        *index += 1;
    }
}

fn option_names(options: &[OptionTokens]) -> HashSet<String> {
    options
        .iter()
        .filter_map(|option| option.name.clone())
        .collect()
}

fn drop_overridden_flags(options: Vec<OptionTokens>, overrides: &HashSet<String>) -> Vec<String> {
    options
        .into_iter()
        .filter(|option| {
            !option
                .name
                .as_ref()
                .is_some_and(|name| overrides.contains(name))
        })
        .flat_map(|option| option.tokens)
        .collect()
}

fn find_subcommand(argv: &[String]) -> Option<usize> {
    let mut position = 1;
    while position < argv.len() {
        let token = argv[position].as_str();
        if token == "--" {
            return None;
        }
        if global_option_takes_value(token) {
            position += 2;
        } else if SUBCOMMANDS_THAT_TAKE_DEFAULTS.contains(&token) {
            return Some(position);
        } else {
            position += 1;
        }
    }
    None
}

fn global_option_takes_value(token: &str) -> bool {
    matches!(
        token,
        "--config"
            | "--profile"
            | "--ssh-port"
            | "--ssh-identity"
            | "--ssh-config"
            | "--ssh-jump"
            | "--remote-backend"
            | "--_bg"
    )
}

fn profile_from_argv_or_env(argv: &[String]) -> Option<String> {
    let mut iter = argv.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
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
        if a == "--" {
            out.extend(argv[i..].iter().cloned());
            break;
        }
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

fn first_matching_host_args(options: &[OptionTokens], config: &Config) -> Vec<String> {
    if config.host.is_empty() {
        return Vec::new();
    }
    for option in options {
        let candidates: Vec<&str> = match option.name.as_deref() {
            None => option.tokens.iter().map(String::as_str).collect(),
            Some("to") => option.tokens[0]
                .strip_prefix("--to=")
                .or_else(|| option.tokens.get(1).map(String::as_str))
                .into_iter()
                .collect(),
            _ => continue,
        };
        for candidate in candidates {
            let Some(rp) = parse_remote_path(candidate) else {
                continue;
            };
            if let Some(defaults) = config.host.get(&rp.host) {
                return defaults.default_args.clone();
            }
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
    fn ssh_option_values_are_not_subcommands_or_endpoint_defaults() {
        let cfg = cfg_with(&[("identity", &["-V"]), ("actual", &["-p"])], &[]);
        let argv = s(&[
            "bcmr",
            "--ssh-jump",
            "copy",
            "copy",
            "--ssh-identity",
            "identity:key",
            "src",
            "actual:dst",
        ]);
        assert_eq!(
            inject_defaults(argv, &cfg).unwrap(),
            s(&[
                "bcmr",
                "--ssh-jump",
                "copy",
                "copy",
                "-p",
                "--ssh-identity",
                "identity:key",
                "src",
                "actual:dst"
            ])
        );
    }

    #[test]
    fn global_temporary_ssh_option_overrides_host_defaults() {
        let cfg = cfg_with(&[("actual", &["--ssh-port", "2299", "-p"])], &[]);
        let argv = s(&["bcmr", "--ssh-port", "2222", "copy", "src", "actual:dst"]);
        assert_eq!(
            inject_defaults(argv, &cfg).unwrap(),
            s(&[
                "bcmr",
                "--ssh-port",
                "2222",
                "copy",
                "-p",
                "src",
                "actual:dst"
            ])
        );
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

    #[test]
    fn cli_short_and_long_spellings_override_grouped_profile_options() {
        let cfg = cfg_with(&[], &[("work", &["-rpj4"])]);
        let argv = s(&[
            "bcmr",
            "--profile",
            "work",
            "copy",
            "--preserve",
            "-j1",
            "src",
            "dst",
        ]);
        assert_eq!(
            inject_defaults(argv, &cfg).unwrap(),
            s(&["bcmr", "copy", "-r", "--preserve", "-j1", "src", "dst"]),
        );
    }

    #[test]
    fn host_defaults_override_profile_in_either_flag_spelling() {
        let cfg = cfg_with(&[("lab", &["-j2"])], &[("work", &["--jobs", "4", "-p"])]);
        let argv = s(&["bcmr", "--profile", "work", "copy", "src", "lab:dst"]);
        assert_eq!(
            inject_defaults(argv, &cfg).unwrap(),
            s(&["bcmr", "copy", "-p", "-j2", "src", "lab:dst"]),
        );
    }

    #[test]
    fn overriding_repeatable_defaults_keeps_every_explicit_value() {
        let cfg = cfg_with(&[], &[("work", &["--ssh-identity", "default-key"])]);
        let argv = s(&[
            "bcmr",
            "--profile",
            "work",
            "copy",
            "--ssh-identity",
            "first-key",
            "--ssh-identity",
            "second-key",
            "src",
            "dst",
        ]);
        assert_eq!(
            inject_defaults(argv, &cfg).unwrap(),
            s(&[
                "bcmr",
                "copy",
                "--ssh-identity",
                "first-key",
                "--ssh-identity",
                "second-key",
                "src",
                "dst"
            ]),
        );
    }

    #[test]
    fn option_values_and_literal_paths_do_not_override_defaults() {
        let cfg = cfg_with(&[], &[("work", &["--jobs", "4", "-p"])]);
        let argv = s(&[
            "bcmr",
            "--profile",
            "work",
            "copy",
            "--exclude=--jobs",
            "--",
            "-p",
            "dst",
        ]);
        assert_eq!(
            inject_defaults(argv, &cfg).unwrap(),
            s(&[
                "bcmr",
                "copy",
                "--jobs",
                "4",
                "-p",
                "--exclude=--jobs",
                "--",
                "-p",
                "dst"
            ]),
        );
    }
}
