//! The app's main panels on a synthetic fleet, laid out as in the app with the Queen selected:
//! top bar, sidebar, graph, legend, the Queen's details and her chat. For checking the look
//! (icons, artwork, both themes) without Orca or a Queen session.
//!
//! `cargo run --example visual_tour [--light] [--empty] [--graph] [--agent] [--shot out.png]`:
//! `--empty` shows a fleet with no agents, `--graph` the graph and legend alone, `--agent` a
//! worker selected instead of the Queen (its details and conversation); with `--shot`, it saves
//! a screenshot once the layout has settled and exits.

use std::time::{Duration, Instant};

use eframe::egui;
use myrstack::chat::{SessionState, Transcript};
use myrstack::model::{
    Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary,
};
use myrstack::orca::AgentIdentity;
use myrstack::quota::Quotas;
use myrstack::ui;
use myrstack::ui::graph::GraphView;
use myrstack::whip::WhipStatus;

// The default pane widths (see `myrstack::ui::dock`), as fixed columns.
const CONVERSATION_SHARE: f32 = 0.45;
const DETAILS_WIDTH: f32 = 270.0;

const SETTLE_TIME: Duration = Duration::from_secs(6);
/// How long before the screenshot the graph is fitted, so the fit is drawn by then.
const FIT_LEAD: Duration = Duration::from_secs(1);
const WINDOW: [f32; 2] = [1440.0, 860.0];

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1).cloned());
    let light = args.iter().any(|a| a == "--light");
    let empty = args.iter().any(|a| a == "--empty");
    let graph_only = args.iter().any(|a| a == "--graph");
    let agent =
        (args.iter().any(|a| a == "--agent") && !empty).then(|| NodeId::dispatch("run_a", "d1"));
    eframe::run_native(
        "visual tour",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            ..Default::default()
        },
        Box::new(move |cc| {
            ui::theme::install(&cc.egui_ctx);
            cc.egui_ctx.set_theme(if light {
                egui::Theme::Light
            } else {
                egui::Theme::Dark
            });
            let snapshot = if empty { empty_fleet() } else { fleet() };
            let mut graph = GraphView::new();
            graph.sync(&snapshot);
            graph.set_selected(Some(agent.as_ref().unwrap_or(&NodeId::queen())));
            if !empty {
                graph.set_pr_ready([NodeId::dispatch("run_a", "d2")].into());
            }
            Ok(Box::new(Tour {
                snapshot,
                graph,
                sidebar: Default::default(),
                filter: Default::default(),
                queen: Default::default(),
                chat: Default::default(),
                transcript: Transcript::default(),
                graph_only,
                agent,
                input: Default::default(),
                shot,
                since: Instant::now(),
                fitted: false,
                shooting: false,
            }))
        }),
    )
}

struct Tour {
    snapshot: FleetSnapshot,
    graph: GraphView,
    sidebar: ui::sidebar::SidebarCache,
    filter: ui::sidebar::FleetFilter,
    queen: ui::queen::QueenPanel,
    chat: ui::chat::ChatPanelState,
    transcript: Transcript,
    graph_only: bool,
    /// The selected agent, when not the Queen.
    agent: Option<NodeId>,
    input: ui::terminal::InputPanelState,
    shot: Option<String>,
    since: Instant,
    fitted: bool,
    shooting: bool,
}

impl Tour {
    /// The sidebar, the Queen's details and her chat, as the app shows them with the Queen
    /// selected.
    fn panels(&mut self, ui: &mut egui::Ui) {
        let window = ui.ctx().content_rect().width();
        if let Some(node) = self.agent.as_ref().and_then(|id| self.snapshot.node(id)) {
            egui::Panel::right("terminal")
                .resizable(true)
                .default_size(window * CONVERSATION_SHARE)
                .show(ui, |ui| {
                    let input = ui::terminal::PanelInput {
                        review_rows: &[],
                        state: &mut self.input,
                        terminal: None,
                        conversation: None,
                        is_node: &|_| false,
                    };
                    ui::terminal::terminal_panel(ui, node, None, input, 0);
                });
            egui::Panel::right("details")
                .resizable(true)
                .default_size(DETAILS_WIDTH)
                .show(ui, |ui| {
                    ui::widgets::section_label(ui, "Details");
                    ui::terminal::details_panel(ui, node, None, None, 0);
                });
            self.sidebar(ui);
            return;
        }
        egui::Panel::right("terminal")
            .resizable(true)
            .default_size(window * CONVERSATION_SHARE)
            .show(ui, |ui| {
                let view = ui::chat::ChatView {
                    transcript: &self.transcript,
                    state: &SessionState::Ready,
                    is_node: &|_| false,
                    auto_mode: false,
                };
                ui::chat::chat_ui(ui, &mut self.chat, view);
            });
        egui::Panel::right("details")
            .resizable(true)
            .default_size(DETAILS_WIDTH)
            .show(ui, |ui| {
                ui::widgets::section_label(ui, "Details");
                let inputs = ui::queen::QueenInputs {
                    snapshot: Some(&self.snapshot),
                    whip: &WhipStatus::default(),
                    quotas: &Quotas::default(),
                    now_ms: 0,
                    playbook: None,
                };
                ui::queen::queen_details(ui, &mut self.queen, inputs, || unreachable!());
            });
        self.sidebar(ui);
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                ui::sidebar::sidebar_cached(
                    ui,
                    &self.snapshot,
                    &mut self.sidebar,
                    &mut self.filter,
                    |_| {},
                )
            });
    }
}

