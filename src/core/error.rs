use std::path::PathBuf;
use thiserror::Error;

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

    #[error("Cryptographic failure: {0}")]
    CryptoFailure(String),
}

impl BcmrError {
    pub fn invalid_source_file_name() -> Self {
        BcmrError::InvalidInput("Invalid source file name".into())
    }

    pub fn invalid_source_dir_name() -> Self {
        BcmrError::InvalidInput("Invalid source directory name".into())
    }

    pub fn invalid_remote_path<D: std::fmt::Display>(path: D) -> Self {
        BcmrError::InvalidInput(format!("Invalid remote path: {path}"))
    }

    pub fn pool_empty() -> Self {
        BcmrError::InvalidInput("pool is empty".into())
    }

    pub fn send_framing_taken() -> Self {
        BcmrError::InvalidInput("send framing taken".into())
    }

    pub fn writer_taken() -> Self {
        BcmrError::InvalidInput("writer already taken".into())
    }

    pub fn hash_task_join_failed<E: std::fmt::Display>(e: E) -> Self {
        BcmrError::InvalidInput(format!("hash task join: {e}"))
    }
}
