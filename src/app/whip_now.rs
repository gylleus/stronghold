//! Manual whip in the app: "Whip" requests from the UI and the control API, and "Whip all
//! stalled now". Nudges run on the whip thread; results come back as reports and show in a
//! toast (and, for the API, in the reply).

use crossbeam_channel::Receiver;
use eframe::egui;
use serde_json::{Value, json};

use super::{MyrstackApp, now_ms};
use crate::control::{ControlError, ErrorCode, Reply};
use crate::model::NodeId;
use crate::whip::manual::{ManualNudge, ManualPlan, ManualSkip, nudges, plan_manual_whip};
use crate::whip::{NudgeVia, WhipNowReport};

/// Manual whips in flight and the popover/toast.
#[derive(Default)]
pub(super) struct WhipNowState {
    pub(super) ui: crate::ui::whip_now::WhipNowUi,
    pending: Vec<Pending>,
}

/// A request waiting for the whip thread's report.
struct Pending {
    rx: Receiver<WhipNowReport>,
    kind: PendingKind,
    /// Set for a control API request.
    reply: Option<Reply>,
}

enum PendingKind {
    Manual {
        nudges: Vec<ManualNudge>,
        skips: Vec<ManualSkip>,
    },
    Stalled,
}

impl MyrstackApp {
    /// The manual whip plan for `ids` against the current fleet; `None` before the first poll.
    fn plan_whip(&self, ids: &[NodeId]) -> Option<Vec<ManualPlan>> {
        let (Some(data), Some(snapshot)) = (&self.data, &self.snapshot) else {
            return None;
        };
        Some(plan_manual_whip(ids, data, snapshot, now_ms()))
    }

    /// "Whip" from the UI (button, context menu, `w`): sends at once, or opens the popover
    /// when a target waits on input or there are many.
    pub(super) fn request_whip(&mut self, ids: Vec<NodeId>) {
        let now = now_ms();
        let Some(plans) = self.plan_whip(&ids) else {
            self.whip_now
                .ui
                .set_toast("No fleet data yet.".into(), true, now);
            return;
        };
        let skips = skips_of(&plans);
        if let Some(send) = self.whip_now.ui.request(plans, now) {
            self.send_manual_whip(send, skips, None);
        }
    }

    /// "Whip all stalled now": an immediate tick under the normal rules.
    pub(super) fn request_whip_stalled(&mut self, reply: Option<Reply>) {
        let rx = self.whip.tick_now();
        self.whip_now.pending.push(Pending {
            rx,
            kind: PendingKind::Stalled,
            reply,
        });
    }

