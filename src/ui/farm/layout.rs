//! Where everything in the farm goes: a function of the snapshot, the view's rect and the
//! room plan ([`super::rooms`]), so the state → position mapping is testable without drawing.
//!
//! The farm is a side view: sky above the surface, the "the user" landing on the surface at the
//! left, the throne hall under the surface in the middle, the storage chamber at the
//! right, a gallery running under them and one tunnel per run (or per repo for loose
//! terminals) dropping from the gallery. A coordinator stands at its tunnel's mouth, its
//! workers in the chamber at the tunnel's end. The room plan fits a grid of chambers under the
//! gallery to the view, as big as their dwarves allow, and keeps each chamber in its cell while
//! its dwarves come and go; when the dwarves can be drawn bigger that way, the grid has more levels:
//! chambers stacked in a column, the lower ones' shafts passing beside the upper chambers. Parked agents sleep in beds in the resting area's
//! chamber, there only while someone is parked (and for the grace an emptied room gets).

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};

use crate::model::{
    ClanSnapshot, EdgeKind, Node, NodeId, NodeKind, NodeState, RepoIndex, dispatch_ended,
};
use crate::orca::AgentIdentity;

use super::rooms::{Demand, LABEL_GAP, RoomPlan};

/// Side of a sprite cell in texels.
pub const CELL: f32 = 16.0;
/// The user's rune portal in texels.
pub const PORTAL_W: f32 = 32.0;
pub const PORTAL_H: f32 = 40.0;
/// Characters of a tunnel label before it is cut.
const LABEL_CHARS: usize = 28;
/// Tunnel depth, as a share of the room below the gallery, for a run without task counts.
const UNKNOWN_PROGRESS_DEPTH: f32 = 0.6;
/// Shallowest tunnel, as a share of the room below the gallery (a run with nothing done).
const MIN_DEPTH: f32 = 0.35;
/// Distance between rows of dwarves, in cells: room for a row's bubbles above and name tags
/// below.
const ROW: f32 = 2.5;
/// Width of a dwarf's slot in a row, in cells: room for its name tag.
pub(super) const SLOT: f32 = 2.4;
/// Room under a dwarf for its name tag, in cells.
const TAG: f32 = 0.85;
/// Width beside an upper chamber for the shaft down to the chamber under it, in cells.
pub(super) const SHAFT_GAP: f32 = 1.8;
/// A bed in texels: its size, and the mattress top's height above the floor.
pub const BED_W: f32 = 20.0;
pub const BED_H: f32 = 9.0;
pub const MATTRESS_TOP: f32 = 6.0;
/// Texels a sleeper is raised to lie on its bed's mattress, under the blanket.
pub const SLEEPER_LIFT: f32 = 2.0;

/// What a tunnel belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TunnelKey {
    Run(String),
    /// Loose terminals of a repo (see [`crate::model::node_repo`]).
    Repo(String),
    /// Parked agents, asleep ([`crate::park`]).
    RestingArea,
}

/// A logical place a dwarf occupies. A dwarf walks when its spot changes; a spot keeping its
/// place while the view resizes does not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Spot {
    King,
    Comms,
    /// On the surface waiting for the user, the nth in line.
    Landing(usize),
    /// In the storage chamber with its crumb, the nth on the pile.
    Storage(usize),
    /// At the mouth of a tunnel (coordinators and attendants).
    Mouth(TunnelKey, usize),
    /// In the chamber at a tunnel's end.
    Chamber(TunnelKey, usize),
    /// Trailing its parent, the nth behind it (subagents).
    Trail(NodeId, usize),
}

/// The area a spot is in; dwarves walk along the gallery between areas and straight within one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Area {
    King,
    Comms,
    Landing,
    Storage,
    Tunnel(TunnelKey),
    Trail,
}

