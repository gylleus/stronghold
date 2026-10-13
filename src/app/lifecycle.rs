//! The app side of agents asking Karl a question (`ask`) and ending their own sessions
//! (`finish`): see [`crate::ask`] and [`crate::finish`].

use serde_json::{Value, json};

use super::control::plan_json;
use super::{StrongholdApp, now_ms};
use crate::ask::{AskRequest, PendingAsk};
use crate::close::{CloseOptions, ClosePlan, plan_close};
use crate::control::view::kind_name;
use crate::control::{ControlError, ErrorCode};
use crate::finish::{CLOSE_DELAY_MS, FinishRequest, FinishedSession, clan_problems, clip};
use crate::model::{Node, NodeKind};
use crate::notify::agent::outcome_reply;
use crate::notify::{Incoming, Severity, Source, target_key};
use crate::review::mark::Origin;

/// Longest notification title built from a question or summary.
const TITLE_CHARS: usize = 100;

/// A self-finished terminal to close once its agent has had the reply.
pub(super) struct PendingClose {
    due_ms: i64,
    plan: ClosePlan,
    labels: Vec<String>,
}

impl StrongholdApp {
    /// The caller's node, from its Orca identity.
    pub(super) fn caller(&self, origin: &Origin) -> Option<&Node> {
        let agent = self.review_agent(origin)?;
        self.snapshot.as_ref()?.node(&agent.id)
    }

    /// The control API's `ask`: notifies Karl of the question and shows the caller waiting on
    /// him until he answers, except the Envoy which keeps taking batches.
    pub(super) fn control_ask(&mut self, ask: AskRequest) -> Result<Value, ControlError> {
        let now = now_ms();
        let node = self.caller(&ask.origin).cloned();
        let (incoming, waiting) = prepare_ask(&mut self.asks, node.as_ref(), &ask, now);
        let outcome = self.raise(incoming);
        if waiting {
            self.overlays_changed = true;
            if let Some(node) = &node {
                self.notifications
                    .hold_cooldown(&target_key(&node.id, node.terminal_handle.as_deref()), now);
            }
        }
        self.publish_control_status();
        let quiet = self.whip_window.notifications().quiets(ask.severity);
        let shown = node.as_ref().map(|n| (n.id.as_str(), n.label.as_str()));
        let mut reply = outcome_reply(&outcome, ask.severity, shown, quiet);
        reply["waiting"] = json!(waiting);
        reply["nonblocking"] = json!(node.as_ref().is_some_and(|n| n.kind == NodeKind::Envoy));
        Ok(reply)
    }

    /// The control API's `finish`: checks the clan's guards, then records the outcome,
    /// notifies Karl and closes the caller's terminal shortly after replying. With `dry_run`
    /// only reports the caller's node and the problems.
    pub(super) fn control_finish(&mut self, req: FinishRequest) -> Result<Value, ControlError> {
        let now = now_ms();
        let node = self.caller(&req.origin).cloned().ok_or_else(|| {
            ControlError::not_found(
                "Stronghold can't tell which terminal you are (no Orca terminal identity that \
                 matches a node), so it can't close you",
            )
        })?;
        let open_prs: Vec<String> = self
            .review_rows(now)
            .into_iter()
            .filter(|r| r.agent.as_ref().is_some_and(|a| a.id == node.id))
            .filter(|r| !r.state.is_some_and(|s| s.is_gone()))
            .map(|r| r.url)
            .collect();
        let snapshot = self.snapshot.as_ref().ok_or_else(|| {
            ControlError::new(ErrorCode::Unavailable, "the app has no clan data yet")
        })?;
        let (refusal, problems) = clan_problems(&node, snapshot, &open_prs);
        let agent = json!({
            "id": node.id,
            "label": node.label,
            "kind": kind_name(node.kind),
            "terminal_handle": node.terminal_handle,
            "worktree": node.worktree_path,
            "branch": node.branch,
            "run_id": node.run_id,
        });
        if req.dry_run {
            return Ok(json!({
                "dry_run": true,
                "agent": agent,
                "refused": refusal,
                "problems": problems,
            }));
        }
        if let Some(reason) = refusal {
            return Err(ControlError::new(ErrorCode::Refused, reason));
        }
        if !problems.is_empty() && !req.force {
            return Err(ControlError::new(
                ErrorCode::Refused,
                format!("not finishing: {}", problems.join("; ")),
            ));
        }
        let terminals = self.data.as_ref().map_or(&[][..], |d| &d.terminals[..]);
        let plan = plan_close(&node, snapshot, terminals, CloseOptions::default());
        if let Some(reason) = &plan.refused {
            return Err(ControlError::new(ErrorCode::Refused, reason.clone()));
        }
        let labels = crate::ui::close::step_labels(&plan, Some(snapshot));
        let described = plan_json(&plan, &labels);

        let mut forced = req.forced.clone();
        forced.extend(problems);
        let record = FinishedSession {
            node_id: node.id.to_string(),
            handle: node.terminal_handle.clone(),
            label: node.label.clone(),
            outcome: req.outcome,
            approval: req.approval.unwrap_or_default(),
            summary: req.summary.clone(),
            pr_url: req.pr_url.clone(),
            forced: forced.clone(),
            worktree_path: node.worktree_path.clone(),
            branch: node.branch.clone(),
            at_ms: now,
        };
        log::info!(
            "{} finished itself ({}): {}",
            node.label,
            req.outcome.name(),
            req.summary
        );
        self.finished.record(record.clone());
        if let Some(handle) = &node.terminal_handle {
            self.asks.remove(handle);
        }
        self.finish_closes.push(PendingClose {
            due_ms: now + CLOSE_DELAY_MS,
            plan,
            labels,
        });
        self.overlays_changed = true;

        let severity = if forced.is_empty() {
            Severity::Info
        } else {
            Severity::Warn
        };
        let mut message = req.summary.clone();
        if let Some(wt) = &node.worktree_path {
            message.push_str(&format!("\nWorktree kept: {wt}"));
        }
        if !forced.is_empty() {
            message.push_str(&format!("\nLeft despite: {}", forced.join("; ")));
        }
        let outcome = self.raise(Incoming {
            source: Source::Agent,
            severity,
            title: format!(
                "{}: {}",
                req.outcome.verb(),
                clip(&req.summary, TITLE_CHARS)
            ),
            message,
            url: req.pr_url.clone(),
            target: Some(node.id.to_string()),
            sender: node.terminal_handle.clone(),
            agent_label: Some(node.label.clone()),
        });
        self.publish_control_status();
        Ok(json!({
            "dry_run": false,
            "agent": agent,
            "finished": record,
            "closing_in_ms": CLOSE_DELAY_MS,
            "plan": described,
            "notification": outcome.name(),
        }))
    }

