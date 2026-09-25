//! The eframe `App`: owns UI state and draws the panels each frame.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;

use crate::APP_NAME;
use crate::actions::{ActionRunner, OrcaExecutor};
use crate::claude::{ClaudeData, ClaudeWorker, SubagentInfo, TranscriptTail};
use crate::detect::detect_prompt;
use crate::fleet::FleetData;
use crate::hotkey::Hotkey;
use crate::model::{FleetSnapshot, Node, NodeId, NodeKind, NodeState};
use crate::notify::{self, Delivered, NeedsInputDetector, Notifier};
use crate::orca::OrcaClient;
use crate::persist::PersistedState;
use crate::poller::{Poller, PollerCommand, PollerConfig, ScreenUpdate};
use crate::ui;
use crate::ui::graph::GraphView;
use crate::ui::sidebar::FleetFilter;
use crate::whip::{self, WhipInput, WhipRunner, WhipStatus};

/// How often the UI redraws without new data, so relative times keep ticking.
const TICK: Duration = Duration::from_secs(1);
/// How often UI state is saved while running (it is also saved on exit).
const AUTOSAVE: Duration = Duration::from_secs(5);

/// Which view fills the central panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum View {
    #[default]
    Graph,
    Table,
}

/// Top-level application state.
pub struct MyrstackApp {
    /// Global show-window hotkey; `None` if registration failed.
    hotkey: Option<Hotkey>,
    poller: Poller,
    /// Newest snapshot; `None` until the first poll completes.
    data: Option<FleetData>,
    /// Graph model built from `data`.
    snapshot: Option<FleetSnapshot>,
    graph: GraphView,
    /// Node selected in the graph, shared by all views and side panels.
    selected: Option<NodeId>,
    view: View,
    /// Terminal whose screen the poller is reading (the selected node's, if live).
    watched_screen: Option<String>,
    /// Newest screen of `watched_screen`.
    screen: Option<ScreenUpdate>,
    /// Which nodes the graph shows; edited in the sidebar.
    filter: FleetFilter,
    /// Maps terminals to Claude sessions and scans subagents off the UI thread.
    claude: ClaudeWorker,
    claude_data: Option<ClaudeData>,
    /// Tail of the selected subagent's transcript.
    transcript: Option<TranscriptTail>,
    broadcast: ui::broadcast::BroadcastWindow,
    /// Runs terminal sends and tab switches off the UI thread.
    actions: ActionRunner,
    /// Input box drafts and send statuses, per terminal.
    input: ui::terminal::InputPanelState,
    /// Cmd+K node search.
    search: ui::search::SearchPalette,
    /// Keyboard shortcut overlay.
    help_open: bool,
    /// Runs the whip on its own thread against the newest data.
    whip: WhipRunner,
    whip_status: WhipStatus,
    whip_window: ui::settings::WhipSettingsWindow,
    /// Rate-limited macOS notifications, shared with the whip's executors.
    notifier: Arc<Notifier>,
    needs_input_detector: NeedsInputDetector,
    /// Posted notifications, newest last; the top bar lists them so they can select their node.
    alerts: Vec<Delivered>,
}

/// How many posted notifications the alerts menu keeps.
const MAX_ALERTS: usize = 10;

