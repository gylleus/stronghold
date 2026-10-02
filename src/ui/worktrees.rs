//! Orca worktrees in the top bar: how many there are and how much disk they take, and the
//! cleanup window (evaluate → plan → confirm → remove) with a broom flying around the ant
//! farm while it runs (or sweeping along the bottom of the window in the other views).
//!
//! The count comes from the poller's `worktree list` for free; disk usage is measured off the
//! UI thread every [`SIZE_SCAN_EVERY_MS`] and after a cleanup. Evaluation and removal run on
//! their own threads ([`crate::worktrees`]).

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui::{self, Color32, Pos2, RichText, Stroke, Vec2, pos2, vec2};

use crate::orca::OrcaClient;
use crate::orca::spawn::Worktree;
use crate::worktrees::{self, Census, Outcome, Plan, Removal, SweepMarker, format_bytes};

/// How often disk usage is measured (`du` over every worktree takes a while).
pub const SIZE_SCAN_EVERY_MS: i64 = 30 * 60 * 1000;
/// How often the sweep marker is checked, for a cleanup the Queen started.
const MARKER_CHECK_EVERY_MS: i64 = 1_000;
/// Seconds for the broom to cross the window once.
const SWEEP_SECONDS: f64 = 7.0;
/// The broom's size along the window's bottom (the ant farm sizes its own to the farm).
const FLOOR_SCALE: f32 = 1.3;

/// Disk usage of the worktrees and the unlisted directories.
#[derive(Debug, Clone, Default)]
struct Sizes {
    by_path: HashMap<PathBuf, u64>,
    orphans: Vec<(PathBuf, Option<u64>)>,
}

enum ExecMsg {
    Progress(Removal),
    Done(Result<Vec<Removal>, String>),
}

/// A cleanup being looked at or run.
enum Stage {
    Evaluating(Receiver<Result<Plan, String>>),
    Failed(String),
    /// The plan, with the removable ids Karl left ticked.
    Review {
        plan: Plan,
        chosen: BTreeSet<String>,
    },
    Running {
        rx: Receiver<ExecMsg>,
        total: usize,
        done: Vec<Removal>,
    },
    Finished {
        removals: Vec<Removal>,
        error: Option<String>,
    },
}

pub struct WorktreePanel {
    ctx: egui::Context,
    client: OrcaClient,
    sizes: Sizes,
    size_scan: Option<Receiver<Sizes>>,
    next_scan_ms: i64,
    stage: Option<Stage>,
    /// The last cleanup's summary, for the top-bar menu.
    last_summary: Option<String>,
    /// A cleanup started elsewhere (the MCP tool), from the marker file.
    external: Option<SweepMarker>,
    next_marker_check_ms: i64,
    reduced_motion: Arc<AtomicBool>,
}

impl WorktreePanel {
    pub fn new(ctx: &egui::Context) -> Self {
        let reduced_motion = Arc::new(AtomicBool::new(false));
        let flag = reduced_motion.clone();
        let _ = thread::Builder::new()
            .name("reduce-motion".into())
            .spawn(move || flag.store(system_reduces_motion(), Ordering::Relaxed));
        Self {
            ctx: ctx.clone(),
            client: OrcaClient::new().without_sender_identity(),
            sizes: Sizes::default(),
            size_scan: None,
            next_scan_ms: 0,
            stage: None,
            last_summary: None,
            external: None,
            next_marker_check_ms: 0,
            reduced_motion,
        }
    }

