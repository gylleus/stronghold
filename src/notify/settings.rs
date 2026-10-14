//! The `notifications` section of `settings.json`: how a notification reaches the user (a macOS
//! pop-up, an in-app pop-up, a sound), from which severity on, and quiet mode. Edited in the
//! Settings window's Notifications section (`crate::ui::settings::notifications`).
//!
//! Give every new field a serde default (the struct is `#[serde(default)]`) so older settings
//! files keep loading.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where macOS keeps its alert sounds.
pub const SYSTEM_SOUNDS_DIR: &str = "/System/Library/Sounds";
/// The sounds macOS ships in [`SYSTEM_SOUNDS_DIR`], listed when the directory can't be read.
pub const KNOWN_SYSTEM_SOUNDS: &[&str] = &[
    "Basso",
    "Blow",
    "Bottle",
    "Frog",
    "Funk",
    "Glass",
    "Hero",
    "Morse",
    "Ping",
    "Pop",
    "Purr",
    "Sosumi",
    "Submarine",
    "Tink",
];

/// How much a notification matters. Ordered: `Info < Warn < Urgent`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing: something finished, a PR is ready.
    #[default]
    Info,
    /// Needs the user soon: an agent waits on input or a decision.
    Warn,
    /// Needs the user now; gets through quiet mode.
    Urgent,
}

impl Severity {
    pub const ALL: [Severity; 3] = [Severity::Info, Severity::Warn, Severity::Urgent];

    /// The API name: `info`, `warn`, `urgent`.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Urgent => "urgent",
        }
    }

    /// A severity from its name; `warning`, `critical` and `high` are accepted as well.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "info" | "low" => Some(Severity::Info),
            "warn" | "warning" | "medium" => Some(Severity::Warn),
            "urgent" | "critical" | "high" => Some(Severity::Urgent),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Severity::Info => "Info",
            Severity::Warn => "Warn",
            Severity::Urgent => "Urgent",
        }
    }
}

/// A sound to play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    None,
    /// A macOS alert sound by name, e.g. `Glass` (`/System/Library/Sounds/Glass.aiff`).
    System(String),
    /// An audio file `afplay` can play (aiff, wav, mp3, m4a, caf).
    File(PathBuf),
}

impl Sound {
    /// The file to play; `None` for [`Sound::None`].
    pub fn path(&self) -> Option<PathBuf> {
        match self {
            Sound::None => None,
            Sound::System(name) => Some(Path::new(SYSTEM_SOUNDS_DIR).join(format!("{name}.aiff"))),
            Sound::File(path) => Some(path.clone()),
        }
    }

    pub fn is_none(&self) -> bool {
        *self == Sound::None
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Sound::None => Ok(()),
            Sound::System(name) if name.trim().is_empty() || name.contains('/') => {
                Err(format!("{name:?} is not a system sound name"))
            }
            Sound::File(path) if path.as_os_str().is_empty() => Err("no sound file chosen".into()),
            _ => Ok(()),
        }
    }
}

/// One sound per severity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeveritySounds {
    pub info: Sound,
    pub warn: Sound,
    pub urgent: Sound,
}

impl Default for SeveritySounds {
    fn default() -> Self {
        Self {
            info: Sound::System("Tink".into()),
            warn: Sound::System("Glass".into()),
            urgent: Sound::System("Sosumi".into()),
        }
    }
}

impl SeveritySounds {
    pub fn get(&self, severity: Severity) -> &Sound {
        match severity {
            Severity::Info => &self.info,
            Severity::Warn => &self.warn,
            Severity::Urgent => &self.urgent,
        }
    }

    pub fn get_mut(&mut self, severity: Severity) -> &mut Sound {
        match severity {
            Severity::Info => &mut self.info,
            Severity::Warn => &mut self.warn,
            Severity::Urgent => &mut self.urgent,
        }
    }
}

/// How notifications are delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifySettings {
    /// macOS notification banners.
    pub native_popups: bool,
    /// Pop-ups in Stronghold's window (top right), also when macOS notifications are off.
    pub in_app_popups: bool,
    /// Lowest severity that pops up (natively and in the app).
    pub popup_min: Severity,
    /// Lowest severity that plays its sound.
    pub sound_min: Severity,
    pub sounds: SeveritySounds,
    /// Sound volume in percent (0–100).
    pub volume_percent: u8,
    /// Do not disturb: only urgent notifications pop up or play a sound. Everything is still
    /// recorded in the inbox.
    pub quiet: bool,
    /// A soft sound with the pop of an agent that is done or starts waiting for input.
    pub pop_sounds: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            native_popups: true,
            in_app_popups: true,
            popup_min: Severity::Info,
            sound_min: Severity::Info,
            sounds: SeveritySounds::default(),
            volume_percent: 70,
            quiet: false,
            pop_sounds: false,
        }
    }
}

/// How one notification is delivered, from [`NotifySettings::delivery`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Delivery {
    pub native: bool,
    pub in_app: bool,
    /// The sound to play, if any.
    pub sound: Option<Sound>,
}

impl Delivery {
    /// Neither pops up nor plays anything.
    pub fn is_silent(&self) -> bool {
        !self.native && !self.in_app && self.sound.is_none()
    }
}

