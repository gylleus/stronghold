//! The app side of the control API: publishes the [`StatusView`] and runs queued commands.

use std::collections::HashMap;
use std::sync::Arc;

use crossbeam_channel::Receiver;
use eframe::egui;
use serde_json::{Value, json};

use super::{MyrstackApp, View, now_ms, repaint_once};
use crate::actions::{Action, ActionId, ActionResult, Key};
use crate::broadcast::{BroadcastSelection, plan_broadcast};
use crate::control::commands::deliver_broadcast;
use crate::control::view::{filter_view, view_name, whip_settings_json};
use crate::control::{
    AppCommands, Backend, ControlCommand, ControlError, ControlServer, ErrorCode, PendingCommand,
    Reply, SharedStatus, StatusView, TerminalKey, ViewInputs,
};
use crate::model::{Node, NodeId};
use crate::orca::{OrcaClient, TerminalInput};
use crate::poller::PollerCommand;
use crate::ui::sidebar::FleetFilter;
use crate::whip::WhipSettings;

/// The control server, the published view and commands in flight.
pub(super) struct AppControl {
    /// `None` when the socket could not be bound (e.g. another Myrstack is running).
    _server: Option<ControlServer>,
    status: SharedStatus,
    commands: Receiver<PendingCommand>,
    /// What the last published view was built from.
    published: Option<Fingerprint>,
    /// Terminal sends waiting for the action runner, with the reply's context.
    pending: HashMap<ActionId, (Reply, Value)>,
}

/// Cheap summary of the state a view is built from; a new view is published when it changes.
#[derive(Debug, Clone, PartialEq)]
struct Fingerprint {
    built_at_ms: Option<i64>,
    polled_at_ms: Option<i64>,
    whip_tick_ms: Option<i64>,
    whip_log_seq: Option<u64>,
    whip_settings: WhipSettings,
    quotas_at_ms: i64,
    alerts: (usize, Option<i64>),
    view: View,
    selected: Option<NodeId>,
    filter: FleetFilter,
    whip_window_open: bool,
}

impl AppControl {
    /// Starts the server on `<data dir>/control.sock`; logs and carries on without it on error.
    pub(super) fn start(ctx: &egui::Context) -> Self {
        let status = SharedStatus::new();
        let (commands, rx) = AppCommands::channel(Some(repaint_once(ctx)));
        let backend = Backend {
            status: status.clone(),
            screens: Arc::new(OrcaClient::new()),
            commands: Arc::new(commands),
        };
        let path = crate::control::socket_path();
        let server = ControlServer::start(&path, backend)
            .inspect_err(|e| log::warn!("control API disabled ({}): {e}", path.display()))
            .ok();
        Self {
            _server: server,
            status,
            commands: rx,
            published: None,
            pending: HashMap::new(),
        }
    }
}

impl MyrstackApp {
    fn control_fingerprint(&self) -> Fingerprint {
        Fingerprint {
            built_at_ms: self.snapshot.as_ref().map(|s| s.built_at_ms),
            polled_at_ms: self.data.as_ref().map(|d| d.polled_at_ms),
            whip_tick_ms: self.whip_status.last_tick_ms,
            whip_log_seq: self.whip_log.entries().back().map(|i| i.seq),
            whip_settings: self.whip_window.whip().clone(),
            quotas_at_ms: self.quotas.fetched_at_ms,
            alerts: (self.alerts.len(), self.alerts.last().map(|a| a.at_ms)),
            view: self.view,
            selected: self.selected.clone(),
            filter: self.filter.clone(),
            whip_window_open: self.whip_window.open,
        }
    }

    /// Publishes a new view for the control API when anything it shows changed.
    pub(super) fn publish_control_status(&mut self) {
        let fingerprint = self.control_fingerprint();
        if self.control.published.as_ref() == Some(&fingerprint) {
            return;
        }
        let view = StatusView::build(&ViewInputs {
            snapshot: self.snapshot.as_ref(),
            data: self.data.as_ref(),
            claude: self.claude_data.as_ref(),
            whip_status: &self.whip_status,
            whip_settings: self.whip_window.whip(),
            whip_log: self.whip_log.entries().iter().collect(),
            quotas: &self.quotas,
            alerts: &self.alerts,
            view: self.view,
            selected: self.selected.as_ref().map(NodeId::as_str),
            filter: &self.filter,
            whip_settings_open: self.whip_window.open,
            now_ms: now_ms(),
        });
        self.control.status.publish(view);
        self.control.published = Some(fingerprint);
    }

