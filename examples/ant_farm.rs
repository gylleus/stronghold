//! The ant farm on a synthetic fleet of 6 runs, loose terminals and subagents (about 30
//! agents), one of each state, to check how it looks.
//!
//! `cargo run --example ant_farm [--light] [--graph] [--sweep] [--shot out.png]`.
//! `--receipt-at-ms 31` freezes a receipt mid-shake; `--reduced-motion` shows its flash.
//! `--bridge-down` and `--approval` show the Envoy receiver health. `--graph`
//! shows the same fleet as the graph; `--sweep` flies the worktree cleanup's broom around the
//! farm. Every few seconds one worker
//! flips between working and waiting on Karl, so ants walk; B broadcasts, S asks for a status
//! report, N hatches a new worker, M sends a burst of messages between ants and T toggles the
//! Queen thinking. With `--shot`, it saves a screenshot once settled and another while a worker
//! walks, a new one emerges, a broadcast ripples and messages fly (`-walk`), then exits.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui;
use myrstack::model::{
    Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary, TaskCounts,
};
use myrstack::traffic::{AgentMessage, MessageKind};
use myrstack::ui::ant_farm::{AntFarmView, Pulse};
use myrstack::ui::envoy_receipt::ReceiptCue;
use myrstack::ui::graph::GraphView;
use myrstack::ui::worktrees::WorktreePanel;
use myrstack::worktrees::SweepMarker;

const WINDOW: [f32; 2] = [1280.0, 800.0];
const FLIP_EVERY: Duration = Duration::from_secs(3);

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1).cloned());
    let light = args.iter().any(|a| a == "--light");
    let health = if args.iter().any(|a| a == "--bridge-down") {
        myrstack::envoy::EnvoyHealth::BridgeDown
    } else if args.iter().any(|a| a == "--approval") {
        myrstack::envoy::EnvoyHealth::AwaitingApproval
    } else {
        myrstack::envoy::EnvoyHealth::Listening
    };
    let receipt_at_ms = args.iter().position(|a| a == "--receipt-at-ms").map(|i| {
        args[i + 1]
            .parse::<i64>()
            .expect("receipt age in milliseconds")
    });
    let reduced_motion = args.iter().any(|a| a == "--reduced-motion");
    let show_graph = args.iter().any(|a| a == "--graph");
    let sweep = args.iter().any(|a| a == "--sweep").then(sweep_marker);
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
            let mut snapshot = fleet();
            snapshot
                .nodes
                .iter_mut()
                .find(|n| n.kind == NodeKind::Envoy)
                .unwrap()
                .label = format!("Envoy · {}", health.label());
            let mut graph = GraphView::new();
            graph.sync(&snapshot);
            graph.set_pr_ready(HashSet::from([
                id("run0-w1"),
                id("run2-w2"),
                id("run4-w0"),
                id("run5-w5"),
            ]));
            graph.set_selected(Some(&id("run2-coord")));
            let sweep = sweep.map(|data| (WorktreePanel::new(&cc.egui_ctx), data));
            Ok(Box::new(Demo {
                snapshot,
                graph,
                farm: {
                    // As in the app with the Queen's chat showing.
                    let mut farm = AntFarmView::new();
                    farm.set_queen_focused(true);
                    farm.set_envoy_health(health);
                    if health == myrstack::envoy::EnvoyHealth::AwaitingApproval {
                        farm.set_envoy_pending(2);
                    }
                    farm
                },
                shot,
                show_graph,
                receipt_at_ms,
                reduced_motion,
                started: Instant::now(),
                flipped_at: Instant::now(),
                stage: 0,
                thinking: false,
                sweep,
            }))
        }),
    )
}

struct Demo {
    snapshot: FleetSnapshot,
    graph: GraphView,
    farm: AntFarmView,
    shot: Option<String>,
    show_graph: bool,
    receipt_at_ms: Option<i64>,
    reduced_motion: bool,
    started: Instant,
    flipped_at: Instant,
    thinking: bool,
    /// 0 settling, 1 waiting for the settled shot, 2 walking, 3 waiting for the walk shot.
    stage: u8,
    /// The cleanup panel drawing the broom, and the temporary data dir holding its marker.
    sweep: Option<(WorktreePanel, tempfile::TempDir)>,
}

