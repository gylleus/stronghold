//! Pre-filled playbook templates: the attendant brief for a run and the kickoff for a task, with
//! every `<…>` placeholder Stronghold can answer from live clan data filled in. The rest stay
//! visibly marked (`<…: FILL IN>`), and the output starts with a header saying what was filled
//! from where and what is left.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clan::{ClanData, Forward};
use crate::orca::{DispatchStatus, InboxMessage, Run, Task, Terminal, Worker, WorkerState};
use crate::playbook::{ATTENDANT_BRIEF, KICKOFF, Version};
use crate::ui::sidebar::repo_of;

/// Appended inside a placeholder nobody could fill: `<fact: FILL IN>`.
pub const FILL_IN: &str = ": FILL IN";
/// The line between the header and the filled template.
pub const CUT_LINE: &str =
    "----- 8< ----- filled template below; the header above is not part of it";
/// Longest placeholder text; a longer `<…>` is not a placeholder.
const MAX_PLACEHOLDER: usize = 400;
/// Longest value shown in the header.
const HEADER_VALUE_CHARS: usize = 72;
/// Dispatches the attendant header lists.
const DISPATCHES_SHOWN: usize = 30;

/// Placeholders of the attendant brief that the attendant fills itself as it works (message
/// ids, event types, times). They are kept verbatim, not marked.
pub const ATTENDANT_RUNTIME: &[&str] = &[
    "id",
    "type",
    "track",
    "subject",
    "time",
    "event",
    "its worker terminal",
    "that line",
    "worker handle",
    "instruction",
    "one line",
];

/// Placeholders of the kickoff the worker fills itself (its approval line).
pub const KICKOFF_RUNTIME: &[&str] = &["one line"];

// ---------------------------------------------------------------------------
// Placeholders

/// One `<…>` in a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    /// The text between the brackets with whitespace runs collapsed.
    pub name: String,
    /// The text between the brackets as written (may span lines).
    pub raw: String,
    /// Byte range of the whole `<…>`.
    pub start: usize,
    pub end: usize,
}

/// Every `<…>` placeholder in `text`, in order. Skips `<…>` itself (the templates' "fill every
/// <…>"), HTML-ish tags and comments (`</`, `<!`), URLs, and brackets that close too far away.
pub fn placeholders(text: &str) -> Vec<Placeholder> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find('<') {
        let start = i + off;
        let after = &text[start + 1..];
        let starts_ok = after.chars().next().is_some_and(|c| {
            !(c.is_whitespace() || matches!(c, '!' | '/' | '=' | '<' | '>' | '-'))
        });
        let close = after
            .find(['<', '>'])
            .filter(|&n| n <= MAX_PLACEHOLDER && after.as_bytes()[n] == b'>');
        if starts_ok && let Some(len) = close {
            let raw = &after[..len];
            if !raw.contains("://") && !matches!(raw.trim(), "…" | "...") {
                let end = start + 1 + len + 1;
                out.push(Placeholder {
                    name: collapse(raw),
                    raw: raw.to_owned(),
                    start,
                    end,
                });
                i = end;
                continue;
            }
        }
        i = start + 1;
    }
    out
}

/// The distinct placeholder names of `text`, in first-seen order.
pub fn placeholder_names(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    placeholders(text)
        .into_iter()
        .filter(|p| seen.insert(p.name.clone()))
        .map(|p| p.name)
        .collect()
}

/// What to put in a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Filled with `value`, taken from `from`.
    Value { value: String, from: String },
    /// Left verbatim: the agent fills it while working.
    Runtime,
    /// Nothing known: marked `<…: FILL IN>`.
    Missing,
}

/// A placeholder that was filled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilledEntry {
    pub placeholder: String,
    pub value: String,
    pub from: String,
}

/// The result of [`fill`]: the text and what happened to each placeholder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FillResult {
    pub text: String,
    pub filled: Vec<FilledEntry>,
    pub missing: Vec<String>,
    pub runtime: Vec<String>,
}

