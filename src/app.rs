//! The eframe `App`: owns UI state and draws the panels each frame.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;

use crate::APP_NAME;

mod chat;
mod control;
mod dictation;
mod palette;
mod playbook;
mod review;
mod settings;
mod spawn;
mod talk;
mod whip_now;
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
    /// Whether the details column is open, collapsed or follows the window width.
    details_pref: ui::columns::DetailsPref,
    /// ⌘J hid the Queen's chat, which otherwise fills the conversation column when nothing
    /// is selected.
    chat_hidden: bool,
    /// Left edge of the conversation column's content last frame, `None` while it is closed;
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
    /// Tail of the selected subagent's transcript.
    transcript: Option<TranscriptTail>,
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
    /// Rate-limited macOS notifications, shared with the whip's executors.
    notifier: Arc<Notifier>,
    needs_input_detector: NeedsInputDetector,
    /// Posted notifications, newest last; the top bar lists them so they can select their node.
    alerts: Vec<Delivered>,
    /// Debug-logged frames per second and repaint causes.
    frame_stats: ui::frame_stats::FrameStats,
    /// Polls `orca account list` for plan quotas every few minutes.
    quota: QuotaPoller,
    quotas: Quotas,
    /// Control API server, its published status view and commands in flight.
    control: control::AppControl,
    /// "Close session" confirmation dialog and the log of closes.
    close: ui::close::CloseDialog,
    /// Who fetched which playbook version, re-read off the UI thread.
    playbook_watch: playbook::PlaybookWatch,
    /// The Queen's chat, drawn in the conversation column.
    chat: chat::AppChat,
    /// Manual whips in flight, their confirmation popover and result toast.
    whip_now: whip_now::WhipNowState,
    /// "New session…" dialog, spawns in flight and the new node to select.
    spawn: spawn::AppSpawn,
    /// ⌘E push-to-talk dictation.
    dictation: dictation::AppDictation,
    /// Hold-to-talk to the Queen (global hotkey) and her spoken replies.
    talk: talk::AppTalk,
    /// PRs agents marked ready for review, their GitHub state and the poller reading it.
    review: review::AppReview,
}

