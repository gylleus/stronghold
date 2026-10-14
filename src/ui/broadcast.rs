//! Broadcast composer, shown in the King's details: pick runs, write one message, confirm, send
//! to their coordinators.

use std::collections::{BTreeSet, HashSet};

use eframe::egui::{self, RichText};

use crate::broadcast::{
    BroadcastJob, BroadcastPlan, BroadcastSelection, BroadcastSender, RunChoice, plan_broadcast,
    run_choices,
};
use crate::model::ClanSnapshot;
use crate::ui::clan_view::{state_color, state_label};

enum Stage {
    Compose,
    /// Plan frozen when Send was pressed, awaiting confirmation.
    Confirm(BroadcastPlan),
    Sending {
        plan: BroadcastPlan,
        job: BroadcastJob,
    },
}

/// State of the broadcast composer. Runs are checked by default; the composer remembers which
/// ones the user unticked.
pub struct BroadcastComposer {
    text: String,
    unchecked: HashSet<String>,
    /// `None` shows every repo.
    repo_filter: Option<String>,
    stage: Stage,
    /// A send was confirmed since [`BroadcastComposer::take_sent`] last looked.
    sent: bool,
}

impl Default for BroadcastComposer {
    fn default() -> Self {
        Self {
            text: String::new(),
            unchecked: HashSet::new(),
            repo_filter: None,
            stage: Stage::Compose,
            sent: false,
        }
    }
}

impl BroadcastComposer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a send was confirmed since the last call (for the farm's ripple).
    pub fn take_sent(&mut self) -> bool {
        std::mem::take(&mut self.sent)
    }

    /// Draws the composer. `make_sender` is called once, when the user confirms a send.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: Option<&ClanSnapshot>,
        make_sender: impl FnOnce() -> Box<dyn BroadcastSender>,
    ) {
        let Some(snapshot) = snapshot else {
            ui.label("Waiting for the first poll…");
            return;
        };
        match &mut self.stage {
            Stage::Compose => self.compose(ui, snapshot),
            Stage::Confirm(plan) => {
                let plan = plan.clone();
                self.confirm(ui, plan, make_sender);
            }
            Stage::Sending { plan, job } => {
                job.poll();
                results(ui, snapshot, plan, job);
                ui.separator();
                let restart = ui
                    .horizontal(|ui| {
                        if !job.is_done() {
                            ui.spinner();
                            ui.label(format!("Sending {}/{}…", job.results().len(), job.total()));
                            false
                        } else {
                            ui.button("New broadcast").clicked()
                        }
                    })
                    .inner;
                if restart {
                    self.text.clear();
                    self.stage = Stage::Compose;
                }
            }
        }
    }

    fn selection(&self, choices: &[RunChoice]) -> BroadcastSelection {
        BroadcastSelection {
            run_ids: choices
                .iter()
                .filter(|c| self.shown(c) && self.checked(c))
                .map(|c| c.run_id.clone())
                .collect(),
        }
    }

    fn shown(&self, choice: &RunChoice) -> bool {
        self.repo_filter
            .as_ref()
            .is_none_or(|repo| choice.repo.as_ref() == Some(repo))
    }

    fn checked(&self, choice: &RunChoice) -> bool {
        choice.coordinator_state.is_some() && !self.unchecked.contains(&choice.run_id)
    }

    fn compose(&mut self, ui: &mut egui::Ui, snapshot: &ClanSnapshot) {
        let choices = run_choices(snapshot);
        if choices.is_empty() {
            ui.label("No live runs.");
            return;
        }
        let repos: BTreeSet<String> = choices.iter().filter_map(|c| c.repo.clone()).collect();
        if self
            .repo_filter
            .as_ref()
            .is_some_and(|r| !repos.contains(r))
        {
            self.repo_filter = None;
        }

        ui.horizontal(|ui| {
            ui.label("Repo:");
            egui::ComboBox::from_id_salt("broadcast_repo")
                .selected_text(self.repo_filter.as_deref().unwrap_or("all"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.repo_filter, None, "all");
                    for repo in &repos {
                        ui.selectable_value(&mut self.repo_filter, Some(repo.clone()), repo);
                    }
                });
            ui.separator();
            let shown: Vec<String> = choices
                .iter()
                .filter(|c| self.shown(c))
                .map(|c| c.run_id.clone())
                .collect();
            if ui.button("Select all").clicked() {
                for id in &shown {
                    self.unchecked.remove(id);
                }
            }
            if ui.button("Select none").clicked() {
                self.unchecked.extend(shown);
            }
        });

        let shown: Vec<&RunChoice> = choices.iter().filter(|c| self.shown(c)).collect();

        let dark = ui.visuals().dark_mode;
        egui::ScrollArea::vertical()
            .id_salt("broadcast_runs")
            .max_height(260.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for &choice in &shown {
                    ui.horizontal(|ui| {
                        let mut checked = self.checked(choice);
                        let live = choice.coordinator_state.is_some();
                        let objective = if choice.objective.is_empty() {
                            choice.run_id.as_str()
                        } else {
                            choice.objective.as_str()
                        };
                        let response = ui
                            .add_enabled(live, egui::Checkbox::without_text(&mut checked))
                            .on_hover_text(&choice.run_id);
                        if response.changed() {
                            if checked {
                                self.unchecked.remove(&choice.run_id);
                            } else {
                                self.unchecked.insert(choice.run_id.clone());
                            }
                        }
                        match choice.coordinator_state {
                            Some(state) => ui.label(
                                RichText::new(state_label(state)).color(state_color(state, dark)),
                            ),
                            None => ui.weak("coordinator not live"),
                        };
                        if let Some(repo) = &choice.repo {
                            ui.weak(repo);
                        }
                        ui.add(egui::Label::new(objective).truncate())
                            .on_hover_text(objective);
                    });
                }
            });

        ui.separator();
        ui.label("Message (sent as a `status` to each coordinator's Run mailbox):");
        let response = ui.add(
            egui::TextEdit::multiline(&mut self.text)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text(crate::user::fill(
                    "Sent as \"King: …\" — agents treat it as {user}'s instruction",
                )),
        );
        crate::ui::widgets::focus_ring(ui, &response);

        let selection = self.selection(&choices);
        // Plan with placeholder text so the count shows before anything is typed.
        let preview_text = if self.text.trim().is_empty() {
            "."
        } else {
            &self.text
        };
        let plan = plan_broadcast(snapshot, &selection, preview_text);
        let n = plan.deliveries.len();
        ui.horizontal(|ui| {
            ui.label(format!(
                "Will send to {n} coordinator{}",
                if n == 1 { "" } else { "s" }
            ));
            let deduped = selection.run_ids.len().saturating_sub(plan.skipped.len());
            if deduped > n {
                ui.weak(format!(
                    "({deduped} runs; shared coordinators get one message)"
                ));
            }
        });
        let can_send = n > 0 && !self.text.trim().is_empty();
        if ui
            .add_enabled(
                can_send,
                crate::ui::widgets::primary_button(ui, can_send, "Send…"),
            )
            .clicked()
        {
            self.stage = Stage::Confirm(plan_broadcast(snapshot, &selection, &self.text));
        }
    }

    fn confirm(
        &mut self,
        ui: &mut egui::Ui,
        plan: BroadcastPlan,
        make_sender: impl FnOnce() -> Box<dyn BroadcastSender>,
    ) {
        let n = plan.deliveries.len();
        ui.label(
            RichText::new(format!(
                "Send this to {n} coordinator{}?",
                if n == 1 { "" } else { "s" }
            ))
            .strong(),
        );
        if let Some(first) = plan.deliveries.first() {
            let msg = &first.message;
            ui.group(|ui| {
                ui.label(msg.body.as_deref().unwrap_or(&msg.subject));
            });
        }
        for d in &plan.deliveries {
            ui.weak(format!(
                "→ {}  ({})",
                d.run_ids.join(", "),
                d.coordinator_handle
            ));
        }
        ui.horizontal(|ui| {
            if ui.button("Confirm send").clicked() {
                let repaint = ui.ctx().clone();
                let job = BroadcastJob::spawn(
                    make_sender(),
                    plan.deliveries.clone(),
                    Some(Box::new(move || repaint.request_repaint())),
                );
                self.stage = Stage::Sending { plan, job };
                self.sent = true;
            } else if ui.button("Back").clicked() {
                self.stage = Stage::Compose;
            }
        });
    }
}

