//! The eframe `App`: owns UI state and draws the panels each frame.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;

mod chat;
mod control;
mod dictation;
pub mod envoy;
mod health;
mod lifecycle;
mod notifications;
mod palette;
mod panes;
mod playbook;
mod review;
mod settings;
mod spawn;
mod talk;
mod traffic;
mod whip_now;
use crate::actions::{ActionRunner, OrcaExecutor};
use crate::claude::{ClaudeData, ClaudeWorker, SubagentInfo};
use crate::detect::detect_prompt;
use crate::fleet::FleetData;
use crate::hotkey::Hotkey;
use crate::model::{FleetSnapshot, Node, NodeId, NodeKind, NodeState};
use crate::notify::{NeedsInputDetector, Notifier};
use crate::orca::OrcaClient;
use crate::persist::PersistedState;
use crate::poller::{Poller, PollerCommand, PollerConfig, ScreenUpdate};
use crate::quota::{QuotaPoller, Quotas};
use crate::ui;
use crate::ui::graph::GraphView;
use crate::ui::sidebar::FleetFilter;
use crate::whip::{self, WhipInput, WhipRunner, WhipStatus};

/// How often the UI redraws without new data, so relative times keep ticking.
const TICK: Duration = Duration::from_secs(1);
/// Delay of [`repaint_once`]: any non-zero delay makes egui draw a single frame.
const REPAINT_SOON: Duration = Duration::from_millis(1);
/// How often UI state is saved while running (it is also saved on exit).
const AUTOSAVE: Duration = Duration::from_secs(5);

/// Which view fills the central panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum View {
    #[default]
    Graph,
    Table,
    /// The fleet as an ant farm; see [`ui::ant_farm`].
    AntFarm,
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
    ant_farm: ui::ant_farm::AntFarmView,
    /// Node selected in the graph, shared by all views and side panels.
    selected: Option<NodeId>,
    /// The nodes selected so far, for back/forward.
    history: ui::history::SelectionHistory,
    view: View,
    /// The dock's pane arrangement: sidebar, graph or table, details and conversation.
    panes: ui::dock::PaneLayout,
    /// A pane to bring to the front of its tab group next frame (the dock is out of `panes`
    /// while it draws).
    bring_forward: Option<ui::dock::Pane>,
    /// ⌘J hid the Queen's chat, which otherwise fills the conversation pane when nothing
    /// is selected.
    chat_hidden: bool,
    /// Left edge of the conversation pane's content last frame, `None` while it is not drawn;
    /// the top bar puts the Queen's quick actions above it.
    conversation_left: Option<f32>,
    /// Width of the top bar's Queen quick actions last frame.
    quick_actions_width: f32,
    /// Terminal whose screen the poller is reading (the selected node's, if live).
    watched_screen: Option<String>,
    /// Newest screen of `watched_screen`.
    screen: Option<ScreenUpdate>,
    /// Which nodes the graph shows; edited in the sidebar.
    filter: FleetFilter,
    /// Maps terminals to Claude sessions and scans subagents off the UI thread.
    claude: ClaudeWorker,
    claude_data: Option<ClaudeData>,
    /// The selected agent's or subagent's conversation, read from its session transcript.
    transcript: Option<ui::chat::TranscriptView>,
    /// The Queen's details: fleet summary and the broadcast composer.
    queen: ui::queen::QueenPanel,
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
    /// Whip action history for the sidebar.
    whip_log: ui::whip_log::WhipLog,
    /// Sidebar rows and counts derived from `snapshot`.
    sidebar_cache: ui::sidebar::SidebarCache,
    /// The Settings window (Queen, Whip, Voice, General); owns `settings.json`.
    whip_window: ui::settings::SettingsPane,
    /// Pops up and sounds notifications; shared with the whip's executors, which raise theirs
    /// through it.
    notifier: Arc<Notifier>,
    needs_input_detector: NeedsInputDetector,
    /// Questions agents asked Karl with `ask_karl`, until answered.
    asks: crate::ask::Asks,
    /// Sessions that ended themselves with `finish_session`, shown done while they fade.
    finished: crate::finish::FinishLog,
    /// Self-finished terminals waiting to be closed, once the agent got its reply.
    finish_closes: Vec<lifecycle::PendingClose>,
    /// Asks or finished sessions changed: rebuild the snapshot.
    overlays_changed: bool,
    /// Messages between agents, for the graph and the ant farm to draw.
    traffic: crate::traffic::Traffic,
    /// Every notification's dispatcher, the saved inbox, toasts and node badges.
    notifications: notifications::AppNotifications,
    /// Debug-logged frames per second and repaint causes.
    frame_stats: ui::frame_stats::FrameStats,
    /// Polls `orca account list` for plan quotas every few minutes.
    quota: QuotaPoller,
    quotas: Quotas,
    /// Control API server, its published status view and commands in flight.
    control: control::AppControl,
    /// "Close session" confirmation dialog and the log of closes.
    close: ui::close::CloseDialog,
    /// Worktree count in the top bar, the cleanup window and its broom.
    worktrees: ui::worktrees::WorktreePanel,
    /// The ant farm drew the broom this frame, so it skips the window's bottom edge.
    broom_in_farm: bool,
    /// Who fetched which playbook version, re-read off the UI thread.
    playbook_watch: playbook::PlaybookWatch,
    /// The Queen's chat, drawn in the conversation column.
    chat: chat::AppChat,
    /// Manual whips in flight, their confirmation popover and result toast.
    whip_now: whip_now::WhipNowState,
    /// "New ant…" dialog, spawns in flight and the new node to select.
    spawn: spawn::AppSpawn,
    /// ⌘E push-to-talk dictation.
    dictation: dictation::AppDictation,
    /// Hold-to-talk to the Queen (global hotkey) and her spoken replies.
    talk: talk::AppTalk,
    /// PRs agents marked ready for review, their GitHub state and the poller reading it.
    review: review::AppReview,
    pub envoy_core: envoy::AppEnvoy,
    envoy_panel: ui::envoy::DraftsPanel,
    /// Health checks for silent misconfigurations, and the badge showing them.
    health: health::AppHealth,
}

