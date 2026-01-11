use clap::{Parser, Subcommand, ValueEnum};
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
}

#[derive(Clone, Debug, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
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

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize shell integration
    Init {
        /// Shell to initialize (bash, zsh, fish)
        shell: Shell,

        /// Command prefix to use (default: no prefix)
        #[arg(long, default_value = "")]
        cmd: String,

        /// Path to add to PATH
        #[arg(long)]
        path: Option<PathBuf>,

        /// No command prefix
        #[arg(long)]
        no_cmd: bool,
    },

    /// Copy files or directories
    Copy {
        /// Source files and destination directory
        /// Last argument is the destination
        #[arg(required = true, num_args = 2..)]
        paths: Vec<PathBuf>,

        /// Recursively copy directories
        #[arg(short, long)]
        recursive: bool,

        /// Preserve file attributes
        #[arg(short, long)]
        preserve: bool,

        /// Overwrite existing files
        #[arg(short, long)]
        force: bool,

        /// Skip confirmation prompt when using force
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// Exclude paths matching regex pattern
        #[arg(short = 'e', long)]
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

        /// Verify file integrity after copy
        #[arg(short = 'V', long, default_value_t = false)]
        verify: bool,

        /// Resume interrupted copy
        #[arg(short = 'C', long, default_value_t = false)]
        resume: bool,

        /// Use strict hash verification for resume
        #[arg(short = 's', long, default_value_t = false)]
        strict: bool,

        /// Append data to existing file (ignores mtime, checks size only)
        #[arg(short = 'a', long, default_value_t = false)]
        append: bool,

        /// Use Copy-on-Write (reflink) if supported
        /// Modes: force, auto (default), disable
        #[arg(long, num_args = 0..=1, default_missing_value = "auto")]
        reflink: Option<String>,
    },

    /// Move files or directories
    Move {
        /// Source files and destination directory
        /// Last argument is the destination
        #[arg(required = true, num_args = 2..)]
        paths: Vec<PathBuf>,

        /// Recursively move directories
        #[arg(short, long)]
        recursive: bool,

        /// Preserve file attributes
        #[arg(short, long)]
        preserve: bool,

        /// Overwrite existing files
        #[arg(short, long)]
        force: bool,

        /// Skip confirmation prompt when using force
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// Exclude paths matching regex pattern
        #[arg(short = 'e', long)]
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

        /// Verify file integrity after move
        #[arg(short = 'V', long, default_value_t = false)]
        verify: bool,

        /// Resume interrupted move (cross-device fallback only)
        #[arg(short = 'C', long, default_value_t = false)]
        resume: bool,

        /// Use strict hash verification for resume
        #[arg(short = 's', long, default_value_t = false)]
        strict: bool,

        /// Append data to existing file (ignores mtime, checks size only)
        #[arg(short = 'a', long, default_value_t = false)]
        append: bool,
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
    Delay(u64),      // Milliseconds delay
    SpeedLimit(u64), // Bytes per second
    None,
}

impl Commands {
    pub fn get_test_mode(&self) -> TestMode {
        match self {
            Commands::Copy { test_mode, .. }
            | Commands::Move { test_mode, .. }
            | Commands::Remove { test_mode, .. } => test_mode.clone().unwrap_or(TestMode::None),
            _ => TestMode::None,
        }
    }

    pub fn compile_excludes(&self) -> Result<Vec<regex::Regex>, regex::Error> {
        let patterns = match self {
            Commands::Copy { exclude, .. }
            | Commands::Move { exclude, .. }
            | Commands::Remove { exclude, .. } => exclude.as_ref(),
            _ => None,
        };

        if let Some(patterns) = patterns {
            patterns.iter().map(|p| regex::Regex::new(p)).collect()
        } else {
            Ok(Vec::new())
        }
    }



    pub fn should_prompt_for_overwrite(&self) -> bool {
        match self {
            Commands::Copy { force, yes, .. } | Commands::Move { force, yes, .. } => {
                *force && !*yes
            }
            Commands::Remove {
                force, interactive, ..
            } => !*force && *interactive,
            Commands::Init { .. } => false, // Init command never needs overwrite prompts
        }
    }

