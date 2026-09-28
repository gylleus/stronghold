//! The app's settings file, and the whip's section of it: stall threshold, cooldown, max
//! nudges, dry-run.
//!
//! Stored as JSON in the user's config dir (`~/Library/Application Support/Myrstack/settings.json`
//! on macOS) with one top-level key per section (`whip`, `queen`, `voice`). Missing sections and
//! fields take their defaults, so older files load. A missing file gives the defaults; an
//! unreadable or invalid one gives the defaults plus a warning.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::chat::settings::QueenSettings;
use crate::voice::settings::VoiceSettings;
use crate::whip::rules::{DEFAULT_ABANDON_AFTER_MS, DEFAULT_THRESHOLD_MS};

const MIN: i64 = 60_000;
/// File name inside the config dir.
pub const SETTINGS_FILE: &str = "settings.json";
/// Environment variable overriding the settings file path, e.g. for test runs.
pub const SETTINGS_ENV: &str = "MYRSTACK_SETTINGS";

/// Everything persisted in `settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub whip: WhipSettings,
    /// The Queen's chat process: model and effort.
    pub queen: QueenSettings,
    pub voice: VoiceSettings,
}

/// Global whip configuration. Durations are in ms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhipSettings {
    /// Master switch: when off the whip evaluates nothing and sends nothing.
    pub enabled: bool,
    /// Log intended actions without sending anything. On until Karl turns it off.
    pub dry_run: bool,
    /// Silence longer than this is a stall.
    pub threshold_ms: i64,
    /// Minimum gap between two nudges to the same terminal.
    pub cooldown_ms: i64,
    /// Nudges per stall before escalating to a notification.
    pub max_nudges: u32,
    /// How often the whip loop runs.
    pub tick_ms: i64,
    /// Send notifications for escalations and long NeedsInput waits.
    pub notify: bool,
    /// A Run with no activity at all for longer than this is abandoned and never nudged.
    pub abandon_after_ms: i64,
}

impl Default for WhipSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            dry_run: true,
            threshold_ms: DEFAULT_THRESHOLD_MS,
            cooldown_ms: 10 * MIN,
            max_nudges: 2,
            tick_ms: MIN,
            notify: true,
            abandon_after_ms: DEFAULT_ABANDON_AFTER_MS,
        }
    }
}

impl WhipSettings {
    /// Problems that make these settings unusable, e.g. a non-positive duration.
    pub fn validate(&self) -> Result<(), String> {
        let positive = [
            ("threshold_ms", self.threshold_ms),
            ("cooldown_ms", self.cooldown_ms),
            ("tick_ms", self.tick_ms),
            ("abandon_after_ms", self.abandon_after_ms),
        ];
        let bad: Vec<String> = positive
            .iter()
            .filter(|(_, v)| *v <= 0)
            .map(|(k, v)| format!("{k} must be positive (got {v})"))
            .collect();
        if bad.is_empty() {
            Ok(())
        } else {
            Err(bad.join(", "))
        }
    }
}

/// Settings read from disk, plus a warning when the file was unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub settings: Settings,
    pub warning: Option<String>,
}

impl Settings {
    /// `<config dir>/Myrstack/settings.json`, when a home directory is known.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", crate::APP_NAME)
            .map(|dirs| dirs.config_dir().join(SETTINGS_FILE))
    }

    /// The settings file in use: `$MYRSTACK_SETTINGS` when set and non-empty, else
    /// [`Settings::default_path`].
    pub fn path() -> Option<PathBuf> {
        Self::path_with_override(std::env::var_os(SETTINGS_ENV))
    }

    fn path_with_override(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
        value
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(Self::default_path)
    }

    /// Reads `path`. Missing file → defaults, silently. Unreadable or invalid → defaults and a
    /// warning (also logged).
    pub fn load_from(path: &Path) -> Loaded {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Loaded {
                    settings: Self::default(),
                    warning: None,
                };
            }
            Err(e) => return Self::fallback(format!("cannot read {}: {e}", path.display())),
        };
        let settings: Settings = match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => return Self::fallback(format!("invalid {}: {e}", path.display())),
        };
        let checks = [
            ("whip", settings.whip.validate()),
            ("queen", settings.queen.validate()),
            ("voice", settings.voice.validate()),
        ];
        match checks
            .into_iter()
            .find_map(|(k, r)| r.err().map(|e| (k, e)))
        {
            None => Loaded {
                settings,
                warning: None,
            },
            Some((section, e)) => Self::fallback(format!(
                "invalid {section} settings in {}: {e}",
                path.display()
            )),
        }
    }

    /// Writes `path` atomically: a temp file in the same directory, then a rename. Creates the
    /// directory if needed.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(dir)?;
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let mut tmp = temp_file_beside(dir, path)?;
        let result = (|| {
            tmp.1.write_all(json.as_bytes())?;
            tmp.1.write_all(b"\n")?;
            tmp.1.sync_all()?;
            fs::rename(&tmp.0, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp.0);
        }
        result
    }

    fn fallback(warning: String) -> Loaded {
        log::warn!("{warning}; using default settings");
        Loaded {
            settings: Self::default(),
            warning: Some(warning),
        }
    }
}