impl MyrstackApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        ui::theme::install(&cc.egui_ctx);
        let hotkey = Hotkey::register(&cc.egui_ctx)
            .inspect_err(|err| log::warn!("global hotkey unavailable: {err:#}"))
            .ok();
        let ctx = &cc.egui_ctx;
        let claude = ClaudeWorker::spawn(Some(repaint_once(ctx)));
        let actions = ActionRunner::spawn(OrcaExecutor::default(), Some(repaint_once(ctx)));
        let poller = Poller::spawn(
            OrcaClient::new(),
            PollerConfig::default(),
            Some(repaint_once(ctx)),
        );
        let whip_window = ui::settings::SettingsPane::load(whip::Settings::path());
        let notifier = Arc::new(Notifier::spawn(Some(repaint_once(ctx))));
        let whip_notifier = notifier.clone();
        let whip = WhipRunner::spawn_with_store(
            whip_window.whip().clone(),
            Box::new(move || {
                Box::new(whip::OrcaExecutor::default().with_notifier(whip_notifier.clone()))
            }),
            Some(repaint_once(ctx)),
            whip::Settings::path().map(|p| p.with_file_name(whip::forward::FORWARDED_FILE)),
        );
        let quota = QuotaPoller::spawn(
            OrcaClient::new(),
            crate::quota::POLL_INTERVAL,
            Some(repaint_once(ctx)),
        );
        let chat = chat::AppChat::start(ctx, whip_window.queen());
        let talk = talk::AppTalk::new(ctx, whip_window.voice());
        let mut app = Self {
            control: control::AppControl::start(ctx),
            quota,
            quotas: Quotas::default(),
            hotkey,
            poller,
            data: None,
            snapshot: None,
            graph: GraphView::new(),
            ant_farm: ui::ant_farm::AntFarmView::new(),
            selected: None,
            history: ui::history::SelectionHistory::default(),
            view: View::default(),
            panes: ui::dock::PaneLayout::default(),
            bring_forward: None,
            chat_hidden: false,
            conversation_left: None,
            quick_actions_width: 0.0,
            watched_screen: None,
            screen: None,
            filter: FleetFilter::default(),
            claude,
            claude_data: None,
            transcript: None,
            queen: ui::queen::QueenPanel::default(),
            actions,
            input: ui::terminal::InputPanelState::default(),
            whip,
            whip_status: WhipStatus::default(),
            whip_log: ui::whip_log::WhipLog::default(),
            sidebar_cache: ui::sidebar::SidebarCache::default(),
            whip_window,
            notifier,
            needs_input_detector: NeedsInputDetector::new(),
            asks: crate::ask::Asks::new(),
            finished: crate::finish::FinishLog::load(
                crate::finish::FinishLog::default_path(),
                now_ms() - crate::model::RELEASED_FADE_MS,
            ),
            finish_closes: Vec::new(),
            overlays_changed: true,
            traffic: Default::default(),
            notifications: notifications::AppNotifications::load(),
            search: ui::search::SearchPalette::default(),
            help_open: false,
            frame_stats: ui::frame_stats::FrameStats::default(),
            close: ui::close::CloseDialog::default(),
            worktrees: ui::worktrees::WorktreePanel::new(ctx),
            broom_in_farm: false,
            playbook_watch: playbook::PlaybookWatch::spawn(repaint_once(ctx)),
            chat,
            whip_now: whip_now::WhipNowState::default(),
            spawn: spawn::AppSpawn::start(ctx),
            dictation: dictation::AppDictation::new(ctx),
            talk,
            review: review::AppReview::start(ctx),
            envoy_core: envoy::AppEnvoy::start(ctx),
            envoy_panel: ui::envoy::DraftsPanel::default(),
            health: health::AppHealth::start(ctx),
        };
        app.talk_startup();
        app.restore_state(cc.storage);
        app
    }

    /// Takes the newest plan quotas, if the poller has fetched again.
    fn poll_quota(&mut self) {
        if let Some(quotas) = self.quota.latest() {
            self.quotas = quotas;
        }
    }

    fn quota_chips(&self, ui: &mut egui::Ui, now_ms: i64) {
        if self.quotas.providers.is_empty() {
            return;
        }
        ui.separator();
        ui::quota::quota_chips(ui, &self.quotas, now_ms);
    }

    /// Takes the whip's newest status; true when the escalated set changed (nodes to redraw).
    fn poll_whip(&mut self) -> bool {
        let Some(mut status) = self.whip.latest() else {
            return false;
        };
        if !status.new_log.is_empty() {
            for message in status.new_log.iter().filter_map(crate::traffic::from_whip) {
                self.traffic.push(message);
            }
            let new = status.new_log.len();
            self.whip_log.extend(status.new_log.drain(..));
            log::debug!(
                "whip log: {new} new entries, {} in history",
                self.whip_log.entries().len()
            );
        }
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

    /// The session transcript of the selected subagent, or of the selected Claude agent.
    fn conversation_path(&self) -> Option<std::path::PathBuf> {
        if let Some((_, info)) = self.selected_subagent() {
            return Some(info.transcript_path.clone());
        }
        let handle = self.selected_node()?.terminal_handle.as_deref()?;
        let session = self.claude_data.as_ref()?.session_by_handle.get(handle)?;
        Some(session.session()?.transcript_path.clone())
    }

    /// Points the worker at the selected conversation's transcript and applies what it read.
    fn sync_transcript(&mut self) {
        let path = self.conversation_path();
        if self.transcript.as_ref().map(|t| &t.path) != path.as_ref() {
            self.transcript = path.clone().map(ui::chat::TranscriptView::new);
            self.claude.watch_transcript(path);
        }
        for chunk in self.claude.history_chunks() {
            if let Some(view) = &mut self.transcript {
                view.apply(&chunk);
            }
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

    /// The selected node, if it is in the snapshot.
    fn selected_node(&self) -> Option<&Node> {
        self.snapshot.as_ref()?.node(self.selected.as_ref()?)
    }

    fn queen_selected(&self) -> bool {
        self.selected.as_ref() == Some(&NodeId::queen())
    }

    /// Whether the conversation column has something to show: the Queen's chat (also when
    /// nothing is selected, unless hidden), the selected agent's screen or transcript, or the
    /// multi-selection.
    fn has_conversation(&self) -> bool {
        self.shows_chat()
            || self.selected_node().is_some()
            || (self.snapshot.is_some() && self.graph.selection().len() > 1)
    }

    /// Whether the details column has something to show: one selected node (or the Queen).
    fn has_details(&self) -> bool {
        self.graph.selection().len() <= 1
            && (self.queen_selected() || self.selected_node().is_some())
    }

    pub(super) fn conversation_contents(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if self.graph.selection().len() > 1 {
            self.selection_contents(ui);
        } else if self.shows_chat() {
            if let Some(id) = self.chat.ui(ui, self.snapshot.as_ref()) {
                self.select_and_focus(id);
            }
        } else if let Some((node, info)) = self.selected_subagent() {
            let (node, info) = (node.clone(), info.clone());
            let snapshot = self.snapshot.as_ref();
            let is_node = |id: &NodeId| snapshot.is_some_and(|s| s.node(id).is_some());
            let clicked = ui::subagent::subagent_transcript(
                ui,
                &node,
                &info,
                self.transcript.as_mut(),
                &is_node,
            );
            if let Some(id) = clicked {
                self.select_and_focus(id);
            }
        } else {
            self.terminal_contents(ui, now_ms);
        }
    }

    /// The selected agent's screen and input box.
    fn terminal_contents(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        let Some(node) = self
            .selected
            .as_ref()
            .and_then(|id| self.snapshot.as_ref()?.node(id))
        else {
            return;
        };
        let terminal = node
            .terminal_handle
            .as_deref()
            .and_then(|h| self.data.as_ref()?.terminals.iter().find(|t| t.handle == h));
        let review_rows = self.review_rows(now_ms);
        let snapshot = self.snapshot.as_ref();
        let is_node = |id: &NodeId| snapshot.is_some_and(|s| s.node(id).is_some());
        let input = ui::terminal::PanelInput {
            review_rows: &review_rows,
            state: &mut self.input,
            terminal,
            conversation: self.transcript.as_mut(),
            is_node: &is_node,
        };
        let response = ui::terminal::terminal_panel(ui, node, self.screen.as_ref(), input, now_ms);
        let id = node.id.clone();
        self.apply_terminal_response(id, response);
    }

    fn details_contents(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if self.selected_envoy() {
            self.envoy_drafts_button(ui);
        }
        if self.queen_selected() {
            self.queen_details(ui, now_ms);
            return;
        }
        if let Some((node, info)) = self.selected_subagent() {
            ui::subagent::subagent_details(ui, node, info, now_ms);
            return;
        }
        let Some(node) = self.selected_node() else {
            return;
        };
        if self
            .snapshot
            .as_ref()
            .is_some_and(|s| s.coordinator_gone(node))
        {
            ui::graph::coordinator_gone_note(ui);
        }
        let usage = self.claude_data.as_ref().and_then(|c| c.node_usage(node));
        let response = ui::terminal::details_panel(ui, node, self.screen.as_ref(), usage, now_ms);
        let id = node.id.clone();
        self.apply_terminal_response(id, response);
    }

    /// Runs what the user did in the selected node's details or conversation.
    fn apply_terminal_response(
        &mut self,
        id: NodeId,
        response: ui::terminal::TerminalPanelResponse,
    ) {
        if let Some(action) = response.review {
            self.apply_review_action(action);
        }
        for request in &response.requests {
            if let (crate::actions::Action::Send { handle, .. }, ui::terminal::InputKind::Text) =
                (&request.action, request.kind)
            {
                self.traffic.push(crate::traffic::AgentMessage::new(
                    "karl",
                    handle.clone(),
                    crate::traffic::MessageKind::Instruction,
                ));
            }
            let action = self.actions.submit(request.action.clone());
            self.input.submitted(action, request);
        }
        if response.close_session {
            self.request_close(vec![id.clone()]);
        }
        if response.whip {
            self.request_whip(vec![id]);
        }
        if response.close {
            self.selected = None;
            self.graph.set_selected(None);
        }
        if let Some(id) = response.select {
            self.select_and_focus(id);
        }
    }

    /// The Queen's details: the review queue, her header and fleet summary, and broadcast.
    fn queen_details(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        self.envoy_drafts_button(ui);
        let (review_rows, review_note, review_state) = self.review_view(now_ms);
        let playbook = self.playbook_status();
        let review_action = ui::review::review_section(
            ui,
            &review_rows,
            review_note.as_deref(),
            now_ms,
            review_state,
        );
        let inputs = ui::queen::QueenInputs {
            snapshot: self.snapshot.as_ref(),
            whip: &self.whip_status,
            quotas: &self.quotas,
            now_ms,
            playbook: playbook.as_ref(),
        };
        let response =
            ui::queen::queen_details(ui, &mut self.queen, inputs, || Box::new(OrcaClient::new()));
        if self.queen.broadcast.take_sent() {
            self.ant_farm.pulse(ui::ant_farm::Pulse::Broadcast);
        }
        if response.whip_stalled {
            self.request_whip_stalled(None);
        }
        let review_select = review_action.and_then(|a| self.apply_review_action(a));
        if let Some(id) = response.select.or(review_select) {
            self.select_and_focus(id);
        } else if response.close {
            self.selected = None;
            self.graph.set_selected(None);
        }
    }

    /// Selects the queen, opening her chat and details, and pans onto her keeping the zoom.
    fn select_queen(&mut self) {
        let id = NodeId::queen();
        self.graph.set_selected(Some(&id));
        self.graph.center_on(&id);
        self.selected = Some(id);
    }

    /// Selects `id` in the app and the graph, and pans/zooms onto it unless it is filtered out.
    fn select_and_focus(&mut self, id: NodeId) {
        self.graph.set_selected(Some(&id));
        if self.graph.is_visible(&id) {
            self.graph.focus_nodes(std::slice::from_ref(&id));
        }
        self.selected = Some(id);
    }

    /// Switches to the graph (unless the ant farm is shown) and selects and focuses `id`
    /// there, as `focus_agent` does.
    fn focus_in_graph(&mut self, id: NodeId) {
        if self.view == View::Table {
            self.view = View::Graph;
        }
        self.select_and_focus(id);
    }

    fn clear_filters(&mut self) {
        self.filter = FleetFilter::default();
        if let Some(snapshot) = &self.snapshot {
            self.graph.set_hidden(&self.filter.hidden(snapshot));
        }
    }

    /// Opens the command palette on its shortcut, draws it and applies the picked node or
    /// action.
    fn search_palette(&mut self, ctx: &egui::Context) {
        if ui::search::take_open_shortcut(ctx) {
            self.search.open();
        }
        let cx = self.palette_context();
        let outcome = self.search.show(
            ctx,
            self.snapshot.as_ref(),
            self.data.as_ref(),
            self.graph.hidden(),
            &cx,
        );
        match outcome {
            Some(ui::search::SearchOutcome::Select(id)) => self.focus_in_graph(id),
            Some(ui::search::SearchOutcome::ClearFiltersAndSelect(id)) => {
                self.clear_filters();
                self.focus_in_graph(id);
            }
            Some(ui::search::SearchOutcome::Run(action)) => self.run_palette_action(ctx, action),
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
        if command == KeyCommand::ToggleChat {
            self.toggle_chat();
            return;
        }
        if command == KeyCommand::OpenSettings {
            self.open_settings();
            return;
        }
        if command == KeyCommand::HistoryBack {
            self.history_step(ui::history::HistoryStep::Back);
            return;
        }
        if command == KeyCommand::HistoryForward {
            self.history_step(ui::history::HistoryStep::Forward);
            return;
        }
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
            KeyCommand::NextRepo | KeyCommand::PrevRepo => {
                let delta = if command == KeyCommand::NextRepo {
                    1
                } else {
                    -1
                };
                let groups = ui::keys::repo_nav(snapshot, hidden);
                if let Some((repo, id)) = ui::keys::step_repo(&groups, current, delta) {
                    self.graph.set_selected(Some(&id));
                    self.graph.focus_repo(&repo);
                    self.selected = Some(id);
                }
                None
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
                } else if let Some(keep) =
                    ui::selection::after_escape(current, &self.graph.selection())
                {
                    self.graph.set_selected(keep.as_ref());
                    self.selected = keep;
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
            KeyCommand::ToggleChat
            | KeyCommand::OpenSettings
            | KeyCommand::HistoryBack
            | KeyCommand::HistoryForward => None,
            KeyCommand::CloseSession => {
                let ids = ui::selection::close_targets(current, &self.graph.selection());
                if !ids.is_empty() {
                    self.request_close(ids);
                }
                None
            }
            KeyCommand::Whip => {
                let ids = ui::selection::whip_targets(current, &self.graph.selection());
                if !ids.is_empty() {
                    self.request_whip(ids);
                }
                None
            }
        };
        if let Some(id) = target {
            self.graph.set_selected(Some(&id));
            self.graph.center_on(&id);
            self.selected = Some(id);
        }
    }

    /// How fresh the fleet data is; `None` before the first poll.
    fn freshness(&self, now_ms: i64) -> Option<crate::stale::Freshness> {
        let data = self.data.as_ref()?;
        Some(crate::stale::freshness(
            data.fresh_at_ms,
            data.failed_polls,
            now_ms,
            PollerConfig::default().terminals,
        ))
    }

    /// While Orca keeps failing: a banner with the error, how old the data is and "Retry now".
    fn stale_banner(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        let Some(crate::stale::Freshness::Failing { age_ms, failures }) = self.freshness(now_ms)
        else {
            return;
        };
        let error = self
            .data
            .as_ref()
            .and_then(|d| d.errors.first().cloned())
            .unwrap_or_default();
        let all_errors = self
            .data
            .as_ref()
            .map(|d| d.errors.join("\n"))
            .unwrap_or_default();
        let shown = age_ms.map_or_else(
            || "no data yet".to_owned(),
            |ms| format!("showing data from {} ago", ui::table::format_age(ms)),
        );
        let retry = egui::Panel::top("stale_banner")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let retry = ui.button("Retry now").clicked();
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("Orca isn't answering ({failures} failed polls): {shown}."),
                    );
                    ui.add(egui::Label::new(egui::RichText::new(error).weak()).truncate())
                        .on_hover_text(all_errors);
                    retry
                })
                .inner
            })
            .inner;
        if retry {
            self.poller.send(PollerCommand::RefreshNow);
        }
    }

    /// A bar offering to clear the filters while the selected node is filtered out.
    fn hidden_selection_notice(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.view == View::Table || !self.graph.hidden().contains(&id) {
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

    /// The top bar: the app's controls, and the Queen and Status report buttons and plan
    /// quotas at the right, above the conversation column.
    fn top_bar(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        let bar = ui.max_rect();
        let row = ui.spacing().interact_size.y;
        let x = ui::columns::quick_actions_x(
            bar.left(),
            bar.right(),
            self.quick_actions_width,
            ui::dock::quick_actions_anchor(self.conversation_left, bar),
        );
        let gap = ui.spacing().item_spacing.x;
        let left = egui::Rect::from_min_size(bar.min, egui::vec2(x - gap - bar.left(), row));
        let right = egui::Rect::from_min_max(
            egui::pos2(x, bar.top()),
            egui::pos2(bar.right(), bar.top() + row),
        );
        // Wraps onto a second line rather than clipping on narrow windows.
        let wrapped = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(left).layout(wrapped),
            |ui| {
                self.top_bar_controls(ui, now_ms);
            },
        );
        // One row, never wrapped: its natural width places it next frame.
        let row_layout = egui::Layout::left_to_right(egui::Align::Center);
        let quick = ui.scope_builder(
            egui::UiBuilder::new().max_rect(right).layout(row_layout),
            |ui| {
                self.queen_quick_actions(ui);
                self.quota_chips(ui, now_ms);
            },
        );
        self.quick_actions_width = quick.response.rect.width();
    }

    fn top_bar_controls(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        ui::widgets::app_title(ui);
        ui.separator();
        let can_back = self.history.can_back(self.selected.as_ref());
        if let Some(step) = ui::history::history_buttons(ui, can_back, self.history.can_forward()) {
            self.history_step(step);
        }
        ui.selectable_value(&mut self.view, View::Graph, "Graph");
        ui.selectable_value(&mut self.view, View::Table, "Table");
        ui.selectable_value(&mut self.view, View::AntFarm, "Ant farm")
            .on_hover_text("The fleet as an ant farm: an ant's place shows its state");
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
            ui.weak(format!("{} terminals", data.terminals.len()));
            ui.separator();
            self.worktrees.top_bar(ui, &data.worktrees);
        }
        if let Some(freshness) = self.freshness(now_ms)
            && let Some(label) = crate::stale::age_label(freshness)
        {
            ui.separator();
            let color = if freshness.is_failing() {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().warn_fg_color
            };
            ui.colored_label(color, label)
                .on_hover_text("Orca hasn't answered lately: the fleet shown may be out of date");
        }
        if ui.button("Refresh").clicked() {
            self.poller.send(PollerCommand::RefreshNow);
        }
        if ui
            .button("Broadcast…")
            .on_hover_text("Select the Queen and open the broadcast to coordinators")
            .clicked()
        {
            self.select_queen();
            self.queen.open_broadcast = true;
        }
        self.whip_window.gear_button(ui);
        self.whip_window.button(ui);
        self.notifications_bell(ui);
        self.review_badge(ui);
        self.health_badge(ui, now_ms);
        self.close.log_menu(ui, now_ms);
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
    }
}

