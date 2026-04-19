use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "bcmr",
    about = "Better Copy Move Remove (BCMR) - A modern CLI tool for file operations",
    version,
    author
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Output results as JSON; copy/move/remove detach to background (query with `bcmr status`)
    #[arg(long, global = true)]
    pub json: bool,

    #[arg(long = "_bg", hide = true)]
    pub _bg: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Clone, Debug)]
pub enum SparseMode {
    Always,
    Auto,
    Never,
}

impl std::fmt::Display for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Shell::Bash => write!(f, "bash"),
            Shell::Zsh => write!(f, "zsh"),
            Shell::Fish => write!(f, "fish"),
        }
    }
}

#[derive(Args, Debug)]
pub struct CopyMoveArgs {
    /// Source files and destination directory (last argument is the destination)
    #[arg(required = true, num_args = 2..)]
    pub paths: Vec<PathBuf>,

    /// Recursively process directories
    #[arg(short, long)]
    pub recursive: bool,

    /// Preserve file attributes
    #[arg(short, long)]
    pub preserve: bool,

    /// Overwrite existing files
    #[arg(short, long)]
    pub force: bool,

    /// Skip confirmation prompt when using force
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Explain what is being done
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Exclude paths matching regex pattern
    #[arg(short = 'e', long)]
    pub exclude: Option<Vec<String>>,

    /// Enable inline TUI mode (classic 3-line display)
    #[arg(short, long)]
    pub tui: bool,

    /// Run in dry-run mode (no changes)
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Hidden test mode for simulation
    #[arg(long, hide = true, value_parser = parse_test_mode)]
    pub test_mode: Option<TestMode>,

    /// Verify file integrity after operation
    #[arg(short = 'V', long, default_value_t = false)]
    pub verify: bool,

    /// Resume interrupted operation
    #[arg(short = 'C', long, default_value_t = false)]
    pub resume: bool,

    /// Use strict hash verification for resume
    #[arg(short = 's', long, default_value_t = false)]
    pub strict: bool,

    /// Append data to existing file (ignores mtime, checks size only)
    #[arg(short = 'a', long, default_value_t = false)]
    pub append: bool,

    /// Sync data to disk after operation (fsync)
    #[arg(long, default_value_t = false)]
    pub sync: bool,

    /// Parallel local file copies (default: CPU count, capped at 8)
    #[arg(short = 'j', long = "jobs")]
    pub jobs: Option<usize>,

    /// Wire compression: auto, zstd, lz4, none
    #[arg(long, default_value = "auto")]
    pub compress: String,

    /// Skip server-side BLAKE3 on GET (caller verifies another way, e.g. -V)
    #[arg(long, default_value_t = false)]
    pub fast: bool,

    /// Data-plane transport: ssh (default) or direct (AES-256-GCM TCP)
    #[arg(long, value_enum, default_value_t = DirectMode::Ssh)]
    pub direct: DirectMode,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum DirectMode {
    Ssh,
    Direct,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize shell integration
    Init {
        /// Shell to initialize (bash, zsh, fish)
        shell: Shell,

        /// Command prefix (base for aliases; empty = no prefix)
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        cmd: Option<String>,

        /// Explicit command prefix (overrides cmd if present)
        #[arg(long, requires = "cmd")]
        prefix: Option<String>,

        /// Command suffix
        #[arg(long, requires = "cmd")]
        suffix: Option<String>,

        /// Path to add to PATH
        #[arg(long)]
        path: Option<PathBuf>,

        /// No command prefix
        #[arg(long)]
        no_cmd: bool,
    },

    /// Copy files or directories
    Copy {
        #[command(flatten)]
        args: CopyMoveArgs,

        /// Copy-on-Write (reflink): force, auto, disable
        #[arg(long, num_args = 0..=1, default_missing_value = "auto")]
        reflink: Option<String>,

        /// Sparse file creation: force, auto, disable
        #[arg(long, num_args = 0..=1, default_missing_value = "auto")]
        sparse: Option<String>,

        /// Number of parallel connections (default from scp.parallel_transfers)
        #[arg(short = 'P', long)]
        parallel: Option<usize>,
    },

    /// Move files or directories
    Move {
        #[command(flatten)]
        args: CopyMoveArgs,
    },

    /// Show status of background jobs
    Status {
        /// Job ID to query (omit to list all jobs)
        job_id: Option<String>,
    },

    /// Check for updates and self-update
    Update,

    #[command(name = "__complete-remote", hide = true)]
    CompleteRemote { partial: String },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },

