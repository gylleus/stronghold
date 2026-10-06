//! Where everything in the ant farm goes: a pure function of the snapshot and the view's
//! rect, so the state → position mapping is testable without drawing.
//!
//! The farm is a side view: sky above the surface, the "Karl" landing on the surface at the
//! left, the Queen's chamber under the surface in the middle, the storage chamber at the
//! right, a gallery running under them and one tunnel per run (or per repo for loose
//! terminals) dropping from the gallery. A coordinator stands at its tunnel's mouth, its
//! workers in the chamber at the tunnel's end. When the ants can be drawn bigger that way,
//! tunnels pair up: a chamber over another in one column, the lower one's shaft passing
//! beside the upper chamber. Parked agents sleep in beds in the last tunnel's chamber, the
//! sleeping room, there only while someone is parked.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

use crate::model::{
    EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RepoIndex, dispatch_ended,
};

/// Side of a sprite cell in texels.
pub const CELL: f32 = 16.0;
/// Karl's pipe in texels: the lip's width and the height above the surface.
pub const PIPE_LIP_W: f32 = 22.0;
pub const PIPE_H: f32 = 24.0;
/// Characters of a tunnel label before it is cut.
const LABEL_CHARS: usize = 28;
/// Tunnel depth, as a share of the room below the gallery, for a run without task counts.
const UNKNOWN_PROGRESS_DEPTH: f32 = 0.6;
/// Shallowest tunnel, as a share of the room below the gallery (a run with nothing done).
const MIN_DEPTH: f32 = 0.35;
/// Distance between rows of ants, in cells: room for a row's bubbles above and name tags
/// below.
const ROW: f32 = 2.5;
/// Width of an ant's slot in a row, in cells: room for its name tag.
const SLOT: f32 = 2.4;
/// Room under an ant for its name tag, in cells.
const TAG: f32 = 0.85;
/// Width beside an upper chamber for the shaft down to the chamber under it, in cells.
const SHAFT_GAP: f32 = 1.8;
/// A bed in texels: its size, and the mattress top's height above the floor.
pub const BED_W: f32 = 20.0;
pub const BED_H: f32 = 9.0;
pub const MATTRESS_TOP: f32 = 6.0;
/// Texels a sleeper is raised to lie on its bed's mattress, under the blanket.
pub const SLEEPER_LIFT: f32 = 2.0;

/// What a tunnel belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TunnelKey {
    Run(String),
    /// Loose terminals of a repo (see [`crate::model::node_repo`]).
    Repo(String),
    /// Parked agents, asleep ([`crate::park`]).
    SleepingRoom,
}

/// A logical place an ant occupies. An ant walks when its spot changes; a spot keeping its
/// place while the view resizes does not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Spot {
    Queen,
    Comms,
    /// On the surface waiting for Karl, the nth in line.
    Landing(usize),
    /// In the storage chamber with its crumb, the nth on the pile.
    Storage(usize),
    /// At the mouth of a tunnel (coordinators and attendants).
    Mouth(usize, usize),
    /// In the chamber at a tunnel's end.
    Chamber(usize, usize),
    /// Trailing its parent, the nth behind it (subagents).
    Trail(NodeId, usize),
}

/// The area a spot is in; ants walk along the gallery between areas and straight within one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Area {
    Queen,
    Comms,
    Landing,
    Storage,
    Tunnel(usize),
    Trail,
}

impl Spot {
    pub fn area(&self) -> Area {
        match self {
            Spot::Queen => Area::Queen,
            Spot::Comms => Area::Comms,
            Spot::Landing(_) => Area::Landing,
            Spot::Storage(_) => Area::Storage,
            Spot::Mouth(t, _) | Spot::Chamber(t, _) => Area::Tunnel(*t),
            Spot::Trail(..) => Area::Trail,
        }
    }
}

/// What an ant is doing, which picks its animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pose {
    /// Standing at its post (a coordinator at work).
    Stand,
    /// Digging at the chamber's face (working: reading, searching, thinking).
    Dig,
    /// Hammering in a hard hat at the chamber's face (editing or building).
    Build,
    /// Lying still (idle or stalled).
    Sleep,
    /// Waving on the landing for Karl.
    Wait,
    /// Holding its crumb in storage.
    Carry,
    /// Red and still.
    Failed,
    Queen,
}

/// The bubble over an ant, so its state does not rely on pose or color alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Waiting on Karl.
    Alert,
    /// Idle.
    Sleep,
    /// Stalled: the whip pokes it.
    Whip,
    Failed,
    /// Done with a PR ready for review.
    PrReady,
}

