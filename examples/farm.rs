//! The farm on a synthetic clan of 6 runs, loose terminals and subagents (about 30
//! agents), one of each state, to check how it looks.
//!
//! `cargo run --example farm [--light] [--hammer] [--heaps N] [--shot out.png]`.
//! `--receipt-at-ms 31` freezes a receipt mid-shake; `--reduced-motion` shows its flash.
//! `--outline fine|regular|bold` chooses ink width.
//! `--scrying` shows the working rune priest; `--stopped` darkens the rune stone.
//! `--bridge-down` and `--approval` show the Envoy receiver health; `--parked` parks three
//! terminals in the resting area; `--dwarves N` keeps only the first N agents.
//! `--taskmasters` shows coordinators in every state, plus a parked coordinator and attendant.
//! The surface carries a heap per worktree: the
//! agents' own and a few with nobody in them, or `--heaps N` of them; `--hammer` runs the worktree
//! cleanup's hammer on one. Every few seconds one worker
//! flips between working and waiting on the user, so dwarves walk; B broadcasts, S asks for a status
//! report, N hatches a new worker, M sends a burst of messages between dwarves, T toggles the
//! King thinking, R delivers a message to the Envoy (it casts) and X closes a worktree (its heap
//! bursts). With `--shot`, it saves a
//! screenshot once settled and another while a worker walks, a new one emerges, a broadcast
//! ripples and messages fly (`-walk`), plus rubble burst (`-break`) and cleared
//! (`-cleared`) frames, then exits. With `--hammer`, the first shot waits for impact.
//! K closes a synthetic dwarf with the King's tool throw; `--king-throw --shot out.png`
//! captures its flight and impact.
//! `--pixels farm|HEIGHT` and `--palette NAME` select dwarf rendering.
//! `--import SLUG_OR_URL_OR_HEX` imports a real palette; `--palette-editor` shows the Art controls.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui;
use stronghold::ledger::{Entry, Shelf};
use stronghold::model::{
    ClanSnapshot, Edge, EdgeKind, Node, NodeId, NodeKind, NodeState, RunSummary, TaskCounts,
};
use stronghold::orca::AgentIdentity;
use stronghold::orca::spawn::Worktree;
use stronghold::traffic::{AgentMessage, MessageKind};
use stronghold::ui::clan_view::ClanView;
use stronghold::ui::envoy_receipt::ReceiptCue;
use stronghold::ui::farm::art::{ArtSettings, Pixels};
use stronghold::ui::farm::rubble;
use stronghold::ui::farm::{FarmView, Pulse};
use stronghold::ui::worktrees::WorktreePanel;
use stronghold::worktrees::SweepMarker;

