//! The chat's current Claude session id, kept in `<data dir>/chat/session.json` so a restart
//! of the process or the app resumes the same conversation.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// File name inside the chat directory.
pub const SESSION_FILE: &str = "session.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    /// A message was sent, so the CLI has the session on disk and it must be resumed
    /// (`--resume`) rather than created (`--session-id`).
    #[serde(default)]
    pub started: bool,
}

impl SessionRecord {
    pub fn fresh() -> Self {
        Self {
            session_id: new_uuid(),
            started: false,
        }
    }

    /// CLI arguments that open this session.
    pub fn cli_args(&self) -> [String; 2] {
        let flag = if self.started {
            "--resume"
        } else {
            "--session-id"
        };
        [flag.to_owned(), self.session_id.clone()]
    }
}

/// Loads and saves the [`SessionRecord`].
#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
    pub record: SessionRecord,
}

impl SessionStore {
    /// The saved session in `dir`, or a fresh one (saved) when there is none or it is
    /// unreadable.
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(SESSION_FILE);
        let saved = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<SessionRecord>(&s).ok())
            .filter(|r| is_uuid(&r.session_id));
        let fresh = saved.is_none();
        let mut store = Self {
            path,
            record: saved.unwrap_or_else(SessionRecord::fresh),
        };
        if fresh {
            store.save();
        }
        store
    }

    pub fn save(&mut self) {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&self.record)?)?;
            std::fs::rename(&tmp, &self.path)
        };
        if let Err(e) = write() {
            log::warn!("chat: cannot save {}: {e}", self.path.display());
        }
    }

    pub fn mark_started(&mut self) {
        if !self.record.started {
            self.record.started = true;
            self.save();
        }
    }

    /// Whether the session must be created next time (the CLI does not know it).
    pub fn mark_unstarted(&mut self) {
        if self.record.started {
            self.record.started = false;
            self.save();
        }
    }

    /// Starts a new conversation.
    pub fn new_session(&mut self) {
        self.record = SessionRecord::fresh();
        self.save();
    }
}

/// A random version-4 UUID (from std's randomly seeded hasher, the clock and the pid; good
/// enough for a session id).
pub fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_u32(std::process::id());
        h.write_usize(i);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// `8-4-4-4-12` lowercase or uppercase hex.
pub fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, len)| g.len() == len && g.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_v4_and_distinct() {
        let a = new_uuid();
        let b = new_uuid();
        assert!(is_uuid(&a), "{a}");
        assert_ne!(a, b);
        assert_eq!(&a[14..15], "4");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
        assert!(!is_uuid("not-a-uuid"));
    }

    #[test]
    fn store_persists_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::open(dir.path());
        let id = store.record.session_id.clone();
        assert_eq!(
            store.record.cli_args(),
            ["--session-id".to_owned(), id.clone()]
        );

        store.mark_started();
        let reopened = SessionStore::open(dir.path());
        assert_eq!(reopened.record.session_id, id);
        assert_eq!(
            reopened.record.cli_args(),
            ["--resume".to_owned(), id.clone()]
        );

        let mut store = reopened;
        store.new_session();
        assert_ne!(store.record.session_id, id);
        assert!(!store.record.started);
        assert_eq!(
            SessionStore::open(dir.path()).record.session_id,
            store.record.session_id
        );
    }

    #[test]
    fn unreadable_record_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SESSION_FILE), "{nope").unwrap();
        let store = SessionStore::open(dir.path());
        assert!(is_uuid(&store.record.session_id));
        assert!(!store.record.started);
    }
}