    /// Runs the commands the control API queued since the last frame.
    pub(super) fn poll_control(&mut self, ctx: &egui::Context) {
        let queued: Vec<PendingCommand> = self.control.commands.try_iter().collect();
        for PendingCommand { command, reply } in queued {
            log::info!("control API: {command:?}");
            if let Err(e) = self.run_control_command(ctx, command, reply) {
                log::debug!("control API: command failed: {e}");
            }
        }
    }

    /// Answers the control API's terminal sends when their action finishes.
    pub(super) fn control_action_done(&mut self, result: &ActionResult) {
        let Some((reply, context)) = self.control.pending.remove(&result.id) else {
            return;
        };
        if Some(result.action.handle()) == self.watched_screen.as_deref() {
            self.poller.send(PollerCommand::RefreshScreen);
        }
        reply.send(match &result.outcome {
            Ok(()) => Ok(context),
            Err(message) => Err(ControlError::failed(message.clone())),
        });
    }

    /// Carries out one command. Replies immediately, or later for terminal sends and broadcasts.
    /// The returned error is only for logging; it has been sent as the reply.
    fn run_control_command(
        &mut self,
        ctx: &egui::Context,
        command: ControlCommand,
        reply: Reply,
    ) -> Result<(), ControlError> {
        let result = match command {
            ControlCommand::Send { agent, text, enter } => {
                let input = TerminalInput {
                    text: (!text.is_empty()).then_some(text),
                    enter,
                    interrupt: false,
                };
                return self.control_terminal_input(&agent, input, "send", reply);
            }
            ControlCommand::Key { agent, key } => {
                let (input, name) = match key {
                    TerminalKey::Enter => (TerminalInput::enter_key(), "enter"),
                    TerminalKey::Escape => (TerminalInput::key(Key::Escape), "esc"),
                    TerminalKey::Interrupt => (TerminalInput::interrupt_key(), "interrupt"),
                };
                return self.control_terminal_input(&agent, input, name, reply);
            }
            ControlCommand::Broadcast { text, runs } => {
                return self.control_broadcast(&text, runs, reply);
            }
            ControlCommand::WhipSettings {
                enabled,
                dry_run,
                notify,
            } => self.control_whip_settings(enabled, dry_run, notify),
            ControlCommand::Select { agent: None } => {
                self.selected = None;
                self.graph.set_selected(None);
                Ok(json!({ "selected": null }))
            }
            ControlCommand::Select { agent: Some(agent) } => self
                .control_node(&agent)
                .map(|node| node.id.clone())
                .map(|id| {
                    self.graph.set_selected(Some(&id));
                    self.selected = Some(id.clone());
                    json!({ "selected": id })
                }),
            ControlCommand::Focus { agent, raise } => self
                .control_node(&agent)
                .map(|node| node.id.clone())
                .map(|id| {
                    self.view = View::Graph;
                    let hidden = self.graph.hidden().contains(&id);
                    self.select_and_focus(id.clone());
                    if raise {
                        for cmd in [
                            egui::ViewportCommand::Minimized(false),
                            egui::ViewportCommand::Visible(true),
                            egui::ViewportCommand::Focus,
                        ] {
                            ctx.send_viewport_cmd(cmd);
                        }
                    }
                    json!({ "selected": id, "hidden_by_filter": hidden })
                }),
            ControlCommand::Filter(change) => {
                change.apply(&mut self.filter);
                if let Some(snapshot) = &self.snapshot {
                    self.graph.set_hidden(&self.filter.hidden(snapshot));
                }
                let empty = crate::model::FleetSnapshot::default();
                let snapshot = self.snapshot.as_ref().unwrap_or(&empty);
                Ok(json!({ "filter": filter_view(&self.filter, snapshot) }))
            }
            ControlCommand::SetView(view) => {
                self.view = view;
                Ok(json!({ "view": view_name(view) }))
            }
            ControlCommand::Fit => {
                self.view = View::Graph;
                self.graph.fit_to_screen();
                Ok(json!({ "view": "graph" }))
            }
            ControlCommand::OpenWhipSettings => {
                self.whip_window.open_window();
                Ok(json!({ "open": true }))
            }
            // TODO(next bead): route to the close/kill session action once it lands.
            ControlCommand::Close { agent } => Err(ControlError::new(
                ErrorCode::NotSupported,
                format!("closing agents is not supported yet (asked to close {agent:?})"),
            )),
        };
        let error = result.as_ref().err().cloned();
        reply.send(result);
        ctx.request_repaint();
        error.map_or(Ok(()), Err)
    }

