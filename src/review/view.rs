//! What the queue shows: one [`ReviewRow`] per listed PR with its agent resolved to a farm
//! node, for the Queen panel, the control API (`review_queue`) and the MCP tool.

use serde::Serialize;
use serde_json::{Value, json};

use crate::model::{FleetSnapshot, NodeId, NodeKind};
use crate::orca::Terminal;
use crate::review::github::{Ci, PrState};
use crate::review::mark::Origin;
use crate::review::queue::{Entry, ReviewQueue};

/// The farm node of the agent that marked a PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRef {
    pub id: NodeId,
    pub label: String,
    pub live: bool,
}

/// The node of the agent with Orca identity `origin`: its terminal handle, else the terminal
/// whose `<tabId>:<leafId>` is its pane key, else the only agent terminal in its tab.
pub fn resolve_agent(
    origin: &Origin,
    snapshot: &FleetSnapshot,
    terminals: &[Terminal],
) -> Option<AgentRef> {
    let handle = origin
        .terminal_handle
        .clone()
        .or_else(|| {
            let key = origin.pane_key.as_deref()?;
            terminals
                .iter()
                .find(|t| {
                    matches!((&t.tab_id, &t.leaf_id), (Some(tab), Some(leaf))
                        if key == format!("{tab}:{leaf}"))
                })
                .map(|t| t.handle.clone())
        })
        .or_else(|| {
            let tab = origin
                .tab_id
                .as_deref()
                .or_else(|| origin.pane_key.as_deref()?.split(':').next())?;
            let mut in_tab = terminals
                .iter()
                .filter(|t| t.tab_id.as_deref() == Some(tab) && t.agent_identity.is_some());
            let only = in_tab.next()?;
            in_tab.next().is_none().then(|| only.handle.clone())
        })?;
    let node = snapshot
        .agent_nodes()
        .filter(|n| n.kind != NodeKind::Subagent)
        .filter(|n| n.terminal_handle.as_deref() == Some(handle.as_str()))
        .max_by_key(|n| (n.live, n.kind == NodeKind::Coordinator))?;
    Some(AgentRef {
        id: node.id.clone(),
        label: node.label.clone(),
        live: node.live,
    })
}

/// One listed PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewRow {
    pub url: String,
    /// `owner/repo`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub summary: Option<String>,
    pub agent: Option<AgentRef>,
    /// The marking terminal's handle, when it isn't a node of the farm.
    pub agent_handle: Option<String>,
    /// `tool`, `cli` or `bus`.
    pub source: &'static str,
    pub marked_ms: i64,
    /// `None` until GitHub was read.
    pub state: Option<PrState>,
    pub ci: Option<Ci>,
    pub head_sha: Option<String>,
    pub ankra_verdict: Option<String>,
    /// `Some(false)`: Ankra reviewed an older commit.
    pub ankra_on_head: Option<bool>,
    /// `ready`, `not_ready`.
    pub marker: Option<&'static str>,
    pub marker_on_head: Option<bool>,
    pub review_decision: Option<String>,
    /// Why GitHub could not be read for this PR.
    pub error: Option<String>,
    /// Reviewed by Karl, then pushed to.
    pub updated_since_review: bool,
}

impl ReviewRow {
    pub fn from_entry(entry: &Entry, agent: Option<AgentRef>) -> Self {
        let status = entry.status.as_ref();
        let agent_handle = match &agent {
            Some(_) => None,
            None => entry.origin.terminal_handle.clone(),
        };
        Self {
            url: entry.url(),
            repo: format!("{}/{}", entry.pr.owner, entry.pr.repo),
            number: entry.pr.number,
            title: entry.title().to_owned(),
            summary: entry.summary.clone(),
            agent,
            agent_handle,
            source: entry.source.name(),
            marked_ms: entry.marked_ms,
            state: status.map(|s| s.state),
            ci: status.map(|s| s.ci),
            head_sha: status.map(|s| s.head_sha.clone()).filter(|h| !h.is_empty()),
            ankra_verdict: status
                .and_then(|s| s.ankra.as_ref())
                .map(|a| a.verdict.clone().unwrap_or_else(|| "no verdict".into())),
            ankra_on_head: status.and_then(|s| s.ankra_on_head()),
            marker: status
                .and_then(|s| s.marker.as_ref())
                .map(|m| match m.kind {
                    crate::review::github::MarkerKind::Ready => "ready",
                    crate::review::github::MarkerKind::NotReady => "not_ready",
                }),
            marker_on_head: status.and_then(|s| s.marker_on_head()),
            review_decision: status.and_then(|s| s.review_decision.clone()),
            error: entry.error.clone(),
            updated_since_review: entry.updated_since_review(),
        }
    }