/// Creates a uniquely named temp file next to `target`.
fn temp_file_beside(dir: &Path, target: &Path) -> io::Result<(PathBuf, fs::File)> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| SETTINGS_FILE.into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    Ok((tmp, file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("nested").join(SETTINGS_FILE)
    }

    #[test]
    fn defaults() {
        let s = WhipSettings::default();
        assert!(s.enabled);
        assert!(s.dry_run, "dry-run must default on");
        assert_eq!(s.threshold_ms, 15 * MIN);
        assert_eq!(s.cooldown_ms, 10 * MIN);
        assert_eq!(s.max_nudges, 2);
        assert_eq!(s.tick_ms, MIN);
        assert!(s.notify);
        assert_eq!(s.abandon_after_ms, 24 * 60 * MIN);
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn default_path_is_settings_json() {
        if let Some(p) = Settings::default_path() {
            assert!(p.ends_with(Path::new("Myrstack").join(SETTINGS_FILE)));
        }
    }

    #[test]
    fn env_override_wins_over_default_path() {
        let custom = Settings::path_with_override(Some("/tmp/x/settings.json".into()));
        assert_eq!(custom, Some(PathBuf::from("/tmp/x/settings.json")));
        assert_eq!(
            Settings::path_with_override(Some("".into())),
            Settings::default_path()
        );
        assert_eq!(Settings::path_with_override(None), Settings::default_path());
    }

    #[test]
    fn missing_file_gives_defaults_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Settings::load_from(&path_in(&dir));
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.warning, None);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        let mut s = Settings::default();
        s.whip.dry_run = false;
        s.whip.threshold_ms = 20 * MIN;
        s.whip.max_nudges = 3;
        s.whip.notify = false;
        s.save_to(&path).unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.settings, s);
        assert_eq!(loaded.warning, None);

        // Overwrite in place and leave no temp files behind.
        s.whip.enabled = false;
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).settings, s);
        let files: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files, vec![std::ffi::OsString::from(SETTINGS_FILE)]);
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);
        fs::write(
            &path,
            r#"{"whip": {"dry_run": false, "cooldown_ms": 300000}, "future": 1}"#,
        )
        .unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.warning, None);
        assert_eq!(
            loaded.settings.whip,
            WhipSettings {
                dry_run: false,
                cooldown_ms: 5 * MIN,
                ..Default::default()
            }
        );
    }

    #[test]
    fn queen_and_voice_sections_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        let mut s = Settings::default();
        s.queen.model = Some("opus".into());
        s.queen.effort = Some("high".into());
        s.whip.dry_run = false;
        s.save_to(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["queen"]["model"], "opus");
        assert_eq!(json["queen"]["effort"], "high");
        assert!(json["voice"].is_object());
        assert_eq!(Settings::load_from(&path).settings, s);
    }

    #[test]
    fn old_file_without_queen_or_voice_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);
        // A file as written before the queen and voice sections existed.
        fs::write(
            &path,
            r#"{"whip": {"enabled": true, "dry_run": false, "threshold_ms": 900000,
                "cooldown_ms": 600000, "max_nudges": 2, "tick_ms": 60000, "notify": true,
                "abandon_after_ms": 86400000}}"#,
        )
        .unwrap();
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.warning, None);
        assert!(!loaded.settings.whip.dry_run);
        assert_eq!(loaded.settings.queen, QueenSettings::default());
        assert_eq!(loaded.settings.voice, VoiceSettings::default());

        // A queen section with only some fields.
        fs::write(&path, r#"{"queen": {"model": "sonnet"}}"#).unwrap();
        let loaded = Settings::load_from(&path).settings;
        assert_eq!(loaded.queen.model.as_deref(), Some("sonnet"));
        assert_eq!(loaded.queen.effort, None);
        assert_eq!(loaded.whip, WhipSettings::default());
    }

    #[test]
    fn invalid_file_gives_defaults_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);
        for bad in [
            "not json",
            r#"{"whip": {"max_nudges": "two"}}"#,
            r#"{"whip": {"threshold_ms": 0}}"#,
            r#"{"whip": {"tick_ms": -5}}"#,
            r#"{"whip": {"abandon_after_ms": 0}}"#,
            r#"{"queen": {"effort": "turbo"}}"#,
            r#"{"queen": {"model": 5}}"#,
        ] {
            fs::write(&path, bad).unwrap();
            let loaded = Settings::load_from(&path);
            assert_eq!(loaded.settings, Settings::default(), "{bad}");
            assert!(loaded.warning.is_some(), "{bad}");
        }
    }
}
