//! Where everything in the ant farm goes: a pure function of the snapshot and the view's
//! rect, so the state → position mapping is testable without drawing.
//!
//! The farm is a side view: sky above the surface, the "Karl" landing on the surface at the
//! left, the Queen's chamber under the surface in the middle, the storage chamber at the
//! right, a gallery running under them and one tunnel per run (or per repo for loose
//! terminals) dropping from the gallery. A coordinator stands at its tunnel's mouth, its
//! workers in the chamber at the tunnel's end.

use std::collections::{BTreeSet, HashMap, HashSet};

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

use crate::model::{EdgeKind, FleetSnapshot, Node, NodeId, NodeKind, NodeState, RepoIndex};

/// Side of a sprite cell in texels.
pub const CELL: f32 = 16.0;
/// Characters of a tunnel label before it is cut.
const LABEL_CHARS: usize = 28;
/// Tunnel depth, as a share of the room below the gallery, for a run without task counts.
const UNKNOWN_PROGRESS_DEPTH: f32 = 0.6;
/// Shallowest tunnel, as a share of the room below the gallery (a run with nothing done).
const MIN_DEPTH: f32 = 0.35;
/// Distance between rows of ants, in cells: room for the bubbles of the row below.
const ROW: f32 = 1.5;

/// What a tunnel belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TunnelKey {
    Run(String),
    /// Loose terminals of a repo (see [`crate::model::node_repo`]).
    Repo(String),
}

/// A logical place an ant occupies. An ant walks when its spot changes; a spot keeping its
/// place while the view resizes does not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Spot {
    Queen,
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
    Landing,
    Storage,
    Tunnel(usize),
    Trail,
}

