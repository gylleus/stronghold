//! The Queen's chat pane: header with the session state and buttons, the message list
//! (user bubbles, markdown replies streaming in, collapsible tool rows, permission cards) and
//! the input box.

use eframe::egui::{self, Color32, RichText, Ui};
use serde_json::Value;

use crate::chat::permissions::{
    DESTRUCTIVE_TOOLS, MYRSTACK_PREFIX, short_tool_name, summarize_input, truncate,
};
use crate::chat::transcript::{ChatItem, Decision, PermissionCard, ToolCall, Transcript};
use crate::chat::{STATUS_REPORT_PROMPT, SessionState};
use crate::model::NodeId;

/// Longest tool input/result shown in an expanded row.
const MAX_DETAIL_CHARS: usize = 4000;

/// Draft and focus state of the pane, kept by the app across frames.
#[derive(Debug, Default)]
pub struct ChatPanelState {
    pub draft: String,
    /// Focus the input box on the next frame.
    pub focus_input: bool,
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
}

/// What the pane reads each frame.
pub struct ChatView<'a> {
    pub transcript: &'a Transcript,
    pub state: &'a SessionState,
    /// Whether a node id names a node in the current snapshot (only those become links).
    pub is_node: &'a dyn Fn(&NodeId) -> bool,
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
        .show(ui, |ui| input_box(ui, state, ready, running, &mut actions));

    egui::ScrollArea::vertical()
        .id_salt("queen_chat_messages")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if view.transcript.items.is_empty() {
                ui.add_space(8.0);
                ui.weak("Ask the Queen about the fleet, or press Status report.");
            }
            for (i, item) in view.transcript.items.iter().enumerate() {
                ui.push_id(i, |ui| item_ui(ui, item, &view, &mut actions));
                ui.add_space(6.0);
            }
            if running && !streaming_text(view.transcript) {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("thinking…");
                });
            }
        });
    actions
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
    let can_send = *view.state == SessionState::Ready && !running;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Chat").strong());
        let (text, color) = state_text(view.state, running, ui.visuals());
        ui.label(RichText::new(format!("● {text}")).color(color))
            .on_hover_text(match view.state {
                SessionState::Crashed { reason, .. } => reason.as_str(),
                _ => "The Queen's Claude Code session",
            });
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
            if running {
                if ui
                    .button("⏹ Stop")
                    .on_hover_text("Interrupt this turn")
                    .clicked()
                {
                    actions.push(ChatAction::Interrupt);
                }
            } else if ui
                .add_enabled(can_send, egui::Button::new("Status report"))
                .on_hover_text(format!("Send \"{STATUS_REPORT_PROMPT}\""))
                .clicked()
            {
                actions.push(ChatAction::Send(STATUS_REPORT_PROMPT.into()));
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
    let clicked = ui
        .horizontal(|ui| {
            let send = ui
                .add_enabled(can_send, egui::Button::new("Send"))
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

fn item_ui(ui: &mut Ui, item: &ChatItem, view: &ChatView<'_>, actions: &mut Vec<ChatAction>) {
    match item {
        ChatItem::User(text) => user_bubble(ui, text),
        ChatItem::Assistant { text, .. } => {
            if let Some(id) = markdown(ui, text, view.is_node) {
                actions.push(ChatAction::SelectNode(id));
            }
        }
        ChatItem::Tool(call) => tool_row(ui, call),
        ChatItem::Permission(card) => permission_card(ui, card, actions),
        ChatItem::Notice(text) => {
            ui.weak(RichText::new(text).italics());
        }
        ChatItem::Error(text) => {
            ui.colored_label(ui.visuals().error_fg_color, text);
        }
    }
}

fn user_bubble(ui: &mut Ui, text: &str) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        egui::Frame::new()
            .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.35))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.85);
                ui.add(egui::Label::new(text).wrap());
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

fn tool_row(ui: &mut Ui, call: &ToolCall) {
    let title = format!(
        "🔧 {} {}",
        short_tool_name(&call.name),
        tool_status_mark(call)
    );
    let color = match &call.result {
        Some(r) if r.is_error => ui.visuals().error_fg_color,
        _ => ui.visuals().weak_text_color(),
    };
    egui::CollapsingHeader::new(RichText::new(title).color(color).small())
        .id_salt(("chat_tool", &call.id))
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
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).monospace().small()).wrap());
        });
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
    Rule,
}

/// Splits `text` into blocks: headings, paragraphs (soft line breaks joined), list items,
/// fenced code (tables kept as code) and rules.
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
            match blocks.last_mut() {
                Some(Block::Code(table)) if table.starts_with('|') => {
                    table.push('\n');
                    table.push_str(trimmed);
                }
                _ => blocks.push(Block::Code(trimmed.to_owned())),
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
    for block in parse_blocks(text) {
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
            Block::Rule => {
                ui.separator();
            }
        }
    }
    clicked
}

fn spans_ui(
    ui: &mut Ui,
    text: &str,
    is_node: &dyn Fn(&NodeId) -> bool,
    heading: bool,
    clicked: &mut Option<NodeId>,
) {
    ui.horizontal_wrapped(|ui| {
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
    });
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
        let text = "# Status\nAll good,\nmostly.\n\n- one\n  - nested\n2. two\n```\nlet x = 1;\n```\n| a | b |\n| 1 | 2 |\n---";
        assert_eq!(
            parse_blocks(text),
            vec![
                Block::Heading("Status".into()),
                Block::Paragraph("All good, mostly.".into()),
                Block::Item(0, "•".into(), "one".into()),
                Block::Item(1, "•".into(), "nested".into()),
                Block::Item(0, "2.".into(), "two".into()),
                Block::Code("let x = 1;".into()),
                Block::Code("| a | b |\n| 1 | 2 |".into()),
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
        for session in [
            SessionState::Starting,
            SessionState::Ready,
            SessionState::Crashed {
                reason: "exit 1".into(),
                retry_in: std::time::Duration::from_secs(2),
            },
            SessionState::Unavailable("no claude".into()),
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
                        },
                    );
                    assert!(actions.is_empty(), "{actions:?}");
                });
                output.textures_delta.clear();
            }
        }
        assert!(!state.focus_input, "focus request consumed once ready");
    }

    /// Enter in the focused input box sends the draft; Shift+Enter does not.
    #[test]
    fn enter_sends_the_draft() {
        let transcript = Transcript::default();
        let ctx = egui::Context::default();
        let mut state = ChatPanelState {
            draft: "status?".into(),
            focus_input: true,
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
}