impl eframe::App for Tour {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui::widgets::app_title(ui);
                    ui.separator();
                    let _ = ui.selectable_label(true, "Graph");
                    let _ = ui.selectable_label(false, "Table");
                    let _ = ui.button("Fit");
                    ui.add_space(40.0);
                    ui::queen::quick_actions(ui, None);
                    ui::queen::envoy_control(
                        ui,
                        myrstack::envoy::EnvoyStatus {
                            session: myrstack::envoy::SessionState::Running,
                            bridge: myrstack::envoy::BridgeStatus::Ok,
                            pending_drafts: 0,
                            dry_run: true,
                            stuck_events: 0,
                            poll_stale: false,
                        },
                    );
                });
            });
        if !self.graph_only {
            self.panels(ui);
        }
        egui::Panel::bottom("legend")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ui, ui::graph::legend);
        egui::CentralPanel::default().show(ui, |ui| {
            self.graph.show(ui);
        });
        ui.ctx().request_repaint_after(Duration::from_millis(50));

        let Some(path) = &self.shot else { return };
        if !self.fitted && self.since.elapsed() + FIT_LEAD >= SETTLE_TIME {
            self.graph.fit_to_screen();
            self.fitted = true;
        }
        if !self.shooting && self.since.elapsed() >= SETTLE_TIME {
            self.shooting = true;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            return;
        }
        let image = ui.ctx().input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            image::save_buffer(path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
                .expect("save screenshot");
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn empty_fleet() -> FleetSnapshot {
    let mut snapshot = FleetSnapshot::default();
    snapshot.add_queen();
    snapshot
}

/// Two runs (one with an attendant) and loose terminals with subagents, covering every kind
/// and state.
fn fleet() -> FleetSnapshot {
    let mut snapshot = FleetSnapshot::default();
    for (run, repo, states) in [
        (
            "run_a",
            "myrstack",
            [NodeState::Working, NodeState::NeedsInput, NodeState::Done],
        ),
        (
            "run_b",
            "foundation",
            [NodeState::Stalled, NodeState::Failed, NodeState::Working],
        ),
    ] {
        let coordinator = NodeId::terminal(&format!("{run}_coord"));
        snapshot.runs.push(RunSummary {
            id: run.into(),
            objective: Some(format!("Ship the {repo} milestone")),
            coordinator: Some(coordinator.clone()),
            task_counts: None,
        });
        let mut coord = node(
            coordinator.clone(),
            NodeKind::Coordinator,
            NodeState::Working,
        );
        coord.run_id = Some(run.into());
        coord.label = format!("{repo} coordinator");
        coord.worktree_path = Some(format!("/x/orca/workspaces/{repo}/coord"));
        snapshot.nodes.push(coord);
        for (i, state) in states.into_iter().enumerate() {
            let id = NodeId::dispatch(run, &format!("d{}", i + 1));
            let mut worker = node(id.clone(), NodeKind::Worker, state);
            worker.run_id = Some(run.into());
            worker.label = format!("{repo} task {}", i + 1);
            snapshot.nodes.push(worker);
            snapshot.edges.push(Edge {
                from: coordinator.clone(),
                to: id,
                kind: EdgeKind::CoordinatorWorker,
            });
        }
        if run == "run_a" {
            let id = NodeId::terminal(&format!("{run}_att"));
            let mut attendant = node(id.clone(), NodeKind::Attendant, NodeState::Idle);
            attendant.run_id = Some(run.into());
            attendant.label = "attendant".into();
            snapshot.nodes.push(attendant);
            snapshot.edges.push(Edge {
                from: coordinator.clone(),
                to: id,
                kind: EdgeKind::Attendant,
            });
        }
    }
    for (t, state) in [NodeState::Working, NodeState::Idle, NodeState::NeedsInput]
        .into_iter()
        .enumerate()
    {
        let id = NodeId::terminal(&format!("loose_{t}"));
        let mut terminal = node(id.clone(), NodeKind::Terminal, state);
        terminal.label = format!("graphics/wt-{t}");
        terminal.worktree_path = Some(format!("/x/orca/workspaces/graphics/wt-{t}"));
        if t == 1 {
            terminal.agent = Some(AgentIdentity::Codex);
        }
        snapshot.nodes.push(terminal);
        if t == 0 {
            for (s, sub_state) in [NodeState::Working, NodeState::Done]
                .into_iter()
                .enumerate()
            {
                let sub = NodeId::subagent("sess", &format!("a{s}"));
                let mut n = node(sub.clone(), NodeKind::Subagent, sub_state);
                n.label = ["Explore", "Plan"][s].into();
                snapshot.nodes.push(n);
                snapshot.edges.push(Edge {
                    from: id.clone(),
                    to: sub,
                    kind: EdgeKind::Subagent,
                });
            }
        }
    }
    let mut envoy = node(NodeId::terminal("envoy"), NodeKind::Envoy, NodeState::Idle);
    envoy.label = "Envoy · listening".into();
    snapshot.nodes.push(envoy);
    snapshot.add_queen();
    snapshot
}

fn node(id: NodeId, kind: NodeKind, state: NodeState) -> Node {
    Node {
        id,
        kind,
        label: String::new(),
        terminal_handle: Some("term".into()),
        live: true,
        run_id: None,
        task_id: None,
        dispatch_id: None,
        agent: Some(AgentIdentity::Claude),
        worktree_path: None,
        branch: None,
        last_output_ms: None,
        preview: None,
        state,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
        parked_at_ms: None,
    }
}
