//! Excavation spoil on the mountainside: one heap per linked worktree, oldest first.
//! Fresh stone and a small survey pennant mark active work; abandoned heaps are weathered.
//! Cleanup strikes with a hammer, then the removed heap crumbles and fades.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Vec2, pos2, vec2};

use super::layout::Scene;
use super::sprites::{Palette, Sprites};
use super::world::{
    Canvas, GOLD, INK, Prop, STONE, STONE_DARK, STONE_LIGHT, WOOD, WOOD_DARK, WOOD_LIGHT, pts, rgb,
};
use crate::model::{Node, NodeState};
use crate::orca::spawn::Worktree;

/// How long a cleared heap takes to collapse, settle and fade, in seconds.
const CRUMBLE_S: f64 = 1.0;
const SETTLE_S: f64 = 0.9;
const FADE_S: f64 = 0.8;
/// Worktrees that fill the surface's strip end to end: the rubble grows from the left like
/// a loading bar, and past this many it only gets denser.
pub const FILL_AT: usize = 40;
/// Most heaps drawn; the rest go on a "+N" sign.
pub const MAX_HEAPS: usize = 160;
/// Texels of a map per farm texel of the biggest heap.
pub const FULL: f32 = 1.65;
/// The biggest heap spans at most this many heaps' spacing.
const DENSE_SPAN: f32 = 4.0;
/// The smallest heap, as a share of the biggest.
const SMALLEST: f32 = 0.5;
/// Sky kept clear above the tallest heap, in farm texels.
const HEADROOM: f32 = 6.0;
/// The back row is drawn darker.
const BACK_TINT: u8 = 165;

/// What is in a worktree, as its heap shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Health {
    /// An agent works in it, or waits on Karl.
    Active,
    /// Its agents are idle, done, failed or parked.
    Resting,
    /// No agent is in it.
    Empty,
}

impl Health {
    fn variant(self) -> u8 {
        match self {
            Self::Active => 0,
            Self::Resting => 1,
            Self::Empty => 2,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Health::Active => "an agent is working in it",
            Health::Resting => "its agents are idle, done or parked",
            Health::Empty => "no agent in it: may be eligible for cleanup",
        }
    }
}

/// One open worktree, as a heap.
#[derive(Debug, Clone, PartialEq)]
pub struct Site {
    pub id: String,
    pub name: String,
    pub health: Health,
}

/// The cleanup at work: its progress, the worktree being cleared and the ones already removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupJob {
    pub done: usize,
    pub total: usize,
    /// Id of the worktree being removed now.
    pub current: Option<String>,
    /// Ids removed so far: their heaps crumble right away, before the next poll drops them.
    pub cleared: Vec<String>,
}

/// The heaps of the linked worktrees (main checkouts and archived ones grow none), oldest
/// first, each with its [`Health`] from the agents in `nodes` whose worktree it is.
pub fn sites(worktrees: &[Worktree], nodes: &[Node]) -> Vec<Site> {
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
            Health::Active
        } else {
            Health::Resting
        };
        let entry = agents.entry(trim(path)).or_insert(health);
        if health == Health::Active {
            *entry = Health::Active;
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
        .map(|w| Site {
            id: w.id.clone(),
            name: w.name().to_string(),
            health: agents.get(&trim(&w.path)).copied().unwrap_or(Health::Empty),
        })
        .collect()
}

/// A heap where it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct Heap {
    pub site: Site,
    /// The middle of its heap base, on the rock.
    pub foot: Pos2,
    /// Screen points per texel of its map.
    pub k: f32,
    /// In the back row: drawn first and darker.
    pub back: bool,
}

impl Heap {
    fn prop(&self) -> Prop {
        Prop::Rubble(self.site.health.variant(), (hash(&self.site.id) % 4) as u8)
    }
    pub fn rect(&self) -> Rect {
        let [w, h] = self.prop().size();
        let size = vec2(w as f32, h as f32) * self.k;
        Rect::from_min_size(self.foot - vec2(size.x / 2.0, size.y), size)
    }

