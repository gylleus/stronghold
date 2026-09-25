//! Token usage per Codex rollout, from its `token_count` events.
//!
//! Each `event_msg` with `payload.type == "token_count"` carries `info.total_token_usage`,
//! the session's cumulative count, so the last one is the total. Files are tailed from a
//! remembered offset, so after the first read a refresh only parses appended lines. The
//! per-model split attributes each event's increase to the model of the latest
//! `turn_context`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::rollouts::RolloutMatch;
use crate::claude::subagents::{Tail, tail_file};
use crate::claude::{TokenCounts, Usage};

/// A Codex terminal's rollout and its token usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSession {
    pub rollout: RolloutMatch,
    /// `None` until the rollout has a `token_count` event with totals.
    pub usage: Option<Usage>,
}

/// `info.total_token_usage` / `last_token_usage` as Codex writes them. `input_tokens`
/// includes the cached and cache-write input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RawTokenUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

impl RawTokenUsage {
    /// Split the way Claude usage is: uncached input, output, cache read, cache write.
    pub fn counts(&self) -> TokenCounts {
        TokenCounts {
            input: self
                .input_tokens
                .saturating_sub(self.cached_input_tokens)
                .saturating_sub(self.cache_write_input_tokens),
            output: self.output_tokens,
            cache_read: self.cached_input_tokens,
            cache_creation: self.cache_write_input_tokens,
        }
    }
}

#[derive(Debug, Default)]
struct FileState {
    tail: Tail,
    model: Option<String>,
    last: Option<TokenCounts>,
    usage: Usage,
}

/// Incremental per-rollout token reader.
#[derive(Debug, Default)]
pub struct CodexUsageTracker {
    files: HashMap<PathBuf, FileState>,
}

impl CodexUsageTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads what was appended to each matched rollout and pairs the matches with usage.
    /// Rollouts no longer matched are forgotten.
    pub fn refresh(
        &mut self,
        matches: BTreeMap<String, RolloutMatch>,
    ) -> BTreeMap<String, CodexSession> {
        let keep: HashSet<&Path> = matches.values().map(|m| m.rollout.path.as_path()).collect();
        self.files.retain(|p, _| keep.contains(p.as_path()));
        matches
            .into_iter()
            .map(|(handle, rollout)| {
                let usage = self.read(&rollout.rollout.path);
                (handle, CodexSession { rollout, usage })
            })
            .collect()
    }

    /// Usage of the rollout at `path` after reading its new lines.
    pub fn read(&mut self, path: &Path) -> Option<Usage> {
        let state = self.files.entry(path.to_path_buf()).or_default();
        let len = std::fs::metadata(path).map_or(0, |m| m.len());
        if len < state.tail.offset {
            // Truncated or replaced: start over.
            *state = FileState::default();
        }
        let (model, last, usage) = (&mut state.model, &mut state.last, &mut state.usage);
        let tail = &mut state.tail;
        tail_file(path, tail, |line| match parse_line(line) {
            Some(Event::Model(m)) => *model = Some(m),
            Some(Event::Total(total)) => {
                let counts = total.counts();
                let prev = last.unwrap_or_default();
                let delta = TokenCounts {
                    input: counts.input.saturating_sub(prev.input),
                    output: counts.output.saturating_sub(prev.output),
                    cache_read: counts.cache_read.saturating_sub(prev.cache_read),
                    cache_creation: counts.cache_creation.saturating_sub(prev.cache_creation),
                };
                let name = model.as_deref().unwrap_or("unknown");
                let entry = usage.by_model.entry(name.to_string()).or_default();
                *entry = add(entry, &delta);
                usage.tokens = counts;
                *last = Some(counts);
            }
            None => {}
        });
        state.last.map(|_| state.usage.clone())
    }
}

fn add(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    TokenCounts {
        input: a.input + b.input,
        output: a.output + b.output,
        cache_read: a.cache_read + b.cache_read,
        cache_creation: a.cache_creation + b.cache_creation,
    }
}