const WINDOW: [f32; 2] = [1280.0, 800.0];
const FLIP_EVERY: Duration = Duration::from_secs(3);

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let shot = args
        .iter()
        .position(|a| a == "--shot")
        .and_then(|i| args.get(i + 1).cloned());
    let light = args.iter().any(|a| a == "--light");
    let scrying = args.iter().any(|a| a == "--scrying");
    let health = if args.iter().any(|a| a == "--stopped") {
        stronghold::envoy::EnvoyHealth::Stopped
    } else if args.iter().any(|a| a == "--bridge-down") {
        stronghold::envoy::EnvoyHealth::BridgeDown
    } else if args.iter().any(|a| a == "--approval") {
        stronghold::envoy::EnvoyHealth::AwaitingApproval
    } else {
        stronghold::envoy::EnvoyHealth::Listening
    };
    let receipt_at_ms = args.iter().position(|a| a == "--receipt-at-ms").map(|i| {
        args[i + 1]
            .parse::<i64>()
            .expect("receipt age in milliseconds")
    });
    let reduced_motion = args.iter().any(|a| a == "--reduced-motion");
    let heaps = args
        .iter()
        .position(|a| a == "--heaps")
        .map(|i| args[i + 1].parse::<usize>().expect("a number of heaps"));
    let hammer = args.iter().any(|a| a == "--hammer").then(sweep_marker);
    let parked = args.iter().any(|a| a == "--parked");
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let mut art = ArtSettings {
        pixels: match value("--pixels").as_deref() {
            None => Pixels::Farm,
            Some("farm") => Pixels::Farm,
            Some(h) => Pixels::Height(h.parse().expect("--pixels farm or a height")),
        },
        palette: value("--palette").unwrap_or_default(),
        outline: match value("--outline").as_deref() {
            None | Some("fine") => stronghold::ui::dwarf::cutout::Outline::Fine,
            Some("regular") => stronghold::ui::dwarf::cutout::Outline::Regular,
            Some("bold") => stronghold::ui::dwarf::cutout::Outline::Bold,
            _ => panic!("--outline fine, regular or bold"),
        },
    };
    let palette_cache = tempfile::tempdir().expect("preview palette cache");
    let palette_path = palette_cache.path().join("palettes.json");
    if let Some(input) = value("--import") {
        use stronghold::ui::dwarf::palettes::{self, Input, Library};
        let input = Input::parse(&input).expect("palette input");
        let mut library = Library::default();
        let palette = library.cached(&input).unwrap_or_else(|| match input {
            Input::Lospec(slug) => palettes::fetch(&slug).expect("Lospec import"),
            Input::Colors(_) => unreachable!(),
        });
        art.palette = palette.selection();
        println!(
            "Imported {} ({} colours) from {}",
            palette.name,
            palette.colors.len(),
            palette.source
        );
        library.insert(palette);
        library.save(&palette_path).expect("palette cache");
    }
    let palette_editor = args
        .iter()
        .any(|a| a == "--palette-editor")
        .then(|| stronghold::ui::settings::art::PaletteEditor::load(Some(palette_path)));
    let dwarves = args
        .iter()
        .position(|a| a == "--dwarves")
        .map(|i| args[i + 1].parse::<usize>().expect("a number of dwarves"));
    eframe::run_native(
        "farm",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            persist_window: false,
            ..Default::default()
        },
        Box::new(move |cc| {
            stronghold::ui::theme::install(&cc.egui_ctx);
            cc.egui_ctx.set_theme(if light {
                egui::Theme::Light
            } else {
                egui::Theme::Dark
            });
            let mut snapshot = clan();
            if args.iter().any(|a| a == "--taskmasters") {
                taskmasters(&mut snapshot);
            }
            if let Some(n) = dwarves {
                keep_dwarves(&mut snapshot, n);
            }
            if parked {
                for name in ["sleepy", "dozy", "napper"] {
                    let mut n = node(
                        name,
                        NodeKind::Terminal,
                        None,
                        NodeState::Idle,
                        "stronghold",
                    );
                    n.parked_at_ms = Some(1);
                    snapshot.nodes.push(n);
                }
            }
            snapshot
                .nodes
                .iter_mut()
                .find(|n| n.kind == NodeKind::Envoy)
                .unwrap()
                .label = format!("Envoy · {}", health.label());
            if scrying {
                snapshot
                    .nodes
                    .iter_mut()
                    .find(|n| n.kind == NodeKind::Envoy)
                    .unwrap()
                    .state = NodeState::Working;
            }
            let mut clan = ClanView::new();
            clan.sync(&snapshot);
            clan.set_pr_ready(HashSet::from([
                id("run0-w1"),
                id("run2-w2"),
                id("run4-w0"),
                id("run5-w5"),
            ]));
            clan.set_selected(Some(&id("run2-coord")));
            let hammer = hammer.map(|data| (WorktreePanel::new(&cc.egui_ctx), data));
            let worktrees = worktrees(&snapshot, heaps);
            Ok(Box::new(Demo {
                snapshot,
                clan,
                farm: {
                    // As in the app with the King's chat showing.
                    let mut farm = FarmView::new();
                    farm.set_art(&art);
                    farm.set_reduce_motion(reduced_motion);
                    farm.set_king_focused(true);
                    farm.set_envoy_health(health);
                    if health == stronghold::envoy::EnvoyHealth::AwaitingApproval {
                        farm.set_envoy_pending(2);
                    }
                    farm
                },
                art,
                palette_editor,
                _palette_cache: palette_cache,
                shot,
                king_throw: args.iter().any(|a| a == "--king-throw"),
                captures: Vec::new(),
                receipt_at_ms,
                received: None,
                reduced_motion,
                started: Instant::now(),
                flipped_at: Instant::now(),
                stage: 0,
                thinking: false,
                hammer,
                worktrees,
            }))
        }),
    )
}