    /// `Ankra comments`, `Ankra looks good (stale)`, `no Ankra review`; `None` before GitHub
    /// was read.
    pub fn ankra_text(&self) -> Option<String> {
        self.state?;
        Some(match &self.ankra_verdict {
            None => "no Ankra review".into(),
            Some(v) => {
                let stale = if self.ankra_on_head == Some(false) {
                    " (stale)"
                } else {
                    ""
                };
                format!("Ankra {v}{stale}")
            }
        })
    }

    /// `✅ ready`, `⏸ not ready (stale)`, `no marker`; `None` before GitHub was read.
    pub fn marker_text(&self) -> Option<String> {
        self.state?;
        let base = match self.marker {
            None => return Some("no marker".into()),
            Some("ready") => "✅ ready",
            Some(_) => "⏸ not ready",
        };
        let stale = if self.marker_on_head == Some(false) {
            " (stale)"
        } else {
            ""
        };
        Some(format!("{base}{stale}"))
    }

    /// `Draft`, `Merged`, `Closed`; `None` for open (the normal case) and unknown.
    pub fn state_note(&self) -> Option<&'static str> {
        match self.state? {
            PrState::Open => None,
            PrState::Draft => Some("Draft"),
            PrState::Merged => Some("Merged"),
            PrState::Closed => Some("Closed"),
        }
    }

    /// The agent's label, its bare handle, or `unknown agent`.
    pub fn agent_text(&self) -> String {
        match (&self.agent, &self.agent_handle) {
            (Some(a), _) => a.label.clone(),
            (None, Some(h)) => h.clone(),
            (None, None) => "unknown agent".into(),
        }
    }
}

/// The listed rows, newest mark first, each with its agent resolved by `resolve`.
pub fn rows(
    queue: &ReviewQueue,
    now_ms: i64,
    mut resolve: impl FnMut(&Origin) -> Option<AgentRef>,
) -> Vec<ReviewRow> {
    queue
        .visible(now_ms)
        .into_iter()
        .map(|e| ReviewRow::from_entry(e, resolve(&e.origin)))
        .collect()
}

/// The control API's `review_queue` result.
pub fn queue_json(rows: &[ReviewRow], github_unavailable: Option<&str>) -> Value {
    json!({
        "count": rows.len(),
        "github": {
            "available": github_unavailable.is_none(),
            "error": github_unavailable,
        },
        "entries": rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::FleetData;
    use crate::orca::{AgentIdentity, Terminal};

    fn terminals() -> Vec<Terminal> {
        vec![
            Terminal {
                handle: "term_a".into(),
                tab_id: Some("tab1".into()),
                leaf_id: Some("leaf1".into()),
                agent_identity: Some(AgentIdentity::Claude),
                ..Terminal::default()
            },
            Terminal {
                handle: "term_b".into(),
                tab_id: Some("tab2".into()),
                leaf_id: Some("leaf2".into()),
                agent_identity: Some(AgentIdentity::Claude),
                ..Terminal::default()
            },
        ]
    }

    fn snapshot() -> FleetSnapshot {
        FleetSnapshot::build(
            &FleetData {
                terminals: terminals(),
                ..FleetData::default()
            },
            0,
        )
    }

    #[test]
    fn agents_resolve_by_handle_pane_key_or_tab() {
        let (snap, terms) = (snapshot(), terminals());
        let by = |origin: Origin| resolve_agent(&origin, &snap, &terms).map(|a| a.id);
        assert_eq!(
            by(Origin {
                terminal_handle: Some("term_a".into()),
                ..Origin::default()
            }),
            Some(NodeId::terminal("term_a"))
        );
        assert_eq!(
            by(Origin {
                pane_key: Some("tab2:leaf2".into()),
                ..Origin::default()
            }),
            Some(NodeId::terminal("term_b"))
        );
        assert_eq!(
            by(Origin {
                tab_id: Some("tab1".into()),
                ..Origin::default()
            }),
            Some(NodeId::terminal("term_a"))
        );
        assert_eq!(
            by(Origin {
                pane_key: Some("tab2:other-leaf".into()),
                ..Origin::default()
            }),
            Some(NodeId::terminal("term_b")),
            "an unknown leaf falls back to its tab"
        );
        assert_eq!(
            by(Origin {
                terminal_handle: Some("term_gone".into()),
                ..Origin::default()
            }),
            None
        );
        assert_eq!(by(Origin::default()), None);
    }
}