impl MyrstackApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let hotkey = Hotkey::register(&cc.egui_ctx)
            .inspect_err(|err| log::warn!("global hotkey unavailable: {err:#}"))
            .ok();
        let repaint_ctx = cc.egui_ctx.clone();
        let claude = ClaudeWorker::spawn(Some(Box::new(move || repaint_ctx.request_repaint())));
        let repaint_ctx = cc.egui_ctx.clone();
        let actions = ActionRunner::spawn(
            OrcaExecutor::default(),
            Some(Box::new(move || repaint_ctx.request_repaint())),
        );
        let repaint_ctx = cc.egui_ctx.clone();
        let poller = Poller::spawn(
            OrcaClient::new(),
            PollerConfig::default(),
            Some(Box::new(move || repaint_ctx.request_repaint())),
        );
        let whip_window = ui::settings::WhipSettingsWindow::load(whip::Settings::path());
        let repaint_ctx = cc.egui_ctx.clone();
        let notifier = Arc::new(Notifier::spawn(Some(Box::new(move || {
            repaint_ctx.request_repaint();
        }))));
        let whip_notifier = notifier.clone();
        let repaint_ctx = cc.egui_ctx.clone();
        let whip = WhipRunner::spawn(
            whip_window.whip().clone(),
            Box::new(move || {
                Box::new(whip::OrcaExecutor::default().with_notifier(whip_notifier.clone()))
            }),
            Some(Box::new(move || repaint_ctx.request_repaint())),
        );
        let mut app = Self {
            hotkey,
            poller,
            data: None,
            snapshot: None,
            graph: GraphView::new(),
            selected: None,
            view: View::default(),
            watched_screen: None,
            screen: None,
            filter: FleetFilter::default(),
            claude,
            claude_data: None,
            transcript: None,
            broadcast: ui::broadcast::BroadcastWindow::new(),
            actions,
            input: ui::terminal::InputPanelState::default(),
            whip,
            whip_status: WhipStatus::default(),
            whip_window,
            notifier,
            needs_input_detector: NeedsInputDetector::new(),
            alerts: Vec::new(),
            search: ui::search::SearchPalette::default(),
            help_open: false,
        };
        app.restore_state(cc.storage);
        app
    }

    /// Takes the whip's newest status; true when the escalated set changed (nodes to redraw).
    fn poll_whip(&mut self) -> bool {
        let Some(status) = self.whip.latest() else {
            return false;
        };
        let changed = status.escalated != self.whip_status.escalated;
        self.whip_status = status;
        changed
    }

    /// Hands the whip the current data, with the terminals shown as NeedsInput.
    fn feed_whip(&self) {
        let (Some(data), Some(snapshot)) = (&self.data, &self.snapshot) else {
            return;
        };
        let needs_input = snapshot
            .nodes
            .iter()
            .filter(|n| n.state == NodeState::NeedsInput)
            .filter_map(|n| n.terminal_handle.clone())
            .collect();
        self.whip.send_input(WhipInput {
            data: data.clone(),
            needs_input,
        });
    }

    /// The selected node and its info when it is a subagent in the latest Claude scan.
    fn selected_subagent(&self) -> Option<(&Node, &SubagentInfo)> {
        let id = self.selected.as_ref()?;
        let node = self.snapshot.as_ref()?.node(id)?;
        if node.kind != NodeKind::Subagent {
            return None;
        }
        let (session_id, agent_id) = id.subagent_parts()?;
        let info = self.claude_data.as_ref()?.subagent(session_id, agent_id)?;
        Some((node, info))
    }

    /// Points the worker's transcript tail at the selected subagent and takes new tails.
    fn sync_transcript(&mut self) {
        let path = self
            .selected_subagent()
            .map(|(_, info)| info.transcript_path.clone());
        if self.transcript.as_ref().map(|t| &t.path) != path.as_ref() {
            self.transcript = None;
            self.claude.watch_transcript(path.clone());
        }
        if let Some(tail) = self.claude.latest_transcript()
            && Some(&tail.path) == path.as_ref()
        {
            self.transcript = Some(tail);
        }
    }

    /// Points the poller's screen watch at the selected node's terminal, if it changed.
    fn sync_screen_watch(&mut self) {
        let handle = self
            .selected
            .as_ref()
            .and_then(|id| self.snapshot.as_ref()?.node(id))
            .and_then(ui::terminal::watch_handle)
            .map(str::to_owned);
        if handle != self.watched_screen {
            self.poller.send(PollerCommand::WatchScreen(handle.clone()));
            self.watched_screen = handle;
            self.screen = None;
        }
    }

    /// Right-hand panel for the selected node: its screen, or its transcript for a subagent.
    fn terminal_panel(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if let Some((node, info)) = self.selected_subagent() {
            let tail = self.transcript.as_ref().map_or(&[][..], |t| &t.lines[..]);
            let default_width = ui.ctx().content_rect().width() * 0.45;
            egui::Panel::right("terminal")
                .resizable(true)
                .default_size(default_width)
                .min_size(240.0)
                .show(ui, |ui| {
                    ui::subagent::subagent_panel(ui, node, info, tail, now_ms)
                });
            return;
        }
        let Some(node) = self
            .selected
            .as_ref()
            .and_then(|id| self.snapshot.as_ref()?.node(id))
        else {
            return;
        };
        let default_width = ui.ctx().content_rect().width() * 0.45;
        let response = egui::Panel::right("terminal")
            .resizable(true)
            .default_size(default_width)
            .min_size(240.0)
            .show(ui, |ui| {
                let terminal = node
                    .terminal_handle
                    .as_deref()
                    .and_then(|h| self.data.as_ref()?.terminals.iter().find(|t| t.handle == h));
                let input = ui::terminal::PanelInput {
                    state: &mut self.input,
                    terminal,
                    usage: self.claude_data.as_ref().and_then(|c| c.node_usage(node)),
                };
                ui::terminal::terminal_panel(ui, node, self.screen.as_ref(), input, now_ms)
            })
            .inner;
        for request in &response.requests {
            let id = self.actions.submit(request.action.clone());
            self.input.submitted(id, request);
        }
        if response.close {
            self.selected = None;
            self.graph.set_selected(None);
        }
    }

    /// Selects `id` in the app and the graph, and pans/zooms onto it unless it is filtered out.
    fn select_and_focus(&mut self, id: NodeId) {
        self.graph.set_selected(Some(&id));
        if self.graph.is_visible(&id) {
            self.graph.focus_nodes(std::slice::from_ref(&id));
        }
        self.selected = Some(id);
    }

    fn clear_filters(&mut self) {
        self.filter = FleetFilter::default();
        if let Some(snapshot) = &self.snapshot {
            self.graph.set_hidden(&self.filter.hidden(snapshot));
        }
    }

    /// Opens the search palette on its shortcut, draws it and applies the picked node.
    fn search_palette(&mut self, ctx: &egui::Context) {
        if ui::search::take_open_shortcut(ctx) {
            self.search.open();
        }
        let outcome = self.search.show(
            ctx,
            self.snapshot.as_ref(),
            self.data.as_ref(),
            self.graph.hidden(),
        );
        match outcome {
            Some(ui::search::SearchOutcome::Select(id)) => self.select_and_focus(id),
            Some(ui::search::SearchOutcome::ClearFiltersAndSelect(id)) => {
                self.clear_filters();
                self.select_and_focus(id);
            }
            None => {}
        }
    }

    /// Runs this frame's keyboard commands (j/k navigation etc.) unless the palette is open.
    fn keyboard(&mut self, ctx: &egui::Context) {
        if self.search.is_open() {
            return;
        }
        for command in ui::keys::take_commands(ctx) {
            self.run_key_command(ctx, command);
        }
        ui::keys::help_window(ctx, &mut self.help_open);
    }

    fn run_key_command(&mut self, ctx: &egui::Context, command: ui::keys::KeyCommand) {
        use ui::keys::KeyCommand;
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let hidden = self.graph.hidden();
        let current = self.selected.as_ref();
        let target = match command {
            KeyCommand::NextNode | KeyCommand::PrevNode => {
                let delta = if command == KeyCommand::NextNode {
                    1
                } else {
                    -1
                };
                ui::keys::step_node(&ui::keys::nav_order(snapshot, hidden), current, delta)
            }
            KeyCommand::NextRun | KeyCommand::PrevRun => {
                let delta = if command == KeyCommand::NextRun {
                    1
                } else {
                    -1
                };
                ui::keys::step_run(&ui::keys::nav_groups(snapshot, hidden), current, delta)
            }
            KeyCommand::OpenPanel => {
                if let Some(id) = current.cloned() {
                    self.select_and_focus(id);
                    return;
                }
                ui::keys::nav_order(snapshot, hidden).into_iter().next()
            }
            KeyCommand::FocusInput => {
                if let Some(handle) = current
                    .and_then(|id| snapshot.node(id))
                    .and_then(ui::terminal::watch_handle)
                {
                    let id = ui::terminal::input_id(handle);
                    ctx.memory_mut(|m| m.request_focus(id));
                }
                None
            }
            KeyCommand::Escape => {
                if let Some(focused) = ctx.memory(|m| m.focused()) {
                    ctx.memory_mut(|m| m.surrender_focus(focused));
                } else if self.help_open {
                    self.help_open = false;
                } else {
                    self.selected = None;
                    self.graph.set_selected(None);
                }
                None
            }
            KeyCommand::OpenInOrca => {
                if let Some(handle) = current
                    .and_then(|id| snapshot.node(id))
                    .and_then(ui::terminal::watch_handle)
                {
                    let request = ui::terminal::ActionRequest::open(handle);
                    let id = self.actions.submit(request.action.clone());
                    self.input.submitted(id, &request);
                }
                None
            }
            KeyCommand::Fit => {
                self.view = View::Graph;
                self.graph.fit_to_screen();
                None
            }
            KeyCommand::ToggleHelp => {
                self.help_open = !self.help_open;
                None
            }
        };
        if let Some(id) = target {
            self.graph.set_selected(Some(&id));
            self.graph.center_on(&id);
            self.selected = Some(id);
        }
    }

    /// A bar offering to clear the filters while the selected node is filtered out.
    fn hidden_selection_notice(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.view != View::Graph || !self.graph.hidden().contains(&id) {
            return;
        }
        let clear = egui::Panel::top("hidden_selection")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "The selected node is hidden by filter.",
                    );
                    ui.button("Clear filters").clicked()
                })
                .inner
            })
            .inner;
        if clear {
            self.clear_filters();
            self.select_and_focus(id);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        ui.horizontal(|ui| {
            ui.heading(APP_NAME);
            ui.separator();
            ui.selectable_value(&mut self.view, View::Graph, "Graph");
            ui.selectable_value(&mut self.view, View::Table, "Table");
            if self.view == View::Graph && ui.button("Fit").clicked() {
                self.graph.fit_to_screen();
            }
            if self.view == View::Graph
                && ui
                    .button("Reset layout")
                    .on_hover_text("Forget saved node positions and lay the graph out again")
                    .clicked()
            {
                self.graph.reset_layout();
            }
            ui.separator();
            if let Some(data) = &self.data {
                ui.label(format!("{} terminals", data.terminals.len()));
                ui.separator();
                ui.label(format!(
                    "updated {} ago",
                    ui::table::format_age(now_ms - data.polled_at_ms)
                ));
            }
            if ui.button("Refresh").clicked() {
                self.poller.send(PollerCommand::RefreshNow);
            }
            if ui.button("Broadcast…").clicked() {
                self.broadcast.open = true;
            }
            self.whip_window.button(ui);
            self.alerts_menu(ui);
            if let Some(data) = &self.data
                && let Some(first) = data.errors.first()
            {
                ui.separator();
                let text = if data.errors.len() > 1 {
                    format!("{first} (+{} more)", data.errors.len() - 1)
                } else {
                    first.clone()
                };
                ui.add(
                    egui::Label::new(egui::RichText::new(text).color(ui.visuals().error_fg_color))
                        .truncate(),
                )
                .on_hover_text(data.errors.join("\n"));
            }
        });
    }
}