/// One ant to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Ant {
    pub id: NodeId,
    pub kind: NodeKind,
    pub state: NodeState,
    pub spot: Spot,
    /// Center of the ant's cell, in screen points.
    pub pos: Pos2,
    /// Side of the ant's cell in points.
    pub size: f32,
    pub pose: Pose,
    pub mark: Option<Mark>,
    /// Faces right (towards the dig face); the sprites face left.
    pub facing_right: bool,
    /// Opacity, for released workers and finished subagents.
    pub alpha: f32,
    /// Name shown under the ant (its node label, or a coordinator's run away from its
    /// tunnel); `None` for the Queen, coordinators at their tunnel's mouth (the tunnel is
    /// labelled) and subagents.
    pub tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tunnel {
    pub key: TunnelKey,
    pub label: String,
    /// The shaft from the gallery down to the chamber.
    pub shaft: Rect,
    /// The chamber at the tunnel's end.
    pub chamber: Rect,
    /// Completed share of the run's tasks, when known.
    pub progress: Option<f32>,
    /// `(completed, total)` tasks of the run, when known.
    pub tasks: Option<(usize, usize)>,
    /// The middle of the top of the tunnel's label.
    pub label_top: Pos2,
}

/// A bed in the sleeping room, with the parked ant lying on it, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Bed {
    pub rect: Rect,
    pub sleeper: Option<NodeId>,
}

/// The Envoy's post and its link to the surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommsRoom {
    pub chamber: Rect,
    pub shaft: Rect,
    pub dish: Pos2,
    pub px: f32,
}

impl CommsRoom {
    pub fn ant_position(&self) -> Pos2 {
        self.chamber.min + vec2(28.0, 31.0) * self.px
    }

    pub fn feed(&self) -> Pos2 {
        self.dish + vec2(3.0, -24.0) * self.px
    }

    pub fn console(&self) -> Pos2 {
        pos2(
            self.chamber.right() - 32.0 * self.px,
            self.chamber.top() + 23.0 * self.px,
        )
    }

    pub fn tray_rect(&self) -> Rect {
        Rect::from_min_size(
            self.chamber.min + vec2(61.0, 20.0) * self.px,
            vec2(17.0, 25.0) * self.px,
        )
    }

    pub fn cable_port(&self) -> Pos2 {
        self.console() + vec2(-8.0, 2.0) * self.px
    }

    pub fn cable_elbow(&self) -> Pos2 {
        pos2(self.dish.x, self.cable_port().y)
    }
}

/// The farm laid out in a rect.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    pub rect: Rect,
    /// Screen points per sprite texel; whole, so pixel art stays crisp.
    pub px: f32,
    pub surface_y: f32,
    /// The passage under the surface linking the chambers and tunnel mouths.
    pub gallery: Rect,
    /// Where ants waiting on Karl stand, on the surface.
    pub landing: Rect,
    pub queen_chamber: Rect,
    pub comms: Option<CommsRoom>,
    pub storage: Rect,
    pub tunnels: Vec<Tunnel>,
    pub beds: Vec<Bed>,
    /// In draw order: parents before the subagents trailing them.
    pub ants: Vec<Ant>,
    /// Ants with a PR ready for review: one coin each on the storage pile.
    pub pr_ready: usize,
}

impl Scene {
    /// Side of a normal ant's cell in points.
    pub fn cell(&self) -> f32 {
        CELL * self.px
    }

    /// The middle of the base of Karl's pipe on the surface, where his messages come out.
    pub fn karl_pipe(&self) -> Pos2 {
        let left = if self.comms.is_some() {
            self.landing.right()
        } else {
            self.rect.left()
        };
        pos2(
            left + 0.3 * self.cell() + PIPE_LIP_W / 2.0 * self.px,
            self.surface_y,
        )
    }

    /// The middle of the pipe's mouth, its top `stretch` texels higher while it pops one out.
    pub fn karl_pipe_mouth(&self, stretch: f32) -> Pos2 {
        self.karl_pipe() - vec2(0.0, (PIPE_H + stretch) * self.px)
    }

    pub fn ant(&self, id: &NodeId) -> Option<&Ant> {
        self.ants.iter().find(|a| &a.id == id)
    }
}

/// What the farm is drawn from: the graph's snapshot and its filters.
pub struct FarmInput<'a> {
    pub snapshot: &'a FleetSnapshot,
    /// Nodes filtered out; they are not drawn.
    pub hidden: &'a BTreeSet<NodeId>,
    pub pr_ready: &'a HashSet<NodeId>,
    /// Nodes whose agent is editing or building: their diggers build instead.
    pub building: &'a HashSet<NodeId>,
    pub now_ms: i64,
}

/// How many ants the farm's places hold, which decides how big they can be drawn.
#[derive(Debug, Clone, Copy, Default)]
pub struct Crowd {
    pub tunnels: usize,
    /// Ants in the fullest chamber.
    pub chamber: usize,
    pub landing: usize,
    pub storage: usize,
    pub envoy: bool,
}