/// Selection history: back and forward over the nodes selected so far.
///
/// Every change of the selected node is recorded once per frame, whoever made it (a click,
/// the keyboard, the palette, a notification, the Queen's `focus_agent`, a spawned session
/// being selected), so back always returns to what was on screen before the jump.
impl MyrstackApp {
    /// Records the selected node if it changed since the last frame.
    fn record_selection(&mut self) {
        self.history.record(self.selected.as_ref());
    }

    /// Selects the previous or next node in the history and centers the graph on it.
    fn history_step(&mut self, step: ui::history::HistoryStep) {
        let target = match step {
            ui::history::HistoryStep::Back => self.history.back(self.selected.as_ref()),
            ui::history::HistoryStep::Forward => self.history.forward(),
        };
        let Some(id) = target else {
            return;
        };
        self.graph.set_selected(Some(&id));
        if self.graph.is_visible(&id) {
            self.graph.center_on(&id);
        }
        self.selected = Some(id);
    }
}

/// Multi-selection: several nodes selected in the graph (box or Shift/Cmd-click).
impl MyrstackApp {
    /// The "N selected" panel, in the conversation column while 2+ nodes are selected.
    fn selection_contents(&mut self, ui: &mut egui::Ui) {
        let selection = self.graph.selection();
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let nodes: Vec<&Node> = selection
            .iter()
            .filter_map(|id| snapshot.node(id))
            .collect();
        match ui::selection::selection_panel(ui, &nodes) {
            Some(ui::selection::SelectionAction::SelectOnly(id)) => {
                self.graph.set_selected(Some(&id));
                self.graph.center_on(&id);
                self.selected = Some(id);
            }
            Some(ui::selection::SelectionAction::Close) => {
                self.request_close(selection.into_iter().collect());
            }
            Some(ui::selection::SelectionAction::Whip) => {
                self.request_whip(selection.into_iter().collect());
            }
            Some(ui::selection::SelectionAction::Clear) => {
                self.graph.set_selected(None);
                self.selected = None;
            }
            None => {}
        }
    }
}

