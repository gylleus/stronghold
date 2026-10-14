//! The user's review surface; draft actions are returned to the app for persistence and delivery.

use std::collections::{BTreeSet, HashMap};

use eframe::egui::{self, PointerButton, Ui};

use crate::envoy::drafts::{Draft, DraftKind, DraftStatus, attribution};
use crate::envoy::inbox::Inbox;

pub const DRY_RUN_BANNER: &str = "Dry run: approving records the message, nothing is posted";

#[derive(Debug, PartialEq, Eq)]
pub enum DraftAction {
    Approve {
        id: String,
        text: String,
    },
    Reject(String),
    DismissEvent(String),
    DismissRequest(String),
    DismissDraft(String),
    ClearHandled,
    Pause(String),
    Adopt,
    Keep(String),
    Settle(String),
    Binding {
        id: String,
        proposal_id: u64,
        accept: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Inbox,
    Outbox,
}

#[derive(Default)]
pub struct DraftsPanel {
    edits: HashMap<String, String>,
    errors: HashMap<String, String>,
    last_action_at: Option<f64>,
    tab: Option<Tab>,
    dismissed_failures: BTreeSet<String>,
    dismissed_requests: BTreeSet<String>,
    recorded_task: Option<String>,
    requests: Vec<crate::envoy::requests::Request>,
}

impl DraftsPanel {
    pub fn set_dismissed_requests(&mut self, requests: BTreeSet<String>) {
        self.dismissed_requests = requests;
    }

    pub fn set_cleanup(&mut self, drafts: &BTreeSet<String>) {
        self.dismissed_failures.clone_from(drafts);
        if self
            .recorded_task
            .as_ref()
            .is_some_and(|id| drafts.contains(id))
        {
            self.recorded_task = None;
        }
    }

    pub fn set_requests(&mut self, requests: &[crate::envoy::requests::Request]) {
        self.requests = requests.to_vec();
    }

    #[cfg(test)]
    pub(crate) fn set_edit(&mut self, id: &str, text: &str) {
        self.edits.insert(id.into(), text.into());
    }

    pub fn open(&mut self, drafts: &[Draft]) {
        self.tab = Some(
            if self
                .requests
                .iter()
                .any(|r| request_inbox(r, &self.dismissed_requests))
                || drafts.iter().any(|d| {
                    d.status == DraftStatus::Pending
                        || (d.status == DraftStatus::Failed
                            && !self.dismissed_failures.contains(&d.id))
                })
            {
                Tab::Inbox
            } else {
                Tab::Outbox
            },
        );
    }