/// Notifications: nodes entering NeedsInput, and the in-app list of posted notifications.
///
/// Clicking an `osascript` notification cannot reach Myrstack, so the alerts menu is how a
/// notification selects its node.
impl MyrstackApp {
    /// Asks for a notification for each node that just entered NeedsInput, unless the whip's
    /// notifications are off.
    fn notify_needs_input(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let requests = self.needs_input_detector.update(snapshot);
        if !self.whip_window.whip().notify {
            return;
        }
        for request in requests {
            self.notifier.notify(request);
        }
    }

    fn take_alerts(&mut self) {
        self.alerts.extend(self.notifier.delivered());
        let excess = self.alerts.len().saturating_sub(MAX_ALERTS);
        self.alerts.drain(..excess);
    }

    /// `Alerts (n)` menu in the top bar; picking an entry selects its node and forgets it.
    fn alerts_menu(&mut self, ui: &mut egui::Ui) {
        if self.alerts.is_empty() {
            return;
        }
        let mut picked = None;
        ui.menu_button(format!("Alerts ({})", self.alerts.len()), |ui| {
            for (i, alert) in self.alerts.iter().enumerate().rev() {
                let title = alert
                    .title
                    .strip_prefix("Myrstack: ")
                    .unwrap_or(&alert.title);
                if ui.button(title).clicked() {
                    picked = Some(i);
                }
            }
            ui.separator();
            if ui.button("Clear").clicked() {
                picked = Some(usize::MAX);
            }
        });
        match picked {
            Some(usize::MAX) => self.alerts.clear(),
            Some(i) => {
                let alert = self.alerts.remove(i);
                let target = self.snapshot.as_ref().and_then(|snapshot| {
                    alert
                        .targets
                        .iter()
                        .find_map(|t| notify::resolve_target(snapshot, t))
                });
                if let Some(id) = target {
                    self.select_and_focus(id);
                }
            }
            None => {}
        }
    }
}

