//! The app side of the control API: publishes the [`StatusView`] and runs queued commands.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use crossbeam_channel::Receiver;
use eframe::egui;
use serde_json::{Value, json};

use super::{MyrstackApp, View, now_ms, repaint_once};
use crate::actions::{Action, ActionId, ActionResult, Key};
use crate::broadcast::{BroadcastSelection, plan_broadcast};
use crate::close::{CloseOptions, ClosePlan, plan_bulk_close, plan_close};
use crate::control::commands::deliver_broadcast;
use crate::control::view::{filter_view, kind_name, view_name, whip_settings_json};
use crate::control::{
    AppCommands, Backend, ControlCommand, ControlError, ControlServer, ErrorCode, PendingCommand,
    Reply, SharedStatus, StatusView, TerminalKey, ViewInputs,
};
use crate::model::{Node, NodeId};
use crate::orca::{OrcaClient, TerminalInput};
use crate::poller::PollerCommand;
use crate::traffic::{AgentMessage, MessageKind};
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
    /// Actions submitted for the API, by action id → the group they belong to.
    in_flight: HashMap<ActionId, u64>,
    /// Commands waiting for their actions (a send has one, a close one per step).
    groups: HashMap<u64, ActionGroup>,
    next_group: u64,
}

/// A finished action and its outcome (the runner's note on success).
type StepOutcome = (Action, Result<Option<String>, String>);

/// One command's actions; the reply goes out when the last one finishes.
struct ActionGroup {
    reply: Reply,
    /// Returned on success, with `steps` added for multi-step commands.
    context: Value,
    /// One slot per action, in submission order.
    outcomes: Vec<Option<StepOutcome>>,
    /// Report every step (a close) rather than just success.
    report_steps: bool,
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
    alerts: u64,
    view: View,
    selected: Option<NodeId>,
    selection: BTreeSet<NodeId>,
    filter: FleetFilter,
    whip_window_open: bool,
    playbook: u64,
    queen_auto: bool,
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
            in_flight: HashMap::new(),
            groups: HashMap::new(),
            next_group: 1,
        }
    }
}

impl AppControl {
    pub(super) fn typing_pending(&self, handle: &str) -> bool {
        self.groups
            .values()
            .any(|g| g.context["handle"] == handle && g.context["sent"] == "send")
    }