enum Event {
    Model(String),
    Total(RawTokenUsage),
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    payload: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct TokenCountPayload {
    info: Option<TokenInfo>,
}

#[derive(Deserialize)]
struct TokenInfo {
    total_token_usage: Option<RawTokenUsage>,
}

/// The model from a `turn_context` record, or the totals from a `token_count` event.
fn parse_line(line: &str) -> Option<Event> {
    let is_count = line.contains("\"token_count\"");
    let is_turn = line.contains("\"turn_context\"");
    if !is_count && !is_turn {
        return None;
    }
    let record: Record = serde_json::from_str(line).ok()?;
    let payload = record.payload?;
    match record.kind.as_str() {
        "turn_context" => Some(Event::Model(payload.get("model")?.as_str()?.to_string())),
        "event_msg" if payload.get("type")?.as_str()? == "token_count" => {
            let p: TokenCountPayload = serde_json::from_value(payload).ok()?;
            Some(Event::Total(p.info?.total_token_usage?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    fn token_count(input: u64, cached: u64, output: u64) -> Value {
        json!({"timestamp": "2026-09-24T09:43:05.923Z", "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": input, "cached_input_tokens": cached,
                    "cache_write_input_tokens": 0, "output_tokens": output,
                    "reasoning_output_tokens": 1, "total_tokens": input + output},
                "last_token_usage": {"input_tokens": 1, "output_tokens": 1},
                "model_context_window": 258_400},
            "rate_limits": {"primary": {"used_percent": 51.0}}}})
    }

    fn turn(model: &str) -> Value {
        json!({"type": "turn_context", "payload": {"cwd": "/x", "model": model}})
    }

    fn append(path: &Path, records: &[Value]) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for r in records {
            writeln!(f, "{r}").unwrap();
        }
    }

    #[test]
    fn raw_counts_split_cached_input() {
        let raw = RawTokenUsage {
            input_tokens: 5_111_091,
            cached_input_tokens: 4_974_976,
            cache_write_input_tokens: 100,
            output_tokens: 16_032,
            reasoning_output_tokens: 1_112,
            total_tokens: 5_127_123,
        };
        let c = raw.counts();
        assert_eq!(
            (c.input, c.output, c.cache_read, c.cache_creation),
            (136_015, 16_032, 4_974_976, 100)
        );
        assert_eq!(c.total(), raw.total_tokens);
    }

    #[test]
    fn reads_last_total_incrementally() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rollout.jsonl");
        let meta = json!({"type": "session_meta", "payload": {"id": "s", "cwd": "/x"}});
        let null_info = json!({"type": "event_msg", "payload": {"type": "token_count",
            "info": null}});
        // A message that merely mentions token_count is not an event.
        let chatter = json!({"type": "response_item", "payload": {"type": "message",
            "content": "\"token_count\" \"turn_context\""}});
        append(&path, &[meta, null_info, chatter]);
        let mut tracker = CodexUsageTracker::new();
        assert_eq!(tracker.read(&path), None, "no totals yet");

        append(&path, &[turn("gpt-6"), token_count(1_000, 800, 50)]);
        let usage = tracker.read(&path).unwrap();
        assert_eq!(usage.tokens.total(), 1_050);
        assert_eq!(usage.tokens.cache_read, 800);

        // A partial line is left for the next read.
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(f, "{{\"type\":\"turn_context\",").unwrap();
        drop(f);
        let offset = tracker.files[&path].tail.offset;
        assert_eq!(tracker.read(&path).unwrap().tokens.total(), 1_050);
        assert_eq!(tracker.files[&path].tail.offset, offset);
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "\"payload\":{{\"model\":\"gpt-6-mini\"}}}}").unwrap();
        drop(f);
        append(&path, &[token_count(3_000, 2_000, 150)]);
        let usage = tracker.read(&path).unwrap();
        assert_eq!(usage.tokens.total(), 3_150);
        assert_eq!(usage.tokens.output, 150);
        assert_eq!(usage.by_model["gpt-6"].total(), 1_050);
        assert_eq!(usage.by_model["gpt-6-mini"].total(), 2_100);
        assert_eq!(usage.label(), "3.1k tok");

        append(&path, &[turn("gpt-6"), token_count(4_000, 2_500, 200)]);
        let usage = tracker.read(&path).unwrap();
        assert_eq!(usage.by_model["gpt-6"].total(), 2_100);
        assert_eq!(
            usage.by_model.values().map(TokenCounts::total).sum::<u64>(),
            usage.tokens.total()
        );
    }

    #[test]
    fn refresh_pairs_matches_and_forgets_unmatched() {
        use crate::codex::rollouts::{Rollout, RolloutMatch};

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rollout-a.jsonl");
        append(&path, &[token_count(10, 0, 5)]);
        let m = RolloutMatch {
            rollout: Rollout {
                session_id: "a".into(),
                path: path.clone(),
                cwd: "/x".into(),
                modified_ms: 0,
            },
            ambiguous: false,
        };
        let mut tracker = CodexUsageTracker::new();
        let out = tracker.refresh(BTreeMap::from([("t1".to_string(), m)]));
        assert_eq!(out["t1"].usage.as_ref().unwrap().tokens.total(), 15);
        assert_eq!(
            out["t1"].usage.as_ref().unwrap().by_model["unknown"].total(),
            15
        );
        tracker.refresh(BTreeMap::new());
        assert!(tracker.files.is_empty());
    }
}
