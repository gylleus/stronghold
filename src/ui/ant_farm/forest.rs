//! The forest on the surface: one tree per open worktree, so the more worktrees are open, the
//! more trees grow. A tree's look says what is in its worktree (an agent at work: leafy; only
//! idle, done or parked ones: autumn; none: bare, likely one the cleanup can take). The
//! worktree cleanup is a chainsaw that cuts the trees of the worktrees it removes; a tree whose
//! worktree goes away falls and leaves a stump for a moment.
//!
//! The forest fills the surface from the left like a loading bar, oldest worktree first, so
//! how far right it reaches shows how much is open. [`groves`] and [`place`] are pure, and a
//! tree's size comes from a hash of its id, so the wood looks the same every frame.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Vec2, pos2, vec2};

use super::layout::Scene;
use super::nature::Deco;
use super::sprites::{Palette, Sprites};
use crate::model::{Node, NodeState};
use crate::orca::spawn::Worktree;

/// How long a sawn tree takes to fall, lies there, and fades with its stump, in seconds.
const FALL_S: f64 = 1.0;
const LIE_S: f64 = 0.9;
const FADE_S: f64 = 0.8;
/// Worktrees that fill the surface's strip end to end: the forest grows from the left like
/// a loading bar, and past this many it only gets denser.
pub const FILL_AT: usize = 40;
/// Most trees drawn; the rest go on a "+N" sign.
pub const MAX_TREES: usize = 160;
/// Texels of a map per farm texel of the biggest tree.
pub const FULL: f32 = 3.0;
/// The biggest tree spans at most this many trees' spacing.
const DENSE_SPAN: f32 = 4.0;
/// The smallest tree, as a share of the biggest.
const SMALLEST: f32 = 0.5;
/// Sky kept clear above the tallest tree, in farm texels.
const HEADROOM: f32 = 6.0;
/// The back row is drawn darker.
const BACK_TINT: u8 = 165;

/// What is in a worktree, as its tree shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Health {
    /// An agent works in it, or waits on Karl.
    Leafy,
    /// Its agents are idle, done, failed or parked.
    Resting,
    /// No agent is in it.
    Bare,
}

impl Health {
    fn deco(self) -> Deco {
        match self {
            Health::Leafy => Deco::Tree,
            Health::Resting => Deco::AutumnTree,
            Health::Bare => Deco::BareTree,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Health::Leafy => "an agent is working in it",
            Health::Resting => "its agents are idle, done or parked",
            Health::Bare => "no agent in it: the saw may be able to take it",
        }
    }
}

/// One open worktree, as a tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Grove {
    pub id: String,
    pub name: String,
    pub health: Health,
}

/// The cleanup at work: its progress, the worktree being cut and the ones already removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SawJob {
    pub done: usize,
    pub total: usize,
    /// Id of the worktree being removed now.
    pub current: Option<String>,
    /// Ids removed so far: their trees fall right away, before the next poll drops them.
    pub felled: Vec<String>,
}

/// The trees of the linked worktrees (main checkouts and archived ones grow none), oldest
/// first, each with its [`Health`] from the agents in `nodes` whose worktree it is.
pub fn groves(worktrees: &[Worktree], nodes: &[Node]) -> Vec<Grove> {
    let trim = |p: &str| p.trim_end_matches('/').to_string();
    let mut agents: HashMap<String, Health> = HashMap::new();
    for node in nodes {
        let Some(path) = node.worktree_path.as_deref() else {
            continue;
        };
        let health = if node.is_parked() || !node.live {
            if !node.is_parked() {
                continue;
            }
            Health::Resting
        } else if matches!(
            node.state,
            NodeState::Working | NodeState::NeedsInput | NodeState::Stalled
        ) {
            Health::Leafy
        } else {
            Health::Resting
        };
        let entry = agents.entry(trim(path)).or_insert(health);
        if health == Health::Leafy {
            *entry = Health::Leafy;
        }
    }
    let mut linked: Vec<&Worktree> = worktrees
        .iter()
        .filter(|w| !w.is_main_worktree && !w.is_archived)
        .collect();
    linked.sort_by(|a, b| {
        (a.created_at.unwrap_or(i64::MAX), &a.id).cmp(&(b.created_at.unwrap_or(i64::MAX), &b.id))
    });
    linked
        .into_iter()
        .map(|w| Grove {
            id: w.id.clone(),
            name: w.name().to_string(),
            health: agents.get(&trim(&w.path)).copied().unwrap_or(Health::Bare),
        })
        .collect()
}