/// Saving and restoring UI state between launches.
impl MyrstackApp {
    fn restore_state(&mut self, storage: Option<&dyn eframe::Storage>) {
        let Some(state) = storage.and_then(PersistedState::load) else {
            return;
        };
        log::debug!(
            "persist: restoring {} node positions, view {:?}",
            state.layout.positions.len(),
            state.view
        );
        self.view = state.view;
        self.filter = state.filter;
        self.selected = state.layout.selected.clone();
        self.graph.import_layout(state.layout);
    }

    fn persisted_state(&self) -> PersistedState {
        PersistedState {
            view: self.view,
            filter: self.filter.clone(),
            layout: self.graph.export_layout(),
            ..PersistedState::default()
        }
    }
}

impl eframe::App for MyrstackApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let state = self.persisted_state();
        log::debug!(
            "persist: saved {} node positions",
            state.layout.positions.len()
        );
        state.save(storage);
    }

    fn auto_save_interval(&self) -> Duration {
        AUTOSAVE
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(hotkey) = &mut self.hotkey {
            hotkey.poll(ctx);
        }
        let mut rebuild = false;
        if let Some(data) = self.poller.latest() {
            log::debug!(
                "snapshot: {} terminals, {} runs, {} workers, {} errors",
                data.terminals.len(),
                data.runs.len(),
                data.workers.len(),
                data.errors.len()
            );
            self.claude.send_terminals(data.terminals.clone());
            let mut data = data;
            if let Some(screen) = &self.screen
                && let Some(old) = &self.data
                && let Some(verdict) = old.needs_input.get(&screen.handle)
            {
                data.needs_input
                    .insert(screen.handle.clone(), verdict.clone());
            }
            self.data = Some(data);
            rebuild = true;
        }
        if let Some(claude) = self.claude.latest() {
            log::debug!(
                "claude: {} mapped terminals, {} sessions with subagents, {} subagents",
                claude.session_by_handle.len(),
                claude.subagents.values().filter(|a| !a.is_empty()).count(),
                claude.subagent_count()
            );
            self.claude_data = Some(claude);
            rebuild = true;
        }
        if let Some(update) = self.poller.latest_screen()
            && Some(&update.handle) == self.watched_screen.as_ref()
        {
            // The selected terminal's screen is read every 500 ms, fresher than the poller's
            // background sampling, so its prompt verdict overrides the sampled one.
            if let Some(data) = &mut self.data {
                let agent = data
                    .terminal(&update.handle)
                    .and_then(|t| t.agent_identity.as_ref());
                let verdict = detect_prompt(&update.lines, agent);
                if data.needs_input.get(&update.handle) != Some(&verdict) {
                    data.needs_input.insert(update.handle.clone(), verdict);
                    rebuild = true;
                }
            }
            self.screen = Some(update);
        }
        rebuild |= self.poll_whip();
        if rebuild && let Some(data) = &self.data {
            let snapshot = FleetSnapshot::build_with_stalled(
                data,
                self.claude_data.as_ref(),
                &self.whip_status.escalated,
                now_ms(),
            );
            self.graph.set_hidden(&self.filter.hidden(&snapshot));
            self.graph.sync(&snapshot);
            if let Some(claude) = &self.claude_data {
                self.graph.set_usage(claude.node_usages(&snapshot.nodes));
            }
            if let Some(id) = &self.selected
                && snapshot.node(id).is_none()
            {
                self.selected = None;
            }
            self.snapshot = Some(snapshot);
            self.feed_whip();
            self.notify_needs_input();
        }
        self.take_alerts();
        for result in self.actions.poll() {
            if self.input.on_result(&result, now_ms()) {
                self.poller.send(PollerCommand::RefreshScreen);
            }
        }
        self.sync_screen_watch();
        self.sync_transcript();
        ctx.request_repaint_after(TICK);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now_ms = now_ms();
        self.search_palette(ui.ctx());
        self.keyboard(ui.ctx());
        egui::Panel::top("top_bar").show(ui, |ui| self.top_bar(ui, now_ms));
        self.hidden_selection_notice(ui);
        if self.view == View::Graph
            && let Some(snapshot) = &self.snapshot
        {
            let filter = &mut self.filter;
            let response = egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| ui::sidebar::sidebar(ui, snapshot, filter))
                .inner;
            if response.filter_changed {
                self.graph.set_hidden(&self.filter.hidden(snapshot));
            }
            if let Some(run_id) = &response.focus_run {
                self.graph.focus_run(run_id);
            }
            if let Some(id) = response.select {
                self.graph.set_selected(Some(&id));
                self.selected = Some(id);
            }
        }
        if self.view == View::Graph {
            egui::Panel::bottom("legend").show(ui, ui::graph::legend);
        }
        self.terminal_panel(ui, now_ms);
        egui::CentralPanel::default().show(ui, |ui| match &self.data {
            None => {
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connecting to Orca…");
                    });
                });
            }
            Some(data) => match self.view {
                View::Graph => {
                    let response = self.graph.show(ui);
                    // A node selected while filtered out is not in the graph; keep it.
                    let hidden_selected = self
                        .selected
                        .as_ref()
                        .is_some_and(|id| self.graph.hidden().contains(id));
                    if response.selected.is_some() || !hidden_selected {
                        self.selected = response.selected;
                    }
                }
                View::Table => ui::table::terminal_table(ui, data, now_ms),
            },
        });
        self.broadcast.show(ui.ctx(), self.snapshot.as_ref(), || {
            Box::new(OrcaClient::new())
        });
        if let Some(settings) = self.whip_window.show(ui.ctx(), &self.whip_status, now_ms) {
            self.whip.update_settings(settings);
        }
        self.sync_screen_watch();
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
