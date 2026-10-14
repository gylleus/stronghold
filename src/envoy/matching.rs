//! Which of Karl's dwarves an inbound event concerns: thread ownership first, then shared refs
//! (ticket keys like `ABC-123`, `repo#45`, PR links, branch names), then distinctive label keywords.

use std::collections::{BTreeMap, BTreeSet};

use super::channel::InboundEvent;

/// A live dwarf as the matcher sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dwarf {
    /// Node id, e.g. `term:term_…`; what `envoy_relay` takes as `to`.
    pub id: String,
    pub label: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    /// Run objective, task titles, PR titles: searched for refs only.
    pub context: Vec<String>,
    /// PRs it marked ready, as `repo#N`.
    pub prs: Vec<String>,
    /// Slack threads it owns (`conversation:ts`), from reach_out and adopted threads.
    pub threads: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub label: String,
    pub why: String,
    score: u32,
}

const MAX_CANDIDATES: usize = 3;

/// Words too common in this clan's labels and Slack to identify a dwarf on their own.
const COMMON: &[&str] = &[
    "about",
    "after",
    "agent",
    "agents",
    "card",
    "cards",
    "check",
    "config",
    "customer",
    "data",
    "deploy",
    "envoy",
    "finish",
    "first",
    "fix",
    "fixes",
    "from",
    "gylleus",
    "headless",
    "index",
    "ingestion",
    "into",
    "issue",
    "issues",
    "karl",
    "kickoff",
    "live",
    "main",
    "merchant",
    "merge",
    "page",
    "plan",
    "pr",
    "prod",
    "product",
    "products",
    "prs",
    "read",
    "recommendation",
    "recommendations",
    "recs",
    "release",
    "review",
    "rework",
    "run",
    "save",
    "search",
    "shop",
    "slack",
    "staging",
    "store",
    "storefront",
    "task",
    "test",
    "tests",
    "that",
    "the",
    "theme",
    "this",
    "thread",
    "ticket",
    "update",
    "with",
    "work",
    "worker",
    "write",
];

/// Dwarves `event` likely concerns, best first. `thread` holds the other inbox events in its
/// thread, whose refs count too (a reply rarely repeats the PR number). `heads` maps `repo#N`
/// to the PR's head branch.
pub fn candidates(
    event: &InboundEvent,
    thread: &[&InboundEvent],
    dwarves: &[Dwarf],
    heads: &BTreeMap<String, String>,
) -> Vec<Candidate> {
    let root = format!(
        "{}:{}",
        event.conversation,
        event.thread_ts.as_deref().unwrap_or(&event.ts)
    );
    let own = event.text.to_lowercase();
    let mut text = own.clone();
    for e in thread {
        text.push('\n');
        text.push_str(&e.text.to_lowercase());
    }
    let refs = refs(&text);
    let words = words(&own);
    let mut out: Vec<Candidate> = dwarves
        .iter()
        .filter_map(|dwarf| {
            let (score, why) = score(dwarf, &root, &refs, &words, &text, heads)?;
            Some(Candidate {
                id: dwarf.id.clone(),
                label: dwarf.label.clone(),
                why,
                score,
            })
        })
        .collect();
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    out.truncate(MAX_CANDIDATES);
    out
}