struct Band {
    surface: f32,
    landing_left: f32,
    landing_right: f32,
    landing_per_row: usize,
    storage_per_row: usize,
    storage_h: f32,
    comms_px: f32,
    comms_h: f32,
    queen_offset: f32,
}

impl Band {
    fn gallery_top(&self, a: f32) -> f32 {
        self.surface
            + 0.4 * a
            + (2.4 * a + self.queen_offset)
                .max(self.storage_h)
                .max(self.comms_h)
            + 0.6 * a
    }
}

// Fitting and placement share the same reservations for the dish, room and waiting ants.
fn band(size: Vec2, a: f32, crowd: Crowd) -> Band {
    let rows = |n: usize, per_row: usize| n.div_ceil(per_row).max(1) as f32;
    let comms_px = if crowd.envoy {
        (a / CELL)
            .max(2.0)
            .min((size.x / (10.0 * CELL)).floor().max(1.0))
    } else {
        0.0
    };
    let b = CELL * comms_px;
    let landing_left = if crowd.envoy {
        5.5 * b + 1.5 * a
    } else {
        2.5 * a
    };
    let landing_right = if crowd.envoy {
        (size.x * 0.4).max(landing_left + SLOT * a)
    } else {
        (size.x * 0.3).max(5.0 * a)
    };
    let landing_per_row = (((landing_right - landing_left) / (SLOT * a)).floor() as usize).max(1);
    let landing_h = (rows(crowd.landing, landing_per_row) - 1.0) * ROW * a + 1.4 * a;
    let surface = (2.4 * a)
        .max(size.y * 0.16)
        .max(landing_h + 0.8 * a)
        .max(58.0 * comms_px);
    let storage_w = (size.x * 0.26).max(3.0 * a);
    let storage_per_row = ((storage_w / (SLOT * a)).floor() as usize).max(1);
    let storage_h =
        ((rows(crowd.storage, storage_per_row) - 1.0) * ROW * a + (2.2 + TAG) * a).max(2.4 * a);
    let comms_h = 3.0 * b;
    // Narrow panes stack the Queen below the comms room instead of squeezing either room.
    let queen_offset = if crowd.envoy && size.x * 0.5 - 2.0 * a < 5.5 * b + 0.5 * a {
        comms_h + 0.6 * a
    } else {
        0.0
    };
    Band {
        surface,
        landing_left,
        landing_right,
        landing_per_row,
        storage_per_row,
        storage_h,
        comms_px,
        comms_h,
        queen_offset,
    }
}

/// Ants that fit side by side in a chamber `width` wide: slots need their gap only between
/// them.
fn chamber_per_row(width: f32, a: f32) -> usize {
    (((width + (SLOT - 2.0) * a) / (SLOT * a)).floor() as usize).max(1)
}

/// Height of a chamber holding `n` slots, `per_row` to a row.
fn chamber_height(n: usize, per_row: usize, a: f32) -> f32 {
    rows_height(n.div_ceil(per_row), a)
}

fn rows_height(rows: usize, a: f32) -> f32 {
    (rows.max(1) as f32 - 1.0) * ROW * a + (1.4 + TAG) * a
}

/// Chamber slots an ant takes with `trail` subagents behind it, each `sub` cells wide: its
/// own, and enough empty ones behind it to hold the trail.
fn footprint(trail: usize, sub: f32) -> usize {
    1 + (trail as f32 * sub / SLOT).ceil() as usize
}

/// `(row, slot)` of each footprint packed into rows of `per_row` slots, in order, and the
/// number of rows. A footprint that does not fit what is left of a row starts the next.
fn pack(footprints: &[usize], per_row: usize) -> (Vec<(usize, usize)>, usize) {
    let (mut row, mut slot) = (0, 0);
    let places = footprints
        .iter()
        .map(|&f| {
            let f = f.min(per_row);
            if slot > 0 && slot + f > per_row {
                (row, slot) = (row + 1, 0);
            }
            let at = (row, slot);
            slot += f;
            at
        })
        .collect();
    (places, row + 1)
}

