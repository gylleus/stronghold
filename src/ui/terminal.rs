//! Terminal panel: the selected node's info, live screen and input box.

use std::collections::HashMap;

use eframe::egui;

use crate::actions::{Action, ActionId, ActionResult, Key};
use crate::claude::Usage;
use crate::model::{Node, NodeId, NodeKind, NodeState};
use crate::orca::{Terminal, TerminalInput, TerminalStatus};
use crate::poller::ScreenUpdate;
use crate::ui::chat::TranscriptView;
use crate::ui::fleet_view::state_color;
use crate::ui::styled_view::styled_view;
use crate::ui::term_style::AgentKind;

/// Columns the screen font is sized to fit in the panel width.
const TARGET_COLUMNS: f32 = 110.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 14.0;
/// Styled lines wrap, so they need not shrink as far as a fixed-column screen.
const STYLED_MIN_FONT_SIZE: f32 = 10.5;

/// A second Interrupt click within this window confirms it.
pub const INTERRUPT_CONFIRM_MS: i64 = 2_000;
/// How long "sent ✓" stays visible.
const OK_STATUS_MS: i64 = 4_000;
/// Digit buttons shown while a node needs input.
const CHOICE_DIGITS: [u8; 4] = [1, 2, 3, 4];

/// What the user did in the panel this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerminalPanelResponse {
    /// The close (×) button was clicked.
    pub close: bool,
    /// "Close…" was clicked: ask to kill the node's session.
    pub close_session: bool,
    /// "Whip" was clicked: nudge the node now.
    pub whip: bool,
    /// Actions to run; the caller submits each and reports the id via
    /// [`InputPanelState::submitted`].
    pub requests: Vec<ActionRequest>,
    /// Open a PR through the review queue's browser action.
    pub review: Option<crate::ui::review::ReviewAction>,
    /// A node link in the conversation was clicked.
    pub select: Option<NodeId>,
}

/// What an input action was, for status text and draft handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// The draft, with Enter.
    Text,
    Enter,
    Escape,
    Digit(u8),
    Interrupt,
    /// Open in Orca.
    Open,
}

impl InputKind {
    fn done_label(self) -> String {
        match self {
            InputKind::Text => "sent ✓".into(),
            InputKind::Enter => "Enter sent ✓".into(),
            InputKind::Escape => "Esc sent ✓".into(),
            InputKind::Digit(d) => format!("{d} sent ✓"),
            InputKind::Interrupt => "interrupt sent ✓".into(),
            InputKind::Open => "opened in Orca ✓".into(),
        }
    }

    fn failed_label(self) -> &'static str {
        match self {
            InputKind::Open => "open in Orca failed",
            _ => "send failed",
        }
    }
}

/// An action the panel wants run.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionRequest {
    pub action: Action,
    pub kind: InputKind,
}

impl ActionRequest {
    fn send(handle: &str, kind: InputKind, input: TerminalInput) -> Self {
        Self {
            action: Action::Send {
                handle: handle.to_owned(),
                input,
            },
            kind,
        }
    }

    pub fn open(handle: &str) -> Self {
        Self {
            action: Action::Switch {
                handle: handle.to_owned(),
            },
            kind: InputKind::Open,
        }
    }
}

/// Last action outcome shown under the input box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputStatus {
    Pending(InputKind),
    Ok { kind: InputKind, at_ms: i64 },
    Err { kind: InputKind, message: String },
}

/// Interrupt needs two clicks on the same terminal within [`INTERRUPT_CONFIRM_MS`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterruptGuard {
    armed: Option<(String, i64)>,
}

impl InterruptGuard {
    /// Registers a click; `true` means "confirmed, send it now".
    pub fn click(&mut self, handle: &str, now_ms: i64) -> bool {
        if self.is_armed(handle, now_ms) {
            self.armed = None;
            true
        } else {
            self.armed = Some((handle.to_owned(), now_ms));
            false
        }
    }

    pub fn is_armed(&self, handle: &str, now_ms: i64) -> bool {
        self.armed.as_ref().is_some_and(|(h, at)| {
            h == handle && (0..=INTERRUPT_CONFIRM_MS).contains(&(now_ms - at))
        })
    }
}

/// A queued action the panel waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    handle: String,
    kind: InputKind,
    /// The draft as sent, for [`InputKind::Text`].
    sent_draft: Option<String>,
}

/// Input box state kept across frames and selection changes: drafts and statuses per
/// terminal handle, in-flight actions and the interrupt guard.
#[derive(Debug, Default)]
pub struct InputPanelState {
    drafts: HashMap<String, String>,
    statuses: HashMap<String, InputStatus>,
    pending: HashMap<ActionId, Pending>,
    pub interrupt: InterruptGuard,
    /// Show the terminal's live screen instead of the conversation.
    pub show_screen: bool,
}

impl InputPanelState {
    pub fn draft(&self, handle: &str) -> &str {
        self.drafts.get(handle).map_or("", String::as_str)
    }

    pub fn draft_mut(&mut self, handle: &str) -> &mut String {
        self.drafts.entry(handle.to_owned()).or_default()
    }

    pub fn status(&self, handle: &str) -> Option<&InputStatus> {
        self.statuses.get(handle)
    }

    /// Whether a text send for `handle` is still in flight.
    pub fn text_pending(&self, handle: &str) -> bool {
        self.pending
            .values()
            .any(|p| p.handle == handle && p.kind == InputKind::Text)
    }

