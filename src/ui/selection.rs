//! Multi-selection: the "N selected" side panel and what keys do to a selection of several
//! nodes.

use std::collections::BTreeSet;

use eframe::egui::{self, Color32, RichText, Ui};

use crate::model::{Node, NodeId};
use crate::ui::clan_view::{kind_label, state_color, state_label};

/// What was clicked in the selection panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionAction {
    /// Make this node the only selected one.
    SelectOnly(NodeId),
    /// Open the close dialog for the whole selection.
    Close,
    /// Nudge the whole selection now.
    Whip,
    Clear,
}

/// The nodes a close request covers: the whole selection when several nodes are selected
/// (the planner skips the King and subagents), else the primary unless it is the King.
pub fn close_targets(primary: Option<&NodeId>, selection: &BTreeSet<NodeId>) -> Vec<NodeId> {
    if selection.len() > 1 {
        return selection.iter().cloned().collect();
    }
    primary
        .filter(|id| **id != NodeId::king())
        .cloned()
        .into_iter()
        .collect()
}

/// The nodes a manual whip covers: the whole selection when several nodes are selected, else
/// the primary (the planner skips the King and subagents with a reason).
pub fn whip_targets(primary: Option<&NodeId>, selection: &BTreeSet<NodeId>) -> Vec<NodeId> {
    if selection.len() > 1 {
        return selection.iter().cloned().collect();
    }
    primary.cloned().into_iter().collect()
}

/// What Esc leaves selected, when it clears a selection: a multi-selection collapses to its
/// primary; a single selection is cleared. `None` when there is no selection.
pub fn after_escape(
    primary: Option<&NodeId>,
    selection: &BTreeSet<NodeId>,
) -> Option<Option<NodeId>> {
    match selection.len() {
        0 if primary.is_none() => None,
        0 | 1 => Some(None),
        _ => Some(primary.cloned().or_else(|| selection.first().cloned())),
    }
}

/// The "N selected" panel: the selected nodes (click one to select only it), a close button
/// for all of them and "Clear selection". `nodes` are the selected nodes still in the clan.
pub fn selection_panel(ui: &mut Ui, nodes: &[&Node]) -> Option<SelectionAction> {
    let mut action = None;
    ui.heading(format!("{} selected", nodes.len()));
    ui.weak("Shift/⌘-click or drag a box to change the selection.");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let red = Color32::from_rgb(190, 40, 40);
        let text = RichText::new(format!("Close {} sessions…", nodes.len()))
            .strong()
            .color(Color32::WHITE);
        if ui
            .add(egui::Button::new(text).fill(red))
            .on_hover_text("Kill the selected sessions in Orca (asks first)")
            .clicked()
        {
            action = Some(SelectionAction::Close);
        }
        if ui
            .button(format!("Whip {}", nodes.len()))
            .on_hover_text(
                "Send each selected agent a Stronghold nudge now (the King and subagents are \
                 skipped) — w",
            )
            .clicked()
        {
            action = Some(SelectionAction::Whip);
        }
        if ui.button("Clear selection").clicked() {
            action = Some(SelectionAction::Clear);
        }
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("selection_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for node in nodes {
                ui.horizontal(|ui| {
                    let color = state_color(node.state, ui.visuals().dark_mode);
                    ui.label(RichText::new("●").color(color));
                    let label = ui
                        .add(egui::Button::selectable(false, &node.label).truncate())
                        .on_hover_text("Select only this node");
                    if label.clicked() {
                        action = Some(SelectionAction::SelectOnly(node.id.clone()));
                    }
                    ui.weak(format!(
                        "{} · {}",
                        kind_label(node.kind),
                        state_label(node.state)
                    ));
                });
            }
        });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, NodeState};

    fn id(s: &str) -> NodeId {
        NodeId(s.to_string())
    }

    fn set(ids: &[&str]) -> BTreeSet<NodeId> {
        ids.iter().map(|s| id(s)).collect()
    }

    fn node(name: &str, kind: NodeKind) -> Node {
        Node {
            id: id(name),
            kind,
            label: name.to_string(),
            terminal_handle: None,
            live: true,
            run_id: None,
            task_id: None,
            dispatch_id: None,
            agent: None,
            worktree_path: None,
            branch: None,
            last_output_ms: None,
            preview: None,
            state: NodeState::Working,
            worker_state: None,
            dispatch_status: None,
            worker_terminal_state: None,
            released_at_ms: None,
            parked_at_ms: None,
        }
    }

    #[test]
    fn close_targets_cover_the_whole_multi_selection_including_the_king() {
        let selection: BTreeSet<NodeId> = [id("a"), id("b"), NodeId::king()].into();
        let targets = close_targets(Some(&id("a")), &selection);
        assert_eq!(targets.len(), 3);
        assert!(targets.contains(&NodeId::king()));
    }

    #[test]
    fn close_targets_of_a_single_selection_is_the_primary_but_never_the_king() {
        assert_eq!(close_targets(Some(&id("a")), &set(&["a"])), vec![id("a")]);
        // A primary hidden by filter is not in the farm's selection.
        assert_eq!(close_targets(Some(&id("a")), &set(&[])), vec![id("a")]);
        let king = NodeId::king();
        assert!(close_targets(Some(&king), &[king.clone()].into()).is_empty());
        assert!(close_targets(None, &set(&[])).is_empty());
    }

    #[test]
    fn whip_targets_cover_the_multi_selection_or_the_primary() {
        let multi = set(&["a", "b"]);
        assert_eq!(whip_targets(Some(&id("a")), &multi), vec![id("a"), id("b")]);
        assert_eq!(whip_targets(Some(&id("a")), &set(&["a"])), vec![id("a")]);
        // The King reaches the planner, which explains why it is skipped.
        let king = NodeId::king();
        assert_eq!(whip_targets(Some(&king), &set(&[])), vec![king]);
        assert!(whip_targets(None, &set(&[])).is_empty());
    }

    #[test]
    fn escape_collapses_a_multi_selection_before_clearing() {
        let multi = set(&["a", "b", "c"]);
        assert_eq!(after_escape(Some(&id("b")), &multi), Some(Some(id("b"))));
        assert_eq!(after_escape(None, &multi), Some(Some(id("a"))));
        assert_eq!(after_escape(Some(&id("b")), &set(&["b"])), Some(None));
        assert_eq!(after_escape(Some(&id("b")), &set(&[])), Some(None));
        assert_eq!(after_escape(None, &set(&[])), None);
    }

    #[test]
    fn whip_button_whips_the_selection() {
        let nodes = [node("w1", NodeKind::Worker), node("t1", NodeKind::Terminal)];
        let refs: Vec<&Node> = nodes.iter().collect();
        let action = crate::ui::test_util::click_label("Whip 2", |ui| selection_panel(ui, &refs));
        assert_eq!(action, Some(SelectionAction::Whip));
    }

    #[test]
    fn panel_draws_headless() {
        let nodes = [
            node("w1", NodeKind::Worker),
            node("t1", NodeKind::Terminal),
            node("c1", NodeKind::Coordinator),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                assert_eq!(selection_panel(ui, &refs), None);
            });
            output.textures_delta.clear();
            assert!(!output.shapes.is_empty());
        }
    }
}
