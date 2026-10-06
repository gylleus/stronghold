//! Which of Karl's ants an inbound event concerns: thread ownership first, then shared refs
//! (`VROOM-123`, `repo#45`, PR links, branch names), then distinctive label keywords.

use std::collections::BTreeSet;

use super::channel::InboundEvent;

/// A live ant as the matcher sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ant {
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

/// Words too common in this fleet's labels and Slack to identify an ant on their own.
const COMMON: &[&str] = &[
    "about", "after", "agent", "agents", "card", "cards", "check", "config", "data", "deploy",
    "envoy", "finish", "first", "fix", "fixes", "from", "gylleus", "into", "issue", "issues",
    "karl", "kickoff", "live", "main", "merge", "page", "plan", "prod", "product", "products",
    "recs", "release", "rework", "review", "run", "save", "search", "staging", "task", "test",
    "tests", "that", "the", "theme", "this", "update", "with", "work", "worker",
];

/// Ants `event` likely concerns, best first. `thread` holds the other inbox events in its
/// thread, whose refs count too (a reply rarely repeats the PR number).
pub fn candidates(event: &InboundEvent, thread: &[&InboundEvent], ants: &[Ant]) -> Vec<Candidate> {
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
    let mut out: Vec<Candidate> = ants
        .iter()
        .filter_map(|ant| {
            let (score, why) = score(ant, &root, &refs, &words, &text)?;
            Some(Candidate {
                id: ant.id.clone(),
                label: ant.label.clone(),
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
    ant: &Ant,
    root: &str,
    refs: &BTreeSet<String>,
    words: &BTreeSet<String>,
    text: &str,
) -> Option<(u32, String)> {
    if ant.threads.iter().any(|t| t == root) {
        return Some((100, "its thread".into()));
    }
    let mut own = BTreeSet::new();
    for s in std::iter::once(&ant.label)
        .chain(ant.branch.iter())
        .chain(&ant.context)
        .chain(&ant.prs)
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
    if let Some(r) = refs
        .iter()
        .find(|r| r.starts_with('#') && bare.contains(*r))
    {
        return Some((30, r.clone()));
    }
    if let Some(branch) = ant.branch.as_deref().filter(|b| b.len() >= 8)
        && text.contains(&branch.to_lowercase())
    {
        return Some((50, branch.into()));
    }
    let keys: BTreeSet<String> = words_of(&ant.label)
        .into_iter()
        .chain(ant.branch.as_deref().map(words_of).unwrap_or_default())
        .filter(|w| ant.repo.as_deref() != Some(w))
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

/// `vroom-123`, `repo#45` and bare `#45` refs, lowercase. PR links become `repo#45`.
pub fn refs(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let lower = text.to_lowercase();
    for (i, _) in lower.match_indices("vroom-") {
        let n: String = lower[i + 6..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !n.is_empty() {
            out.insert(format!("vroom-{n}"));
        }
    }
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
        .map(str::to_owned)
        .collect()
}

/// Distinctive words of a label or branch.
fn words_of(s: &str) -> BTreeSet<String> {
    words(&s.to_lowercase())
        .into_iter()
        .filter(|w| w.len() >= 4 && !COMMON.contains(&w.as_str()))
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

/// ` — ants: Label (id, why); …` or ` — no ant match`, appended to an event line.
pub fn annotation(candidates: &[Candidate]) -> String {
    if candidates.is_empty() {
        return " — no ant match".into();
    }
    let list: Vec<String> = candidates
        .iter()
        .map(|c| {
            let label: String = c.label.chars().take(40).collect();
            format!("{label} ({}, {})", c.id, c.why)
        })
        .collect();
    format!(" — ants: {}", list.join("; "))
}

#[cfg(test)]
mod tests;