    /// Collects background results and starts what is due. Returns true when a cleanup just
    /// finished (the fleet should be polled again).
    pub fn tick(&mut self, worktrees: Option<&[Worktree]>, now_ms: i64) -> bool {
        if let Some(rx) = &self.size_scan {
            match rx.try_recv() {
                Ok(sizes) => {
                    self.sizes = sizes;
                    self.size_scan = None;
                }
                Err(TryRecvError::Disconnected) => self.size_scan = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(worktrees) = worktrees
            && !worktrees.is_empty()
            && self.size_scan.is_none()
            && now_ms >= self.next_scan_ms
        {
            self.next_scan_ms = now_ms + SIZE_SCAN_EVERY_MS;
            self.size_scan = Some(self.scan_sizes(worktrees));
        }
        if now_ms >= self.next_marker_check_ms {
            self.next_marker_check_ms = now_ms + MARKER_CHECK_EVERY_MS;
            let own = matches!(self.stage, Some(Stage::Running { .. }));
            self.external = (!own)
                .then(|| worktrees::running_sweep(&worktrees::default_marker_path(), now_ms))
                .flatten();
        }
        self.poll_stage()
    }

    fn poll_stage(&mut self) -> bool {
        let next = match &mut self.stage {
            Some(Stage::Evaluating(rx)) => match rx.try_recv() {
                Ok(Ok(plan)) => Some(Stage::Review {
                    chosen: plan.removable().map(|e| e.id.clone()).collect(),
                    plan,
                }),
                Ok(Err(e)) => Some(Stage::Failed(e)),
                Err(TryRecvError::Disconnected) => {
                    Some(Stage::Failed("the evaluation stopped".into()))
                }
                Err(TryRecvError::Empty) => None,
            },
            Some(Stage::Running { rx, done, .. }) => {
                let mut finished = None;
                loop {
                    match rx.try_recv() {
                        Ok(ExecMsg::Progress(r)) => done.push(r),
                        Ok(ExecMsg::Done(result)) => {
                            finished = Some(result);
                            break;
                        }
                        Err(TryRecvError::Disconnected) => {
                            finished = Some(Err("the cleanup stopped".into()));
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                    }
                }
                finished.map(|result| {
                    let (removals, error) = match result {
                        Ok(r) => (r, None),
                        Err(e) => (std::mem::take(done), Some(e)),
                    };
                    self.last_summary = Some(worktrees::summary(&removals));
                    Stage::Finished { removals, error }
                })
            }
            _ => None,
        };
        let Some(next) = next else {
            return false;
        };
        let finished = matches!(next, Stage::Finished { .. });
        self.stage = Some(next);
        if finished {
            // Measure again with the removed ones gone.
            self.next_scan_ms = 0;
        }
        finished
    }

    fn scan_sizes(&self, worktrees: &[Worktree]) -> Receiver<Sizes> {
        let paths: Vec<PathBuf> = worktrees
            .iter()
            .filter(|w| !w.is_main_worktree)
            .map(|w| PathBuf::from(&w.path))
            .collect();
        let listed = worktrees.to_vec();
        let ctx = self.ctx.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = thread::Builder::new()
            .name("worktree-sizes".into())
            .spawn(move || {
                let orphans = worktrees::workspaces_root()
                    .map(|root| worktrees::orphan_dirs(&root, &listed))
                    .unwrap_or_default();
                let mut all = paths;
                all.extend(orphans.iter().cloned());
                let by_path = worktrees::disk_usages(&all);
                let orphans = orphans
                    .into_iter()
                    .map(|p| {
                        let size = by_path.get(&p).copied();
                        (p, size)
                    })
                    .collect();
                let _ = tx.send(Sizes { by_path, orphans });
                ctx.request_repaint();
            });
        rx
    }

    fn start_evaluation(&mut self) {
        let client = self.client.clone();
        let sizes = self.sizes.by_path.clone();
        let ctx = self.ctx.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = thread::Builder::new()
            .name("worktree-evaluate".into())
            .spawn(move || {
                let _ = tx.send(worktrees::evaluate(&client, &sizes, now_ms()));
                ctx.request_repaint();
            });
        self.stage = Some(Stage::Evaluating(rx));
    }

    fn start_removal(&mut self, plan: &Plan, chosen: &BTreeSet<String>) {
        let mut plan = plan.clone();
        plan.entries
            .retain(|e| e.verdict.remove && chosen.contains(&e.id));
        let total = plan.entries.len();
        let client = self.client.clone();
        let ctx = self.ctx.clone();
        let (tx, rx) = crossbeam_channel::unbounded();
        let _ = thread::Builder::new()
            .name("worktree-remove".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let progress_ctx = ctx.clone();
                let result = worktrees::execute(
                    &client,
                    &plan,
                    &worktrees::default_log_path(),
                    now_ms,
                    |r| {
                        let _ = progress_tx.send(ExecMsg::Progress(r.clone()));
                        progress_ctx.request_repaint();
                    },
                );
                let _ = tx.send(ExecMsg::Done(result));
                ctx.request_repaint();
            });
        self.stage = Some(Stage::Running {
            rx,
            total,
            done: Vec::new(),
        });
    }

    /// Opens the window on an already evaluated plan (for `examples/worktree_cleanup.rs`).
    pub fn show_plan(&mut self, plan: Plan) {
        self.stage = Some(Stage::Review {
            chosen: plan.removable().map(|e| e.id.clone()).collect(),
            plan,
        });
    }

    /// Whether a cleanup is removing worktrees right now (here or from the MCP tool).
    pub fn sweeping(&self) -> bool {
        matches!(self.stage, Some(Stage::Running { .. })) || self.external.is_some()
    }

    // -----------------------------------------------------------------------
    // Drawing

    /// The "N worktrees · size" menu in the top bar.
    pub fn top_bar(&mut self, ui: &mut egui::Ui, worktrees: &[Worktree]) {
        let census = worktrees::census(worktrees);
        let linked: Vec<&Worktree> = worktrees.iter().filter(|w| !w.is_main_worktree).collect();
        let total_bytes: Option<u64> = (!self.sizes.by_path.is_empty()).then(|| {
            linked
                .iter()
                .filter_map(|w| self.sizes.by_path.get(&PathBuf::from(&w.path)))
                .sum()
        });
        let mut title = format!(
            "{} worktree{}",
            census.total,
            if census.total == 1 { "" } else { "s" }
        );
        if let Some(bytes) = total_bytes {
            title.push_str(&format!(" · {}", format_bytes(bytes)));
        }
        let mut clean = false;
        ui.menu_button(RichText::new(title).weak(), |ui| {
            ui.set_min_width(260.0);
            self.census_menu(ui, &census, &linked);
            ui.separator();
            let busy = matches!(
                self.stage,
                Some(Stage::Evaluating(_) | Stage::Running { .. })
            ) || self.external.is_some();
            clean = ui
                .add_enabled(!busy, egui::Button::new("🧹 Clean up…"))
                .on_hover_text(
                    "Check which worktrees are still needed and plan removing the rest. \
                     Nothing is removed until you confirm.",
                )
                .clicked();
            if let Some(summary) = &self.last_summary {
                ui.weak(format!("Last cleanup: {summary}"));
            }
        })
        .response
        .on_hover_text("Orca worktrees (linked, not main checkouts) and their disk usage");
        if clean {
            self.start_evaluation();
        }
    }

    fn census_menu(&self, ui: &mut egui::Ui, census: &Census, linked: &[&Worktree]) {
        let size_of = |repo: &str| -> Option<u64> {
            if self.sizes.by_path.is_empty() {
                return None;
            }
            Some(
                linked
                    .iter()
                    .filter(|w| worktrees::repo_name(w) == repo)
                    .filter_map(|w| self.sizes.by_path.get(&PathBuf::from(&w.path)))
                    .sum(),
            )
        };
        egui::Grid::new("worktree_census")
            .num_columns(3)
            .spacing([16.0, 2.0])
            .show(ui, |ui| {
                for (repo, count) in &census.per_repo {
                    ui.label(repo);
                    ui.label(count.to_string());
                    ui.weak(size_of(repo).map(format_bytes).unwrap_or_default());
                    ui.end_row();
                }
            });
        if census.per_repo.is_empty() {
            ui.weak("No linked worktrees");
        }
        if self.size_scan.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Measuring disk usage…");
            });
        }
        if !self.sizes.orphans.is_empty() {
            let bytes: u64 = self.sizes.orphans.iter().filter_map(|(_, b)| *b).sum();
            ui.weak(format!(
                "+ {} folder{} in ~/orca/workspaces Orca doesn't list ({})",
                self.sizes.orphans.len(),
                if self.sizes.orphans.len() == 1 {
                    ""
                } else {
                    "s"
                },
                format_bytes(bytes)
            ))
            .on_hover_text(
                self.sizes
                    .orphans
                    .iter()
                    .map(|(p, b)| {
                        format!(
                            "{} ({})",
                            p.display(),
                            b.map(format_bytes).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }

    /// The cleanup window, while a cleanup is open.
    pub fn window(&mut self, ctx: &egui::Context) {
        let Some(stage) = self.stage.as_mut() else {
            return;
        };
        let running = matches!(stage, Stage::Running { .. });
        let mut open = true;
        let mut action = None;
        let max_height = ctx.content_rect().height() * 0.7;
        egui::Window::new("Worktree cleanup")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                action = stage_view(ui, stage, max_height);
            });
        match action {
            Some(Action::Remove) => {
                if let Some(Stage::Review { plan, chosen }) = &self.stage {
                    let (plan, chosen) = (plan.clone(), chosen.clone());
                    self.start_removal(&plan, &chosen);
                }
            }
            Some(Action::Retry) => self.start_evaluation(),
            Some(Action::Close) => self.stage = None,
            None if !open && !running => self.stage = None,
            None => {}
        }
    }

    /// The removal progress, while a cleanup runs here or from the MCP tool.
    fn progress(&self) -> Option<(usize, usize)> {
        match (&self.stage, &self.external) {
            (Some(Stage::Running { total, done, .. }), _) => Some((done.len(), *total)),
            (_, Some(m)) => Some((m.done, m.total)),
            _ => None,
        }
    }

    /// The broom flying around inside the ant farm (`farm` is its rect) while worktrees are
    /// removed. It paints into the farm's own layer, so windows stay on top and clicks pass
    /// through.
    pub fn broom_in_farm(&self, ui: &egui::Ui, farm: egui::Rect) {
        let Some((done, total)) = self.progress() else {
            return;
        };
        let ctx = ui.ctx();
        let painter = ui.painter_at(farm);
        let label = format!("Sweeping worktrees {done}/{total}");
        let scale = (farm.width().min(farm.height()) / 200.0).clamp(1.6, 2.8);
        // The broom's middle, so the whole broom (any tilt) clears the farm's edges.
        let reach = 40.0 * scale;
        let lift = vec2(0.0, 29.0 * scale);
        let inner = farm.shrink2(vec2(
            (farm.width() / 2.0 - 1.0).min(reach),
            (farm.height() / 2.0 - 1.0).min(reach + 18.0),
        ));
        if self.reduced_motion.load(Ordering::Relaxed) {
            let center = inner.right_bottom();
            draw_broom(&painter, center + lift, 0.0, 1.0, scale);
            broom_label(&painter, center - lift - vec2(0.0, 6.0), label);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            return;
        }
        let t = ctx.input(|i| i.time);
        let center = flight(inner, t);
        let velocity = (flight(inner, t + 0.05) - center) / 0.05;
        let dir = if velocity.x < 0.0 { -1.0 } else { 1.0 };
        // Leans into the turn, bristles trailing, with a quick sweeping wobble.
        let lean = (velocity.x / (inner.width() + 1.0)).clamp(-1.0, 1.0) * 0.6;
        let wobble = (t * 7.0).sin() as f32 * 0.08;
        let dark = ctx.global_style().visuals.dark_mode;
        draw_trail(&painter, inner, lift, t, dark);
        draw_broom(&painter, center + lift, lean + wobble, dir, scale);
        broom_label(&painter, center - lift - vec2(0.0, 6.0), label);
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    /// The broom sweeping along the bottom of the window while worktrees are removed, when
    /// the ant farm is not on screen.
    pub fn broom(&self, ctx: &egui::Context) {
        let Some((done, total)) = self.progress() else {
            return;
        };
        let reduced = self.reduced_motion.load(Ordering::Relaxed);
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("worktree_broom"),
        ));
        let floor = screen.bottom() - 6.0;
        let t = ctx.input(|i| i.time);
        let label = format!("Sweeping worktrees {done}/{total}");
        let text_color = ctx.global_style().visuals.weak_text_color();
        if reduced {
            let at = pos2(screen.right() - 150.0, floor);
            draw_broom(&painter, at, 0.0, 1.0, FLOOR_SCALE);
            painter.text(
                at + vec2(-40.0, -4.0),
                egui::Align2::RIGHT_BOTTOM,
                label,
                egui::FontId::proportional(12.0),
                text_color,
            );
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            return;
        }
        let span = (screen.width() - 120.0).max(1.0);
        // Ping-pong across the window, eased at each end.
        let phase = (t / SWEEP_SECONDS).rem_euclid(2.0);
        let (u, dir) = if phase < 1.0 {
            (phase, 1.0)
        } else {
            (2.0 - phase, -1.0)
        };
        let eased = 0.5 - 0.5 * (u * std::f64::consts::PI).cos();
        let x = screen.left() + 60.0 + span * eased as f32;
        // The sweeping stroke: the broom rocks a few times a second.
        let rock = (t * 7.0).sin() as f32;
        let at = pos2(x, floor);
        draw_dust(
            &painter,
            at,
            dir as f32,
            t,
            ctx.global_style().visuals.dark_mode,
        );
        draw_broom(
            &painter,
            at,
            rock * 0.22 - dir as f32 * 0.25,
            dir as f32,
            FLOOR_SCALE,
        );
        painter.text(
            at + vec2(0.0, -82.0),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::proportional(12.0),
            text_color,
        );
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

enum Action {
    Remove,
    Retry,
    Close,
}

fn stage_view(ui: &mut egui::Ui, stage: &mut Stage, max_height: f32) -> Option<Action> {
    match stage {
        Stage::Evaluating(_) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Checking every worktree: git status, merges, PRs, terminals, processes…");
            });
            None
        }
        Stage::Failed(e) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Couldn't evaluate: {e}"),
            );
            ui.weak("Nothing was removed.");
            ui.horizontal(|ui| {
                if ui.button("Try again").clicked() {
                    return Some(Action::Retry);
                }
                ui.button("Close").clicked().then_some(Action::Close)
            })
            .inner
        }
        Stage::Review { plan, chosen } => review_view(ui, plan, chosen, max_height),
        Stage::Running { total, done, .. } => {
            ui.label(format!(
                "Removing worktrees through Orca… {}/{total}",
                done.len()
            ));
            ui.add(egui::ProgressBar::new(
                done.len() as f32 / (*total).max(1) as f32,
            ));
            removal_list(ui, done, max_height);
            None
        }
        Stage::Finished { removals, error } => {
            ui.label(RichText::new(worktrees::summary(removals)).strong());
            if let Some(e) = error {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
            }
            removal_list(ui, removals, max_height);
            ui.weak(format!(
                "Logged to {}",
                worktrees::default_log_path().display()
            ));
            ui.button("Close").clicked().then_some(Action::Close)
        }
    }
}

