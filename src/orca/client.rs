//! Runs `orca` subcommands as child processes and decodes their JSON output.

use std::path::PathBuf;

/// Errors from invoking the `orca` CLI.
#[derive(Debug, thiserror::Error)]
pub enum OrcaError {
    #[error("failed to spawn orca: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("orca exited with {status}: {stderr}")]
    Exit { status: i32, stderr: String },
    #[error("orca returned ok:false: {0}")]
    NotOk(String),
    #[error("invalid orca JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Handle for calling the `orca` binary.
#[derive(Debug, Clone)]
pub struct OrcaClient {
    pub binary: PathBuf,
}

impl Default for OrcaClient {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("orca"),
        }
    }
}