/// A tree where it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    pub grove: Grove,
    /// The middle of its trunk's base, in the grass.
    pub foot: Pos2,
    /// Screen points per texel of its map.
    pub k: f32,
    /// In the back row: drawn first and darker.
    pub back: bool,
}

impl Tree {
    pub fn rect(&self) -> Rect {
        let [w, h] = self.grove.health.deco().size();
        let size = vec2(w as f32, h as f32) * self.k;
        Rect::from_min_size(self.foot - vec2(size.x / 2.0, size.y), size)
    }

    /// The trunk's middle, a little above the grass: where the saw cuts.
    fn cut(&self) -> Pos2 {
        self.foot - vec2(0.0, 3.0 * self.k)
    }
}

/// The trees on the surface, and those that did not fit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Forest {
    /// In draw order: back row first, then left to right.
    pub trees: Vec<Tree>,
    /// Worktrees with no room for a tree (the newest).
    pub more: usize,
    /// Where the "+N" sign stands when some did not fit.
    pub sign: Option<Pos2>,
}

impl Forest {
    pub fn tree(&self, id: &str) -> Option<&Tree> {
        self.trees.iter().find(|t| t.grove.id == id)
    }
}

/// The strip of surface the forest grows on: the whole width, behind everything else there.
fn strip(scene: &Scene) -> (f32, f32) {
    let a = scene.cell();
    (scene.rect.left() + 0.5 * a, scene.rect.right() - 0.5 * a)
}

/// Points per map texel of a tree at `scale` farm texels each, shrunk so it fits under the sky.
pub fn texel(scene: &Scene, scale: f32) -> f32 {
    let [_, tree_h] = Deco::Tree.size();
    let sky = scene.surface_y - scene.rect.top() - HEADROOM * scene.px;
    (scale * scene.px).min(sky / tree_h as f32).max(scene.px)
}