    /// Records that `request` was queued as `id`.
    pub fn submitted(&mut self, id: ActionId, request: &ActionRequest) {
        let handle = request.action.handle().to_owned();
        let sent_draft = match (&request.action, request.kind) {
            (Action::Send { .. }, InputKind::Text) => Some(self.draft(&handle).to_owned()),
            _ => None,
        };
        self.statuses
            .insert(handle.clone(), InputStatus::Pending(request.kind));
        self.pending.insert(
            id,
            Pending {
                handle,
                kind: request.kind,
                sent_draft,
            },
        );
    }

    /// Applies a finished action. A successful text send clears the draft unless it was edited
    /// meanwhile; a failure keeps it. Returns `true` for a successful send (the screen is worth
    /// re-reading).
    pub fn on_result(&mut self, result: &ActionResult, now_ms: i64) -> bool {
        let Some(pending) = self.pending.remove(&result.id) else {
            return false;
        };
        let status = match &result.outcome {
            Ok(()) => {
                if let Some(sent) = &pending.sent_draft
                    && self.draft(&pending.handle) == sent
                {
                    self.drafts.remove(&pending.handle);
                }
                InputStatus::Ok {
                    kind: pending.kind,
                    at_ms: now_ms,
                }
            }
            Err(message) => InputStatus::Err {
                kind: pending.kind,
                message: message.clone(),
            },
        };
        // Only the newest action's outcome is shown.
        let newer_pending = self.pending.values().any(|p| p.handle == pending.handle);
        if !newer_pending {
            self.statuses.insert(pending.handle.clone(), status);
        }
        result.outcome.is_ok() && matches!(result.action, Action::Send { .. })
    }
}

/// Why input to `terminal` is disabled, or `None` if it can be sent to.
pub fn input_block_reason(
    terminal: Option<&Terminal>,
    screen: Option<&ScreenUpdate>,
) -> Option<&'static str> {
    let Some(terminal) = terminal else {
        return Some("The terminal is not live in Orca.");
    };
    if !terminal.connected {
        return Some("The terminal is disconnected.");
    }
    if !terminal.writable {
        return Some("Orca reports the terminal as not writable.");
    }
    if screen.is_some_and(|s| s.status == Some(TerminalStatus::Exited)) {
        return Some("The terminal's process has exited.");
    }
    None
}

/// Removes plain Enter presses (no Shift/Alt/Ctrl) from `events`, returning whether there was
/// one. Shift+Enter is left for the text box, which inserts a newline.
pub fn take_submit_enter(events: &mut Vec<egui::Event>) -> bool {
    let before = events.len();
    events.retain(|event| {
        !matches!(
            event,
            egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                modifiers,
                ..
            } if !modifiers.shift && !modifiers.alt && !modifiers.ctrl
        )
    });
    events.len() != before
}

/// Input for the selected node's terminal, from the app.
pub struct PanelInput<'a> {
    pub state: &'a mut InputPanelState,
    /// The same live rows that drive the farm PR badges.
    pub review_rows: &'a [crate::review::view::ReviewRow],
    /// The node's row in `terminal list`, if live.
    pub terminal: Option<&'a Terminal>,
    /// The agent's conversation read from its session transcript, when it has one (Claude
    /// agents mapped to a session).
    pub conversation: Option<&'a mut TranscriptView>,
    /// Whether a node id names a node in the current snapshot (only those become links).
    pub is_node: &'a dyn Fn(&NodeId) -> bool,
}

/// The terminal whose screen should be polled for `node`: live terminals only (not dead
/// workers or subagents).
pub fn watch_handle(node: &Node) -> Option<&str> {
    if !node.live || node.kind == NodeKind::Subagent {
        return None;
    }
    node.terminal_handle.as_deref()
}

/// The details column for `node`: its label, Close… and attributes. Open in Orca and Whip
/// sit in the conversation header instead, where they are used.
pub fn details_panel(
    ui: &mut egui::Ui,
    node: &Node,
    screen: Option<&ScreenUpdate>,
    usage: Option<&Usage>,
    now_ms: i64,
) -> TerminalPanelResponse {
    let mut response = TerminalPanelResponse::default();
    let handle = watch_handle(node);
    let screen = screen.filter(|s| Some(s.handle.as_str()) == handle);

    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if ui
                .button("×")
                .on_hover_text("Close (clears selection)")
                .clicked()
            {
                response.close = true;
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&node.label).strong()).wrap());
            });
        });
    });
    if node.kind != NodeKind::Subagent
        && ui
            .button(egui::RichText::new("Close…").color(ui.visuals().error_fg_color))
            .on_hover_text("Kill this session in Orca (asks first) — x")
            .clicked()
    {
        response.close_session = true;
    }
    ui.add_space(4.0);
    header(ui, node, screen, handle.is_some(), usage, now_ms);
    response
}

/// ⌘O: show the selected terminal's tab in Orca.
const OPEN_SHORTCUT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);

/// Below this width the conversation's Open in Orca and Whip sit under its title.
const ACTIONS_INLINE_MIN_WIDTH: f32 = 420.0;

/// Text of the switch to the agent's conversation.
pub const CHAT_TAB: &str = "Chat";
/// Text of the switch to the terminal's live screen.
pub const SCREEN_TAB: &str = "Screen";