fn review_view(
    ui: &mut egui::Ui,
    plan: &Plan,
    chosen: &mut BTreeSet<String>,
    max_height: f32,
) -> Option<Action> {
    let removable: Vec<_> = plan.removable().collect();
    let kept: Vec<_> = plan.kept().collect();
    let freed: u64 = removable
        .iter()
        .filter(|e| chosen.contains(&e.id))
        .filter_map(|e| e.bytes)
        .sum();
    ui.label(format!(
        "{} linked worktrees: {} can go, {} are kept.",
        plan.entries.len(),
        removable.len(),
        kept.len()
    ));
    ui.weak(
        "A worktree goes only if nothing runs in it, it has no uncommitted changes or open PR, \
         and its work is merged (or its PR merged/closed with every commit pushed). Each is \
         checked again right before removal.",
    );
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .max_height(max_height - 140.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::CollapsingHeader::new(format!("Remove ({})", removable.len()))
                .default_open(true)
                .show(ui, |ui| {
                    if removable.is_empty() {
                        ui.weak("Nothing to remove.");
                    }
                    for e in &removable {
                        let mut on = chosen.contains(&e.id);
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut on, "").changed() {
                                if on {
                                    chosen.insert(e.id.clone());
                                } else {
                                    chosen.remove(&e.id);
                                }
                            }
                            ui.label(format!("{}/{}", e.repo, e.name))
                                .on_hover_text(&e.path);
                            ui.weak(e.bytes.map(format_bytes).unwrap_or_default());
                            ui.weak(e.verdict.reasons.join("; "));
                        });
                    }
                });
            egui::CollapsingHeader::new(format!("Keep ({})", kept.len()))
                .default_open(false)
                .show(ui, |ui| {
                    for e in &kept {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("{}/{}", e.repo, e.name))
                                .on_hover_text(&e.path);
                            ui.weak(e.verdict.reasons.join("; "));
                        });
                    }
                });
            if !plan.orphans.is_empty() {
                egui::CollapsingHeader::new(format!(
                    "Folders Orca doesn't list ({}, not touched)",
                    plan.orphans.len()
                ))
                .default_open(false)
                .show(ui, |ui| {
                    for p in &plan.orphans {
                        ui.weak(p);
                    }
                });
            }
        });
    ui.separator();
    ui.horizontal(|ui| {
        let n = chosen.len();
        let remove = ui
            .add_enabled(
                n > 0,
                egui::Button::new(
                    RichText::new(format!(
                        "Remove {n} worktree{} ({})",
                        if n == 1 { "" } else { "s" },
                        format_bytes(freed)
                    ))
                    .color(ui.visuals().error_fg_color),
                ),
            )
            .on_hover_text("Runs `orca worktree rm` (no --force) for each ticked worktree")
            .clicked();
        if remove {
            return Some(Action::Remove);
        }
        if ui.button("Re-check").clicked() {
            return Some(Action::Retry);
        }
        ui.button("Cancel").clicked().then_some(Action::Close)
    })
    .inner
}

