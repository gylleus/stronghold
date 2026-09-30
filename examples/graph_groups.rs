//! Graph view on a synthetic fleet of `repos` repos with `per_repo` nodes each, to check how
//! the repo grouping looks at different group sizes.
//!
//! `cargo run --example graph_groups [repos] [per_repo] [loose|runs|mixed] [--shot out.png]
//! [--drag]` (default 6 × 4, mixed: even repos hold loose terminals, odd ones a run of a
//! coordinator and its workers). With `--shot`, it saves a screenshot once the layout has had
//! time to settle, prints each repo region's canvas size and exits. With `--drag` too, it then
//! drags repo-0's first terminal next to its second through synthetic pointer events, and
//! saves `-dropped` and `-later` screenshots, printing where the node was dropped and where it
//! is once the layout has settled again.

use std::time::{Duration, Instant};

use eframe::egui::{self, Pos2, Vec2};
use egui_graphs::MetadataFrame;
use myrstack::model::{
    Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary,
};
use myrstack::orca::AgentIdentity;
use myrstack::ui::graph::{DEFAULT_GRAPH_ID, GraphView};

/// Time the layout gets before the screenshot, and again after the drop.
const SETTLE_TIME: Duration = Duration::from_secs(8);
const WINDOW: [f32; 2] = [1280.0, 800.0];
/// Where the dragged node is dropped, relative to its sibling (canvas units).
const DROP_OFFSET: Vec2 = Vec2::new(80.0, 20.0);
/// Pointer moves a drag is split into.
const DRAG_STEPS: usize = 20;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Loose,
    Runs,
    Mixed,
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1).cloned());
    let drag = args.iter().any(|a| a == "--drag");
    let mut positional = args
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .map(String::as_str);
    let repos = positional.next().and_then(|a| a.parse().ok()).unwrap_or(6);
    let per_repo = positional.next().and_then(|a| a.parse().ok()).unwrap_or(4);
    let mode = match positional.next() {
        Some("loose") => Mode::Loose,
        Some("runs") => Mode::Runs,
        _ => Mode::Mixed,
    };

    eframe::run_native(
        "graph groups",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            ..Default::default()
        },
        Box::new(move |cc| {
            myrstack::ui::theme::install(&cc.egui_ctx);
            let snapshot = fleet(repos, per_repo, mode);
            let mut view = GraphView::new();
            view.sync(&snapshot);
            Ok(Box::new(Demo {
                view,
                shot,
                drag,
                phase: Phase::Settling,
                since: Instant::now(),
                widget_origin: Pos2::ZERO,
                pending: Vec::new(),
                dropped: None,
            }))
        }),
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Settling,
    /// Waiting for the screenshot with this file suffix.
    Shooting(&'static str),
    Dragging,
    Resettling,
    Done,
}

struct Demo {
    view: GraphView,
    shot: Option<String>,
    drag: bool,
    phase: Phase,
    since: Instant,
    /// Screen position of the graph widget's top-left corner.
    widget_origin: Pos2,
    /// Synthetic input events, one batch per frame.
    pending: Vec<Vec<egui::Event>>,
    /// Where the dragged node was dropped (canvas).
    dropped: Option<Pos2>,
}

fn dragged() -> NodeId {
    NodeId::terminal("repo-0_t0")
}

fn sibling() -> NodeId {
    NodeId::terminal("repo-0_t1")
}

impl Demo {
    fn shoot(&mut self, ctx: &egui::Context, suffix: &'static str) {
        self.phase = Phase::Shooting(suffix);
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    }

