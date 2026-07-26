use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

const CLI_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Copy a file to a remote host:
      bcmr copy ./report.pdf host:backup/

  Recursive with attribute preservation:
      bcmr copy -r -p ./project/ host:archives/

  Resume a partial transfer:
      bcmr copy -C ./large.iso host:backup/

  Background with JSON for scripts:
      bcmr copy --json -V ./big.tar.gz host:dst/

  Compare source and destination without copying:
      bcmr check ./project/ host:archives/

ENVIRONMENT:
  BCMR_CAS_DIR                  CAS directory (default: $XDG_CACHE_HOME/bcmr/cas)
  BCMR_CAS_CAP_MB               CAS size cap in MB
  BCMR_DEBUG_SSH_STDERR=1       surface ssh stderr for debugging
  BCMR_RENDEZVOUS_TIMEOUT_SECS  direct-TCP rendezvous timeout (seconds)
  BCMR_SSH_NO_MULTIPLEX=1       disable ControlMaster (one TCP per process).
                                Only helps if the remote's sshd has been tuned
                                (e.g. MaxStartups N:0:N to disable throttling);
                                with default MaxStartups 10:30:60 this is worse
                                than the default muxed path at parallelism >10.
  BCMR_UNSAFE_LAN_LISTEN=1      opt-in to LAN listen on 'bcmr serve' (requires peer-auth)
  NO_COLOR                      any non-empty value disables colored output
  TERM=dumb                     selects plain renderer

CONFIGURATION:
  ~/.config/bcmr/config.toml      (override with $XDG_CONFIG_HOME)

EXIT CODES:
  0    success
  1    transfer error / 'bcmr check' not in sync
  2    error result in --json mode / SourceNotFound on bare 'bcmr check'
  64   invalid arguments (clap)
  130  Ctrl-C / SIGINT

DOCUMENTATION:
  https://app.snaix.homes/bcmr
";

const COPY_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Local copy with verify:
      bcmr copy -V src.iso dst.iso

  Recursive with preserve, parallel local jobs:
      bcmr copy -r -p -j 4 ./project/ ./backup/

  Upload to remote host:
      bcmr copy ./report.pdf host:archive/

  Resume a previous run after interruption:
      bcmr copy -C ./large.tar.gz host:dst/

  Background job with JSON status events:
      bcmr copy --json -V ./big.bin host:dst/   # query: bcmr status

  Sparse-aware copy:
      bcmr copy --sparse=auto disk.img dst.img

  Compress wire payload (auto-skips already-compressed files):
      bcmr copy --compress=auto src/ host:dst/
";

const MOVE_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Local rename or move (atomic when same filesystem):
      bcmr move ./old.txt ./new.txt

  Move recursively to a remote host (copy + verify + delete-source):
      bcmr move -r -V ./project/ host:archive/
";

const CHECK_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Compare a single local file against a remote one:
      bcmr check ./report.pdf host:archive/report.pdf

  Recursive directory check (size + content hash for size-matched files):
      bcmr check -r ./project/ host:archive/

  JSON output for scripts:
      bcmr check --json ./project/ host:archive/

  Skip content hashing (legacy size+mtime only):
      bcmr check --no-hash ./project/ host:archive/
";

const REMOVE_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Remove a file:
      bcmr remove ./old.log

  Recursive removal with confirmation:
      bcmr remove -r ./build/

  Force removal (no prompt) and verbose output:
      bcmr remove -rf -v ./tmp/

  Empty directory only (rmdir-like):
      bcmr remove -d ./empty-dir/

  Dry-run to preview:
      bcmr remove -rn ./candidate/
";

#[derive(Parser, Debug)]
#[command(
    name = "bcmr",
    about = "Better Copy Move Remove (BCMR) - A modern CLI tool for file operations",
    version,
    author,
    after_long_help = CLI_AFTER_LONG_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Output results as JSON; copy/move/remove detach to background (query with `bcmr status`)
    #[arg(long, global = true)]
    pub json: bool,

    /// Use this config file instead of ~/.config/bcmr/config.toml (layered on top of defaults)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Use a named profile from config (overrides BCMR_PROFILE)
    // argv_inject consumes --profile before clap parses; declared here only so it shows in --help.
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    #[arg(long = "_bg", hide = true, value_parser = parse_job_id)]
    pub _bg: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
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
            Shell::Powershell => write!(f, "powershell"),
        }
    }
}

#[derive(Args, Debug)]
pub struct CopyMoveArgs {
    /// Source files and destination directory (last argument is the destination,
    /// unless --to is given — then all positionals are sources)
    #[arg(required = true, num_args = 1..)]
    pub paths: Vec<PathBuf>,