/// The conversation column for `node`: a one-line title, then its conversation (the whole
/// session, from its transcript) or live screen, and the input box. While the agent needs
/// input, the live screen also shows above the input box, so its prompt can be answered.
pub fn terminal_panel(
    ui: &mut egui::Ui,
    node: &Node,
    screen: Option<&ScreenUpdate>,
    input: PanelInput<'_>,
    now_ms: i64,
) -> TerminalPanelResponse {
    let mut response = TerminalPanelResponse::default();
    let handle = watch_handle(node);
    // Only an update for this node's terminal counts.
    let screen = screen.filter(|s| Some(s.handle.as_str()) == handle);

    if let Some(handle) = handle
        && ui.input_mut(|i| i.consume_shortcut(&OPEN_SHORTCUT))
    {
        response.requests.push(ActionRequest::open(handle));
    }

    let PanelInput {
        state,
        terminal,
        mut conversation,
        is_node,
        review_rows,
    } = input;
    let has_chat = conversation.is_some();
    // A narrow pane gives the actions their own row, so the title keeps its room.
    let actions_below = ui.available_width() < ACTIONS_INLINE_MIN_WIDTH;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("×")
                .on_hover_text("Close (clears selection)")
                .clicked()
            {
                response.close = true;
            }
            if has_chat && handle.is_some() {
                // Right to left: Screen, then Chat.
                if ui
                    .selectable_label(state.show_screen, SCREEN_TAB)
                    .on_hover_text("The terminal's live screen")
                    .clicked()
                {
                    state.show_screen = true;
                }
                if ui
                    .selectable_label(!state.show_screen, CHAT_TAB)
                    .on_hover_text("The whole conversation, from the session transcript")
                    .clicked()
                {
                    state.show_screen = false;
                }
            }
            if !actions_below {
                agent_actions(ui, node, handle, &mut response);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let color = state_color(node.state, ui.visuals().dark_mode);
                crate::ui::widgets::state_pill(ui, color, state_label(node.state));
                let has_prs = review_rows
                    .iter()
                    .any(|row| row.agent.as_ref().is_some_and(|agent| agent.id == node.id));
                let name_width = if has_prs && !actions_below {
                    ui.available_width() * 0.4
                } else {
                    ui.available_width()
                };
                ui.add_sized(
                    [name_width, ui.spacing().interact_size.y],
                    egui::Label::new(egui::RichText::new(&node.label).strong()).truncate(),
                )
                .on_hover_text(&node.label);
                if !actions_below {
                    response.review =
                        crate::ui::review::agent_pr_buttons(ui, &node.id, review_rows);
                }
            });
        });
    });
    if actions_below {
        if review_rows
            .iter()
            .any(|row| row.agent.as_ref().is_some_and(|agent| agent.id == node.id))
        {
            ui.horizontal(|ui| {
                response.review = crate::ui::review::agent_pr_buttons(ui, &node.id, review_rows);
            });
        }
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                agent_actions(ui, node, handle, &mut response);
            });
        });
    }
    ui.separator();

    let agent = AgentKind::from_identity(node.agent.as_ref());
    let Some(handle) = handle else {
        if conversation.is_some() {
            response.select = crate::ui::chat::transcript_conversation(
                ui,
                ("agent_conversation", &node.id),
                conversation,
                false,
                is_node,
            );
        } else {
            ui.weak(match node.kind {
                NodeKind::Subagent => "Subagents have no terminal.",
                _ => "No live terminal.",
            });
        }
        return response;
    };
    let blocked = input_block_reason(terminal, screen);
    let needs_input = node.state == NodeState::NeedsInput;
    egui::Panel::bottom("terminal_input")
        .resizable(false)
        .show(ui, |ui| {
            input_area(
                ui,
                handle,
                state,
                blocked,
                needs_input,
                now_ms,
                &mut response.requests,
            );
        });
    let show_chat = conversation.is_some() && !state.show_screen;
    if !show_chat {
        screen_view(ui, "terminal_screen", screen, agent);
        return response;
    }
    if needs_input && screen.is_some() {
        egui::Panel::bottom("terminal_live_screen")
            .resizable(true)
            .default_size(ui.available_height() * 0.4)
            .show(ui, |ui| {
                ui.weak("Live screen: the agent is waiting for an answer");
                screen_view(ui, "terminal_screen_live", screen, agent);
            });
    }
    let working = node.state == NodeState::Working;
    response.select = crate::ui::chat::transcript_conversation(
        ui,
        ("agent_conversation", &node.id),
        conversation.take(),
        working,
        is_node,
    );
    response
}

/// Open in Orca and Whip for `node`, in a right-to-left header (Whip is added first so it ends
/// up right of Open in Orca). Neither is shown for the Queen or a subagent: they have no
/// terminal of their own.
fn agent_actions(
    ui: &mut egui::Ui,
    node: &Node,
    handle: Option<&str>,
    response: &mut TerminalPanelResponse,
) {
    if matches!(node.kind, NodeKind::Queen | NodeKind::Subagent) {
        return;
    }
    let whippable = handle.is_some()
        && matches!(
            node.kind,
            NodeKind::Coordinator | NodeKind::Worker | NodeKind::Terminal
        );
    if ui
        .add_enabled(whippable, egui::Button::new("Whip"))
        .on_hover_text("Send this agent a Myrstack nudge now — w")
        .on_disabled_hover_text("No live terminal to nudge.")
        .clicked()
    {
        response.whip = true;
    }
    let open = ui
        .add_enabled(
            handle.is_some(),
            crate::ui::widgets::orca_button(ui, handle.is_some()),
        )
        .on_hover_text(format!(
            "Show this terminal's tab in Orca ({})",
            ui.ctx().format_shortcut(&OPEN_SHORTCUT)
        ))
        .on_disabled_hover_text("No live terminal.");
    if open.clicked()
        && let Some(handle) = handle
    {
        response.requests.push(ActionRequest::open(handle));
    }
}

