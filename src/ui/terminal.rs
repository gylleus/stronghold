//! Terminal panel: the selected node's info and live screen (input box follows in myr-och.2).

use eframe::egui;

use crate::model::{Node, NodeKind, NodeState};
use crate::orca::TerminalStatus;
use crate::poller::ScreenUpdate;
use crate::ui::graph::state_color;

/// Columns the screen font is sized to fit in the panel width.
const TARGET_COLUMNS: f32 = 110.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 14.0;

/// What the user did in the panel this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalPanelResponse {
    /// The close (×) button was clicked.
    pub close: bool,
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
    now_ms: i64,
) -> TerminalPanelResponse {
    let mut response = TerminalPanelResponse::default();
    let has_screen = watch_handle(node).is_some();
    // Only an update for this node's terminal counts.
    let screen = screen.filter(|s| Some(s.handle.as_str()) == watch_handle(node));

    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("×")
                .on_hover_text("Close (clears selection)")
                .clicked()
            {
                response.close = true;
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&node.label).strong()).truncate());
            });
        });
    });
    header(ui, node, screen, has_screen, now_ms);
    ui.separator();

    if !has_screen {
        ui.weak(match node.kind {
            NodeKind::Subagent => "Subagents have no terminal.",
            _ => "No live terminal.",
        });
        return response;
    }

    // Reserved for the input box (myr-och.2).
    let input_height = 0.0;
    let screen_height = (ui.available_height() - input_height).max(0.0);
    ui.allocate_ui(egui::vec2(ui.available_width(), screen_height), |ui| {
        screen_view(ui, screen);
    });
    // --- input area (myr-och.2) ---

    response
}

fn header(
    ui: &mut egui::Ui,
    node: &Node,
    screen: Option<&ScreenUpdate>,
    has_screen: bool,
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
