//! Terminal panel: the selected node's info, live screen and input box.

use std::collections::HashMap;

use eframe::egui;

use crate::actions::{Action, ActionId, ActionResult, Key};
use crate::claude::Usage;
use crate::model::{Node, NodeKind, NodeState};
use crate::orca::{Terminal, TerminalInput, TerminalStatus};
use crate::poller::ScreenUpdate;
use crate::ui::graph::state_color;

/// Columns the screen font is sized to fit in the panel width.
const TARGET_COLUMNS: f32 = 110.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 14.0;

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
    /// Actions to run; the caller submits each and reports the id via
    /// [`InputPanelState::submitted`].
    pub requests: Vec<ActionRequest>,
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
    /// The node's row in `terminal list`, if live.
    pub terminal: Option<&'a Terminal>,
    /// Claude token usage of the node's session.
    pub usage: Option<&'a Usage>,
}

/// The terminal whose screen should be polled for `node`: live terminals only (not dead
/// workers or subagents).
pub fn watch_handle(node: &Node) -> Option<&str> {
    if !node.live || node.kind == NodeKind::Subagent {
        return None;
    }
    node.terminal_handle.as_deref()
}

/// Draws the header for `node`, then its screen (if it has a live terminal).
pub fn terminal_panel(
    ui: &mut egui::Ui,
    node: &Node,
    screen: Option<&ScreenUpdate>,
    input: PanelInput<'_>,
    now_ms: i64,
) -> TerminalPanelResponse {
    let mut response = TerminalPanelResponse::default();
    let handle = watch_handle(node);
    let has_screen = handle.is_some();
    // Only an update for this node's terminal counts.
    let screen = screen.filter(|s| Some(s.handle.as_str()) == handle);

    let open_shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);
    if let Some(handle) = handle
        && ui.input_mut(|i| i.consume_shortcut(&open_shortcut))
    {
        response.requests.push(ActionRequest::open(handle));
    }

    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("×")
                .on_hover_text("Close (clears selection)")
                .clicked()
            {
                response.close = true;
            }
            let open = ui
                .add_enabled(handle.is_some(), egui::Button::new("Open in Orca"))
                .on_hover_text(format!(
                    "Show this terminal's tab in Orca ({})",
                    ui.ctx().format_shortcut(&open_shortcut)
                ))
                .on_disabled_hover_text("No live terminal.");
            if open.clicked()
                && let Some(handle) = handle
            {
                response.requests.push(ActionRequest::open(handle));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&node.label).strong()).truncate());
            });
        });
    });
    header(ui, node, screen, has_screen, input.usage, now_ms);
    ui.separator();

    if !has_screen {
        ui.weak(match node.kind {
            NodeKind::Subagent => "Subagents have no terminal.",
            _ => "No live terminal.",
        });
        return response;
    }

    if let Some(handle) = handle {
        let blocked = input_block_reason(input.terminal, screen);
        let needs_input = node.state == NodeState::NeedsInput;
        egui::Panel::bottom("terminal_input")
            .resizable(false)
            .show(ui, |ui| {
                input_area(
                    ui,
                    handle,
                    input.state,
                    blocked,
                    needs_input,
                    now_ms,
                    &mut response.requests,
                );
            });
    }
    screen_view(ui, screen);

    response
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
        ui.add(
            egui::TextEdit::multiline(state.draft_mut(handle))
                .id(edit_id)
                .hint_text("Type a reply…  Enter sends, Shift+Enter adds a line")
                .font(egui::TextStyle::Monospace)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        )
        .on_disabled_hover_text(disabled_text);
    });

    ui.horizontal_wrapped(|ui| {
        let send = ui
            .add_enabled(can_send, egui::Button::new("Send"))
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
            ui.label(kind_label(node.kind));
            ui.end_row();

            ui.weak("state");
            let color = state_color(node.state, ui.visuals().dark_mode);
            ui.colored_label(color, state_label(node.state));
            ui.end_row();

            if let Some(agent) = &node.agent {
                ui.weak("agent");
                ui.label(agent.as_str());
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

fn screen_view(ui: &mut egui::Ui, screen: Option<&ScreenUpdate>) {
    let Some(screen) = screen else {
        return;
    };
    let font = screen_font(ui, ui.available_width());
    let text = screen.lines.join("\n");
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(4.0)
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt("terminal_screen")
                .auto_shrink(false)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).font(font))
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
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
        crate::ui::table::format_age(age_ms)
    }
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Coordinator => "coordinator",
        NodeKind::Worker => "worker",
        NodeKind::Terminal => "terminal",
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