    /// Fan-out destination (repeatable). Each --to is an additional copy target;
    /// all positional paths are sources. Errors are per-target (one failure does
    /// not abort the others).
    #[arg(long = "to", value_name = "DEST")]
    pub to: Vec<PathBuf>,

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

    /// Use plain inline progress (3-line) instead of the fancy TUI box
    #[arg(long, alias = "tui", short_alias = 't')]
    pub plain: bool,

    /// Suppress progress UI; only errors print to stderr
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Run in dry-run mode (no changes)
    #[arg(short = 'n', long)]
    pub dry_run: bool,

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

    /// Don't follow symbolic links — replicate the link itself at the destination (cp -P)
    #[arg(long = "no-deref", default_value_t = false)]
    pub no_deref: bool,

    /// Sync data to disk after operation (fsync)
    #[arg(long, default_value_t = false)]
    pub sync: bool,

    /// Parallel local file copies (default: CPU count, capped at 8)
    #[arg(short = 'j', long = "jobs", value_parser = parse_positive_usize)]
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
        /// Shell to initialize (bash, zsh, fish, powershell)
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
    #[command(after_long_help = COPY_AFTER_LONG_HELP)]
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
        #[arg(short = 'P', long, value_parser = parse_positive_usize)]
        parallel: Option<usize>,
    },

    /// Move files or directories
    #[command(after_long_help = MOVE_AFTER_LONG_HELP)]
    Move {
        #[command(flatten)]
        args: CopyMoveArgs,
    },

    /// Show status of background jobs
    Status {
        /// Job ID to query (omit to list all jobs)
        #[arg(value_parser = parse_job_id)]
        job_id: Option<String>,

        /// Remove the named job's log file (use --all to drop every job)
        #[arg(long)]
        rm: bool,

        /// With --rm, target every job log
        #[arg(long, requires = "rm", conflicts_with = "job_id")]
        all: bool,

        /// Garbage-collect job logs older than the configured retention (7d)
        #[arg(long, conflicts_with_all = ["rm", "all"])]
        gc: bool,

        /// Follow a running job's NDJSON log until its terminal result
        #[arg(long, requires = "job_id", conflicts_with_all = ["rm", "all", "gc"])]
        watch: bool,
    },

    /// Check for updates and self-update
    Update {
        /// Print the current and latest versions and exit, without installing.
        #[arg(long)]
        check: bool,
    },

    #[command(name = "__complete-remote", hide = true)]
    CompleteRemote { partial: String },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,

        /// Write the completion script to the conventional shell path
        #[arg(long)]
        install: bool,

        /// With --install, only print the target path; do not write
        #[arg(long, requires = "install")]
        print: bool,

        /// With --install, remove the file at the conventional path
        #[arg(long, requires = "install", conflicts_with = "print")]
        uninstall: bool,
    },

    /// Diagnose local + remote setup (ssh / config / bcmr presence)
    Doctor {
        /// Optional remote hosts (user@host) to probe
        #[arg(value_name = "HOST")]
        hosts: Vec<String>,
    },

    /// List files under a remote path (host:path)
    Ls {
        /// Remote path (host:path or @bookmark)
        path: PathBuf,
    },

    /// Show type and size of a remote file or directory
    Stat {
        /// Remote path (host:path or @bookmark)
        path: PathBuf,
    },

    /// Show recursive size of a remote path (du -sh equivalent)
    Du {
        /// Remote path (host:path or @bookmark)
        path: PathBuf,
    },

    /// Compute the BLAKE3 hash of a remote file
    Hash {
        /// Remote path (host:path or @bookmark)
        path: PathBuf,
    },

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

        /// Use sudo on the remote for the install step (requires passwordless sudo)
        #[arg(long)]
        sudo: bool,
    },

    /// Compare source and destination without making changes
    #[command(after_long_help = CHECK_AFTER_LONG_HELP)]
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

        /// Skip content hashing — flag size-matched, mtime-drifted files as modified
        #[arg(long = "no-hash")]
        no_hash: bool,
    },

    /// Remove files or directories
    #[command(after_long_help = REMOVE_AFTER_LONG_HELP)]
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

        /// Use plain inline progress (3-line) instead of the fancy TUI box
        #[arg(long, alias = "tui", short_alias = 't')]
        plain: bool,

        /// Suppress progress UI; only errors print to stderr
        #[arg(short = 'q', long)]
        quiet: bool,

        /// Run in dry-run mode (no changes)
        #[arg(short = 'n', long)]
        dry_run: bool,

        #[arg(long, hide = true, value_parser = parse_test_mode)]
        test_mode: Option<TestMode>,
    },
}