impl Spot {
    pub fn area(&self) -> Area {
        match self {
            Spot::King => Area::King,
            Spot::Comms => Area::Comms,
            Spot::Landing(_) => Area::Landing,
            Spot::Storage(_) => Area::Storage,
            Spot::Mouth(t, _) | Spot::Chamber(t, _) => Area::Tunnel(t.clone()),
            Spot::Trail(..) => Area::Trail,
        }
    }
}

/// What a dwarf is doing, which picks its animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pose {
    /// Standing at its post (a coordinator at work).
    Stand,
    /// Digging at the chamber's face (working: reading, searching, thinking).
    Dig,
    /// Building at the chamber's face (editing or building).
    Build,
    /// Lying still (idle or stalled).
    Sleep,
    /// Waving on the landing for the user.
    Wait,
    /// Holding its crumb in storage.
    Carry,
    /// Red and still.
    Failed,
    King,
}

/// The bubble over a dwarf, so its state does not rely on pose or color alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Waiting on the user.
    Alert,
    /// Idle.
    Sleep,
    /// Stalled: a pebble bonks the helmet.
    Stalled,
    Failed,
    /// Done with a PR ready for review.
    PrReady,
}

/// One dwarf to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Dwarf {
    pub id: NodeId,
    pub kind: NodeKind,
    pub state: NodeState,
    pub spot: Spot,
    /// Center of the dwarf's cell, in screen points.
    pub pos: Pos2,
    /// Side of the dwarf's cell in points.
    pub size: f32,
    pub pose: Pose,
    pub mark: Option<Mark>,
    /// Faces right (towards the dig face); the sprites face left.
    pub facing_right: bool,
    /// Opacity, for released workers and finished subagents.
    pub alpha: f32,
    /// Name shown under the dwarf (its node label, or a coordinator's run away from its
    /// tunnel); `None` for the King, coordinators at their tunnel's mouth (the tunnel is
    /// labelled) and subagents.
    pub tag: Option<String>,
    /// The coding agent it runs, when known.
    pub agent: Option<AgentIdentity>,
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

/// A bed in the resting area, with the parked dwarf lying on it, if any.
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
    pub receiver: Pos2,
    pub px: f32,
}

impl CommsRoom {
    pub fn dwarf_position(&self) -> Pos2 {
        self.chamber.min + vec2(28.0, 31.0) * self.px
    }

    pub fn feed(&self) -> Pos2 {
        self.receiver + vec2(3.0, -24.0) * self.px
    }

    pub fn pool(&self) -> Pos2 {
        pos2(
            self.chamber.right() - 40.0 * self.px,
            self.chamber.top() + 23.0 * self.px,
        )
    }

    /// Low enough for the priest's raised hands to hover over the water. Its rim
    /// overlaps the front of his cell; his figure is painted over the basin.
    pub fn basin_rect(&self) -> Rect {
        let foot = self.pool() + vec2(0.0, 23.0) * self.px;
        Rect::from_min_size(
            foot - vec2(14.0, 18.0) * self.px,
            vec2(28.0, 18.0) * self.px,
        )
    }

    pub fn tray_rect(&self) -> Rect {
        Rect::from_min_size(
            self.chamber.min + vec2(61.0, 20.0) * self.px,
            vec2(17.0, 25.0) * self.px,
        )
    }

    pub fn rune_port(&self) -> Pos2 {
        self.pool() + vec2(-8.0, 2.0) * self.px
    }

    pub fn rune_turn(&self) -> Pos2 {
        pos2(self.receiver.x, self.rune_port().y)
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
    /// Where dwarves waiting on the user stand, on the surface.
    pub landing: Rect,
    pub throne_hall: Rect,
    pub comms: Option<CommsRoom>,
    pub storage: Rect,
    /// The Ledger's chamber (see [`super::ledger::spot`]); optional while frames interpolate.
    pub ledger: Option<Rect>,
    pub tunnels: Vec<Tunnel>,
    pub beds: Vec<Bed>,
    /// In draw order: parents before the subagents trailing them.
    pub dwarves: Vec<Dwarf>,
    /// Dwarves with a PR ready for review: one coin each on the storage pile.
    pub pr_ready: usize,
}

impl Scene {
    /// Side of a normal dwarf's cell in points.
    pub fn cell(&self) -> f32 {
        CELL * self.px
    }