    pub fn is_tui_mode(&self) -> bool {
        match self {
            Commands::Copy { tui, .. }
            | Commands::Move { tui, .. }
            | Commands::Remove { tui, .. } => *tui,
            _ => false,
        }
    }

    pub fn is_dry_run(&self) -> bool {
        match self {
            Commands::Copy { dry_run, .. }
            | Commands::Move { dry_run, .. }
            | Commands::Remove { dry_run, .. } => *dry_run,
            _ => false,
        }
    }

    // Helper method to split paths into sources and destination
    pub fn get_sources_and_dest(&self) -> (&[PathBuf], &PathBuf) {
        match self {
            Commands::Copy { paths, .. } | Commands::Move { paths, .. } => {
                let (dest, sources) = paths
                    .split_last()
                    .expect("Clap should ensure at least 2 args");
                (sources, dest)
            }
            _ => panic!("Command does not have source/dest structure"),
        }
    }


    pub fn is_verify(&self) -> bool {
        match self {
            Commands::Copy { verify, .. } | Commands::Move { verify, .. } => *verify,
            _ => false,
        }
    }

    pub fn is_resume(&self) -> bool {
        match self {
            Commands::Copy { resume, .. } | Commands::Move { resume, .. } => *resume,
            _ => false,
        }
    }

    pub fn is_strict(&self) -> bool {
        match self {
            Commands::Copy { strict, .. } | Commands::Move { strict, .. } => *strict,
            _ => false,
        }
    }

    pub fn is_append(&self) -> bool {
        match self {
            Commands::Copy { append, .. } | Commands::Move { append, .. } => *append,
            _ => false,
        }
    }

    pub fn get_reflink_mode(&self) -> Option<String> {
        match self {
            Commands::Copy { reflink, .. } => reflink.clone(),
            _ => None,
        }
    }

    pub fn is_recursive(&self) -> bool {
        match self {
            Commands::Copy { recursive, .. }
            | Commands::Move { recursive, .. }
            | Commands::Remove { recursive, .. } => *recursive,
            _ => false,
        }
    }

    pub fn is_preserve(&self) -> bool {
        match self {
            Commands::Copy { preserve, .. } | Commands::Move { preserve, .. } => *preserve,
            _ => false,
        }
    }

    pub fn is_force(&self) -> bool {
        match self {
            Commands::Copy { force, .. }
            | Commands::Move { force, .. }
            | Commands::Remove { force, .. } => *force,
            _ => false,
        }
    }

    pub fn is_interactive(&self) -> bool {
        match self {
            Commands::Remove { interactive, .. } => *interactive,
            _ => false,
        }
    }

    pub fn is_verbose(&self) -> bool {
        match self {
            Commands::Remove { verbose, .. } => *verbose,
            _ => false,
        }
    }

    pub fn is_dir_only(&self) -> bool {
        match self {
            Commands::Remove { dir, .. } => *dir,
            _ => false,
        }
    }

    pub fn get_remove_paths(&self) -> Option<&Vec<PathBuf>> {
        match self {
            Commands::Remove { paths, .. } => Some(paths),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn get_operation_type(&self) -> &'static str {
        match self {
            Commands::Copy { .. } => "Copying",
            Commands::Move { .. } => "Moving",
            Commands::Remove { .. } => "Removing",
            Commands::Init { .. } => "Initializing",
        }
    }
}

pub fn parse_args() -> Cli {
    Cli::parse()
}

fn parse_test_mode(s: &str) -> Result<TestMode, String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 2 {
        match (parts[0], parts[1].parse::<u64>()) {
            ("delay", Ok(ms)) => Ok(TestMode::Delay(ms)),
            ("speed_limit", Ok(bps)) => Ok(TestMode::SpeedLimit(bps)),
            _ => Err(format!("Invalid test mode format: {}", s)),
        }
    } else {
        Ok(TestMode::None)
    }
}
