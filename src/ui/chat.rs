//! The Queen's chat pane: header with the session state and buttons, the message list
//! (user bubbles, markdown replies streaming in, collapsible tool rows, permission cards) and
//! the input box.

use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText, Ui};
use serde_json::Value;

use crate::chat::SessionState;
use crate::chat::history::HistoryChunk;
use crate::chat::permissions::{
    DESTRUCTIVE_TOOLS, MYRSTACK_PREFIX, short_tool_name, summarize_input, truncate,
};
use crate::chat::say::split_say;
use crate::chat::transcript::{ChatItem, Decision, PermissionCard, ToolCall, Transcript};
use crate::model::NodeId;
use crate::ui::message_list::MessageList;

/// Longest tool input/result shown in an expanded row.
const MAX_DETAIL_CHARS: usize = 4000;

/// User messages longer than this many lines show collapsed, with "Show more".
const COLLAPSED_LINES: usize = 12;
/// …or longer than this many characters.
const COLLAPSED_CHARS: usize = 1200;

/// Draft and focus state of the pane, kept by the app across frames.
#[derive(Debug, Default)]
pub struct ChatPanelState {
    pub draft: String,
    /// Focus the input box on the next frame.
    pub focus_input: bool,
    /// Scroll position and row heights of the message list.
    pub list: MessageList,
}

/// A conversation read from a session transcript on disk (an agent's or a subagent's), with
/// its list state.
#[derive(Debug)]
pub struct TranscriptView {
    pub path: PathBuf,
    pub transcript: Transcript,
    pub list: MessageList,
    /// Bytes of the file applied so far; the next chunk must start here.
    read_to: u64,
    /// The first chunk (the whole file as it was) arrived.
    pub loaded: bool,
}

impl TranscriptView {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            transcript: Transcript::default(),
            list: MessageList::default(),
            read_to: 0,
            loaded: false,
        }
    }

    /// Applies `chunk` if it is the next part of this view's file; returns whether it was.
    pub fn apply(&mut self, chunk: &HistoryChunk) -> bool {
        if chunk.path != self.path {
            return false;
        }
        if chunk.reset {
            self.transcript = Transcript::default();
            self.list.reset();
        } else if chunk.start != self.read_to {
            return false;
        }
        for record in &chunk.records {
            self.transcript.apply_history(record);
        }
        self.read_to = chunk.end;
        self.loaded = true;
        true
    }
}

/// What the user did in the pane this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatAction {
    Send(String),
    Interrupt,
    Decide {
        request_id: String,
        decision: Decision,
    },
    NewChat,
    Retry,
    SelectNode(NodeId),
    /// Karl flipped the Auto switch.
    SetAutoMode(bool),
}

/// What the pane reads each frame.
pub struct ChatView<'a> {
    pub transcript: &'a Transcript,
    pub state: &'a SessionState,
    /// Whether a node id names a node in the current snapshot (only those become links).
    pub is_node: &'a dyn Fn(&NodeId) -> bool,
    /// The Queen's tool calls run without asking.
    pub auto_mode: bool,
}

/// Line under the input while auto mode is on.
pub const AUTO_HINT: &str = "Auto mode: the Queen's actions run without asking";

/// Color of the auto mode badge, hint and notices.
fn accent(ui: &Ui) -> Color32 {
    ui.visuals().selection.bg_fill
}

/// Id of the input box, for focusing it from a shortcut.
pub fn input_id() -> egui::Id {
    egui::Id::new("queen_chat_input")
}

/// Draws the pane into the rest of `ui`.
pub fn chat_ui(ui: &mut Ui, state: &mut ChatPanelState, view: ChatView<'_>) -> Vec<ChatAction> {
    let mut actions = Vec::new();
    let running = view.transcript.running;
    let ready = *view.state == SessionState::Ready;
    header(ui, &view, &mut actions);

    egui::Panel::bottom("queen_chat_input_panel")
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            input_box(ui, state, ready, running, &mut actions);
            if view.auto_mode {
                ui.label(RichText::new(AUTO_HINT).small().color(accent(ui)));
            }
        });

    if view.transcript.items.is_empty() {
        crate::ui::art::empty_state(
            ui,
            crate::ui::art::queen(),
            96.0,
            "Ask the Queen",
            "Ask her about the fleet, or press Status report above.",
        );
    } else {
        let thinking = running && !streaming_text(view.transcript);
        if let Some(id) = conversation(
            ui,
            "queen_chat_messages",
            &mut state.list,
            &view.transcript.items,
            thinking,
            view.is_node,
            &mut actions,
        ) {
            actions.push(ChatAction::SelectNode(id));
        }
    }
    actions
}

/// The message list of `items` (plus a "thinking…" row when `thinking`), drawing only the rows
/// in view. Returns a node whose link was clicked; permission answers go to `actions`.
pub fn conversation(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    list: &mut MessageList,
    items: &[ChatItem],
    thinking: bool,
    is_node: &dyn Fn(&NodeId) -> bool,
    actions: &mut Vec<ChatAction>,
) -> Option<NodeId> {
    let mut clicked = None;
    let len = items.len() + usize::from(thinking);
    let metrics = TextMetrics::of(ui);
    let estimate = |i: usize, width: f32| {
        items
            .get(i)
            .map_or(metrics.row, |item| estimate_height(item, width, &metrics))
    };
    list.show(ui, id_salt, len, &estimate, &mut |ui, i| {
        let Some(item) = items.get(i) else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(WORKING);
            });
            return;
        };
        if let Some(id) = item_ui(ui, item, is_node, actions) {
            clicked = Some(id);
        }
    });
    clicked
}

