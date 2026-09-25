//! The queen's panel: the Queen's chat, under a fleet summary (state counts, runs, nodes
//! needing input or stalled, whip status, plan quotas) and the broadcast composer.

use std::collections::BTreeMap;

use eframe::egui::{self, RichText, Ui};

use crate::broadcast::BroadcastSender;
use crate::model::{FleetSnapshot, NodeId, NodeKind, NodeState};
use crate::playbook::fetches::PlaybookStatus;
use crate::quota::Quotas;
use crate::ui::broadcast::BroadcastComposer;
use crate::ui::graph::{ALL_STATES, hexagon, kind_label, queen_color, state_color, state_label};
use crate::ui::sidebar::run_rows;
use crate::whip::WhipStatus;

/// One node in a summary list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRow {
    pub id: NodeId,
    pub label: String,
    pub kind: NodeKind,
}

/// One run in the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLine {
    pub run_id: String,
    pub title: String,
    pub coordinator: Option<NodeId>,
    /// `None` when the coordinator is not live.
    pub coordinator_state: Option<NodeState>,
}

/// What the queen's panel shows of a snapshot. The queen itself is not counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetSummary {
    pub states: BTreeMap<NodeState, usize>,
    pub coordinators: usize,
    pub agents: usize,
    /// Runs, most recently active first.
    pub runs: Vec<RunLine>,
    pub needs_input: Vec<SummaryRow>,
    pub stalled: Vec<SummaryRow>,
}

impl FleetSummary {
    pub fn new(snapshot: &FleetSnapshot) -> Self {
        let mut summary = Self::default();
        for node in snapshot.agent_nodes() {
            *summary.states.entry(node.state).or_insert(0) += 1;
            summary.agents += 1;
            summary.coordinators += usize::from(node.kind == NodeKind::Coordinator);
            let row = || SummaryRow {
                id: node.id.clone(),
                label: node.label.clone(),
                kind: node.kind,
            };
            match node.state {
                NodeState::NeedsInput => summary.needs_input.push(row()),
                NodeState::Stalled => summary.stalled.push(row()),
                _ => {}
            }
        }
        summary.runs = run_rows(snapshot)
            .into_iter()
            .map(|row| RunLine {
                coordinator: snapshot.run(&row.id).and_then(|r| r.coordinator.clone()),
                title: row.objective.clone().unwrap_or_else(|| row.id.clone()),
                run_id: row.id,
                coordinator_state: row.coordinator_state,
            })
            .collect();
        summary
    }
}

/// The queen panel's state: the broadcast draft lives here across selections.
#[derive(Default)]
pub struct QueenPanel {
    pub broadcast: BroadcastComposer,
}

/// What the panel reads each frame.
pub struct QueenInputs<'a> {
    /// `None` until the first poll.
    pub snapshot: Option<&'a FleetSnapshot>,
    pub whip: &'a WhipStatus,
    pub quotas: &'a Quotas,
    pub now_ms: i64,
    /// Who runs which playbook version; `None` hides it.
    pub playbook: Option<&'a PlaybookStatus>,
}

/// What the user did in the panel this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueenResponse {
    /// A node to select (a run's coordinator, or a row of the needs-input/stalled lists).
    pub select: Option<NodeId>,
    /// The close button was clicked.
    pub close: bool,
}

/// Draws the queen panel: the Queen's header with the fleet counts, the fleet summary and
/// the broadcast composer as collapsible sections, and `chat` (the Queen's chat) filling the
/// rest. `make_sender` is called once, when the user confirms a broadcast.
pub fn queen_panel(
    ui: &mut Ui,
    panel: &mut QueenPanel,
    inputs: QueenInputs<'_>,
    make_sender: impl FnOnce() -> Box<dyn BroadcastSender>,
    chat: impl FnOnce(&mut Ui),
) -> QueenResponse {
    let mut response = QueenResponse::default();
    let summary = inputs.snapshot.map(FleetSummary::new).unwrap_or_default();
    let dark = ui.visuals().dark_mode;
    let fleet = inputs
        .snapshot
        .and_then(FleetSnapshot::queen)
        .map_or(NodeState::Idle, |q| q.state);

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
                queen_badge(ui, fleet, dark);
                ui.label(RichText::new("Queen").strong());
                ui.label(RichText::new(state_label(fleet)).color(state_color(fleet, dark)))
                    .on_hover_text("The most urgent state in the fleet");
            });
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.weak(format!(
            "{} coordinator{} · {} agent{}",
            summary.coordinators,
            plural(summary.coordinators),
            summary.agents,
            plural(summary.agents)
        ));
        for state in ALL_STATES {
            let count = summary.states.get(&state).copied().unwrap_or(0);
            if count > 0 {
                ui.label(
                    RichText::new(format!("{} {count}", state_label(state)))
                        .color(state_color(state, dark)),
                );
            }
        }
    });
    if let Some(status) = inputs.playbook {
        crate::ui::playbook::playbook_line(ui, status, inputs.now_ms);
    }

    egui::CollapsingHeader::new(RichText::new("Fleet summary").strong())
        .id_salt("queen_summary")
        .default_open(false)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("queen_panel")
                .max_height(ui.ctx().content_rect().height() * 0.3)
                .show(ui, |ui| {
                    node_list(ui, "Needs input", &summary.needs_input, &mut response);
                    node_list(ui, "Stalled", &summary.stalled, &mut response);
                    runs_section(ui, &summary.runs, dark, &mut response);
                    ui.add_space(4.0);
                    ui.label(RichText::new("Whip").strong());
                    ui.weak(crate::ui::whip_log::status_line(inputs.whip, inputs.now_ms));
                    if !inputs.quotas.providers.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            crate::ui::quota::quota_chips(ui, inputs.quotas, inputs.now_ms);
                        });
                    }
                    if let Some(status) = inputs.playbook
                        && let Some(id) = crate::ui::playbook::stale_list(ui, status, inputs.now_ms)
                    {
                        response.select = Some(id);
                    }
                });
        });

    egui::CollapsingHeader::new(RichText::new("Broadcast to coordinators").strong())
        .id_salt("queen_broadcast")
        .default_open(false)
        .show(ui, |ui| {
            panel.broadcast.ui(ui, inputs.snapshot, make_sender);
        });
    ui.separator();
    chat(ui);
    response
}

