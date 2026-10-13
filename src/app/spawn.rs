//! The app side of starting sessions: the "New dwarf…" dialog, spawns requested through the
//! control API (the King's `spawn_session`), and selecting the new node once it appears (only
//! with "Focus new dwarves when they're spawned" on).

use std::collections::HashMap;

use eframe::egui;

use super::{MyrstackApp, now_ms, repaint_once};
use crate::control::{ControlError, ErrorCode, Reply};
use crate::poller::PollerCommand;
use crate::spawn::exec::{Job, JobId, OrcaBackend};
use crate::spawn::pending::{PendingSelect, take_ready};
use crate::spawn::resolve::outcome_json;
use crate::spawn::{LiveAgent, SpawnArgs, SpawnEvent, SpawnRunner};
use crate::ui::spawn::{SpawnClan, SpawnDialog};

/// The spawn runner, the dialog, API spawns waiting for their reply, and the node to select.
pub(super) struct AppSpawn {
    runner: SpawnRunner,
    pub(super) dialog: SpawnDialog,
    replies: HashMap<JobId, Reply>,
    pending: Option<PendingSelect>,
}

impl AppSpawn {
    pub(super) fn start(ctx: &egui::Context) -> Self {
        Self {
            runner: SpawnRunner::spawn(OrcaBackend::default(), Some(repaint_once(ctx))),
            dialog: SpawnDialog::default(),
            replies: HashMap::new(),
            pending: None,
        }
    }
}

impl MyrstackApp {
    /// Opens "New dwarf…", with the sidebar repo `repo` picked.
    pub(super) fn open_spawn_dialog(&mut self, repo: Option<&str>) {
        self.spawn.dialog.open(repo);
    }

    /// Draws the dialog and launches its plan when asked.
    pub(super) fn spawn_dialog(&mut self, ctx: &egui::Context) {
        if !self.spawn.dialog.open {
            return;
        }
        let clan = SpawnClan::from_data(self.data.as_ref()).with_agents(self.snapshot.as_ref());
        let dwarves = self.whip_window.dwarves().clone();
        if let Some(plan) = self.spawn.dialog.show(ctx, &clan, &dwarves) {
            log::info!("spawn: launching {:?}", plan.steps);
            let job = self.spawn.runner.submit(Job::Plan(plan.clone()));
            self.spawn.dialog.started(job, plan);
        }
    }

    /// Takes the runner's events: dialog progress, API replies, refresh and, when the setting
    /// asks for it, the pending select.
    pub(super) fn poll_spawn(&mut self) {
        for event in self.spawn.runner.poll() {
            self.spawn.dialog.on_event(&event);
            let SpawnEvent::Done { job, result } = event else {
                continue;
            };
            let context = match &result {
                Ok(outcome) => Some(&outcome.context),
                Err(failure) => failure.step.map(|_| &*failure.context),
            };
            if let Some(context) = context {
                self.poller.send(PollerCommand::RefreshNow);
                if let Some(handle) = &context.handle {
                    self.spawn.pending = self.whip_window.spawn().pending_select(handle, now_ms());
                }
            }
            if let Some(reply) = self.spawn.replies.remove(&job) {
                reply.send(match &result {
                    Ok(outcome) => Ok(outcome_json(outcome)),
                    Err(failure) => Err(ControlError::failed(failure.to_string())),
                });
            }
        }
    }

    /// Selects and focuses the spawned session's node once the snapshot has it.
    pub(super) fn spawn_select_pending(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if let Some(id) = take_ready(&mut self.spawn.pending, snapshot, now_ms()) {
            log::info!("spawn: selecting {id}");
            self.select_and_focus(id);
        }
    }

    /// The control API's `spawn`: resolves and runs on the spawn thread; replies when done.
    pub(super) fn control_spawn(
        &mut self,
        mut args: SpawnArgs,
        reply: Reply,
    ) -> Result<(), ControlError> {
        let Some(data) = &self.data else {
            let e = ControlError::new(ErrorCode::Unavailable, "the app has no clan data yet");
            reply.send(Err(e.clone()));
            return Err(e);
        };
        if let Some(key) = args.terminal.clone() {
            let handle = if data.terminal(&key).is_some() {
                Ok(key)
            } else {
                self.control_node(&key).and_then(|node| {
                    crate::ui::terminal::watch_handle(node)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            ControlError::invalid_params(format!(
                                "{} has no live terminal",
                                node.id
                            ))
                        })
                })
            };
            match handle {
                Ok(handle) => args.terminal = Some(handle),
                Err(e) => {
                    reply.send(Err(e.clone()));
                    return Err(e);
                }
            }
        }
        self.whip_window
            .dwarves()
            .fill(args.agent, &mut args.model, &mut args.effort);
        let job = Job::Args {
            args,
            runs: data.runs.clone(),
            terminals: data.terminals.clone(),
            agents: self
                .snapshot
                .as_ref()
                .map(LiveAgent::from_snapshot)
                .unwrap_or_default(),
        };
        let id = self.spawn.runner.submit(job);
        self.spawn.replies.insert(id, reply);
        Ok(())
    }
}