    /// Run as a remote helper (called via SSH, not directly by users)
    #[command(hide = true)]
    Serve {
        /// Restrict all paths to this directory (defaults to $HOME)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Listen on a TCP address instead of stdin/stdout (loopback only)
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
    },

    /// Deploy bcmr to a remote host for serve protocol support
    Deploy {
        /// Remote target (user@host)
        target: String,

        /// Installation path on remote host
        #[arg(long, default_value = "~/.local/bin/bcmr")]
        path: Option<String>,
    },

    /// Compare source and destination without making changes
    Check {
        /// Source files and destination (last argument is the destination)
        #[arg(required = true, num_args = 2..)]
        paths: Vec<PathBuf>,

        /// Recursively compare directories
        #[arg(short, long)]
        recursive: bool,

        /// Exclude paths matching regex pattern
        #[arg(short = 'e', long)]
        exclude: Option<Vec<String>>,
    },

    /// Remove files or directories
    Remove {
        /// Files or directories to remove
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// Recursively remove directories (like rm -r)
        #[arg(short, long)]
        recursive: bool,

        /// Force removal without confirmation (like rm -f)
        #[arg(short = 'f', long)]
        force: bool,

        /// Skip confirmation prompt
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// Interactively prompt before removal
        #[arg(short = 'i', long)]
        interactive: bool,

        /// Explain what is being done
        #[arg(short = 'v', long)]
        verbose: bool,

        /// Remove empty directories (like rmdir)
        #[arg(short = 'd', long)]
        dir: bool,

        /// Exclude files/directories that match these regex patterns
        #[arg(short = 'e', long, value_name = "PATTERN", value_delimiter = ',')]
        exclude: Option<Vec<String>>,

        /// Enable inline TUI mode (classic 3-line display)
        #[arg(short, long)]
        tui: bool,

        /// Run in dry-run mode (no changes)
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Hidden test mode for simulation
        #[arg(long, hide = true, value_parser = parse_test_mode)]
        test_mode: Option<TestMode>,
    },
}

#[derive(Debug, Clone)]
pub enum TestMode {
    Delay(u64),
    SpeedLimit(u64),
    None,
}

impl Commands {
    fn copy_move_args(&self) -> Option<&CopyMoveArgs> {
        match self {
            Commands::Copy { args, .. } | Commands::Move { args, .. } => Some(args),
            _ => None,
        }
    }

    pub fn get_test_mode(&self) -> TestMode {
        match self {
            Commands::Copy { args, .. } | Commands::Move { args, .. } => {
                args.test_mode.clone().unwrap_or(TestMode::None)
            }
            Commands::Remove { test_mode, .. } => test_mode.clone().unwrap_or(TestMode::None),
            _ => TestMode::None,
        }
    }

    pub fn compile_excludes(&self) -> Result<Vec<regex::Regex>, regex::Error> {
        let patterns = match self {
            Commands::Copy { args, .. } | Commands::Move { args, .. } => args.exclude.as_ref(),
            Commands::Remove { exclude, .. } | Commands::Check { exclude, .. } => exclude.as_ref(),
            _ => None,
        };

        match patterns {
            Some(p) => p.iter().map(|s| regex::Regex::new(s)).collect(),
            None => Ok(Vec::new()),
        }
    }