    /// The mine portal stands beside the waiting queue, clear of the rune stone.
    pub fn entrance_x(&self) -> f32 {
        self.landing.left() - 0.6 * self.cell()
    }

    /// Texture foot, with the stone threshold sunk into the turf. The drawing has two
    /// texels of padding below its base, so this seats the stone one texel into the rock.
    pub fn user_portal(&self) -> Pos2 {
        let left = self.landing.right();
        pos2(
            left + 0.3 * self.cell() + PORTAL_W / 2.0 * self.px,
            self.surface_y + 3.0 * self.px,
        )
    }

    /// The heart of the portal, where scrolls materialise and disappear.
    pub fn user_portal_focus(&self) -> Pos2 {
        self.user_portal() - vec2(0.0, PORTAL_H * 0.525 * self.px)
    }

    pub fn dwarf(&self, id: &NodeId) -> Option<&Dwarf> {
        self.dwarves.iter().find(|a| &a.id == id)
    }
}

/// What the farm is drawn from: the clan's snapshot and its filters.
pub struct FarmInput<'a> {
    pub snapshot: &'a ClanSnapshot,
    /// Nodes filtered out; they are not drawn.
    pub hidden: &'a BTreeSet<NodeId>,
    pub pr_ready: &'a HashSet<NodeId>,
    /// Nodes whose agent is editing or building: their diggers build instead.
    pub building: &'a HashSet<NodeId>,
    pub now_ms: i64,
}

/// How many dwarves the surface band holds, which decides its height.
#[derive(Debug, Clone, Copy, Default)]
pub struct Crowd {
    pub landing: usize,
    pub storage: usize,
    pub envoy: bool,
}

pub(super) struct Band {
    surface: f32,
    landing_left: f32,
    landing_right: f32,
    landing_per_row: usize,
    storage_per_row: usize,
    storage_w: f32,
    storage_h: f32,
    comms_px: f32,
    comms_h: f32,
    king_offset: f32,
    ledger: super::ledger::Spot,
}

impl Band {
    /// The height a Ledger chamber stacked under the throne hall adds.
    fn ledger_stack(&self, a: f32) -> f32 {
        if self.ledger == super::ledger::Spot::Stacked {
            super::ledger::STACK * a
        } else {
            0.0
        }
    }

    pub(super) fn gallery_top(&self, a: f32) -> f32 {
        self.surface
            + 0.4 * a
            + (2.4 * a + self.king_offset + self.ledger_stack(a))
                .max(self.storage_h)
                .max(self.comms_h)
            + 0.6 * a
    }
}

// Fitting and placement share the same reservations for the rune stone, room and waiting dwarves.
pub(super) fn band(size: Vec2, a: f32, crowd: Crowd) -> Band {
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
    let surface = (2.4 * a).max(landing_h + 0.8 * a).max(58.0 * comms_px);
    // Storage widens towards the throne hall to keep its pile low.
    let pile = crowd.storage as f32 * SLOT * a + a;
    let storage_w = (size.x * 0.26)
        .max(pile.min(size.x * 0.5 - 3.5 * a))
        .max(3.0 * a);
    let storage_per_row = ((storage_w / (SLOT * a)).floor() as usize).max(1);
    let storage_h =
        ((rows(crowd.storage, storage_per_row) - 1.0) * ROW * a + (2.2 + TAG) * a).max(2.4 * a);
    let comms_h = 3.0 * b;
    // Narrow panes stack the King below the comms room instead of squeezing either room.
    let king_offset = if crowd.envoy && size.x * 0.5 - 2.0 * a < 5.5 * b + 0.5 * a {
        comms_h + 0.6 * a
    } else {
        0.0
    };
    // The Ledger's chamber looks for free rock between the upper rooms' columns.
    let px = a / CELL;
    let entrance = landing_left - 0.6 * a;
    let mut occupied = vec![
        (size.x * 0.5 - 2.0 * a, size.x * 0.5 + 2.0 * a),
        (size.x - 0.5 * a - storage_w, size.x - 0.5 * a),
        (entrance - 12.0 * px, entrance + 12.0 * px),
    ];
    if crowd.envoy {
        occupied.push((0.5 * b, 5.5 * b));
    }
    let rock_h = (2.4 * a + king_offset).max(storage_h).max(comms_h) + a
        - super::ledger::surface_clearance(px)
        - 5.0 * px;
    let ledger = super::ledger::spot(size, px, occupied, rock_h);
    Band {
        surface,
        landing_left,
        landing_right,
        landing_per_row,
        storage_per_row,
        storage_w,
        storage_h,
        comms_px,
        comms_h,
        king_offset,
        ledger,
    }
}

