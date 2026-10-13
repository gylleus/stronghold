//! Theme demo for the King chat pane (see UI-INVESTIGATION.md): the real `ui::chat` pane on
//! a scripted transcript, with a "Polished" toggle between stock egui and the app theme
//! (`ui::theme`). Typing in the input echoes a canned reply.
//!
//! `cargo run --example chat_theme_poc` (`… -- stock` starts with the stock theme)

use eframe::egui;
use myrstack::chat::protocol::{PermissionRequest, parse_line};
use myrstack::chat::{SessionState, Transcript};
use myrstack::model::NodeId;
use myrstack::ui::chat::{ChatAction, ChatPanelState, ChatView, chat_ui};
use myrstack::ui::theme;

fn main() -> eframe::Result {
    let stock = std::env::args().any(|a| a == "stock");
    eframe::run_native(
        "chat theme PoC",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([560.0, 760.0])
                .with_position([40.0, 40.0]),
            // The demo always opens at its spec'd size, not a remembered one.
            persist_window: false,
            ..Default::default()
        },
        Box::new(move |_| {
            Ok(Box::new(Demo {
                polished: !stock,
                ..Demo::default()
            }))
        }),
    )
}

struct Demo {
    transcript: Transcript,
    state: ChatPanelState,
    auto_mode: bool,
    polished: bool,
    /// The theme applied last frame; restyle only when the toggle flips.
    applied: Option<bool>,
}

impl Default for Demo {
    fn default() -> Self {
        let state = ChatPanelState {
            focus_input: true,
            ..Default::default()
        };
        Self {
            transcript: scripted_transcript(),
            state,
            auto_mode: false,
            polished: true,
            applied: None,
        }
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.applied != Some(self.polished) {
            apply_theme(ui.ctx(), self.polished);
            self.applied = Some(self.polished);
        }
        egui::Panel::top("poc_controls").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.polished, "Polished theme");
                ui.weak("same pane code, different egui Style + fonts");
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let actions = chat_ui(
                ui,
                &mut self.state,
                ChatView {
                    transcript: &self.transcript,
                    state: &SessionState::Ready,
                    is_node: &|id: &NodeId| id.as_str() == "term:term_1",
                    auto_mode: self.auto_mode,
                },
            );
            for action in actions {
                match action {
                    ChatAction::Send(text) => {
                        self.transcript.push_user(&text);
                        reply(
                            &mut self.transcript,
                            "Noted. **2 workers** are on it; `term:term_1` will report back.",
                        );
                    }
                    ChatAction::SetAutoMode(auto) => self.auto_mode = auto,
                    ChatAction::Decide {
                        request_id,
                        decision,
                    } => {
                        self.transcript.decide(&request_id, decision);
                    }
                    ChatAction::NewChat => self.transcript = scripted_transcript(),
                    ChatAction::Interrupt
                    | ChatAction::Compact
                    | ChatAction::Retry
                    | ChatAction::SelectNode(_) => {}
                }
            }
        });
    }
}

/// The real app theme (`ui::theme`), or stock egui with `polished = false`.
fn apply_theme(ctx: &egui::Context, polished: bool) {
    if polished {
        theme::install(ctx);
    } else {
        ctx.set_fonts(egui::FontDefinitions::default());
        ctx.set_style_of(egui::Theme::Dark, egui::Style::default());
        ctx.set_style_of(egui::Theme::Light, egui::Style::default());
    }
}

fn reply(t: &mut Transcript, text: &str) {
    let quoted = serde_json::to_string(text).expect("plain string");
    for line in [
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m9"}},"parent_tool_use_id":null}"#.to_owned(),
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},"parent_tool_use_id":null}"#.to_owned(),
        format!(
            r#"{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":{quoted}}}}},"parent_tool_use_id":null}}"#
        ),
        r#"{"type":"stream_event","event":{"type":"message_stop"},"parent_tool_use_id":null}"#.to_owned(),
        r#"{"type":"result","subtype":"success","is_error":false}"#.to_owned(),
    ] {
        if let Ok(message) = parse_line(&line) {
            t.apply(&message);
        }
    }
}

/// A conversation exercising every item kind: bubbles, say-line, markdown, tool row,
/// permission card, system notice.
fn scripted_transcript() -> Transcript {
    let mut t = Transcript::default();
    t.push_user("How is the clan doing? Anything stuck?");
    for line in [
        r#"{"type":"system","subtype":"init","session_id":"s","model":"claude-test","tools":[]}"#,
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}},"parent_tool_use_id":null}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"mcp__myrstack__fleet_status"}},"parent_tool_use_id":null}"#,
        r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"mcp__myrstack__fleet_status","input":{}}]},"parent_tool_use_id":null}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"{\"working\":9,\"stalled\":1}"}]},"parent_tool_use_id":null}"#,
    ] {
        t.apply(&parse_line(line).expect("scripted line"));
    }
    reply(
        &mut t,
        "<say>Nine working, one stalled. Want me to whip it?</say>\n\n\
         # Clan status\n\
         **9 working**, 1 stalled: `term:term_1` sits at a permission prompt.\n\n\
         - run alpha: 5/6 tasks done\n\
         - run beta: 4 workers active\n\n\
         ```\nwhip term:term_1  # would nudge it now\n```",
    );
    t.push_voice_user("Yes, whip it and tell beta to hurry up.");
    t.push_permission(PermissionRequest {
        request_id: "p1".into(),
        tool_name: "mcp__myrstack__send_to_agent".into(),
        display_name: None,
        input: serde_json::json!({
            "agent": "term:term_1",
            "text": "King: you have been idle 12 minutes; carry on."
        }),
        tool_use_id: None,
        description: None,
    });
    // End the turn so the input box is usable; the card stays pending to show its look.
    t.apply(
        &parse_line(r#"{"type":"result","subtype":"success","is_error":false}"#)
            .expect("result line"),
    );
    t
}