    /// The upper rock: where the hammer strikes.
    fn impact(&self) -> Pos2 {
        self.foot - vec2(0.0, 14.0 * self.k)
    }
}

/// The heaps on the surface, and those that did not fit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rubble {
    /// In draw order: back row first, then left to right.
    pub heaps: Vec<Heap>,
    /// Worktrees with no room for a heap (the newest).
    pub more: usize,
    /// Where the "+N" sign stands when some did not fit.
    pub sign: Option<Pos2>,
}

impl Rubble {
    pub fn heap(&self, id: &str) -> Option<&Heap> {
        self.heaps.iter().find(|t| t.site.id == id)
    }
}

/// The strip of surface the rubble grows on: the whole width, behind everything else there.
fn strip(scene: &Scene) -> (f32, f32) {
    let a = scene.cell();
    (scene.rect.left() + 0.5 * a, scene.rect.right() - 0.5 * a)
}

/// Points per map texel of a heap at `scale` farm texels each, shrunk so it fits under the sky.
pub fn texel(scene: &Scene, scale: f32) -> f32 {
    let [_, heap_h] = Prop::Rubble(0, 0).size();
    let sky = scene.surface_y - scene.rect.top() - HEADROOM * scene.px;
    (scale * scene.px).min(sky / heap_h as f32).max(scene.px)
}

