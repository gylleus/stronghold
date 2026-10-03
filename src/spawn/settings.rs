//! Spawn settings: the `spawn` section of `settings.json`, edited in the Settings pane's
//! General section.
//!
//! Give every new field a serde default (the struct is `#[serde(default)]`) so older settings
//! files keep loading.

use serde::{Deserialize, Serialize};

use super::pending::PendingSelect;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpawnSettings {
    /// Select and zoom onto a new ant's node once it appears. Off: a spawn leaves the selection
    /// and the view alone.
    pub focus_new: bool,
}

impl SpawnSettings {
    /// What to select once a spawn started terminal `handle`: nothing unless `focus_new`.
    pub fn pending_select(&self, handle: &str, now_ms: i64) -> Option<PendingSelect> {
        self.focus_new.then(|| PendingSelect::new(handle, now_ms))
    }
}
