use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

const CLI_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Copy a file to a remote host:
      bcmr copy ./report.pdf host:backup/

  Use a temporary SSH port and identity:
      bcmr --ssh-port 2222 --ssh-identity ~/.ssh/work copy ./report.pdf host:backup/

  Download an absolute URI path (its port overrides --ssh-port):
      bcmr copy 'scp://alice@host:2222/srv/report%20Q4.pdf' ./downloads/

  Recursive with attribute preservation:
      bcmr copy -r -p ./project/ host:archives/

  Resume a partial transfer:
      bcmr copy -C ./large.iso host:backup/

  Background with JSON for scripts:
      bcmr copy --background --json -V ./big.tar.gz host:dst/

  Compare source and destination without copying:
      bcmr check -r ./project/ host:archives/

  Mix local and remote sources, or relay between servers:
      bcmr copy local.txt 'host:remote.txt' ./downloads/
      bcmr copy -r -V 'host-a:results' 'host-b:archive/'

  Limit aggregate payload or select an SFTP-only account:
      bcmr copy --bandwidth-limit 10MiB/s 'host:large' ./
      bcmr copy --remote-backend=sftp file 'restricted:new/'
      SFTP file publication needs hardlink@openssh.com; existing SFTP
      targets cannot be force-replaced while preserving ACL/xattr safety.

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
  ~/.config/bcmr/config.{toml,yaml}, then the platform config directory.
  $BCMR_CONFIG / --config add the final override. Use bcmr doctor to inspect layers.

EXIT CODES:
  0    success
  1    operation error in human mode / 'bcmr check' not in sync
  2    invalid arguments / configuration error / runtime error in --json mode
  64   missing or unparseable explicit config / unknown profile
  130  Ctrl-C / SIGINT

DOCUMENTATION:
  https://app.snaix.homes/bcmr
";

const DEPLOY_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Upgrade a specific system installation:
      bcmr deploy iAu --path /usr/bin/bcmr --sudo

  Deploy to several hosts with the same install options:
      bcmr deploy A100 4090 --path /usr/bin/bcmr --sudo

INSTALLATION:
  The selected path is checked and verified, independently of another bcmr on PATH.
  The default is ~/.local/bin/bcmr. A different PATH entry can shadow it; each
  result shows the selected installation and the PATH-resolved path and version.
  Matching platforms upload the local executable. Other platforms download the
  release with the local version number and verify the installed version.

SUDO:
  In an interactive terminal, SSH requests a remote PTY for native sudo password
  authentication. Without a terminal, or with --json, sudo -n must be permitted.

RESULTS:
  Hosts run one at a time. A failure does not stop later hosts. The final summary
  lists installed, skipped, and failed hosts; any failure returns a nonzero exit.
";

const COPY_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Local copy with verify:
      bcmr copy -V src.iso dst.iso

  Create a destination directory and retain the source name:
      bcmr copy report.pdf ./new/parent/

  Recursive with preserve, parallel local jobs:
      bcmr copy -r -p -j 4 ./project/ ./backup/

  Upload to remote host:
      bcmr copy ./report.pdf host:archive/

  Download sources matching a star (quote for direct shell invocation):
      bcmr copy 'host:reports/report*' ./downloads/

  Resume a previous run after interruption:
      bcmr copy -C ./large.tar.gz host:dst/

  Background job with JSON status events:
      bcmr copy --background --json -V ./big.bin host:dst/   # query: bcmr status

  Sparse-aware copy:
      bcmr copy --sparse=auto disk.img dst.img

  Compress wire payload (auto-skips already-compressed files):
      bcmr copy -r --compress=auto src/ host:dst/

SOURCE STARS:
  Copy/move expand * within source path components, locally or remotely.
  Hidden names require a leading dot; matched directories still require -r.
  No matches is an error. Exact existing paths win; destinations stay literal.
  Zsh wrappers from `bcmr init zsh` accept unquoted stars; direct bcmr calls
  should quote patterns. Other pattern syntax is not expanded by bcmr.