    /// Whether the control API is serving its socket.
    pub(super) fn listening(&self) -> bool {
        self._server.is_some()
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
            alerts: self.notifications.generation(),
            view: self.view,
            selected: self.selected.clone(),
            selection: self.graph.selection(),
            filter: self.filter.clone(),
            whip_window_open: self.whip_window.open,
            playbook: self.playbook_watch.generation,
            queen_auto: self.chat.auto_mode(),
        }
    }

    /// Publishes a new view for the control API when anything it shows changed.
    pub(super) fn publish_control_status(&mut self) {
        let fingerprint = self.control_fingerprint();
        if self.control.published.as_ref() == Some(&fingerprint) {
            return;
        }
        let mut view = StatusView::build(&ViewInputs {
            snapshot: self.snapshot.as_ref(),
            data: self.data.as_ref(),
            claude: self.claude_data.as_ref(),
            whip_status: &self.whip_status,
            whip_settings: self.whip_window.whip(),
            whip_log: self.whip_log.entries().iter().collect(),
            quotas: &self.quotas,
            alerts: self.notifications.entries(),
            view: self.view,
            selected: self.selected.as_ref().map(NodeId::as_str),
            selection: fingerprint.selection.iter().map(NodeId::as_str).collect(),
            filter: &self.filter,
            whip_settings_open: self.whip_window.open,
            now_ms: now_ms(),
        });
        if let Some(status) = self.playbook_status() {
            view.playbook = status.view();
        }
        view.queen_auto = self.chat.auto_mode();
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

    /// Records an API action's result; answers its command once all its actions finished.
    pub(super) fn control_action_done(&mut self, result: &ActionResult) {
        let Some(group_id) = self.control.in_flight.remove(&result.id) else {
            return;
        };
        if Some(result.action.handle()) == self.watched_screen.as_deref() {
            self.poller.send(PollerCommand::RefreshScreen);
        }
        let Some(group) = self.control.groups.get_mut(&group_id) else {
            return;
        };
        let slot = group.outcomes.iter_mut().find(|o| o.is_none());
        if let Some(slot) = slot {
            let outcome = result.outcome.clone().map(|()| result.note.clone());
            *slot = Some((result.action.clone(), outcome));
        }
        if group.outcomes.iter().any(Option::is_none) {
            return;
        }
        if let Some(group) = self.control.groups.remove(&group_id) {
            group.finish();
        }
    }

    /// Submits `actions` for one command; its reply goes out when they all finish.
    pub(super) fn submit_control_actions(
        &mut self,
        actions: Vec<Action>,
        reply: Reply,
        context: Value,
        report_steps: bool,
    ) -> Vec<ActionId> {
        let group_id = self.control.next_group;
        self.control.next_group += 1;
        let mut ids = Vec::new();
        for action in actions {
            let id = self.actions.submit(action);
            self.control.in_flight.insert(id, group_id);
            ids.push(id);
        }
        self.control.groups.insert(
            group_id,
            ActionGroup {
                reply,
                context,
                outcomes: vec![None; ids.len()],
                report_steps,
            },
        );
        ids
    }

    /// Carries out one command. Replies immediately, or later for terminal sends and broadcasts.
    /// The returned error is only for logging; it has been sent as the reply.
    pub(super) fn run_control_command(
        &mut self,
        ctx: &egui::Context,
        command: ControlCommand,
        reply: Reply,
    ) -> Result<(), ControlError> {
        let result = match command {
            ControlCommand::Send {
                agent,
                text,
                enter,
                from,
            } => {
                if !text.is_empty() {
                    let from = from.unwrap_or_else(|| "queen".into());
                    let kind = match from.as_str() {
                        "queen" => MessageKind::Instruction,
                        _ => MessageKind::Message,
                    };
                    self.traffic
                        .push(AgentMessage::new(from, agent.clone(), kind));
                }
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
            ControlCommand::SelectMany { agents } => self.control_select_many(&agents),
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
                    let hidden = self.graph.hidden().contains(&id);
                    self.focus_in_graph(id.clone());
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
            ControlCommand::Close {
                agents,
                whole_tab,
                dry_run,
            } => return self.control_close(&agents, whole_tab, dry_run, reply),
            ControlCommand::Whip { agents, stalled } => {
                return self.control_whip(&agents, stalled, reply);
            }
            ControlCommand::Envoy(command) => return self.control_envoy(ctx, command, reply),
            ControlCommand::Review(command) => self.control_review(command),
            ControlCommand::Spawn(args) => return self.control_spawn(args, reply),
            ControlCommand::Notify(notice) => {
                let kind = match notice.severity {
                    crate::notify::Severity::Info => MessageKind::Status,
                    _ => MessageKind::Ask,
                };
                self.traffic_to_karl(&notice.origin, notice.agent.as_deref(), kind);
                self.control_notify(notice)
            }
            ControlCommand::Ask(ask) => {
                self.traffic_to_karl(&ask.origin, None, MessageKind::Ask);
                self.control_ask(ask)
            }
            ControlCommand::AgentMessage(message) => {
                self.traffic.push(message);
                Ok(json!({ "drawn": true }))
            }
            ControlCommand::Finish(request) => self.control_finish(request),
        };
        let error = result.as_ref().err().cloned();
        reply.send(result);
        ctx.request_repaint();
        error.map_or(Ok(()), Err)
    }

    /// The snapshot node the API means by `key` (node id, terminal handle or label).
    /// Selects several nodes; the first is the primary. Nodes hidden by the filter can't be
    /// part of a multi-selection and are reported in `hidden_by_filter`.
    fn control_select_many(&mut self, agents: &[String]) -> Result<Value, ControlError> {
        let mut ids = Vec::new();
        for agent in agents {
            let id = self.control_node(agent)?.id.clone();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let hidden: Vec<&NodeId> = ids.iter().filter(|id| !self.graph.is_visible(id)).collect();
        let hidden = json!(hidden);
        let set: BTreeSet<NodeId> = ids.iter().cloned().collect();
        self.graph.set_selection(&set, ids.first());
        self.selected = self.graph.selected().or_else(|| ids.first().cloned());
        Ok(json!({
            "selected": self.selected,
            "selection": self.graph.selection(),
            "hidden_by_filter": hidden,
        }))
    }

    pub(super) fn control_node(&self, key: &str) -> Result<&Node, ControlError> {
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
        let action = Action::Send {
            handle: handle.clone(),
            input,
        };
        let context = json!({ "agent": id, "handle": handle, "sent": what });
        let _ = self.submit_control_actions(vec![action], reply, context, false);
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
        self.ant_farm.pulse(crate::ui::ant_farm::Pulse::Broadcast);
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

impl ActionGroup {
    fn finish(self) {
        let failures: Vec<String> = self
            .outcomes
            .iter()
            .flatten()
            .filter_map(|(action, outcome)| {
                let err = outcome.as_ref().err()?;
                Some(format!("{}: {err}", action.handle()))
            })
            .collect();
        if !self.report_steps {
            let result = if failures.is_empty() {
                Ok(self.context)
            } else {
                Err(ControlError::failed(failures.join("; ")))
            };
            self.reply.send(result);
            return;
        }
        let steps: Vec<Value> = self
            .outcomes
            .iter()
            .flatten()
            .map(|(action, outcome)| {
                let command = format!("orca {}", action.cli_args().join(" "));
                match outcome {
                    Ok(note) => json!({ "command": command, "ok": true, "note": note }),
                    Err(error) => json!({ "command": command, "ok": false, "error": error }),
                }
            })
            .collect();
        let mut context = self.context;
        if let Value::Object(map) = &mut context {
            map.insert("ok".into(), json!(failures.is_empty()));
            map.insert("steps".into(), Value::Array(steps));
        }
        self.reply.send(Ok(context));
    }
}

/// Closing sessions for the API: the same plans the "Close session" dialog confirms.
impl MyrstackApp {
    fn control_close(
        &mut self,
        agents: &[String],
        whole_tab: bool,
        dry_run: bool,
        reply: Reply,
    ) -> Result<(), ControlError> {
        let plan = self.control_close_plan(agents, whole_tab);
        let (plan, labels) = match plan {
            Ok(p) => p,
            Err(e) => {
                reply.send(Err(e.clone()));
                return Err(e);
            }
        };
        let described = plan_json(&plan, &labels);
        if dry_run {
            reply.send(Ok(json!({ "dry_run": true, "plan": described })));
            return Ok(());
        }
        if !plan.can_run() {
            let reason = plan
                .refused
                .clone()
                .unwrap_or_else(|| "nothing to close".into());
            let e = ControlError::new(ErrorCode::Refused, reason);
            reply.send(Err(e.clone()));
            return Err(e);
        }
        log::info!("control API: closing sessions: {:?}", plan.steps);
        let ids = self.submit_control_actions(
            plan.actions(),
            reply,
            json!({ "dry_run": false, "plan": described }),
            true,
        );
        // Show the close in the UI's close log, like a close confirmed in the dialog.
        self.close.started(plan, labels, &ids, now_ms());
        Ok(())
    }

    /// The close plan for `agents` and its step labels.
    fn control_close_plan(
        &self,
        agents: &[String],
        whole_tab: bool,
    ) -> Result<(ClosePlan, Vec<String>), ControlError> {
        let nodes = agents
            .iter()
            .map(|a| self.control_node(a))
            .collect::<Result<Vec<&Node>, _>>()?;
        let snapshot = self.snapshot.as_ref().ok_or_else(|| {
            ControlError::new(ErrorCode::Unavailable, "the app has no fleet data yet")
        })?;
        let terminals = self.data.as_ref().map_or(&[][..], |d| &d.terminals[..]);
        let opts = CloseOptions { whole_tab };
        let plan = match nodes.as_slice() {
            [node] => plan_close(node, snapshot, terminals, opts),
            _ => plan_bulk_close(nodes.iter().copied(), snapshot, terminals, opts),
        };
        let labels = crate::ui::close::step_labels(&plan, Some(snapshot));
        Ok((plan, labels))
    }
}

pub(super) fn plan_json(plan: &ClosePlan, labels: &[String]) -> Value {
    json!({
        "targets": plan.targets.iter().map(|t| json!({
            "id": t.id,
            "label": t.label,
            "kind": kind_name(t.kind),
        })).collect::<Vec<_>>(),
        "steps": plan.steps.iter().enumerate().map(|(i, step)| json!({
            "command": step.command(),
            "label": labels.get(i),
        })).collect::<Vec<_>>(),
        "warnings": plan.warnings,
        "refused": plan.refused,
        "can_run": plan.can_run(),
    })
}
