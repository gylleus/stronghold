//! `orca account list`: the provider accounts and their plan rate limits.
//!
//! Only `result.rateLimits` is decoded. It maps a provider key (`claude`, `codex`, `gemini`, …)
//! to an object with a `status`, an optional `error` and zero or more usage windows
//! (`session`, `weekly`, `fableWeekly`, `monthly`, …), each `{usedPercent, windowMinutes,
//! resetsAt, resetDescription}` or `null`. Account emails and ids are never read.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::client::{OrcaClient, OrcaError, parse_result};

/// `result` of `orca account list`, reduced to the rate limits.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountList {
    /// Provider key → provider object; also holds a few flags (`grokAuthConfigured`, …).
    pub rate_limits: BTreeMap<String, Value>,
}

/// One provider's entry in `rateLimits`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRateLimit {
    /// The key in `rateLimits`, e.g. `claude` or `opencodeGo`.
    pub key: String,
    /// `status`, e.g. `ok` or `unavailable`.
    pub status: Option<String>,
    pub error: Option<String>,
    pub updated_at_ms: Option<i64>,
    /// Non-null windows, in the order `session`, `weekly`, `fableWeekly`, `monthly`, then others.
    pub windows: Vec<RateWindow>,
}

/// One usage window, e.g. Claude's 5-hour session.
#[derive(Debug, Clone, PartialEq)]
pub struct RateWindow {
    /// The field name, e.g. `session` or `fableWeekly`.
    pub key: String,
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub resets_at_ms: Option<i64>,
    /// Orca's own wording, e.g. `Tue 5:00 AM`.
    pub reset_description: Option<String>,
}

const WINDOW_ORDER: &[&str] = &["session", "weekly", "fableWeekly", "monthly"];

impl AccountList {
    /// Every provider object in `rateLimits`, skipping flags and malformed entries.
    pub fn providers(&self) -> Vec<ProviderRateLimit> {
        self.rate_limits
            .iter()
            .filter_map(|(key, value)| provider(key, value))
            .collect()
    }
}

fn provider(key: &str, value: &Value) -> Option<ProviderRateLimit> {
    let object = value.as_object()?;
    // Provider objects carry `provider` or `status`; flags and targets carry neither.
    if !object.contains_key("provider") && !object.contains_key("status") {
        return None;
    }
    let mut windows: Vec<RateWindow> = object
        .iter()
        .filter_map(|(name, v)| window(name, v))
        .collect();
    windows.sort_by_key(|w| {
        let rank = WINDOW_ORDER
            .iter()
            .position(|k| *k == w.key)
            .unwrap_or(WINDOW_ORDER.len());
        (rank, w.key.clone())
    });
    Some(ProviderRateLimit {
        key: key.to_string(),
        status: object.get("status").and_then(Value::as_str).map(Into::into),
        error: object.get("error").and_then(Value::as_str).map(Into::into),
        updated_at_ms: object.get("updatedAt").and_then(as_i64),
        windows,
    })
}

fn window(key: &str, value: &Value) -> Option<RateWindow> {
    let object = value.as_object()?;
    let used_percent = object.get("usedPercent")?.as_f64()?;
    Some(RateWindow {
        key: key.to_string(),
        used_percent,
        window_minutes: object
            .get("windowMinutes")
            .and_then(as_i64)
            .and_then(|m| u64::try_from(m).ok()),
        resets_at_ms: object.get("resetsAt").and_then(as_i64),
        reset_description: object
            .get("resetDescription")
            .and_then(Value::as_str)
            .map(Into::into),
    })
}

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f.round() as i64))
}

pub fn account_list_args() -> Vec<String> {
    vec!["account".into(), "list".into()]
}

pub fn parse_account_list(stdout: &[u8]) -> Result<AccountList, OrcaError> {
    parse_result(stdout)
}

impl OrcaClient {
    /// `orca account list`.
    pub fn account_list(&self) -> Result<AccountList, OrcaError> {
        self.call(&account_list_args())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/orca/account_list.json");

    #[test]
    fn account_list_fixture() {
        let list = parse_account_list(FIXTURE).unwrap();
        let providers = list.providers();
        let keys: Vec<&str> = providers.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "antigravity",
                "claude",
                "codex",
                "gemini",
                "grok",
                "kimi",
                "minimax",
                "opencodeGo"
            ]
        );
        let claude = &providers[1];
        assert_eq!(claude.status.as_deref(), Some("ok"));
        assert_eq!(claude.error, None);
        let windows: Vec<(&str, f64, Option<u64>)> = claude
            .windows
            .iter()
            .map(|w| (w.key.as_str(), w.used_percent, w.window_minutes))
            .collect();
        assert_eq!(
            windows,
            [
                ("session", 8.0, Some(300)),
                ("weekly", 17.0, Some(10080)),
                ("fableWeekly", 91.0, Some(10080))
            ]
        );
        assert_eq!(claude.windows[0].resets_at_ms, Some(1_790_353_800_000));
        assert_eq!(
            claude.windows[0].reset_description.as_deref(),
            Some("6:30 PM")
        );

        let codex = &providers[2];
        assert_eq!(codex.windows.len(), 1, "session is null");
        assert_eq!(codex.windows[0].key, "weekly");

        let gemini = &providers[3];
        assert_eq!(gemini.status.as_deref(), Some("unavailable"));
        assert!(gemini.windows.is_empty());
        assert!(gemini.error.as_deref().unwrap().contains("Gemini"));
    }

    #[test]
    fn partial_and_odd_fields_are_tolerated() {
        let list: AccountList = serde_json::from_value(json!({"rateLimits": {
            "claude": {"provider": "claude", "session": {"usedPercent": 12.5},
                "weekly": {"windowMinutes": 10080}, "monthly": "n/a",
                "custom": {"usedPercent": 3, "resetsAt": 1.5e12}},
            "flag": true,
            "target": {"runtime": "host"},
        }}))
        .unwrap();
        let providers = list.providers();
        assert_eq!(providers.len(), 1);
        let p = &providers[0];
        assert_eq!(p.status, None);
        let keys: Vec<&str> = p.windows.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, ["session", "custom"]);
        assert_eq!(p.windows[0].window_minutes, None);
        assert_eq!(p.windows[1].resets_at_ms, Some(1_500_000_000_000));

        let empty: AccountList = serde_json::from_value(json!({})).unwrap();
        assert!(empty.providers().is_empty());
    }
}
