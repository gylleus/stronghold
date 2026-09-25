//! Force-directed layout: Fruchterman-Reingold plus run-cluster and repo-region forces, paused
//! once it settles.
//!
//! Each run forms a cluster around its coordinator; each repo's loose terminals form a group.
//! Clusters and groups gather in their repo's region; regions are kept apart and pulled toward
//! the view center as rigid wholes. The queen sits at the view center with the regions arranged
//! around it: its edges do not attract (they would pull every cluster into one knot) and
//! clusters and regions keep a ring of space around it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use eframe::egui::{Pos2, Rect, Vec2};
use egui_graphs::{
    DisplayEdge, DisplayNode, Extra, ExtraForce, FruchtermanReingoldState,
    FruchtermanReingoldWithExtras, FruchtermanReingoldWithExtrasState, Graph, LayoutForceDirected,
};
use petgraph::EdgeType;
use petgraph::stable_graph::{IndexType, NodeIndex};
use serde::{Deserialize, Serialize};

use crate::model::NodeKind;

/// Forces applied on top of FR repulsion/attraction, as a cons list `(Extra<F, on>, rest)`.
pub type Extras = (Extra<RunClusters, true>, ());
pub type FleetLayoutState = FruchtermanReingoldWithExtrasState<Extras>;
pub type FleetLayout = LayoutForceDirected<FruchtermanReingoldWithExtras<Extras>>;

/// Average per-node step (canvas units) below which a frame counts as calm.
pub const SETTLE_EPSILON: f32 = 0.2;
/// Consecutive calm frames before the simulation pauses.
pub const SETTLE_FRAMES: u32 = 30;
/// Average per-node step below which a layout that stops improving also pauses: it is caught
/// in a small oscillation (a limit cycle just above [`SETTLE_EPSILON`]) that never calms down.
pub const STALL_EPSILON: f32 = 1.0;
/// Frames without the step shrinking by [`STALL_PROGRESS`] before a stalled layout pauses.
pub const STALL_FRAMES: u32 = 120;
/// Fraction the smallest step so far must shrink to by to count as progress.
pub const STALL_PROGRESS: f32 = 0.9;

/// Space around the outermost node of a cluster that counts as part of the cluster.
pub const CLUSTER_PADDING: f32 = 24.0;
/// Space between a repo region's edge and its outermost node centers: room for the node and
/// its label.
pub const REGION_PADDING: f32 = 36.0;
/// Extra space at the top of a repo region for its name.
pub const REGION_LABEL_HEIGHT: f32 = 18.0;

/// The base layout's minimum distance in its repulsion term.
const FR_EPSILON: f32 = 1e-3;

/// Region index of nodes whose repo is unknown.
const NO_REPO_INDEX: u32 = u32::MAX;

pub fn initial_state() -> FleetLayoutState {
    state_with(Vec::new(), Vec::new())
}

/// The initial layout state with the given cluster memberships and repo regions (see
/// [`memberships`] and [`repo_indices`]).
pub fn state_with(members: Vec<Option<Membership>>, repos: Vec<Option<u32>>) -> FleetLayoutState {
    FleetLayoutState {
        base: FruchtermanReingoldState {
            k_scale: 0.5,
            ..FruchtermanReingoldState::default()
        },
        extras: (
            Extra::new(RunClustersParams {
                members,
                repos,
                ..RunClustersParams::default()
            }),
            (),
        ),
    }
}

/// Which cluster a node belongs to and its role there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Membership {
    /// The center of run cluster `.0`.
    Coordinator(u32),
    /// A worker (or other node) of run cluster `.0`.
    Member(u32),
    /// Part of its repo's group of terminals that belong to no run.
    Loose,
    /// The queen, anchored at the view center.
    Queen,
}

/// What [`memberships`] and [`repo_indices`] need to know about a node.
#[derive(Debug, Clone, Copy)]
pub struct ClusterNode<'a> {
    /// The node's graph index.
    pub index: usize,
    pub run: Option<&'a str>,
    pub kind: NodeKind,
    /// Graph index of the node it hangs off (a subagent's terminal).
    pub parent: Option<usize>,
    /// The repo it is grouped under; `None` to follow its parent.
    pub repo: Option<&'a str>,
}

