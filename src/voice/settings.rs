//! Voice settings: the `voice` section of `settings.json`, edited in the Settings pane's Voice
//! section (`crate::ui::settings::voice`); "Speak replies" is also in the Queen section.
//!
//! Give every new field a serde default (the struct is `#[serde(default)]`) so older settings
//! files keep loading.

use serde::{Deserialize, Serialize};

use super::tts::DEFAULT_VOICE;

/// The default talk-to-the-Queen hotkey, in `global-hotkey`'s string form: ⌘⇧E.
pub const DEFAULT_TALK_HOTKEY: &str = "shift+super+KeyE";

/// Talk hotkeys offered in the Settings dropdown, as `(global-hotkey string, label)`.
pub const TALK_HOTKEYS: &[(&str, &str)] = &[
    (DEFAULT_TALK_HOTKEY, "⌘⇧E"),
    ("shift+super+Space", "⌘⇧Space"),
    ("alt+Space", "⌥Space"),
    ("control+alt+Space", "⌃⌥Space"),
    ("alt+Backquote", "⌥`"),
    ("control+Backquote", "⌃`"),
];

/// The show-window hotkey (⌘⇧M), which the talk hotkey must not take.
const SHOW_WINDOW_HOTKEY: &str = "shift+super+KeyM";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSettings {
    /// Hold to talk to the Queen from anywhere (`global-hotkey` string, e.g. `shift+super+KeyE`).
    pub talk_hotkey: String,
    /// Speak every reply of the Queen, not only replies to voice messages.
    pub speak_replies: bool,
    /// Kokoro voice name, e.g. `af_heart`.
    pub voice: String,
    /// Speech rate in percent (50–200).
    pub speed_percent: u16,
    /// Playback volume in percent (0–100).
    pub volume_percent: u8,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            talk_hotkey: DEFAULT_TALK_HOTKEY.to_owned(),
            speak_replies: false,
            voice: DEFAULT_VOICE.to_owned(),
            speed_percent: 100,
            volume_percent: 80,
        }
    }
}

impl VoiceSettings {
    /// Problems that make these settings unusable.
    pub fn validate(&self) -> Result<(), String> {
        let hotkey: global_hotkey::hotkey::HotKey = self
            .talk_hotkey
            .parse()
            .map_err(|e| format!("talk hotkey {:?}: {e}", self.talk_hotkey))?;
        if SHOW_WINDOW_HOTKEY
            .parse::<global_hotkey::hotkey::HotKey>()
            .is_ok_and(|show| show.id() == hotkey.id())
        {
            return Err("the talk hotkey cannot be ⌘⇧M (it shows the window)".into());
        }
        if !(50..=200).contains(&self.speed_percent) {
            return Err(format!("speed {}% is outside 50–200%", self.speed_percent));
        }
        if self.volume_percent > 100 {
            return Err(format!("volume {}% is over 100%", self.volume_percent));
        }
        if self.voice.trim().is_empty() {
            return Err("no voice chosen".into());
        }
        Ok(())
    }

    /// The talk hotkey, parsed.
    pub fn talk_hotkey(&self) -> Option<global_hotkey::hotkey::HotKey> {
        self.talk_hotkey.parse().ok()
    }

    /// The talk hotkey as the UI shows it, e.g. `⌘⇧E`.
    pub fn talk_hotkey_label(&self) -> String {
        hotkey_label(&self.talk_hotkey)
    }

    pub fn speed(&self) -> f32 {
        f32::from(self.speed_percent) / 100.0
    }

    pub fn volume(&self) -> f32 {
        f32::from(self.volume_percent) / 100.0
    }
}

