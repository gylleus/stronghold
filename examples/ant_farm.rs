//! The ant farm on a synthetic fleet of 6 runs, loose terminals and subagents (about 30
//! agents), one of each state, to check how it looks.
//!
//! `cargo run --example ant_farm [--light] [--shot out.png]`: every few seconds one worker
//! flips between working and waiting on Karl, so ants walk. With `--shot`, it saves a
//! screenshot once settled and another mid-walk (`-walk`), then exits.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui;
use myrstack::model::{
    Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary, TaskCounts,
};
use myrstack::ui::ant_farm::AntFarmView;
use myrstack::ui::graph::GraphView;

const WINDOW: [f32; 2] = [1280.0, 800.0];
const FLIP_EVERY: Duration = Duration::from_secs(3);

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1).cloned());
    let light = args.iter().any(|a| a == "--light");
    eframe::run_native(
        "ant farm",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            ..Default::default()
        },
        Box::new(move |cc| {
            myrstack::ui::theme::install(&cc.egui_ctx);
            cc.egui_ctx.set_theme(if light {
                egui::Theme::Light
            } else {
                egui::Theme::Dark
            });
            let snapshot = fleet();
            let mut graph = GraphView::new();
            graph.sync(&snapshot);
            graph.set_pr_ready(HashSet::from([
                id("run0-w1"),
                id("run2-w2"),
                id("run4-w0"),
                id("run5-w5"),
            ]));
            graph.set_selected(Some(&id("run2-coord")));
            Ok(Box::new(Demo {
                snapshot,
                graph,
                farm: AntFarmView::new(),
                shot,
                started: Instant::now(),
                flipped_at: Instant::now(),
                stage: 0,
            }))
        }),
    )
}

struct Demo {
    snapshot: FleetSnapshot,
    graph: GraphView,
    farm: AntFarmView,
    shot: Option<String>,
    started: Instant,
    flipped_at: Instant,
    /// 0 settling, 1 waiting for the settled shot, 2 walking, 3 waiting for the walk shot.
    stage: u8,
}

impl Demo {
    fn flip(&mut self) {
        let node = self
            .snapshot
            .nodes
            .iter_mut()
            .find(|n| n.id == id("run0-w0"))
            .expect("flipping worker");
        node.state = match node.state {
            NodeState::Working => NodeState::NeedsInput,
            _ => NodeState::Working,
        };
        self.graph.sync(&self.snapshot);
        self.flipped_at = Instant::now();
    }

    fn save_shot(&self, image: &egui::ColorImage, suffix: &str) {
        let Some(path) = &self.shot else { return };
        let path = if suffix.is_empty() {
            path.clone()
        } else {
            format!("{}-{suffix}.png", path.trim_end_matches(".png"))
        };
        let [w, h] = image.size;
        let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
        image::save_buffer(&path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
            .expect("save screenshot");
        println!("saved {path}");
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let response = self.farm.show(ui, &self.snapshot, &self.graph);
                if let Some(id) = &response.clicked {
                    self.graph.click_node(id, false);
                }
            });
        if self.shot.is_none() {
            if self.flipped_at.elapsed() >= FLIP_EVERY {
                self.flip();
            }
            ui.ctx().request_repaint_after(Duration::from_millis(200));
            return;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(30));
        let image = ui.ctx().input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let shoot = |ctx: &egui::Context| {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()))
        };
        match self.stage {
            0 if self.started.elapsed() >= Duration::from_secs(1) => {
                shoot(ui.ctx());
                self.stage = 1;
            }
            1 => {
                if let Some(image) = image {
                    self.save_shot(&image, "");
                    self.flip();
                    self.stage = 2;
                }
            }
            2 if self.flipped_at.elapsed() >= Duration::from_millis(700) => {
                shoot(ui.ctx());
                self.stage = 3;
            }
            3 => {
                if let Some(image) = image {
                    self.save_shot(&image, "walk");
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            _ => {}
        }
    }
}

/// Task display names, as coordinators give them.
const TASKS: [&str; 12] = [
    "Layout tests",
    "Sprite layer",
    "Repaint budget",
    "Cache warmup",
    "Fix flaky poller test",
    "Rerank eval harness",
    "Migrate invoices table",
    "Docs: install guide",
    "Retry checkout calls",
    "Index rebuild",
    "Remove dead flags",
    "Bump egui to 0.36",
];

fn id(s: &str) -> NodeId {
    NodeId(s.to_string())
}

fn node(name: &str, kind: NodeKind, run: Option<&str>, state: NodeState, repo: &str) -> Node {
    Node {
        id: id(name),
        kind,
        label: name.to_string(),
        terminal_handle: None,
        live: true,
        run_id: run.map(str::to_string),
        task_id: None,
        dispatch_id: None,
        agent: None,
        worktree_path: Some(format!("/x/orca/workspaces/{repo}/{name}")),
        branch: None,
        last_output_ms: None,
        preview: Some(format!("{name} preview line")),
        state,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
    }
}

/// The Queen, 6 runs of a coordinator and 3–5 workers, loose terminals in two repos, and
/// subagents under a worker and a terminal.
fn fleet() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    s.nodes
        .push(node("queen", NodeKind::Queen, None, NodeState::Working, ""));
    let objectives = [
        "Ant farm view",
        "Checkout latency",
        "Search reranker eval",
        "Billing migration",
        "Docs cleanup",
        "Flaky tests",
    ];
    let states = [
        NodeState::Working,
        NodeState::Working,
        NodeState::Idle,
        NodeState::Stalled,
        NodeState::Done,
        NodeState::Failed,
    ];
    for (r, objective) in objectives.iter().enumerate() {
        let run = format!("run{r}");
        let total = 6;
        s.runs.push(RunSummary {
            id: run.clone(),
            objective: Some((*objective).into()),
            coordinator: Some(id(&format!("{run}-coord"))),
            task_counts: Some(TaskCounts {
                completed: r,
                pending: total - r,
                ..TaskCounts::default()
            }),
        });
        let coord_state = if r == 4 {
            NodeState::NeedsInput
        } else {
            NodeState::Working
        };
        let coord = format!("{run}-coord");
        s.nodes.push(node(
            &coord,
            NodeKind::Coordinator,
            Some(&run),
            coord_state,
            "myrstack",
        ));
        for w in 0..(3 + r % 3) {
            let name = format!("{run}-w{w}");
            let state = states[(w + r) % states.len()];
            let mut worker = node(&name, NodeKind::Worker, Some(&run), state, "myrstack");
            worker.label = TASKS[(r * 5 + w) % TASKS.len()].into();
            s.nodes.push(worker);
            s.edges.push(Edge {
                from: id(&coord),
                to: id(&name),
                kind: EdgeKind::CoordinatorWorker,
            });
        }
    }
    s.nodes
        .iter_mut()
        .find(|n| n.id == id("run0-w0"))
        .unwrap()
        .state = NodeState::Working;
    for (name, repo, state) in [
        ("shop-1", "shop", NodeState::Working),
        ("shop-2", "shop", NodeState::NeedsInput),
        ("recs-1", "recs", NodeState::Idle),
    ] {
        s.nodes
            .push(node(name, NodeKind::Terminal, None, state, repo));
    }
    for (sub, parent) in [
        ("sub-a", "run1-w1"),
        ("sub-b", "run1-w1"),
        ("sub-c", "shop-1"),
    ] {
        s.nodes
            .push(node(sub, NodeKind::Subagent, None, NodeState::Working, ""));
        s.edges.push(Edge {
            from: id(parent),
            to: id(sub),
            kind: EdgeKind::Subagent,
        });
    }
    s
}
