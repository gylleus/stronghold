//! Whip: detects stalled runs and nudges or alerts.

pub mod actions;
pub mod engine;
pub mod progress;
pub mod rules;
pub mod settings;

pub use actions::{
    DryRunExecutor, NUDGE_PREFIX, NudgeVia, OrcaExecutor, WhipAction, WhipExecutor, WhipLogEntry,
};
pub use engine::{DEFAULT_LOG_CAPACITY, TargetState, WhipEngine};
pub use progress::{ProgressReport, ProgressSource, ProgressTracker, RunProgress, TaskProgress};
pub use rules::{
    DEFAULT_THRESHOLD_MS, RuleContext, StallFinding, StallReason, StallTarget,
    effective_coordinator, evaluate,
};
pub use settings::{Loaded, SETTINGS_FILE, Settings, WhipSettings};