/// Widget id of `handle`'s input box, so other code can focus it.
pub fn input_id(handle: &str) -> egui::Id {
    egui::Id::new(("terminal_input", handle))
}

/// The input box, key buttons and last-action status for `handle`.
fn input_area(
    ui: &mut egui::Ui,
    handle: &str,
    state: &mut InputPanelState,
    blocked: Option<&str>,
    needs_input: bool,
    now_ms: i64,
    requests: &mut Vec<ActionRequest>,
) {
    let enabled = blocked.is_none();
    let disabled_text = blocked.unwrap_or_default();
    let edit_id = input_id(handle);
    let text_pending = state.text_pending(handle);
    let can_send = enabled && !text_pending && !state.draft(handle).trim().is_empty();

    ui.add_space(4.0);
    // Enter sends; take it before the text box sees it (Shift+Enter still inserts a newline).
    let enter_pressed = enabled
        && ui.memory(|m| m.has_focus(edit_id))
        && ui.input_mut(|i| take_submit_enter(&mut i.events));
    let mut send_text = enter_pressed && can_send;

    ui.add_enabled_ui(enabled, |ui| {
        let response = ui
            .add(
                egui::TextEdit::multiline(state.draft_mut(handle))
                    .id(edit_id)
                    .hint_text("Type a reply…  Enter sends, Shift+Enter adds a line")
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            )
            .on_disabled_hover_text(disabled_text);
        crate::ui::widgets::focus_ring(ui, &response);
    });

    ui.horizontal_wrapped(|ui| {
        let send = ui
            .add_enabled(
                can_send,
                crate::ui::widgets::primary_button(ui, can_send, "Send"),
            )
            .on_hover_text("Send the text followed by Enter")
            .on_disabled_hover_text(if enabled {
                "Type something first."
            } else {
                disabled_text
            });
        send_text |= send.clicked();

        let key_button = |ui: &mut egui::Ui, label: &str, hover: &str| {
            ui.add_enabled(enabled, egui::Button::new(label))
                .on_hover_text(hover)
                .on_disabled_hover_text(disabled_text)
                .clicked()
        };
        if key_button(ui, "⏎", "Send a bare Enter (accept the default choice)") {
            requests.push(ActionRequest::send(
                handle,
                InputKind::Enter,
                TerminalInput::enter_key(),
            ));
        }
        if key_button(ui, "Esc", "Send the Escape key") {
            requests.push(ActionRequest::send(
                handle,
                InputKind::Escape,
                TerminalInput::key(Key::Escape),
            ));
        }

        let armed = state.interrupt.is_armed(handle, now_ms);
        let (label, hover) = if armed {
            ("Confirm interrupt", "Click again to send Ctrl-C")
        } else {
            ("Interrupt", "Send Ctrl-C (click twice within 2 s)")
        };
        let mut button = egui::Button::new(label);
        if armed {
            button = button.fill(ui.visuals().error_fg_color.gamma_multiply(0.35));
            // Repaint when the confirm window closes so the button resets.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(
                    u64::try_from(INTERRUPT_CONFIRM_MS).unwrap_or(0),
                ));
        }
        let interrupt = ui
            .add_enabled(enabled, button)
            .on_hover_text(hover)
            .on_disabled_hover_text(disabled_text);
        if interrupt.clicked() && state.interrupt.click(handle, now_ms) {
            requests.push(ActionRequest::send(
                handle,
                InputKind::Interrupt,
                TerminalInput::interrupt_key(),
            ));
        }

        if needs_input {
            ui.separator();
            for digit in CHOICE_DIGITS {
                let label = digit.to_string();
                if key_button(
                    ui,
                    &label,
                    &format!("Pick choice {digit} (sends the digit key)"),
                ) {
                    requests.push(ActionRequest::send(
                        handle,
                        InputKind::Digit(digit),
                        TerminalInput::key(Key::Digit(digit)),
                    ));
                }
            }
        }
    });

    if send_text {
        let text = state
            .draft(handle)
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        requests.push(ActionRequest::send(
            handle,
            InputKind::Text,
            TerminalInput::line(text),
        ));
    }

    status_line(ui, state.status(handle), now_ms);
    ui.add_space(2.0);
}

fn status_line(ui: &mut egui::Ui, status: Option<&InputStatus>, now_ms: i64) {
    match status {
        Some(InputStatus::Pending(_)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("sending…");
            });
        }
        Some(InputStatus::Ok { kind, at_ms }) if now_ms - at_ms < OK_STATUS_MS => {
            let color = state_color(NodeState::Done, ui.visuals().dark_mode);
            ui.colored_label(color, kind.done_label());
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
        Some(InputStatus::Err { kind, message }) => {
            let text = format!("{}: {message}", kind.failed_label());
            ui.add(
                egui::Label::new(egui::RichText::new(&text).color(ui.visuals().error_fg_color))
                    .truncate(),
            )
            .on_hover_text(text);
        }
        _ => {
            // Keep the panel height steady.
            ui.label("");
        }
    }
}