DESTINATIONS:
  An existing directory or a trailing slash means a parent directory; missing
  directories are created. A single bare missing target names the new file or
  directory. Multiple sources require a directory target. Dry-run creates none.
";

const MOVE_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Local rename or move (atomic when same filesystem):
      bcmr move ./old.txt ./new.txt

  Move recursively to a remote host (copy + verify + delete-source):
      bcmr move -r -V ./project/ host:archive/

  Move files matching a star:
      bcmr move 'host:reports/report*' ./downloads/

SOURCE STARS:
  Same source * rules as copy. Fan-out resolves matches once; sources are
  removed only after all destinations succeed.
  Excluded paths and newly arrived source entries remain at the source.
  Cleanup uses the captured source selection after every destination succeeds.
  Moves preserve symbolic links; copies follow them unless --no-deref is set.
";

const CHECK_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Compare a single local file against a remote one:
      bcmr check ./report.pdf host:archive/report.pdf

  Recursive check (metadata triage; hash size-matched files whose mtimes differ):
      bcmr check -r ./project/ host:archive/

  JSON output for scripts:
      bcmr check -r --json ./project/ host:archive/

  Skip content hashing (legacy size+mtime only):
      bcmr check -r --no-hash ./project/ host:archive/

DIRECTORIES:
  An existing destination directory is a parent: source project/ is compared
  against destination/project/. Use the same parent passed to recursive copy.
  Matching size + mtime is trusted for directory entries. Explicit file pairs
  are always hash-checked unless --no-hash is given.
";

const INIT_AFTER_LONG_HELP: &str = "\
ACTIVATION:
  This command prints a script. Evaluate or source it in your shell to activate it.
  The default prefix is empty: cp, mv, and rm become bcmr shell functions.
  Use --cmd b for bcp, bmv, and brm. Zsh also activates their completions.
  Zsh copy/move wrappers use noglob aliases so bcmr can expand source stars.

EXAMPLES:
  Zsh (add to ~/.zshrc and run in the current shell):
      eval \"$(bcmr init zsh --cmd b)\"

  Bash (add to ~/.bashrc):
      eval \"$(bcmr init bash --cmd b)\"

  Fish (add to ~/.config/fish/config.fish):
      bcmr init fish --cmd b | source

  PowerShell (add to $PROFILE):
      bcmr init powershell --cmd b | Out-String | Invoke-Expression

  Add to PATH without defining wrappers or activating zsh completions:
      eval \"$(bcmr init zsh --no-cmd --path ~/.local/bin)\"
";

const COMPLETIONS_AFTER_LONG_HELP: &str = "\
EXAMPLES:
  Activate zsh completions and bcp/bmv/brm in the current shell:
      eval \"$(bcmr init zsh --cmd b)\"

  Install a zsh completion file (does not activate it in the current shell):
      bcmr completions zsh --install

  Print the install path without writing:
      bcmr completions zsh --install --print

  Remove the installed file:
      bcmr completions zsh --install --uninstall

  For file-based zsh loading, add the installed directory to fpath before compinit.
  PowerShell has no --install support; append the generated script to $PROFILE.
";

