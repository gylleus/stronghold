//! The King's saved model settings (the `king` section of `settings.json`) and how they
//! combine with the `MYRSTACK_CHAT_MODEL` override.

use serde::{Deserialize, Serialize};

use super::config::MODEL_ENV;

/// Model aliases the `claude` CLI accepts for `--model` (each names the latest of its family).
pub const MODEL_ALIASES: &[&str] = &["fable", "opus", "sonnet", "haiku"];

/// Full model ids offered besides the aliases.
pub const MODEL_IDS: &[&str] = &[
    "claude-fable-5-1",
    "claude-fable-5",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5",
];

/// Levels the CLI accepts for `--effort`.
pub const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// The King's `claude` process options. `None` leaves the choice to the CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KingSettings {
    /// `--model`: an alias (`opus`) or a full id (`claude-opus-5-5`).
    pub model: Option<String>,
    /// `--effort`: one of [`EFFORT_LEVELS`].
    pub effort: Option<String>,
}

impl KingSettings {
    /// Trims the values and drops empty ones.
    pub fn normalized(&self) -> Self {
        let clean = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        Self {
            model: clean(&self.model),
            effort: clean(&self.effort),
        }
    }

    /// Problems that make these settings unusable.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(model) = &self.model
            && model.chars().any(char::is_whitespace)
        {
            return Err(format!("model {model:?} contains whitespace"));
        }
        if let Some(effort) = &self.effort
            && !EFFORT_LEVELS.contains(&effort.as_str())
        {
            return Err(format!(
                "effort must be one of {} (got {effort:?})",
                EFFORT_LEVELS.join(", ")
            ));
        }
        Ok(())
    }
}

/// Where the King's model comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// `MYRSTACK_CHAT_MODEL` is set and wins over the settings.
    Env(String),
    Settings(String),
    /// Neither: the CLI picks its default.
    CliDefault,
}

impl ModelSource {
    /// The `--model` value, if any.
    pub fn model(&self) -> Option<&str> {
        match self {
            ModelSource::Env(m) | ModelSource::Settings(m) => Some(m),
            ModelSource::CliDefault => None,
        }
    }
}

/// The model the King runs with: `env` (the value of `MYRSTACK_CHAT_MODEL`) when non-empty,
/// else the settings' model, else the CLI default.
pub fn resolve_model(settings: &KingSettings, env: Option<String>) -> ModelSource {
    if let Some(env) = env.map(|e| e.trim().to_owned()).filter(|e| !e.is_empty()) {
        return ModelSource::Env(env);
    }
    match settings.normalized().model {
        Some(m) => ModelSource::Settings(m),
        None => ModelSource::CliDefault,
    }
}

/// [`resolve_model`] with this process's `MYRSTACK_CHAT_MODEL`.
pub fn resolve_model_from_env(settings: &KingSettings) -> ModelSource {
    resolve_model(settings, std::env::var(MODEL_ENV).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins_over_settings() {
        let saved = KingSettings {
            model: Some("opus".into()),
            effort: None,
        };
        assert_eq!(
            resolve_model(&saved, Some("sonnet".into())),
            ModelSource::Env("sonnet".into())
        );
        assert_eq!(
            resolve_model(&saved, Some("  ".into())),
            ModelSource::Settings("opus".into()),
            "an empty variable is no override"
        );
        assert_eq!(
            resolve_model(&saved, None),
            ModelSource::Settings("opus".into())
        );
        assert_eq!(
            resolve_model(&KingSettings::default(), None),
            ModelSource::CliDefault
        );
        assert_eq!(
            resolve_model(&KingSettings::default(), Some("haiku".into())).model(),
            Some("haiku")
        );
    }

    #[test]
    fn normalizes_and_validates() {
        let s = KingSettings {
            model: Some("  opus ".into()),
            effort: Some("".into()),
        };
        assert_eq!(
            s.normalized(),
            KingSettings {
                model: Some("opus".into()),
                effort: None
            }
        );
        assert!(s.normalized().validate().is_ok());
        let bad_effort = KingSettings {
            model: None,
            effort: Some("turbo".into()),
        };
        assert!(bad_effort.validate().is_err());
        let bad_model = KingSettings {
            model: Some("claude opus".into()),
            effort: None,
        };
        assert!(bad_model.validate().is_err());
        for level in EFFORT_LEVELS {
            let s = KingSettings {
                model: None,
                effort: Some((*level).into()),
            };
            assert!(s.validate().is_ok(), "{level}");
        }
    }
}