/// A conversation read from disk into the rest of `ui`: a spinner until its first chunk, a
/// note when it is empty, else the message list (with a "working…" row while `running`).
/// Returns a node whose link was clicked.
pub fn transcript_conversation(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    view: Option<&mut TranscriptView>,
    running: bool,
    is_node: &dyn Fn(&NodeId) -> bool,
) -> Option<NodeId> {
    let Some(view) = view.filter(|v| v.loaded) else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.weak("Loading the conversation…");
        });
        return None;
    };
    if view.transcript.items.is_empty() {
        ui.weak("No messages yet.");
        return None;
    }
    let mut actions = Vec::new();
    conversation(
        ui,
        id_salt,
        &mut view.list,
        &view.transcript.items,
        running,
        is_node,
        &mut actions,
    )
}

/// The last row of a conversation whose agent is at work.
pub const WORKING: &str = "working…";

/// Row height and average character width of the body font, for estimating row heights.
struct TextMetrics {
    row: f32,
    char_width: f32,
}

impl TextMetrics {
    fn of(ui: &Ui) -> Self {
        let font = egui::TextStyle::Body.resolve(ui.style());
        ui.ctx().fonts_mut(|f| Self {
            row: f.row_height(&font),
            char_width: f.glyph_width(&font, 'n'),
        })
    }

    /// Height of `text` wrapped at `width`.
    fn text_height(&self, text: &str, width: f32) -> f32 {
        let per_line = (width / self.char_width).max(8.0);
        let lines: f32 = text
            .lines()
            .map(|l| (l.chars().count() as f32 / per_line).ceil().max(1.0))
            .sum();
        lines.max(1.0) * self.row
    }
}

/// A guess at the height of `item` at `width`, before it is first drawn.
fn estimate_height(item: &ChatItem, width: f32, m: &TextMetrics) -> f32 {
    match item {
        ChatItem::User(text) | ChatItem::VoiceUser(text) => {
            let shown: String = if is_long(text) {
                text.lines()
                    .take(COLLAPSED_LINES)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                text.clone()
            };
            m.text_height(&shown, width * 0.8 - 24.0) + 14.0
        }
        ChatItem::Assistant { text, .. } => m.text_height(text, width) + m.row * 0.5,
        ChatItem::Permission(card) if card.decision.is_none() => m.row * 8.0,
        ChatItem::Error(text) => m.text_height(text, width),
        ChatItem::Tool(_) | ChatItem::Permission(_) | ChatItem::Notice(_) | ChatItem::System(_) => {
            m.row
        }
    }
}

/// Whether the newest item is reply text still streaming in (the thinking indicator hides).
fn streaming_text(transcript: &Transcript) -> bool {
    matches!(
        transcript.items.last(),
        Some(ChatItem::Assistant { streaming: true, text }) if !text.is_empty()
    )
}

/// Label and color of the session state.
pub fn state_text(
    state: &SessionState,
    running: bool,
    visuals: &egui::Visuals,
) -> (String, Color32) {
    match state {
        SessionState::Ready if running => ("running".into(), visuals.warn_fg_color),
        SessionState::Ready => ("ready".into(), Color32::from_rgb(0x4c, 0xaf, 0x50)),
        SessionState::Starting => ("starting…".into(), visuals.weak_text_color()),
        SessionState::Crashed { retry_in, .. } => (
            format!("crashed · restarting in {}s", retry_in.as_secs().max(1)),
            visuals.error_fg_color,
        ),
        SessionState::Unavailable(_) => ("unavailable".into(), visuals.error_fg_color),
        SessionState::Stopped => ("stopped".into(), visuals.weak_text_color()),
    }
}

fn header(ui: &mut Ui, view: &ChatView<'_>, actions: &mut Vec<ChatAction>) {
    let running = view.transcript.running;
    ui.horizontal(|ui| {
        ui.add(crate::ui::art::queen_avatar(
            ui.text_style_height(&egui::TextStyle::Body) + 4.0,
        ));
        ui.label(RichText::new("Chat").strong());
        let (text, color) = state_text(view.state, running, ui.visuals());
        ui.label(RichText::new(format!("● {text}")).color(color))
            .on_hover_text(match view.state {
                SessionState::Crashed { reason, .. } => reason.as_str(),
                _ => "The Queen's Claude Code session",
            });
        if view.auto_mode {
            auto_badge(ui);
        }
        if let Some(model) = &view.transcript.model {
            ui.weak(model);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("New chat")
                .on_hover_text("Start a fresh conversation")
                .clicked()
            {
                actions.push(ChatAction::NewChat);
            }
            let mut auto = view.auto_mode;
            if ui
                .toggle_value(&mut auto, "Auto")
                .on_hover_text(
                    "Auto mode: every tool call of the Queen's (Myrstack sends, keys, closes, \
                     spawns and whip changes, file reads, web fetches, other MCP servers) \
                     runs without asking. close_agents still dry-runs unless told otherwise.",
                )
                .changed()
            {
                actions.push(ChatAction::SetAutoMode(auto));
            }
            // Status report is in the top bar, right above this header.
            if running
                && ui
                    .button("⏹ Stop")
                    .on_hover_text("Interrupt this turn")
                    .clicked()
            {
                actions.push(ChatAction::Interrupt);
            }
        });
    });
    if let SessionState::Unavailable(hint) = view.state {
        ui.colored_label(ui.visuals().error_fg_color, hint);
        if ui.button("Retry").clicked() {
            actions.push(ChatAction::Retry);
        }
    }
    ui.separator();
}

