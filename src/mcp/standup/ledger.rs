//! Bounded local Ledger scan. Modification time selects candidates, never dates an outcome.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

const NOTES: usize = 20;
const EXCERPT: usize = 1600;

pub(super) fn recent(root: &Path, since: i64, until: i64) -> Value {
    let mut files = Vec::new();
    let mut budget = 5000;
    let mut incomplete = false;
    collect(root, 0, &mut budget, &mut files, &mut incomplete);
    files.retain(|(_, time)| super::in_window(*time, since, until));
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let truncated = files.len() > NOTES || incomplete;
    let entries: Vec<_> = files
        .into_iter()
        .take(NOTES)
        .filter_map(|(path, modified)| {
            let mut text = String::new();
            let read =
                fs::File::open(&path).and_then(|f| f.take(64 * 1024).read_to_string(&mut text));
            if read.is_err() {
                incomplete = true;
                return None;
            }
            Some(
                json!({"source": path.strip_prefix(root).unwrap_or(&path).to_string_lossy(),
            "modified_at_ms": modified, "excerpt": text.chars().take(EXCERPT).collect::<String>(),
            "excerpt_truncated": text.chars().count() > EXCERPT}),
            )
        })
        .collect();
    json!({"entries": entries, "incomplete": incomplete, "truncated": truncated,
        "scope": "Newest 20 Markdown notes modified since yesterday's local midnight. Excerpts are unverified claims; modification dates are not event dates."})
}

fn collect(
    dir: &Path,
    depth: usize,
    budget: &mut usize,
    out: &mut Vec<(PathBuf, i64)>,
    incomplete: &mut bool,
) {
    if depth > 8 || *budget == 0 {
        *incomplete = true;
        return;
    }
    let Ok(items) = fs::read_dir(dir) else {
        *incomplete = true;
        return;
    };
    for item in items {
        if *budget == 0 {
            *incomplete = true;
            break;
        }
        *budget -= 1;
        let Ok(item) = item else {
            *incomplete = true;
            continue;
        };
        if item.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(kind) = item.file_type() else {
            *incomplete = true;
            continue;
        };
        if kind.is_dir() {
            collect(&item.path(), depth + 1, budget, out, incomplete);
        } else if kind.is_file() && item.path().extension().is_some_and(|ext| ext == "md") {
            match item.metadata().and_then(|m| m.modified()) {
                Ok(time) => {
                    let ms = time
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as i64);
                    out.push((item.path(), ms));
                }
                Err(_) => *incomplete = true,
            }
        }
    }
}
