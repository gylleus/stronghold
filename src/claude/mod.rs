//! Scanner for Claude transcripts and subagents under `~/.claude/projects`.

pub mod forwards;
pub mod hook;
pub mod sessions;
pub mod subagents;
pub mod transcript;
pub mod usage;
pub mod worker;

pub use forwards::ForwardScanner;
pub use sessions::{HookMappings, SessionInfo, SessionLocator, SessionMatch, encode_project_dir};
pub use subagents::{
    SubagentInfo, SubagentParent, SubagentScanner, SubagentState, SubagentWatcher,
};
pub use usage::{SessionUsage, TokenCounts, Usage, UsageTracker, format_tokens};
pub use worker::{ClaudeData, ClaudeWorker, ClaudeWorkerConfig, TranscriptTail};
