//! The notification inbox: every notification the dispatcher accepted, newest last, with
//! read/unread state. Saved to `<data dir>/notifications.json` so it survives restarts.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Source;
use super::settings::Severity;

/// File the inbox is saved to, in the data dir.
pub const HISTORY_FILE: &str = "notifications.json";
/// Most entries kept; the oldest go first.
pub const MAX_ENTRIES: usize = 200;

/// One notification in the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    /// When it last arrived (a collapsed duplicate moves it).
    pub at_ms: i64,
    pub first_at_ms: i64,
    pub source: Source,
    pub severity: Severity,
    pub title: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What a click selects: a node id, a terminal handle or a PR URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Who raised it, for rate limits and duplicates (an agent's terminal handle or pane key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    /// The agent's name when it was raised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// How many identical notifications were collapsed into this one.
    #[serde(default = "one")]
    pub count: u32,
    #[serde(default)]
    pub read: bool,
}

fn one() -> u32 {
    1
}

/// The inbox, oldest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct History {
    entries: Vec<Entry>,
    next_id: u64,
    /// Bumped on every change, so views know when to rebuild.
    generation: u64,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    next_id: u64,
    entries: Vec<Entry>,
}

impl History {
    /// `<data dir>/notifications.json`; `None` in unit tests so they never touch the real file.
    pub fn default_path() -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        Some(crate::claude::hook::data_dir().join(HISTORY_FILE))
    }

    /// The saved inbox; empty when the file is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    log::warn!("notifications {}: {e}", path.display());
                }
                return Self::default();
            }
        };
        match serde_json::from_str::<Saved>(&text) {
            Ok(saved) => {
                let max_id = saved.entries.iter().map(|e| e.id).max().unwrap_or(0);
                let mut history = Self {
                    entries: saved.entries,
                    next_id: saved.next_id.max(max_id + 1),
                    generation: 0,
                };
                history.trim();
                history
            }
            Err(e) => {
                log::warn!(
                    "notifications {}: unreadable, starting empty: {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Writes the inbox (0600) through a temporary file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let saved = Saved {
            version: 1,
            next_id: self.next_id,
            entries: self.entries.clone(),
        };
        let json = serde_json::to_vec_pretty(&saved).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&json)?;
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, path)
    }

    /// Every entry, oldest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Entry> {
        self.generation += 1;
        self.entries.iter_mut().find(|e| e.id == id)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn unread(&self) -> usize {
        self.entries.iter().filter(|e| !e.read).count()
    }

    /// Adds `entry` under a fresh id (its own id is replaced) and returns the id.
    pub fn push(&mut self, mut entry: Entry) -> u64 {
        self.next_id = self.next_id.max(1);
        entry.id = self.next_id;
        self.next_id += 1;
        self.entries.push(entry);
        self.trim();
        self.generation += 1;
        self.next_id - 1
    }

    fn trim(&mut self) {
        let excess = self.entries.len().saturating_sub(MAX_ENTRIES);
        self.entries.drain(..excess);
    }

    /// Marks the entries `pick` chooses read; returns how many changed.
    pub fn mark_read_where(&mut self, mut pick: impl FnMut(&Entry) -> bool) -> usize {
        let mut changed = 0;
        for e in self.entries.iter_mut().filter(|e| !e.read) {
            if pick(e) {
                e.read = true;
                changed += 1;
            }
        }
        if changed > 0 {
            self.generation += 1;
        }
        changed
    }

    pub fn mark_read(&mut self, id: u64) -> bool {
        self.mark_read_where(|e| e.id == id) > 0
    }

    pub fn mark_all_read(&mut self) -> usize {
        self.mark_read_where(|_| true)
    }

    /// Deletes entry `id`; returns whether it was there.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        let removed = self.entries.len() < before;
        if removed {
            self.generation += 1;
        }
        removed
    }

    pub fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.entries.clear();
            self.generation += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn entry(title: &str, at_ms: i64) -> Entry {
        Entry {
            id: 0,
            at_ms,
            first_at_ms: at_ms,
            source: Source::Agent,
            severity: Severity::Warn,
            title: title.into(),
            message: "m".into(),
            url: None,
            target: Some("term_a".into()),
            sender: Some("term_a".into()),
            agent_label: Some("fix-login".into()),
            count: 1,
            read: false,
        }
    }

    #[test]
    fn ids_are_fresh_and_the_oldest_go_first() {
        let mut h = History::default();
        let a = h.push(entry("a", 1));
        let b = h.push(entry("b", 2));
        assert_eq!((a, b), (1, 2));
        for i in 0..MAX_ENTRIES {
            h.push(entry("x", i as i64));
        }
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        assert!(h.get(a).is_none() && h.get(b).is_none());
        assert_eq!(h.entries().last().unwrap().id, (MAX_ENTRIES + 2) as u64);
    }

    #[test]
    fn read_state_changes_and_counts() {
        let mut h = History::default();
        let a = h.push(entry("a", 1));
        h.push(entry("b", 2));
        assert_eq!(h.unread(), 2);
        let generation = h.generation();
        assert!(h.mark_read(a));
        assert!(!h.mark_read(a), "already read");
        assert!(h.generation() > generation);
        assert_eq!(h.unread(), 1);
        assert_eq!(h.mark_all_read(), 1);
        assert_eq!(h.unread(), 0);
        h.clear();
        assert!(h.entries().is_empty());
    }

    #[test]
    fn a_removed_entry_leaves_the_unread_count_and_stays_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(HISTORY_FILE);
        let mut h = History::default();
        let a = h.push(entry("a", 1));
        let b = h.push(entry("b", 2));
        let generation = h.generation();
        assert!(h.remove(a));
        assert!(h.generation() > generation);
        assert!(!h.remove(a), "already gone");
        assert_eq!(h.unread(), 1);
        assert!(h.get(a).is_none());
        h.save(&path).unwrap();
        let mut loaded = History::load(&path);
        assert_eq!(
            loaded.entries().iter().map(|e| e.id).collect::<Vec<_>>(),
            [b]
        );
        assert_eq!(loaded.unread(), 1);
        assert_eq!(loaded.push(entry("c", 3)), 3, "a removed id is not reused");
    }

    #[test]
    fn saved_inbox_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(HISTORY_FILE);
        let mut h = History::default();
        let a = h.push(entry("a", 1));
        let mut b = entry("b", 2);
        b.url = Some("https://example.com".into());
        h.push(b);
        h.mark_read(a);
        h.save(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let loaded = History::load(&path);
        assert_eq!(loaded.entries(), h.entries());
        let mut loaded = loaded;
        assert_eq!(
            loaded.push(entry("c", 3)),
            3,
            "ids continue after a restart"
        );

        fs::write(&path, "not json").unwrap();
        assert!(History::load(&path).entries().is_empty());
        assert!(
            History::load(&dir.path().join("missing.json"))
                .entries()
                .is_empty()
        );
    }
}