    /// The snapshot node the API means by `key` (node id, terminal handle or label).
    fn control_node(&self, key: &str) -> Result<&Node, ControlError> {
        let snapshot = self.snapshot.as_ref().ok_or_else(|| {
            ControlError::new(ErrorCode::Unavailable, "the app has no fleet data yet")
        })?;
        let view = self.control.status.current();
        let id = view
            .find_agent(key)
            .map(|a| NodeId(a.id.clone()))
            .unwrap_or_else(|| NodeId(key.to_owned()));
        snapshot
            .node(&id)
            .ok_or_else(|| ControlError::not_found(format!("no agent {key:?}")))
    }

    fn control_terminal_input(
        &mut self,
        agent: &str,
        input: TerminalInput,
        what: &str,
        reply: Reply,
    ) -> Result<(), ControlError> {
        let target = self.control_node(agent).and_then(|node| {
            let handle = crate::ui::terminal::watch_handle(node).ok_or_else(|| {
                ControlError::invalid_params(format!(
                    "{} has no live terminal to type into",
                    node.id
                ))
            })?;
            Ok((node.id.clone(), handle.to_owned()))
        });
        let (id, handle) = match target {
            Ok(t) => t,
            Err(e) => {
                reply.send(Err(e.clone()));
                return Err(e);
            }
        };
        let action_id = self.actions.submit(Action::Send {
            handle: handle.clone(),
            input,
        });
        let context = json!({ "agent": id, "handle": handle, "sent": what });
        self.control.pending.insert(action_id, (reply, context));
        Ok(())
    }

    fn control_broadcast(
        &mut self,
        text: &str,
        runs: Option<Vec<String>>,
        reply: Reply,
    ) -> Result<(), ControlError> {
        let Some(snapshot) = &self.snapshot else {
            let e = ControlError::new(ErrorCode::Unavailable, "the app has no fleet data yet");
            reply.send(Err(e.clone()));
            return Err(e);
        };
        let runs = runs.unwrap_or_else(|| snapshot.runs.iter().map(|r| r.id.clone()).collect());
        let plan = plan_broadcast(snapshot, &BroadcastSelection::new(runs), text);
        if plan.deliveries.is_empty() {
            let skipped: Vec<String> = plan
                .skipped
                .iter()
                .map(|s| format!("{} ({})", s.run_id, s.reason.describe()))
                .collect();
            let e = ControlError::not_found(format!(
                "no live coordinator to deliver to; skipped: {}",
                if skipped.is_empty() {
                    "none".into()
                } else {
                    skipped.join(", ")
                }
            ));
            reply.send(Err(e.clone()));
            return Err(e);
        }
        let spawned = std::thread::Builder::new()
            .name("control-broadcast".into())
            .spawn(move || {
                let mut client = OrcaClient::new();
                reply.send(Ok(deliver_broadcast(&mut client, &plan)));
            });
        spawned.map(|_| ()).map_err(|e| {
            log::warn!("control API: cannot spawn broadcast thread: {e}");
            ControlError::failed(format!("cannot spawn broadcast thread: {e}"))
        })
    }

    fn control_whip_settings(
        &mut self,
        enabled: Option<bool>,
        dry_run: Option<bool>,
        notify: Option<bool>,
    ) -> Result<Value, ControlError> {
        let mut next = self.whip_window.whip().clone();
        if let Some(v) = enabled {
            next.enabled = v;
        }
        if let Some(v) = dry_run {
            next.dry_run = v;
        }
        if let Some(v) = notify {
            next.notify = v;
        }
        if &next == self.whip_window.whip() {
            return Ok(json!({
                "changed": false,
                "settings": whip_settings_json(&next),
            }));
        }
        let message = self
            .whip_window
            .apply(next)
            .map_err(ControlError::invalid_params)?;
        let applied = self.whip_window.whip().clone();
        self.whip.update_settings(applied.clone());
        Ok(json!({
            "changed": true,
            "message": message,
            "settings": whip_settings_json(&applied),
        }))
    }
}
