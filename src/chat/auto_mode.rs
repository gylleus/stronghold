//! The King's auto mode: while on, all his tool calls run without Karl's confirmation.
//! Kept in `<data dir>/chat/auto_mode.json`, next to `session.json`; off unless saved on.
//! Only Karl's toggle in the chat header changes it; the control API and the MCP server only
//! report it.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// File name inside the chat directory.
pub const AUTO_MODE_FILE: &str = "auto_mode.json";

/// Told to the King (with Karl's next message) when auto mode turns on.
pub const NOTE_ON: &str = "[Stronghold] Auto mode is now ON: your actions run without Karl's \
confirmation, so don't ask him to confirm them (tool texts saying \"confirm with Karl\" don't \
apply while it is on). Be precise; prefer dry runs for closes first.";
/// Told to the King when auto mode turns off.
pub const NOTE_OFF: &str = "[Stronghold] Auto mode is now OFF: your destructive Stronghold actions \
and other tool calls that need permission are shown to Karl for approval again before they run.";

/// Notice shown in the transcript when the mode changes.
pub fn notice(on: bool) -> &'static str {
    if on {
        "Auto mode on: the King's actions run without asking"
    } else {
        "Auto mode off: the King's actions ask again"
    }
}

/// The note that tells the King about the mode.
pub fn note(on: bool) -> &'static str {
    if on { NOTE_ON } else { NOTE_OFF }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    enabled: bool,
}

/// The saved mode in `dir`; off when missing or unreadable.
pub fn load(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(AUTO_MODE_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<Record>(&s).ok())
        .is_some_and(|r| r.enabled)
}

pub fn save(dir: &Path, enabled: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(AUTO_MODE_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&Record { enabled })?)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults_off() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("chat");
        assert!(!load(&chat), "missing file is off");
        save(&chat, true).unwrap();
        assert!(load(&chat));
        save(&chat, false).unwrap();
        assert!(!load(&chat));
        std::fs::write(chat.join(AUTO_MODE_FILE), "{nope").unwrap();
        assert!(!load(&chat), "unreadable file is off");
        std::fs::write(chat.join(AUTO_MODE_FILE), "{}").unwrap();
        assert!(!load(&chat));
    }

    #[test]
    fn notes_read_as_stronghold_system_notes() {
        assert!(note(true).starts_with("[Stronghold] Auto mode is now ON"));
        assert!(note(true).contains("dry runs"));
        assert!(note(false).starts_with("[Stronghold] Auto mode is now OFF"));
        assert_ne!(notice(true), notice(false));
    }
}