fn header(
    ui: &mut egui::Ui,
    node: &Node,
    screen: Option<&ScreenUpdate>,
    has_screen: bool,
    usage: Option<&Usage>,
    now_ms: i64,
) {
    egui::Grid::new("terminal_panel_header")
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            ui.weak("kind");
            ui.label(crate::ui::icons::with_icon(
                ui,
                crate::ui::icons::kind_icon(node.kind),
                kind_label(node.kind),
                egui::TextStyle::Body,
                None,
            ));
            ui.end_row();

            ui.weak("state");
            let color = state_color(node.state, ui.visuals().dark_mode);
            ui.horizontal(|ui| {
                crate::ui::widgets::state_pill(ui, color, state_label(node.state));
            });
            ui.end_row();

            if let Some(agent) = &node.agent {
                ui.weak("agent");
                ui.label(agent.as_str());
                ui.end_row();
            }
            if let Some(repo) = node
                .worktree_path
                .as_deref()
                .and_then(crate::model::repo_of)
            {
                ui.weak("repo");
                ui.label(repo);
                ui.end_row();
            }
            if let Some(run) = &node.run_id {
                ui.weak("run");
                ui.add(egui::Label::new(run.as_str()).truncate());
                ui.end_row();
            }
            if let Some(task) = &node.task_id {
                ui.weak("task");
                ui.add(egui::Label::new(task.as_str()).truncate());
                ui.end_row();
            }
            if let Some(branch) = &node.branch {
                ui.weak("branch");
                ui.add(egui::Label::new(branch.as_str()).truncate());
                ui.end_row();
            }
            if let Some(worktree) = &node.worktree_path {
                ui.weak("worktree");
                ui.add(egui::Label::new(worktree.as_str()).truncate())
                    .on_hover_text(worktree.as_str());
                ui.end_row();
            }
            if let Some(handle) = &node.terminal_handle {
                ui.weak("terminal");
                ui.add(egui::Label::new(handle.as_str()).truncate());
                ui.end_row();
            }
            if let Some(usage) = usage {
                ui.weak("tokens");
                ui.label(usage.label()).on_hover_text(usage.detail());
                ui.end_row();
            }
            if has_screen {
                ui.weak("screen");
                match screen {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("reading…");
                        });
                    }
                    Some(s) => {
                        let mut text = format!("{} old", format_screen_age(now_ms - s.read_at_ms));
                        if s.status == Some(TerminalStatus::Exited) {
                            text.push_str(" · exited");
                        }
                        ui.horizontal(|ui| {
                            ui.label(text);
                            if let Some(err) = &s.error {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(err).color(ui.visuals().error_fg_color),
                                    )
                                    .truncate(),
                                );
                            }
                        });
                    }
                }
                ui.end_row();
            }
        });
}

fn screen_view(ui: &mut egui::Ui, id_salt: &str, screen: Option<&ScreenUpdate>, agent: AgentKind) {
    let Some(screen) = screen else {
        return;
    };
    let font_size = screen_font(ui, ui.available_width())
        .size
        .max(STYLED_MIN_FONT_SIZE);
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            styled_view(ui, id_salt, &screen.lines, agent, font_size);
        });
}

/// Monospace font sized so [`TARGET_COLUMNS`] fit in `width`, clamped to a readable range.
fn screen_font(ui: &egui::Ui, width: f32) -> egui::FontId {
    let probe = egui::FontId::monospace(10.0);
    let char_width_at_10 = ui.ctx().fonts_mut(|f| f.glyph_width(&probe, 'M'));
    let size = fit_font_size(width, char_width_at_10 / 10.0);
    egui::FontId::monospace(size)
}

/// Font size at which `TARGET_COLUMNS` characters of `char_width_per_pt` fill `width`.
pub fn fit_font_size(width: f32, char_width_per_pt: f32) -> f32 {
    if char_width_per_pt <= 0.0 || !width.is_finite() {
        return MAX_FONT_SIZE;
    }
    (width / (TARGET_COLUMNS * char_width_per_pt)).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

/// Age with one decimal under a minute (`"0.4s"`), coarser after.
pub fn format_screen_age(age_ms: i64) -> String {
    let age_ms = age_ms.max(0);
    if age_ms < 60_000 {
        format!("{:.1}s", age_ms as f64 / 1000.0)
    } else {
        crate::ui::fleet_view::format_age(age_ms)
    }
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Coordinator => "coordinator",
        NodeKind::Attendant => "attendant",
        NodeKind::Worker => "worker",
        NodeKind::Terminal => "terminal",
        NodeKind::Envoy => "envoy",
        NodeKind::Subagent => "subagent",
        NodeKind::Queen => "queen",
    }
}