fn score(
    dwarf: &Dwarf,
    root: &str,
    refs: &BTreeSet<String>,
    words: &BTreeSet<String>,
    text: &str,
    heads: &BTreeMap<String, String>,
) -> Option<(u32, String)> {
    if dwarf.threads.iter().any(|t| t == root) {
        return Some((100, "its thread".into()));
    }
    let mut own = BTreeSet::new();
    for s in std::iter::once(&dwarf.label)
        .chain(dwarf.branch.iter())
        .chain(&dwarf.context)
        .chain(&dwarf.prs)
    {
        own.extend(self::refs(&s.to_lowercase()));
    }
    let bare: BTreeSet<String> = own
        .iter()
        .filter_map(|r| r.split_once('#').map(|(_, n)| format!("#{n}")))
        .collect();
    if let Some(r) = refs.iter().find(|r| own.contains(*r)) {
        return Some((50, r.clone()));
    }
    if let Some(branch) = dwarf.branch.as_deref()
        && let Some(r) = refs
            .iter()
            .find(|r| heads.get(*r).is_some_and(|h| h == branch))
    {
        return Some((50, format!("{r} is its branch")));
    }
    if let Some(r) = refs
        .iter()
        .find(|r| r.starts_with('#') && bare.contains(*r))
    {
        return Some((30, r.clone()));
    }
    if let Some(branch) = dwarf.branch.as_deref().filter(|b| b.len() >= 8)
        && text.contains(&branch.to_lowercase())
    {
        return Some((50, branch.into()));
    }
    let keys: BTreeSet<String> = words_of(&dwarf.label)
        .into_iter()
        .chain(dwarf.branch.as_deref().map(words_of).unwrap_or_default())
        .filter(|w| dwarf.repo.as_deref() != Some(w))
        .collect();
    let hits: Vec<&String> = keys.intersection(words).collect();
    if hits.len() >= 2 {
        let why = hits
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("+");
        return Some((10 * hits.len() as u32, why));
    }
    None
}

/// `abc-123` keys in lowercase `text`: a word of two or more letters, a dash and digits not
/// followed by a letter, digit or `.` (so `gpt-5.6` and `myr-8hj` are not tickets).
fn ticket_keys(lower: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, _) in lower.match_indices('-') {
        let prefix: String = lower[..i]
            .chars()
            .rev()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let n: String = lower[i + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let next = lower[i + 1 + n.len()..].chars().next();
        if prefix.len() >= 2
            && prefix.chars().all(|c| c.is_ascii_lowercase())
            && !n.is_empty()
            && !next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.')
        {
            out.insert(format!("{prefix}-{n}"));
        }
    }
    out
}

/// Ticket keys (`abc-123`), `repo#45` and bare `#45` refs, lowercase. PR links become `repo#45`.
pub fn refs(text: &str) -> BTreeSet<String> {
    let lower = text.to_lowercase();
    let mut out = ticket_keys(&lower);
    for (i, _) in lower.match_indices("/pull/") {
        let repo = lower[..i].rsplit('/').next().unwrap_or_default();
        let n: String = lower[i + 6..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !repo.is_empty() && !n.is_empty() {
            out.insert(format!("{repo}#{n}"));
        }
    }
    for (i, _) in lower.match_indices('#') {
        let n: String = lower[i + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if n.is_empty() {
            continue;
        }
        let repo: String = lower[..i]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if repo.is_empty() {
            out.insert(format!("#{n}"));
        } else {
            out.insert(format!("{repo}#{n}"));
        }
    }
    out
}

fn words(lower: &str) -> BTreeSet<String> {
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(singular)
        .collect()
}

/// `replicas` → `replica`, so a plural and its singular are one word.
fn singular(w: &str) -> String {
    match w.strip_suffix('s') {
        Some(stem) if stem.len() >= 4 && !stem.ends_with('s') => stem.into(),
        _ => w.into(),
    }
}

/// Distinctive words of a label or branch.
fn words_of(s: &str) -> BTreeSet<String> {
    words(&s.to_lowercase())
        .into_iter()
        .filter(|w| w.len() >= 4 && !COMMON.contains(&w.as_str()))
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

/// ` — dwarves: Label (id, why); …` or ` — no dwarf match`, appended to an event line.
pub fn annotation(candidates: &[Candidate]) -> String {
    if candidates.is_empty() {
        return " — no dwarf match".into();
    }
    let list: Vec<String> = candidates
        .iter()
        .map(|c| {
            let label: String = c.label.chars().take(40).collect();
            format!("{label} ({}, {})", c.id, c.why)
        })
        .collect();
    format!(" — dwarves: {}", list.join("; "))
}

#[cfg(test)]
mod tests;