/// Points per texel for a view of `size` and the levels of chambers: the biggest whole
/// scale that leaves each tunnel room for an ant and its name tag, fits the crowds on the
/// landing, in storage and in the fullest chamber, and packs that chamber side by side: no
/// more rows than ants to a row. When no scale packs it, the smallest that fits, which packs
/// it best. Chambers stack two to a column only when that draws the ants bigger or packs
/// them where one level cannot.
pub fn pixel_scale(size: Vec2, crowd: Crowd) -> (f32, usize) {
    // The scale for `levels` and whether the fullest chamber is packed at it.
    let best = |levels: usize| {
        let column = size.x / crowd.tunnels.div_ceil(levels).max(1) as f32;
        let fits = |px: f32| {
            let a = CELL * px;
            let gap = if levels > 1 { SHAFT_GAP * a } else { 0.0 };
            let per_row = chamber_per_row(column - 0.6 * a - gap, a);
            let band = band(size, a, crowd);
            let gallery_bottom = band.gallery_top(a) + 1.2 * a;
            let chambers = levels as f32 * (a + chamber_height(crowd.chamber, per_row, a));
            let bottom = gallery_bottom + chambers + 0.3 * a;
            let fits = column >= (SLOT + 0.6) * a + gap
                && bottom <= size.y
                && (!crowd.envoy || px <= (size.x / (18.0 * CELL)).floor().max(1.0));
            (fits, crowd.chamber.div_ceil(per_row) <= per_row)
        };
        let fitting: Vec<(f32, bool)> = [4.0, 3.0, 2.0, 1.0]
            .into_iter()
            .filter_map(|px| match fits(px) {
                (true, packed) => Some((px, packed)),
                (false, _) => None,
            })
            .collect();
        fitting
            .iter()
            .find(|(_, packed)| *packed)
            .or(fitting.last())
            .copied()
    };
    let one = best(1);
    let two = (crowd.tunnels > 1).then(|| best(2)).flatten();
    match (one, two) {
        (Some((px1, packed1)), Some((px2, packed2)))
            if px2 > px1 || (px2 == px1 && packed2 && !packed1) =>
        {
            (px2, 2)
        }
        (None, Some((px2, _))) => (px2, 2),
        (Some((px1, _)), _) => (px1, 1),
        (None, None) => (1.0, 1),
    }
}

/// Where an ant of `kind` in `state` belongs, before slots are assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Home {
    Queen,
    Comms,
    Landing,
    Storage,
    Mouth,
    Chamber,
    Trail,
}

/// The state → place rule: waiting on Karl goes to the landing, done (or idle with a PR ready
/// for review) goes to storage, all else stays at its post (a coordinator at the mouth, a
/// worker in the chamber), failed ones turning red there. Subagents trail their parent
/// whatever their state.
pub fn home(kind: NodeKind, state: NodeState, pr_ready: bool) -> Home {
    match kind {
        NodeKind::Queen => return Home::Queen,
        NodeKind::Envoy => return Home::Comms,
        NodeKind::Subagent => return Home::Trail,
        NodeKind::Coordinator | NodeKind::Attendant | NodeKind::Worker | NodeKind::Terminal => {}
    }
    match state {
        NodeState::NeedsInput => Home::Landing,
        NodeState::Done => Home::Storage,
        NodeState::Idle if pr_ready => Home::Storage,
        NodeState::Working | NodeState::Idle | NodeState::Stalled | NodeState::Failed => {
            if matches!(kind, NodeKind::Coordinator | NodeKind::Attendant) {
                Home::Mouth
            } else {
                Home::Chamber
            }
        }
    }
}

pub fn pose(kind: NodeKind, state: NodeState, home: Home) -> Pose {
    if kind == NodeKind::Envoy {
        return Pose::Stand;
    }
    if kind == NodeKind::Queen {
        return Pose::Queen;
    }
    match state {
        NodeState::NeedsInput => Pose::Wait,
        NodeState::Done => Pose::Carry,
        NodeState::Idle if home == Home::Storage => Pose::Carry,
        NodeState::Failed => Pose::Failed,
        NodeState::Idle | NodeState::Stalled => Pose::Sleep,
        NodeState::Working if home == Home::Chamber => Pose::Dig,
        NodeState::Working => Pose::Stand,
    }
}

pub fn mark(state: NodeState, pr_ready: bool) -> Option<Mark> {
    match state {
        NodeState::NeedsInput => Some(Mark::Alert),
        NodeState::Idle if pr_ready => Some(Mark::PrReady),
        NodeState::Idle => Some(Mark::Sleep),
        NodeState::Stalled => Some(Mark::Whip),
        NodeState::Failed => Some(Mark::Failed),
        NodeState::Done | NodeState::Working => pr_ready.then_some(Mark::PrReady),
    }
}

/// The tunnel a node digs in: its run's, else its repo's.
fn tunnel_key(node: &Node, repos: &HashMap<NodeId, String>) -> TunnelKey {
    match &node.run_id {
        _ if node.is_parked() => TunnelKey::SleepingRoom,
        Some(run) => TunnelKey::Run(run.clone()),
        None => TunnelKey::Repo(repos.get(&node.id).cloned().unwrap_or_default()),
    }
}

/// Order in a chamber: diggers first (filled from the face at the right), then failed, then
/// sleepers further from the face.
fn chamber_rank(state: NodeState) -> u8 {
    match state {
        NodeState::Working => 0,
        NodeState::Failed => 1,
        NodeState::Stalled => 2,
        _ => 3,
    }
}