fn auto_badge(ui: &mut Ui) {
    egui::Frame::new()
        .fill(accent(ui))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(
                RichText::new("AUTO")
                    .small()
                    .strong()
                    .color(ui.visuals().selection.stroke.color),
            )
        })
        .response
        .on_hover_text("Auto mode is on: the Queen's actions run without asking");
}

fn input_box(
    ui: &mut Ui,
    state: &mut ChatPanelState,
    ready: bool,
    running: bool,
    actions: &mut Vec<ChatAction>,
) {
    ui.add_space(4.0);
    let id = input_id();
    let can_send = ready && !running && !state.draft.trim().is_empty();
    // Enter sends; take it before the text box sees it (Shift+Enter still adds a line).
    let enter = ready
        && ui.memory(|m| m.has_focus(id))
        && ui.input_mut(|i| crate::ui::terminal::take_submit_enter(&mut i.events));
    let response = ui.add_enabled(
        ready,
        egui::TextEdit::multiline(&mut state.draft)
            .id(id)
            .hint_text("Ask the Queen…  Enter sends, Shift+Enter adds a line")
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
    if state.focus_input && ready {
        response.request_focus();
        state.focus_input = false;
    }
    crate::ui::widgets::focus_ring(ui, &response);
    let clicked = ui
        .horizontal(|ui| {
            let send = ui
                .add_enabled(
                    can_send,
                    crate::ui::widgets::primary_button(ui, can_send, "Send"),
                )
                .clicked();
            if !ready {
                ui.weak("waiting for the session…");
            } else if running {
                ui.weak("the Queen is answering…");
            }
            send
        })
        .inner;
    if (enter || clicked) && can_send {
        actions.push(ChatAction::Send(std::mem::take(&mut state.draft)));
    }
}

/// Draws one message; returns a node whose link was clicked.
fn item_ui(
    ui: &mut Ui,
    item: &ChatItem,
    is_node: &dyn Fn(&NodeId) -> bool,
    actions: &mut Vec<ChatAction>,
) -> Option<NodeId> {
    match item {
        ChatItem::User(text) => user_bubble(ui, text),
        ChatItem::VoiceUser(text) => user_bubble(ui, &voice_bubble_text(text)),
        ChatItem::Assistant { text, .. } => {
            let split = split_say(text);
            if let Some(say) = split.say.as_deref().filter(|s| !s.is_empty()) {
                say_line(ui, say);
                if !split.rest.trim().is_empty() {
                    ui.add_space(4.0);
                }
            }
            if split.say.is_none() || !split.rest.trim().is_empty() {
                return markdown(ui, &split.rest, is_node);
            }
        }
        ChatItem::Tool(call) => tool_row(ui, call),
        ChatItem::Permission(card) => permission_card(ui, card, actions),
        ChatItem::Notice(text) => {
            ui.weak(RichText::new(text).italics());
        }
        ChatItem::System(text) => {
            ui.label(
                RichText::new(format!("⚙ {text}"))
                    .small()
                    .italics()
                    .color(accent(ui)),
            );
        }
        ChatItem::Error(text) => {
            ui.colored_label(ui.visuals().error_fg_color, text);
        }
    }
    None
}

/// Marker in front of the Queen's spoken line.
pub const SAY_MARKER: &str = "🔊";

/// The reply's spoken line (`<say>…</say>`): a highlighted summary above the rest, with the
/// speaker icon, or an accent bar where the font lacks it.
fn say_line(ui: &mut Ui, say: &str) {
    let accent = accent(ui);
    let has_icon = can_draw(ui, SAY_MARKER);
    let frame = egui::Frame::new()
        .fill(accent.gamma_multiply(0.18))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if has_icon {
                    ui.label(SAY_MARKER);
                }
                ui.add(egui::Label::new(RichText::new(say).strong()).wrap());
            });
        });
    if !has_icon {
        let rect = frame.response.rect;
        let bar = egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + 3.0, rect.max.y));
        ui.painter().rect_filled(bar, 1.5, accent);
    }
}

/// Whether the body font draws the character `c` rather than the replacement glyph.
pub fn can_draw(ui: &Ui, c: &str) -> bool {
    /// What egui draws for a missing glyph.
    const REPLACEMENT: char = '◻';
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.ctx().fonts_mut(|f| {
        if f.has_glyphs(&font, c) {
            return true;
        }
        // `has_glyphs` is also false for anything in the replacement glyph's own face (the
        // emoji font), so compare the drawn glyph's size with the replacement's (a glyph
        // that happens to match it counts as missing).
        let galley = f.layout_no_wrap(format!("{c}{REPLACEMENT}"), font, Color32::WHITE);
        let glyphs: Vec<_> = galley.rows.iter().flat_map(|r| &r.row.glyphs).collect();
        matches!(glyphs.as_slice(), [a, b]
            if a.uv_rect.size != b.uv_rect.size)
    })
}

/// Marker in front of a message Karl spoke (🎙 is not in egui's default fonts; 🎤 is).
pub const VOICE_MARKER: &str = "🎤";