struct Demo {
    art: ArtSettings,
    palette_editor: Option<stronghold::ui::settings::art::PaletteEditor>,
    _palette_cache: tempfile::TempDir,
    snapshot: ClanSnapshot,
    clan: ClanView,
    farm: FarmView,
    shot: Option<String>,
    king_throw: bool,
    /// Encode after capturing all frames so PNG compression cannot skip short effects.
    captures: Vec<(std::sync::Arc<egui::ColorImage>, &'static str)>,
    receipt_at_ms: Option<i64>,
    /// When R last delivered a message to the Envoy.
    received: Option<Instant>,
    reduced_motion: bool,
    started: Instant,
    flipped_at: Instant,
    thinking: bool,
    /// Alternating screenshot requests and captures: settled, break, walk, cleared.
    stage: u8,
    /// The cleanup panel running the hammer, and the temporary data dir holding its marker.
    hammer: Option<(WorktreePanel, tempfile::TempDir)>,
    /// One heap each on the surface.
    worktrees: Vec<Worktree>,
}

/// Worktrees for the coordinators' and loose terminals' own, then empty ones up to `count`
/// (four empty ones by default).
fn worktrees(snapshot: &ClanSnapshot, count: Option<usize>) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = snapshot
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Coordinator | NodeKind::Terminal))
        .filter_map(|n| n.worktree_path.clone())
        .map(|path| worktree(&path))
        .collect();
    let count = count.unwrap_or(out.len() + 4);
    out.truncate(count);
    for i in 0..count.saturating_sub(out.len()) {
        out.push(worktree(&format!(
            "/x/orca/workspaces/stronghold/{}",
            STALE[i % STALE.len()]
        )));
    }
    for (i, tree) in out.iter_mut().enumerate() {
        tree.created_at = Some(i as i64);
    }
    if let Some(i) = out
        .iter()
        .position(|w| w.path.ends_with("graph-trackpad-pan"))
    {
        out[i].display_name = Some("graph-trackpad-pan".into());
    }
    out
}

/// Names for worktrees nobody works in any more.
const STALE: [&str; 6] = [
    "graph-trackpad-pan",
    "farm-view",
    "recs-pool-dedupe",
    "docs-recs-custom-css",
    "serving-field",
    "rperf",
];

fn worktree(path: &str) -> Worktree {
    let name = path.rsplit('/').next().unwrap_or(path);
    Worktree {
        id: format!("stronghold::{name}"),
        path: path.into(),
        branch: Some(format!("refs/heads/karl/{name}")),
        ..Worktree::default()
    }
}

