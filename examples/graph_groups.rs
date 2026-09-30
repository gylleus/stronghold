//! Graph view on a synthetic fleet of `repos` repos with `per_repo` nodes each, to check how
//! the repo grouping looks at different group sizes.
//!
//! `cargo run --example graph_groups [repos] [per_repo] [loose|runs|mixed] [--shot out.png]`
//! (default 6 × 4, mixed: even repos hold loose terminals, odd ones a run of a coordinator and
//! its workers). With `--shot`, it saves a screenshot once the layout has had time to settle,
//! prints each repo region's canvas size and exits.

use std::time::{Duration, Instant};

use eframe::egui;
use myrstack::model::{
    Edge, EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RunSummary,
};
use myrstack::orca::AgentIdentity;
use myrstack::ui::graph::GraphView;

/// Time the layout gets before the screenshot.
const SETTLE_TIME: Duration = Duration::from_secs(8);
const WINDOW: [f32; 2] = [1280.0, 800.0];

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
    let mut positional = args
        .iter()
        .take_while(|a| *a != "--shot")
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
                started: Instant::now(),
                requested: false,
            }))
        }),
    )
}

struct Demo {
    view: GraphView,
    shot: Option<String>,
    started: Instant,
    requested: bool,
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            self.view.show(ui);
        });
        let Some(path) = self.shot.clone() else {
            return;
        };
        if !self.requested && self.started.elapsed() >= SETTLE_TIME {
            self.requested = true;
            self.view.fit_to_screen();
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
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
            image::save_buffer(&path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
                .expect("save screenshot");
            for region in self.view.repo_regions() {
                let size = region.rect.size();
                println!("{}: {:.0} x {:.0}", region.repo, size.x, size.y);
            }
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(100));
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