    pub fn is_yes(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.yes)
            || matches!(self, Commands::Remove { yes: true, .. })
    }

    pub fn should_prompt_for_overwrite(&self) -> bool {
        match self {
            Commands::Copy { args, .. } | Commands::Move { args, .. } => args.force && !args.yes,
            Commands::Remove {
                force, interactive, ..
            } => !*force && *interactive,
            _ => false,
        }
    }

    pub fn is_tui_mode(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.tui)
            || matches!(self, Commands::Remove { tui: true, .. })
    }

    pub fn is_dry_run(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.dry_run)
            || matches!(self, Commands::Remove { dry_run: true, .. })
    }

    pub fn get_sources_and_dest(&self) -> std::result::Result<(&[PathBuf], &PathBuf), String> {
        let paths = match self {
            Commands::Copy { args, .. } | Commands::Move { args, .. } => &args.paths,
            Commands::Check { paths, .. } => paths,
            _ => return Err("command does not have source/destination structure".to_string()),
        };
        let (dest, sources) = paths
            .split_last()
            .ok_or_else(|| "missing source/destination arguments".to_string())?;
        Ok((sources, dest))
    }

    pub fn is_verify(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.verify)
    }

    pub fn is_resume(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.resume)
    }

    pub fn is_strict(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.strict)
    }

    pub fn is_append(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.append)
    }

    pub fn is_sync(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.sync)
    }

    pub fn local_jobs(&self) -> usize {
        self.copy_move_args()
            .and_then(|a| a.jobs)
            .unwrap_or_else(|| num_cpus::get().clamp(1, 8))
    }

    pub fn compression_caps(&self) -> u8 {
        use crate::core::protocol::{CAP_LZ4, CAP_ZSTD};
        match self
            .copy_move_args()
            .map(|a| a.compress.as_str())
            .unwrap_or("auto")
            .to_lowercase()
            .as_str()
        {
            "none" | "off" | "disable" => 0,
            "lz4" => CAP_LZ4,
            "zstd" => CAP_ZSTD,
            _ => CAP_LZ4 | CAP_ZSTD,
        }
    }

    pub fn protocol_caps(&self) -> u8 {
        use crate::core::protocol::{CAP_DEDUP, CAP_FAST, CAP_SYNC};
        let mut caps = self.compression_caps() | CAP_DEDUP;
        if self.copy_move_args().is_some_and(|a| a.fast) {
            caps |= CAP_FAST;
        }
        if self.is_sync() {
            caps |= CAP_SYNC;
        }
        caps
    }

    pub fn use_direct_tcp(&self) -> bool {
        matches!(
            self.copy_move_args().map(|a| a.direct),
            Some(DirectMode::Direct)
        )
    }

    pub fn get_reflink_mode(&self) -> Option<String> {
        match self {
            Commands::Copy { reflink, .. } => reflink.clone(),
            _ => None,
        }
    }

    pub fn get_sparse_mode(&self) -> Option<String> {
        match self {
            Commands::Copy { sparse, .. } => sparse.clone(),
            _ => None,
        }
    }

    pub fn get_parallel(&self) -> Option<usize> {
        match self {
            Commands::Copy { parallel, .. } => *parallel,
            _ => None,
        }
    }

    pub fn is_recursive(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.recursive)
            || matches!(
                self,
                Commands::Remove {
                    recursive: true,
                    ..
                } | Commands::Check {
                    recursive: true,
                    ..
                }
            )
    }

    pub fn is_preserve(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.preserve)
    }

    pub fn is_force(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.force)
            || matches!(self, Commands::Remove { force: true, .. })
    }

    pub fn is_interactive(&self) -> bool {
        matches!(
            self,
            Commands::Remove {
                interactive: true,
                ..
            }
        )
    }

    pub fn is_verbose(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.verbose)
            || matches!(self, Commands::Remove { verbose: true, .. })
    }

    pub fn is_dir_only(&self) -> bool {
        matches!(self, Commands::Remove { dir: true, .. })
    }

    pub fn get_remove_paths(&self) -> std::result::Result<&[PathBuf], String> {
        match self {
            Commands::Remove { paths, .. } => Ok(paths),
            _ => Err("command does not support remove paths".to_string()),
        }
    }
}

pub fn parse_args() -> Cli {
    Cli::parse()
}