/// Replaces the placeholders of `template` as `resolve(name, line_before)` says;
/// `line_before` is the text between the start of the line and the placeholder.
pub fn fill(template: &str, mut resolve: impl FnMut(&str, &str) -> Resolution) -> FillResult {
    let mut out = FillResult::default();
    let mut last = 0;
    for p in placeholders(template) {
        out.text.push_str(&template[last..p.start]);
        let line_start = template[..p.start].rfind('\n').map_or(0, |n| n + 1);
        match resolve(&p.name, &template[line_start..p.start]) {
            Resolution::Value { value, from } => {
                out.text.push_str(&value);
                let entry = FilledEntry {
                    placeholder: p.name,
                    value,
                    from,
                };
                if !out.filled.contains(&entry) {
                    out.filled.push(entry);
                }
            }
            Resolution::Runtime => {
                out.text.push_str(&template[p.start..p.end]);
                push_unique(&mut out.runtime, p.name);
            }
            Resolution::Missing => {
                out.text.push('<');
                out.text.push_str(&p.raw);
                out.text.push_str(FILL_IN);
                out.text.push('>');
                push_unique(&mut out.missing, p.name);
            }
        }
        last = p.end;
    }
    out.text.push_str(&template[last..]);
    out
}

fn push_unique(list: &mut Vec<String>, name: String) {
    if !list.contains(&name) {
        list.push(name);
    }
}

// ---------------------------------------------------------------------------
// Clan data on the wire

/// The part of one poll's [`ClanData`] templates are filled from, as the control API's
/// `clan_data` method serves it. Terminal previews and message bodies are dropped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClanFacts {
    pub polled_at_ms: i64,
    pub terminals: Vec<Terminal>,
    pub runs: Vec<Run>,
    pub workers: Vec<Worker>,
    pub tasks: BTreeMap<String, Vec<Task>>,
    pub inbox: Vec<InboxMessage>,
    /// Forward targets read from declared coordinators' transcripts, so the attendant and the
    /// real coordinator resolve as in the app (see [`ClanData::run_roles`]).
    #[serde(default)]
    pub forwards: BTreeMap<String, Vec<Forward>>,
}

impl ClanFacts {
    pub fn from_clan(data: &ClanData) -> Self {
        Self {
            polled_at_ms: data.polled_at_ms,
            terminals: data
                .terminals
                .iter()
                .map(|t| Terminal {
                    preview: None,
                    ..t.clone()
                })
                .collect(),
            runs: data.runs.clone(),
            workers: data.workers.clone(),
            tasks: data.tasks.clone(),
            inbox: data
                .inbox
                .iter()
                .map(|m| InboxMessage {
                    body: None,
                    payload: None,
                    ..m.clone()
                })
                .collect(),
            forwards: data.forwards.clone(),
        }
    }

    pub fn into_clan(self) -> ClanData {
        ClanData {
            polled_at_ms: self.polled_at_ms,
            terminals: self.terminals,
            runs: self.runs,
            workers: self.workers,
            tasks: self.tasks,
            inbox: self.inbox,
            forwards: self.forwards,
            ..ClanData::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Filled templates

/// Where the fill came from, for the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillContext {
    /// The template file's version stamp.
    pub version: Version,
    /// `the running Stronghold app` or `one direct Orca poll`.
    pub source: String,
    pub now_ms: i64,
}

/// A filled template with its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filled {
    pub template: &'static str,
    /// `run run_…` or `task task_…`.
    pub subject: String,
    pub result: FillResult,
    /// Header lines with facts for the reader (dispatches, deps, …).
    pub facts: Vec<String>,
    pub context: FillContext,
    pub polled_at_ms: i64,
}

impl Filled {
    /// The header: what was filled from where, what is left, and the facts.
    pub fn header(&self) -> String {
        let r = &self.result;
        let mut out = format!(
            "Stronghold pre-filled {} for {} · {}\n{}\nData: {}, Orca polled {}\n",
            self.template,
            self.subject,
            utc(self.context.now_ms),
            self.context.version.header(),
            self.context.source,
            utc(self.polled_at_ms),
        );
        if r.filled.is_empty() {
            out.push_str("Filled from live data: nothing\n");
        } else {
            out.push_str("Filled from live data:\n");
            for e in &r.filled {
                out.push_str(&format!(
                    "- <{}> = {} ({})\n",
                    short(&e.placeholder, 40),
                    short(&e.value, HEADER_VALUE_CHARS),
                    e.from
                ));
            }
        }
        if r.missing.is_empty() {
            out.push_str("Left to fill: nothing\n");
        } else {
            out.push_str(&format!(
                "Left to fill (marked \"{}\"): {}\n",
                FILL_IN.trim_start_matches(": "),
                bracketed(&r.missing)
            ));
        }
        if !r.runtime.is_empty() {
            out.push_str(&format!(
                "Kept as-is (the agent fills these as it works): {}\n",
                bracketed(&r.runtime)
            ));
        }
        for line in &self.facts {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(CUT_LINE);
        out
    }

    pub fn text(&self) -> String {
        format!("{}\n\n{}", self.header(), self.result.text.trim_end())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "template": self.template,
            "subject": self.subject,
            "version": self.context.version.to_json(),
            "source": self.context.source,
            "polled_at_ms": self.polled_at_ms,
            "filled": self.result.filled,
            "missing": self.result.missing,
            "runtime": self.result.runtime,
            "text": self.text(),
        })
    }
}

