//! The human who runs the clan: the name prompts, tool texts, the playbook and the UI call them
//! by. Texts say `{user}` where the name goes; [`fill`] puts it in.

use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// Where the name goes in prompts, tool texts and the playbook.
pub const PLACEHOLDER: &str = "{user}";
/// The name when neither the settings nor the account give one.
pub const FALLBACK: &str = "the user";

/// The `user` section of `settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    /// What agents and the King call the user. Unset or blank: the first word of the macOS
    /// account's full name, else the login name.
    pub name: Option<String>,
}

static NAME: RwLock<Option<String>> = RwLock::new(None);

/// The user's name: as last [`set`], else from the settings file and the account.
pub fn name() -> String {
    if let Some(name) = NAME.read().ok().and_then(|n| n.clone()) {
        return name;
    }
    let name = initial();
    if let Ok(mut slot) = NAME.write() {
        slot.get_or_insert_with(|| name.clone());
    }
    name
}

/// Makes `settings` the source of [`name`], e.g. after Settings → General saved it.
pub fn set(settings: &UserSettings) {
    let name = resolve(settings.name.as_deref(), account_full_name, account_login);
    if let Ok(mut slot) = NAME.write() {
        *slot = Some(name);
    }
}

/// The user's name and login, lowercased: words that mark the user's own branches and labels
/// (`karl/fix-cart`) rather than what a thread is about.
pub fn own_words() -> Vec<String> {
    let mut words = vec![name().to_lowercase()];
    words.extend(account_login().map(|l| l.to_lowercase()));
    words.dedup();
    words
}

/// `text` with every `{user}` replaced by [`name`].
pub fn fill(text: &str) -> String {
    if text.contains(PLACEHOLDER) {
        text.replace(PLACEHOLDER, &name())
    } else {
        text.to_owned()
    }
}

/// [`fill`] over every string in a JSON value, in place.
pub fn fill_value(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::String(s) if s.contains(PLACEHOLDER) => *s = fill(s),
        Value::Array(items) => items.iter_mut().for_each(fill_value),
        Value::Object(map) => map.values_mut().for_each(fill_value),
        _ => {}
    }
}

/// Tests get a fixed name, never the machine's settings or account.
#[cfg(test)]
fn initial() -> String {
    "Karl".into()
}

#[cfg(not(test))]
fn initial() -> String {
    let configured = crate::whip::Settings::path()
        .map(|p| crate::whip::Settings::load_from(&p).settings.user.name)
        .unwrap_or_default();
    resolve(configured.as_deref(), account_full_name, account_login)
}

/// The configured name, else the first word of the account's full name, else the login, else
/// [`FALLBACK`]. The account lookups only run when needed.
fn resolve(
    configured: Option<&str>,
    full_name: impl FnOnce() -> Option<String>,
    login: impl FnOnce() -> Option<String>,
) -> String {
    let word = |s: &str| s.split_whitespace().next().map(str::to_owned);
    configured
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .or_else(|| full_name().as_deref().and_then(word))
        .or_else(|| login().as_deref().and_then(word))
        .unwrap_or_else(|| FALLBACK.into())
}

/// The account's full name from `id -F` (macOS).
fn account_full_name() -> Option<String> {
    if cfg!(test) {
        return None;
    }
    let out = std::process::Command::new("id").arg("-F").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|n| !n.is_empty())
}

fn account_login() -> Option<String> {
    if cfg!(test) {
        return None;
    }
    std::env::var("USER").ok().filter(|u| !u.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Option<String> {
        None
    }

    #[test]
    fn the_setting_wins_then_the_full_name_then_the_login() {
        let full = || Some("Ada Lovelace".to_owned());
        let login = || Some("ada".to_owned());
        assert_eq!(resolve(Some(" Countess "), full, login), "Countess");
        assert_eq!(resolve(Some("  "), full, login), "Ada");
        assert_eq!(resolve(None, none, login), "ada");
        assert_eq!(resolve(None, none, none), FALLBACK);
    }

    #[test]
    fn the_account_is_not_asked_when_the_setting_names_someone() {
        let ask = || -> Option<String> { panic!("asked the account") };
        assert_eq!(resolve(Some("Bo"), ask, ask), "Bo");
    }

    #[test]
    fn tests_see_a_fixed_name_and_fill_replaces_every_placeholder() {
        assert_eq!(name(), "Karl");
        assert_eq!(fill("ask {user}; {user}'s call"), "ask Karl; Karl's call");
        let mut v = serde_json::json!({"a": ["to {user}", 1], "b": {"c": "{user}"}});
        fill_value(&mut v);
        assert_eq!(
            v,
            serde_json::json!({"a": ["to Karl", 1], "b": {"c": "Karl"}})
        );
    }
}