fn parse_test_mode(s: &str) -> Result<TestMode, String> {
    if s == "none" {
        return Ok(TestMode::None);
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 2 {
        match (parts[0], parts[1].parse::<u64>()) {
            ("delay", Ok(ms)) => Ok(TestMode::Delay(ms)),
            ("speed_limit", Ok(bps)) => Ok(TestMode::SpeedLimit(bps)),
            _ => Err(format!("Invalid test mode format: {}", s)),
        }
    } else {
        Err(format!(
            "Invalid test mode '{}'. Expected: none, delay:<ms>, or speed_limit:<bps>",
            s
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_test_mode_delay() {
        match parse_test_mode("delay:100").unwrap() {
            TestMode::Delay(ms) => assert_eq!(ms, 100),
            _ => panic!("Expected Delay"),
        }
    }

    #[test]
    fn test_parse_test_mode_speed_limit() {
        match parse_test_mode("speed_limit:1048576").unwrap() {
            TestMode::SpeedLimit(bps) => assert_eq!(bps, 1048576),
            _ => panic!("Expected SpeedLimit"),
        }
    }

    #[test]
    fn test_parse_test_mode_none() {
        match parse_test_mode("none").unwrap() {
            TestMode::None => {}
            _ => panic!("Expected None"),
        }
    }

    #[test]
    fn test_parse_test_mode_invalid() {
        assert!(parse_test_mode("invalid:abc").is_err());
    }

    fn test_args(paths: Vec<PathBuf>) -> CopyMoveArgs {
        CopyMoveArgs {
            paths,
            recursive: false,
            preserve: false,
            force: false,
            yes: false,
            verbose: false,
            exclude: None,
            tui: false,
            dry_run: false,
            test_mode: None,
            verify: false,
            resume: false,
            strict: false,
            append: false,
            sync: false,
            jobs: None,
            compress: "auto".to_string(),
            fast: false,
            direct: DirectMode::Ssh,
        }
    }

    #[test]
    fn test_commands_copy_accessors() {
        let cmd = Commands::Copy {
            args: CopyMoveArgs {
                recursive: true,
                preserve: true,
                force: true,
                verbose: true,
                exclude: Some(vec!["*.log".to_string()]),
                dry_run: true,
                verify: true,
                resume: true,
                strict: true,
                ..test_args(vec![PathBuf::from("src"), PathBuf::from("dst")])
            },
            reflink: Some("auto".to_string()),
            sparse: None,
            parallel: Some(4),
        };

        assert!(cmd.is_recursive());
        assert!(cmd.is_preserve());
        assert!(cmd.is_force());
        assert!(!cmd.is_yes());
        assert!(cmd.is_verbose());
        assert!(cmd.is_dry_run());
        assert!(!cmd.is_tui_mode());
        assert!(cmd.is_verify());
        assert!(cmd.is_resume());
        assert!(cmd.is_strict());
        assert!(!cmd.is_append());
        assert!(!cmd.is_sync());
        assert_eq!(cmd.get_reflink_mode(), Some("auto".to_string()));
        assert_eq!(cmd.get_sparse_mode(), None);
        assert_eq!(cmd.get_parallel(), Some(4));
        assert!(cmd.should_prompt_for_overwrite());
    }

    #[test]
    fn test_commands_get_sources_and_dest() {
        let cmd = Commands::Copy {
            args: test_args(vec![
                PathBuf::from("a"),
                PathBuf::from("b"),
                PathBuf::from("dest"),
            ]),
            reflink: None,
            sparse: None,
            parallel: None,
        };

        let (sources, dest) = cmd.get_sources_and_dest().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(dest, &PathBuf::from("dest"));
    }

    #[test]
    fn test_commands_remove_accessors() {
        let cmd = Commands::Remove {
            paths: vec![PathBuf::from("file.txt")],
            recursive: false,
            force: true,
            yes: false,
            interactive: true,
            verbose: false,
            dir: true,
            exclude: None,
            tui: false,
            dry_run: false,
            test_mode: None,
        };

        assert!(cmd.is_force());
        assert!(cmd.is_interactive());
        assert!(cmd.is_dir_only());
        assert!(!cmd.is_recursive());
        let paths = cmd.get_remove_paths().unwrap();
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn test_commands_non_file_defaults() {
        let cmd = Commands::Update;
        assert!(!cmd.is_recursive());
        assert!(!cmd.is_force());
        assert!(!cmd.is_preserve());
        assert!(!cmd.is_verify());
        assert!(!cmd.is_dry_run());
        assert!(!cmd.is_verbose());
        assert_eq!(cmd.get_parallel(), None);
    }

    #[test]
    fn test_protocol_caps_sync_gate() {
        use crate::core::protocol::{CAP_FAST, CAP_SYNC};

        let cmd_no_sync = Commands::Copy {
            args: test_args(vec![PathBuf::from("dst")]),
            reflink: None,
            sparse: None,
            parallel: None,
        };
        assert_eq!(
            cmd_no_sync.protocol_caps() & CAP_SYNC,
            0,
            "default has no CAP_SYNC"
        );

        let mut a = test_args(vec![PathBuf::from("dst")]);
        a.sync = true;
        a.fast = true;
        let cmd_sync_fast = Commands::Copy {
            args: a,
            reflink: None,
            sparse: None,
            parallel: None,
        };
        let caps = cmd_sync_fast.protocol_caps();
        assert_eq!(caps & CAP_SYNC, CAP_SYNC, "--sync sets CAP_SYNC");
        assert_eq!(caps & CAP_FAST, CAP_FAST, "--fast still sets CAP_FAST");
    }
}