/// A dispatch record that is no ant: its terminal is gone and its dispatch ended, so it is a
/// past attempt (often superseded by a re-dispatch of the same task). Released workers keep
/// fading out instead, and a dispatch still open without its terminal stays, as failed.
fn is_history(node: &Node) -> bool {
    node.kind == NodeKind::Worker
        && !node.live
        && node.released_at_ms.is_none()
        && dispatch_ended(node)
}

/// Runs whose tasks have all completed or failed and that have no worker left in `nodes`:
/// their coordinator may be at other work, so the run gets no tunnel.
fn finished_runs<'a>(snapshot: &'a FleetSnapshot, nodes: &[&Node]) -> HashSet<&'a str> {
    snapshot
        .runs
        .iter()
        .filter(|r| {
            r.task_counts
                .is_some_and(|c| c.total() > 0 && c.completed + c.failed == c.total())
        })
        .filter(|r| {
            !nodes
                .iter()
                .any(|n| n.kind == NodeKind::Worker && n.run_id.as_ref() == Some(&r.id))
        })
        .map(|r| r.id.as_str())
        .collect()
}

fn cut(label: &str) -> String {
    if label.chars().count() <= LABEL_CHARS {
        return label.to_string();
    }
    let mut cut: String = label.chars().take(LABEL_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Lays the farm out in `rect`.
pub fn layout(input: &FarmInput, rect: Rect) -> Scene {
    let snapshot = input.snapshot;
    let present: Vec<&Node> = snapshot
        .nodes
        .iter()
        .filter(|n| !input.hidden.contains(&n.id) && !is_history(n))
        .collect();
    let finished = finished_runs(snapshot, &present);
    // A finished run's coordinator is just a terminal now: no tunnel of its own.
    let visible: Vec<Cow<Node>> = present
        .into_iter()
        .map(|n| match &n.run_id {
            Some(run)
                if finished.contains(run.as_str())
                    && matches!(n.kind, NodeKind::Coordinator | NodeKind::Attendant) =>
            {
                Cow::Owned(Node {
                    kind: NodeKind::Terminal,
                    run_id: None,
                    ..n.clone()
                })
            }
            _ => Cow::Borrowed(n),
        })
        .collect();
    let repos = RepoIndex::new(snapshot).by_node;
    let parents: HashMap<&NodeId, &NodeId> = snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Subagent)
        .map(|e| (&e.to, &e.from))
        .collect();
    let shown: HashSet<&NodeId> = visible.iter().map(|n| &n.id).collect();
    // A subagent whose parent is not drawn digs in its repo's tunnel instead.
    let homes: HashMap<&NodeId, Home> = visible
        .iter()
        .map(|n| {
            let mut home = home(n.kind, n.state, input.pr_ready.contains(&n.id));
            if n.is_parked() && !matches!(home, Home::Queen | Home::Comms | Home::Trail) {
                home = Home::Chamber;
            }
            if home == Home::Trail && !parents.get(&n.id).is_some_and(|p| shown.contains(p)) {
                home = Home::Chamber;
            }
            (&n.id, home)
        })
        .collect();

    // Tunnels: runs in run-list order, then runs the list lacks, then repos by name, then
    // the sleeping room.
    let mut keys: Vec<TunnelKey> = Vec::new();
    for n in &visible {
        if matches!(homes[&n.id], Home::Queen | Home::Comms | Home::Trail) {
            continue;
        }
        let key = tunnel_key(n, &repos);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let run_order = |key: &TunnelKey| match key {
        TunnelKey::Run(id) => (
            0,
            snapshot
                .runs
                .iter()
                .position(|r| &r.id == id)
                .unwrap_or(usize::MAX),
            String::new(),
        ),
        TunnelKey::Repo(repo) => (1, 0, repo.clone()),
        TunnelKey::SleepingRoom => (2, 0, String::new()),
    };
    keys.sort_by_key(run_order);
    let tunnel_of: HashMap<&TunnelKey, usize> =
        keys.iter().enumerate().map(|(i, k)| (k, i)).collect();
    // The ant each subagent trails: its nearest ancestor not itself trailing.
    let root_of = |id: &NodeId| -> NodeId {
        let mut at = id;
        while homes.get(at) == Some(&Home::Trail) {
            at = parents[at];
        }
        at.clone()
    };
    let mut trails: HashMap<NodeId, usize> = HashMap::new();
    for node in &visible {
        if homes[&node.id] == Home::Trail {
            *trails.entry(root_of(&node.id)).or_default() += 1;
        }
    }
    let trail_of = |id: &NodeId| trails.get(id).copied().unwrap_or(0);
    // Slots per chamber, sized for subagents as big as their parents until the scale is
    // known.
    let mut chamber_counts = vec![0usize; keys.len()];
    for node in &visible {
        if homes[&node.id] == Home::Chamber {
            chamber_counts[tunnel_of[&tunnel_key(node, &repos)]] +=
                footprint(trail_of(&node.id), 1.0);
        }
    }
    let count = |home: Home| homes.values().filter(|h| **h == home).count();
    let crowd = Crowd {
        tunnels: keys.len(),
        chamber: chamber_counts.iter().copied().max().unwrap_or(0),
        landing: count(Home::Landing),
        storage: count(Home::Storage),
        envoy: count(Home::Comms) > 0,
    };
    let (px, levels) = pixel_scale(rect.size(), crowd);
    let a = CELL * px;
    let sub = (CELL * (px - 1.0).max(1.0)).min(a);

    // Surface band: sky, the landing on the surface, chambers just under it. The landing
    // and storage grow a row per row-full of ants.
    let band = band(rect.size(), a, crowd);
    let Band {
        surface,
        landing_per_row,
        storage_per_row,
        storage_h,
        ..
    } = band;
    let surface_y = rect.top() + surface;
    let landing_left = rect.left() + band.landing_left;
    let landing_right = rect.left() + band.landing_right;
    let landing_h = surface_y - (rect.top() + 0.4 * a);
    let landing = Rect::from_min_max(
        pos2(landing_left, surface_y - landing_h),
        pos2(landing_right, surface_y),
    );
    let chamber_h = 2.4 * a;
    let queen_chamber = Rect::from_center_size(
        pos2(
            rect.center().x,
            surface_y + 0.4 * a + band.queen_offset + chamber_h / 2.0,
        ),
        vec2(4.0 * a, chamber_h),
    );
    let storage_w = (rect.width() * 0.26).max(3.0 * a);
    let storage = Rect::from_min_size(
        pos2(rect.right() - 0.5 * a - storage_w, surface_y + 0.4 * a),
        vec2(storage_w, storage_h),
    );
    let gallery_top = rect.top() + band.gallery_top(a);
    let gallery = Rect::from_min_max(
        pos2(rect.left() + 0.5 * a, gallery_top),
        pos2(rect.right() - 0.5 * a, gallery_top + 1.2 * a),
    );

    let comms = crowd.envoy.then(|| {
        let b = CELL * band.comms_px;
        let chamber = Rect::from_min_size(
            pos2(rect.left() + 0.5 * b, surface_y + 0.4 * a),
            vec2(5.0 * b, band.comms_h),
        );
        CommsRoom {
            chamber,
            shaft: Rect::from_min_max(
                pos2(
                    chamber.left() + 28.0 * band.comms_px - 0.6 * b,
                    chamber.bottom() - px,
                ),
                pos2(
                    chamber.left() + 28.0 * band.comms_px + 0.6 * b,
                    gallery.center().y,
                ),
            ),
            dish: pos2(chamber.center().x, surface_y),
            px: band.comms_px,
        }
    });

    // Tunnel `i` is in column `i / levels`, on level `i % levels`. An upper chamber with one
    // under it leaves room on its right for that one's shaft.
    let n = keys.len().max(1);
    let column_w = rect.width() / n.div_ceil(levels) as f32;
    let place = |i: usize| {
        let column_left = rect.left() + column_w * (i / levels) as f32;
        let level = i % levels;
        let gap = if level + 1 < levels && i + 1 < keys.len() {
            SHAFT_GAP * a
        } else {
            0.0
        };
        let chamber_w = (column_w - 0.6 * a - gap).max(SLOT * a);
        let left = column_left + 0.3 * a;
        let shaft_x = if level > 0 {
            column_left + column_w - 0.3 * a - SHAFT_GAP * a / 2.0
        } else {
            left + chamber_w / 2.0
        };
        (level, left, chamber_w, shaft_x)
    };
    let per_row = |i: usize| chamber_per_row(place(i).2, a);
    // A full row spreads across the chamber.
    let slot_w = |i: usize| {
        let (per_row, chamber_w) = (per_row(i), place(i).2);
        if per_row > 1 {
            ((chamber_w - 2.0 * a) / (per_row - 1) as f32).max(SLOT * a)
        } else {
            SLOT * a
        }
    };
    // Each chamber's ants in rank order, and where they pack.
    let mut chamber_nodes: Vec<Vec<&Node>> = vec![Vec::new(); keys.len()];
    for node in &visible {
        if homes[&node.id] == Home::Chamber {
            chamber_nodes[tunnel_of[&tunnel_key(node, &repos)]].push(node);
        }
    }
    let packed: Vec<(Vec<(usize, usize)>, usize)> = chamber_nodes
        .iter_mut()
        .enumerate()
        .map(|(i, nodes)| {
            nodes.sort_by_key(|n| chamber_rank(n.state));
            let footprints: Vec<usize> = nodes
                .iter()
                .map(|n| footprint(trail_of(&n.id), sub / a))
                .collect();
            pack(&footprints, per_row(i))
        })
        .collect();
    // Each level's band below the gallery; a lower band starts with its tunnels' labels.
    let below_top = gallery.bottom() + a;
    let below = (rect.bottom() - 0.3 * a) - below_top;
    let band_h = (below - (levels - 1) as f32 * a) / levels as f32;
    let band_top = |level: usize| below_top + level as f32 * (band_h + a);
    let tunnels: Vec<Tunnel> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            let (level, left, chamber_w, shaft_x) = place(i);
            let height = rows_height(packed[i].1, a);
            let tasks = match key {
                TunnelKey::Run(id) => snapshot
                    .runs
                    .iter()
                    .find(|r| &r.id == id)
                    .and_then(|r| r.task_counts)
                    .filter(|c| c.total() > 0)
                    .map(|c| (c.completed, c.total())),
                TunnelKey::Repo(_) | TunnelKey::SleepingRoom => None,
            };
            let progress = tasks.map(|(done, total)| done as f32 / total as f32);
            let depth = progress.map_or(UNKNOWN_PROGRESS_DEPTH, |p| {
                MIN_DEPTH + (1.0 - MIN_DEPTH) * p
            });
            let room = (band_h - height).max(0.0);
            let top = band_top(level) + room * depth;
            let chamber = Rect::from_min_size(pos2(left, top), vec2(chamber_w, height));
            let shaft = Rect::from_min_max(
                pos2(shaft_x - 0.6 * a, gallery.center().y),
                pos2(shaft_x + 0.6 * a, chamber.top() + 0.5 * a),
            );
            let label_top = if level == 0 {
                pos2(shaft_x, gallery.bottom() + 3.0 * px)
            } else {
                pos2(chamber.center().x, band_top(level) - a + 3.0 * px)
            };
            let label = match key {
                TunnelKey::Run(id) => snapshot
                    .runs
                    .iter()
                    .find(|r| &r.id == id)
                    .and_then(|r| r.objective.as_deref())
                    .unwrap_or(id),
                TunnelKey::Repo(repo) => repo,
                TunnelKey::SleepingRoom => crate::park::SLEEPING_ROOM,
            };
            Tunnel {
                key: key.clone(),
                label: cut(label),
                shaft,
                chamber,
                progress,
                tasks,
                label_top,
            }
        })
        .collect();

    // Slots, in snapshot order within each place.
    let mut landing_n = 0;
    let mut storage_n = 0;
    let mut mouth_n = vec![0usize; tunnels.len()];
    let mut ants: Vec<Ant> = Vec::new();
    let mut pr_ready = 0;
    let mut trailing: Vec<&Node> = Vec::new();
    for node in &visible {
        let home = homes[&node.id];
        let is_pr = input.pr_ready.contains(&node.id);
        let (spot, pos) = match home {
            Home::Trail => {
                trailing.push(node);
                continue;
            }
            Home::Chamber => continue,
            Home::Comms => (Spot::Comms, comms.expect("Envoy has a room").ant_position()),
            Home::Queen => (Spot::Queen, queen_chamber.center() + vec2(0.0, 0.2 * a)),
            Home::Landing => {
                let k = landing_n;
                landing_n += 1;
                let (row, col) = (k / landing_per_row, k % landing_per_row);
                let pos = pos2(
                    landing.left() + (col as f32 + 0.5) * SLOT * a,
                    surface_y - 0.5 * a - row as f32 * ROW * a,
                );
                (Spot::Landing(k), pos)
            }
            Home::Storage => {
                let k = storage_n;
                storage_n += 1;
                pr_ready += usize::from(is_pr);
                let (row, col) = (k / storage_per_row, k % storage_per_row);
                let pos = pos2(
                    storage.right() - (col as f32 + 0.5) * SLOT * a,
                    storage.bottom() - (0.5 + TAG) * a - row as f32 * ROW * a,
                );
                (Spot::Storage(k), pos)
            }
            Home::Mouth => {
                let t = tunnel_of[&tunnel_key(node, &repos)];
                let k = mouth_n[t];
                mouth_n[t] += 1;
                let shaft = tunnels[t].shaft;
                let side = if k.is_multiple_of(2) { 1.0 } else { -1.0 };
                let x = shaft.center().x + side * (k.div_ceil(2) as f32) * a;
                (Spot::Mouth(t, k), pos2(x, gallery.bottom() - 0.5 * a))
            }
        };
        let size = if home == Home::Comms {
            CELL * band.comms_px
        } else {
            a
        };
        let mut placed = ant(node, spot, pos, size, home, is_pr, input.now_ms);
        // A coordinator away from its tunnel's mouth is named by its run.
        if matches!(node.kind, NodeKind::Coordinator | NodeKind::Attendant)
            && matches!(home, Home::Landing | Home::Storage)
        {
            let objective = snapshot
                .runs
                .iter()
                .find(|r| node.run_id.as_ref() == Some(&r.id))
                .and_then(|r| r.objective.as_deref());
            placed.tag = Some(objective.unwrap_or(&node.label).to_string());
        }
        ants.push(placed);
    }
    let mut beds = Vec::new();
    for (t, nodes) in chamber_nodes.iter().enumerate() {
        let chamber = tunnels[t].chamber;
        let slot = |row: usize, col: usize| {
            pos2(
                chamber.right() - a - col as f32 * slot_w(t),
                chamber.bottom() - (0.5 + TAG) * a - row as f32 * ROW * a,
            )
        };
        if tunnels[t].key == TunnelKey::SleepingRoom {
            // A bed under each sleeper, and a spare one where the last row has room.
            let (places, rows) = &packed[t];
            let used = places
                .iter()
                .filter(|(row, _)| row + 1 == *rows)
                .map(|(_, col)| col + 1)
                .max()
                .unwrap_or(0);
            let spare = (used < per_row(t)).then_some((rows - 1, used));
            let sleepers = nodes.iter().map(|n| Some(n.id.clone()));
            for (sleeper, &(row, col)) in sleepers
                .chain([None])
                .zip(places.iter().chain(spare.as_ref()))
            {
                beds.push(Bed {
                    rect: bed_rect(slot(row, col), a, px),
                    sleeper,
                });
            }
        }
        for (k, (node, &(row, col))) in nodes.iter().zip(&packed[t].0).enumerate() {
            let pos = slot(row, col);
            let is_pr = input.pr_ready.contains(&node.id);
            ants.push(ant(
                node,
                Spot::Chamber(t, k),
                pos,
                a,
                Home::Chamber,
                is_pr,
                input.now_ms,
            ));
        }
    }
    let parked: HashSet<&NodeId> = visible
        .iter()
        .filter(|n| n.is_parked())
        .map(|n| &n.id)
        .collect();
    for ant in &mut ants {
        if parked.contains(&ant.id) {
            ant.pose = Pose::Sleep;
            ant.mark = Some(Mark::Sleep);
            ant.facing_right = false;
        } else if ant.pose == Pose::Dig && input.building.contains(&ant.id) {
            ant.pose = Pose::Build;
        }
    }
    // Subagents, nested ones too, trail the ant they belong to in a line, nearest first.
    let mut behind: HashMap<NodeId, usize> = HashMap::new();
    for node in trailing {
        let root = root_of(&node.id);
        let Some(at) = ants.iter().find(|x| x.id == root) else {
            continue;
        };
        let k = behind.entry(root.clone()).or_default();
        let dir = if at.facing_right { -1.0 } else { 1.0 };
        let step = (at.size + sub) / 2.0 + *k as f32 * sub;
        let pos = pos2(at.pos.x + dir * step, at.pos.y + (at.size - sub) / 2.0);
        let facing_right = at.facing_right;
        let mut trail = ant(
            node,
            Spot::Trail(root, *k),
            pos,
            sub,
            Home::Trail,
            false,
            input.now_ms,
        );
        trail.facing_right = facing_right;
        *k += 1;
        ants.push(trail);
    }

    Scene {
        rect,
        px,
        surface_y,
        gallery,
        landing,
        queen_chamber,
        comms,
        storage,
        tunnels,
        beds,
        ants,
        pr_ready,
    }
}