/// Dwarves that fit side by side in a chamber `width` wide: slots need their gap only between
/// them.
pub(super) fn chamber_per_row(width: f32, a: f32) -> usize {
    (((width + (SLOT - 2.0) * a) / (SLOT * a)).floor() as usize).max(1)
}

pub(super) fn rows_height(rows: usize, a: f32) -> f32 {
    (rows.max(1) as f32 - 1.0) * ROW * a + (1.4 + TAG) * a
}

/// Chamber slots a dwarf takes with `trail` subagents behind it, each `sub` cells wide: its
/// own, and enough empty ones behind it to hold the trail.
fn footprint(trail: usize, sub: f32) -> usize {
    1 + (trail as f32 * sub / SLOT).ceil() as usize
}

/// `(row, slot)` of each footprint packed into rows of `per_row` slots, in order, and the
/// number of rows. A footprint that does not fit what is left of a row starts the next.
pub(super) fn pack(footprints: &[usize], per_row: usize) -> (Vec<(usize, usize)>, usize) {
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

/// Where a dwarf of `kind` in `state` belongs, before slots are assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Home {
    King,
    Comms,
    Landing,
    Storage,
    Mouth,
    Chamber,
    Trail,
}

/// The state → place rule: waiting on the user goes to the landing, done (or idle with a PR ready
/// for review) goes to storage, all else stays at its post (a coordinator at the mouth, a
/// worker in the chamber), failed ones turning red there. Subagents trail their parent
/// whatever their state.
pub fn home(kind: NodeKind, state: NodeState, pr_ready: bool) -> Home {
    match kind {
        NodeKind::King => return Home::King,
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
    if kind == NodeKind::King {
        return Pose::King;
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
        NodeState::Stalled => Some(Mark::Stalled),
        NodeState::Failed => Some(Mark::Failed),
        NodeState::Done | NodeState::Working => pr_ready.then_some(Mark::PrReady),
    }
}

/// The tunnel a node digs in: its run's, else its repo's.
fn tunnel_key(node: &Node, repos: &HashMap<NodeId, String>) -> TunnelKey {
    match &node.run_id {
        _ if node.is_parked() => TunnelKey::RestingArea,
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

/// A dispatch record that is no dwarf: its terminal is gone and its dispatch ended, so it is a
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
fn finished_runs<'a>(snapshot: &'a ClanSnapshot, nodes: &[&Node]) -> HashSet<&'a str> {
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

/// Lays the farm out in `rect` afresh, as on a first frame.
pub fn layout(input: &FarmInput, rect: Rect) -> Scene {
    layout_in(input, rect, &mut RoomPlan::default())
}

/// Lays the farm out in `rect`, keeping the rooms where `plan` has them and bringing it up
/// to the clan (see [`super::rooms`]).
pub fn layout_in(input: &FarmInput, rect: Rect, plan: &mut RoomPlan) -> Scene {
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
            if n.is_parked() && !matches!(home, Home::King | Home::Comms | Home::Trail) {
                home = Home::Chamber;
            }
            if home == Home::Trail && !parents.get(&n.id).is_some_and(|p| shown.contains(p)) {
                home = Home::Chamber;
            }
            (&n.id, home)
        })
        .collect();

    // Tunnels: runs in run-list order, then runs the list lacks, then repos by name, then
    // the resting area.
    let mut keys: Vec<TunnelKey> = Vec::new();
    for n in &visible {
        if matches!(homes[&n.id], Home::King | Home::Comms | Home::Trail) {
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
        TunnelKey::RestingArea => (2, 0, String::new()),
    };
    keys.sort_by_key(run_order);
    let tunnel_of: HashMap<&TunnelKey, usize> =
        keys.iter().enumerate().map(|(i, k)| (k, i)).collect();
    // The dwarf each subagent trails: its nearest ancestor not itself trailing.
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
    // Each room's dwarves in rank order, with their footprints sized for subagents as big as
    // their parents: the plan digs for those, so the dwarves fit at any scale.
    let mut chamber_nodes: Vec<Vec<&Node>> = vec![Vec::new(); keys.len()];
    for node in &visible {
        if homes[&node.id] == Home::Chamber {
            chamber_nodes[tunnel_of[&tunnel_key(node, &repos)]].push(node);
        }
    }
    for nodes in &mut chamber_nodes {
        nodes.sort_by_key(|n| chamber_rank(n.state));
    }
    let count = |home: Home| homes.values().filter(|h| **h == home).count();
    let envoy = count(Home::Comms) > 0;
    let demand = Demand {
        rooms: keys
            .iter()
            .zip(&chamber_nodes)
            .map(|(key, nodes)| {
                let footprints = nodes.iter().map(|n| footprint(trail_of(&n.id), 1.0));
                (key.clone(), footprints.collect())
            })
            .collect(),
        landing: count(Home::Landing),
        storage: count(Home::Storage),
        envoy,
    };
    plan.update(&demand, rect.size(), input.now_ms);
    let crowd = plan.crowd(envoy);
    let ground = plan
        .ground(rect.size(), envoy)
        .expect("an updated plan has a grid");
    let px = ground.px;
    let a = CELL * px;
    let sub = (CELL * (px - 1.0).max(1.0)).min(a);

    // Surface band: sky, the landing on the surface, chambers just under it. The landing
    // and storage grow a row per row-full of dwarves.
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
    let throne_hall = Rect::from_center_size(
        pos2(
            rect.center().x,
            surface_y + 0.4 * a + band.king_offset + chamber_h / 2.0,
        ),
        vec2(4.0 * a, chamber_h),
    );
    let storage = Rect::from_min_size(
        pos2(rect.right() - 0.5 * a - band.storage_w, surface_y + 0.4 * a),
        vec2(band.storage_w, storage_h),
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
            receiver: pos2(chamber.center().x, surface_y),
            px: band.comms_px,
        }
    });

    let ledger = match band.ledger {
        super::ledger::Spot::Beside {
            left,
            right,
            height,
        } => {
            let top = surface_y + super::ledger::surface_clearance(px);
            Some(Rect::from_min_max(
                pos2(rect.left() + left, top),
                pos2(rect.left() + right, top + height),
            ))
        }
        super::ledger::Spot::Stacked => Some(Rect::from_min_size(
            pos2(
                throne_hall.left(),
                throne_hall.bottom() + super::ledger::GAP * a,
            ),
            vec2(throne_hall.width(), super::ledger::HEIGHT * a),
        )),
    };

    // The plan's rooms, emptied ones too while they wait out their grace. A room's chamber
    // fills its cell; a lower room's shaft comes down at the right of its column, beside the
    // chambers over it.
    let dwarves_of: HashMap<&TunnelKey, &Vec<&Node>> = keys.iter().zip(&chamber_nodes).collect();
    let none = Vec::new();
    let band_top = |level: usize| {
        rect.top() + ground.below_top + level as f32 * (ground.band_h + LABEL_GAP * a)
    };
    let mut room_nodes: Vec<&Vec<&Node>> = Vec::new();
    let mut packed: Vec<(Vec<(usize, usize)>, usize)> = Vec::new();
    let mut slot_ws: Vec<f32> = Vec::new();
    let mut per_rows: Vec<usize> = Vec::new();
    let tunnels: Vec<Tunnel> = plan
        .rooms
        .iter()
        .map(|room| {
            let key = &room.key;
            let nodes = dwarves_of.get(key).copied().unwrap_or(&none);
            let chamber_w = ground.chamber_w(room.level, room.col);
            let per_row = ground.per_row(room.level, room.col);
            let footprints: Vec<usize> = nodes
                .iter()
                .map(|n| footprint(trail_of(&n.id), sub / a))
                .collect();
            let (places, rows) = pack(&footprints, per_row);
            // A full row spreads across the chamber.
            slot_ws.push(if per_row > 1 {
                ((chamber_w - 2.0 * a) / (per_row - 1) as f32).max(SLOT * a)
            } else {
                SLOT * a
            });
            room_nodes.push(nodes);
            per_rows.push(per_row);
            packed.push((places, rows));
            let left = rect.left() + ground.chamber_left(room.col);
            let shaft_x = rect.left() + ground.shaft_x(room.level, room.col);
            let height = rows_height(room.rows.max(rows), a);
            let tasks = match key {
                TunnelKey::Run(id) => snapshot
                    .runs
                    .iter()
                    .find(|r| &r.id == id)
                    .and_then(|r| r.task_counts)
                    .filter(|c| c.total() > 0)
                    .map(|c| (c.completed, c.total())),
                TunnelKey::Repo(_) | TunnelKey::RestingArea => None,
            };
            let progress = tasks.map(|(done, total)| done as f32 / total as f32);
            let depth = progress.map_or(UNKNOWN_PROGRESS_DEPTH, |p| {
                MIN_DEPTH + (1.0 - MIN_DEPTH) * p
            });
            let room_h = (ground.band_h - height).max(0.0);
            let top = band_top(room.level) + room_h * depth;
            let chamber = Rect::from_min_size(pos2(left, top), vec2(chamber_w, height));
            let shaft = Rect::from_min_max(
                pos2(shaft_x - 0.6 * a, gallery.center().y),
                pos2(shaft_x + 0.6 * a, chamber.top() + 0.5 * a),
            );
            let label_top = if room.level == 0 {
                pos2(shaft_x, gallery.bottom() + 3.0 * px)
            } else {
                pos2(
                    chamber.center().x,
                    band_top(room.level) - LABEL_GAP * a + 3.0 * px,
                )
            };
            let label = match key {
                TunnelKey::Run(id) => snapshot
                    .runs
                    .iter()
                    .find(|r| &r.id == id)
                    .and_then(|r| r.objective.as_deref())
                    .unwrap_or(id),
                TunnelKey::Repo(repo) => repo,
                TunnelKey::RestingArea => crate::park::RESTING_AREA,
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
    let room_of: HashMap<&TunnelKey, usize> = tunnels
        .iter()
        .enumerate()
        .map(|(i, t)| (&t.key, i))
        .collect();
    // Slots, in snapshot order within each place.
    let mut landing_n = 0;
    let mut storage_n = 0;
    let mut mouth_n = vec![0usize; tunnels.len()];
    let mut dwarves: Vec<Dwarf> = Vec::new();
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
            Home::Comms => (
                Spot::Comms,
                comms.expect("Envoy has a room").dwarf_position(),
            ),
            Home::King => (Spot::King, throne_hall.center_bottom() - vec2(0.0, 0.6 * a)),
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
                let key = tunnel_key(node, &repos);
                let t = room_of[&key];
                let k = mouth_n[t];
                mouth_n[t] += 1;
                let shaft = tunnels[t].shaft;
                let side = if k.is_multiple_of(2) { 1.0 } else { -1.0 };
                let x = shaft.center().x + side * (k.div_ceil(2) as f32) * a;
                (Spot::Mouth(key, k), pos2(x, gallery.bottom() - 0.5 * a))
            }
        };
        let size = if home == Home::Comms {
            CELL * band.comms_px
        } else {
            a
        };
        let mut placed = dwarf(node, spot, pos, size, home, is_pr, input.now_ms);
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
        dwarves.push(placed);
    }
    let mut beds = Vec::new();
    for (t, nodes) in room_nodes.iter().enumerate() {
        let chamber = tunnels[t].chamber;
        let slot = |row: usize, col: usize| {
            pos2(
                chamber.right() - a - col as f32 * slot_ws[t],
                chamber.bottom() - (0.5 + TAG) * a - row as f32 * ROW * a,
            )
        };
        if tunnels[t].key == TunnelKey::RestingArea {
            // A bed under each sleeper, and a spare one where the last row has room.
            let (places, rows) = &packed[t];
            let used = places
                .iter()
                .filter(|(row, _)| row + 1 == *rows)
                .map(|(_, col)| col + 1)
                .max()
                .unwrap_or(0);
            let spare = (used < per_rows[t]).then_some((rows - 1, used));
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
            dwarves.push(dwarf(
                node,
                Spot::Chamber(tunnels[t].key.clone(), k),
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
    for dwarf in &mut dwarves {
        if parked.contains(&dwarf.id) {
            dwarf.pose = Pose::Sleep;
            dwarf.mark = Some(Mark::Sleep);
            dwarf.facing_right = false;
        } else if dwarf.pose == Pose::Dig && input.building.contains(&dwarf.id) {
            dwarf.pose = Pose::Build;
        }
    }
    // Subagents, nested ones too, trail the dwarf they belong to in a line, nearest first.
    let mut behind: HashMap<NodeId, usize> = HashMap::new();
    for node in trailing {
        let root = root_of(&node.id);
        let Some(at) = dwarves.iter().find(|x| x.id == root) else {
            continue;
        };
        let k = behind.entry(root.clone()).or_default();
        let dir = if at.facing_right { -1.0 } else { 1.0 };
        let step = (at.size + sub) / 2.0 + *k as f32 * sub;
        let pos = pos2(at.pos.x + dir * step, at.pos.y + (at.size - sub) / 2.0);
        let facing_right = at.facing_right;
        let mut trail = dwarf(
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
        dwarves.push(trail);
    }

    Scene {
        rect,
        px,
        surface_y,
        gallery,
        landing,
        throne_hall,
        comms,
        storage,
        ledger,
        tunnels,
        beds,
        dwarves,
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

fn dwarf(
    node: &Node,
    spot: Spot,
    pos: Pos2,
    size: f32,
    home: Home,
    pr: bool,
    now_ms: i64,
) -> Dwarf {
    let pose = pose(node.kind, node.state, home);
    Dwarf {
        id: node.id.clone(),
        kind: node.kind,
        state: node.state,
        spot,
        pos,
        size,
        pose,
        mark: if matches!(node.kind, NodeKind::King | NodeKind::Envoy) {
            None
        } else {
            mark(node.state, pr)
        },
        facing_right: pose == Pose::Dig || node.kind == NodeKind::Envoy,
        tag: match node.kind {
            NodeKind::Envoy => Some("Envoy".into()),
            NodeKind::Worker | NodeKind::Terminal => Some(node.label.clone()),
            NodeKind::Coordinator | NodeKind::Attendant | NodeKind::Subagent | NodeKind::King => {
                None
            }
        },
        alpha: crate::ui::clan_view::fade_alpha(node.released_at_ms, now_ms),
        agent: node.agent.clone(),
    }
}