    /// Per frame: closes the self-finished terminals whose reply has had time to arrive.
    pub(super) fn run_finish_closes(&mut self) {
        if self.finish_closes.is_empty() {
            return;
        }
        let now = now_ms();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.finish_closes)
            .into_iter()
            .partition(|c| c.due_ms <= now);
        self.finish_closes = later;
        for close in due {
            log::info!("closing self-finished session: {:?}", close.plan.steps);
            let ids: Vec<_> = close
                .plan
                .actions()
                .into_iter()
                .map(|a| self.actions.submit(a))
                .collect();
            self.close.started(close.plan, close.labels, &ids, now);
        }
    }
}

/// Records the question separately from notification delivery so the inbox and node state
/// share the same caller identity.
pub(super) fn prepare_ask(
    asks: &mut crate::ask::Asks,
    node: Option<&Node>,
    ask: &AskRequest,
    now: i64,
) -> (Incoming, bool) {
    let mut message = ask.question.clone();
    if !ask.context.is_empty() {
        message = format!("{message}\n\n{}", ask.context);
    }
    let incoming = Incoming {
        source: Source::Agent,
        severity: ask.severity,
        title: format!("Question: {}", clip(&ask.question, TITLE_CHARS)),
        message,
        url: ask.url.clone(),
        target: node
            .map(|n| n.id.to_string())
            .or_else(|| ask.origin.terminal_handle.clone()),
        sender: ask
            .origin
            .terminal_handle
            .clone()
            .or_else(|| ask.origin.pane_key.clone()),
        agent_label: node.map(|n| n.label.clone()),
    };
    let handle = node
        .filter(|n| n.kind != NodeKind::Envoy)
        .and_then(|n| n.terminal_handle.as_deref());
    if let Some(handle) = handle {
        asks.insert(
            handle,
            PendingAsk {
                question: ask.question.clone(),
                at_ms: now,
            },
        );
    }
    (incoming, handle.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::Asks;
    use crate::clan::ClanData;
    use crate::model::{ClanSnapshot, NodeId, NodeState};
    use crate::orca::Terminal;

    #[test]
    fn only_envoy_questions_are_nonblocking() {
        let data = ClanData {
            terminals: vec![Terminal {
                handle: "t1".into(),
                connected: true,
                ..Terminal::default()
            }],
            ..ClanData::default()
        };
        let request = AskRequest {
            question: "May I proceed?".into(),
            context: String::new(),
            severity: Severity::Warn,
            url: None,
            origin: Origin {
                terminal_handle: Some("t1".into()),
                ..Origin::default()
            },
        };
        for kind in [
            NodeKind::Terminal,
            NodeKind::Worker,
            NodeKind::Coordinator,
            NodeKind::Attendant,
            NodeKind::Envoy,
        ] {
            let mut snapshot = ClanSnapshot::build(&data, 100);
            let node = snapshot
                .nodes
                .iter_mut()
                .find(|n| n.id == NodeId::terminal("t1"))
                .unwrap();
            node.kind = kind;
            let mut asks = Asks::new();
            let (_, waiting) = prepare_ask(&mut asks, Some(node), &request, 100);
            assert_eq!(waiting, kind != NodeKind::Envoy, "{kind:?}");
            snapshot.mark_waiting(&asks.waiting(&data, None));
            assert_eq!(
                snapshot.node(&NodeId::terminal("t1")).unwrap().state,
                if waiting {
                    NodeState::NeedsInput
                } else {
                    NodeState::Idle
                },
                "{kind:?}"
            );
        }
        let (_, waiting) = prepare_ask(&mut Asks::new(), None, &request, 100);
        assert!(!waiting);
    }
}
