//! Karl's review surface; draft actions are returned to the app for persistence and delivery.

use std::collections::HashMap;

use eframe::egui::{self, PointerButton, Ui};

use crate::envoy::drafts::{ATTRIBUTION, Draft, DraftKind, DraftStatus};
use crate::envoy::inbox::Inbox;

pub const DRY_RUN_BANNER: &str = "Dry run: approving records the message, nothing is posted";

#[derive(Debug, PartialEq, Eq)]
pub enum DraftAction {
    Approve { id: String, text: String },
    Reject(String),
}

#[derive(Default)]
pub struct DraftsPanel {
    edits: HashMap<String, String>,
    errors: HashMap<String, String>,
}

impl DraftsPanel {
    #[cfg(test)]
    pub(crate) fn set_edit(&mut self, id: &str, text: &str) {
        self.edits.insert(id.into(), text.into());
    }

    pub fn result(&mut self, id: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.errors.remove(id);
                self.edits.remove(id);
            }
            Err(error) => {
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
        self.errors
            .retain(|id, _| drafts.iter().any(|d| d.id == *id));
        let mut action = None;
        ui.heading("Envoy drafts");
        if dry_run {
            ui.label(DRY_RUN_BANNER);
        } else {
            ui.label("Approved replies are posted to Slack. Approved tasks go to the Queen.");
        }
        if let Some(error) = error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("envoy_drafts")
            .show(ui, |ui| {
                let pending: Vec<_> = drafts
                    .iter()
                    .filter(|d| d.status == DraftStatus::Pending)
                    .collect();
                if pending.is_empty() {
                    ui.weak("No drafts waiting for approval.");
                }
                for draft in pending {
                    ui.push_id(&draft.id, |ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            destination(ui, draft, inbox);
                            ui.label(format!("Why: {}", draft.rationale));
                            let text = self
                                .edits
                                .entry(draft.id.clone())
                                .or_insert_with(|| draft.text.clone());
                            ui.add(
                                egui::TextEdit::multiline(text)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(4),
                            );
                            if draft.kind == DraftKind::Reply
                                && !text.trim_end().ends_with(ATTRIBUTION)
                            {
                                ui.label(ATTRIBUTION);
                            }
                            if let Some(error) = self.errors.get(&draft.id) {
                                ui.colored_label(ui.visuals().error_fg_color, error);
                            }
                            ui.horizontal(|ui| {
                                let approve = if draft.kind == DraftKind::TaskProposal {
                                    "Send to Queen"
                                } else {
                                    "Approve"
                                };
                                // Keyboard activation must never authorize an outbound message.
                                if ui.button(approve).clicked_by(PointerButton::Primary) {
                                    action = Some(DraftAction::Approve {
                                        id: draft.id.clone(),
                                        text: text.clone(),
                                    });
                                }
                                if ui.button("Reject").clicked_by(PointerButton::Primary) {
                                    action = Some(DraftAction::Reject(draft.id.clone()));
                                }
                            });
                        });
                    });
                    ui.add_space(8.0);
                }
                let history: Vec<_> = drafts
                    .iter()
                    .rev()
                    .filter(|d| d.status != DraftStatus::Pending)
                    .collect();
                egui::CollapsingHeader::new(format!("History ({})", history.len()))
                    .default_open(false)
                    .show(ui, |ui| {
                        for draft in history {
                            ui.push_id(&draft.id, |ui| {
                                ui.strong(format!("{} · {}", status_label(draft.status), draft.id));
                                destination(ui, draft, inbox);
                                ui.label(&draft.text);
                                if let Some(error) = &draft.error {
                                    ui.colored_label(ui.visuals().error_fg_color, error);
                                }
                                ui.separator();
                            });
                        }
                    });
            });
        action
    }
}

fn destination(ui: &mut Ui, draft: &Draft, inbox: &Inbox) {
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
        ui.strong("Task for the Queen");
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
        DraftStatus::DryRunSent => "Dry run sent",
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
    fn proposals_have_explicit_send_to_queen_action() {
        let drafts = [Draft::proposal(
            "Do the task".into(),
            Some("repo".into()),
            "Requested".into(),
            "https://example.slack.com/message".into(),
            0,
        )];
        let action = click_label("Send to Queen", |ui| {
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
    fn errors_stay_inline_and_history_is_collapsed() {
        let mut draft = reply();
        let mut panel = DraftsPanel::default();
        panel.result(&draft.id, Err("Blocked: token detected".into()));
        click_label("Blocked: token detected", |ui| {
            panel.show(ui, &[draft.clone()], &Inbox::default(), true, None)
        });
        draft.status = DraftStatus::Failed;
        draft.error = Some("delivery failed".into());
        click_label("History (1)", |ui| {
            panel.show(ui, &[draft.clone()], &Inbox::default(), true, None)
        });
        assert_eq!(status_label(DraftStatus::DryRunSent), "Dry run sent");
    }
}
