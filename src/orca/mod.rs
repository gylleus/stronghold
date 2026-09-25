//! Wrapper around the `orca` CLI: spawns `orca … --json` and parses the typed results.

pub mod client;
pub mod commands;
pub mod types;

pub use client::{ApiError, OrcaClient, OrcaError};
pub use types::*;
