//! Orca worktrees in the top bar: how many there are and how much disk they take, and the
//! cleanup window (evaluate → plan → confirm → saw down). While it runs, a chainsaw cuts the
//! removed worktrees' trees in the ant farm ([`crate::ui::ant_farm::forest`]), or runs along
//! the bottom of the window in the other views.
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
use eframe::egui::{self, Color32, RichText, Vec2, pos2, vec2};

use crate::orca::OrcaClient;
use crate::orca::spawn::Worktree;
use crate::ui::ant_farm::forest::SawJob;
use crate::ui::ant_farm::nature::Deco;
use crate::worktrees::{self, Census, Outcome, Plan, Removal, SweepMarker, format_bytes};

/// How often disk usage is measured (`du` over every worktree takes a while).
pub const SIZE_SCAN_EVERY_MS: i64 = 30 * 60 * 1000;
/// How often the sweep marker is checked, for a cleanup the Queen started.
const MARKER_CHECK_EVERY_MS: i64 = 1_000;
/// Seconds for the saw to cross the window once.
const CROSS_SECONDS: f64 = 7.0;
/// Points per texel of the saw along the window's bottom (the ant farm sizes its own).
const FLOOR_TEXEL: f32 = 3.0;

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
        /// The ids being removed, in order.
        queue: Vec<String>,
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
    /// The chainsaw's pixel art, for the menu and the floor.
    saw: egui::TextureHandle,
}

impl WorktreePanel {
    pub fn reduced_motion(&self) -> bool {
        self.reduced_motion.load(Ordering::Relaxed)
    }

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
            saw: ctx.load_texture(
                "worktrees/saw",
                Deco::Saw.image(),
                egui::TextureOptions::NEAREST,
            ),
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
        let queue = plan.entries.iter().map(|e| e.id.clone()).collect();
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
            queue,
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
    pub fn sawing(&self) -> bool {
        matches!(self.stage, Some(Stage::Running { .. })) || self.external.is_some()
    }

    /// The cleanup removing worktrees right now (here or from the MCP tool), for the saw in
    /// the ant farm.
    pub fn saw_job(&self) -> Option<SawJob> {
        match (&self.stage, &self.external) {
            (
                Some(Stage::Running {
                    queue, done, total, ..
                }),
                _,
            ) => Some(SawJob {
                done: done.len(),
                total: *total,
                current: queue.get(done.len()).cloned(),
                felled: done
                    .iter()
                    .filter(|r| r.outcome == Outcome::Removed)
                    .map(|r| r.id.clone())
                    .collect(),
            }),
            (_, Some(m)) => Some(SawJob {
                done: m.done,
                total: m.total,
                current: m.current_id.clone(),
                felled: Vec::new(),
            }),
            _ => None,
        }
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
            let saw = egui::Image::new(&self.saw).fit_to_exact_size(vec2(22.0, 8.0));
            clean = ui
                .add_enabled(!busy, egui::Button::image_and_text(saw, "Saw down…"))
                .on_hover_text(
                    "Check which worktrees (the trees in the ant farm) are still needed and \
                     plan sawing down the rest. Nothing is cut until you confirm.",
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
        egui::Window::new("Saw down worktrees")
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
        self.saw_job().map(|job| (job.done, job.total))
    }

    /// The chainsaw running along the bottom of the window while worktrees are removed, when
    /// the ant farm (where it cuts their trees) is not on screen.
    pub fn saw(&self, ctx: &egui::Context) {
        let Some((done, total)) = self.progress() else {
            return;
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("worktree_saw"),
        ));
        let screen = ctx.content_rect();
        let size = self.saw.size_vec2() * FLOOR_TEXEL;
        let floor = screen.bottom() - 6.0;
        let label = format!("Sawing worktrees {done}/{total}");
        let text_color = ctx.global_style().visuals.weak_text_color();
        let t = ctx.input(|i| i.time);
        let reduced = self.reduced_motion();
        let span = (screen.width() - 2.0 * size.x).max(1.0);
        // Ping-pong across the window, eased at each end, buzzing as it goes.
        let phase = (t / CROSS_SECONDS).rem_euclid(2.0);
        let u = if phase < 1.0 { phase } else { 2.0 - phase };
        let eased = 0.5 - 0.5 * (u * std::f64::consts::PI).cos();
        let (x, buzz) = if reduced {
            (screen.right() - 150.0, 0.0)
        } else {
            (
                screen.left() + size.x + span * eased as f32,
                (t * 30.0).sin() as f32 * 1.2,
            )
        };
        let rect = egui::Rect::from_center_size(pos2(x, floor - size.y / 2.0 + buzz), size);
        if !reduced {
            draw_chips(&painter, rect.right_center(), t);
        }
        painter.image(
            self.saw.id(),
            rect,
            egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.text(
            rect.center_top() - vec2(0.0, 6.0),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::proportional(12.0),
            text_color,
        );
        ctx.request_repaint_after(std::time::Duration::from_millis(if reduced {
            500
        } else {
            33
        }));
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
                "Sawing down worktrees through Orca… {}/{total}",
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
        "{} linked worktrees: {} can be sawn down, {} are kept (holding {}).",
        plan.entries.len(),
        removable.len(),
        kept.len(),
        format_bytes(plan.kept_bytes())
    ));
    ui.weak(
        "A worktree goes only if nothing runs in it, it has no uncommitted changes, untracked \
         files (other than notes and logs) or open PR, and its work is in the base or on \
         GitHub (merged, rebase- or squash-merged, or in a merged PR's head). Notes are saved \
         and logs deleted first. Each is checked again right before removal.",
    );
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .max_height(max_height - 140.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::CollapsingHeader::new(format!("Saw down ({})", removable.len()))
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
                            let reasons = ui.weak(e.verdict.reasons.join("; "));
                            if !e.verdict.notes.is_empty()
                                && let Some(root) = worktrees::notes_root()
                            {
                                reasons.on_hover_text(format!(
                                    "Notes go to {}:\n{}",
                                    e.notes_dir(&root).display(),
                                    e.verdict.notes.join("\n")
                                ));
                            }
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
                            ui.weak(e.size_text());
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
                        "Saw down {n} worktree{} ({})",
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

/// Wood chips flying off the tip of the bar.
fn draw_chips(painter: &egui::Painter, tip: egui::Pos2, t: f64) {
    let chip = Color32::from_rgb(0xd8, 0xb0, 0x70);
    for i in 0..8 {
        let life = ((t * 2.0 + i as f64 / 8.0).fract()) as f32;
        let seed = (i as f32 * 2.3).sin();
        let at = tip
            + vec2(
                6.0 + life * (20.0 + seed * 8.0),
                -life * 14.0 + life * life * 22.0,
            );
        let alpha = ((1.0 - life) * 200.0) as u8;
        painter.rect_filled(
            egui::Rect::from_center_size(at, Vec2::splat(2.5)),
            0.0,
            Color32::from_rgba_unmultiplied(chip.r(), chip.g(), chip.b(), alpha),
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