impl NotifySettings {
    /// Problems that make these settings unusable.
    pub fn validate(&self) -> Result<(), String> {
        if self.volume_percent > 100 {
            return Err(format!("volume {}% is over 100%", self.volume_percent));
        }
        for severity in Severity::ALL {
            self.sounds
                .get(severity)
                .validate()
                .map_err(|e| format!("{} sound: {e}", severity.name()))?;
        }
        Ok(())
    }

    /// Whether quiet mode holds back a notification of `severity`.
    pub fn quiets(&self, severity: Severity) -> bool {
        self.quiet && severity < Severity::Urgent
    }

    /// How a notification of `severity` is delivered under these settings.
    pub fn delivery(&self, severity: Severity) -> Delivery {
        if self.quiets(severity) {
            return Delivery::default();
        }
        let pops = severity >= self.popup_min;
        let sound = self.sounds.get(severity);
        Delivery {
            native: pops && self.native_popups,
            in_app: pops && self.in_app_popups,
            sound: (severity >= self.sound_min && !sound.is_none()).then(|| sound.clone()),
        }
    }

    /// `afplay`'s volume for [`NotifySettings::volume_percent`].
    pub fn volume(&self) -> f32 {
        f32::from(self.volume_percent.min(100)) / 100.0
    }
}

/// The alert sounds in [`SYSTEM_SOUNDS_DIR`] by name, sorted; the known list when the
/// directory can't be read.
pub fn system_sounds() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(SYSTEM_SOUNDS_DIR)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let is_aiff = path.extension().is_some_and(|x| x == "aiff");
            is_aiff.then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    if names.is_empty() {
        names = KNOWN_SYSTEM_SOUNDS
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
    }
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severities_order_and_parse() {
        assert!(Severity::Info < Severity::Warn && Severity::Warn < Severity::Urgent);
        for s in Severity::ALL {
            assert_eq!(Severity::parse(s.name()), Some(s));
        }
        assert_eq!(Severity::parse(" Warning "), Some(Severity::Warn));
        assert_eq!(Severity::parse("critical"), Some(Severity::Urgent));
        assert_eq!(Severity::parse("loud"), None);
    }

    #[test]
    fn delivery_follows_the_thresholds() {
        let s = NotifySettings {
            popup_min: Severity::Warn,
            sound_min: Severity::Urgent,
            ..NotifySettings::default()
        };
        assert!(s.delivery(Severity::Info).is_silent());
        let warn = s.delivery(Severity::Warn);
        assert!(warn.native && warn.in_app && warn.sound.is_none());
        let urgent = s.delivery(Severity::Urgent);
        assert_eq!(urgent.sound, Some(Sound::System("Sosumi".into())));
    }

    #[test]
    fn channels_switch_off_separately() {
        let mut s = NotifySettings {
            native_popups: false,
            ..NotifySettings::default()
        };
        let d = s.delivery(Severity::Info);
        assert!(!d.native && d.in_app && d.sound.is_some());
        s.sounds.info = Sound::None;
        assert_eq!(
            s.delivery(Severity::Info).sound,
            None,
            "a severity can be silent"
        );
        assert!(s.delivery(Severity::Warn).sound.is_some());
    }

    #[test]
    fn quiet_mode_lets_only_urgent_through() {
        let s = NotifySettings {
            quiet: true,
            ..NotifySettings::default()
        };
        assert!(s.delivery(Severity::Info).is_silent());
        assert!(s.delivery(Severity::Warn).is_silent());
        let urgent = s.delivery(Severity::Urgent);
        assert!(urgent.native && urgent.in_app && urgent.sound.is_some());
    }

    #[test]
    fn sounds_resolve_to_files_and_validate() {
        assert_eq!(
            Sound::System("Glass".into()).path().unwrap(),
            Path::new("/System/Library/Sounds/Glass.aiff")
        );
        assert_eq!(Sound::None.path(), None);
        let mut s = NotifySettings::default();
        assert!(s.validate().is_ok());
        s.sounds.warn = Sound::File(PathBuf::new());
        assert!(s.validate().unwrap_err().starts_with("warn sound"));
        s.sounds.warn = Sound::System("../x".into());
        assert!(s.validate().is_err());
        s.sounds.warn = Sound::None;
        s.volume_percent = 101;
        assert!(s.validate().is_err());
    }

    #[test]
    fn settings_round_trip_and_old_files_load() {
        let s = NotifySettings {
            sounds: SeveritySounds {
                info: Sound::None,
                warn: Sound::File("/tmp/ding.wav".into()),
                urgent: Sound::System("Hero".into()),
            },
            quiet: true,
            ..NotifySettings::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["sounds"]["info"], "none");
        assert_eq!(json["sounds"]["warn"]["file"], "/tmp/ding.wav");
        assert_eq!(json["popup_min"], "info");
        let back: NotifySettings = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
        let old: NotifySettings = serde_json::from_str(r#"{"quiet": true}"#).unwrap();
        assert_eq!(old.sounds, SeveritySounds::default());
        assert!(old.quiet);
    }
}