/// The Queen's hexagon in her colour, with a dot in the fleet state's colour.
fn queen_badge(ui: &mut Ui, fleet: NodeState, dark: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    painter.add(egui::Shape::convex_polygon(
        hexagon(center, 9.0),
        queen_color(dark),
        egui::Stroke::NONE,
    ));
    painter.circle_filled(
        center + egui::vec2(6.5, -6.5),
        3.5,
        state_color(fleet, dark),
    );
}

/// A titled list of nodes whose rows select their node; nothing when empty.
fn node_list(ui: &mut Ui, title: &str, rows: &[SummaryRow], response: &mut QueenResponse) {
    if rows.is_empty() {
        return;
    }
    ui.label(RichText::new(format!("{title} ({})", rows.len())).strong());
    for row in rows {
        let text = format!("{}  ·  {}", row.label, kind_label(row.kind));
        if ui
            .selectable_label(false, text)
            .on_hover_text("Select this node")
            .clicked()
        {
            response.select = Some(row.id.clone());
        }
    }
    ui.add_space(4.0);
}

fn runs_section(ui: &mut Ui, runs: &[RunLine], dark: bool, response: &mut QueenResponse) {
    ui.label(RichText::new(format!("Runs ({})", runs.len())).strong());
    if runs.is_empty() {
        ui.weak("No active runs");
    }
    for run in runs {
        ui.horizontal(|ui| {
            match run.coordinator_state {
                Some(state) => {
                    ui.label(RichText::new(state_label(state)).color(state_color(state, dark)))
                }
                None => ui.weak("not live"),
            };
            let clicked = ui
                .add(
                    egui::Label::new(&run.title)
                        .truncate()
                        .sense(egui::Sense::click()),
                )
                .on_hover_text(format!(
                    "{}\n\nrun {} · click to select its coordinator",
                    run.title, run.run_id
                ))
                .clicked();
            if clicked && let Some(coordinator) = &run.coordinator {
                response.select = Some(coordinator.clone());
            }
        });
    }
    ui.add_space(4.0);
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::FleetData;
    use crate::orca::{OutgoingMessage, Run, Terminal};

    struct NeverSender;

    impl BroadcastSender for NeverSender {
        fn send(&mut self, _: &OutgoingMessage) -> Result<Option<String>, String> {
            panic!("the queen panel test never confirms a send");
        }
    }

    fn data() -> FleetData {
        FleetData {
            terminals: vec![
                Terminal {
                    handle: "c1".into(),
                    worktree_path: Some("/u/orca/workspaces/depict/a".into()),
                    ..Terminal::default()
                },
                Terminal {
                    handle: "loose".into(),
                    ..Terminal::default()
                },
            ],
            runs: vec![Run {
                id: "r1".into(),
                objective: Some("ship the queen".into()),
                coordinator_handle: Some("c1".into()),
                ..Run::default()
            }],
            ..FleetData::default()
        }
    }

    #[test]
    fn summary_counts_agents_not_the_queen() {
        let mut snapshot = FleetSnapshot::build(&data(), 0);
        snapshot.nodes[1].state = NodeState::NeedsInput;
        snapshot.nodes[0].state = NodeState::Stalled;
        let summary = FleetSummary::new(&snapshot);
        assert_eq!(summary.agents, 2);
        assert_eq!(summary.coordinators, 1);
        assert_eq!(summary.states.values().sum::<usize>(), 2);
        assert_eq!(summary.needs_input.len(), 1);
        assert_eq!(summary.needs_input[0].id, NodeId::terminal("loose"));
        assert_eq!(summary.stalled[0].id, NodeId::terminal("c1"));
        assert_eq!(summary.runs.len(), 1);
        assert_eq!(summary.runs[0].title, "ship the queen");
        assert_eq!(summary.runs[0].coordinator, Some(NodeId::terminal("c1")));
    }

    /// Draws the panel headlessly for a few frames with a sender that must never be used.
    #[test]
    fn panel_draws_headless_without_sending() {
        let mut snapshot = FleetSnapshot::build(&data(), 0);
        snapshot.nodes[1].state = NodeState::NeedsInput;
        let mut panel = QueenPanel::default();
        let whip = WhipStatus::default();
        let quotas = Quotas::default();
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let response = queen_panel(
                    ui,
                    &mut panel,
                    QueenInputs {
                        snapshot: Some(&snapshot),
                        whip: &whip,
                        quotas: &quotas,
                        now_ms: 0,
                        playbook: None,
                    },
                    || Box::new(NeverSender),
                    |ui| {
                        ui.label("chat");
                    },
                );
                assert_eq!(response, QueenResponse::default());
            });
            output.textures_delta.clear();
        }
    }
}