    pub fn result(&mut self, id: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.errors.remove(id);
                self.edits.remove(id);
            }
            Err(error) => {
                if self.recorded_task.as_deref() == Some(id) {
                    self.recorded_task = None;
                }
                self.errors.insert(id.into(), error);
            }
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        drafts: &[Draft],
        inbox: &Inbox,
        dry_run: bool,
        error: Option<&str>,
    ) -> Option<DraftAction> {
        self.edits.retain(|id, _| {
            drafts
                .iter()
                .any(|d| d.id == *id && d.status == DraftStatus::Pending)
        });
        self.errors.retain(|id, _| {
            drafts.iter().any(|d| d.id == *id)
                || self.requests.iter().any(|r| r.id == *id)
                || inbox.get(id).is_some()
                || id == "cleanup"
                || id == "adopt"
        });
        let mut action = None;
        let now = ui.input(|i| i.time);
        let ready = self.last_action_at.is_none_or(|at| now - at >= 0.6);
        if !ready {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.tab.is_none() {
            self.open(drafts);
        }
        ui.heading("Envoy");
        if let Some(error) = self.errors.get("adopt") {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if deliberate_click(&ui.button("Adopt open threads")) {
            action = Some(DraftAction::Adopt);
        }
        if dry_run {
            ui.label(DRY_RUN_BANNER);
        } else {
            ui.label("Approved replies are posted to Slack. Accepted tasks go to the King.");
        }
        if let Some(error) = error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Some(Tab::Inbox), "Inbox");
            ui.selectable_value(&mut self.tab, Some(Tab::Outbox), "Outbox");
        });
        if self.tab == Some(Tab::Inbox) {
            if ui.button("Clear handled / stale").on_hover_text(
                "Dismiss handled drafts and messages older than 30 minutes, including queued events. Pending approvals are kept."
            ).clicked() {
                action = Some(DraftAction::ClearHandled);
            }
            if let Some(error) = self.errors.get("cleanup") {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        }
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt(("envoy", self.tab == Some(Tab::Inbox)))
            .show(ui, |ui| {
                let inbox_tab = self.tab == Some(Tab::Inbox);
                for request in self
                    .requests
                    .iter()
                    .filter(|r| request_inbox(r, &self.dismissed_requests) == inbox_tab)
                {
                    ui.push_id(&request.id, |ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.strong(format!(
                                "{} · {:?} · {}",
                                request.id, request.status, request.to
                            ));
                            ui.label(format!(
                                "Asked by {}",
                                request
                                    .bound_requester()
                                    .map_or("unknown", |r| r.label.as_str())
                            ));
                            if let Some(error) = self.errors.get(&request.id) {
                                ui.colored_label(ui.visuals().error_fg_color, error);
                            }
                            if request.close_requested {
                                ui.weak(if dry_run {
                                    "Dry run: close recorded; subscription retained"
                                } else {
                                    "Closing subscription…"
                                });
                            }
                            for turn in &request.turns {
                                ui.label(format!("Why: {}", turn.why));
                                ui.label(&turn.ask);
                                if let Some(error) = &turn.error {
                                    ui.colored_label(ui.visuals().error_fg_color, error);
                                }
                            }
                            for reply in &request.deliveries {
                                if reply.inbox {
                                    ui.strong("Reply for a finished or unknown dwarf");
                                }
                                ui.label(&reply.text);
                                if reply.dry_run {
                                    ui.weak("Dry run: reply recorded without terminal delivery");
                                }
                            }
                            if inbox_tab {
                                if let Some(proposal) = &request.binding_proposal {
                                    let thread = &proposal.preview.thread;
                                    ui.strong(format!(
                                        "Route replies from {} / {} to {}?",
                                        thread.conversation, thread.ts, proposal.requester.label
                                    ));
                                    ui.label(&proposal.preview.first_line);
                                    ui.weak(format!("Terminal: {}", proposal.requester.handle()));
                                    slack_link(ui, Some(&proposal.preview.slack_link));
                                    ui.label(format!("Envoy reason: {}", proposal.reason));
                                    ui.push_id(proposal.id, |ui| {
                                        ui.horizontal(|ui| {
                                            for (label, accept) in
                                                [("Accept", true), ("Reject", false)]
                                            {
                                                if deliberate_click(&ui.button(label)) {
                                                    action = Some(DraftAction::Binding {
                                                        id: request.id.clone(),
                                                        proposal_id: proposal.id,
                                                        accept,
                                                    });
                                                }
                                            }
                                        });
                                    });
                                }
                                if request.bound_requester().is_none() && !request.orphan_kept {
                                    ui.strong("Orphan thread");
                                    if ui.button("Keep").clicked() {
                                        action = Some(DraftAction::Keep(request.id.clone()));
                                    }
                                }
                                if ui.button("Dismiss").clicked() {
                                    action = Some(DraftAction::DismissRequest(request.id.clone()));
                                }
                                if ui
                                    .button(if request.bound_requester().is_none() {
                                        "Close"
                                    } else {
                                        "Settle"
                                    })
                                    .clicked()
                                {
                                    action = Some(DraftAction::Settle(request.id.clone()));
                                }
                            }
                        });
                    });
                    ui.add_space(8.0);
                }
                if self.tab == Some(Tab::Inbox) {
                    let mut cards = 0;
                    for draft in drafts {
                        let failed = draft.status == DraftStatus::Failed
                            && !self.dismissed_failures.contains(&draft.id);
                        let recorded = draft.status == DraftStatus::DryRunSent
                            && self.recorded_task.as_deref() == Some(&draft.id);
                        if draft.status != DraftStatus::Pending && !failed && !recorded {
                            continue;
                        }
                        cards += 1;
                        ui.push_id(&draft.id, |ui| {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                if draft.status == DraftStatus::Pending {
                                    if let Some(next) =
                                        self.pending_card(ui, draft, inbox, dry_run, ready)
                                    {
                                        action = Some(next);
                                    }
                                } else {
                                    decided_card(ui, draft, inbox);
                                    if let Some(error) = self.errors.get(&draft.id) {
                                        ui.colored_label(ui.visuals().error_fg_color, error);
                                    }
                                    if ui.button("Dismiss").clicked() {
                                        action = Some(DraftAction::DismissDraft(draft.id.clone()));
                                    }
                                }
                            });
                        });
                        ui.add_space(8.0);
                    }
                    if cards == 0 {
                        ui.weak("No decisions waiting for you.");
                    }
                    if let Some(next) = recent_messages(ui, inbox, &self.errors) {
                        action = Some(next);
                    }
                } else {
                    let mut history: Vec<_> = drafts
                        .iter()
                        .rev()
                        .filter(|d| d.status != DraftStatus::Pending)
                        .collect();
                    history.sort_by_key(|d| std::cmp::Reverse(d.updated_ms));
                    egui::CollapsingHeader::new(format!("Decisions ({})", history.len()))
                        .id_salt("envoy_outbox_decisions")
                        .default_open(true)
                        .show(ui, |ui| {
                            if history.is_empty() {
                                ui.weak("No decisions yet.");
                            }
                            for draft in history {
                                ui.push_id(&draft.id, |ui| {
                                    decided_card(ui, draft, inbox);
                                    ui.separator();
                                });
                            }
                        });
                }
            });
        if action.is_some() {
            self.last_action_at = Some(now);
        }
        action
    }

    fn pending_card(
        &mut self,
        ui: &mut Ui,
        draft: &Draft,
        inbox: &Inbox,
        dry_run: bool,
        ready: bool,
    ) -> Option<DraftAction> {
        destination(ui, draft, inbox);
        ui.label(format!("Why: {}", draft.rationale));
        if let Some(link) = &draft.request {
            ui.strong(format!("Asked by {}", link.requester));
            ui.label(format!("Why: {}", link.why));
        }
        if let Some(at) = draft.auto_at_ms {
            let remaining = (at - crate::control::server::now_ms()).max(0);
            ui.label(format!(
                "Automatic approval in {}s",
                (remaining + 999) / 1000
            ));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        let text = self
            .edits
            .entry(draft.id.clone())
            .or_insert_with(|| draft.text.clone());
        let edit = |ui: &mut Ui, text: &mut String| {
            ui.add_enabled(
                !draft.request.as_ref().is_some_and(|r| r.agent),
                egui::TextEdit::multiline(text)
                    .desired_width(f32::INFINITY)
                    .desired_rows(4),
            )
            .changed()
        };
        let edited = if let Some(link) = &draft.request {
            ui.columns(2, |columns| {
                columns[0].strong("Original ask");
                columns[0].label(&link.original_ask);
                columns[1].strong("Draft");
                edit(&mut columns[1], text)
            })
        } else {
            edit(ui, text)
        };
        if draft.kind != DraftKind::TaskProposal && !text.trim_end().ends_with(&attribution()) {
            ui.label(if draft.automatic {
                crate::envoy::drafts::auto_attribution()
            } else {
                attribution()
            });
        }
        if draft.kind == DraftKind::TaskProposal {
            ui.label(if dry_run {
                "Dry run: accepting records this task; it is not sent to the King."
            } else {
                "The King will start the dwarf for this task."
            });
        }
        if let Some(error) = self.errors.get(&draft.id) {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let mut action = if edited && draft.auto_at_ms.is_some() {
            Some(DraftAction::Pause(draft.id.clone()))
        } else {
            None
        };
        ui.horizontal(|ui| {
            let approve = if draft.kind == DraftKind::TaskProposal {
                "Accept task"
            } else {
                "Approve"
            };
            // Keyboard activation must never authorize an outbound message.
            let response = ui.add_enabled(ready, egui::Button::new(approve));
            if deliberate_click(&response) {
                if draft.kind == DraftKind::TaskProposal && dry_run {
                    self.recorded_task = Some(draft.id.clone());
                }
                action = Some(DraftAction::Approve {
                    id: draft.id.clone(),
                    text: text.clone(),
                });
            }
            let response = ui.add_enabled(
                ready,
                egui::Button::new(if draft.auto_at_ms.is_some() {
                    "Cancel"
                } else {
                    "Reject"
                }),
            );
            if deliberate_click(&response) {
                action = Some(DraftAction::Reject(draft.id.clone()));
            }
        });
        action
    }
}

fn decided_card(ui: &mut Ui, draft: &Draft, inbox: &Inbox) {
    ui.strong(format!("{} · {}", status_label(draft.status), draft.id));
    destination(ui, draft, inbox);
    ui.label(&draft.text);
    if draft.kind == DraftKind::TaskProposal && draft.status == DraftStatus::DryRunSent {
        ui.label("Task recorded in dry run; not sent to the King.");
    }
    if let Some(error) = &draft.error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
}

fn request_inbox(request: &crate::envoy::requests::Request, dismissed: &BTreeSet<String>) -> bool {
    use crate::envoy::requests::Status;
    !request.settled
        && !dismissed.contains(&request.id)
        && (request.binding_proposal.is_some()
            || request.status != Status::Declined
            || request.turns.iter().any(|t| {
                matches!(
                    t.status,
                    Status::Queued | Status::Drafted | Status::Approved | Status::Failed
                )
            }))
}

fn recent_messages(
    ui: &mut Ui,
    inbox: &Inbox,
    errors: &HashMap<String, String>,
) -> Option<DraftAction> {
    let mut action = None;
    egui::CollapsingHeader::new("Recent messages")
        .id_salt("envoy_recent_messages")
        .default_open(false)
        .show(ui, |ui| {
            if inbox.open_events().next().is_none() {
                ui.weak("No incoming messages yet.");
            }
            for event in inbox.open_events().rev() {
                let time = crate::envoy::inbox::timestamp_us(&event.ts)
                    .and_then(|us| jiff::Timestamp::from_microsecond(us).ok())
                    .map(|t| {
                        t.to_zoned(jiff::tz::TimeZone::system())
                            .strftime("%b %d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|| event.ts.clone());
                ui.strong(format!(
                    "{} · {} · {time}",
                    event.user_name, event.conversation
                ));
                let first_line: String = event
                    .text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                ui.label(first_line);
                slack_link(ui, event.permalink.as_deref());
                if let Some(error) = errors.get(&event.id()) {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.push_id(event.id(), |ui| {
                    if ui.button("Dismiss").clicked() {
                        action = Some(DraftAction::DismissEvent(event.id()));
                    }
                });
                ui.separator();
            }
        });
    action
}

fn deliberate_click(response: &egui::Response) -> bool {
    response.clicked_by(PointerButton::Primary)
        && !response.double_clicked_by(PointerButton::Primary)
        && !response.triple_clicked_by(PointerButton::Primary)
}

fn destination(ui: &mut Ui, draft: &Draft, inbox: &Inbox) {
    if let Some(conversation) = &draft.conversation {
        ui.strong(format!("To {conversation}"));
        ui.label(
            draft
                .thread_ts
                .as_ref()
                .map_or_else(|| "New message".into(), |ts| format!("Thread {ts}")),
        );
        slack_link(ui, draft.slack_link.as_deref());
        return;
    }

    if let Some(target) = &draft.in_reply_to {
        ui.strong(format!("Slack · {}", target.conversation));
        ui.label(
            draft
                .thread_ts
                .as_ref()
                .map_or_else(|| "Channel message".into(), |ts| format!("Thread {ts}")),
        );
        if let Some(event) = inbox.get(&format!("{}:{}", target.conversation, target.ts)) {
            ui.label(format!(
                "{} ({}): {}",
                event.user_name, event.user, event.text
            ));
            slack_link(
                ui,
                event.permalink.as_deref().or(draft.slack_link.as_deref()),
            );
        } else {
            ui.weak("Original message is no longer in the inbox.");
            ui.label(format!("In reply to {}", target.ts));
            slack_link(ui, draft.slack_link.as_deref());
        }
    } else {
        ui.strong("Task for the King");
        if let Some(repo) = &draft.repo {
            ui.label(format!("Repository: {repo}"));
        }
        slack_link(ui, draft.slack_link.as_deref());
    }
}

fn slack_link(ui: &mut Ui, link: Option<&str>) {
    if let Some(link) = link.filter(|s| crate::notify::agent::is_web_url(s)) {
        ui.hyperlink_to("Open Slack message", link);
    }
}

fn status_label(status: DraftStatus) -> &'static str {
    match status {
        DraftStatus::Pending => "Pending",
        DraftStatus::Approved => "Sending",
        DraftStatus::Sent => "Sent",
        DraftStatus::Failed => "Failed",
        DraftStatus::Rejected => "Rejected",
        DraftStatus::DryRunSent => "Dry run recorded",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envoy::drafts::ReplyTo;
    use crate::ui::test_util::{click_label, text_center};

    fn reply() -> Draft {
        let mut draft = Draft::reply(
            ReplyTo {
                conversation: "C123".into(),
                ts: "1.000001".into(),
            },
            None,
            "Visible reply".into(),
            "Requested update".into(),
            0,
        );
        draft.id = "envoy-draft-1".into();
        draft
    }

    #[test]
    fn pointer_click_approves_visible_edit_and_rejects() {
        let drafts = [reply()];
        let mut panel = DraftsPanel::default();
        panel.set_edit(&drafts[0].id, "Edited final text");
        let action = click_label("Approve", |ui| {
            panel.show(ui, &drafts, &Inbox::default(), true, None)
        });
        assert_eq!(
            action,
            Some(DraftAction::Approve {
                id: drafts[0].id.clone(),
                text: "Edited final text".into()
            })
        );
        panel.last_action_at = None;
        assert_eq!(
            click_label("Reject", |ui| panel.show(
                ui,
                &drafts,
                &Inbox::default(),
                true,
                None
            )),
            Some(DraftAction::Reject(drafts[0].id.clone()))
        );
    }

    #[test]
    fn double_click_cannot_approve_the_card_that_moves_under_the_pointer() {
        let ctx = egui::Context::default();
        let mut drafts = vec![reply(), reply()];
        drafts[1].id = "envoy-draft-2".into();
        let mut panel = DraftsPanel::default();
        let mut frame = |time, events: Vec<egui::Event>| {
            let mut action = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| action = panel.show(ui, &drafts, &Inbox::default(), true, None),
            );
            if let Some(DraftAction::Approve { id, .. }) = &action {
                drafts.iter_mut().find(|d| d.id == *id).unwrap().status = DraftStatus::DryRunSent;
            }
            output.textures_delta.clear();
            (output, action)
        };
        frame(0.0, vec![]);
        let at = text_center(&frame(0.01, vec![]).0, "Approve").unwrap();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(0.05, vec![egui::Event::PointerMoved(at), button(true)]);
        assert!(frame(0.06, vec![button(false)]).1.is_some());
        frame(0.1, vec![]);
        frame(0.15, vec![button(true)]);
        assert!(frame(0.16, vec![button(false)]).1.is_none());
        frame(0.9, vec![]);
        frame(0.95, vec![button(true)]);
        assert!(
            matches!(frame(0.96, vec![button(false)]).1, Some(DraftAction::Approve { id, .. }) if id == "envoy-draft-2")
        );
    }

    #[test]
    fn proposals_have_explicit_accept_task_action() {
        let drafts = [Draft::proposal(
            "Do the task".into(),
            Some("repo".into()),
            "Requested".into(),
            "https://example.slack.com/message".into(),
            0,
        )];
        click_label("The King will start the dwarf for this task.", |ui| {
            DraftsPanel::default().show(ui, &drafts, &Inbox::default(), false, None)
        });
        let action = click_label("Accept task", |ui| {
            DraftsPanel::default().show(ui, &drafts, &Inbox::default(), true, None)
        });
        assert!(matches!(action, Some(DraftAction::Approve { text, .. }) if text == "Do the task"));
    }

    #[test]
    fn keyboard_activation_never_approves_and_banner_tracks_effective_mode() {
        let ctx = egui::Context::default();
        let drafts = [reply()];
        let mut panel = DraftsPanel::default();
        for dry_run in [true, false] {
            for step in 0..12 {
                let keys = [egui::Key::Tab, egui::Key::Enter, egui::Key::Space];
                let key = keys[step % keys.len()];
                let mut action = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(900.0, 900.0),
                        )),
                        events: vec![
                            egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            },
                            egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed: false,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        ..Default::default()
                    },
                    |ui| action = panel.show(ui, &drafts, &Inbox::default(), dry_run, None),
                );
                assert!(action.is_none());
                assert_eq!(text_center(&output, DRY_RUN_BANNER).is_some(), dry_run);
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn errors_stay_inline_after_delivery_failure() {
        let mut draft = reply();
        let mut panel = DraftsPanel::default();
        panel.result(&draft.id, Err("Blocked: token detected".into()));
        click_label("Blocked: token detected", |ui| {
            panel.show(ui, &[draft.clone()], &Inbox::default(), true, None)
        });
        draft.status = DraftStatus::Failed;
        draft.error = Some("delivery failed".into());
        click_label("delivery failed", |ui| {
            panel.show(ui, &[draft.clone()], &Inbox::default(), true, None)
        });
        assert_eq!(status_label(DraftStatus::DryRunSent), "Dry run recorded");
    }

    struct Harness {
        ctx: egui::Context,
        panel: DraftsPanel,
        drafts: Vec<Draft>,
        inbox: Inbox,
        time: f64,
    }

    impl Harness {
        fn new(drafts: Vec<Draft>) -> Self {
            Self {
                ctx: egui::Context::default(),
                panel: DraftsPanel::default(),
                drafts,
                inbox: Inbox::default(),
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> (egui::FullOutput, Option<DraftAction>) {
            self.time += 0.2;
            let mut action = None;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 1000.0),
                    )),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ui| action = self.panel.show(ui, &self.drafts, &self.inbox, true, None),
            );
            output.textures_delta.clear();
            (output, action)
        }

        fn click(&mut self, label: &str) -> Option<DraftAction> {
            self.frame(vec![]);
            let at = text_center(&self.frame(vec![]).0, label)
                .unwrap_or_else(|| panic!("missing {label}"));
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
            self.frame(vec![button(false)]).1
        }

        fn visible(&mut self, label: &str) -> bool {
            text_center(&self.frame(vec![]).0, label).is_some()
        }
    }

    #[test]
    fn opening_selects_inbox_for_decisions_and_outbox_otherwise() {
        let mut h = Harness::new(vec![reply()]);
        assert!(h.visible("Approve"));
        h.click("Outbox");
        assert!(!h.visible("Approve"));
        h.panel.open(&h.drafts);
        assert!(h.visible("Approve"));
        h.drafts[0].status = DraftStatus::Sent;
        h.panel.open(&h.drafts);
        assert!(h.visible("Decisions (1)"));
        assert!(!h.visible("Approve"));
        let mut empty = Harness::new(vec![]);
        assert!(empty.visible("Decisions (0)"));
    }

    #[test]
    fn gateway_card_keeps_original_ask_and_auto_cancel_visible() {
        use crate::envoy::drafts::RequestLink;
        let link = RequestLink {
            id: "request-1".into(),
            turn_id: 1,
            original_ask: "Review abc012def at 12:34".into(),
            requester: "Build dwarf".into(),
            why: "Unblock review".into(),
            agent: true,
        };
        let mut draft = Draft::outgoing(
            "C123".into(),
            Some("1.000001".into()),
            link.verbatim(),
            "Avery owns review".into(),
            link,
            0,
        );
        draft.id = "draft-1".into();
        draft.automatic = true;
        draft.auto_at_ms = Some(crate::control::server::now_ms() + 120_000);
        let mut h = Harness::new(vec![draft]);
        assert!(h.visible("Asked by Build dwarf"));
        assert!(h.visible("Thread 1.000001"));
        assert!(h.visible("Original ask"));
        assert!(h.visible("Review abc012def at 12:34"));
        assert!(h.visible("Why: Unblock review"));
        assert_eq!(
            h.click("Cancel"),
            Some(DraftAction::Reject("draft-1".into()))
        );
    }

    #[test]
    fn adopted_orphan_opens_inbox_and_offers_keep_and_close() {
        use crate::envoy::requests::{Requests, Thread};
        let dir = tempfile::tempdir().unwrap();
        let mut requests = Requests::load(dir.path().join("requests.json")).unwrap();
        requests
            .adopt(
                Thread {
                    conversation: "D123".into(),
                    ts: "1.000001".into(),
                },
                "Fixture".into(),
                0,
            )
            .unwrap();
        let id = requests.all()[0].id.clone();
        let mut h = Harness::new(vec![]);
        h.panel.set_requests(requests.all());
        assert!(h.visible("Orphan thread"));
        assert_eq!(h.click("Keep"), Some(DraftAction::Keep(id.clone())));
        assert_eq!(h.click("Close"), Some(DraftAction::Settle(id.clone())));
        requests
            .change(&id, |r| {
                r.settled = true;
                Ok(())
            })
            .unwrap();
        h.panel.set_requests(requests.all());
        assert!(!h.visible("Asked by unknown"));
        h.click("Outbox");
        assert!(h.visible("Asked by unknown"));
    }

    #[test]
    fn binding_requires_deliberate_click_on_the_displayed_proposal() {
        use crate::envoy::requests::{
            BindingPreview, BindingProposal, Requester, Requests, Thread,
        };
        use crate::review::mark::Origin;
        let dir = tempfile::tempdir().unwrap();
        let mut requests = Requests::load(dir.path().join("requests.json")).unwrap();
        let thread = Thread {
            conversation: "D123".into(),
            ts: "1.000001".into(),
        };
        requests.adopt(thread.clone(), "Fixture".into(), 0).unwrap();
        let id = requests.all()[0].id.clone();
        let requester = Requester::from_origin(Origin {
            terminal_handle: Some("term:fixture".into()),
            ..Default::default()
        })
        .unwrap();
        requests
            .change(&id, |r| {
                r.binding_proposal = Some(BindingProposal {
                    id: 1,
                    requester,
                    reason: "Review belongs here".into(),
                    preview: BindingPreview {
                        thread,
                        first_line: "Root context".into(),
                        slack_link: "https://example.slack.com/archives/D123/p1000001".into(),
                    },
                });
                Ok(())
            })
            .unwrap();
        let mut h = Harness::new(vec![]);
        h.panel.set_requests(requests.all());
        assert!(h.visible("Route replies from D123 / 1.000001 to term:fixture?"));
        assert!(h.visible("Root context"));
        assert!(h.visible("Envoy reason: Review belongs here"));
        for key in [egui::Key::Tab, egui::Key::Enter, egui::Key::Space]
            .into_iter()
            .cycle()
            .take(18)
        {
            let events = [true, false].map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            assert!(h.frame(events.into()).1.is_none());
        }
        let at = text_center(&h.frame(vec![]).0, "Accept").unwrap();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        requests
            .change(&id, |r| {
                r.binding_proposal.as_mut().unwrap().id = 2;
                Ok(())
            })
            .unwrap();
        h.panel.set_requests(requests.all());
        assert!(
            h.frame(vec![button(false)]).1.is_none(),
            "a replacement proposal must not inherit the previous pointer press"
        );
        assert_eq!(
            h.click("Accept"),
            Some(DraftAction::Binding {
                id: id.clone(),
                proposal_id: 2,
                accept: true
            })
        );
        assert_eq!(
            h.click("Reject"),
            Some(DraftAction::Binding {
                id,
                proposal_id: 2,
                accept: false
            })
        );
    }

    #[test]
    fn failed_delivery_stays_in_inbox_until_dismissed_and_remains_in_outbox() {
        let mut h = Harness::new(vec![reply()]);
        assert!(matches!(
            h.click("Approve"),
            Some(DraftAction::Approve { .. })
        ));
        h.drafts[0].status = DraftStatus::Failed;
        h.drafts[0].error = Some("Network unavailable".into());
        h.panel.result(&h.drafts[0].id, Ok(()));
        assert!(h.visible("Network unavailable"));
        assert!(!h.visible("Approve"));
        h.panel.open(&h.drafts);
        assert!(h.visible("Network unavailable"));
        assert_eq!(
            h.click("Dismiss"),
            Some(DraftAction::DismissDraft(h.drafts[0].id.clone()))
        );
        h.panel
            .set_cleanup(&BTreeSet::from([h.drafts[0].id.clone()]));
        assert!(!h.visible("Network unavailable"));
        assert!(h.visible("No decisions waiting for you."));
        h.panel.open(&h.drafts);
        assert!(h.visible("Decisions (1)"));
        assert!(h.visible("Network unavailable"));
        h.click("Inbox");
        assert!(!h.visible("Network unavailable"));
    }

    #[test]
    fn accepting_a_dry_run_task_shows_confirmation_on_the_card() {
        let mut task = Draft::proposal(
            "Start a fixture dwarf".into(),
            None,
            "Requested".into(),
            String::new(),
            0,
        );
        task.id = "task-1".into();
        let mut h = Harness::new(vec![task]);
        assert!(h.visible("Dry run: accepting records this task; it is not sent to the King."));
        assert!(matches!(
            h.click("Accept task"),
            Some(DraftAction::Approve { .. })
        ));
        h.drafts[0].status = DraftStatus::DryRunSent;
        h.panel.result("task-1", Ok(()));
        assert!(h.visible("Task recorded in dry run; not sent to the King."));
        assert_eq!(
            h.click("Dismiss"),
            Some(DraftAction::DismissDraft("task-1".into()))
        );
        h.panel.set_cleanup(&BTreeSet::from(["task-1".into()]));
        assert!(!h.visible("Task recorded in dry run; not sent to the King."));
        h.click("Outbox");
        assert!(h.visible("Task recorded in dry run; not sent to the King."));
    }

    #[test]
    fn outbox_keeps_expansion_when_items_arrive_and_orders_by_decision_time() {
        let mut older = reply();
        older.status = DraftStatus::Rejected;
        older.updated_ms = 10;
        let mut h = Harness::new(vec![older.clone()]);
        assert!(h.visible("Visible reply"));
        h.click("Decisions (1)");
        assert!(!h.visible("Visible reply"));
        let mut newer = older.clone();
        newer.id = "second".into();
        newer.text = "New decision".into();
        newer.updated_ms = 20;
        h.drafts.push(newer);
        assert!(h.visible("Decisions (2)"));
        assert!(!h.visible("New decision"));
        h.click("Decisions (2)");
        assert!(h.visible("New decision"));
        let mut latest = older;
        latest.id = "third".into();
        latest.text = "Latest decision".into();
        latest.updated_ms = 30;
        h.drafts.insert(0, latest);
        let output = h.frame(vec![]).0;
        assert!(
            text_center(&output, "Latest decision").unwrap().y
                < text_center(&output, "New decision").unwrap().y
        );
        assert!(
            text_center(&output, "New decision").unwrap().y
                < text_center(&output, "Visible reply").unwrap().y
        );
    }

    #[test]
    fn recent_messages_show_events_without_drafts_and_only_the_first_line() {
        use crate::envoy::channel::{EventKind, InboundEvent};
        let mut h = Harness::new(vec![]);
        for (ts, text) in [
            ("1.0", "Older message"),
            ("2.0", "Latest message\nPrivate second line"),
        ] {
            h.inbox.insert(InboundEvent {
                channel_kind: "slack".into(),
                conversation: "C123".into(),
                ts: ts.into(),
                thread_ts: None,
                user: "U123".into(),
                user_name: "Avery".into(),
                text: text.into(),
                kind: EventKind::Mention,
                permalink: Some("https://example.slack.com/archives/C123/p2".into()),
            });
        }
        h.click("Inbox");
        assert!(h.visible("Recent messages"));
        assert!(!h.visible("Latest message"));
        assert!(h.click("Recent messages").is_none());
        let output = h.frame(vec![]).0;
        assert!(
            text_center(&output, "Latest message").unwrap().y
                < text_center(&output, "Older message").unwrap().y
        );
        assert!(!h.visible("Private second line"));
        assert!(h.visible("Open Slack message"));
        assert!(!h.visible("Approve"));
        assert!(!h.visible("Accept task"));
    }

    #[test]
    fn inbox_cleanup_buttons_return_persistable_actions() {
        let mut h = Harness::new(vec![]);
        let event: crate::envoy::channel::InboundEvent =
            serde_json::from_value(serde_json::json!({
                "channel_kind":"slack", "conversation":"C123", "ts":"1.000001",
                "user":"U123", "user_name":"Avery", "text":"Queued event", "kind":"agent_comms"
            }))
            .unwrap();
        h.inbox.insert(event.clone());
        h.click("Inbox");
        assert_eq!(
            h.click("Clear handled / stale"),
            Some(DraftAction::ClearHandled)
        );
        h.click("Recent messages");
        assert!(h.visible("Queued event"));
        assert_eq!(
            h.click("Dismiss"),
            Some(DraftAction::DismissEvent(event.id()))
        );
        h.inbox.resolve([event.id()]);
        assert!(!h.visible("Queued event"));
    }
}