fn results(ui: &mut egui::Ui, snapshot: &ClanSnapshot, plan: &BroadcastPlan, job: &BroadcastJob) {
    let ok = ui.visuals().widgets.noninteractive.fg_stroke.color;
    let err = ui.visuals().error_fg_color;
    for (index, delivery) in plan.deliveries.iter().enumerate() {
        let result = job.results().iter().find(|r| r.index == index);
        ui.horizontal(|ui| {
            match result.map(|r| &r.outcome) {
                None => ui.weak("…"),
                Some(Ok(_)) => ui.label(RichText::new("✔").color(ok)),
                Some(Err(_)) => ui.label(RichText::new("✖").color(err)),
            };
            let label = delivery
                .run_ids
                .iter()
                .map(|id| {
                    snapshot
                        .run(id)
                        .and_then(|r| r.objective.clone())
                        .unwrap_or_else(|| id.clone())
                })
                .collect::<Vec<_>>()
                .join(" + ");
            ui.add(egui::Label::new(label).truncate());
        });
        if let Some(Err(e)) = result.map(|r| &r.outcome) {
            ui.label(RichText::new(e).color(err).small());
        }
    }
    for skipped in &plan.skipped {
        ui.horizontal(|ui| {
            ui.weak("–");
            ui.weak(format!("{}: {}", skipped.run_id, skipped.reason.describe()));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::BroadcastSender;
    use crate::clan::ClanData;
    use crate::orca::{OutgoingMessage, Run, Terminal};

    struct NeverSender;

    impl BroadcastSender for NeverSender {
        fn send(&mut self, _: &OutgoingMessage) -> Result<Option<String>, String> {
            panic!("the smoke test never confirms a send");
        }
    }

    /// Draws every stage's frame path headlessly; nothing is sent.
    #[test]
    fn composer_draws_without_panicking() {
        let data = ClanData {
            terminals: vec![Terminal {
                handle: "c1".into(),
                worktree_path: Some("/u/orca/workspaces/shop/a".into()),
                ..Terminal::default()
            }],
            runs: vec![Run {
                id: "r1".into(),
                objective: Some("objective".into()),
                coordinator_handle: Some("c1".into()),
                ..Run::default()
            }],
            ..ClanData::default()
        };
        let snapshot = ClanSnapshot::build(&data, 0);
        let mut composer = BroadcastComposer::new();
        composer.text = "hello".into();
        let ctx = egui::Context::default();
        for stage in 0..3 {
            if stage == 1 {
                composer.stage = Stage::Confirm(plan_broadcast(
                    &snapshot,
                    &BroadcastSelection::new(["r1"]),
                    "hello",
                ));
            }
            let snap = (stage != 2).then_some(&snapshot);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                composer.ui(ui, snap, || Box::new(NeverSender));
            });
            output.textures_delta.clear();
        }
        assert!(matches!(composer.stage, Stage::Confirm(_)));
    }
}
