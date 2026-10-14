//! The Ledger, the clan's knowledge store, as the hold shows it: how many entries its folder
//! holds and which changed last. basic-memory serves the same markdown files to the agents (the
//! `ledger` MCP server); the app searches the server and only reads the folder.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, bounded};

use crate::quota::format_duration;

pub mod search;

/// How often the folder is read again.
pub const INTERVAL: Duration = Duration::from_secs(10);
/// How many of the newest entries the hold lists.
pub const NEWEST: usize = 5;

/// `ledger` in the [agents dir](crate::paths::agents_dir).
pub fn default_dir() -> Option<PathBuf> {
    crate::paths::agents_dir().map(|d| d.join("ledger"))
}

/// One entry: a markdown file in the Ledger's folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Actual file name relative to the Ledger, independent of the frontmatter title.
    pub path: PathBuf,
    /// The frontmatter `title`, else the file name.
    pub title: String,
    /// The folder it is filed under (a repo's name, or `general`); empty at the top level.
    pub folder: String,
    /// The frontmatter `type`: finding, decision or how-to.
    pub kind: Option<String>,
    /// Last change, in ms since the epoch.
    pub modified_ms: i64,
}

/// What the Ledger holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shelf {
    pub count: usize,
    /// The [`NEWEST`] most recently changed entries, newest first.
    pub newest: Vec<Entry>,
}

/// Reads `dir`: every `.md` file below it, hidden files and folders skipped. A missing folder
/// is an empty shelf.
pub fn scan(dir: &Path) -> Shelf {
    let mut files = Vec::new();
    collect(dir, &mut files);
    let count = files.len();
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let newest = files
        .into_iter()
        .take(NEWEST)
        .map(|(path, modified_ms)| entry(dir, &path, modified_ms))
        .collect();
    Shelf { count, newest }
}

fn collect(dir: &Path, out: &mut Vec<(PathBuf, i64)>) {
    let Ok(items) = std::fs::read_dir(dir) else {
        return;
    };
    for item in items.flatten() {
        if item.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if item.file_type().is_ok_and(|t| t.is_symlink()) {
            continue;
        }
        let Ok(meta) = item.metadata() else {
            continue;
        };
        let path = item.path();
        if meta.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            out.push((path, modified_ms));
        }
    }
}

fn entry(dir: &Path, path: &Path, modified_ms: i64) -> Entry {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let stem = || {
        path.file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
    };
    Entry {
        path: path.strip_prefix(dir).unwrap_or(path).to_owned(),
        title: frontmatter(&text, "title").unwrap_or_else(stem),
        folder: path
            .parent()
            .and_then(|p| p.strip_prefix(dir).ok())
            .map_or_else(String::new, |p| p.to_string_lossy().into_owned()),
        kind: frontmatter(&text, "type"),
        modified_ms,
    }
}

/// A top-level `key: value` of the YAML frontmatter `text` starts with, quotes stripped.
fn frontmatter(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    lines.take_while(|l| l.trim() != "---").find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?.trim();
        let value = value.trim_matches(['"', '\'']);
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The chamber's hover text: the count and the newest entries.
pub fn describe(shelf: &Shelf, now_ms: i64) -> String {
    if shelf.count == 0 {
        return "The Ledger: no entries yet".into();
    }
    let plural = if shelf.count == 1 { "entry" } else { "entries" };
    let mut out = format!("The Ledger: {} {plural}\nNewest:", shelf.count);
    for e in &shelf.newest {
        let mut about: Vec<String> = [Some(&e.folder), e.kind.as_ref()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();
        about.push(format!(
            "{} ago",
            format_duration((now_ms - e.modified_ms).max(0))
        ));
        out.push_str(&format!("\n• {} ({})", e.title, about.join(", ")));
    }
    out
}

/// The newest [`Shelf`], read on a background thread every [`INTERVAL`].
pub struct ShelfWatch {
    rx: Receiver<Shelf>,
    shelf: Shelf,
}

impl ShelfWatch {
    /// Starts reading `dir`; without one the shelf stays empty.
    pub fn spawn(dir: Option<PathBuf>, repaint: Box<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = bounded(1);
        if let Some(dir) = dir
            && let Err(e) = thread::Builder::new()
                .name("ledger-shelf".into())
                .spawn(move || run(&dir, &tx, &*repaint))
        {
            log::warn!("ledger reader not started: {e}");
        }
        Self {
            rx,
            shelf: Shelf::default(),
        }
    }

    /// Takes the thread's newest read, if it sent one, and returns the shelf.
    pub fn poll(&mut self) -> &Shelf {
        if let Ok(shelf) = self.rx.try_recv() {
            self.shelf = shelf;
        }
        &self.shelf
    }
}

fn run(dir: &Path, tx: &Sender<Shelf>, repaint: &(dyn Fn() + Send + Sync)) {
    let mut last: Option<Shelf> = None;
    loop {
        let shelf = scan(dir);
        if last.as_ref() != Some(&shelf) {
            if tx.send(shelf.clone()).is_err() {
                return;
            }
            repaint();
            last = Some(shelf);
        }
        thread::sleep(INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::SystemTime;

    use super::*;

    fn write(dir: &Path, path: &str, text: &str, age_s: u64) {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        let at = SystemTime::now() - Duration::from_secs(age_s);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    #[test]
    fn counts_markdown_entries_and_lists_the_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "myrstack/Use basic-memory.md",
            "---\ntitle: \"Clan memory store is basic-memory\"\ntype: decision\ntags:\n- ledger\n---\n\nBody.\n",
            60,
        );
        write(root, "general/No header.md", "Just text.\n", 10);
        write(root, "Top.md", "---\ntitle: Top level\n---\n", 3_600);
        write(root, "general/notes.txt", "not an entry", 0);
        write(root, ".git/HEAD.md", "hidden", 0);
        for i in 0..NEWEST {
            write(root, &format!("old/e{i}.md"), "", 86_400 + i as u64);
        }

        let shelf = scan(root);
        assert_eq!(shelf.count, 3 + NEWEST);
        assert_eq!(shelf.newest.len(), NEWEST);
        let first = &shelf.newest[0];
        assert_eq!(
            (
                first.title.as_str(),
                first.folder.as_str(),
                first.kind.as_deref()
            ),
            ("No header", "general", None)
        );
        let second = &shelf.newest[1];
        assert_eq!(
            (
                second.title.as_str(),
                second.folder.as_str(),
                second.kind.as_deref()
            ),
            (
                "Clan memory store is basic-memory",
                "myrstack",
                Some("decision")
            )
        );
        assert_eq!(
            (
                shelf.newest[2].title.as_str(),
                shelf.newest[2].folder.as_str()
            ),
            ("Top level", "")
        );
        assert_eq!(scan(&root.join("missing")), Shelf::default());
    }

    #[test]
    fn describes_the_count_and_the_newest_entries() {
        assert_eq!(describe(&Shelf::default(), 0), "The Ledger: no entries yet");
        let shelf = Shelf {
            count: 1,
            newest: vec![Entry {
                path: "myrstack/Setup gotchas.md".into(),
                title: "Setup gotchas".into(),
                folder: "myrstack".into(),
                kind: Some("finding".into()),
                modified_ms: 0,
            }],
        };
        assert_eq!(
            describe(&shelf, 2 * 3_600_000),
            "The Ledger: 1 entry\nNewest:\n• Setup gotchas (myrstack, finding, 2h 0m ago)"
        );
    }
}
