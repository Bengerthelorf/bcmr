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
        /// Source file or directory
        #[arg(value_name = "SOURCE")]
        source: PathBuf,

        /// Destination file or directory
        #[arg(value_name = "DESTINATION")]
        destination: PathBuf,

        /// Recursively copy directories
        #[arg(short, long)]
        recursive: bool,

        /// Preserve file attributes (mode, ownership, timestamps)
        #[arg(long)]
        preserve: bool,

        /// Force overwrite destination if exists
        #[arg(short = 'f', long)]
        force: bool,

        /// Skip confirmation prompt when using force
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// Exclude files/directories that match these patterns
        #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
        exclude: Option<Vec<String>>,

        /// Use plain text progress
        #[arg(long)]
        plain_progress: bool,

        /// Hidden test mode with artificial delay
        #[arg(long, hide = true)]
        test_mode: Option<String>,
    },
    
    /// Move files or directories
    Move {
        /// Source file or directory
        #[arg(value_name = "SOURCE")]
        source: PathBuf,

        /// Destination file or directory
        #[arg(value_name = "DESTINATION")]
        destination: PathBuf,

        /// Recursively move directories
        #[arg(short, long)]
        recursive: bool,

        /// Preserve file attributes (mode, ownership, timestamps)
        #[arg(long)]
        preserve: bool,

        /// Force overwrite destination if exists
        #[arg(short = 'f', long)]
        force: bool,

        /// Skip confirmation prompt when using force
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// Exclude files/directories that match these patterns
        #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
        exclude: Option<Vec<String>>,

        /// Use plain text progress
        #[arg(long)]
        plain_progress: bool,

        /// Hidden test mode with artificial delay
        #[arg(long, hide = true)]
        test_mode: Option<String>,
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

        /// Exclude files/directories that match these patterns
        #[arg(long, value_name = "PATTERN", value_delimiter = ',')]
        exclude: Option<Vec<String>>,

        /// Use plain text progress
        #[arg(long)]
        plain_progress: bool,

        /// Hidden test mode with artificial delay
        #[arg(long, hide = true)]
        test_mode: Option<String>,
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
            Commands::Copy { test_mode, .. } | 
            Commands::Move { test_mode, .. } |
            Commands::Remove { test_mode, .. } => {
                if let Some(test_mode) = test_mode {
                    let parts: Vec<&str> = test_mode.split(':').collect();
                    if parts.len() == 2 {
                        match (parts[0], parts[1].parse::<u64>()) {
                            ("delay", Ok(ms)) => TestMode::Delay(ms),
                            ("speed_limit", Ok(bps)) => TestMode::SpeedLimit(bps),
                            _ => TestMode::None,
                        }
                    } else {
                        TestMode::None
                    }
                } else {
                    TestMode::None
                }
            }
            _ => TestMode::None,
        }
    }

    pub fn should_exclude(&self, path: &str) -> bool {
        match self {
            Commands::Copy { exclude, .. } | 
            Commands::Move { exclude, .. } |
            Commands::Remove { exclude, .. } => {
                if let Some(patterns) = exclude {
                    patterns.iter().any(|pattern| path.contains(pattern))
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    pub fn should_prompt_for_overwrite(&self) -> bool {
        match self {
            Commands::Copy { force, yes, .. } | Commands::Move { force, yes, .. } => *force && !*yes,
            Commands::Remove { force, interactive, .. } => !*force && *interactive,
            Commands::Init { .. } => false, // Init command never needs overwrite prompts
        }
    }

    pub fn is_plain_progress(&self) -> bool {
        match self {
            Commands::Copy { plain_progress, .. } | 
            Commands::Move { plain_progress, .. } |
            Commands::Remove { plain_progress, .. } => *plain_progress,
            _ => false,
        }
    }

    // Helper methods to get common fields
    pub fn get_source(&self) -> &PathBuf {
        match self {
            Commands::Copy { source, .. } | Commands::Move { source, .. } => source,
            Commands::Remove { paths, .. } => &paths[0],
            _ => panic!("Command doesn't have a source path"),
        }
    }

    pub fn get_destination(&self) -> &PathBuf {
        match self {
            Commands::Copy { destination, .. } | Commands::Move { destination, .. } => destination,
            _ => panic!("Command doesn't have a destination path"),
        }
    }

    pub fn is_recursive(&self) -> bool {
        match self {
            Commands::Copy { recursive, .. } | 
            Commands::Move { recursive, .. } |
            Commands::Remove { recursive, .. } => *recursive,
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
            Commands::Copy { force, .. } | 
            Commands::Move { force, .. } |
            Commands::Remove { force, .. } => *force,
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