/// A voice message's bubble text.
pub fn voice_bubble_text(text: &str) -> String {
    format!("{VOICE_MARKER} {text}")
}

/// Whether a user message is long enough to show collapsed.
fn is_long(text: &str) -> bool {
    text.len() > COLLAPSED_CHARS || text.lines().count() > COLLAPSED_LINES
}

/// The start of a long message shown collapsed: its first lines, cut to the size limit.
fn collapsed(text: &str) -> String {
    let head: String = text
        .lines()
        .take(COLLAPSED_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}…",
        truncate(&head, COLLAPSED_CHARS)
            .trim_end_matches('…')
            .trim_end()
    )
}

/// Text of the toggle under a collapsed message.
pub const SHOW_MORE: &str = "Show more";

fn user_bubble(ui: &mut Ui, text: &str) {
    let long = is_long(text);
    let id = ui.id().with("expanded");
    let mut expanded = long && ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        egui::Frame::new()
            .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.35))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.8);
                // The bubble sits on the right; its lines read left to right.
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    if long && !expanded {
                        ui.add(egui::Label::new(collapsed(text)).wrap());
                    } else {
                        ui.add(egui::Label::new(text).wrap());
                    }
                    if long {
                        let label = if expanded { "Show less" } else { SHOW_MORE };
                        if ui.link(RichText::new(label).small()).clicked() {
                            expanded = !expanded;
                            ui.data_mut(|d| d.insert_temp(id, expanded));
                        }
                    }
                });
            });
    });
}

/// `✓`, `✗` or `…` for a tool call's outcome.
pub fn tool_status_mark(call: &ToolCall) -> &'static str {
    match &call.result {
        None => "…",
        Some(r) if r.is_error => "✗",
        Some(_) => "✓",
    }
}

/// A tool row's title: name, outcome, what it ran on (the command, file, …) and, when auto
/// mode allowed it, an `auto` marker.
pub fn tool_title(call: &ToolCall) -> String {
    let (name, rest) = tool_parts(call);
    format!("🔧 {name} {}{rest}", tool_status_mark(call))
}

/// The text a tool row draws after its status icon: [`tool_title`] without the marks.
pub fn tool_label(call: &ToolCall) -> String {
    let (name, rest) = tool_parts(call);
    format!("{name}{rest}")
}

/// A tool call's short name, and what follows it: ` · summary` and ` · auto`.
fn tool_parts(call: &ToolCall) -> (String, String) {
    let mut rest = String::new();
    if let Some(summary) = call
        .input
        .as_ref()
        .map(crate::claude::transcript::tool_summary)
        .filter(|s| !s.is_empty())
    {
        rest.push_str(" · ");
        rest.push_str(&summary);
    }
    if call.auto {
        rest.push_str(" · auto");
    }
    (short_tool_name(&call.name), rest)
}

fn tool_row(ui: &mut Ui, call: &ToolCall) {
    let visuals = ui.visuals();
    let (icon, icon_color, color) = match &call.result {
        Some(r) if r.is_error => (
            crate::ui::icons::X,
            visuals.error_fg_color,
            visuals.error_fg_color,
        ),
        Some(_) => (
            crate::ui::icons::CHECK,
            crate::ui::graph::state_color(crate::model::NodeState::Done, visuals.dark_mode),
            visuals.weak_text_color(),
        ),
        None => (
            crate::ui::icons::HOURGLASS_HIGH,
            visuals.weak_text_color(),
            visuals.weak_text_color(),
        ),
    };
    let mut title = crate::ui::icons::with_icon(
        ui,
        icon,
        &tool_label(call),
        egui::TextStyle::Small,
        Some(color),
    );
    if let Some(section) = title.sections.first_mut() {
        section.format.color = icon_color;
    }
    title.wrap.max_width = ui.available_width() - 24.0;
    title.wrap.max_rows = 1;
    egui::CollapsingHeader::new(title)
        .id_salt(("chat_tool", &call.id))
        .show_background(false)
        .default_open(false)
        .show(ui, |ui| {
            if let Some(input) = &call.input {
                ui.weak("Input");
                code_block(ui, &truncate(&pretty(input), MAX_DETAIL_CHARS));
            }
            match &call.result {
                Some(result) => {
                    ui.weak(if result.is_error { "Error" } else { "Result" });
                    code_block(ui, &truncate(&pretty_text(&result.text), MAX_DETAIL_CHARS));
                }
                None => {
                    ui.weak("running…");
                }
            }
        });
}