/// A running sweep's marker in a temporary data dir, as when the Queen runs `clean_worktrees`.
fn sweep_marker() -> tempfile::TempDir {
    let data = tempfile::tempdir().expect("temp dir");
    // SAFETY: set before any thread starts.
    unsafe { std::env::set_var("MYRSTACK_DATA_DIR", data.path()) };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let marker = SweepMarker {
        started_at_ms: now_ms,
        updated_at_ms: now_ms + 3_600_000,
        total: 4,
        done: 1,
        current: Some("graph-trackpad-pan".into()),
    };
    std::fs::write(
        myrstack::worktrees::default_marker_path(),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    data
}

impl Demo {
    /// A new worker in run 2, emerging from the Queen.
    fn hatch(&mut self) {
        let n = self.snapshot.nodes.len();
        let mut worker = node(
            &format!("new-{n}"),
            NodeKind::Worker,
            Some("run2"),
            NodeState::Working,
            "myrstack",
        );
        worker.label = "Fresh hatchling".into();
        self.snapshot.nodes.push(worker);
        self.graph.sync(&self.snapshot);
    }

    /// One message of each kind, plus a repeat that joins its envelope.
    fn mail(&mut self, now_s: f64) {
        for (from, to, kind) in [
            ("run0-w1", "run0-coord", MessageKind::MergeReady),
            ("run2-w2", "run2-coord", MessageKind::Ask),
            ("run2-w2", "run2-coord", MessageKind::Ask),
            ("queen", "run1-coord", MessageKind::Instruction),
            ("karl", "queen", MessageKind::Instruction),
            ("slack", "shop-1", MessageKind::Relay),
            ("slack", "envoy", MessageKind::Relay),
            ("envoy", "slack", MessageKind::Message),
            ("queen", "run3-w1", MessageKind::Nudge),
            ("shop-2", "karl", MessageKind::Status),
            ("run4-coord", "run5-coord", MessageKind::Message),
        ] {
            let message = AgentMessage::new(from, to, kind);
            let flight = message
                .resolve(&self.snapshot)
                .expect("both ends in the fleet");
            self.graph.message(flight.clone(), now_s, false);
            self.farm.message(flight, now_s, false);
        }
    }

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
        let receipt = self
            .receipt_at_ms
            .map_or_else(ReceiptCue::default, |elapsed| {
                ReceiptCue::at(Some(0), elapsed, self.reduced_motion)
            });
        self.graph.set_envoy_receipt(receipt);
        self.farm.set_envoy_receipt(receipt);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                if self.show_graph {
                    self.graph.show(ui);
                    return;
                }
                let farm = egui::Rect::from_min_size(ui.cursor().min, ui.available_size());
                let response = self.farm.show(ui, &self.snapshot, &self.graph);
                if let Some((panel, _)) = &mut self.sweep {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as i64);
                    panel.tick(None, now_ms);
                    panel.broom_in_farm(ui, farm);
                }
                if let Some(id) = &response.clicked {
                    self.graph.click_node(id, false);
                }
            });
        if self.shot.is_none() {
            if self.flipped_at.elapsed() >= FLIP_EVERY {
                self.flip();
            }
            let (b, st, n, t, m) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::B),
                    i.key_pressed(egui::Key::S),
                    i.key_pressed(egui::Key::N),
                    i.key_pressed(egui::Key::T),
                    i.key_pressed(egui::Key::M),
                )
            });
            if m {
                self.mail(ui.input(|i| i.time));
            }
            if t {
                self.thinking = !self.thinking;
                self.farm.set_queen_busy(self.thinking);
            }
            if b {
                self.farm.pulse(Pulse::Broadcast);
            }
            if st {
                self.farm.pulse(Pulse::StatusReport);
            }
            if n {
                self.hatch();
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
                    self.hatch();
                    self.farm.pulse(Pulse::Broadcast);
                    self.mail(ui.input(|i| i.time));
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
        parked_at_ms: None,
    }
}

/// The Queen, 6 runs of a coordinator and 3–5 workers, loose terminals in two repos, and
/// subagents under a worker and a terminal.
fn fleet() -> FleetSnapshot {
    let mut s = FleetSnapshot::default();
    let mut envoy = node("envoy", NodeKind::Envoy, None, NodeState::Idle, "myrstack");
    envoy.label = "Envoy · listening".into();
    s.nodes.push(envoy);
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
            if w == 0 {
                worker.label = format!("{} and tidy the follow-ups", worker.label);
            }
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
