//! Whip: detects stalled runs and nudges or alerts.

pub mod actions;
pub mod progress;
pub mod rules;
pub mod settings;

pub use progress::{ProgressReport, ProgressSource, ProgressTracker, RunProgress, TaskProgress};
pub use rules::{
    DEFAULT_THRESHOLD_MS, RuleContext, StallFinding, StallReason, StallTarget,
    effective_coordinator, evaluate,
};
pub use settings::{Loaded, SETTINGS_FILE, Settings, WhipSettings};
