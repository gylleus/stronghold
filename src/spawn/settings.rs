//! Spawn settings: the `spawn` section of `settings.json` (edited in the Settings pane's
//! General section) and the `ants` section (the Queen section): the default model and effort
//! of new ants.
//!
//! Give every new field a serde default (the struct is `#[serde(default)]`) so older settings
//! files keep loading.

use serde::{Deserialize, Serialize};

use super::Agent;
use super::pending::PendingSelect;
use crate::chat::settings::QueenSettings;

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

/// The `ants` section of `settings.json`: the model and effort every new Claude ant gets when
/// its spawn names none (the "New ant…" dialog, `spawn_session`, coordinators starting workers).
/// An explicit model on the request still wins; Codex ants keep Codex's own default. `None`
/// leaves the choice to the CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AntSettings {
    /// `--model`: an alias (`fable`) or a full id (`claude-fable-5-1`).
    pub model: Option<String>,
    /// `--effort`: one of [`crate::chat::settings::EFFORT_LEVELS`].
    pub effort: Option<String>,
}

impl AntSettings {
    /// Trims the values and drops empty ones.
    pub fn normalized(&self) -> Self {
        let q = self.as_queen().normalized();
        Self {
            model: q.model,
            effort: q.effort,
        }
    }

    /// Problems that make these settings unusable (same rules as the Queen's).
    pub fn validate(&self) -> Result<(), String> {
        self.as_queen().validate()
    }

    /// The same values as a [`QueenSettings`], which owns the normalization and validation.
    pub fn as_queen(&self) -> QueenSettings {
        QueenSettings {
            model: self.model.clone(),
            effort: self.effort.clone(),
        }
    }

    /// Fills a request's missing model and effort for a Claude ant. A request with its own
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
