//! Reads which terminals a Claude session forwarded input to (`orca terminal send --terminal
//! <handle>`) from its transcript, incrementally: each scan reads only what was appended.
//!
//! Two consumers: [`ForwardScanner`] keeps the newest send per target of a few terminals (role
//! detection, which wants recent history), and [`SendWatch`] turns the sends every Claude
//! terminal makes from now on into traffic (envelopes), never replaying history.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::fleet::Forward;
use crate::orca::parse_timestamp_ms;
use crate::traffic::{AgentMessage, FRESH_MS, MessageKind};

/// On first sight of a transcript only its last this many bytes are read: the newest forwards
/// are what matters, and attendant transcripts reach tens of MB.
pub const INITIAL_TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// Most bytes one scan reads from one transcript; the rest waits for the next scan.
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// Per-transcript read state.
#[derive(Debug, Default)]
struct Scan {
    /// Whether the transcript was read before; the first read starts near its end.
    started: bool,
    /// Byte offset up to which complete lines were read.
    offset: u64,
    /// Forward targets found so far, oldest send first, one entry per target.
    forwards: Vec<Forward>,
}

impl Scan {
    fn record(&mut self, forward: Forward) {
        self.forwards.retain(|f| f.target != forward.target);
        self.forwards.push(forward);
    }
}

/// Keeps a read offset per transcript so repeated scans only read appended lines.
#[derive(Debug, Default)]
pub struct ForwardScanner {
    scans: HashMap<PathBuf, Scan>,
}

impl ForwardScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forward targets per terminal handle, from `transcripts` (handle → transcript path).
    /// Transcripts no longer asked for are forgotten.
    pub fn refresh(
        &mut self,
        transcripts: &BTreeMap<String, PathBuf>,
    ) -> BTreeMap<String, Vec<Forward>> {
        self.scans
            .retain(|path, _| transcripts.values().any(|p| p == path));
        let mut out = BTreeMap::new();
        for (handle, path) in transcripts {
            let scan = self.scans.entry(path.clone()).or_default();
            if let Err(e) = read_new_lines(path, scan) {
                log::debug!("cannot scan {} for forwards: {e}", path.display());
            }
            if !scan.forwards.is_empty() {
                out.insert(handle.clone(), scan.forwards.clone());
            }
        }
        out
    }
}

/// Turns the `orca terminal send`s Claude terminals run into [`AgentMessage`]s as they happen.
/// A transcript is first seen at its end, so history is never replayed; after that each scan
/// reads only complete lines appended since.
#[derive(Debug, Default)]
pub struct SendWatch {
    /// Byte offset up to which complete lines were read, per transcript.
    offsets: HashMap<PathBuf, u64>,
    /// Sends already emitted (sender, target, time), kept while fresh: a send split into
    /// `--text` and `--enter` commands, or a record written twice, is one envelope.
    seen: HashSet<(String, String, i64)>,
}

impl SendWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sends appended to `transcripts` (sender handle → transcript path) since the last
    /// call, sent within [`FRESH_MS`] of `now_ms`. Transcripts no longer asked for are
    /// forgotten.
    pub fn refresh(
        &mut self,
        transcripts: &BTreeMap<String, PathBuf>,
        now_ms: i64,
    ) -> Vec<AgentMessage> {
        self.offsets
            .retain(|path, _| transcripts.values().any(|p| p == path));
        self.seen.retain(|(_, _, at)| now_ms - at < FRESH_MS);
        let mut out = Vec::new();
        for (handle, path) in transcripts {
            let lines = match read_appended(path, self.offsets.get(path).copied()) {
                Ok((lines, offset)) => {
                    self.offsets.insert(path.clone(), offset);
                    lines
                }
                Err(e) => {
                    log::debug!("cannot scan {} for sends: {e}", path.display());
                    continue;
                }
            };
            for line in lines.split(|&b| b == b'\n') {
                let Ok(line) = std::str::from_utf8(line) else {
                    continue;
                };
                for send in forwards_in_line(line) {
                    if let Some(at) = send.at_ms {
                        let key = (handle.clone(), send.target.clone(), at);
                        if now_ms - at >= FRESH_MS || !self.seen.insert(key) {
                            continue;
                        }
                    }
                    out.push(AgentMessage::new(
                        handle.clone(),
                        send.target,
                        MessageKind::Message,
                    ));
                }
            }
        }
        out
    }
}