/// The bed under a sleeper lying at `pos`: centered on it, its legs on the floor the sleeper
/// would lie on without one.
fn bed_rect(pos: Pos2, a: f32, px: f32) -> Rect {
    let floor = pos.y + 0.5 * a + 2.0 * px;
    Rect::from_min_max(
        pos2(pos.x - BED_W / 2.0 * px, floor - BED_H * px),
        pos2(pos.x + BED_W / 2.0 * px, floor),
    )
}

fn ant(node: &Node, spot: Spot, pos: Pos2, size: f32, home: Home, pr: bool, now_ms: i64) -> Ant {
    let pose = pose(node.kind, node.state, home);
    let size = if node.kind == NodeKind::Queen {
        size * 2.0
    } else {
        size
    };
    Ant {
        id: node.id.clone(),
        kind: node.kind,
        state: node.state,
        spot,
        pos,
        size,
        pose,
        mark: if matches!(node.kind, NodeKind::Queen | NodeKind::Envoy) {
            None
        } else {
            mark(node.state, pr)
        },
        facing_right: pose == Pose::Dig || node.kind == NodeKind::Envoy,
        tag: match node.kind {
            NodeKind::Envoy => Some("Envoy".into()),
            NodeKind::Worker | NodeKind::Terminal => Some(node.label.clone()),
            NodeKind::Coordinator | NodeKind::Attendant | NodeKind::Subagent | NodeKind::Queen => {
                None
            }
        },
        alpha: crate::ui::graph::fade_alpha(node.released_at_ms, now_ms),
    }
}