/// Closing (killing) sessions: requests from the terminal panel, the graph's context menu, `x`
/// and the sidebar's "Close shown…" open one confirmation dialog; confirmed steps run on the
/// action runner.
impl MyrstackApp {
    fn request_close(&mut self, ids: Vec<NodeId>) {
        self.close.request(ui::close::CloseRequest::Nodes(ids));
    }

    /// Draws the dialog and submits a confirmed plan's steps, in order.
    /// The worktree cleanup window and the broom while it removes.
    fn worktree_cleanup(&mut self, ctx: &egui::Context, now_ms: i64) {
        let worktrees = self.data.as_ref().map(|d| &d.worktrees[..]);
        if self.worktrees.tick(worktrees, now_ms) {
            self.poller.send(PollerCommand::RefreshNow);
        }
        self.worktrees.window(ctx);
        if !std::mem::take(&mut self.broom_in_farm) {
            self.worktrees.broom(ctx);
        }
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        let terminals = self.data.as_ref().map_or(&[][..], |d| &d.terminals[..]);
        let confirmed =
            self.close
                .show(ctx, self.snapshot.as_ref(), terminals, self.graph.hidden());
        if let Some((plan, labels)) = confirmed {
            log::info!("closing sessions: {:?}", plan.steps);
            let ids: Vec<_> = plan
                .actions()
                .into_iter()
                .map(|action| self.actions.submit(action))
                .collect();
            self.close.started(plan, labels, &ids, now_ms());
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
        if let Some(panes) = state
            .panes
            .as_deref()
            .and_then(ui::dock::PaneLayout::decode)
        {
            self.panes = panes;
        }
        self.panes.notifications = state.notifications_panel;
        self.chat_hidden = state.chat_hidden;
        self.filter = state.filter;
        self.selected = state.layout.selected.clone();
        self.graph.import_layout(state.layout);
        self.search
            .set_recents(ui::commands::Recents::from_keys(&state.recent_actions));
    }

    fn persisted_state(&self) -> PersistedState {
        PersistedState {
            view: self.view,
            chat_hidden: self.chat_hidden,
            filter: self.filter.clone(),
            layout: self.graph.export_layout(),
            recent_actions: self.search.recents().keys(),
            panes: self.panes.encode(),
            notifications_panel: self.panes.notifications,
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
        self.frame_stats.tick(ctx);
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
            self.claude.watch_forwards(data.declared_coordinators());
            let mut data = data;
            if let Some(claude) = &self.claude_data {
                data.forwards.clone_from(&claude.forwards);
            }
            if let Some(screen) = &self.screen
                && let Some(old) = &self.data
                && let Some(verdict) = old.needs_input.get(&screen.handle)
            {
                data.needs_input
                    .insert(screen.handle.clone(), verdict.clone());
            }
            for message in self.traffic.inbox.update(&data.inbox, now_ms()) {
                self.traffic.push(message);
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
            if let Some(data) = &mut self.data {
                data.forwards.clone_from(&claude.forwards);
            }
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
        rebuild |= std::mem::take(&mut self.overlays_changed);
        self.poll_quota();
        self.poll_playbook();
        if rebuild && let Some(data) = &self.data {
            let now = now_ms();
            let mut snapshot = FleetSnapshot::build_with_stalled(
                data,
                self.claude_data.as_ref(),
                &self.whip_status.escalated,
                now,
            );
            self.asks.prune(data, self.claude_data.as_ref());
            snapshot.mark_waiting(&self.asks.waiting(data, self.claude_data.as_ref()));
            self.finished.prune(now, crate::model::RELEASED_FADE_MS);
            snapshot.add_finished(self.finished.recent(now, crate::model::RELEASED_FADE_MS));
            self.graph.set_hidden(&self.filter.hidden(&snapshot));
            self.graph.sync(&snapshot);
            if let Some(claude) = &self.claude_data {
                self.graph.set_usage(claude.node_usages(&snapshot.nodes));
                self.graph
                    .set_building(claude.building_nodes(&snapshot.nodes));
            }
            if let Some(id) = &self.selected
                && snapshot.node(id).is_none()
            {
                self.selected = None;
            }
            self.history.prune(|id| snapshot.node(id).is_some());
            self.snapshot = Some(snapshot);
            self.sidebar_cache.snapshot_changed();
            self.search.snapshot_changed();
            self.whip_log.snapshot_changed();
            self.feed_whip();
            self.notify_needs_input();
            self.spawn_select_pending();
        }
        self.poll_notifications(ctx);
        for message in self.claude.terminal_sends() {
            self.traffic.push(message);
        }
        self.poll_traffic(ctx);
        self.poll_chat();
        self.talk_logic(ctx);
        self.poll_control(ctx);
        self.poll_review();
        self.poll_envoy();
        self.poll_health();
        self.poll_whip_now();
        self.poll_spawn();
        self.run_finish_closes();
        for result in self.actions.poll() {
            self.control_action_done(&result);
            self.envoy_action_done(&result);
            if self.input.on_result(&result, now_ms()) {
                self.poller.send(PollerCommand::RefreshScreen);
            }
            if self.close.on_result(&result, now_ms()) {
                self.poller.send(PollerCommand::RefreshNow);
            }
        }
        self.sync_screen_watch();
        self.sync_transcript();
        self.publish_control_status();
        ctx.request_repaint_after(TICK);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now_ms = now_ms();
        self.talk_ui(ui.ctx());
        self.dictation_frame(ui.ctx());
        self.search_palette(ui.ctx());
        self.keyboard(ui.ctx());
        let top_bar = egui::Panel::top("top_bar")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ui, |ui| self.top_bar(ui, now_ms));
        self.panes(ui, now_ms);
        self.notification_toasts(ui.ctx(), top_bar.response.rect.bottom());
        self.close_dialog(ui.ctx());
        self.worktree_cleanup(ui.ctx(), now_ms);
        self.settings_window(ui.ctx(), now_ms);
        if self.whip_window.take_whip_stalled() {
            self.request_whip_stalled(None);
        }
        self.whip_now_dialogs(ui.ctx(), now_ms);
        self.spawn_dialog(ui.ctx());
        self.record_selection();
        self.sync_screen_watch();
    }
}

/// A repaint callback for background threads that draws one frame. `request_repaint` would
/// draw two (egui lets frame-delayed responses settle), doubling what each data arrival costs.
fn repaint_once(ctx: &egui::Context) -> Box<dyn Fn() + Send + Sync> {
    let ctx = ctx.clone();
    Box::new(move || ctx.request_repaint_after(REPAINT_SOON))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