/// The complete lines appended to `path` after `offset`, and the offset to read from next.
/// Without an offset (first sight) or after a truncation nothing is read: the file's end is
/// where reading starts.
fn read_appended(path: &Path, offset: Option<u64>) -> io::Result<(Vec<u8>, u64)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = match offset {
        Some(offset) if offset <= len => offset,
        _ => return Ok((Vec::new(), len)),
    };
    if start == len {
        return Ok((Vec::new(), len));
    }
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.take(MAX_READ_BYTES.min(len - start))
        .read_to_end(&mut buf)?;
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
        // No complete line yet: read it again next time, unless it is too long to ever fit.
        let next = if buf.len() as u64 == MAX_READ_BYTES {
            start + MAX_READ_BYTES
        } else {
            start
        };
        return Ok((Vec::new(), next));
    };
    buf.truncate(end);
    Ok((buf, start + end as u64 + 1))
}

/// Forward targets of `data`'s declared coordinators, read once from their transcripts under
/// the default Claude projects root: for callers without a [`crate::claude::ClaudeWorker`].
pub fn read_forwards(data: &crate::fleet::FleetData) -> BTreeMap<String, Vec<Forward>> {
    use super::hook::{default_session_map_path, load_hook_mappings};
    use super::sessions::{SessionLocator, default_projects_root};

    let declared = data.declared_coordinators();
    if declared.is_empty() {
        return BTreeMap::new();
    }
    // Map every terminal: sessions are told apart among the terminals sharing a worktree.
    let hooks = load_hook_mappings(&default_session_map_path());
    let sessions =
        SessionLocator::new(default_projects_root()).map_terminals(&data.terminals, Some(&hooks));
    let transcripts: BTreeMap<String, PathBuf> = sessions
        .into_iter()
        .filter(|(h, _)| declared.contains(h))
        .filter_map(|(h, m)| Some((h, m.session()?.transcript_path.clone())))
        .collect();
    ForwardScanner::new().refresh(&transcripts)
}

fn read_new_lines(path: &Path, scan: &mut Scan) -> io::Result<()> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < scan.offset {
        // Truncated or replaced: start over.
        *scan = Scan::default();
    }
    let first = !scan.started;
    scan.started = true;
    let start = if first {
        len.saturating_sub(INITIAL_TAIL_BYTES)
    } else {
        scan.offset
    };
    if start >= len {
        scan.offset = len;
        return Ok(());
    }
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.take(MAX_READ_BYTES.min(len - start))
        .read_to_end(&mut buf)?;
    // A tail read starts mid-line; skip to the first full line.
    let skip = if first && start > 0 {
        buf.iter()
            .position(|&b| b == b'\n')
            .map_or(buf.len(), |i| i + 1)
    } else {
        0
    };
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
        // No complete line yet: read it again next time.
        scan.offset = start + skip as u64;
        return Ok(());
    };
    for line in buf[skip.min(end)..end].split(|&b| b == b'\n') {
        if let Ok(line) = std::str::from_utf8(line) {
            for forward in forwards_in_line(line) {
                scan.record(forward);
            }
        }
    }
    scan.offset = start + end as u64 + 1;
    Ok(())
}

/// Forward targets of the `orca terminal send` commands an assistant record runs.
pub fn forwards_in_line(line: &str) -> Vec<Forward> {
    if !line.contains("\"type\":\"assistant\"")
        || !line.contains("\"tool_use\"")
        || !line.contains("terminal send")
    {
        return Vec::new();
    }
    let at_ms = timestamp(line);
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(pos) = rest.find("terminal send") {
        rest = &rest[pos + "terminal send".len()..];
        // The command runs to the end of its shell line (an escaped newline in the JSON) or
        // the next send.
        let end = [rest.find("\\n"), rest.find("terminal send")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());
        if let Some(target) = terminal_flag(&rest[..end]) {
            out.push(Forward {
                target: target.to_string(),
                at_ms,
            });
        }
    }
    out
}