fn permission_card(ui: &mut Ui, card: &PermissionCard, actions: &mut Vec<ChatAction>) {
    let request = &card.request;
    let name = short_tool_name(&request.tool_name);
    let reaches_agents = request
        .tool_name
        .strip_prefix(MYRSTACK_PREFIX)
        .is_some_and(|t| DESTRUCTIVE_TOOLS.contains(&t));
    if let Some(decision) = card.decision {
        let text = match decision {
            Decision::Allow => format!("✓ Approved {name}"),
            Decision::AlwaysAllow => format!("✓ Approved {name} (always, this chat)"),
            Decision::Deny => format!("✗ Denied {name}"),
            Decision::Cancelled => format!("– {name}: request cancelled"),
            Decision::Auto => format!("✓ Approved {name} (auto mode)"),
        };
        ui.weak(text);
        return;
    }
    let accent = ui.visuals().warn_fg_color;
    egui::Frame::group(ui.style())
        .stroke(egui::Stroke::new(1.5, accent))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(format!("Allow {name}?"))
                    .strong()
                    .color(accent),
            );
            if reaches_agents {
                ui.weak("This acts on agents as the Queen; they treat it as your instruction.");
            }
            if let Some(description) = &request.description {
                ui.weak(description);
            }
            egui::Grid::new(("perm_grid", &request.request_id))
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (key, value) in summarize_input(&request.input, 240) {
                        ui.weak(key);
                        ui.add(egui::Label::new(value).wrap());
                        ui.end_row();
                    }
                });
            egui::CollapsingHeader::new(RichText::new("Full input").small())
                .id_salt(("perm_input", &request.request_id))
                .default_open(false)
                .show(ui, |ui| {
                    code_block(ui, &truncate(&pretty(&request.input), MAX_DETAIL_CHARS));
                });
            ui.horizontal_wrapped(|ui| {
                let mut decide = |decision| {
                    actions.push(ChatAction::Decide {
                        request_id: request.request_id.clone(),
                        decision,
                    });
                };
                if ui.button(RichText::new("Approve").strong()).clicked() {
                    decide(Decision::Allow);
                }
                if ui.button("Deny").clicked() {
                    decide(Decision::Deny);
                }
                if ui.button("Always allow this tool for this chat").clicked() {
                    decide(Decision::AlwaysAllow);
                }
            });
        });
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Pretty-prints `text` when it is JSON.
fn pretty_text(text: &str) -> String {
    match serde_json::from_str::<Value>(text) {
        Ok(v @ (Value::Object(_) | Value::Array(_))) => pretty(&v),
        _ => text.to_owned(),
    }
}

fn code_block(ui: &mut Ui, text: &str) {
    let frame = egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).monospace().small()).wrap());
        });
    // Hover reveals a copy button in the corner (contains_pointer, so it survives its own hover).
    if frame.response.contains_pointer() {
        let rect = frame.response.rect;
        let button = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 54.0, rect.top() + 4.0),
            egui::vec2(48.0, 20.0),
        );
        if ui
            .put(button, egui::Button::new(RichText::new("Copy").small()))
            .on_hover_text("Copy this block")
            .clicked()
        {
            ui.ctx().copy_text(text.to_owned());
        }
    }
}

// ---- markdown ----

/// A block of a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading(String),
    Paragraph(String),
    /// A list item: indent level, marker (`•` or `3.`), text.
    Item(usize, String, String),
    Code(String),
    /// A table's rows of cells, the header first (the `|---|` row is dropped).
    Table(Vec<Vec<String>>),
    Rule,
}

/// Splits `text` into blocks: headings, paragraphs (soft line breaks joined), list items,
/// fenced code, tables and rules.
pub fn parse_blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut code: Option<Vec<&str>> = None;
    let flush = |paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(paragraph.join(" ")));
            paragraph.clear();
        }
    };
    for line in text.lines() {
        let continues_table =
            matches!(blocks.last(), Some(Block::Table(_))) && paragraph.is_empty();
        if let Some(lines) = &mut code {
            if line.trim_start().starts_with("```") {
                blocks.push(Block::Code(lines.join("\n")));
                code = None;
            } else {
                lines.push(line);
            }
            continue;
        }
        let trimmed = line.trim_start();
        let indent = (line.len() - trimmed.len()) / 2;
        if trimmed.starts_with("```") {
            flush(&mut paragraph, &mut blocks);
            code = Some(Vec::new());
        } else if trimmed.is_empty() {
            flush(&mut paragraph, &mut blocks);
        } else if trimmed.starts_with('#') {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading(
                trimmed.trim_start_matches('#').trim().to_owned(),
            ));
        } else if matches!(trimmed, "---" | "***" | "___") {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Rule);
        } else if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Item(indent, "•".into(), rest.to_owned()));
        } else if let Some((number, rest)) = ordered_item(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Item(indent, format!("{number}."), rest.to_owned()));
        } else if trimmed.starts_with('|') {
            flush(&mut paragraph, &mut blocks);
            let cells = table_cells(trimmed);
            let rule = cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
            match blocks.last_mut() {
                Some(Block::Table(rows)) if continues_table => {
                    if !rule {
                        rows.push(cells);
                    }
                }
                _ if rule => {}
                _ => blocks.push(Block::Table(vec![cells])),
            }
        } else {
            paragraph.push(trimmed);
        }
    }
    if let Some(lines) = code {
        blocks.push(Block::Code(lines.join("\n")));
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

/// The cells of a table row `| a | b |`, trimmed.
fn table_cells(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|');
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').map(|c| c.trim().to_owned()).collect()
}

fn ordered_item(line: &str) -> Option<(&str, &str)> {
    let (number, rest) = line.split_once(". ")?;
    (!number.is_empty() && number.len() <= 3 && number.chars().all(|c| c.is_ascii_digit()))
        .then_some((number, rest))
}

/// An inline run of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Plain(String),
    Bold(String),
    Code(String),
    Node(NodeId),
}