/// Cluster memberships indexed by graph node index. Runs are numbered in ID order; a node
/// without a run joins its parent's cluster, or the loose group if it has none.
pub fn memberships(nodes: &[ClusterNode]) -> Vec<Option<Membership>> {
    let runs: BTreeMap<&str, u32> = nodes
        .iter()
        .filter_map(|n| n.run)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .zip(0..)
        .collect();
    let len = nodes.iter().map(|n| n.index + 1).max().unwrap_or(0);
    let mut out = vec![None; len];
    for n in nodes {
        out[n.index] = n.run.map(|run| {
            if n.kind == NodeKind::Coordinator {
                Membership::Coordinator(runs[run])
            } else {
                Membership::Member(runs[run])
            }
        });
    }
    for n in nodes.iter().filter(|n| n.run.is_none()) {
        out[n.index] = Some(if n.kind == NodeKind::Queen {
            Membership::Queen
        } else {
            parent_membership(nodes, &out, n.parent)
        });
    }
    out
}

/// The membership a child of `parent` gets; follows parent chains of run-less nodes.
fn parent_membership(
    nodes: &[ClusterNode],
    known: &[Option<Membership>],
    mut parent: Option<usize>,
) -> Membership {
    for _ in 0..nodes.len() {
        let Some(p) = parent else { break };
        match known.get(p).copied().flatten() {
            Some(Membership::Coordinator(c) | Membership::Member(c)) => {
                return Membership::Member(c);
            }
            _ => parent = nodes.iter().find(|n| n.index == p).and_then(|n| n.parent),
        }
    }
    Membership::Loose
}

/// Repo region per graph node index. Repos are numbered in name order; a node without a repo
/// takes its parent's; the queen and nodes without either get `None`.
pub fn repo_indices(nodes: &[ClusterNode]) -> Vec<Option<u32>> {
    let names: BTreeMap<&str, u32> = nodes
        .iter()
        .filter_map(|n| n.repo)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .zip(0..)
        .collect();
    let by_index: HashMap<usize, &ClusterNode> = nodes.iter().map(|n| (n.index, n)).collect();
    let len = nodes.iter().map(|n| n.index + 1).max().unwrap_or(0);
    let mut out = vec![None; len];
    for n in nodes.iter().filter(|n| n.kind != NodeKind::Queen) {
        let mut at = Some(n);
        for _ in 0..nodes.len() {
            let Some(node) = at else { break };
            if let Some(repo) = node.repo {
                out[n.index] = Some(names[repo]);
                break;
            }
            at = node.parent.and_then(|p| by_index.get(&p).copied());
        }
    }
    out
}

/// Cluster forces. Run level: (a) run members are pulled toward their coordinator, (b)
/// overlapping clusters push each other apart, (c) a coordinator is pulled toward its members'
/// centroid, (d) each repo's loose terminals are pulled into a compact group. Repo level: every
/// repo is a region holding its run clusters and loose group; (e) each region drifts toward the
/// view center as a whole, (g) its groups are pulled toward the region's centroid, and (h)
/// overlapping regions push each other apart. Queen: (f) the queen is held at the view center,
/// its edges' base attraction is cancelled and clusters keep [`RunClustersParams::queen_gap`]
/// from it, and (i) regions keep out of that gap too, so they ring around it.
#[derive(Debug, Default)]
pub struct RunClusters;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunClustersParams {
    /// Membership by graph node index; nodes without one feel no cluster forces.
    pub members: Vec<Option<Membership>>,
    /// Repo region by graph node index (see [`repo_indices`]).
    #[serde(default)]
    pub repos: Vec<Option<u32>>,
    /// (a) Spring constant pulling members toward their coordinator.
    pub spring: f32,
    /// (b) Push per unit of overlap between two clusters.
    pub separation: f32,
    /// (b) Free space kept between neighboring clusters.
    pub gap: f32,
    /// (c) Spring constant pulling a coordinator toward its members' centroid.
    pub anchor: f32,
    /// (d) Spring constant pulling loose terminals toward their group's centroid.
    pub loose_pull: f32,
    /// (e) Spring constant pulling each region's centroid toward the view center.
    pub gravity: f32,
    /// (e, g) Cap on a per-node pull, so far groups are not flung inward.
    pub max_gravity: f32,
    /// Share of the base node-to-node repulsion kept between nodes of different clusters;
    /// the rest is cancelled so clusters pack by (b) instead of drifting far apart.
    pub cross_repulsion: f32,
    /// (f) Spring constant pulling the queen to the view center (capped like `max_gravity`).
    pub queen_pull: f32,
    /// (f, i) Free space kept between the queen and each cluster or region.
    pub queen_gap: f32,
    /// (f) The base layout's attraction constant, cancelled along the queen's edges.
    pub base_attract: f32,
    /// (g) Spring constant pulling a region's groups toward the region's centroid.
    #[serde(default = "default_repo_pull")]
    pub repo_pull: f32,
    /// (h, i) Push per unit of overlap between two regions, or a region and the queen.
    #[serde(default = "default_repo_separation")]
    pub repo_separation: f32,
    /// (h) Free space kept between neighboring regions.
    #[serde(default = "default_repo_gap")]
    pub repo_gap: f32,
}