#[derive(Debug, Clone)]
pub enum TestMode {
    Delay(u64),
    SpeedLimit(u64),
    CorruptBeforeFinalize,
    None,
}

impl Commands {
    pub fn copy_move_args(&self) -> Option<&CopyMoveArgs> {
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

    pub fn is_plain_progress(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.plain)
            || matches!(self, Commands::Remove { plain: true, .. })
    }

    pub fn is_quiet(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.quiet)
            || matches!(self, Commands::Remove { quiet: true, .. })
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

    pub fn is_no_deref(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.no_deref)
    }

    pub fn is_sync(&self) -> bool {
        self.copy_move_args().is_some_and(|a| a.sync)
    }

    pub fn local_jobs(&self) -> usize {
        self.copy_move_args()
            .and_then(|a| a.jobs)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
                    .clamp(1, 8)
            })
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
        use crate::core::protocol::{CAP_DEDUP, CAP_FAST, CAP_PUT_OFFSET, CAP_SYNC};
        let mut caps = self.compression_caps() | CAP_DEDUP | CAP_PUT_OFFSET;
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
    let raw: Vec<String> = std::env::args().collect();
    // We can't set propagate_version: clap would auto-assign -V to every
    // subcommand, colliding with --verify. Hand-roll detection so users still
    // get `bcmr <sub> --version`.
    if subcommand_version_requested(&raw) {
        println!("bcmr {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }
    // CONFIG is Lazy and reads $BCMR_CONFIG; propagate --config before the
    // first deref so host/profile defaults come from the file the user named.
    if let Some(path) = pre_scan_config_path(&raw) {
        if let Err(msg) = validate_explicit_config(&path) {
            eprintln!("Error: {msg}");
            std::process::exit(64);
        }
        std::env::set_var("BCMR_CONFIG", path);
    }
    let injected = crate::app::argv_inject::inject_defaults(raw, &crate::config::CONFIG)
        .unwrap_or_else(|e| {
            eprintln!("Error: {e}");
            std::process::exit(64);
        });
    Cli::parse_from(injected)
}

fn validate_explicit_config(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Err(format!("--config file not found: {path}"));
    }
    config::Config::builder()
        .add_source(config::File::from(p.to_path_buf()))
        .build()
        .map_err(|e| format!("--config '{path}' is not valid: {e}"))?;
    Ok(())
}

fn subcommand_version_requested(argv: &[String]) -> bool {
    // Sync with Cli's global value-taking flags (--json is bool, excluded).
    const TOP_LEVEL_FLAGS_WITH_VALUE: &[&str] = &["--config", "--profile", "--_bg"];
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            return false;
        }
        if a.starts_with('-') {
            let takes_value = !a.contains('=') && TOP_LEVEL_FLAGS_WITH_VALUE.contains(&a.as_str());
            i += if takes_value { 2 } else { 1 };
            continue;
        }
        if a == "help" {
            return false;
        }
        return argv[i + 1..]
            .iter()
            .take_while(|t| t.as_str() != "--")
            .any(|t| t == "--version");
    }
    false
}

fn pre_scan_config_path(argv: &[String]) -> Option<String> {
    let mut iter = argv.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--config" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--config=") {
            return Some(rest.to_string());
        }
    }
    None
}

fn parse_test_mode(s: &str) -> Result<TestMode, String> {
    if s == "none" {
        return Ok(TestMode::None);
    }
    if s == "corrupt_before_finalize" {
        return Ok(TestMode::CorruptBeforeFinalize);
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
            "Invalid test mode '{}'. Expected: none, corrupt_before_finalize, delay:<ms>, or speed_limit:<bps>",
            s
        ))
    }
}

fn parse_positive_usize(s: &str) -> Result<usize, String> {
    let value = s
        .parse::<usize>()
        .map_err(|_| format!("expected a positive integer, got '{s}'"))?;
    if value == 0 {
        return Err("must be greater than zero".to_string());
    }
    Ok(value)
}

