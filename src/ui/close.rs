//! "Close session" confirmation dialog, its inline results and the log of past closes.
//!
//! The dialog re-plans every frame from the newest snapshot, so the steps it shows are the
//! ones that run. Confirming hands the plan to the app, which submits its actions and reports
//! their ids through [`CloseDialog::started`]; results come back through
//! [`CloseDialog::on_result`].

use std::collections::{BTreeSet, HashMap};

use eframe::egui::{self, RichText};

use crate::actions::{ActionId, ActionResult};
use crate::close::{self, CloseOptions, ClosePlan, CloseStep};
use crate::model::{FleetSnapshot, NodeId};
use crate::orca::Terminal;
use crate::ui::graph::kind_label;

/// How many finished closes the log keeps.
const MAX_LOG: usize = 20;

/// What to close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseRequest {
    /// These nodes (one from the panel, graph menu or `x`).
    Nodes(Vec<NodeId>),
    /// Every node the filter shows that is a loose terminal or worker (and coordinators when
    /// ticked), re-evaluated while the dialog is open.
    Shown,
}

/// Where one step stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Ok(Option<String>),
    Failed(String),
}

/// A confirmed plan and how far it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseRun {
    pub plan: ClosePlan,
    /// Step descriptions, fixed when confirmed (the nodes vanish once closed).
    pub labels: Vec<String>,
    pub statuses: Vec<StepStatus>,
    pub started_ms: i64,
}

impl CloseRun {
    pub fn finished(&self) -> bool {
        !self.statuses.contains(&StepStatus::Pending)
    }

    pub fn failures(&self) -> usize {
        self.statuses
            .iter()
            .filter(|s| matches!(s, StepStatus::Failed(_)))
            .count()
    }

    /// "Closed a, b" / "Close of a: 1 of 2 steps failed".
    pub fn summary(&self) -> String {
        let names: Vec<&str> = self.plan.targets.iter().map(|t| t.label.as_str()).collect();
        let names = if names.len() > 3 {
            format!("{} and {} more", names[..3].join(", "), names.len() - 3)
        } else {
            names.join(", ")
        };
        match self.failures() {
            0 => format!("Closed {names}"),
            n => format!(
                "Close of {names}: {n} of {} steps failed",
                self.statuses.len()
            ),
        }
    }

    /// One line per step with its outcome.
    pub fn details(&self) -> Vec<String> {
        self.labels
            .iter()
            .zip(&self.statuses)
            .map(|(label, status)| match status {
                StepStatus::Pending => format!("… {label}"),
                StepStatus::Ok(None) => format!("✓ {label}"),
                StepStatus::Ok(Some(note)) => format!("✓ {label}: {note}"),
                StepStatus::Failed(err) => format!("✗ {label}: {err}"),
            })
            .collect()
    }
}

/// A finished close, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseLogEntry {
    pub at_ms: i64,
    pub summary: String,
    pub details: Vec<String>,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Dialog {
    request: CloseRequest,
    include_coordinators: bool,
    /// The run this dialog confirmed, once confirmed.
    run: Option<u64>,
}

/// Dialog state, runs in flight and the log.
#[derive(Debug, Default)]
pub struct CloseDialog {
    dialog: Option<Dialog>,
    runs: HashMap<u64, CloseRun>,
    next_run: u64,
    /// Submitted action → (run, step index).
    in_flight: HashMap<ActionId, (u64, usize)>,
    log: Vec<CloseLogEntry>,
}

impl CloseDialog {
    /// Opens the dialog for `request`, replacing any open one.
    pub fn request(&mut self, request: CloseRequest) {
        self.dialog = Some(Dialog {
            request,
            include_coordinators: false,
            run: None,
        });
    }

    pub fn is_open(&self) -> bool {
        self.dialog.is_some()
    }

    pub fn log(&self) -> &[CloseLogEntry] {
        &self.log
    }

