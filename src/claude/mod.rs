//! Scanner for Claude transcripts and subagents under `~/.claude/projects`.

pub mod hook;
pub mod sessions;
pub mod subagents;

pub use sessions::{HookMappings, SessionInfo, SessionLocator, SessionMatch, encode_project_dir};