fn default_repo_pull() -> f32 {
    0.3
}

fn default_repo_separation() -> f32 {
    20.0
}

fn default_repo_gap() -> f32 {
    30.0
}

impl Default for RunClustersParams {
    fn default() -> Self {
        Self {
            members: Vec::new(),
            repos: Vec::new(),
            spring: 0.3,
            separation: 5.0,
            gap: 40.0,
            anchor: 0.5,
            loose_pull: 0.4,
            gravity: 0.5,
            max_gravity: 150.0,
            cross_repulsion: 0.1,
            queen_pull: 0.5,
            queen_gap: 60.0,
            base_attract: FruchtermanReingoldState::default().c_attract,
            repo_pull: default_repo_pull(),
            repo_separation: default_repo_separation(),
            repo_gap: default_repo_gap(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GroupKey {
    Run(u32),
    /// The loose terminals of one repo.
    Loose(u32),
    Queen,
}

fn group_key(role: Membership, repo: Option<u32>) -> GroupKey {
    match role {
        Membership::Coordinator(c) | Membership::Member(c) => GroupKey::Run(c),
        Membership::Loose => GroupKey::Loose(repo.unwrap_or(NO_REPO_INDEX)),
        Membership::Queen => GroupKey::Queen,
    }
}

#[derive(Debug)]
struct Group {
    key: GroupKey,
    /// Positions in the `indices` slice.
    nodes: Vec<usize>,
    coordinator: Option<usize>,
    /// The group's region; `None` for the queen.
    repo: Option<u32>,
    centroid: Pos2,
    radius: f32,
}

/// A repo's region: the groups it holds and the area they cover.
#[derive(Debug)]
struct Region {
    /// Indices into the group list.
    groups: Vec<usize>,
    nodes: Vec<usize>,
    centroid: Pos2,
    rect: Rect,
}

impl ExtraForce for RunClusters {
    type Params = RunClustersParams;

    fn apply<N, E, Ty, Ix, Dn, De>(
        params: &Self::Params,
        g: &Graph<N, E, Ty, Ix, Dn, De>,
        indices: &[NodeIndex<Ix>],
        disp: &mut [Vec2],
        area: Rect,
        k: f32,
    ) where
        N: Clone,
        E: Clone,
        Ty: EdgeType,
        Ix: IndexType,
        Dn: DisplayNode<N, E, Ty, Ix>,
        De: DisplayEdge<N, E, Ty, Ix, Dn>,
    {
        let pos: Vec<Pos2> = indices
            .iter()
            .map(|&i| g.g().node_weight(i).map_or(Pos2::ZERO, |n| n.location()))
            .collect();
        let roles: Vec<Option<Membership>> = indices
            .iter()
            .map(|i| params.members.get(i.index()).copied().flatten())
            .collect();
        let repos: Vec<Option<u32>> = indices
            .iter()
            .map(|i| params.repos.get(i.index()).copied().flatten())
            .collect();
        let links = roles
            .iter()
            .position(|r| *r == Some(Membership::Queen))
            .map(|q| {
                let slot: HashMap<NodeIndex<Ix>, usize> =
                    indices.iter().enumerate().map(|(i, &n)| (n, i)).collect();
                g.g()
                    .neighbors_undirected(indices[q])
                    .filter_map(|n| slot.get(&n).copied())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let nodes = ForceNodes {
            pos: &pos,
            roles: &roles,
            repos: &repos,
        };
        apply_cluster_forces(params, &nodes, &links, disp, area.center(), k);
    }
}

/// Per-node inputs of [`apply_cluster_forces`], indexed alike.
struct ForceNodes<'a> {
    pos: &'a [Pos2],
    roles: &'a [Option<Membership>],
    repos: &'a [Option<u32>],
}

/// `pull`, shortened to at most `max`.
fn capped(pull: Vec2, max: f32) -> Vec2 {
    pull * (max / pull.length().max(1e-3)).min(1.0)
}

/// [`RunClusters`] on plain positions; `queen_links` are the positions of the queen's
/// neighbors.
fn apply_cluster_forces(
    params: &RunClustersParams,
    nodes: &ForceNodes<'_>,
    queen_links: &[usize],
    disp: &mut [Vec2],
    center: Pos2,
    k: f32,
) {
    let pos = nodes.pos;
    let keys: Vec<Option<GroupKey>> = nodes
        .roles
        .iter()
        .zip(nodes.repos)
        .map(|(role, repo)| role.map(|r| group_key(r, *repo)))
        .collect();
    cancel_cross_repulsion(params, pos, &keys, disp, k);
    let queen = nodes
        .roles
        .iter()
        .position(|r| *r == Some(Membership::Queen));
    if let Some(q) = queen {
        cancel_queen_attraction(params, pos, q, queen_links, disp, k);
    }
    let groups = groups(pos, &keys, nodes);

    for group in &groups {
        match group.key {
            GroupKey::Run(_) => {
                let target = group.coordinator.map_or(group.centroid, |c| pos[c]);
                for &i in &group.nodes {
                    if Some(i) != group.coordinator {
                        disp[i] += (target - pos[i]) * params.spring;
                    }
                }
                if let Some(c) = group.coordinator {
                    let members: Vec<Pos2> = group
                        .nodes
                        .iter()
                        .filter(|&&i| i != c)
                        .map(|&i| pos[i])
                        .collect();
                    if let Some(members_center) = centroid(&members) {
                        disp[c] += (members_center - pos[c]) * params.anchor;
                    }
                }
            }
            GroupKey::Loose(_) => {
                for &i in &group.nodes {
                    disp[i] += (group.centroid - pos[i]) * params.loose_pull;
                }
            }
            GroupKey::Queen => {}
        }
    }

    for (a, ga) in groups.iter().enumerate() {
        for gb in &groups[a + 1..] {
            let delta = ga.centroid - gb.centroid;
            let distance = delta.length();
            let with_queen = ga.key == GroupKey::Queen || gb.key == GroupKey::Queen;
            let gap = if with_queen {
                params.queen_gap
            } else {
                params.gap
            };
            let overlap = ga.radius + gb.radius + gap - distance;
            if overlap <= 0.0 {
                continue;
            }
            let dir = if distance > 1e-3 {
                delta / distance
            } else {
                Vec2::angled(a as f32)
            };
            let push = dir * overlap * params.separation;
            for &i in &ga.nodes {
                disp[i] += push;
            }
            for &i in &gb.nodes {
                disp[i] -= push;
            }
        }
    }

    let regions = regions(pos, &groups);
    for region in &regions {
        let drift = capped(
            (center - region.centroid) * params.gravity,
            params.max_gravity,
        );
        for &i in &region.nodes {
            disp[i] += drift;
        }
        if region.groups.len() < 2 {
            continue;
        }
        for &g in &region.groups {
            let group = &groups[g];
            let pull = capped(
                (region.centroid - group.centroid) * params.repo_pull,
                params.max_gravity,
            );
            for &i in &group.nodes {
                disp[i] += pull;
            }
        }
    }
    separate_regions(params, &regions, disp);

    if let Some(q) = queen {
        for region in &regions {
            let push = queen_exclusion(region.rect.expand(params.queen_gap), pos[q]);
            for &i in &region.nodes {
                disp[i] += push * params.repo_separation;
            }
        }
        // The queen ignores every other force: it only heads for the view center.
        disp[q] = capped((center - pos[q]) * params.queen_pull, params.max_gravity);
    }
}

/// (h) Pushes overlapping regions (with [`RunClustersParams::repo_gap`] between them) apart
/// along the axis they overlap least on.
fn separate_regions(params: &RunClustersParams, regions: &[Region], disp: &mut [Vec2]) {
    let half_gap = params.repo_gap / 2.0;
    for (a, ra) in regions.iter().enumerate() {
        for rb in &regions[a + 1..] {
            let overlap = ra.rect.expand(half_gap).intersect(rb.rect.expand(half_gap));
            if !overlap.is_positive() {
                continue;
            }
            let delta = ra.rect.center() - rb.rect.center();
            let push = if overlap.width() < overlap.height() {
                Vec2::new(away(delta.x) * overlap.width(), 0.0)
            } else {
                Vec2::new(0.0, away(delta.y) * overlap.height())
            } * (params.repo_separation / 2.0);
            for &i in &ra.nodes {
                disp[i] += push;
            }
            for &i in &rb.nodes {
                disp[i] -= push;
            }
        }
    }
}

/// The sign of `d`, or 1 when it is about zero, so coincident regions still part.
fn away(d: f32) -> f32 {
    if d.abs() > 1e-3 { d.signum() } else { 1.0 }
}

/// (i) The shortest move that takes `rect` off `queen`; zero when it is already clear.
fn queen_exclusion(rect: Rect, queen: Pos2) -> Vec2 {
    if !rect.contains(queen) {
        return Vec2::ZERO;
    }
    [
        Vec2::new(queen.x - rect.min.x, 0.0),
        Vec2::new(queen.x - rect.max.x, 0.0),
        Vec2::new(0.0, queen.y - rect.min.y),
        Vec2::new(0.0, queen.y - rect.max.y),
    ]
    .into_iter()
    .min_by(|a, b| a.length().total_cmp(&b.length()))
    .unwrap_or(Vec2::ZERO)
}

/// Takes back the base FR attraction `c · d² / k` along the queen's edges, which would pull
/// every coordinator and loose terminal into one knot around it.
fn cancel_queen_attraction(
    params: &RunClustersParams,
    pos: &[Pos2],
    queen: usize,
    links: &[usize],
    disp: &mut [Vec2],
    k: f32,
) {
    for &j in links {
        let delta = pos[j] - pos[queen];
        let distance = delta.length().max(FR_EPSILON);
        let pull = delta / distance * (params.base_attract * distance * distance / k);
        disp[queen] -= pull;
        disp[j] += pull;
    }
}

/// Takes back part of the base FR repulsion `k² / d` between nodes of different clusters.
fn cancel_cross_repulsion(
    params: &RunClustersParams,
    pos: &[Pos2],
    keys: &[Option<GroupKey>],
    disp: &mut [Vec2],
    k: f32,
) {
    let cancel = (1.0 - params.cross_repulsion) * k * k;
    if cancel <= 0.0 {
        return;
    }
    for i in 0..pos.len() {
        let Some(gi) = keys[i] else {
            continue;
        };
        for j in i + 1..pos.len() {
            if keys[j].is_none_or(|g| g == gi) {
                continue;
            }
            let delta = pos[i] - pos[j];
            let distance = delta.length().max(FR_EPSILON);
            let pull = delta / distance * (cancel / distance);
            disp[i] -= pull;
            disp[j] += pull;
        }
    }
}

fn groups(pos: &[Pos2], keys: &[Option<GroupKey>], nodes: &ForceNodes<'_>) -> Vec<Group> {
    let mut by_key: HashMap<GroupKey, usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let Some(key) = *key else { continue };
        let g = *by_key.entry(key).or_insert_with(|| {
            groups.push(Group {
                key,
                nodes: Vec::new(),
                coordinator: None,
                repo: (key != GroupKey::Queen).then(|| nodes.repos[i].unwrap_or(NO_REPO_INDEX)),
                centroid: Pos2::ZERO,
                radius: 0.0,
            });
            groups.len() - 1
        });
        groups[g].nodes.push(i);
        if matches!(nodes.roles[i], Some(Membership::Coordinator(_))) {
            groups[g].coordinator = Some(i);
        }
    }
    for group in &mut groups {
        let points: Vec<Pos2> = group.nodes.iter().map(|&i| pos[i]).collect();
        group.centroid = centroid(&points).unwrap_or(Pos2::ZERO);
        group.radius = points
            .iter()
            .map(|p| (*p - group.centroid).length())
            .fold(0.0, f32::max)
            + CLUSTER_PADDING;
    }
    groups
}

/// The repo regions of `groups`, in repo order.
fn regions(pos: &[Pos2], groups: &[Group]) -> Vec<Region> {
    let mut by_repo: BTreeMap<u32, Region> = BTreeMap::new();
    for (g, group) in groups.iter().enumerate() {
        let Some(repo) = group.repo else { continue };
        let region = by_repo.entry(repo).or_insert_with(|| Region {
            groups: Vec::new(),
            nodes: Vec::new(),
            centroid: Pos2::ZERO,
            rect: Rect::NOTHING,
        });
        region.groups.push(g);
        region.nodes.extend(&group.nodes);
    }
    let mut regions: Vec<Region> = by_repo.into_values().collect();
    for region in &mut regions {
        let points: Vec<Pos2> = region.nodes.iter().map(|&i| pos[i]).collect();
        region.centroid = centroid(&points).unwrap_or(Pos2::ZERO);
        region.rect = region_rect(&points);
    }
    regions
}

/// The area a region with nodes at `points` covers: their bounding box plus
/// [`REGION_PADDING`], with room for the repo name on top.
pub fn region_rect(points: &[Pos2]) -> Rect {
    let bounds = Rect::from_points(points);
    Rect::from_min_max(
        bounds.min - Vec2::new(REGION_PADDING, REGION_PADDING + REGION_LABEL_HEIGHT),
        bounds.max + Vec2::splat(REGION_PADDING),
    )
}

pub(super) fn centroid(points: &[Pos2]) -> Option<Pos2> {
    if points.is_empty() {
        return None;
    }
    let sum = points.iter().fold(Vec2::ZERO, |acc, p| acc + p.to_vec2());
    Some((sum / points.len() as f32).to_pos2())
}

/// Counts calm frames and says when the simulation has settled: either it is calm, or its
/// steps are small and have stopped shrinking.
#[derive(Debug, Clone)]
pub struct SettleTracker {
    calm_frames: u32,
    /// Smallest step since the steps dropped below [`STALL_EPSILON`].
    best: f32,
    /// Frames since `best` last improved by [`STALL_PROGRESS`].
    stalled_frames: u32,
}

impl Default for SettleTracker {
    fn default() -> Self {
        Self {
            calm_frames: 0,
            best: f32::INFINITY,
            stalled_frames: 0,
        }
    }
}

impl SettleTracker {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feeds one step's average displacement; `true` once the layout should pause.
    pub fn observe(&mut self, avg_displacement: Option<f32>) -> bool {
        match avg_displacement {
            Some(avg) if avg < SETTLE_EPSILON => self.calm_frames += 1,
            _ => self.calm_frames = 0,
        }
        match avg_displacement {
            Some(avg) if avg < STALL_EPSILON => {
                if avg < self.best * STALL_PROGRESS {
                    self.best = avg;
                    self.stalled_frames = 0;
                } else {
                    self.stalled_frames += 1;
                }
            }
            _ => {
                self.best = f32::INFINITY;
                self.stalled_frames = 0;
            }
        }
        self.calm_frames >= SETTLE_FRAMES || self.stalled_frames >= STALL_FRAMES
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