/// A running sweep's marker in a temporary data dir, as when the King runs `clean_worktrees`.
fn sweep_marker() -> tempfile::TempDir {
    let data = tempfile::tempdir().expect("temp dir");
    // SAFETY: set before any thread starts.
    unsafe { std::env::set_var("STRONGHOLD_DATA_DIR", data.path()) };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let marker = SweepMarker {
        started_at_ms: now_ms,
        updated_at_ms: now_ms + 3_600_000,
        total: 4,
        done: 1,
        current: Some("graph-trackpad-pan".into()),
        current_id: Some("stronghold::graph-trackpad-pan".into()),
    };
    std::fs::write(
        stronghold::worktrees::default_marker_path(),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    data
}

impl Demo {
    fn throw_tool(&mut self, now_s: f64) {
        let Some(target) = self
            .snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Terminal)
            .map(|n| n.id.clone())
        else {
            return;
        };
        self.farm
            .prepare_king_close(std::iter::once(target.clone()), now_s);
        self.snapshot.nodes.retain(|n| n.id != target);
        self.snapshot.add_king();
        self.clan.sync(&self.snapshot);
        self.flipped_at = Instant::now();
    }

    /// A new worker in run 2, emerging from the King.
    fn hatch(&mut self) {
        let n = self.snapshot.nodes.len();
        let mut worker = node(
            &format!("new-{n}"),
            NodeKind::Worker,
            Some("run2"),
            NodeState::Working,
            "stronghold",
        );
        worker.label = "Fresh hatchling".into();
        self.snapshot.nodes.push(worker);
        self.clan.sync(&self.snapshot);
    }

    /// One message of each kind, plus a repeat that joins its envelope.
    fn mail(&mut self, now_s: f64) {
        for (from, to, kind) in [
            ("ledger", "run1-coord", MessageKind::Ledger),
            ("run0-w1", "ledger", MessageKind::Ledger),
            ("run0-w1", "run0-coord", MessageKind::MergeReady),
            ("run2-w2", "run2-coord", MessageKind::Ask),
            ("run2-w2", "run2-coord", MessageKind::Ask),
            ("king", "run1-coord", MessageKind::Instruction),
            ("karl", "king", MessageKind::Instruction),
            ("slack", "shop-1", MessageKind::Relay),
            ("slack", "envoy", MessageKind::Relay),
            ("envoy", "slack", MessageKind::Message),
            ("king", "run3-w1", MessageKind::Nudge),
            ("shop-2", "karl", MessageKind::Status),
            ("run4-coord", "run5-coord", MessageKind::Message),
        ] {
            let message = AgentMessage::new(from, to, kind);
            let flight = message
                .resolve(&self.snapshot)
                .expect("both ends in the clan");
            self.farm.message(flight, now_s, self.reduced_motion);
        }
    }

    /// Closes the hammer's target (or newest worktree): its heap bursts.
    fn clear_heap(&mut self) {
        let target = self
            .hammer
            .as_ref()
            .and_then(|(panel, _)| panel.cleanup_job())
            .and_then(|job| job.current);
        if let Some(index) = target.and_then(|id| self.worktrees.iter().position(|w| w.id == id)) {
            self.worktrees.remove(index);
        } else {
            self.worktrees.pop();
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
        self.clan.sync(&self.snapshot);
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
    fn persist_egui_memory(&self) -> bool {
        false
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let receipt = self
            .receipt_at_ms
            .or_else(|| self.received.map(|at| at.elapsed().as_millis() as i64))
            .map_or_else(ReceiptCue::default, |elapsed| {
                ReceiptCue::at(Some(0), elapsed, self.reduced_motion)
            });
        self.farm.set_envoy_receipt(receipt);
        self.farm.set_ledger(demo_ledger());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let hammer = self.hammer.as_mut().and_then(|(panel, _)| {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as i64);
                    panel.tick(None, now_ms);
                    panel.cleanup_job()
                });
                self.farm
                    .set_rubble(rubble::sites(&self.worktrees, &self.snapshot.nodes), hammer);
                let response = self.farm.show(ui, &self.snapshot, &mut self.clan);
                if let Some(id) = &response.clicked {
                    self.clan.click_node(id, false);
                }
            });
        if let Some(editor) = &mut self.palette_editor {
            egui::Window::new("Settings · Art")
                .fixed_pos(egui::pos2(16.0, 16.0))
                .default_width(550.0)
                .show(ui.ctx(), |ui| {
                    if let Some(art) = editor.section(ui, &self.art) {
                        self.art = art;
                        self.farm.set_art(&self.art);
                    }
                });
        }
        if self.shot.is_none() {
            if self.flipped_at.elapsed() >= FLIP_EVERY {
                self.flip();
            }
            if ui.input(|i| i.key_pressed(egui::Key::K)) {
                self.throw_tool(ui.input(|i| i.time));
            }
            if ui.input(|i| i.key_pressed(egui::Key::R)) {
                self.received = Some(Instant::now());
                let flight = AgentMessage::new("slack", "envoy", MessageKind::Relay)
                    .resolve(&self.snapshot)
                    .expect("the Envoy in the clan");
                self.farm
                    .message(flight, ui.input(|i| i.time), self.reduced_motion);
            }
            let (b, st, n, t, m, x) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::B),
                    i.key_pressed(egui::Key::S),
                    i.key_pressed(egui::Key::N),
                    i.key_pressed(egui::Key::T),
                    i.key_pressed(egui::Key::M),
                    i.key_pressed(egui::Key::X),
                )
            });
            if x {
                self.clear_heap();
            }
            if m {
                self.mail(ui.input(|i| i.time));
            }
            if t {
                self.thinking = !self.thinking;
                self.farm.set_king_busy(self.thinking);
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
                egui::Event::Screenshot {
                    image, user_data, ..
                } if user_data
                    .data
                    .as_ref()
                    .and_then(|data| data.downcast_ref::<u8>())
                    == Some(&self.stage) =>
                {
                    Some(image.clone())
                }
                _ => None,
            })
        });
        let captured = image.is_some();
        match self.stage {
            0 if self.started.elapsed() >= Duration::from_secs(1)
                && (self.hammer.is_none()
                    || (0.78..0.84)
                        .contains(&((ui.input(|i| i.time) / 1.2).rem_euclid(1.0) as f32))) =>
            {
                self.stage = 1;
            }
            1 => {
                if let Some(image) = image {
                    self.captures.push((image, ""));
                    if self.king_throw {
                        self.throw_tool(ui.input(|i| i.time));
                    } else {
                        self.flip();
                        self.hatch();
                        self.farm.pulse(Pulse::Broadcast);
                        self.mail(ui.input(|i| i.time));
                        self.clear_heap();
                    }
                    self.hammer = None;
                    self.stage = 2;
                }
            }
            2 if self.flipped_at.elapsed() >= Duration::from_millis(180) => {
                self.stage = 3;
            }
            3 => {
                if let Some(image) = image {
                    self.captures
                        .push((image, if self.king_throw { "flight" } else { "break" }));
                    self.stage = 4;
                }
            }
            4 if self.flipped_at.elapsed()
                >= Duration::from_millis(if self.king_throw { 570 } else { 700 }) =>
            {
                self.stage = 5;
            }
            5 => {
                if let Some(image) = image {
                    self.captures
                        .push((image, if self.king_throw { "impact" } else { "walk" }));
                    self.stage = 6;
                }
            }
            6 if self.flipped_at.elapsed() >= Duration::from_millis(1500) => {
                self.stage = 7;
            }
            7 => {
                if let Some(image) = image {
                    self.captures.push((image, "cleared"));
                    for (image, suffix) in &self.captures {
                        self.save_shot(image, suffix);
                    }
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            _ => {}
        }
        // Keep requesting until the renderer replies; stage tags discard late captures.
        if self.stage % 2 == 1 && !captured {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                    self.stage,
                )));
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
        agent: if matches!(kind, NodeKind::King | NodeKind::Envoy) {
            None
        } else if stronghold::ui::dwarf::seed_of(name).is_multiple_of(2) {
            Some(AgentIdentity::Claude)
        } else {
            Some(AgentIdentity::Codex)
        },
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