/// Where each site's heap stands: oldest at the left, the rubble reaching further right the
/// more worktrees there are (all the way at [`FILL_AT`]), each heap a size and a nudge of its
/// own from a hash of its id, so they cluster like spoil heaps. Past [`MAX_HEAPS`] the newest go
/// uncounted into a "+N" sign at the right end.
pub fn place(scene: &Scene, sites: &[Site]) -> Rubble {
    let (start, end) = strip(scene);
    let [heap_w, _] = Prop::Rubble(0, 0).size();
    let room = sites.len().min(MAX_HEAPS);
    // Dense spoil is drawn smaller, so its heaps stay apart enough to count.
    let spacing = (end - start) / room.max(FILL_AT) as f32;
    let full = texel(scene, FULL).min((spacing * DENSE_SPAN / heap_w as f32).max(2.0 * scene.px));
    let more = sites.len() - room;
    let sign = (more > 0).then(|| pos2(end - 1.5 * scene.cell(), scene.surface_y + scene.px));
    let right_end = if more > 0 {
        end - 3.0 * scene.cell()
    } else {
        end
    };
    if room == 0 || right_end <= start {
        return Rubble {
            more: sites.len(),
            sign,
            ..Rubble::default()
        };
    }
    // Heaps run from where the biggest silhouette clears the strip's left end.
    let left = (start + heap_w as f32 * full / 2.0).min(right_end);
    let fill = (room as f32 / FILL_AT as f32).min(1.0);
    let reach = left + (right_end - heap_w as f32 * full * SMALLEST / 2.0 - left).max(0.0) * fill;
    let gap = (reach - left) / room as f32;
    let mut heaps: Vec<Heap> = sites
        .iter()
        .take(room)
        .enumerate()
        .map(|(i, site)| {
            let h = hash(&site.id);
            let size = SMALLEST + (1.0 - SMALLEST) * (h % 1000) as f32 / 999.0;
            let k = full * size;
            let half = heap_w as f32 * k / 2.0;
            let nudge = ((h >> 10) % 1000) as f32 / 999.0 - 0.5;
            let x = left + (i as f32 + 0.5 + 0.6 * nudge) * gap;
            Heap {
                site: site.clone(),
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
    heaps.sort_by(|a, b| {
        (!a.back, b.k, a.foot.x)
            .partial_cmp(&(!b.back, a.k, b.foot.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Rubble { heaps, more, sign }
}

/// The heap under `pointer`, front row first.
pub fn hit(rubble: &Rubble, pointer: Pos2) -> Option<&Heap> {
    rubble
        .heaps
        .iter()
        .rev()
        .find(|t| t.rect().shrink2(vec2(2.0 * t.k, 0.0)).contains(pointer))
}

/// FNV-1a, so a worktree's heap looks the same on every launch.
fn hash(id: &str) -> u32 {
    id.bytes().fold(0x811c_9dc5, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// A heap broken down (or whose worktree closed), crumbling where it stood.
#[derive(Debug, Clone)]
struct Cleared {
    heap: Heap,
    start_s: f64,
    /// 1 throws chips to the right.
    dir: f32,
}

impl Cleared {
    fn over(&self, now_s: f64) -> bool {
        now_s - self.start_s >= CRUMBLE_S + SETTLE_S + FADE_S
    }
}

/// The rubble across frames: what the app last told it, and the heaps crumbling.
#[derive(Default)]
pub struct RubbleView {
    sites: Vec<Site>,
    hammer: Option<CleanupJob>,
    /// Last frame's heaps by worktree id.
    shown: HashMap<String, Heap>,
    cleared: Vec<Cleared>,
    /// The heap whose name was clicked to stay up.
    pinned: Option<String>,
    /// Reduce motion: no shiver, collapse or hammer swing.
    still: bool,
}

impl RubbleView {
    pub fn set(&mut self, sites: Vec<Site>, hammer: Option<CleanupJob>) {
        self.sites = sites;
        self.hammer = hammer;
    }

    /// This frame's heaps, without those the hammer has cleared. Heaps gone since the last frame
    /// start crumbling (not after a gap, `away`, when the farm was off screen).
    pub fn layout(&mut self, scene: &Scene, now_s: f64, away: bool, still: bool) -> Rubble {
        self.still = still;
        let cleared: HashSet<&str> = self
            .hammer
            .iter()
            .flat_map(|s| s.cleared.iter().map(String::as_str))
            .collect();
        let sites: Vec<Site> = self
            .sites
            .iter()
            .filter(|g| !cleared.contains(g.id.as_str()))
            .cloned()
            .collect();
        let rubble = place(scene, &sites);
        let alive: HashSet<&str> = sites.iter().map(|g| g.id.as_str()).collect();
        if !away {
            for (id, heap) in &self.shown {
                if !alive.contains(id.as_str()) {
                    self.cleared.push(Cleared {
                        heap: heap.clone(),
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
        self.cleared.retain(|f| !f.over(now_s));
        self.shown = rubble
            .heaps
            .iter()
            .map(|t| (t.site.id.clone(), t.clone()))
            .collect();
        if self
            .pinned
            .as_ref()
            .is_some_and(|id| !alive.contains(id.as_str()))
        {
            self.pinned = None;
        }
        rubble
    }

    /// Whether anything moves: a heap crumbles or the hammer swings.
    pub fn animating(&self) -> bool {
        !self.cleared.is_empty() || self.hammer.is_some()
    }

    /// Pins the clicked heap's name up, or takes it down.
    pub fn click(&mut self, heap: &Heap) {
        let id = &heap.site.id;
        self.pinned = (self.pinned.as_ref() != Some(id)).then(|| id.clone());
    }

    /// The heaps, the crumbling ones and the "+N" sign: behind everything else on the surface.
    pub fn paint_heaps(
        &self,
        painter: &egui::Painter,
        sprites: &Sprites,
        scene: &Scene,
        rubble: &Rubble,
        palette: Palette,
        now_s: f64,
    ) {
        let still = self.still;
        let cutting = self.hammer.as_ref().and_then(|s| s.current.as_deref());
        for heap in &rubble.heaps {
            let alpha = if heap.back {
                f32::from(BACK_TINT) / 255.0
            } else {
                1.0
            };
            let phase = super::hammer::phase(now_s, still);
            let shiver = if cutting == Some(heap.site.id.as_str())
                && !still
                && (0.68..0.83).contains(&phase)
            {
                ((phase - 0.68) * 110.0).sin() * scene.px
            } else {
                0.0
            };
            sprites.prop(
                painter,
                heap.prop(),
                heap.rect().translate(vec2(shiver, 0.0)),
                alpha,
            );
        }
        for f in &self.cleared {
            paint_cleared(painter, sprites, f, now_s, still);
        }
        if let Some(at) = rubble.sign {
            paint_sign(painter, scene, at, rubble.more, palette);
        }
    }

    /// The hovered or pinned heap's name, and the hammer: over everything else.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_overlay(
        &self,
        painter: &egui::Painter,
        sprites: &Sprites,
        scene: &Scene,
        rubble: &Rubble,
        hovered: Option<&str>,
        palette: Palette,
        now_s: f64,
    ) {
        let still = self.still;
        for heap in &rubble.heaps {
            let id = heap.site.id.as_str();
            if hovered == Some(id) || self.pinned.as_deref() == Some(id) {
                super::label(
                    painter,
                    heap.rect().center_top() - vec2(0.0, scene.px),
                    Align2::CENTER_BOTTOM,
                    &heap.site.name,
                    FontId::proportional((5.5 * scene.px).clamp(10.0, 14.0)),
                    12.0 * scene.cell(),
                    scene.px,
                    palette,
                );
            }
        }
        if let Some(job) = &self.hammer {
            let at = job
                .current
                .as_deref()
                .and_then(|id| rubble.heap(id))
                .map(|t| (t.impact(), t.k))
                .or_else(|| {
                    rubble
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
            paint_hammer(painter, sprites, scene, at, job, now_s, still);
        }
    }

    /// What the tooltip over `heap` says.
    pub fn tooltip(heap: &Heap) -> String {
        format!(
            "Worktree {}\n{}",
            heap.site.name,
            heap.site.health.describe()
        )
    }
}

fn paint_cleared(painter: &egui::Painter, sprites: &Sprites, f: &Cleared, now_s: f64, still: bool) {
    let t = now_s - f.start_s;
    let fade = (1.0 - ((t - CRUMBLE_S - SETTLE_S) / FADE_S).clamp(0.0, 1.0)) as f32;
    let u = if still {
        1.0
    } else {
        (t / CRUMBLE_S).clamp(0.0, 1.0) as f32
    };
    let size = f.heap.rect().size() * vec2(1.0 + 0.18 * u, 1.0 - 0.78 * u);
    let rect = Rect::from_min_size(f.heap.foot - vec2(size.x / 2.0, size.y), size);
    sprites.prop(painter, f.heap.prop(), rect, fade);
    if !still && u < 1.0 {
        paint_chips(painter, f.heap.impact(), f.heap.k, u, f.dir);
    }
}

/// A little wooden sign with the count of worktrees that have no heap.
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

/// A generated sledgehammer swings into the heap; chips appear only on impact.
fn paint_hammer(
    painter: &egui::Painter,
    sprites: &Sprites,
    scene: &Scene,
    (cut, _k): (Pos2, f32),
    job: &CleanupJob,
    now_s: f64,
    still: bool,
) {
    let phase = super::hammer::phase(now_s, still);
    let scale = scene.px * 0.9;
    let (rect, turn) = super::hammer::placement(cut, scale, phase);
    sprites.prop_turned(painter, Prop::CleanupHammer, rect, turn, 1.0);
    if !still && phase >= 0.68 {
        paint_chips(painter, cut, scene.px, (phase - 0.68) / 0.32, 1.0);
    }
    super::label(
        painter,
        cut - vec2(0.0, 26.0 * scene.px),
        Align2::CENTER_BOTTOM,
        &format!("Clearing worktrees {}/{}", job.done, job.total),
        FontId::proportional((5.5 * scene.px).clamp(11.0, 14.0)),
        12.0 * scene.cell(),
        scene.px,
        Palette::DAY,
    );
}

/// Broken granite and a couple of pale impact flecks, following one ballistic burst.
fn paint_chips(painter: &egui::Painter, cut: Pos2, s: f32, life: f32, dir: f32) {
    for i in 0..10 {
        let seed = (i as f32 * 2.7).sin();
        let vx = seed * 16.0 * s * dir;
        let vy = -(6.0 + (seed * 1.5).abs()) * s;
        let at = cut + vec2(vx * life, vy * life + 12.0 * s * life * life);
        let color = if i < 2 { STONE_LIGHT } else { STONE };
        painter.rect_filled(
            Rect::from_center_size(at, Vec2::splat(s * 1.1)),
            0.0,
            color.gamma_multiply(1.0 - life),
        );
    }
}

/// Soil, freshly fractured granite, snapped roots and discarded shoring. Variants share
/// their silhouette bounds so the worktree hit target and collapse remain stable.
pub(super) fn draw(c: &mut Canvas, health: u8, seed: u8) {
    let shift = f32::from(seed % 4);
    let earth = rgb(0x695442);
    c.cut(
        pts(&[
            (1.0, 20.0),
            (4.0, 16.0),
            (8.0, 13.0),
            (13.0, 9.0),
            (19.0, 10.0),
            (25.0, 13.0),
            (30.0, 15.0),
            (35.0, 20.0),
            (31.0, 21.0),
            (5.0, 21.0),
        ]),
        earth,
    );
    c.paint(
        pts(&[
            (3.0, 20.0),
            (12.0, 15.0),
            (18.0, 14.0),
            (24.0, 18.0),
            (34.0, 20.0),
        ]),
        WOOD_DARK,
    );
    // A splintered beam, with grain and a bent iron nail, partly buried by the rocks.
    c.cut(
        pts(&[
            (5.0, 10.0 + shift),
            (7.0, 8.0 + shift),
            (29.0, 17.0),
            (27.0, 18.0),
            (28.0, 20.0),
            (23.0, 17.0),
        ]),
        WOOD,
    );
    c.line((8.0, 10.0 + shift), (25.0, 17.0), 0.45, WOOD_LIGHT);
    c.line((25.0, 16.0), (25.0, 13.0), 0.55, INK);
    c.line((25.0, 13.0), (27.0, 12.0), 0.55, INK);
    for (i, (x, y, r)) in [
        (12.0, 13.0, 5.0),
        (21.0, 11.0, 6.0),
        (27.0, 16.0, 4.0),
        (6.0, 18.0, 2.8),
        (16.0, 18.0, 3.4),
    ]
    .into_iter()
    .enumerate()
    {
        let x = x + if i % 2 == 0 {
            shift * 0.4
        } else {
            -shift * 0.3
        };
        let color = if health == 2 { STONE_DARK } else { STONE };
        c.cut(
            pts(&[
                (x - r, y),
                (x - r * 0.7, y - r * 0.7),
                (x + r * 0.2, y - r),
                (x + r, y - r * 0.2),
                (x + r * 0.65, y + r * 0.65),
                (x - r * 0.6, y + r * 0.5),
            ]),
            color,
        );
        c.paint(
            pts(&[
                (x - r, y),
                (x - r * 0.7, y - r * 0.7),
                (x + r * 0.2, y - r),
                (x, y),
            ]),
            if health == 2 { STONE } else { STONE_LIGHT },
        );
        c.paint(
            pts(&[
                (x, y),
                (x + r, y - r * 0.2),
                (x + r * 0.65, y + r * 0.65),
                (x - r * 0.6, y + r * 0.5),
            ]),
            STONE_DARK,
        );
        if i == 1 {
            c.line((x - 1.0, y - r * 0.6), (x + 1.0, y + 1.0), 0.45, INK);
        }
    }
    c.line((3.0, 19.0), (10.0, 16.0), 0.8, WOOD_DARK);
    c.line((8.0, 17.0), (7.0, 14.0), 0.6, WOOD_DARK);
    for (x, y) in [(11.0, 20.0), (30.0, 20.0), (32.0, 18.0), (19.0, 21.0)] {
        c.detail(x, y, 1.0, 0.8, STONE_LIGHT);
    }
    // A survey pennant means this worktree still has agents; no pennant means empty.
    if health != 2 {
        c.line((8.0, 14.0), (7.0, 2.0), 0.7, WOOD_DARK);
        c.cut(
            pts(&[(7.0, 2.0), (13.0, 3.0), (11.0, 5.0), (7.0, 5.0)]),
            if health == 0 { GOLD } else { rgb(0x7b7a67) },
        );
    }
}