    /// Records that `plan` was confirmed and its steps were submitted as `ids` (one per step).
    pub fn started(&mut self, plan: ClosePlan, labels: Vec<String>, ids: &[ActionId], now_ms: i64) {
        let run_id = self.next_run;
        self.next_run += 1;
        for (step, id) in ids.iter().enumerate() {
            self.in_flight.insert(*id, (run_id, step));
        }
        let statuses = vec![StepStatus::Pending; plan.steps.len()];
        self.runs.insert(
            run_id,
            CloseRun {
                plan,
                labels,
                statuses,
                started_ms: now_ms,
            },
        );
        if let Some(dialog) = &mut self.dialog {
            dialog.run = Some(run_id);
        }
        self.finish_if_done(run_id, now_ms);
    }

    /// Applies an action result; `true` when it finished a close (time to re-poll Orca).
    pub fn on_result(&mut self, result: &ActionResult, now_ms: i64) -> bool {
        let Some((run_id, step)) = self.in_flight.remove(&result.id) else {
            return false;
        };
        let Some(run) = self.runs.get_mut(&run_id) else {
            return false;
        };
        if let Some(status) = run.statuses.get_mut(step) {
            *status = match &result.outcome {
                Ok(()) => StepStatus::Ok(result.note.clone()),
                Err(err) => StepStatus::Failed(err.clone()),
            };
        }
        self.finish_if_done(run_id, now_ms)
    }

    fn finish_if_done(&mut self, run_id: u64, now_ms: i64) -> bool {
        let Some(run) = self.runs.get(&run_id) else {
            return false;
        };
        if !run.finished() {
            return false;
        }
        self.log.push(CloseLogEntry {
            at_ms: now_ms,
            summary: run.summary(),
            details: run.details(),
            ok: run.failures() == 0,
        });
        let excess = self.log.len().saturating_sub(MAX_LOG);
        self.log.drain(..excess);
        self.forget_unshown_runs();
        true
    }

    /// Drops finished runs the dialog no longer shows.
    fn forget_unshown_runs(&mut self) {
        let shown = self.dialog.as_ref().and_then(|d| d.run);
        self.runs
            .retain(|id, run| Some(*id) == shown || !run.finished());
    }

    /// Draws the dialog; returns the plan and its step labels when "Kill" was clicked.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        snapshot: Option<&FleetSnapshot>,
        terminals: &[Terminal],
        hidden: &BTreeSet<NodeId>,
    ) -> Option<(ClosePlan, Vec<String>)> {
        let dialog = self.dialog.as_mut()?;
        let mut open = true;
        let mut confirmed = None;
        let mut dismiss = false;
        let run = dialog.run.and_then(|id| self.runs.get(&id));
        // A finished run hides the dialog unless a step failed, so failures stay visible.
        if run.is_some_and(|r| r.finished() && r.failures() == 0) {
            self.dialog = None;
            self.forget_unshown_runs();
            return None;
        }
        let plan_with = |whole_tab: bool| match (run, snapshot) {
            (Some(_), _) => None,
            (None, Some(snapshot)) => Some(plan_request(
                &dialog.request,
                snapshot,
                terminals,
                hidden,
                CloseOptions { whole_tab },
                dialog.include_coordinators,
            )),
            (None, None) => Some(ClosePlan {
                refused: Some("no fleet data yet".into()),
                ..ClosePlan::default()
            }),
        };
        let plan = plan_with(false);
        let tab_plan = plan_with(true);
        let companions = match (&plan, snapshot) {
            (Some(plan), Some(snapshot)) => {
                close::tab_companions(&plan.targets, snapshot, terminals)
            }
            _ => Vec::new(),
        };
        let title = match &dialog.request {
            CloseRequest::Nodes(ids) if ids.len() == 1 => "Close session",
            _ => "Close sessions",
        };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| match (run, &plan, &tab_plan) {
                (Some(run), _, _) => run_view(ui, run),
                (None, Some(plan), Some(tab_plan)) => {
                    let labels = step_labels(plan, snapshot);
                    match confirm_view(ui, dialog, plan, tab_plan, &labels, &companions) {
                        Some(Choice::Kill) => confirmed = Some((plan.clone(), labels)),
                        Some(Choice::KillTab) => {
                            confirmed = Some((tab_plan.clone(), step_labels(tab_plan, snapshot)));
                        }
                        Some(Choice::Cancel) => dismiss = true,
                        None => {}
                    }
                }
                _ => {}
            });
        if !open || dismiss {
            self.dialog = None;
            self.forget_unshown_runs();
        }
        confirmed
    }

    /// `Closed (n)` menu in the top bar listing finished closes, newest first.
    pub fn log_menu(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if self.log.is_empty() {
            return;
        }
        let failed = self.log.iter().any(|e| !e.ok);
        let title = RichText::new(format!("Closed ({})", self.log.len()));
        let title = if failed {
            title.color(ui.visuals().warn_fg_color)
        } else {
            title
        };
        let mut clear = false;
        ui.menu_button(title, |ui| {
            for entry in self.log.iter().rev() {
                let age = crate::ui::table::format_age(now_ms - entry.at_ms);
                let text = format!("{} ({age} ago)", entry.summary);
                let text = if entry.ok {
                    RichText::new(text)
                } else {
                    RichText::new(text).color(ui.visuals().error_fg_color)
                };
                ui.label(text).on_hover_text(entry.details.join("\n"));
            }
            ui.separator();
            clear = ui.button("Clear").clicked();
        });
        if clear {
            self.log.clear();
        }
    }
}

