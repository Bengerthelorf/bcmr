use serde::Serialize;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Core output envelope — used by check and error reporting
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum CommandOutput {
    Check(CheckResult),
    Error(ErrorResult),
}

// ---------------------------------------------------------------------------
// Check
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct CheckResult {
    pub status: Status,
    pub in_sync: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<FileDiff>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub modified: Vec<FileDiff>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<FileDiff>,
    pub summary: CheckSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
}

#[derive(Serialize)]
pub struct FileDiff {
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_size: Option<u64>,
    pub is_dir: bool,
}

#[derive(Serialize)]
pub struct CheckSummary {
    pub added: u64,
    pub modified: u64,
    pub missing: u64,
    pub total_bytes: u64,
}

// ---------------------------------------------------------------------------
// Error (for --json mode error output)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ErrorResult {
    pub status: Status,
    pub error: String,
    pub error_kind: String,
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Success,
    Error,
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl CheckResult {
    pub fn error(msg: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            status: Status::Error,
            in_sync: false,
            added: Vec::new(),
            modified: Vec::new(),
            missing: Vec::new(),
            summary: CheckSummary {
                added: 0,
                modified: 0,
                missing: 0,
                total_bytes: 0,
            },
            error: Some(msg.into()),
            error_kind: Some(kind.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl CommandOutput {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("CommandOutput must be serializable")
    }

    /// Exit code: 0 = success/in-sync, 1 = check found diffs, 2 = error
    pub fn exit_code(&self) -> i32 {
        match self {
            CommandOutput::Check(r) => {
                if matches!(r.status, Status::Error) {
                    2
                } else if r.in_sync {
                    0
                } else {
                    1
                }
            }
            CommandOutput::Error(_) => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

fn error_kind_from(err: &dyn std::error::Error) -> String {
    let msg = err.to_string();
    if msg.contains("not found") {
        "source_not_found".into()
    } else if msg.contains("already exists") {
        "already_exists".into()
    } else if msg.contains("Permission denied") || msg.contains("permission denied") {
        "permission_denied".into()
    } else if msg.contains("cancelled") || msg.contains("Cancelled") {
        "cancelled".into()
    } else if msg.contains("Is a directory") || msg.contains("is a directory") {
        "is_directory".into()
    } else if msg.contains("Invalid input") {
        "invalid_input".into()
    } else {
        "io_error".into()
    }
}

pub fn error_output(command: &str, err: &anyhow::Error) -> CommandOutput {
    let kind = error_kind_from(err.as_ref());
    let msg = format!("{:#}", err);
    match command {
        "check" => CommandOutput::Check(CheckResult::error(msg, kind)),
        _ => CommandOutput::Error(ErrorResult {
            status: Status::Error,
            error: msg,
            error_kind: kind,
        }),
    }
}