impl Spot {
    pub fn area(&self) -> Area {
        match self {
            Spot::Queen => Area::Queen,
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
    /// Digging at the chamber's face.
    Dig,
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
    pub storage: Rect,
    pub tunnels: Vec<Tunnel>,
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
    pub now_ms: i64,
}

/// Points per texel for a view of `size` with `tunnels` tunnels: the biggest whole scale
/// that leaves each tunnel room for a few ants side by side and the farm room for its depth.
pub fn pixel_scale(size: Vec2, tunnels: usize) -> f32 {
    let column = size.x / tunnels.max(1) as f32;
    [4.0, 3.0, 2.0]
        .into_iter()
        .find(|px| column >= 2.6 * CELL * px && size.y >= 15.0 * CELL * px)
        .unwrap_or(1.0)
}

/// Where an ant of `kind` in `state` belongs, before slots are assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Home {
    Queen,
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
    let visible: Vec<&Node> = snapshot
        .nodes
        .iter()
        .filter(|n| !input.hidden.contains(&n.id))
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
            if home == Home::Trail && !parents.get(&n.id).is_some_and(|p| shown.contains(p)) {
                home = Home::Chamber;
            }
            (&n.id, home)
        })
        .collect();

    // Tunnels: runs in run-list order, then runs the list lacks, then repos by name.
    let mut keys: Vec<TunnelKey> = Vec::new();
    for n in &visible {
        if matches!(homes[&n.id], Home::Queen | Home::Trail) {
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
    };
    keys.sort_by_key(run_order);
    let tunnel_of: HashMap<&TunnelKey, usize> =
        keys.iter().enumerate().map(|(i, k)| (k, i)).collect();
    let px = pixel_scale(rect.size(), keys.len());
    let a = CELL * px;

    // Surface band: sky, the landing on the surface, chambers just under it. The landing
    // and storage grow a row per row-full of ants.
    let count = |home: Home| homes.values().filter(|h| **h == home).count();
    let rows = |n: usize, per_row: usize| n.div_ceil(per_row).max(1) as f32;
    let landing_left = rect.left() + 2.5 * a;
    let landing_right = rect.left() + (rect.width() * 0.3).max(5.0 * a);
    let landing_per_row = (((landing_right - landing_left) / a).floor() as usize).max(1);
    let landing_h = (rows(count(Home::Landing), landing_per_row) - 1.0) * ROW * a + 1.4 * a;
    let surface_y = rect.top() + (2.4 * a).max(rect.height() * 0.16).max(landing_h + 0.8 * a);
    let landing = Rect::from_min_max(
        pos2(landing_left, surface_y - landing_h),
        pos2(landing_right, surface_y),
    );
    let chamber_h = 2.4 * a;
    let queen_chamber = Rect::from_center_size(
        pos2(rect.center().x, surface_y + 0.4 * a + chamber_h / 2.0),
        vec2(4.0 * a, chamber_h),
    );
    let storage_w = (rect.width() * 0.26).max(3.0 * a);
    let storage_per_row = ((storage_w / a).floor() as usize).max(1);
    let storage_h =
        ((rows(count(Home::Storage), storage_per_row) - 1.0) * ROW * a + 2.2 * a).max(chamber_h);
    let storage = Rect::from_min_size(
        pos2(rect.right() - 0.5 * a - storage_w, surface_y + 0.4 * a),
        vec2(storage_w, storage_h),
    );
    let gallery_top = queen_chamber.bottom().max(storage.bottom()) + 0.6 * a;
    let gallery = Rect::from_min_max(
        pos2(rect.left() + 0.5 * a, gallery_top),
        pos2(rect.right() - 0.5 * a, gallery_top + 1.2 * a),
    );

    // Each tunnel's chamber needs a row per chamber-full of its ants.
    let n = keys.len().max(1);
    let column_w = rect.width() / n as f32;
    let chamber_w = (column_w - 0.6 * a).max(a);
    let per_row = ((chamber_w / a).floor() as usize).max(1);
    let mut chamber_counts = vec![0usize; keys.len()];
    for node in &visible {
        if homes[&node.id] == Home::Chamber {
            chamber_counts[tunnel_of[&tunnel_key(node, &repos)]] += 1;
        }
    }
    let below = (rect.bottom() - 0.3 * a) - (gallery.bottom() + a);
    let tunnels: Vec<Tunnel> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            let x = rect.left() + column_w * (i as f32 + 0.5);
            let rows = chamber_counts[i].div_ceil(per_row).max(1);
            let height = (rows as f32 - 1.0) * ROW * a + 1.4 * a;
            let progress = match key {
                TunnelKey::Run(id) => snapshot
                    .runs
                    .iter()
                    .find(|r| &r.id == id)
                    .and_then(|r| r.task_counts)
                    .filter(|c| c.total() > 0)
                    .map(|c| c.completed as f32 / c.total() as f32),
                TunnelKey::Repo(_) => None,
            };
            let depth = progress.map_or(UNKNOWN_PROGRESS_DEPTH, |p| {
                MIN_DEPTH + (1.0 - MIN_DEPTH) * p
            });
            let room = (below - height).max(0.0);
            let top = gallery.bottom() + a + room * depth;
            let chamber =
                Rect::from_min_size(pos2(x - chamber_w / 2.0, top), vec2(chamber_w, height));
            let shaft = Rect::from_min_max(
                pos2(x - 0.6 * a, gallery.center().y),
                pos2(x + 0.6 * a, chamber.top() + 0.5 * a),
            );
            let label = match key {
                TunnelKey::Run(id) => snapshot
                    .runs
                    .iter()
                    .find(|r| &r.id == id)
                    .and_then(|r| r.objective.as_deref())
                    .unwrap_or(id),
                TunnelKey::Repo(repo) => repo,
            };
            Tunnel {
                key: key.clone(),
                label: cut(label),
                shaft,
                chamber,
                progress,
            }
        })
        .collect();

    // Slots, in snapshot order within each place.
    let mut landing_n = 0;
    let mut storage_n = 0;
    let mut mouth_n = vec![0usize; tunnels.len()];
    let mut chamber_nodes: Vec<Vec<&Node>> = vec![Vec::new(); tunnels.len()];
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
            Home::Chamber => {
                chamber_nodes[tunnel_of[&tunnel_key(node, &repos)]].push(node);
                continue;
            }
            Home::Queen => (Spot::Queen, queen_chamber.center() + vec2(0.0, 0.2 * a)),
            Home::Landing => {
                let k = landing_n;
                landing_n += 1;
                let (row, col) = (k / landing_per_row, k % landing_per_row);
                let pos = pos2(
                    landing.left() + (col as f32 + 0.5) * a,
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
                    storage.right() - (col as f32 + 0.5) * a,
                    storage.bottom() - 0.5 * a - row as f32 * ROW * a,
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
        ants.push(ant(node, spot, pos, a, home, is_pr, input.now_ms));
    }
    for (t, nodes) in chamber_nodes.iter_mut().enumerate() {
        nodes.sort_by_key(|n| chamber_rank(n.state));
        let chamber = tunnels[t].chamber;
        for (k, node) in nodes.iter().enumerate() {
            let (row, col) = (k / per_row, k % per_row);
            let pos = pos2(
                chamber.right() - (col as f32 + 0.5) * a,
                chamber.bottom() - 0.5 * a - row as f32 * ROW * a,
            );
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
    // Subagents trail their parent, nearest first; nested ones once their parent is placed.
    let sub = (CELL * (px - 1.0).max(1.0)).min(a);
    let mut behind: HashMap<NodeId, usize> = HashMap::new();
    while !trailing.is_empty() {
        let before = trailing.len();
        trailing.retain(|node| {
            let parent = parents[&node.id];
            let Some(at) = ants.iter().find(|x| &x.id == parent) else {
                return true;
            };
            let k = behind.entry(parent.clone()).or_default();
            let dir = if at.facing_right { -1.0 } else { 1.0 };
            let step = (at.size + sub) / 2.0 + *k as f32 * sub;
            let pos = pos2(at.pos.x + dir * step, at.pos.y + (at.size - sub) / 2.0);
            let mut trail = ant(
                node,
                Spot::Trail(parent.clone(), *k),
                pos,
                sub,
                Home::Trail,
                false,
                input.now_ms,
            );
            trail.facing_right = at.facing_right;
            *k += 1;
            ants.push(trail);
            false
        });
        if trailing.len() == before {
            break;
        }
    }

    Scene {
        rect,
        px,
        surface_y,
        gallery,
        landing,
        queen_chamber,
        storage,
        tunnels,
        ants,
        pr_ready,
    }
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
        mark: if node.kind == NodeKind::Queen {
            None
        } else {
            mark(node.state, pr)
        },
        facing_right: pose == Pose::Dig,
        alpha: crate::ui::graph::fade_alpha(node.released_at_ms, now_ms),
    }
}