/// The value of `--terminal <h>` or `--terminal=<h>` in a command, quotes stripped.
fn terminal_flag(command: &str) -> Option<&str> {
    let mut rest = command;
    while let Some(pos) = rest.find("--terminal") {
        rest = &rest[pos + "--terminal".len()..];
        let Some(value) = rest.strip_prefix('=').or_else(|| rest.strip_prefix(' ')) else {
            continue;
        };
        let value = value.trim_start_matches([' ', '\'', '"', '\\']);
        let len = value
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')))
            .unwrap_or(value.len());
        let handle = &value[..len];
        // `$COORD` and other shell expansions are not handles.
        if !handle.is_empty() && handle.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Some(handle);
        }
    }
    None
}

fn timestamp(line: &str) -> Option<i64> {
    let key = "\"timestamp\":\"";
    let start = line.rfind(key)? + key.len();
    let end = start + line[start..].find('"')?;
    parse_timestamp_ms(&line[start..end])
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;

    fn tool_use(command: &str, at: &str) -> String {
        json!({
            "type": "assistant", "timestamp": at,
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "name": "Bash", "input": {"command": command}}
            ]},
        })
        .to_string()
    }

    fn targets(line: &str) -> Vec<String> {
        forwards_in_line(line)
            .into_iter()
            .map(|f| f.target)
            .collect()
    }

    #[test]
    fn finds_both_flag_spellings() {
        let line = tool_use(
            "orca terminal send --terminal term_440d-c3f1 --text \"worker_done\" --enter",
            "2026-09-25T14:24:02.431Z",
        );
        let found = forwards_in_line(&line);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target, "term_440d-c3f1");
        assert_eq!(
            found[0].at_ms,
            parse_timestamp_ms("2026-09-25T14:24:02.431Z")
        );

        let line = tool_use(
            "orca terminal send --text 'hi' --terminal=term_b --enter\norca terminal send \
             --terminal \"term_c\" --text x",
            "2026-09-25T14:24:02Z",
        );
        assert_eq!(targets(&line), ["term_b", "term_c"]);
    }

    #[test]
    fn ignores_other_records_and_commands() {
        let user = json!({"type": "user", "message": {"content":
            "forward with orca terminal send --terminal term_x"}})
        .to_string();
        assert!(targets(&user).is_empty());
        let read = tool_use(
            "orca terminal read --terminal term_x --json",
            "2026-09-25T00:00:00Z",
        );
        assert!(targets(&read).is_empty());
        let var = tool_use(
            "orca terminal send --terminal $COORD --text hi",
            "2026-09-25T00:00:00Z",
        );
        assert!(targets(&var).is_empty());
        // `--terminal` of a later command on another shell line is not this send's.
        let split = tool_use(
            "orca terminal send --text hi\norca terminal read --terminal term_y",
            "2026-09-25T00:00:00Z",
        );
        assert!(targets(&split).is_empty());
    }

    #[test]
    fn scanner_reads_incrementally_and_keeps_newest_per_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut f = File::create(&path).unwrap();
        let send = |h: &str, at: &str| {
            tool_use(&format!("orca terminal send --terminal {h} --text x"), at)
        };
        writeln!(f, "{}", send("term_a", "2026-09-25T10:00:00Z")).unwrap();
        writeln!(f, "{}", send("term_b", "2026-09-25T11:00:00Z")).unwrap();
        f.flush().unwrap();

        let transcripts: BTreeMap<String, PathBuf> = [("term_att".into(), path.clone())].into();
        let mut scanner = ForwardScanner::new();
        let found = scanner.refresh(&transcripts);
        let names: Vec<&str> = found["term_att"]
            .iter()
            .map(|f| f.target.as_str())
            .collect();
        assert_eq!(names, ["term_a", "term_b"]);

        // A partial line is left for the next scan.
        let line = send("term_a", "2026-09-25T12:00:00Z");
        let (head, tail) = line.split_at(20);
        write!(f, "{head}").unwrap();
        f.flush().unwrap();
        let found = scanner.refresh(&transcripts);
        assert_eq!(found["term_att"].len(), 2);
        writeln!(f, "{tail}").unwrap();
        f.flush().unwrap();
        let found = scanner.refresh(&transcripts);
        let last = found["term_att"].last().unwrap();
        assert_eq!(last.target, "term_a");
        assert_eq!(last.at_ms, parse_timestamp_ms("2026-09-25T12:00:00Z"));
        assert_eq!(found["term_att"].len(), 2);

        assert!(scanner.refresh(&BTreeMap::new()).is_empty());
        assert!(scanner.scans.is_empty(), "forgotten");
    }

    #[test]
    fn first_scan_reads_only_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.jsonl");
        let mut f = File::create(&path).unwrap();
        let old = tool_use(
            "orca terminal send --terminal term_old --text x",
            "2026-09-25T10:00:00Z",
        );
        writeln!(f, "{old}").unwrap();
        let filler = format!("{}\n", "x".repeat(1023));
        for _ in 0..(INITIAL_TAIL_BYTES / 1024 + 1) {
            f.write_all(filler.as_bytes()).unwrap();
        }
        let new = tool_use(
            "orca terminal send --terminal term_new --text x",
            "2026-09-25T11:00:00Z",
        );
        writeln!(f, "{new}").unwrap();
        f.flush().unwrap();

        let transcripts: BTreeMap<String, PathBuf> = [("h".into(), path)].into();
        let found = ForwardScanner::new().refresh(&transcripts);
        let names: Vec<&str> = found["h"].iter().map(|f| f.target.as_str()).collect();
        assert_eq!(names, ["term_new"]);
    }

    /// A transcript `send_watch_*` tests append to, watched as terminal `term_from`.
    struct Watched {
        _dir: tempfile::TempDir,
        path: PathBuf,
        file: File,
        transcripts: BTreeMap<String, PathBuf>,
    }

    impl Watched {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("s.jsonl");
            let file = File::create(&path).unwrap();
            let transcripts = [("term_from".to_string(), path.clone())].into();
            Self {
                _dir: dir,
                path,
                file,
                transcripts,
            }
        }

        fn send(&mut self, command: &str, at: &str) {
            writeln!(self.file, "{}", tool_use(command, at)).unwrap();
            self.file.flush().unwrap();
        }
    }

    fn ms(at: &str) -> i64 {
        parse_timestamp_ms(at).unwrap()
    }

    fn pairs(messages: &[AgentMessage]) -> Vec<(&str, &str)> {
        messages
            .iter()
            .map(|m| {
                assert_eq!(m.kind, MessageKind::Message);
                (m.from.as_str(), m.to.as_str())
            })
            .collect()
    }

    #[test]
    fn send_watch_never_replays_history() {
        let mut w = Watched::new();
        let now = ms("2026-09-25T12:00:00Z");
        // Fresh, but already in the transcript when it is first seen.
        w.send(
            "orca terminal send --terminal term_a --text x",
            "2026-09-25T11:59:59Z",
        );
        let mut watch = SendWatch::new();
        assert!(watch.refresh(&w.transcripts, now).is_empty());

        w.send(
            "orca terminal send --terminal term_b --text x",
            "2026-09-25T12:00:01Z",
        );
        let sent = watch.refresh(&w.transcripts, now + 2000);
        assert_eq!(pairs(&sent), [("term_from", "term_b")]);
        assert!(
            watch.refresh(&w.transcripts, now + 4000).is_empty(),
            "read once"
        );
    }

    #[test]
    fn send_watch_emits_one_envelope_per_send_and_drops_stale_ones() {
        let mut w = Watched::new();
        let now = ms("2026-09-25T12:00:00Z");
        let mut watch = SendWatch::new();
        assert!(watch.refresh(&w.transcripts, now).is_empty());

        // Text then Enter to the same terminal in one tool call is one message; two targets
        // are two.
        w.send(
            "orca terminal send --terminal term_a --text hi\n\
             orca terminal send --terminal term_a --enter\n\
             orca terminal send --terminal term_b --text hi --enter",
            "2026-09-25T12:00:01Z",
        );
        // The same record written twice.
        w.send(
            "orca terminal send --terminal term_a --text hi",
            "2026-09-25T12:00:01Z",
        );
        // Appended late but sent long ago.
        w.send(
            "orca terminal send --terminal term_c --text x",
            "2026-09-25T11:50:00Z",
        );
        // Not a send.
        w.send(
            "orca terminal read --terminal term_d",
            "2026-09-25T12:00:01Z",
        );
        let sent = watch.refresh(&w.transcripts, now + 3000);
        assert_eq!(
            pairs(&sent),
            [("term_from", "term_a"), ("term_from", "term_b")]
        );

        // A later send to the same terminal is a new message.
        w.send(
            "orca terminal send --terminal term_a --text again",
            "2026-09-25T12:00:05Z",
        );
        let sent = watch.refresh(&w.transcripts, now + 6000);
        assert_eq!(pairs(&sent), [("term_from", "term_a")]);
        assert!(
            watch
                .seen
                .iter()
                .all(|(_, _, at)| now + 6000 - at < FRESH_MS),
            "only fresh keys kept"
        );
    }

    #[test]
    fn send_watch_waits_for_complete_lines_and_survives_truncation() {
        let mut w = Watched::new();
        let now = ms("2026-09-25T12:00:00Z");
        let mut watch = SendWatch::new();
        assert!(watch.refresh(&w.transcripts, now).is_empty());

        let line = tool_use(
            "orca terminal send --terminal term_a --text x",
            "2026-09-25T12:00:01Z",
        );
        let (head, tail) = line.split_at(30);
        write!(w.file, "{head}").unwrap();
        w.file.flush().unwrap();
        assert!(watch.refresh(&w.transcripts, now + 1000).is_empty());
        writeln!(w.file, "{tail}").unwrap();
        w.file.flush().unwrap();
        let sent = watch.refresh(&w.transcripts, now + 2000);
        assert_eq!(pairs(&sent), [("term_from", "term_a")]);

        // Replaced by a shorter file: start over at its end, replaying nothing.
        drop(std::mem::replace(
            &mut w.file,
            File::create(&w.path).unwrap(),
        ));
        w.send(
            "orca terminal send --terminal term_b --text x",
            "2026-09-25T12:00:02Z",
        );
        assert!(watch.refresh(&w.transcripts, now + 3000).is_empty());
        w.send(
            "orca terminal send --terminal term_c --text x",
            "2026-09-25T12:00:03Z",
        );
        let sent = watch.refresh(&w.transcripts, now + 4000);
        assert_eq!(pairs(&sent), [("term_from", "term_c")]);

        assert!(watch.refresh(&BTreeMap::new(), now + 5000).is_empty());
        assert!(watch.offsets.is_empty(), "forgotten");
    }

    #[test]
    fn send_watch_leaves_the_role_scanner_alone() {
        let mut w = Watched::new();
        w.send(
            "orca terminal send --terminal term_a --text x",
            "2026-09-25T10:00:00Z",
        );
        let now = ms("2026-09-25T12:00:00Z");
        let mut watch = SendWatch::new();
        let mut scanner = ForwardScanner::new();
        assert!(watch.refresh(&w.transcripts, now).is_empty());
        w.send(
            "orca terminal send --terminal term_b --text x",
            "2026-09-25T12:00:01Z",
        );
        w.send(
            "orca terminal send --terminal term_a --text y",
            "2026-09-25T12:00:02Z",
        );
        assert_eq!(watch.refresh(&w.transcripts, now + 3000).len(), 2);
        // The scanner still sees the history and keeps the newest send per target.
        let found = scanner.refresh(&w.transcripts);
        let names: Vec<&str> = found["term_from"]
            .iter()
            .map(|f| f.target.as_str())
            .collect();
        assert_eq!(names, ["term_b", "term_a"]);
        assert_eq!(
            found["term_from"][1].at_ms,
            Some(ms("2026-09-25T12:00:02Z"))
        );
    }
}