/// The attendant brief for `run_id`. `scratchpad` is the coordinator's scratchpad dir when it
/// could be found.
pub fn attendant_brief(
    template: &str,
    data: &ClanData,
    run_id: &str,
    scratchpad: Option<&str>,
    context: FillContext,
) -> Result<Filled, String> {
    let run = data
        .run(run_id)
        .ok_or_else(|| format!("no run {run_id} in Orca's run list"))?;
    let objective = run.objective.as_deref().map(collapse);
    let declared = run.coordinator_handle.as_deref();
    let roles = data.run_roles(run_id);
    let coordinator = roles.coordinator.clone();
    let coordinator_from = match (declared, &coordinator) {
        (Some(d), Some(c)) if d == c => "the run's coordinator, live",
        (_, Some(_)) if roles.attendant.is_some() => {
            "the run's real coordinator: its declared one is the attendant"
        }
        (_, Some(_)) => "effective coordinator: the run's declared one is not live",
        _ => "",
    };
    let campaign = objective.as_deref().map(campaign_name);
    let result = fill(template, |name, before| match name {
        "campaign" => match &campaign {
            Some((c, from)) => value(c.clone(), from),
            None => Resolution::Missing,
        },
        "campaign objective" => match &objective {
            Some(o) => value(o.replace('"', "\\\""), "run objective"),
            None => Resolution::Missing,
        },
        "coordinator terminal handle" => match &coordinator {
            Some(h) => value(h.clone(), coordinator_from),
            None => Resolution::Missing,
        },
        "coordinator scratchpad path" => match scratchpad {
            Some(p) => value(p.to_owned(), "the coordinator's Claude session"),
            None => Resolution::Missing,
        },
        "id" if before.ends_with("RUN_ID=") => value(run_id.to_owned(), "run id"),
        n if ATTENDANT_RUNTIME.contains(&n) => Resolution::Runtime,
        _ => Resolution::Missing,
    });

    let mut facts = Vec::new();
    let tasks = data.tasks.get(run_id).map(Vec::as_slice).unwrap_or(&[]);
    let workers: Vec<&Worker> = data
        .workers
        .iter()
        .filter(|w| w.run_id.as_deref() == Some(run_id))
        .collect();
    facts.push(format!(
        "Run: {run_id}, created {} · {} task{} · {} dispatch{}",
        run.created_at
            .as_ref()
            .and_then(|t| t.epoch_ms())
            .map_or_else(|| "at an unknown time".to_owned(), utc),
        tasks.len(),
        plural(tasks.len()),
        workers.len(),
        if workers.len() == 1 { "" } else { "es" },
    ));
    facts.push(format!(
        "The run already exists: skip step 1 (`run-create`) and write {run_id} to \
         SCRATCH/run-id.txt."
    ));
    let (finished, live): (Vec<&Worker>, Vec<&Worker>) =
        workers.iter().partition(|w| is_finished(w));
    if !live.is_empty() {
        facts.push(format!("Live dispatches ({}):", live.len()));
        for w in live.iter().take(DISPATCHES_SHOWN) {
            let task = w
                .task_id
                .as_deref()
                .and_then(|id| tasks.iter().find(|t| t.id == id));
            facts.push(format!(
                "- {} · worker {} · task {} · {}",
                w.dispatch_id,
                w.agent_terminal_handle
                    .as_deref()
                    .unwrap_or("(no terminal)"),
                match (w.task_id.as_deref(), task) {
                    (Some(id), Some(t)) => format!("{id} \"{}\"", short(t.label(), 60)),
                    (Some(id), None) => id.to_owned(),
                    (None, _) => "(none)".to_owned(),
                },
                worker_state(w),
            ));
        }
        if live.len() > DISPATCHES_SHOWN {
            facts.push(format!("- … {} more", live.len() - DISPATCHES_SHOWN));
        }
    }
    if !finished.is_empty() {
        let mut by_state: BTreeMap<String, usize> = BTreeMap::new();
        for w in &finished {
            *by_state.entry(worker_state(w)).or_insert(0) += 1;
        }
        facts.push(format!(
            "Finished dispatches: {} ({})",
            finished.len(),
            by_state
                .iter()
                .map(|(s, n)| format!("{s} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let seen = event_types(&data.inbox, run_id);
    if !seen.is_empty() {
        facts.push(format!(
            "Bus event types on this run so far: {}",
            seen.iter()
                .map(|(t, n)| format!("{t} ×{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(types) = waited_types(template) {
        facts.push(format!("The waiter forwards: {types}"));
    }
    Ok(Filled {
        template: ATTENDANT_BRIEF,
        subject: format!("run {run_id}"),
        result,
        facts,
        context,
        polled_at_ms: data.polled_at_ms,
    })
}

/// The kickoff for `task_id`.
pub fn kickoff(
    template: &str,
    data: &ClanData,
    task_id: &str,
    context: FillContext,
) -> Result<Filled, String> {
    let (run_id, task) = data
        .tasks
        .iter()
        .find_map(|(run, tasks)| tasks.iter().find(|t| t.id == task_id).map(|t| (run, t)))
        .ok_or_else(|| {
            format!(
                "no task {task_id} in the active runs' task lists ({} runs polled)",
                data.tasks.len()
            )
        })?;
    let run = data.run(run_id);
    let tasks = &data.tasks[run_id];
    let worker = task
        .dispatch_id
        .as_deref()
        .and_then(|d| data.workers.iter().find(|w| w.dispatch_id == d))
        .or_else(|| {
            data.workers
                .iter()
                .rfind(|w| w.task_id.as_deref() == Some(task_id))
        });
    let worker_terminal = worker
        .and_then(|w| w.agent_terminal_handle.as_deref())
        .and_then(|h| data.terminal(h));
    let coordinator_terminal = data
        .effective_coordinator(run_id)
        .and_then(|h| data.terminal(&h).cloned());
    let repo = worker_terminal
        .and_then(|t| t.worktree_path.as_deref())
        .and_then(repo_of)
        .map(|r| (r, "the worker's worktree"))
        .or_else(|| {
            coordinator_terminal
                .as_ref()
                .and_then(|t| t.worktree_path.as_deref())
                .and_then(repo_of)
                .map(|r| (r, "the coordinator's worktree"))
        });
    let title = task
        .task_title
        .as_deref()
        .map(collapse)
        .unwrap_or_else(|| collapse(task.label()));
    let label = collapse(task.label());
    let ticket = ticket_id(&title).map(|t| (t, "task title")).or_else(|| {
        task.spec
            .as_deref()
            .and_then(ticket_id)
            .map(|t| (t, "task spec"))
    });
    let by_id = |id: &str| tasks.iter().find(|t| t.id == id);
    let dep_line = |id: &str| match by_id(id) {
        Some(t) => format!("{id} \"{}\" ({})", short(t.label(), 60), t.status),
        None => id.to_owned(),
    };
    let blocked_by: Vec<String> = task.deps.iter().map(|d| dep_line(d)).collect();
    let blocks: Vec<String> = tasks
        .iter()
        .filter(|t| t.deps.iter().any(|d| d == task_id))
        .map(|t| dep_line(&t.id))
        .collect();
    let list_or_none = |items: &[String]| {
        if items.is_empty() {
            "none".to_owned()
        } else {
            items.join("; ")
        }
    };

    let result = fill(template, |name, _| match name {
        "track name" | "track" => value(label.clone(), "task name"),
        "one-line title" => value(title.clone(), "task title"),
        "repo" => match &repo {
            Some((r, from)) => value(r.clone(), from),
            None => Resolution::Missing,
        },
        "VROOM-###" => match &ticket {
            Some((t, from)) => value(t.clone(), from),
            None => Resolution::Missing,
        },
        n if n.starts_with("What this track owns") => match task.spec.as_deref() {
            Some(spec) if !spec.trim().is_empty() => value(spec.trim().to_owned(), "task spec"),
            _ => Resolution::Missing,
        },
        n if n.starts_with("task/PR, or") => value(list_or_none(&blocked_by), "task deps"),
        n if n.starts_with("task, or") => {
            value(list_or_none(&blocks), "tasks of the run that depend on it")
        }
        n if KICKOFF_RUNTIME.contains(&n) => Resolution::Runtime,
        _ => Resolution::Missing,
    });

    let mut facts = vec![format!(
        "Task: {task_id} \"{}\" · {} · run {run_id}{}",
        short(&label, 60),
        task.status,
        run.and_then(|r| r.objective.as_deref())
            .map(|o| format!(" (\"{}\")", short(&collapse(o), 60)))
            .unwrap_or_default()
    )];
    match worker {
        Some(w) => facts.push(format!(
            "Dispatch: {} · worker {} · worktree {} · branch {}",
            w.dispatch_id,
            w.agent_terminal_handle
                .as_deref()
                .unwrap_or("(no terminal)"),
            worker_terminal
                .and_then(|t| t.worktree_path.as_deref())
                .unwrap_or("(unknown)"),
            worker_terminal
                .and_then(|t| t.branch.as_deref())
                .map(|b| b.trim_start_matches("refs/heads/"))
                .unwrap_or("(unknown)"),
        )),
        None => facts.push("Dispatch: not dispatched yet".to_owned()),
    }
    facts.push(format!("Blocked by: {}", list_or_none(&blocked_by)));
    facts.push(format!("Blocks: {}", list_or_none(&blocks)));
    Ok(Filled {
        template: KICKOFF,
        subject: format!("task {task_id}"),
        result,
        facts,
        context,
        polled_at_ms: data.polled_at_ms,
    })
}

/// A dispatch that has ended (succeeded, failed, stopped).
fn is_finished(w: &Worker) -> bool {
    matches!(
        w.worker_state,
        Some(WorkerState::Succeeded | WorkerState::Failed | WorkerState::Stopped)
    ) || matches!(
        w.dispatch_status,
        Some(DispatchStatus::Completed | DispatchStatus::Failed | DispatchStatus::CircuitBroken)
    )
}

fn worker_state(w: &Worker) -> String {
    w.worker_state
        .as_ref()
        .map(|s| s.to_string())
        .or_else(|| w.dispatch_status.as_ref().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn value(value: String, from: &str) -> Resolution {
    Resolution::Value {
        value,
        from: from.to_owned(),
    }
}

/// A campaign name from a run objective: the part before its first `:` when that is short.
fn campaign_name(objective: &str) -> (String, &'static str) {
    match objective.split_once(':') {
        Some((head, _)) if !head.trim().is_empty() && head.chars().count() <= 60 => (
            head.trim().to_owned(),
            "run objective, up to its first colon",
        ),
        _ => (objective.to_owned(), "run objective"),
    }
}

/// Message types on the bus for `run_id`, with counts, most frequent first.
fn event_types(inbox: &[InboxMessage], run_id: &str) -> Vec<(String, usize)> {
    let home = format!("run:{run_id}");
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for m in inbox {
        if m.run_id.as_deref() == Some(run_id) || m.to_handle.as_deref() == Some(home.as_str()) {
            let t = m
                .message_type
                .as_ref()
                .map_or("untyped".to_owned(), |t| t.to_string());
            *counts.entry(t).or_insert(0) += 1;
        }
    }
    let mut out: Vec<_> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The `--types a,b,c` of the template's waiter command.
fn waited_types(template: &str) -> Option<String> {
    let rest = &template[template.find("--types ")? + "--types ".len()..];
    let types = rest.split_whitespace().next()?;
    Some(types.split(',').collect::<Vec<_>>().join(", "))
}

/// A ticket key such as `VROOM-1521`: `VROOM-` first, else any `ABC-123`.
pub fn ticket_id(text: &str) -> Option<String> {
    let keys: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter_map(|word| {
            let (prefix, number) = word.split_once('-')?;
            let ok = prefix.len() >= 2
                && prefix.chars().all(|c| c.is_ascii_uppercase())
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit());
            ok.then(|| word.to_owned())
        })
        .collect();
    keys.iter()
        .find(|k| k.starts_with("VROOM-"))
        .or(keys.first())
        .cloned()
}

/// `2026-09-25 14:03 UTC`.
pub fn utc(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|_| format!("{ms} ms"))
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn short(s: &str, max: usize) -> String {
    let s = collapse(s);
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

fn bracketed(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("<{}>", short(n, 48)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::playbook::BAKED_DIR;

    pub(crate) const RUN: &str = "run_1";
    pub(crate) const COORD: &str = "term_coord";
    pub(crate) const W1: &str = "term_w1";
    pub(crate) const W2: &str = "term_w2";
    const NOW: i64 = 1_790_352_833_794;

    /// A run whose declared coordinator is gone (the live one created its tasks), one live
    /// dispatch, one finished dispatch, a dependency chain and some bus traffic.
    pub(crate) fn fixture_clan() -> ClanData {
        let facts: ClanFacts = serde_json::from_value(json!({
            "polled_at_ms": NOW,
            "terminals": [
                {"handle": COORD, "worktreePath": "/u/orca/workspaces/depict/coord",
                 "branch": "refs/heads/main", "agentIdentity": "claude", "tabId": "tab1", "leafId": "l1"},
                {"handle": W1, "worktreePath": "/u/orca/workspaces/depict/track-a",
                 "branch": "refs/heads/karl/track-a", "agentIdentity": "claude"},
                {"handle": W2, "worktreePath": "/u/orca/workspaces/depict/track-c",
                 "agentIdentity": "codex"},
            ],
            "runs": [
                {"id": RUN, "objective": "VROOM-1521 wave: ship \"plan\" updates",
                 "coordinator_handle": "term_gone", "created_at": "2026-09-23T08:37:04Z"},
                {"id": "run_2", "objective": "other", "coordinator_handle": COORD},
            ],
            "workers": [
                {"dispatchId": "d1", "taskId": "t_a", "runId": RUN, "workerState": "ready",
                 "agentTerminalHandle": W1},
                {"dispatchId": "d0", "taskId": "t_c", "runId": RUN, "workerState": "succeeded",
                 "agentTerminalHandle": W2},
            ],
            "tasks": {RUN: [
                {"id": "t_a", "run_id": RUN, "task_title": "VROOM-1522 track A",
                 "spec": "Own the A path.\nNot B.", "status": "dispatched", "deps": "[]",
                 "dispatch_id": "d1", "created_by_terminal_handle": COORD,
                 "created_at": "2026-09-23T09:00:00Z"},
                {"id": "t_b", "run_id": RUN, "task_title": "track B", "status": "pending",
                 "deps": "[\"t_a\"]", "created_by_terminal_handle": COORD},
                {"id": "t_c", "run_id": RUN, "display_name": "track C", "status": "completed",
                 "deps": []},
            ]},
            "inbox": [
                {"id": "m1", "run_id": RUN, "type": "heartbeat", "from_handle": W1},
                {"id": "m2", "run_id": RUN, "type": "heartbeat", "from_handle": W1},
                {"id": "m3", "to_handle": "run:run_1", "type": "worker_done", "from_handle": W2},
                {"id": "m4", "run_id": "run_2", "type": "question", "from_handle": W2},
            ],
        }))
        .unwrap();
        facts.into_clan()
    }

    fn context() -> FillContext {
        FillContext {
            version: Version {
                commit: Some("1b0b685".into()),
                dirty: false,
                in_git: true,
                hash: "4acf3d6".into(),
            },
            source: "a test".into(),
            now_ms: NOW,
        }
    }

    fn template(file: &str) -> String {
        std::fs::read_to_string(std::path::Path::new(BAKED_DIR).join(file)).unwrap()
    }

    #[test]
    fn finds_every_placeholder_of_the_attendant_brief() {
        assert_eq!(
            placeholder_names(&template(ATTENDANT_BRIEF)),
            [
                "campaign",
                "coordinator terminal handle",
                "coordinator scratchpad path",
                "campaign objective",
                "id",
                "type",
                "track",
                "subject",
                "time",
                "event",
                "its worker terminal",
                "that line",
                "worker handle",
                "instruction",
                "one line",
            ]
        );
    }

    #[test]
    fn finds_every_placeholder_of_the_kickoff_including_multiline_ones() {
        let names = placeholder_names(&template(KICKOFF));
        assert_eq!(
            names,
            [
                "track name",
                "track",
                "repo",
                "repoId",
                "VROOM-###",
                "one-line title",
                "track-number",
                "standard | gated",
                "What this track owns, in two or three sentences. The decision to be made, not \
                 the keystrokes.",
                "the adjacent thing another track owns, or the follow-up that is deliberately \
                 not this PR.",
                "none",
                "task/PR, or \"none\"",
                "task, or \"none\"",
                "migrations, tables, stacked PRs — name the other track and the coordination \
                 rule, or \"nothing\"",
                "globs this track may edit",
                "globs other live tracks own, or \"nothing\"",
                "independent | after #N, rebase on it once it merges",
                "The facts the plan will rest on. The reviewer's first job is to check these \
                 against the code, not to accept them. If one is wrong, say so before \
                 implementing anything.",
                "fact",
                "`just check`, or this repo's actual gate",
                "what a working end-to-end path looks like here — the real request, the real \
                 data, the real page. Name it concretely.",
                "Anything already known: prior attempts, related PRs, telemetry links, the \
                 reason this is scoped the way it is.",
            ]
        );
    }

    #[test]
    fn placeholder_discovery_skips_tags_urls_comparisons_and_the_ellipsis() {
        let text = "fill every <…> and <...>; <!-- c --> </b> <https://x.y> a < b > c \
                    <-x> <ok> <a\nb> <unclosed";
        assert_eq!(placeholder_names(text), ["ok", "a b"]);
        let p = &placeholders("x <a\nb> y")[0];
        assert_eq!((p.raw.as_str(), p.start, p.end), ("a\nb", 2, 7));
    }

    #[test]
    fn fill_replaces_marks_and_keeps_placeholders() {
        let r = fill("<a> <b> <c>\nRUN=<c> <a>", |name, before| match name {
            "a" => value("A".into(), "src"),
            "c" if before.ends_with("RUN=") => value("R".into(), "run"),
            "c" => Resolution::Runtime,
            _ => Resolution::Missing,
        });
        assert_eq!(r.text, "A <b: FILL IN> <c>\nRUN=R A");
        assert_eq!(r.missing, ["b"]);
        assert_eq!(r.runtime, ["c"]);
        assert_eq!(r.filled.len(), 2, "duplicates collapse: {:?}", r.filled);
    }

    #[test]
    fn attendant_brief_fills_from_the_run_and_marks_the_rest() {
        let data = fixture_clan();
        let t = template(ATTENDANT_BRIEF);
        let f = attendant_brief(&t, &data, RUN, None, context()).unwrap();
        let body = &f.result.text;
        assert!(body.starts_with("# Attendant brief — VROOM-1521 wave (fill every <…>"));
        assert!(body.contains(&format!("- Coordinator terminal: `{COORD}`")));
        assert!(body.contains("`<coordinator scratchpad path: FILL IN>` — `SCRATCH` below."));
        assert!(body.contains("--objective \"VROOM-1521 wave: ship \\\"plan\\\" updates\" --json"));
        assert!(body.contains("`RUN_ID=run_1`"));
        assert!(body.contains("\"<type> from <track>: <subject> — msg <id>\""));
        assert_eq!(f.result.missing, ["coordinator scratchpad path"]);
        assert!(f.result.runtime.contains(&"worker handle".to_owned()));
        let coordinator = f
            .result
            .filled
            .iter()
            .find(|e| e.placeholder == "coordinator terminal handle")
            .unwrap();
        assert!(coordinator.from.contains("not live"), "{coordinator:?}");

        let header = f.header();
        let lines: Vec<&str> = header.lines().collect();
        assert_eq!(
            lines[0],
            "Stronghold pre-filled ATTENDANT-BRIEF-TEMPLATE.md for run run_1 · 2026-09-25 16:13 UTC"
        );
        assert_eq!(lines[1], "Playbook version: 1b0b685 (clean) · hash 4acf3d6");
        assert!(
            header.contains("Left to fill (marked \"FILL IN\"): <coordinator scratchpad path>")
        );
        assert!(
            header.contains("Run: run_1, created 2026-09-23 08:37 UTC · 3 tasks · 2 dispatches")
        );
        assert!(header.contains("- d1 · worker term_w1 · task t_a \"VROOM-1522 track A\" · ready"));
        assert!(header.contains("Finished dispatches: 1 (succeeded 1)"));
        assert!(
            lines.contains(&"Bus event types on this run so far: heartbeat ×2, worker_done ×1"),
            "{header}"
        );
        assert!(
            header.contains("The waiter forwards: worker_done, escalation, question, merge_ready")
        );
        assert!(f.text().starts_with(&header));
        assert!(
            f.text()
                .contains(&format!("{CUT_LINE}\n\n# Attendant brief"))
        );
        assert_eq!(
            f.to_json()["missing"],
            json!(["coordinator scratchpad path"])
        );
    }

    #[test]
    fn attendant_brief_names_the_real_coordinator_not_the_attendant() {
        let mut data = fixture_clan();
        data.terminals.push(Terminal {
            handle: "term_att".into(),
            title: Some("✳ wave-ATTENDANT brief".into()),
            ..Terminal::default()
        });
        data.runs[0].coordinator_handle = Some("term_att".into());
        // Round-trips through the wire format with the transcript forwards.
        data.forwards.insert(
            "term_att".into(),
            vec![crate::clan::Forward {
                target: COORD.into(),
                at_ms: Some(NOW),
            }],
        );
        let wire = serde_json::to_value(ClanFacts::from_clan(&data)).unwrap();
        let data = serde_json::from_value::<ClanFacts>(wire)
            .unwrap()
            .into_clan();
        assert_eq!(data.run_roles(RUN).attendant.as_deref(), Some("term_att"));

        let f = attendant_brief(&template(ATTENDANT_BRIEF), &data, RUN, None, context()).unwrap();
        assert!(
            f.result
                .text
                .contains(&format!("- Coordinator terminal: `{COORD}`"))
        );
        let coordinator = f
            .result
            .filled
            .iter()
            .find(|e| e.placeholder == "coordinator terminal handle")
            .unwrap();
        assert!(coordinator.from.contains("attendant"), "{coordinator:?}");
    }

    #[test]
    fn attendant_brief_with_a_scratchpad_leaves_nothing_to_fill() {
        let data = fixture_clan();
        let t = template(ATTENDANT_BRIEF);
        let f = attendant_brief(&t, &data, RUN, Some("/tmp/s"), context()).unwrap();
        assert!(f.result.missing.is_empty());
        assert!(f.result.text.contains("- Scratchpad: `/tmp/s`"));
        assert!(f.header().contains("Left to fill: nothing"));
    }

    #[test]
    fn attendant_brief_for_an_unknown_run_is_an_error() {
        let e = attendant_brief("x", &fixture_clan(), "run_x", None, context()).unwrap_err();
        assert!(e.contains("run_x"), "{e}");
    }

    #[test]
    fn kickoff_fills_from_the_task_its_dispatch_and_deps() {
        let data = fixture_clan();
        let t = template(KICKOFF);
        let f = kickoff(&t, &data, "t_a", context()).unwrap();
        let body = &f.result.text;
        assert!(body.starts_with("# KICKOFF — VROOM-1522 track A\n"));
        assert!(body.contains("- **Repo:** depict (`orca repo list` → `id:<repoId: FILL IN>`)"));
        assert!(body.contains("- **Ticket:** VROOM-1522 — VROOM-1522 track A"));
        assert!(body.contains("<track-number: FILL IN>00–<track-number: FILL IN>99"));
        assert!(body.contains("## Scope\n\nOwn the A path.\nNot B.\n"));
        assert!(body.contains("- Blocked by: none\n"));
        assert!(body.contains("- Blocks: t_b \"track B\" (pending)\n"));
        assert!(body.contains("deliberately not this PR.: FILL IN>"));
        assert!(body.contains("- **Risk:** <standard | gated: FILL IN>"));
        assert!(body.contains("**Open decisions:** <none: FILL IN>"));
        assert!(f.result.missing.contains(&"fact".to_owned()));
        let header = f.header();
        assert!(header.contains("Task: t_a \"VROOM-1522 track A\" · dispatched · run run_1"));
        assert!(header.contains(
            "Dispatch: d1 · worker term_w1 · worktree /u/orca/workspaces/depict/track-a · \
             branch karl/track-a"
        ));
        assert!(header.contains("- <repo> = depict (the worker's worktree)"));

        let f = kickoff(&t, &data, "t_b", context()).unwrap();
        assert!(
            f.result
                .text
                .contains("- Blocked by: t_a \"VROOM-1522 track A\" (dispatched)")
        );
        assert!(f.result.text.contains("- Blocks: none"));
        assert!(f.result.text.contains("<VROOM-###: FILL IN>"));
        assert!(f.header().contains("Dispatch: not dispatched yet"));
        assert!(
            f.header()
                .contains("- <repo> = depict (the coordinator's worktree)")
        );
        assert!(
            f.result.text.contains("<What this track owns"),
            "no spec: marked"
        );

        let e = kickoff(&t, &data, "t_zzz", context()).unwrap_err();
        assert!(e.contains("t_zzz"), "{e}");
    }

    #[test]
    fn clan_facts_round_trip_through_json_without_previews_or_bodies() {
        let mut data = fixture_clan();
        data.terminals[0].preview = Some("secret screen".into());
        data.inbox[0].body = Some("long body".into());
        let facts = ClanFacts::from_clan(&data);
        let json = serde_json::to_value(&facts).unwrap();
        assert!(!json.to_string().contains("secret screen"));
        assert!(!json.to_string().contains("long body"));
        let back: ClanFacts = serde_json::from_value(json).unwrap();
        assert_eq!(back, facts);
        assert_eq!(back.tasks[RUN][1].deps, ["t_a"]);
    }

    #[test]
    fn ticket_ids_prefer_vroom() {
        assert_eq!(
            ticket_id("HACK-7 then VROOM-12: x").as_deref(),
            Some("VROOM-12")
        );
        assert_eq!(ticket_id("(HACK-76) looks").as_deref(), Some("HACK-76"));
        assert_eq!(ticket_id("hack-77-admin-speed A-1 X-y"), None);
    }
}