/// Keeps the Envoy, the King and the first `n` other agents of `s`.
fn keep_dwarves(s: &mut ClanSnapshot, n: usize) {
    let mut kept = 0;
    s.nodes.retain(|node| {
        if matches!(node.kind, NodeKind::King | NodeKind::Envoy) {
            return true;
        }
        kept += 1;
        kept <= n
    });
    let ids: HashSet<NodeId> = s.nodes.iter().map(|n| n.id.clone()).collect();
    s.edges
        .retain(|e| ids.contains(&e.from) && ids.contains(&e.to));
}

fn taskmasters(s: &mut ClanSnapshot) {
    let states = [
        NodeState::Working,
        NodeState::Idle,
        NodeState::NeedsInput,
        NodeState::Stalled,
        NodeState::Done,
        NodeState::Failed,
    ];
    for (coordinator, state) in s
        .nodes
        .iter_mut()
        .filter(|n| n.kind == NodeKind::Coordinator)
        .zip(states)
    {
        coordinator.state = state;
    }
    let mut parked = node(
        "Off duty",
        NodeKind::Coordinator,
        Some("run0"),
        NodeState::Idle,
        "stronghold",
    );
    parked.parked_at_ms = Some(1);
    s.nodes.push(parked);
    s.nodes.push(node(
        "Attendant",
        NodeKind::Attendant,
        Some("run0"),
        NodeState::Working,
        "stronghold",
    ));
}

/// The King, 6 runs of a coordinator and 3–5 workers, loose terminals in two repos, and
/// subagents under a worker and a terminal.
fn clan() -> ClanSnapshot {
    let mut s = ClanSnapshot::default();
    let mut envoy = node(
        "envoy",
        NodeKind::Envoy,
        None,
        NodeState::Idle,
        "stronghold",
    );
    envoy.label = "Envoy · listening".into();
    s.nodes.push(envoy);
    s.nodes
        .push(node("king", NodeKind::King, None, NodeState::Working, ""));
    let objectives = [
        "Farm view",
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
            "stronghold",
        ));
        for w in 0..(3 + r % 3) {
            let name = format!("{run}-w{w}");
            let state = states[(w + r) % states.len()];
            let mut worker = node(&name, NodeKind::Worker, Some(&run), state, "stronghold");
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

/// A Ledger of a dozen entries, for the chamber's sign and hover text.
fn demo_ledger() -> Shelf {
    let entry = |title: &str, folder: &str, kind: &str, age_min: i64| Entry {
        path: format!("{folder}/{title}.md").into(),
        title: title.into(),
        folder: folder.into(),
        kind: Some(kind.into()),
        modified_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
            - age_min * 60_000,
    };
    Shelf {
        count: 12,
        newest: vec![
            entry("Reranker eval needs the warm cache", "web", "finding", 4),
            entry(
                "Billing invoices migrate per market",
                "billing",
                "decision",
                55,
            ),
            entry(
                "Run the flaky poller test alone",
                "stronghold",
                "how-to",
                180,
            ),
        ],
    }
}
