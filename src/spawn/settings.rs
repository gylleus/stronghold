//! Spawn settings: the `spawn` section of `settings.json` (edited in the Settings pane's
//! General section) and the `dwarves` section (the King section): the default model and effort
//! of new dwarves.
//!
//! Give every new field a serde default (the struct is `#[serde(default)]`) so older settings
//! files keep loading.

use serde::{Deserialize, Serialize};

use super::Agent;
use super::pending::PendingSelect;
use crate::chat::settings::KingSettings;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpawnSettings {
    /// Select and pan to a new dwarf's node at the current zoom once it appears. Off: a spawn
    /// leaves the selection and the view alone.
    pub focus_new: bool,
}

impl SpawnSettings {
    /// What to select once a spawn started terminal `handle`: nothing unless `focus_new`.
    pub fn pending_select(&self, handle: &str, now_ms: i64) -> Option<PendingSelect> {
        self.focus_new.then(|| PendingSelect::new(handle, now_ms))
    }
}

/// The `dwarves` section of `settings.json`: the model and effort every new Claude dwarf gets when
/// its spawn names none (the "New dwarf…" dialog, `spawn_session`, coordinators starting workers).
/// An explicit model on the request still wins; Codex dwarves keep Codex's own default. `None`
/// leaves the choice to the CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DwarfSettings {
    /// `--model`: an alias (`fable`) or a full id (`claude-fable-5-1`).
    pub model: Option<String>,
    /// `--effort`: one of [`crate::chat::settings::EFFORT_LEVELS`].
    pub effort: Option<String>,
}

impl DwarfSettings {
    /// Trims the values and drops empty ones.
    pub fn normalized(&self) -> Self {
        let q = self.as_king().normalized();
        Self {
            model: q.model,
            effort: q.effort,
        }
    }

    /// Problems that make these settings unusable (same rules as the King's).
    pub fn validate(&self) -> Result<(), String> {
        self.as_king().validate()
    }

    /// The same values as a [`KingSettings`], which owns the normalization and validation.
    pub fn as_king(&self) -> KingSettings {
        KingSettings {
            model: self.model.clone(),
            effort: self.effort.clone(),
        }
    }

    /// Fills a request's missing model and effort for a Claude dwarf. A request with its own
    /// model keeps it and its own effort (the saved effort belongs to the saved model).
    pub fn fill(&self, agent: Agent, model: &mut Option<String>, effort: &mut Option<String>) {
        if agent != Agent::Claude || model.is_some() {
            return;
        }
        let saved = self.normalized();
        *model = saved.model;
        if effort.is_none() {
            *effort = saved.effort;
        }
    }

    /// The default as shown to people and agents: `fable (effort high)`, or `CLI default`.
    pub fn describe(&self) -> String {
        let s = self.normalized();
        match (s.model, s.effort) {
            (Some(m), Some(e)) => format!("{m} (effort {e})"),
            (Some(m), None) => m,
            (None, _) => "CLI default".into(),
        }
    }
}