fn removal_list(ui: &mut egui::Ui, removals: &[Removal], max_height: f32) {
    egui::ScrollArea::vertical()
        .max_height(max_height - 120.0)
        .auto_shrink([false, true])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for r in removals {
                let name = format!("{}/{}", r.repo, r.name);
                match &r.outcome {
                    Outcome::Removed => {
                        ui.label(format!(
                            "✓ {name} {}",
                            r.bytes.map(format_bytes).unwrap_or_default()
                        ));
                    }
                    Outcome::Skipped { reasons } => {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!("kept {name}: {}", reasons.join("; ")),
                        );
                    }
                    Outcome::Failed { error } => {
                        ui.colored_label(ui.visuals().error_fg_color, format!("✗ {name}: {error}"));
                    }
                }
            }
        });
}

/// Where the flying broom's middle is at `t` seconds: two slow Lissajous loops blended, so
/// it curves up, down and across `area` without ever leaving it.
fn flight(area: egui::Rect, t: f64) -> Pos2 {
    let x = 0.75 * (t * 0.42).sin() + 0.25 * (t * 1.13 + 1.0).sin();
    let y = 0.7 * (t * 0.61 + 0.6).sin() + 0.3 * (t * 0.97).cos();
    area.center() + vec2(x as f32, y as f32) * area.size() / 2.0
}