/// The plan for `request` against the current snapshot.
pub fn plan_request(
    request: &CloseRequest,
    snapshot: &FleetSnapshot,
    terminals: &[Terminal],
    hidden: &BTreeSet<NodeId>,
    options: CloseOptions,
    include_coordinators: bool,
) -> ClosePlan {
    match request {
        CloseRequest::Nodes(ids) => {
            let nodes: Vec<_> = ids.iter().filter_map(|id| snapshot.node(id)).collect();
            match nodes.as_slice() {
                [] => ClosePlan {
                    refused: Some("the node is gone".into()),
                    ..ClosePlan::default()
                },
                [node] => close::plan_close(node, snapshot, terminals, options),
                _ => close::plan_bulk_close(nodes, snapshot, terminals, options),
            }
        }
        CloseRequest::Shown => close::plan_bulk_close(
            close::bulk_candidates(snapshot, hidden, include_coordinators),
            snapshot,
            terminals,
            options,
        ),
    }
}

/// Human descriptions of each step, naming the node behind a handle.
pub fn step_labels(plan: &ClosePlan, snapshot: Option<&FleetSnapshot>) -> Vec<String> {
    let name = |handle: &str| {
        snapshot
            .and_then(|s| s.node(&NodeId::terminal(handle)))
            .map_or_else(|| handle.to_owned(), |n| n.label.clone())
    };
    plan.steps
        .iter()
        .map(|step| match step {
            CloseStep::WorkerStop {
                dispatch_id,
                handle,
            } => match handle {
                Some(h) => format!("Stop dispatch {dispatch_id} of {}", name(h)),
                None => format!("Stop dispatch {dispatch_id}"),
            },
            CloseStep::CloseTerminal { handle, tab: false } => {
                format!("Close terminal {}", name(handle))
            }
            CloseStep::CloseTerminal { handle, tab: true } => {
                format!("Close the whole tab of {}", name(handle))
            }
        })
        .collect()
}

enum Choice {
    Kill,
    KillTab,
    Cancel,
}