/// How many posted notifications the alerts menu keeps.
const MAX_ALERTS: usize = 10;

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
        let whip = WhipRunner::spawn(
            whip_window.whip().clone(),
            Box::new(move || {
                Box::new(whip::OrcaExecutor::default().with_notifier(whip_notifier.clone()))
            }),
            Some(repaint_once(ctx)),
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
            selected: None,
            view: View::default(),
            details_pref: ui::columns::DetailsPref::default(),
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
            alerts: Vec::new(),
            search: ui::search::SearchPalette::default(),
            help_open: false,
            frame_stats: ui::frame_stats::FrameStats::default(),
            close: ui::close::CloseDialog::default(),
            playbook_watch: playbook::PlaybookWatch::spawn(repaint_once(ctx)),
            chat,
            whip_now: whip_now::WhipNowState::default(),
            spawn: spawn::AppSpawn::start(ctx),
            dictation: dictation::AppDictation::new(ctx),
            talk,
            review: review::AppReview::start(ctx),
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

    /// The right-hand column, full height: the Queen's chat, the selected agent's screen and
    /// input or transcript, or the multi-selection panel.
    fn conversation_column(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if !self.has_conversation() {
            self.conversation_left = None;
            return;
        }
        let window = ui.ctx().content_rect().width();
        let margin = egui::Frame::side_top_panel(ui.style()).inner_margin.leftf();
        let response = egui::Panel::right("terminal")
            .resizable(true)
            .default_size(window * ui::columns::CONVERSATION_SHARE)
            .min_size(ui::columns::CONVERSATION_MIN_WIDTH)
            .show(ui, |ui| self.conversation_contents(ui, now_ms))
            .response;
        self.conversation_left = Some(response.rect.left() + margin);
    }

    fn conversation_contents(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        if self.graph.selection().len() > 1 {
            self.selection_contents(ui);
        } else if self.shows_chat() {
            if let Some(id) = self.chat.ui(ui, self.snapshot.as_ref()) {
                self.select_and_focus(id);
            }
        } else if let Some((node, info)) = self.selected_subagent() {
            let tail = self.transcript.as_ref().map_or(&[][..], |t| &t.lines[..]);
            ui::subagent::subagent_transcript(ui, node, info, tail);
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
        let input = ui::terminal::PanelInput {
            state: &mut self.input,
            terminal,
        };
        let response = ui::terminal::terminal_panel(ui, node, self.screen.as_ref(), input, now_ms);
        let id = node.id.clone();
        self.apply_terminal_response(id, response);
    }

    /// The details column between the graph and the conversation column: the selected node's
    /// attributes and actions, or the Queen's fleet summary, review queue and broadcast.
    /// Collapses to a strip.
    fn details_column(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        use ui::columns::{self, DetailsColumn};
        let window = ui.ctx().content_rect().width();
        let shown = columns::details_column(self.details_pref, self.has_details(), window);
        let toggle = match shown {
            DetailsColumn::Hidden => false,
            DetailsColumn::Collapsed => {
                egui::Panel::right("details_strip")
                    .resizable(false)
                    .exact_size(columns::STRIP_WIDTH)
                    .show(ui, |ui| {
                        ui.button("«")
                            .on_hover_text("Show the selection's details")
                            .clicked()
                    })
                    .inner
            }
            DetailsColumn::Open => {
                egui::Panel::right("details")
                    .resizable(true)
                    .default_size(columns::DETAILS_DEFAULT_WIDTH)
                    .min_size(columns::DETAILS_MIN_WIDTH)
                    .max_size(columns::DETAILS_MAX_WIDTH)
                    .show(ui, |ui| {
                        let collapse = ui
                            .horizontal(|ui| {
                                ui::widgets::section_label(ui, "Details");
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.small_button("»")
                                            .on_hover_text("Collapse the details column")
                                            .clicked()
                                    },
                                )
                                .inner
                            })
                            .inner;
                        egui::ScrollArea::vertical()
                            .id_salt("details_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.details_contents(ui, now_ms));
                        collapse
                    })
                    .inner
            }
        };
        if toggle {
            self.details_pref = columns::toggled(shown);
        }
    }

    fn details_contents(&mut self, ui: &mut egui::Ui, now_ms: i64) {
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
        for request in &response.requests {
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
    }

    /// The Queen's details: the review queue, her header and fleet summary, and broadcast.
    fn queen_details(&mut self, ui: &mut egui::Ui, now_ms: i64) {
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

    /// Switches to the graph and selects and focuses `id` there, as `focus_agent` does.
    fn focus_in_graph(&mut self, id: NodeId) {
        self.view = View::Graph;
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
            KeyCommand::ToggleChat | KeyCommand::OpenSettings => None,
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

    /// The top bar: the app's controls, and the Queen and Status report buttons and plan
    /// quotas at the right, above the conversation column.
    fn top_bar(&mut self, ui: &mut egui::Ui, now_ms: i64) {
        let bar = ui.max_rect();
        let row = ui.spacing().interact_size.y;
        let x = ui::columns::quick_actions_x(
            bar.left(),
            bar.right(),
            self.quick_actions_width,
            self.conversation_left,
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
            ui.weak(format!("{} terminals", data.terminals.len()));
            ui.separator();
            ui.weak(format!(
                "updated {} ago",
                ui::table::format_age(now_ms - data.polled_at_ms)
            ));
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
        self.alerts_menu(ui);
        self.review_badge(ui);
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

/// Notifications: nodes entering NeedsInput, and the in-app list of posted notifications.
///
/// Clicking a native notification (running from `Myrstack.app`) selects its node; clicking an
/// `osascript` one cannot reach Myrstack, so the alerts menu is how it selects its node.
impl MyrstackApp {
    /// Selects the node of each clicked notification, as picking its alert would, and brings
    /// the window forward. Clicks wait for the first snapshot (a click can launch the app).
    fn open_clicked_notifications(&mut self, ctx: &egui::Context) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let mut select = None;
        let mut review = false;
        for targets in self.notifier.clicked() {
            self.alerts.retain(|a| a.targets != targets);
            select = notify::click_target(snapshot, &targets).or(select);
            review |= self.is_review_alert(&targets);
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if review && select.is_none() {
            self.open_review_queue();
        }
        if let Some(id) = select {
            self.select_and_focus(id);
        }
    }

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
                let target = self
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| notify::click_target(snapshot, &alert.targets));
                if let Some(id) = target {
                    self.select_and_focus(id);
                } else if self.is_review_alert(&alert.targets) {
                    self.open_review_queue();
                }
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
        self.details_pref = state.details;
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
            details: self.details_pref,
            chat_hidden: self.chat_hidden,
            filter: self.filter.clone(),
            layout: self.graph.export_layout(),
            recent_actions: self.search.recents().keys(),
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
        self.poll_quota();
        self.poll_playbook();
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
            self.sidebar_cache.snapshot_changed();
            self.search.snapshot_changed();
            self.whip_log.snapshot_changed();
            self.feed_whip();
            self.notify_needs_input();
            self.spawn_select_pending();
        }
        self.take_alerts();
        self.open_clicked_notifications(ctx);
        self.poll_chat();
        self.talk_logic(ctx);
        self.poll_control(ctx);
        self.poll_review();
        self.poll_whip_now();
        self.poll_spawn();
        for result in self.actions.poll() {
            self.control_action_done(&result);
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
        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ui, |ui| self.top_bar(ui, now_ms));
        // Side columns before the graph's own bars, so they run the full height.
        self.conversation_column(ui, now_ms);
        self.details_column(ui, now_ms);
        let mut whip_pick = None;
        if self.view == View::Graph
            && let Some(snapshot) = &self.snapshot
        {
            let filter = &mut self.filter;
            let (whip_log, whip_status) = (&mut self.whip_log, &self.whip_status);
            let cache = &mut self.sidebar_cache;
            let response = egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| {
                    ui::sidebar::sidebar_cached(ui, snapshot, cache, filter, |ui| {
                        whip_pick = whip_log.section(ui, whip_status, snapshot, now_ms);
                    })
                })
                .inner;
            if response.filter_changed {
                self.graph.set_hidden(&self.filter.hidden(snapshot));
            }
            if let Some(run_id) = &response.focus_run {
                self.graph.focus_run(run_id);
            }
            if let Some(repo) = &response.focus_repo {
                self.graph.focus_repo(repo);
            }
            if let Some(id) = &response.focus_node {
                self.graph.center_on(id);
            }
            if let Some(id) = response.select {
                self.graph.set_selected(Some(&id));
                self.selected = Some(id);
            }
            if response.close_shown {
                self.close.request(ui::close::CloseRequest::Shown);
            }
            if let Some(repo) = &response.new_session_repo {
                self.open_spawn_dialog(Some(repo));
            }
        }
        if let Some(id) = whip_pick {
            self.select_and_focus(id);
        }
        self.hidden_selection_notice(ui);
        if self.view == View::Graph {
            egui::Panel::bottom("legend")
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .inner_margin(egui::Margin::symmetric(10, 6)),
                )
                .show(ui, ui::graph::legend);
        }
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
                    if !response.close_requested.is_empty() {
                        self.close
                            .request(ui::close::CloseRequest::Nodes(response.close_requested));
                    }
                    if !response.whip_requested.is_empty() {
                        self.request_whip(response.whip_requested);
                    }
                }
                View::Table => ui::table::terminal_table(ui, data, now_ms),
            },
        });
        self.close_dialog(ui.ctx());
        self.settings_window(ui.ctx(), now_ms);
        if self.whip_window.take_whip_stalled() {
            self.request_whip_stalled(None);
        }
        self.whip_now_dialogs(ui.ctx(), now_ms);
        self.spawn_dialog(ui.ctx());
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