fn state_label(state: NodeState) -> &'static str {
    match state {
        NodeState::Working => "working",
        NodeState::Idle => "idle",
        NodeState::NeedsInput => "needs input",
        NodeState::Stalled => "stalled",
        NodeState::Done => "done",
        NodeState::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeId;

    fn node(kind: NodeKind, live: bool, handle: Option<&str>) -> Node {
        Node {
            id: NodeId("n".into()),
            kind,
            label: "n".into(),
            terminal_handle: handle.map(Into::into),
            live,
            run_id: None,
            task_id: None,
            dispatch_id: None,
            agent: None,
            worktree_path: None,
            branch: None,
            last_output_ms: None,
            preview: None,
            state: NodeState::Idle,
            worker_state: None,
            dispatch_status: None,
            worker_terminal_state: None,
            released_at_ms: None,
            parked_at_ms: None,
        }
    }

    /// Clicks `label` in `node`'s conversation.
    fn click_in_chat(node: &Node, label: &str) -> TerminalPanelResponse {
        let mut state = InputPanelState::default();
        crate::ui::test_util::click_label(label, |ui| {
            let input = PanelInput {
                review_rows: &[],
                state: &mut state,
                terminal: None,
                conversation: None,
                is_node: &|_| false,
            };
            terminal_panel(ui, node, None, input, 0)
        })
    }

    #[test]
    fn whip_button_asks_to_whip_a_live_agent() {
        for kind in [NodeKind::Worker, NodeKind::Coordinator, NodeKind::Terminal] {
            let response = click_in_chat(&node(kind, true, Some("t1")), "Whip");
            assert!(response.whip, "{kind:?}");
            assert!(response.requests.is_empty());
        }
        // Disabled without a live terminal: the click does nothing.
        let response = click_in_chat(&node(NodeKind::Worker, false, Some("t1")), "Whip");
        assert!(!response.whip);
    }

    #[test]
    fn open_in_orca_button_asks_to_switch_to_the_terminal() {
        let response = click_in_chat(&node(NodeKind::Worker, true, Some("t1")), "Open in Orca");
        assert_eq!(response.requests, vec![ActionRequest::open("t1")]);
        assert!(!response.whip);
        // Disabled without a live terminal.
        let response = click_in_chat(&node(NodeKind::Worker, false, Some("t1")), "Open in Orca");
        assert!(response.requests.is_empty());
    }

    #[test]
    fn narrow_conversation_keeps_the_actions_on_their_own_row() {
        let node = node(NodeKind::Worker, true, Some("t1"));
        let mut state = InputPanelState::default();
        let response = crate::ui::test_util::click_label("Open in Orca", |ui| {
            ui.set_max_width(ACTIONS_INLINE_MIN_WIDTH - 1.0);
            let input = PanelInput {
                review_rows: &[],
                state: &mut state,
                terminal: None,
                conversation: None,
                is_node: &|_| false,
            };
            terminal_panel(ui, &node, None, input, 0)
        });
        assert_eq!(response.requests, vec![ActionRequest::open("t1")]);
    }

    /// Shapes of one headless frame of `draw`.
    fn frame(draw: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let mut draw = draw;
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| draw(ui));
        output.textures_delta.clear();
        output
    }

    /// A conversation of `turns` prompts and replies, loaded as from a transcript.
    fn loaded_view(turns: usize) -> TranscriptView {
        use crate::chat::history::{HistoryChunk, HistoryRecord};
        use crate::chat::protocol::parse_line;
        let mut records = Vec::new();
        for i in 0..turns {
            records.push(HistoryRecord::Prompt(format!("question {i}")));
            let reply = format!(
                r#"{{"type":"assistant","message":{{"id":"m{i}","content":[{{"type":"text","text":"answer {i}"}}]}}}}"#
            );
            records.push(HistoryRecord::Message(parse_line(&reply).unwrap()));
        }
        let path = std::path::PathBuf::from("/s.jsonl");
        let mut view = TranscriptView::new(path.clone());
        assert!(view.apply(&HistoryChunk {
            path,
            reset: false,
            start: 0,
            end: 10,
            records,
        }));
        view
    }

    /// Texts drawn in frames of the agent pane for `node` with a conversation and a screen;
    /// `click` is clicked in the last frame when given.
    fn agent_pane(
        node: &Node,
        state: &mut InputPanelState,
        view: &mut TranscriptView,
        click: Option<&str>,
    ) -> Vec<String> {
        let ctx = egui::Context::default();
        let screen = ScreenUpdate {
            handle: "t1".into(),
            lines: vec!["screen row".into()],
            status: None,
            read_at_ms: 0,
            error: None,
        };
        let mut texts = Vec::new();
        let mut events = Vec::new();
        for i in 0..6 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 900.0),
                )),
                time: Some(i as f64 / 60.0),
                events: std::mem::take(&mut events),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                let input = PanelInput {
                    review_rows: &[],
                    state: &mut *state,
                    terminal: None,
                    conversation: Some(&mut *view),
                    is_node: &|_| false,
                };
                terminal_panel(ui, node, Some(&screen), input, 0);
            });
            output.textures_delta.clear();
            if let Some(label) = click
                && (2..5).contains(&i)
                && let Some(at) = crate::ui::test_util::text_center(&output, label)
            {
                events = match i {
                    2 => vec![egui::Event::PointerMoved(at)],
                    _ => vec![egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: i == 3,
                        modifiers: egui::Modifiers::NONE,
                    }],
                };
            }
            texts = Vec::new();
            for clipped in &output.shapes {
                collect_texts(&clipped.shape, &mut texts);
            }
        }
        texts
    }

    fn collect_texts(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect_texts(s, out)),
            _ => {}
        }
    }

    /// With a transcript, the pane shows the conversation at its newest message rather than
    /// the screen; the switch shows the screen instead.
    #[test]
    fn agent_pane_shows_the_whole_conversation_and_can_switch_to_the_screen() {
        let node = node(NodeKind::Worker, true, Some("t1"));
        let mut state = InputPanelState::default();
        let mut view = loaded_view(500);
        let texts = agent_pane(&node, &mut state, &mut view, None);
        let has = |t: &str| texts.iter().any(|x| x == t);
        assert!(has(CHAT_TAB) && has(SCREEN_TAB), "{texts:?}");
        assert!(has("answer 499"), "the newest reply is in view: {texts:?}");
        assert!(!has("question 0"), "only rows in view are drawn");
        assert!(!has("screen row"), "{texts:?}");
        assert!(view.list.following());

        let texts = agent_pane(&node, &mut state, &mut view, Some(SCREEN_TAB));
        assert!(state.show_screen);
        let texts = if texts.iter().any(|t| t == "screen row") {
            texts
        } else {
            agent_pane(&node, &mut state, &mut view, None)
        };
        assert!(texts.iter().any(|t| t == "screen row"), "{texts:?}");
    }

    /// While the agent waits for an answer, its live screen shows under the conversation.
    #[test]
    fn agent_pane_shows_the_live_screen_while_it_needs_input() {
        let mut node = node(NodeKind::Worker, true, Some("t1"));
        node.state = NodeState::NeedsInput;
        let mut state = InputPanelState::default();
        let mut view = loaded_view(50);
        let texts = agent_pane(&node, &mut state, &mut view, None);
        assert!(texts.iter().any(|t| t == "screen row"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "answer 49"), "{texts:?}");
    }

    #[test]
    fn transcript_view_takes_chunks_in_order_only() {
        use crate::chat::history::{HistoryChunk, HistoryRecord};
        let path = std::path::PathBuf::from("/s.jsonl");
        let chunk = |start, end, reset, text: &str| HistoryChunk {
            path: path.clone(),
            reset,
            start,
            end,
            records: vec![HistoryRecord::Prompt(text.into())],
        };
        let mut view = TranscriptView::new(path.clone());
        assert!(!view.loaded);
        assert!(view.apply(&chunk(0, 10, false, "a")));
        assert!(view.loaded);
        assert!(!view.apply(&chunk(0, 10, false, "a")), "a stale repeat");
        assert!(view.apply(&chunk(10, 20, false, "b")));
        let other = HistoryChunk {
            path: "/other.jsonl".into(),
            ..chunk(20, 30, false, "c")
        };
        assert!(!view.apply(&other), "another file");
        assert_eq!(view.transcript.items.len(), 2);
        assert!(view.apply(&chunk(0, 5, true, "new")), "a rewritten file");
        assert_eq!(
            view.transcript.items,
            vec![crate::chat::ChatItem::User("new".into())]
        );
    }

    fn chat_frame(node: &Node) -> egui::FullOutput {
        let mut state = InputPanelState::default();
        frame(|ui| {
            let input = PanelInput {
                review_rows: &[],
                state: &mut state,
                terminal: None,
                conversation: None,
                is_node: &|_| false,
            };
            terminal_panel(ui, node, None, input, 0);
        })
    }

    #[test]
    fn agent_actions_are_in_the_conversation_attributes_in_the_details() {
        use crate::ui::test_util::text_center;
        let node = node(NodeKind::Worker, true, Some("t1"));
        let details = frame(|ui| {
            details_panel(ui, &node, None, None, 0);
        });
        for label in ["kind", "terminal", "Close…"] {
            assert!(text_center(&details, label).is_some(), "{label} in details");
        }
        for label in ["Whip", "Open in Orca"] {
            assert!(
                text_center(&details, label).is_none(),
                "{label} not in details"
            );
        }
        let conversation = chat_frame(&node);
        for label in ["Whip", "Open in Orca", "×"] {
            assert!(
                text_center(&conversation, label).is_some(),
                "{label} in chat"
            );
        }
        for label in ["kind", "terminal"] {
            assert!(
                text_center(&conversation, label).is_none(),
                "{label} not in chat"
            );
        }
    }

    #[test]
    fn no_agent_actions_for_subagents() {
        use crate::ui::test_util::text_center;
        let conversation = chat_frame(&node(NodeKind::Subagent, true, Some("t1")));
        for label in ["Whip", "Open in Orca"] {
            assert!(text_center(&conversation, label).is_none(), "{label}");
        }
    }

    #[test]
    fn watch_handle_only_for_live_terminals() {
        assert_eq!(
            watch_handle(&node(NodeKind::Terminal, true, Some("t1"))),
            Some("t1")
        );
        assert_eq!(
            watch_handle(&node(NodeKind::Worker, true, Some("t2"))),
            Some("t2")
        );
        assert_eq!(
            watch_handle(&node(NodeKind::Worker, false, Some("t3"))),
            None
        );
        assert_eq!(
            watch_handle(&node(NodeKind::Subagent, true, Some("t4"))),
            None
        );
        assert_eq!(watch_handle(&node(NodeKind::Terminal, true, None)), None);
    }

    fn text_request(handle: &str, text: &str) -> ActionRequest {
        ActionRequest::send(handle, InputKind::Text, TerminalInput::line(text))
    }

    fn result(id: ActionId, request: &ActionRequest, outcome: Result<(), &str>) -> ActionResult {
        ActionResult {
            id,
            action: request.action.clone(),
            outcome: outcome.map_err(str::to_owned),
            note: None,
        }
    }

    #[test]
    fn drafts_are_kept_per_handle() {
        let mut state = InputPanelState::default();
        state.draft_mut("t1").push_str("hello");
        state.draft_mut("t2").push_str("other");
        assert_eq!(state.draft("t1"), "hello");
        assert_eq!(state.draft("t2"), "other");
        assert_eq!(state.draft("t3"), "");
    }

    #[test]
    fn successful_send_clears_draft_and_requests_refresh() {
        let mut state = InputPanelState::default();
        state.draft_mut("t1").push_str("yes");
        let req = text_request("t1", "yes");
        state.submitted(7, &req);
        assert!(state.text_pending("t1"));
        assert_eq!(
            state.status("t1"),
            Some(&InputStatus::Pending(InputKind::Text))
        );
        assert!(state.on_result(&result(7, &req, Ok(())), 1_000));
        assert_eq!(state.draft("t1"), "");
        assert!(!state.text_pending("t1"));
        assert_eq!(
            state.status("t1"),
            Some(&InputStatus::Ok {
                kind: InputKind::Text,
                at_ms: 1_000
            })
        );
    }

    #[test]
    fn failed_send_keeps_draft_and_shows_error() {
        let mut state = InputPanelState::default();
        state.draft_mut("t1").push_str("yes");
        let req = text_request("t1", "yes");
        state.submitted(1, &req);
        assert!(!state.on_result(&result(1, &req, Err("boom")), 0));
        assert_eq!(state.draft("t1"), "yes");
        assert_eq!(
            state.status("t1"),
            Some(&InputStatus::Err {
                kind: InputKind::Text,
                message: "boom".into()
            })
        );
    }

    #[test]
    fn draft_edited_while_sending_is_kept() {
        let mut state = InputPanelState::default();
        state.draft_mut("t1").push_str("yes");
        let req = text_request("t1", "yes");
        state.submitted(1, &req);
        state.draft_mut("t1").push_str(" and more");
        state.on_result(&result(1, &req, Ok(())), 0);
        assert_eq!(state.draft("t1"), "yes and more");
    }

    #[test]
    fn key_sends_leave_draft_and_open_does_not_refresh() {
        let mut state = InputPanelState::default();
        state.draft_mut("t1").push_str("draft");
        let esc = ActionRequest::send("t1", InputKind::Escape, TerminalInput::key(Key::Escape));
        state.submitted(1, &esc);
        assert!(!state.text_pending("t1"));
        assert!(state.on_result(&result(1, &esc, Ok(())), 0));
        assert_eq!(state.draft("t1"), "draft");

        let open = ActionRequest::open("t1");
        state.submitted(2, &open);
        assert!(!state.on_result(&result(2, &open, Ok(())), 0));
        // Unknown ids are ignored.
        assert!(!state.on_result(&result(99, &open, Ok(())), 0));
    }

    #[test]
    fn older_result_does_not_hide_newer_pending_status() {
        let mut state = InputPanelState::default();
        let first = ActionRequest::send("t1", InputKind::Enter, TerminalInput::enter_key());
        let second =
            ActionRequest::send("t1", InputKind::Digit(2), TerminalInput::key(Key::Digit(2)));
        state.submitted(1, &first);
        state.submitted(2, &second);
        state.on_result(&result(1, &first, Ok(())), 0);
        assert_eq!(
            state.status("t1"),
            Some(&InputStatus::Pending(InputKind::Digit(2)))
        );
    }

    #[test]
    fn interrupt_needs_second_click_within_window() {
        let mut guard = InterruptGuard::default();
        assert!(!guard.click("t1", 0));
        assert!(guard.is_armed("t1", 1_000));
        assert!(!guard.is_armed("t2", 1_000));
        assert!(guard.click("t1", 1_500));
        assert!(!guard.is_armed("t1", 1_500));

        // Too slow: the second click re-arms instead.
        assert!(!guard.click("t1", 10_000));
        assert!(!guard.click("t1", 10_000 + INTERRUPT_CONFIRM_MS + 1));
        assert!(guard.click("t1", 12_100));

        // A click on another terminal doesn't confirm.
        assert!(!guard.click("t1", 20_000));
        assert!(!guard.click("t2", 20_100));
        assert!(!guard.is_armed("t1", 20_100));
    }

    #[test]
    fn plain_enter_submits_shift_enter_does_not() {
        let key = |modifiers| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let mut events = vec![key(egui::Modifiers::SHIFT)];
        assert!(!take_submit_enter(&mut events));
        assert_eq!(events.len(), 1);

        let mut events = vec![key(egui::Modifiers::NONE), egui::Event::Text("x".into())];
        assert!(take_submit_enter(&mut events));
        assert_eq!(events, vec![egui::Event::Text("x".into())]);

        let mut events = vec![key(egui::Modifiers::COMMAND)];
        assert!(take_submit_enter(&mut events));
    }

    #[test]
    fn input_blocked_unless_live_and_writable() {
        let live = Terminal {
            handle: "t1".into(),
            connected: true,
            writable: true,
            ..Terminal::default()
        };
        assert_eq!(input_block_reason(Some(&live), None), None);
        assert!(input_block_reason(None, None).is_some());
        let read_only = Terminal {
            writable: false,
            ..live.clone()
        };
        assert!(input_block_reason(Some(&read_only), None).is_some());
        let disconnected = Terminal {
            connected: false,
            ..live.clone()
        };
        assert!(input_block_reason(Some(&disconnected), None).is_some());
        let exited = ScreenUpdate {
            handle: "t1".into(),
            lines: vec![],
            status: Some(TerminalStatus::Exited),
            read_at_ms: 0,
            error: None,
        };
        assert!(input_block_reason(Some(&live), Some(&exited)).is_some());
    }

    #[test]
    fn font_fits_target_columns_within_bounds() {
        // 0.6 pt of width per pt of size: 660 px fits 110 columns at 10 pt.
        assert!((fit_font_size(660.0, 0.6) - 10.0).abs() < 1e-4);
        assert_eq!(fit_font_size(100.0, 0.6), MIN_FONT_SIZE);
        assert_eq!(fit_font_size(5000.0, 0.6), MAX_FONT_SIZE);
        assert_eq!(fit_font_size(500.0, 0.0), MAX_FONT_SIZE);
    }

    #[test]
    fn screen_age_formatting() {
        assert_eq!(format_screen_age(400), "0.4s");
        assert_eq!(format_screen_age(-20), "0.0s");
        assert_eq!(format_screen_age(12_345), "12.3s");
        assert_eq!(format_screen_age(120_000), "2m");
    }
}