    fn save_shot(&self, image: &egui::ColorImage, suffix: &str) {
        let Some(path) = &self.shot else { return };
        let path = match (suffix, path.strip_suffix(".png")) {
            ("", _) => path.clone(),
            (_, Some(stem)) => format!("{stem}-{suffix}.png"),
            (_, None) => format!("{path}-{suffix}"),
        };
        let [w, h] = image.size;
        let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
        image::save_buffer(&path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
            .expect("save screenshot");
    }

    /// Queues a primary-button drag of `from` to `to` (screen points), one event per frame.
    fn queue_drag(&mut self, from: Pos2, to: Pos2) {
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.pending.push(vec![egui::Event::PointerMoved(from)]);
        self.pending.push(vec![button(from, true)]);
        for step in 1..=DRAG_STEPS {
            let t = step as f32 / DRAG_STEPS as f32;
            self.pending
                .push(vec![egui::Event::PointerMoved(from + (to - from) * t)]);
        }
        self.pending.push(vec![button(to, false)]);
        self.pending.push(Vec::new());
        self.pending.reverse();
    }
}

impl eframe::App for Demo {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(events) = self.pending.pop() {
            raw_input.events.extend(events);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            self.widget_origin = ui.available_rect_before_wrap().min;
            self.view.show(ui);
        });
        ui.ctx().request_repaint_after(Duration::from_millis(50));
        if self.shot.is_none() {
            return;
        }
        let image = ui.ctx().input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        match self.phase {
            Phase::Settling if self.since.elapsed() >= SETTLE_TIME => {
                self.view.fit_to_screen();
                self.shoot(ui.ctx(), "");
            }
            Phase::Shooting(suffix) => {
                let Some(image) = image else { return };
                self.save_shot(&image, suffix);
                match suffix {
                    "" => {
                        for region in self.view.repo_regions() {
                            let size = region.rect.size();
                            println!("{}: {:.0} x {:.0}", region.repo, size.x, size.y);
                        }
                        if self.drag {
                            self.phase = Phase::Dragging;
                            let meta = MetadataFrame::new(Some(DEFAULT_GRAPH_ID.into())).load(ui);
                            let to_screen = |p: Pos2| {
                                meta.canvas_to_screen_pos(p) + self.widget_origin.to_vec2()
                            };
                            let from = self.view.node_position(&dragged()).expect("dragged node");
                            let to =
                                self.view.node_position(&sibling()).expect("sibling") + DROP_OFFSET;
                            println!("drag {from:?} -> {to:?}");
                            self.queue_drag(to_screen(from), to_screen(to));
                        } else {
                            self.phase = Phase::Done;
                        }
                    }
                    "dropped" => {
                        self.phase = Phase::Resettling;
                        self.since = Instant::now();
                    }
                    _ => self.phase = Phase::Done,
                }
            }
            Phase::Dragging if self.pending.is_empty() => {
                let at = self.view.node_position(&dragged());
                println!(
                    "dropped at {at:?}, pinned: {}",
                    self.view.is_pinned(&dragged())
                );
                self.dropped = at;
                self.shoot(ui.ctx(), "dropped");
            }
            Phase::Resettling if self.since.elapsed() >= SETTLE_TIME => {
                let at = self.view.node_position(&dragged());
                let moved = at.zip(self.dropped).map(|(a, d)| (a - d).length());
                println!("{SETTLE_TIME:?} later at {at:?}, moved {moved:?}");
                self.shoot(ui.ctx(), "later");
            }
            Phase::Done => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }
}

/// `repos` repos of `per_repo` nodes each, plus the queen.
fn fleet(repos: usize, per_repo: usize, mode: Mode) -> FleetSnapshot {
    let mut snapshot = FleetSnapshot::default();
    for r in 0..repos {
        let repo = format!("repo-{r}");
        let runs = match mode {
            Mode::Loose => false,
            Mode::Runs => true,
            Mode::Mixed => r % 2 == 1,
        };
        if !runs {
            for t in 0..per_repo {
                let mut n = node(
                    NodeId::terminal(&format!("{repo}_t{t}")),
                    NodeKind::Terminal,
                );
                n.label = format!("{repo}/wt-{t}");
                n.worktree_path = Some(format!("/x/orca/workspaces/{repo}/wt-{t}"));
                n.state = [NodeState::Idle, NodeState::Working][t % 2];
                snapshot.nodes.push(n);
            }
            continue;
        }
        let run_id = format!("run_{r}");
        let coordinator = NodeId::terminal(&format!("{repo}_coord"));
        snapshot.runs.push(RunSummary {
            id: run_id.clone(),
            objective: Some(format!("Objective in {repo}")),
            coordinator: Some(coordinator.clone()),
            task_counts: None,
        });
        let mut coord = node(coordinator.clone(), NodeKind::Coordinator);
        coord.run_id = Some(run_id.clone());
        coord.label = format!("{repo} coordinator");
        coord.worktree_path = Some(format!("/x/orca/workspaces/{repo}/coord"));
        coord.state = NodeState::Working;
        snapshot.nodes.push(coord);
        for w in 1..per_repo {
            let id = NodeId::dispatch(&run_id, &format!("ctx_{r}_{w}"));
            let mut worker = node(id.clone(), NodeKind::Worker);
            worker.run_id = Some(run_id.clone());
            worker.label = format!("task {r}.{w}");
            worker.state = [NodeState::Working, NodeState::Idle, NodeState::Done][w % 3];
            snapshot.nodes.push(worker);
            snapshot.edges.push(Edge {
                from: coordinator.clone(),
                to: id,
                kind: EdgeKind::CoordinatorWorker,
            });
        }
    }
    snapshot.add_queen();
    snapshot
}

fn node(id: NodeId, kind: NodeKind) -> Node {
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
        state: NodeState::Idle,
        worker_state: None,
        dispatch_status: None,
        worker_terminal_state: None,
        released_at_ms: None,
    }
}