fn parse_job_id(s: &str) -> Result<String, String> {
    crate::commands::jobs::validate_job_id(s)?;
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_parallel_counts_reject_zero_during_clap_parsing() {
        for args in [
            ["bcmr", "copy", "-j0", "src", "dst"].as_slice(),
            ["bcmr", "copy", "--jobs=0", "src", "dst"].as_slice(),
            ["bcmr", "copy", "-P0", "src", "host:dst"].as_slice(),
            ["bcmr", "copy", "--parallel=0", "src", "host:dst"].as_slice(),
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "zero concurrency must be rejected by Clap: {args:?}"
            );
        }
    }

    #[test]
    fn copy_parallel_counts_accept_positive_values() {
        for args in [
            ["bcmr", "copy", "-j1", "src", "dst"].as_slice(),
            ["bcmr", "copy", "--jobs=2", "src", "dst"].as_slice(),
            ["bcmr", "copy", "-P1", "src", "host:dst"].as_slice(),
            ["bcmr", "copy", "--parallel=2", "src", "host:dst"].as_slice(),
        ] {
            assert!(
                Cli::try_parse_from(args).is_ok(),
                "positive concurrency must parse: {args:?}"
            );
        }
    }

    #[test]
    fn status_and_background_job_ids_reject_unsafe_values_during_clap_parsing() {
        for id in [
            "",
            "/absolute",
            "has/slash",
            "has\\backslash",
            ".",
            "..",
            "\0",
        ] {
            assert!(
                Cli::try_parse_from(["bcmr", "status", id]).is_err(),
                "status must reject unsafe job ID: {id:?}"
            );
            assert!(
                Cli::try_parse_from(["bcmr", "--_bg", id, "--json", "copy", "src", "dst"]).is_err(),
                "background worker must reject unsafe job ID: {id:?}"
            );
        }

        let generated = crate::commands::jobs::new_job_id();
        assert!(Cli::try_parse_from(["bcmr", "status", &generated]).is_ok());
    }

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn subcommand_version_requested_basic() {
        assert!(subcommand_version_requested(&argv(&[
            "bcmr",
            "copy",
            "--version"
        ])));
        assert!(subcommand_version_requested(&argv(&[
            "bcmr",
            "ls",
            "--version"
        ])));
        assert!(!subcommand_version_requested(&argv(&["bcmr", "--version"])));
        assert!(!subcommand_version_requested(&argv(&["bcmr"])));
    }

    #[test]
    fn subcommand_version_requested_skips_help_and_dashdash() {
        assert!(!subcommand_version_requested(&argv(&[
            "bcmr",
            "help",
            "copy",
            "--version"
        ])));
        assert!(!subcommand_version_requested(&argv(&[
            "bcmr",
            "copy",
            "--",
            "--version"
        ])));
    }

    #[test]
    fn subcommand_version_requested_handles_global_flag_values() {
        assert!(subcommand_version_requested(&argv(&[
            "bcmr",
            "--config",
            "/some/path",
            "copy",
            "--version"
        ])));
        assert!(subcommand_version_requested(&argv(&[
            "bcmr",
            "--json",
            "copy",
            "--version"
        ])));
        assert!(subcommand_version_requested(&argv(&[
            "bcmr",
            "--config=/some/path",
            "ls",
            "--version"
        ])));
    }

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
    fn test_parse_test_mode_corrupt_before_finalize() {
        match parse_test_mode("corrupt_before_finalize").unwrap() {
            TestMode::CorruptBeforeFinalize => {}
            _ => panic!("Expected CorruptBeforeFinalize"),
        }
    }

    #[test]
    fn test_parse_test_mode_invalid() {
        assert!(parse_test_mode("invalid:abc").is_err());
    }

    fn test_args(paths: Vec<PathBuf>) -> CopyMoveArgs {
        CopyMoveArgs {
            paths,
            to: Vec::new(),
            recursive: false,
            preserve: false,
            force: false,
            yes: false,
            verbose: false,
            exclude: None,
            plain: false,
            quiet: false,
            dry_run: false,
            test_mode: None,
            verify: false,
            resume: false,
            strict: false,
            append: false,
            no_deref: false,
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
        assert!(!cmd.is_plain_progress());
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
            plain: false,
            quiet: false,
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
        let cmd = Commands::Update { check: false };
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

    #[test]
    fn test_protocol_caps_advertises_put_offset() {
        use crate::core::protocol::CAP_PUT_OFFSET;
        let cmd = Commands::Copy {
            args: test_args(vec![PathBuf::from("dst")]),
            reflink: None,
            sparse: None,
            parallel: None,
        };
        assert_eq!(
            cmd.protocol_caps() & CAP_PUT_OFFSET,
            CAP_PUT_OFFSET,
            "client must advertise CAP_PUT_OFFSET so the server-negotiated \
             intersection keeps it; otherwise `-C` resume silently falls back \
             to full re-upload",
        );
    }
}
