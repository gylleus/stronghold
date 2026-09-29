//! Standalone graph view on a synthetic fleet that changes every 2 s like the real poller:
//! states rotate, workers come and go, and a run appears after a while.
//!
//! `cargo run --example graph_demo [runs] [workers per run] [tick seconds]` (default 12 × 8,
//! ticking every 2 s; a tick of 0 freezes the fleet, e.g. to check idle CPU use).

use std::time::{Duration, Instant};

use eframe::egui;
use myrstack::model::{
    Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary,
};
use myrstack::orca::AgentIdentity;
use myrstack::ui::graph::{GraphView, legend, state_label};

const DEFAULT_TICK_SECS: u64 = 2;
const STATES: [NodeState; 6] = [
    NodeState::Working,
    NodeState::Idle,
    NodeState::NeedsInput,
    NodeState::Stalled,
    NodeState::Done,
    NodeState::Failed,
];

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1).map(|a| a.parse::<usize>());
    let runs = args.next().and_then(Result::ok).unwrap_or(12);
    let workers = args.next().and_then(Result::ok).unwrap_or(8);
    let tick_secs = args
        .next()
        .and_then(Result::ok)
        .unwrap_or(DEFAULT_TICK_SECS as usize);
    let tick = (tick_secs > 0).then(|| Duration::from_secs(tick_secs as u64));

    eframe::run_native(
        "graph demo",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            myrstack::ui::theme::install(&cc.egui_ctx);
            Ok(Box::new(Demo::new(runs, workers, tick)))
        }),
    )
}

struct Demo {
    runs: usize,
    workers: usize,
    /// Time between fleet changes; `None` keeps the fleet still.
    tick_every: Option<Duration>,
    tick: u64,
    last_tick: Instant,
    started_ms: i64,
    view: GraphView,
    snapshot: FleetSnapshot,
    last_click: Option<NodeId>,
}

impl Demo {
    fn new(runs: usize, workers: usize, tick_every: Option<Duration>) -> Self {
        let started_ms = now_ms();
        let snapshot = fleet(runs, workers, 0, started_ms);
        let mut view = GraphView::new();
        view.sync(&snapshot);
        Self {
            runs,
            workers,
            tick_every,
            tick: 0,
            last_tick: Instant::now(),
            started_ms,
            view,
            snapshot,
            last_click: None,
        }
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(every) = self.tick_every {
            if self.last_tick.elapsed() >= every {
                self.last_tick = Instant::now();
                self.tick += 1;
                self.snapshot = fleet(self.runs, self.workers, self.tick, self.started_ms);
                self.view.sync(&self.snapshot);
            }
            // The graph stops repainting once settled; wake up for the next tick.
            ui.ctx()
                .request_repaint_after(every.saturating_sub(self.last_tick.elapsed()));
        }

        egui::Panel::top("controls").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Fit to screen").clicked() {
                    self.view.fit_to_screen();
                }
                ui.label(format!(
                    "{} nodes, {} edges, tick {}",
                    self.view.node_count(),
                    self.view.edge_count(),
                    self.tick
                ));
                let selected = self.view.selected();
                let state = selected
                    .as_ref()
                    .and_then(|id| self.snapshot.nodes.iter().find(|n| &n.id == id))
                    .map_or("", |n| state_label(n.state));
                ui.label(format!(
                    "selected: {} {state}  last click: {}",
                    selected.map_or("-".into(), |id| id.to_string()),
                    self.last_click
                        .as_ref()
                        .map_or("-".into(), |id| id.to_string()),
                ));
            });
        });
        egui::Panel::bottom("legend").show(ui, legend);
        egui::CentralPanel::default().show(ui, |ui| {
            let response = self.view.show(ui);
            if response.clicked.is_some() {
                self.last_click = response.clicked;
            }
        });
    }
}

/// The fleet at `tick`: `runs` runs with up to `workers` workers each, a few loose terminals,
/// one released worker per run, an extra run from tick 5 and a rotating missing worker.
fn fleet(runs: usize, workers: usize, tick: u64, started_ms: i64) -> FleetSnapshot {
    let now = started_ms + tick as i64 * DEFAULT_TICK_SECS as i64 * 1_000;
    let mut snapshot = FleetSnapshot::default();
    let run_count = if tick >= 5 { runs + 1 } else { runs };
    for r in 0..run_count {
        let run_id = format!("run_{r}");
        let coordinator = NodeId::terminal(&format!("coord_{r}"));
        snapshot.runs.push(RunSummary {
            id: run_id.clone(),
            objective: Some(format!("Objective {r}: build the thing")),
            coordinator: Some(coordinator.clone()),
            task_counts: None,
        });
        let mut coord = node(coordinator.clone(), NodeKind::Coordinator, now);
        coord.run_id = Some(run_id.clone());
        coord.label = format!("coordinator {r}");
        coord.state = NodeState::Working;
        snapshot.nodes.push(coord);

        let missing = (tick as usize + r) % (workers + 3);
        for w in 0..workers {
            if w == missing {
                continue;
            }
            let id = NodeId::dispatch(&run_id, &format!("ctx_{r}_{w}"));
            let mut worker = node(id.clone(), NodeKind::Worker, now);
            worker.run_id = Some(run_id.clone());
            worker.label = format!("task {r}.{w}");
            worker.state = STATES[(w + r + tick as usize / 3) % STATES.len()];
            worker.released_at_ms = (w == workers - 1).then_some(now - 60_000);
            if w % 3 == 1 {
                worker.agent = Some(AgentIdentity::Codex);
            }
            snapshot.nodes.push(worker);
            snapshot.edges.push(Edge {
                from: coordinator.clone(),
                to: id,
                kind: EdgeKind::CoordinatorWorker,
            });
        }
    }
    for t in 0..3 {
        let mut loose = node(
            NodeId::terminal(&format!("shell_{t}")),
            NodeKind::Terminal,
            now,
        );
        loose.label = format!("shell {t}");
        loose.state = NodeState::Idle;
        snapshot.nodes.push(loose);
    }
    snapshot
}

fn node(id: NodeId, kind: NodeKind, now: i64) -> Node {
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
        branch: Some("feature/demo".into()),
        last_output_ms: Some(now - 125_000),
        preview: Some("\n  Running cargo test…\nmore".into()),
        state: NodeState::Idle,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