/// Splits a line into spans: `code`, **bold**, and node ids (`term:…`, `sub:…`,
/// `dispatch:…`) for which `is_node` holds, whether bare or in backticks.
pub fn parse_spans(text: &str, is_node: &dyn Fn(&NodeId) -> bool) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let code = rest.find('`');
        let bold = rest.find("**");
        let next = match (code, bold) {
            (Some(c), Some(b)) => Some(c.min(b)),
            (c, b) => c.or(b),
        };
        let Some(at) = next else {
            push_plain(&mut spans, rest, is_node, false);
            break;
        };
        let is_code = code == Some(at);
        let marker = if is_code { "`" } else { "**" };
        let after = &rest[at + marker.len()..];
        let Some(end) = after.find(marker) else {
            push_plain(&mut spans, rest, is_node, false);
            break;
        };
        push_plain(&mut spans, &rest[..at], is_node, false);
        let inner = &after[..end];
        if is_code {
            let id = NodeId(inner.to_owned());
            if looks_like_node(inner) && is_node(&id) {
                spans.push(Span::Node(id));
            } else {
                spans.push(Span::Code(inner.to_owned()));
            }
        } else {
            push_plain(&mut spans, inner, is_node, true);
        }
        rest = &after[end + marker.len()..];
    }
    spans
}

fn looks_like_node(s: &str) -> bool {
    ["term:", "sub:", "dispatch:"]
        .iter()
        .any(|p| s.starts_with(p) && s.len() > p.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.'))
}

/// Pushes `text` as plain (or bold) spans, cutting out node ids.
fn push_plain(spans: &mut Vec<Span>, text: &str, is_node: &dyn Fn(&NodeId) -> bool, bold: bool) {
    let wrap = |s: &str| {
        if bold {
            Span::Bold(s.to_owned())
        } else {
            Span::Plain(s.to_owned())
        }
    };
    let mut start = 0;
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < text.len() {
        let at_boundary = i == 0 || !is_id_char(bytes[i - 1] as char);
        if at_boundary
            && ["term:", "sub:", "dispatch:"]
                .iter()
                .any(|p| text[i..].starts_with(p))
        {
            let len = text[i..]
                .find(|c: char| !is_id_char(c))
                .unwrap_or(text.len() - i);
            let candidate = text[i..i + len].trim_end_matches(['.', ':', '-']);
            let id = NodeId(candidate.to_owned());
            if looks_like_node(candidate) && is_node(&id) {
                if start < i {
                    spans.push(wrap(&text[start..i]));
                }
                spans.push(Span::Node(id));
                i += candidate.len();
                start = i;
                continue;
            }
        }
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
    if start < text.len() {
        spans.push(wrap(&text[start..]));
    }
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.')
}

/// Renders `text` as simple markdown; returns a node whose link was clicked.
pub fn markdown(ui: &mut Ui, text: &str, is_node: &dyn Fn(&NodeId) -> bool) -> Option<NodeId> {
    let mut clicked = None;
    for (i, block) in parse_blocks(text).into_iter().enumerate() {
        if i > 0 {
            ui.add_space(4.0);
        }
        match block {
            Block::Heading(text) => {
                ui.add_space(2.0);
                spans_ui(ui, &text, is_node, true, &mut clicked);
            }
            Block::Paragraph(text) => spans_ui(ui, &text, is_node, false, &mut clicked),
            Block::Item(indent, marker, text) => {
                ui.horizontal(|ui| {
                    ui.add_space(8.0 + indent as f32 * 14.0);
                    ui.label(marker);
                    ui.vertical(|ui| spans_ui(ui, &text, is_node, false, &mut clicked));
                });
            }
            Block::Code(code) => code_block(ui, &code),
            Block::Table(rows) => table(ui, &rows, is_node, &mut clicked),
            Block::Rule => {
                ui.separator();
            }
        }
    }
    clicked
}

/// A markdown table: header row bold, rows striped, scrolling sideways when too wide.
fn table(
    ui: &mut Ui,
    rows: &[Vec<String>],
    is_node: &dyn Fn(&NodeId) -> bool,
    clicked: &mut Option<NodeId>,
) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let id = ui.id().with(("table", rows.len(), rows.first()));
    egui::Frame::new()
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(id.with("scroll"))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Grid::new(id)
                        .num_columns(columns)
                        .striped(true)
                        .spacing([14.0, 4.0])
                        .show(ui, |ui| {
                            for (r, row) in rows.iter().enumerate() {
                                for cell in row {
                                    ui.horizontal(|ui| {
                                        spans_line(ui, cell, is_node, r == 0, clicked);
                                    });
                                }
                                ui.end_row();
                            }
                        });
                });
        });
}

fn spans_ui(
    ui: &mut Ui,
    text: &str,
    is_node: &dyn Fn(&NodeId) -> bool,
    heading: bool,
    clicked: &mut Option<NodeId>,
) {
    ui.horizontal_wrapped(|ui| spans_line(ui, text, is_node, heading, clicked));
}

