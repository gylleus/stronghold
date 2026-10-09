//! A selected Claude subagent: its metadata (details column) and its read-only conversation
//! (conversation column).

use eframe::egui::{self, RichText, Ui};

use crate::claude::{SubagentInfo, SubagentState};
use crate::model::{Node, NodeId};
use crate::ui::chat::TranscriptView;
use crate::ui::graph::{format_ago, state_color};

/// Header rows (label, value) describing `info` as of `now_ms`.
pub fn header_rows(info: &SubagentInfo, now_ms: i64) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Type", info.agent_type.clone())];
    if let Some(model) = &info.model {
        rows.push(("Model", model.clone()));
    }
    let state = match info.state {
        SubagentState::Running => "running",
        SubagentState::Done => "done",
        SubagentState::Failed => "failed",
        SubagentState::Stale => "stale",
    };
    let state = if info.background {
        format!("{state} (background)")
    } else {
        state.to_string()
    };
    rows.push(("State", state));
    rows.push(("Depth", info.spawn_depth.to_string()));
    rows.push(("Started", format_ago(now_ms, info.started_ms)));
    match info.finished_ms {
        Some(at) => rows.push(("Finished", format_ago(now_ms, at))),
        None => rows.push(("Last activity", format_ago(now_ms, info.last_activity_ms))),
    }
    rows
}

/// The details column for a subagent: its label, description and header rows.
pub fn subagent_details(ui: &mut Ui, node: &Node, info: &SubagentInfo, now_ms: i64) {
    ui.horizontal(|ui| {
        let color = state_color(node.state, ui.visuals().dark_mode);
        ui.label(RichText::new("●").color(color));
        ui.add(egui::Label::new(RichText::new(&node.label).strong()).wrap());
    });
    if !info.description.is_empty() && info.description != node.label {
        ui.label(&info.description);
    }
    egui::Grid::new(("subagent_header", &info.agent_id))
        .num_columns(2)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            for (label, value) in header_rows(info, now_ms) {
                ui.label(RichText::new(label).weak());
                ui.label(value);
                ui.end_row();
            }
        });
}

/// The conversation column for a subagent: a title and its whole conversation, read from its
/// transcript. Returns a node whose link was clicked.
pub fn subagent_transcript(
    ui: &mut Ui,
    node: &Node,
    info: &SubagentInfo,
    view: Option<&mut TranscriptView>,
    is_node: &dyn Fn(&NodeId) -> bool,
) -> Option<NodeId> {
    ui.horizontal(|ui| {
        let color = state_color(node.state, ui.visuals().dark_mode);
        ui.label(RichText::new("●").color(color));
        crate::ui::chat::model_label(
            ui,
            view.as_ref()
                .and_then(|view| view.transcript.model.as_deref())
                .or(info.model.as_deref()),
        );
        ui.add(egui::Label::new(RichText::new(&node.label).strong()).truncate());
    });
    ui.separator();
    let running = !info.state.is_finished();
    crate::ui::chat::transcript_conversation(
        ui,
        ("subagent_conversation", &info.agent_id),
        view,
        running,
        is_node,
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::claude::SubagentParent;

    fn info(state: SubagentState, finished_ms: Option<i64>) -> SubagentInfo {
        SubagentInfo {
            agent_id: "a1".into(),
            session_id: "s1".into(),
            agent_type: "Explore".into(),
            description: "Find callers".into(),
            model: Some("haiku".into()),
            spawn_depth: 2,
            tool_use_id: None,
            parent: SubagentParent::Session,
            background: true,
            started_ms: 0,
            last_activity_ms: 50_000,
            state,
            finished_ms,
            transcript_path: PathBuf::new(),
        }
    }

    #[test]
    fn header_shows_finish_or_last_activity() {
        let rows = header_rows(&info(SubagentState::Running, None), 120_000);
        assert_eq!(
            rows,
            vec![
                ("Type", "Explore".to_string()),
                ("Model", "haiku".to_string()),
                ("State", "running (background)".to_string()),
                ("Depth", "2".to_string()),
                ("Started", "2m ago".to_string()),
                ("Last activity", "1m ago".to_string()),
            ]
        );
        let mut done = info(SubagentState::Failed, Some(110_000));
        done.model = None;
        done.background = false;
        let rows = header_rows(&done, 120_000);
        assert_eq!(rows[0], ("Type", "Explore".to_string()));
        assert_eq!(rows[1], ("State", "failed".to_string()));
        assert_eq!(rows[4], ("Finished", "10s ago".to_string()));
    }
}