/// Where each grove's tree stands: oldest at the left, the forest reaching further right the
/// more worktrees there are (all the way at [`FILL_AT`]), each tree a size and a nudge of its
/// own from a hash of its id, so they cluster like a wood. Past [`MAX_TREES`] the newest go
/// uncounted into a "+N" sign at the right end.
pub fn place(scene: &Scene, groves: &[Grove]) -> Forest {
    let (start, end) = strip(scene);
    let [tree_w, _] = Deco::Tree.size();
    let room = groves.len().min(MAX_TREES);
    // A dense wood is drawn smaller, so its trees stay apart enough to count.
    let spacing = (end - start) / room.max(FILL_AT) as f32;
    let full = texel(scene, FULL).min((spacing * DENSE_SPAN / tree_w as f32).max(2.0 * scene.px));
    let more = groves.len() - room;
    let sign = (more > 0).then(|| pos2(end - 1.5 * scene.cell(), scene.surface_y + scene.px));
    let right_end = if more > 0 {
        end - 3.0 * scene.cell()
    } else {
        end
    };
    if room == 0 || right_end <= start {
        return Forest {
            more: groves.len(),
            sign,
            ..Forest::default()
        };
    }
    // Trunks run from where the biggest tree's crown clears the strip's left end.
    let left = (start + tree_w as f32 * full / 2.0).min(right_end);
    let fill = (room as f32 / FILL_AT as f32).min(1.0);
    let reach = left + (right_end - tree_w as f32 * full * SMALLEST / 2.0 - left).max(0.0) * fill;
    let gap = (reach - left) / room as f32;
    let mut trees: Vec<Tree> = groves
        .iter()
        .take(room)
        .enumerate()
        .map(|(i, grove)| {
            let h = hash(&grove.id);
            let size = SMALLEST + (1.0 - SMALLEST) * (h % 1000) as f32 / 999.0;
            let k = full * size;
            let half = tree_w as f32 * k / 2.0;
            let nudge = ((h >> 10) % 1000) as f32 / 999.0 - 0.5;
            let x = left + (i as f32 + 0.5 + 0.6 * nudge) * gap;
            Tree {
                grove: grove.clone(),
                foot: pos2(
                    x.clamp(start + half, (right_end - half).max(start + half)),
                    scene.surface_y + scene.px,
                ),
                k,
                back: (h >> 20).is_multiple_of(3),
            }
        })
        .collect();
    // Back row first, then the big ones behind the small.
    trees.sort_by(|a, b| {
        (!a.back, b.k, a.foot.x)
            .partial_cmp(&(!b.back, a.k, b.foot.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Forest { trees, more, sign }
}

/// The tree under `pointer`, front row first.
pub fn hit(forest: &Forest, pointer: Pos2) -> Option<&Tree> {
    forest
        .trees
        .iter()
        .rev()
        .find(|t| t.rect().shrink2(vec2(2.0 * t.k, 0.0)).contains(pointer))
}

/// FNV-1a, so a worktree's tree looks the same on every launch.
fn hash(id: &str) -> u32 {
    id.bytes().fold(0x811c_9dc5, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// A tree cut down (or whose worktree closed), falling where it stood.
#[derive(Debug, Clone)]
struct Felled {
    tree: Tree,
    start_s: f64,
    /// 1 falls to the right.
    dir: f32,
}

impl Felled {
    fn over(&self, now_s: f64) -> bool {
        now_s - self.start_s >= FALL_S + LIE_S + FADE_S
    }
}

/// The forest across frames: what the app last told it, and the trees falling.
#[derive(Default)]
pub struct ForestView {
    groves: Vec<Grove>,
    saw: Option<SawJob>,
    /// Last frame's trees by worktree id.
    shown: HashMap<String, Tree>,
    felled: Vec<Felled>,
    /// The tree whose name was clicked to stay up.
    pinned: Option<String>,
    /// Reduce motion: no shiver, fall or sawing stroke.
    still: bool,
}

impl ForestView {
    pub fn set(&mut self, groves: Vec<Grove>, saw: Option<SawJob>) {
        self.groves = groves;
        self.saw = saw;
    }

    /// This frame's trees, without those the saw has felled. Trees gone since the last frame
    /// start falling (not after a gap, `away`, when the farm was off screen).
    pub fn layout(&mut self, scene: &Scene, now_s: f64, away: bool, still: bool) -> Forest {
        self.still = still;
        let felled: HashSet<&str> = self
            .saw
            .iter()
            .flat_map(|s| s.felled.iter().map(String::as_str))
            .collect();
        let groves: Vec<Grove> = self
            .groves
            .iter()
            .filter(|g| !felled.contains(g.id.as_str()))
            .cloned()
            .collect();
        let forest = place(scene, &groves);
        let alive: HashSet<&str> = groves.iter().map(|g| g.id.as_str()).collect();
        if !away {
            for (id, tree) in &self.shown {
                if !alive.contains(id.as_str()) {
                    self.felled.push(Felled {
                        tree: tree.clone(),
                        start_s: now_s,
                        dir: if hash(id).is_multiple_of(2) {
                            1.0
                        } else {
                            -1.0
                        },
                    });
                }
            }
        }
        self.felled.retain(|f| !f.over(now_s));
        self.shown = forest
            .trees
            .iter()
            .map(|t| (t.grove.id.clone(), t.clone()))
            .collect();
        if self
            .pinned
            .as_ref()
            .is_some_and(|id| !alive.contains(id.as_str()))
        {
            self.pinned = None;
        }
        forest
    }

    /// Whether anything moves: a tree falls or the saw runs.
    pub fn animating(&self) -> bool {
        !self.felled.is_empty() || self.saw.is_some()
    }

    /// Pins the clicked tree's name up, or takes it down.
    pub fn click(&mut self, tree: &Tree) {
        let id = &tree.grove.id;
        self.pinned = (self.pinned.as_ref() != Some(id)).then(|| id.clone());
    }

    /// Roots of the standing trees, under the passages.
    pub fn paint_roots(&self, painter: &egui::Painter, sprites: &Sprites, forest: &Forest) {
        for (i, tree) in forest.trees.iter().enumerate() {
            let width = tree.rect().width() * 0.5;
            sprites.roots(painter, tree.foot, width, tree.k / 2.0, 1_000 + i);
        }
    }

    /// The trees, the falling ones and the "+N" sign: behind everything else on the surface.
    pub fn paint_trees(
        &self,
        painter: &egui::Painter,
        sprites: &Sprites,
        scene: &Scene,
        forest: &Forest,
        palette: Palette,
        now_s: f64,
    ) {
        let still = self.still;
        let cutting = self.saw.as_ref().and_then(|s| s.current.as_deref());
        for tree in &forest.trees {
            let tint = if tree.back {
                Color32::from_gray(BACK_TINT)
            } else {
                Color32::WHITE
            };
            // The tree being cut shivers.
            let shiver = if cutting == Some(tree.grove.id.as_str()) && !still {
                (now_s * 40.0).sin() as f32 * 0.025
            } else {
                0.0
            };
            sprites.deco_turned(
                painter,
                tree.grove.health.deco(),
                tree.foot,
                tree.k,
                (shiver, tree.foot),
                tint,
            );
        }
        for f in &self.felled {
            paint_felled(painter, sprites, f, now_s, still);
        }
        if let Some(at) = forest.sign {
            paint_sign(painter, scene, at, forest.more, palette);
        }
    }

    /// The hovered or pinned tree's name, and the saw: over everything else.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_overlay(
        &self,
        painter: &egui::Painter,
        sprites: &Sprites,
        scene: &Scene,
        forest: &Forest,
        hovered: Option<&str>,
        palette: Palette,
        now_s: f64,
    ) {
        let still = self.still;
        for tree in &forest.trees {
            let id = tree.grove.id.as_str();
            if hovered == Some(id) || self.pinned.as_deref() == Some(id) {
                super::label(
                    painter,
                    tree.rect().center_top() - vec2(0.0, scene.px),
                    Align2::CENTER_BOTTOM,
                    &tree.grove.name,
                    FontId::proportional((5.5 * scene.px).clamp(10.0, 14.0)),
                    12.0 * scene.cell(),
                    scene.px,
                    palette,
                );
            }
        }
        if let Some(job) = &self.saw {
            let at = job
                .current
                .as_deref()
                .and_then(|id| forest.tree(id))
                .map(|t| (t.cut(), t.k))
                .or_else(|| {
                    forest
                        .sign
                        .map(|s| (s - vec2(0.0, 3.0 * scene.px), scene.px))
                })
                .unwrap_or_else(|| {
                    let (_, end) = strip(scene);
                    (
                        pos2(end - 8.0 * scene.cell(), scene.surface_y - 3.0 * scene.px),
                        scene.px,
                    )
                });
            paint_saw(painter, sprites, scene, at, job, now_s, still);
        }
    }

    /// What the tooltip over `tree` says.
    pub fn tooltip(tree: &Tree) -> String {
        format!(
            "Worktree {}\n{}",
            tree.grove.name,
            tree.grove.health.describe()
        )
    }
}

fn paint_felled(painter: &egui::Painter, sprites: &Sprites, f: &Felled, now_s: f64, still: bool) {
    let t = now_s - f.start_s;
    let fade = (1.0 - ((t - FALL_S - LIE_S) / FADE_S).clamp(0.0, 1.0)) as f32;
    let tint = Color32::WHITE.gamma_multiply(fade);
    let stump = (Deco::Stump, f.tree.foot, f.tree.k);
    sprites.deco_turned(painter, stump.0, stump.1, stump.2, (0.0, stump.1), tint);
    if still {
        return;
    }
    // Falls like a hinge: slow at first, then fast, with a small bounce as it lands.
    let u = (t / FALL_S).clamp(0.0, 1.0) as f32;
    let bounce = if u >= 1.0 {
        let b = ((t - FALL_S) / 0.25).clamp(0.0, 1.0) as f32;
        (b * std::f32::consts::PI).sin() * 0.06
    } else {
        0.0
    };
    let angle = f.dir * (std::f32::consts::FRAC_PI_2 * u * u - bounce);
    let hinge = f.tree.cut();
    // The trunk above the cut.
    sprites.deco_turned(
        painter,
        f.tree.grove.health.deco(),
        hinge,
        f.tree.k,
        (angle, hinge),
        tint,
    );
}

/// A little wooden sign with the count of worktrees that have no tree.
fn paint_sign(painter: &egui::Painter, scene: &Scene, foot: Pos2, more: usize, palette: Palette) {
    let px = scene.px;
    let post = Color32::from_rgb(0x6b, 0x44, 0x23);
    let board = Color32::from_rgb(0xb8, 0x86, 0x4a);
    painter.rect_filled(
        Rect::from_center_size(foot - vec2(0.0, 4.0 * px), vec2(1.5 * px, 8.0 * px)),
        0.0,
        post,
    );
    let text = format!("+{more}");
    let font = FontId::proportional((6.0 * px).clamp(10.0, 14.0));
    let galley = painter.layout_no_wrap(text, font, palette.label);
    let rect = Rect::from_center_size(
        foot - vec2(0.0, 9.0 * px + galley.size().y / 2.0),
        galley.size() + vec2(4.0 * px, 2.0 * px),
    );
    painter.rect_filled(rect, px, board);
    painter.rect_stroke(
        rect,
        px,
        egui::Stroke::new(px.min(2.0), post),
        egui::StrokeKind::Inside,
    );
    painter.galley(
        rect.center() - galley.size() / 2.0,
        galley,
        Color32::from_rgb(0x2a, 0x1a, 0x0c),
    );
}

/// The chainsaw cutting in at `cut` (a trunk's middle; `k` points per texel of that tree),
/// with flying sawdust and the progress over it.
fn paint_saw(
    painter: &egui::Painter,
    sprites: &Sprites,
    scene: &Scene,
    (cut, k): (Pos2, f32),
    job: &SawJob,
    now_s: f64,
    still: bool,
) {
    let px = scene.px;
    // The saw's own scale stays readable on small trees.
    let s = k.max(1.5 * px);
    let [w, h] = Deco::Saw.size();
    let size = vec2(w as f32, h as f32) * s;
    // Bar tip at the far side of the trunk, the body to its left.
    let stroke = if still {
        0.0
    } else {
        (now_s * 9.0).sin() as f32 * 1.5 * s
    };
    let foot = cut + vec2(-size.x / 2.0 + 2.0 * k + stroke, size.y / 2.0);
    let tilt = if still {
        0.0
    } else {
        (now_s * 23.0).sin() as f32 * 0.03
    };
    sprites.deco_turned(painter, Deco::Saw, foot, s, (tilt, cut), Color32::WHITE);
    if !still {
        paint_sawdust(painter, cut, s, now_s);
    }
    let label = format!("Sawing worktrees {}/{}", job.done, job.total);
    let font = FontId::proportional((5.5 * px).clamp(11.0, 14.0));
    let at = Pos2::new(cut.x, cut.y - size.y.max(16.0 * k) - 2.0 * px);
    let shadow = Color32::from_black_alpha(160);
    let ink = Color32::from_rgb(0xf4, 0xe8, 0xc8);
    painter.text(
        at + Vec2::splat(1.0),
        Align2::CENTER_BOTTOM,
        &label,
        font.clone(),
        shadow,
    );
    painter.text(at, Align2::CENTER_BOTTOM, label, font, ink);
}

/// Wood chips thrown out under the cut.
fn paint_sawdust(painter: &egui::Painter, cut: Pos2, s: f32, now_s: f64) {
    let chip = Color32::from_rgb(0xe8, 0xc8, 0x8a);
    for i in 0..10 {
        let life = (now_s * 2.2 + i as f64 / 10.0).fract() as f32;
        let seed = (i as f32 * 2.7).sin();
        let vx = (1.0 + seed.abs()) * 6.0 * s * if i % 2 == 0 { 1.0 } else { 0.6 };
        let vy = -(2.0 + seed * 1.5) * s;
        let gravity = 9.0 * s;
        let at = cut + vec2(vx * life, vy * life + gravity * life * life);
        let alpha = ((1.0 - life) * 230.0) as u8;
        painter.rect_filled(
            Rect::from_center_size(at, Vec2::splat(s * 0.8)),
            0.0,
            Color32::from_rgba_unmultiplied(chip.r(), chip.g(), chip.b(), alpha),
        );
    }
}