/// A `global-hotkey` string as macOS writes shortcuts: `shift+super+KeyE` → `⌘⇧E`.
pub fn hotkey_label(hotkey: &str) -> String {
    if let Some((_, label)) = TALK_HOTKEYS.iter().find(|(s, _)| *s == hotkey) {
        return (*label).to_owned();
    }
    let mut mods = [false; 4];
    let mut key = String::new();
    for token in hotkey.split('+').map(str::trim) {
        match token.to_ascii_uppercase().as_str() {
            "CONTROL" | "CTRL" => mods[0] = true,
            "ALT" | "OPTION" => mods[1] = true,
            "SHIFT" => mods[2] = true,
            "SUPER" | "CMD" | "COMMAND" | "COMMANDORCONTROL" | "CMDORCTRL" => mods[3] = true,
            _ => key = key_label(token),
        }
    }
    let mut label: String = ["⌃", "⌥", "⇧", "⌘"]
        .iter()
        .zip(mods)
        .filter(|(_, on)| *on)
        .map(|(s, _)| *s)
        .collect();
    label.push_str(&key);
    label
}

fn key_label(code: &str) -> String {
    let named = match code {
        "Backquote" => Some("`"),
        "Space" => Some("Space"),
        "Semicolon" => Some(";"),
        "Quote" => Some("'"),
        "Comma" => Some(","),
        "Period" => Some("."),
        "Slash" => Some("/"),
        "Backslash" => Some("\\"),
        "Minus" => Some("-"),
        "Equal" => Some("="),
        _ => None,
    };
    if let Some(named) = named {
        return named.to_owned();
    }
    code.strip_prefix("Key")
        .or_else(|| code.strip_prefix("Digit"))
        .unwrap_or(code)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_round_trip() {
        let s = VoiceSettings::default();
        assert_eq!(s.validate(), Ok(()));
        assert!(!s.speak_replies, "speak replies is off by default");
        assert_eq!(s.voice, "af_heart");
        assert_eq!(s.talk_hotkey_label(), "⌘⇧E");
        let custom = VoiceSettings {
            talk_hotkey: "alt+Space".into(),
            speak_replies: true,
            voice: "bm_george".into(),
            speed_percent: 125,
            volume_percent: 40,
        };
        let json = serde_json::to_string(&custom).unwrap();
        assert_eq!(
            serde_json::from_str::<VoiceSettings>(&json).unwrap(),
            custom
        );
        assert!((custom.speed() - 1.25).abs() < 1e-6);
        assert!((custom.volume() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s: VoiceSettings = serde_json::from_str(r#"{"speak_replies": true}"#).unwrap();
        assert!(s.speak_replies);
        assert_eq!(s.talk_hotkey, DEFAULT_TALK_HOTKEY);
        assert_eq!(s.volume_percent, 80);
        let s: VoiceSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, VoiceSettings::default());
    }

    #[test]
    fn validation_catches_bad_values() {
        let bad = |f: fn(&mut VoiceSettings)| {
            let mut s = VoiceSettings::default();
            f(&mut s);
            s.validate().unwrap_err()
        };
        assert!(bad(|s| s.talk_hotkey = "nonsense+Key".into()).contains("talk hotkey"));
        assert!(bad(|s| s.talk_hotkey = "super+shift+KeyM".into()).contains("⌘⇧M"));
        assert!(bad(|s| s.speed_percent = 20).contains("speed"));
        assert!(bad(|s| s.volume_percent = 150).contains("volume"));
        assert!(bad(|s| s.voice = " ".into()).contains("voice"));
    }

    #[test]
    fn every_preset_parses_and_labels() {
        for (hotkey, label) in TALK_HOTKEYS {
            let s = VoiceSettings {
                talk_hotkey: (*hotkey).into(),
                ..VoiceSettings::default()
            };
            assert_eq!(s.validate(), Ok(()), "{hotkey}");
            assert_eq!(s.talk_hotkey_label(), *label);
        }
        assert_eq!(hotkey_label("control+shift+KeyT"), "⌃⇧T");
        assert_eq!(hotkey_label("super+Digit5"), "⌘5");
        assert_eq!(hotkey_label("F13"), "F13");
    }
}