    fn send_manual_whip(
        &mut self,
        nudges: Vec<ManualNudge>,
        skips: Vec<ManualSkip>,
        reply: Option<Reply>,
    ) {
        log::info!(
            "manual whip: {}",
            nudges
                .iter()
                .map(|n| n.handle.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let rx = self.whip.whip_now(nudges.clone());
        self.whip_now.pending.push(Pending {
            rx,
            kind: PendingKind::Manual { nudges, skips },
            reply,
        });
    }

    /// Takes finished reports: toasts them and answers API requests.
    pub(super) fn poll_whip_now(&mut self) {
        let now = now_ms();
        let mut done = Vec::new();
        self.whip_now.pending.retain_mut(|p| match p.rx.try_recv() {
            Ok(report) => {
                done.push((
                    report,
                    std::mem::replace(&mut p.kind, PendingKind::Stalled),
                    p.reply.take(),
                ));
                false
            }
            Err(crossbeam_channel::TryRecvError::Empty) => true,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                if let Some(reply) = p.reply.take() {
                    reply.send(Err(ControlError::new(
                        ErrorCode::Unavailable,
                        "the whip thread stopped",
                    )));
                }
                false
            }
        });
        for (report, kind, reply) in done {
            let (text, error, result) = match &kind {
                PendingKind::Manual { nudges, skips } => {
                    let (text, error) = crate::ui::whip_now::manual_result_text(&report, skips);
                    (text, error, manual_json(&report, nudges, skips))
                }
                PendingKind::Stalled => (
                    crate::ui::whip_now::stalled_result_text(&report),
                    !report.ran,
                    stalled_json(&report),
                ),
            };
            self.whip_now.ui.set_toast(text, error, now);
            if let Some(reply) = reply {
                reply.send(Ok(result));
            }
        }
    }

    /// Draws the popover and toast; sends a confirmed whip.
    pub(super) fn whip_now_dialogs(&mut self, ctx: &egui::Context, now_ms: i64) {
        if let Some((send, skips)) = self.whip_now.ui.show(ctx, now_ms) {
            self.send_manual_whip(send, skips, None);
        }
    }

    /// Control API `whip`: nudge `agents` now, or run a pass over the stalled ones. The API
    /// never asks for confirmation (the MCP tool is marked destructive).
    pub(super) fn control_whip(
        &mut self,
        agents: &[String],
        stalled: bool,
        reply: Reply,
    ) -> Result<(), ControlError> {
        if stalled {
            self.request_whip_stalled(Some(reply));
            return Ok(());
        }
        let ids: Result<Vec<NodeId>, ControlError> = agents
            .iter()
            .map(|a| self.control_node(a).map(|n| n.id.clone()))
            .collect();
        let plans = ids.and_then(|ids| {
            self.plan_whip(&ids).ok_or_else(|| {
                ControlError::new(ErrorCode::Unavailable, "the app has no fleet data yet")
            })
        });
        let plans = match plans {
            Ok(p) => p,
            Err(e) => {
                reply.send(Err(e.clone()));
                return Err(e);
            }
        };
        let skips = skips_of(&plans);
        let send = nudges(&plans);
        if send.is_empty() {
            reply.send(Ok(manual_json(&WhipNowReport::default(), &[], &skips)));
            return Ok(());
        }
        self.send_manual_whip(send, skips, Some(reply));
        Ok(())
    }
}

fn skips_of(plans: &[ManualPlan]) -> Vec<ManualSkip> {
    plans.iter().filter_map(ManualPlan::skip).cloned().collect()
}

fn via_json(via: &NudgeVia) -> Value {
    match via {
        NudgeVia::Bus { to, run_id } => json!({ "bus": to, "run": run_id }),
        NudgeVia::Terminal { handle } => json!({ "terminal": handle }),
    }
}

/// The API reply for a manual whip: one entry per nudge (in order) and the skips.
pub(super) fn manual_json(
    report: &WhipNowReport,
    nudges: &[ManualNudge],
    skips: &[ManualSkip],
) -> Value {
    let results: Vec<Value> = nudges
        .iter()
        .zip(&report.items)
        .map(|(n, item)| {
            json!({
                "id": n.node,
                "label": n.label,
                "handle": n.handle,
                "via": via_json(&n.via),
                "target": item.entry.target,
                "text": n.text,
                "ok": item.entry.outcome == "sent",
                "outcome": item.entry.outcome,
                "warning": n.warn,
                "seq": item.seq,
            })
        })
        .collect();
    let sent = results.iter().filter(|r| r["ok"] == json!(true)).count();
    json!({
        "mode": "manual",
        "sent": sent,
        "failed": results.len() - sent,
        "nudges": results,
        "skipped": skips.iter().map(|s| json!({
            "id": s.node,
            "label": s.label,
            "reason": s.reason,
        })).collect::<Vec<_>>(),
        "automatic_dry_run": report.dry_run,
    })
}

/// The API reply for "whip all stalled now".
pub(super) fn stalled_json(report: &WhipNowReport) -> Value {
    json!({
        "mode": "stalled",
        "ran": report.ran,
        "dry_run": report.dry_run,
        "actions": report.items.iter().map(|i| json!({
            "seq": i.seq,
            "action": i.entry.action,
            "target": i.entry.target,
            "handle": i.handle,
            "outcome": i.entry.outcome,
            "dry_run": i.entry.dry_run,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whip::{WhipLogEntry, WhipLogItem};

    fn item(outcome: &str) -> WhipLogItem {
        WhipLogItem {
            seq: 4,
            entry: WhipLogEntry {
                at_ms: 0,
                action: "nudge via bus to dispatch:ctx_1".into(),
                target: "worker w task t1".into(),
                reason: String::new(),
                dry_run: false,
                outcome: outcome.into(),
                manual: true,
            },
            handle: Some("w".into()),
            run_id: None,
            task_id: None,
        }
    }

    #[test]
    fn manual_reply_pairs_nudges_with_outcomes() {
        let nudge = ManualNudge {
            node: NodeId::terminal("w"),
            label: "worker".into(),
            handle: "w".into(),
            targets: vec![],
            via: NudgeVia::Bus {
                to: "dispatch:ctx_1".into(),
                run_id: Some("r".into()),
            },
            text: "Myrstack: hi".into(),
            warn: None,
        };
        let skip = ManualSkip {
            node: NodeId::queen(),
            label: "Queen".into(),
            reason: "the Queen is Myrstack itself".into(),
        };
        let report = WhipNowReport {
            ran: true,
            items: vec![item("sent")],
            dry_run: true,
        };
        let v = manual_json(&report, &[nudge], &[skip]);
        assert_eq!(v["sent"], 1);
        assert_eq!(v["failed"], 0);
        assert_eq!(v["automatic_dry_run"], true);
        assert_eq!(v["nudges"][0]["id"], "term:w");
        assert_eq!(v["nudges"][0]["via"]["bus"], "dispatch:ctx_1");
        assert_eq!(v["nudges"][0]["seq"], 4);
        assert_eq!(v["skipped"][0]["reason"], "the Queen is Myrstack itself");
    }

    #[test]
    fn stalled_reply_lists_the_pass() {
        let report = WhipNowReport {
            ran: true,
            items: vec![item("dry-run")],
            dry_run: true,
        };
        let v = stalled_json(&report);
        assert_eq!(v["mode"], "stalled");
        assert_eq!(v["ran"], true);
        assert_eq!(v["actions"][0]["outcome"], "dry-run");
    }
}