/// The spans of `text` laid out in `ui`'s current row.
fn spans_line(
    ui: &mut Ui,
    text: &str,
    is_node: &dyn Fn(&NodeId) -> bool,
    heading: bool,
    clicked: &mut Option<NodeId>,
) {
    ui.spacing_mut().item_spacing.x = 0.0;
    for span in parse_spans(text, is_node) {
        match span {
            Span::Plain(s) => {
                let text = RichText::new(s);
                ui.label(if heading { text.strong() } else { text });
            }
            Span::Bold(s) => {
                ui.label(RichText::new(s).strong());
            }
            Span::Code(s) => {
                ui.label(RichText::new(s).code());
            }
            Span::Node(id) => {
                if ui
                    .link(RichText::new(id.as_str()).monospace())
                    .on_hover_text("Select this node")
                    .clicked()
                {
                    *clicked = Some(id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::chat::protocol::{PermissionRequest, parse_line};

    fn any_node(_: &NodeId) -> bool {
        true
    }

    #[test]
    fn blocks_cover_headings_lists_code_and_paragraphs() {
        let text = "# Status\nAll good,\nmostly.\n\n- one\n  - nested\n2. two\n```\nlet x = 1;\n```\n| a | b |\n|---|:-:|\n| 1 | 2 |\n---";
        assert_eq!(
            parse_blocks(text),
            vec![
                Block::Heading("Status".into()),
                Block::Paragraph("All good, mostly.".into()),
                Block::Item(0, "•".into(), "one".into()),
                Block::Item(1, "•".into(), "nested".into()),
                Block::Item(0, "2.".into(), "two".into()),
                Block::Code("let x = 1;".into()),
                Block::Table(vec![
                    vec!["a".into(), "b".into()],
                    vec!["1".into(), "2".into()]
                ]),
                Block::Rule,
            ]
        );
        assert_eq!(
            parse_blocks("```\nunclosed"),
            vec![Block::Code("unclosed".into())]
        );
    }

    #[test]
    fn spans_find_code_bold_and_nodes() {
        let spans = parse_spans(
            "**2** stalled: `term:term_ab-1`, sub:s1:a2. and `x`",
            &any_node,
        );
        assert_eq!(
            spans,
            vec![
                Span::Bold("2".into()),
                Span::Plain(" stalled: ".into()),
                Span::Node(NodeId("term:term_ab-1".into())),
                Span::Plain(", ".into()),
                Span::Node(NodeId("sub:s1:a2".into())),
                Span::Plain(". and ".into()),
                Span::Code("x".into()),
            ]
        );
        // Unknown nodes stay text; unmatched markers are literal.
        let none = |_: &NodeId| false;
        assert_eq!(
            parse_spans("`term:term_1` term:term_2 **open", &none),
            vec![
                Span::Code("term:term_1".into()),
                Span::Plain(" term:term_2 **open".into()),
            ]
        );
        assert_eq!(
            parse_spans("determ:x", &any_node),
            vec![Span::Plain("determ:x".into())]
        );
    }

    fn scripted_transcript() -> Transcript {
        let mut t = Transcript::default();
        t.push_user("How many agents need input?");
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"s","model":"claude-test","tools":[]}"#,
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}},"parent_tool_use_id":null}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"mcp__myrstack__fleet_status"}},"parent_tool_use_id":null}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"mcp__myrstack__fleet_status","input":{}}]},"parent_tool_use_id":null}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"{\"needs_input\":[]}"}]},"parent_tool_use_id":null}"#,
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m2"}},"parent_tool_use_id":null}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},"parent_tool_use_id":null}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"**None** need input; `term:term_1` is stalled.\n\n- run A: 3/5"}},"parent_tool_use_id":null}"#,
        ];
        for line in lines {
            t.apply(&parse_line(line).unwrap());
        }
        t.push_permission(PermissionRequest {
            request_id: "p1".into(),
            tool_name: "mcp__myrstack__send_to_agent".into(),
            display_name: None,
            input: json!({"agent": "term:term_1", "text": "Queen: carry on"}),
            tool_use_id: None,
            description: None,
        });
        t
    }

    /// Renders the pane headlessly for a few frames with a scripted conversation and a
    /// permission card, and clicks nothing.
    #[test]
    fn pane_draws_headless() {
        let transcript = scripted_transcript();
        assert!(transcript.running);
        assert!(matches!(transcript.tool_calls().next(), Some(c) if tool_status_mark(c) == "✓"));
        let ctx = egui::Context::default();
        let mut state = ChatPanelState {
            focus_input: true,
            ..ChatPanelState::default()
        };
        for (session, auto_mode) in [
            (SessionState::Starting, false),
            (SessionState::Ready, false),
            (SessionState::Ready, true),
            (
                SessionState::Crashed {
                    reason: "exit 1".into(),
                    retry_in: std::time::Duration::from_secs(2),
                },
                true,
            ),
            (SessionState::Unavailable("no claude".into()), false),
        ] {
            for _ in 0..2 {
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let actions = chat_ui(
                        ui,
                        &mut state,
                        ChatView {
                            transcript: &transcript,
                            state: &session,
                            is_node: &|id| id.as_str() == "term:term_1",
                            auto_mode,
                        },
                    );
                    assert!(actions.is_empty(), "{actions:?}");
                });
                output.textures_delta.clear();
            }
        }
        assert!(!state.focus_input, "focus request consumed once ready");
    }

    /// A spoken message shows as Karl said it, with the voice marker.
    #[test]
    fn voice_messages_show_the_marker() {
        let mut transcript = Transcript::default();
        transcript.push_voice_user("How many agents need input?");
        let ctx = egui::Context::default();
        let mut state = ChatPanelState::default();
        let mut output = None;
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                chat_ui(
                    ui,
                    &mut state,
                    ChatView {
                        transcript: &transcript,
                        state: &SessionState::Ready,
                        is_node: &any_node,
                        auto_mode: false,
                    },
                );
            });
            out.textures_delta.clear();
            output = Some(out);
        }
        let output = output.unwrap();
        let text = voice_bubble_text("How many agents need input?");
        assert!(crate::ui::test_util::text_center(&output, &text).is_some());
    }

    /// Enter in the focused input box sends the draft; Shift+Enter does not.
    #[test]
    fn enter_sends_the_draft() {
        let transcript = Transcript::default();
        let ctx = egui::Context::default();
        let mut state = ChatPanelState {
            draft: "status?".into(),
            focus_input: true,
            ..ChatPanelState::default()
        };
        let frame = |state: &mut ChatPanelState, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut actions = Vec::new();
            let mut output = ctx.run_ui(input, |ui| {
                actions = chat_ui(
                    ui,
                    state,
                    ChatView {
                        transcript: &transcript,
                        state: &SessionState::Ready,
                        is_node: &any_node,
                        auto_mode: false,
                    },
                );
            });
            output.textures_delta.clear();
            actions
        };
        let enter = |shift: bool| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            },
        };
        assert!(frame(&mut state, vec![]).is_empty());
        assert!(frame(&mut state, vec![enter(true)]).is_empty());
        let actions = frame(&mut state, vec![enter(false)]);
        assert_eq!(actions.len(), 1, "{actions:?}");
        let ChatAction::Send(text) = &actions[0] else {
            panic!("{actions:?}");
        };
        assert!(text.starts_with("status?"));
        assert!(state.draft.is_empty());
    }

    /// Frames of the pane with `transcript`; returns the actions and all text drawn.
    fn draw(
        transcript: &Transcript,
        auto_mode: bool,
        events: Vec<egui::Event>,
    ) -> (Vec<ChatAction>, String) {
        let ctx = egui::Context::default();
        let mut state = ChatPanelState::default();
        let mut actions = Vec::new();
        let mut shapes = Vec::new();
        for events in [Vec::new(), events] {
            let input = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 900.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                actions = chat_ui(
                    ui,
                    &mut state,
                    ChatView {
                        transcript,
                        state: &SessionState::Ready,
                        is_node: &any_node,
                        auto_mode,
                    },
                );
            });
            output.textures_delta.clear();
            shapes = output.shapes;
        }
        let mut text = String::new();
        for shape in shapes {
            if let egui::epaint::Shape::Text(t) = shape.shape {
                text.push_str(t.galley.text());
                text.push('\n');
            }
        }
        (actions, text)
    }

    #[test]
    fn auto_mode_shows_badge_hint_notice_and_markers() {
        let mut t = Transcript::default();
        t.push_system(crate::chat::auto_mode::notice(true));
        t.apply(&parse_line(r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t9","name":"mcp__myrstack__send_to_agent","input":{}}]},"parent_tool_use_id":null}"#).unwrap());
        t.mark_auto(&PermissionRequest {
            request_id: "p9".into(),
            tool_name: "mcp__myrstack__send_to_agent".into(),
            display_name: None,
            input: json!({}),
            tool_use_id: Some("t9".into()),
            description: None,
        });
        let call = t.tool_calls().next().unwrap();
        assert!(call.auto);
        assert_eq!(tool_title(call), "🔧 send_to_agent … · auto");

        let (_, on) = draw(&t, true, Vec::new());
        assert!(on.contains("AUTO"), "{on}");
        assert!(on.contains(AUTO_HINT), "{on}");
        assert!(
            on.contains("⚙ Auto mode on: the Queen's actions run without asking"),
            "{on}"
        );
        assert!(on.contains("send_to_agent · auto"), "{on}");

        let (_, off) = draw(&Transcript::default(), false, Vec::new());
        assert!(off.contains("Auto"), "the switch is always there: {off}");
        assert!(!off.contains("AUTO") && !off.contains(AUTO_HINT), "{off}");
    }

    /// The say-line shows as a highlighted summary with the speaker icon, tags hidden, above
    /// the rest; replies without tags show as before.
    #[test]
    fn say_line_is_shown_apart_and_tags_are_hidden() {
        let reply = |text: &str, streaming: bool| ChatItem::Assistant {
            text: text.into(),
            streaming,
        };
        let mut t = Transcript::default();
        t.items.push(reply(
            "<say>Two agents need you. Want the list?</say>\n\n**Details**: `term:term_1` waits",
            false,
        ));
        t.items.push(reply("Plain reply, no tags.", false));
        t.items.push(reply("<say>Still stream", true));
        t.items.push(reply("Half a tag <sa", true));
        let (_, text) = draw(&t, false, Vec::new());
        assert!(!text.contains("<say") && !text.contains("say>"), "{text}");
        assert!(!text.contains("<sa"), "{text}");
        assert!(
            text.contains("Two agents need you. Want the list?"),
            "{text}"
        );
        assert!(text.contains(SAY_MARKER), "{text}");
        assert!(text.contains("Details"), "{text}");
        assert!(text.contains("Plain reply, no tags."), "{text}");
        assert!(text.contains("Still stream"), "{text}");
        let say_at = text.find("Two agents").unwrap();
        let details_at = text.find("Details").unwrap();
        assert!(say_at < details_at, "the say-line comes first: {text}");
        assert_eq!(text.matches(SAY_MARKER).count(), 2, "{text}");
    }

    #[test]
    fn the_speaker_icon_is_in_the_font() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(can_draw(ui, SAY_MARKER));
            assert!(can_draw(ui, VOICE_MARKER));
            assert!(can_draw(ui, "a"));
            assert!(!can_draw(ui, "\u{10FFFD}"), "a private-use char is missing");
        });
        output.textures_delta.clear();
    }
}
