use thiserror::Error;
use std::path::PathBuf;

#[derive(Error, Debug)]
pub enum BcmrError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("WalkDir error: {0}")]
    WalkDir(#[from] walkdir::Error),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("Task join error: {0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("Path strip prefix error: {0}")]
    StripPrefix(#[from] std::path::StripPrefixError),

    #[error("Reflink failed: {0}")]
    Reflink(String),

    #[error("Destination '{0}' already exists. Use -f to force overwrite.")]
    TargetExists(PathBuf),

    #[error("Source '{0}' not found")]
    SourceNotFound(PathBuf),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Verification failed for '{0}'")]
    VerificationError(PathBuf),

    #[error("Operation cancelled")]
    Cancelled,
}