/// Dust puffs strewn along the path the flying broom just took, under its bristles.
fn draw_trail(painter: &egui::Painter, area: egui::Rect, lift: Vec2, t: f64, dark: bool) {
    let base = dust_color(dark);
    for i in 1..=10 {
        let age = i as f32 / 10.0;
        let at = flight(area, t - f64::from(age) * 0.7) + lift;
        let alpha = ((1.0 - age) * 130.0) as u8;
        painter.circle_filled(
            at,
            2.5 + age * 6.0,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha),
        );
    }
}

/// The progress label over the flying broom.
fn broom_label(painter: &egui::Painter, at: Pos2, label: String) {
    let font = egui::FontId::proportional(13.0);
    let shadow = Color32::from_black_alpha(160);
    painter.text(
        at + vec2(1.0, 1.0),
        egui::Align2::CENTER_BOTTOM,
        &label,
        font.clone(),
        shadow,
    );
    painter.text(
        at,
        egui::Align2::CENTER_BOTTOM,
        label,
        font,
        Color32::from_rgb(0xf4, 0xe8, 0xc8),
    );
}

fn dust_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0xe6, 0xdc, 0xc8)
    } else {
        Color32::from_rgb(0x9a, 0x86, 0x6a)
    }
}

/// A broom standing on `foot` (the bristles' bottom center), tilted `tilt` radians, facing
/// `dir` (1 sweeps right), `scale` times its base size.
fn draw_broom(painter: &egui::Painter, foot: Pos2, tilt: f32, dir: f32, scale: f32) {
    let rot = |p: Vec2| -> Pos2 {
        let (s, c) = tilt.sin_cos();
        foot + scale * vec2(p.x * c - p.y * s, p.x * s + p.y * c)
    };
    let handle = Color32::from_rgb(0x9a, 0x6a, 0x3c);
    let handle_dark = Color32::from_rgb(0x6e, 0x4a, 0x28);
    let straw = Color32::from_rgb(0xe8, 0xc1, 0x5a);
    let straw_dark = Color32::from_rgb(0xc4, 0x96, 0x34);
    let band = Color32::from_rgb(0xc0, 0x4a, 0x3a);
    // Handle, leaning back against the sweep.
    let top = rot(vec2(-dir * 14.0, -58.0));
    let neck = rot(vec2(0.0, -20.0));
    painter.line_segment([top, neck], Stroke::new(3.0 * scale, handle_dark));
    painter.line_segment([top, neck], Stroke::new(1.9 * scale, handle));
    // Bristles: a trapezoid, wider at the floor.
    let bristles = vec![
        rot(vec2(-6.0, -20.0)),
        rot(vec2(6.0, -20.0)),
        rot(vec2(13.0, 0.0)),
        rot(vec2(-13.0, 0.0)),
    ];
    painter.add(egui::Shape::convex_polygon(
        bristles,
        straw,
        Stroke::new(0.8 * scale, straw_dark),
    ));
    for i in -3..=3 {
        let x = i as f32;
        painter.line_segment(
            [rot(vec2(x * 1.6, -17.0)), rot(vec2(x * 3.6, -1.0))],
            Stroke::new(0.8 * scale, straw_dark),
        );
    }
    // Binding.
    painter.line_segment(
        [rot(vec2(-7.0, -16.0)), rot(vec2(7.0, -16.0))],
        Stroke::new(2.3 * scale, band),
    );
}

/// Little dust puffs kicked up behind the broom.
fn draw_dust(painter: &egui::Painter, foot: Pos2, dir: f32, t: f64, dark: bool) {
    let base = dust_color(dark);
    for i in 0..6 {
        // Each puff lives 0.9 s, staggered.
        let life = ((t * 1.4 + i as f64 / 6.0).fract()) as f32;
        let seed = (i as f32 * 2.3).sin();
        let offset = vec2(
            -dir * (10.0 + life * 34.0 + seed * 6.0),
            -2.0 - life * (10.0 + seed.abs() * 8.0),
        );
        let alpha = ((1.0 - life) * 150.0) as u8;
        painter.circle_filled(
            foot + offset,
            2.0 + life * 5.0,
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha),
        );
    }
}

/// macOS "Reduce motion" (System Settings → Accessibility → Display).
fn system_reduces_motion() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "com.apple.universalaccess", "reduceMotion"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
