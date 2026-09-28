//! Voice settings: the `voice` section of `settings.json`, edited in the Settings pane's Voice
//! section (`crate::ui::settings::voice`).
//!
//! Empty for now; dictation and spoken-reply options go here. Give every new field a serde
//! default (the struct is `#[serde(default)]`) so older settings files keep loading.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSettings {}

impl VoiceSettings {
    /// Problems that make these settings unusable.
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}