const UPDATE_AFTER_LONG_HELP: &str = "\
HOMEBREW:
  Homebrew-managed binaries must be upgraded through Homebrew:
      brew update
      brew upgrade Bengerthelorf/tap/bcmr

  Checking for a newer release is still available:
      bcmr update --check
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

    /// Emit structured JSON/NDJSON output without changing execution mode
    #[arg(long, global = true)]
    pub json: bool,

    /// Run copy/move/remove as a background job (query with `bcmr status`)
    #[arg(short = 'b', long, global = true)]
    pub background: bool,

    /// Layer this config file over defaults and discovered configuration
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Argument defaults for copy/move/remove/check (overrides BCMR_PROFILE)
    // argv_inject consumes --profile before clap parses; declared here only so it shows in --help.
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    /// Temporary SSH port (an explicit scp:// endpoint port takes precedence)
    #[arg(long, global = true, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub ssh_port: Option<u16>,

    /// Temporary SSH identity file (repeat to offer several keys)
    #[arg(long, global = true, value_name = "PATH")]
    pub ssh_identity: Vec<PathBuf>,

    /// Temporary OpenSSH configuration file (separate from bcmr --config)
    #[arg(long, global = true, value_name = "PATH")]
    pub ssh_config: Option<PathBuf>,

    /// Temporary SSH jump host or comma-separated jump chain
    #[arg(long, global = true, value_name = "HOST")]
    pub ssh_jump: Option<String>,

    /// Remote backend: auto detects exec-denied SFTP accounts; ssh uses serve/shell
    #[arg(long, global = true, default_value = "auto", value_parser = ["auto", "ssh", "sftp"])]
    pub remote_backend: String,

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CompressionMode {
    Auto,
    Zstd,
    Lz4,
    None,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ProgressMode {
    #[default]
    Auto,
    Tui,
    Inline,
    Plain,
    Off,
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

#[derive(Args, Debug, Clone)]
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

    /// Progress display: auto, tui, inline, plain, or off
    #[arg(long, value_enum, default_value = "auto")]
    pub progress: ProgressMode,

    /// Suppress progress UI; only errors print to stderr
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Run in dry-run mode (no changes)
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    #[arg(long, hide = true, value_parser = parse_test_mode)]
    pub test_mode: Option<TestMode>,

    /// Verify copied data with BLAKE3 (move: copy phase only)
    #[arg(short = 'V', long, default_value_t = false)]
    pub verify: bool,

    /// Resume interrupted operation
    #[arg(short = 'C', long, default_value_t = false)]
    pub resume: bool,

    /// Use strict hash verification for resume
    #[arg(short = 's', long, default_value_t = false)]
    pub strict: bool,

    /// Append to partial destination (local: size only; remote: hash-checked prefix)
    #[arg(short = 'a', long, default_value_t = false)]
    pub append: bool,

    /// Preserve symbolic links instead of following them (moves always preserve links)
    #[arg(long = "no-deref", default_value_t = false)]
    pub no_deref: bool,

    /// Sync data to disk after operation (fsync)
    #[arg(long, default_value_t = false)]
    pub sync: bool,

    /// Parallel local file copies (default: CPU count, capped at 8)
    #[arg(short = 'j', long = "jobs", value_parser = parse_positive_usize)]
    pub jobs: Option<usize>,

    /// Aggregate payload bytes/s across all workers (e.g. 10MiB or 5MB/s)
    #[arg(long, value_name = "RATE", value_parser = crate::core::bandwidth::parse_rate)]
    pub bandwidth_limit: Option<u64>,

    /// Wire compression: auto, zstd, lz4, none
    #[arg(long, value_enum, default_value = "auto")]
    pub compress: CompressionMode,

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
    /// Print a shell integration script (evaluate/source it to activate)
    #[command(after_long_help = INIT_AFTER_LONG_HELP)]
    Init {
        /// Shell to initialize (bash, zsh, fish, powershell)
        shell: Shell,

        /// Wrapper prefix (default: empty creates cp/mv/rm; --cmd b creates bcp/bmv/brm)
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        cmd: Option<String>,

        /// Explicit wrapper prefix (requires --cmd; overrides its value)
        #[arg(long, requires = "cmd")]
        prefix: Option<String>,

        /// Wrapper suffix (requires --cmd)
        #[arg(long, requires = "cmd")]
        suffix: Option<String>,

        /// Path to add to PATH
        #[arg(long)]
        path: Option<PathBuf>,

        /// Do not define command wrappers or activate zsh completions
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
    #[command(after_long_help = UPDATE_AFTER_LONG_HELP)]
    Update {
        /// Print the current and latest versions and exit, without installing.
        #[arg(long)]
        check: bool,
    },

    #[command(name = "__complete-remote", hide = true)]
    CompleteRemote { partial: String },

    /// Generate shell completions
    #[command(after_long_help = COMPLETIONS_AFTER_LONG_HELP)]
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

    /// Deploy bcmr to one or more remote hosts for serve protocol support
    #[command(after_long_help = DEPLOY_AFTER_LONG_HELP)]
    Deploy {
        /// Remote targets (SSH aliases or user@host); failures do not stop later hosts
        #[arg(required = true, num_args = 1..)]
        targets: Vec<String>,

        /// Installation path to check, upgrade, and verify on every remote host
        #[arg(long, default_value = "~/.local/bin/bcmr")]
        path: Option<String>,

        /// Use remote sudo (terminal: password prompt; non-interactive/JSON: sudo -n)
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

        /// Progress display: auto, tui, inline, plain, or off
        #[arg(long, value_enum, default_value = "auto")]
        progress: ProgressMode,

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
    #[cfg(feature = "test-support")]
    TruncateSourceAfterSnapshot,
    #[cfg(feature = "test-support")]
    TruncateSourceAfterSnapshotDelay,
    #[cfg(feature = "test-support")]
    TruncateSourceAfterSnapshotSpeedLimit,
    #[cfg(feature = "test-support")]
    TruncateSourceAfterStageWrite,
    #[cfg(feature = "test-support")]
    CreateDestinationBeforeFinalize,
    #[cfg(feature = "test-support")]
    ReplaceDestinationBeforeFinalize,
    #[cfg(feature = "test-support")]
    ReplaceDestinationAfterResumeResolution,
    #[cfg(feature = "test-support")]
    ReplaceDestinationWithFifoAfterObservation,
    #[cfg(feature = "test-support")]
    CreateDestinationHardlinkBeforeFinalize,
    #[cfg(feature = "test-support")]
    FailSymlinkCreate,
    #[cfg(feature = "test-support")]
    FailSymlinkCommit,
    #[cfg(feature = "test-support")]
    CreateDestinationBeforeSymlinkCommit,
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

    pub fn progress_mode(&self) -> ProgressMode {
        self.copy_move_args()
            .map(|args| args.progress)
            .or(match self {
                Commands::Remove { progress, .. } => Some(*progress),
                _ => None,
            })
            .unwrap_or_default()
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
            .map(|args| args.compress)
            .unwrap_or(CompressionMode::Auto)
        {
            CompressionMode::None => 0,
            CompressionMode::Lz4 => CAP_LZ4,
            CompressionMode::Zstd => CAP_ZSTD,
            CompressionMode::Auto => CAP_LZ4 | CAP_ZSTD,
        }
    }

    pub fn protocol_caps(&self) -> u8 {
        use crate::core::protocol::{CAP_DEDUP, CAP_FAST, CAP_PUT_OFFSET, CAP_SYNC};
        let mut caps = self.compression_caps() | CAP_DEDUP | CAP_PUT_OFFSET;
        if self.copy_move_args().is_some_and(|a| a.fast) && !self.is_verify() {
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
    // subcommand, colliding with --verify. Probe long-only subcommand versions.
    if subcommand_version_requested(&raw) {
        println!("bcmr {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }
    // Let clap identify real help/version requests before touching config.
    // This respects option values and `--`, unlike scanning for flag text.
    match Cli::try_parse_from(&raw) {
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            error.exit();
        }
        Ok(cli)
            if matches!(
                &cli.command,
                Commands::Doctor { .. } | Commands::Init { .. } | Commands::Completions { .. }
            ) =>
        {
            if let Some(path) = &cli.config {
                std::env::set_var("BCMR_CONFIG", path);
            }
            return cli;
        }
        _ => {}
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
    use clap::CommandFactory;

    let command = Cli::command()
        .disable_version_flag(true)
        .mut_subcommands(|sub| {
            sub.version(env!("CARGO_PKG_VERSION"))
                .disable_version_flag(true)
                .arg(
                    clap::Arg::new("version")
                        .long("version")
                        .action(clap::ArgAction::Version),
                )
        });
    matches!(
        command.try_get_matches_from(argv),
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayVersion
    )
}

fn pre_scan_config_path(argv: &[String]) -> Option<String> {
    let mut iter = argv.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
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
    #[cfg(feature = "test-support")]
    match s {
        "truncate_source_after_snapshot" => {
            return Ok(TestMode::TruncateSourceAfterSnapshot);
        }
        "truncate_source_after_snapshot_delay" => {
            return Ok(TestMode::TruncateSourceAfterSnapshotDelay);
        }
        "truncate_source_after_snapshot_speed_limit" => {
            return Ok(TestMode::TruncateSourceAfterSnapshotSpeedLimit);
        }
        "truncate_source_after_stage_write" => {
            return Ok(TestMode::TruncateSourceAfterStageWrite);
        }
        "create_destination_before_finalize" => {
            return Ok(TestMode::CreateDestinationBeforeFinalize);
        }
        "replace_destination_before_finalize" => {
            return Ok(TestMode::ReplaceDestinationBeforeFinalize);
        }
        "replace_destination_after_resume_resolution" => {
            return Ok(TestMode::ReplaceDestinationAfterResumeResolution);
        }
        "replace_destination_with_fifo_after_observation" => {
            return Ok(TestMode::ReplaceDestinationWithFifoAfterObservation);
        }
        "create_destination_hardlink_before_finalize" => {
            return Ok(TestMode::CreateDestinationHardlinkBeforeFinalize);
        }
        "fail_symlink_create" => {
            return Ok(TestMode::FailSymlinkCreate);
        }
        "fail_symlink_commit" => {
            return Ok(TestMode::FailSymlinkCommit);
        }
        "create_destination_before_symlink_commit" => {
            return Ok(TestMode::CreateDestinationBeforeSymlinkCommit);
        }
        _ => {}
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
    fn temporary_ssh_options_parse_globally_without_stealing_existing_flags() {
        let cli = Cli::try_parse_from([
            "bcmr",
            "--ssh-port",
            "2222",
            "copy",
            "--ssh-identity",
            "first key",
            "--ssh-identity",
            "second key",
            "--ssh-config",
            "ssh config",
            "--ssh-jump",
            "jump",
            "--config",
            "bcmr.toml",
            "-P",
            "3",
            "src",
            "host:dst",
        ])
        .unwrap();
        assert_eq!(cli.ssh_port, Some(2222));
        assert_eq!(
            cli.ssh_identity,
            [PathBuf::from("first key"), PathBuf::from("second key")]
        );
        assert_eq!(cli.ssh_config, Some(PathBuf::from("ssh config")));
        assert_eq!(cli.config, Some(PathBuf::from("bcmr.toml")));
        assert_eq!(cli.ssh_jump.as_deref(), Some("jump"));
        assert_eq!(cli.command.get_parallel(), Some(3));
        for port in ["0", "65536", "-1", "abc"] {
            assert!(Cli::try_parse_from(["bcmr", "--ssh-port", port, "doctor"]).is_err());
        }
    }

    #[test]
    fn background_execution_is_an_explicit_global_flag() {
        let parsed = Cli::try_parse_from(["bcmr", "--background", "copy", "--yes", "src", "dst"]);
        assert!(
            parsed.is_ok(),
            "background execution must not be coupled to the output format: {parsed:?}"
        );
        assert!(parsed.unwrap().background);
    }

    #[test]
    fn compression_mode_rejects_unknown_values() {
        assert!(
            Cli::try_parse_from(["bcmr", "copy", "--compress=bogus", "src", "dst"]).is_err(),
            "a mistyped compression mode must not silently become auto"
        );
    }

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
            progress: ProgressMode::Auto,
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
            bandwidth_limit: None,
            compress: CompressionMode::Auto,
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
        assert_eq!(cmd.progress_mode(), ProgressMode::Auto);
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
            progress: ProgressMode::Auto,
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

        let mut verify_args = test_args(vec![PathBuf::from("dst")]);
        verify_args.fast = true;
        verify_args.verify = true;
        let cmd_verify_fast = Commands::Copy {
            args: verify_args,
            reflink: None,
            sparse: None,
            parallel: None,
        };
        assert_eq!(
            cmd_verify_fast.protocol_caps() & CAP_FAST,
            0,
            "--verify must retain the streaming server hash needed before atomic publish"
        );
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