/// The plan and its buttons. `tab_plan` is the same request closing whole tabs, and
/// `companions` the other sessions those tabs would take down with them.
fn confirm_view(
    ui: &mut egui::Ui,
    dialog: &mut Dialog,
    plan: &ClosePlan,
    tab_plan: &ClosePlan,
    labels: &[String],
    companions: &[String],
) -> Option<Choice> {
    let error = ui.visuals().error_fg_color;
    let warn = ui.visuals().warn_fg_color;
    if !plan.targets.is_empty() {
        ui.label(RichText::new(format!("Targets ({})", plan.targets.len())).strong());
        egui::ScrollArea::vertical()
            .id_salt("close_targets")
            .max_height(140.0)
            .show(ui, |ui| {
                for target in &plan.targets {
                    ui.label(format!("• {} ({})", target.label, kind_label(target.kind)));
                }
            });
    }
    if !plan.steps.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new("Steps").strong());
        egui::ScrollArea::vertical()
            .id_salt("close_steps")
            .max_height(160.0)
            .show(ui, |ui| {
                for (i, (step, label)) in plan.steps.iter().zip(labels).enumerate() {
                    ui.label(format!("{}. {label}", i + 1));
                    ui.label(RichText::new(step.command()).monospace().small().weak());
                }
            });
        if plan
            .steps
            .iter()
            .any(|s| matches!(s, CloseStep::WorkerStop { .. }))
        {
            ui.weak(
                "worker-stop fences the dispatch first and closes a supervised worker's pane \
                 itself; the close after it then reports \"already closed\".",
            );
        }
    }
    for warning in &plan.warnings {
        ui.colored_label(warn, format!("⚠ {warning}"));
    }
    if let Some(reason) = &plan.refused {
        ui.colored_label(error, format!("Can't close: {reason}"));
    }
    if dialog.request == CloseRequest::Shown {
        ui.add_space(6.0);
        ui.checkbox(&mut dialog.include_coordinators, "Include coordinators");
    }
    ui.add_space(6.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(RichText::new("Close whole tab").strong());
        if companions.is_empty() {
            ui.weak("Nothing else is in the tab; only the session above closes.");
        } else {
            ui.label(format!("Also closes {}:", companions.len()));
            for name in companions {
                ui.label(format!("• {name}"));
            }
        }
    });
    ui.add_space(6.0);
    let mut choice = None;
    ui.horizontal(|ui| {
        let red = egui::Color32::from_rgb(190, 40, 40);
        let text = match plan.targets.len() {
            1 => "Kill session".to_string(),
            n => format!("Kill {n} sessions"),
        };
        let kill =
            egui::Button::new(RichText::new(text).strong().color(egui::Color32::WHITE)).fill(red);
        if ui.add_enabled(plan.can_run(), kill).clicked() {
            choice = Some(Choice::Kill);
        }
        let tab = egui::Button::new(
            RichText::new("Close whole tab")
                .strong()
                .color(egui::Color32::WHITE),
        )
        .fill(red);
        if ui
            .add_enabled(tab_plan.can_run(), tab)
            .on_hover_text("orca terminal close --tab: closes every pane in the Orca tab")
            .clicked()
        {
            choice = Some(Choice::KillTab);
        }
        if ui.button("Cancel").clicked() {
            choice = Some(Choice::Cancel);
        }
    });
    choice
}

/// Progress of a confirmed close. A successful run hides the dialog on its own; a failed one
/// stays open until the window's × is clicked.
fn run_view(ui: &mut egui::Ui, run: &CloseRun) {
    let error = ui.visuals().error_fg_color;
    for (label, status) in run.labels.iter().zip(&run.statuses) {
        ui.horizontal_wrapped(|ui| match status {
            StepStatus::Pending => {
                ui.spinner();
                ui.label(label);
            }
            StepStatus::Ok(note) => {
                ui.label(format!("✓ {label}"));
                if let Some(note) = note {
                    ui.weak(note);
                }
            }
            StepStatus::Failed(err) => {
                ui.colored_label(error, format!("✗ {label}: {err}"));
            }
        });
    }
    if run.finished() {
        ui.add_space(6.0);
        ui.label(RichText::new(run.summary()).strong());
    }
}

#[cfg(test)]
mod tests;
