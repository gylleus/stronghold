//! Force-directed layout: Fruchterman-Reingold plus run-cluster forces, paused once it settles.
//!
//! Each run forms a cluster around its coordinator; clusters are kept apart and pulled toward
//! the view center as rigid groups; loose terminals gather in their own group left of the runs.

use std::collections::{BTreeMap, HashMap};

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

/// Space around the outermost node of a cluster that counts as part of the cluster.
pub const CLUSTER_PADDING: f32 = 24.0;

/// The base layout's minimum distance in its repulsion term.
const FR_EPSILON: f32 = 1e-3;

pub fn initial_state() -> FleetLayoutState {
    state_with(Vec::new())
}

/// The initial layout state with the given cluster memberships (see [`memberships`]).
pub fn state_with(members: Vec<Option<Membership>>) -> FleetLayoutState {
    FleetLayoutState {
        base: FruchtermanReingoldState {
            k_scale: 0.5,
            ..FruchtermanReingoldState::default()
        },
        extras: (
            Extra::new(RunClustersParams {
                members,
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
    /// Part of the group of terminals that belong to no run.
    Loose,
}

impl Membership {
    fn group(self) -> GroupKey {
        match self {
            Self::Coordinator(c) | Self::Member(c) => GroupKey::Run(c),
            Self::Loose => GroupKey::Loose,
        }
    }
}

/// What [`memberships`] needs to know about a node.
#[derive(Debug, Clone, Copy)]
pub struct ClusterNode<'a> {
    /// The node's graph index.
    pub index: usize,
    pub run: Option<&'a str>,
    pub kind: NodeKind,
    /// Graph index of the node it hangs off (a subagent's terminal).
    pub parent: Option<usize>,
}

/// Cluster memberships indexed by graph node index. Runs are numbered in ID order; a node
/// without a run joins its parent's cluster, or the loose group if it has none.
pub fn memberships(nodes: &[ClusterNode]) -> Vec<Option<Membership>> {
    let runs: BTreeMap<&str, u32> = nodes
        .iter()
        .filter_map(|n| n.run)
        .collect::<std::collections::BTreeSet<_>>()
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
        out[n.index] = Some(parent_membership(nodes, &out, n.parent));
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

/// Cluster forces: (a) run members are pulled toward their coordinator, (b) overlapping
/// clusters push each other apart, (c) a coordinator is pulled toward its members' centroid,
/// (d) loose terminals are pulled into a compact group left of the runs, and (e) run clusters
/// drift toward the view center as a whole.
#[derive(Debug, Default)]
pub struct RunClusters;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunClustersParams {
    /// Membership by graph node index; nodes without one feel no cluster forces.
    pub members: Vec<Option<Membership>>,
    /// (a) Spring constant pulling members toward their coordinator.
    pub spring: f32,
    /// (b) Push per unit of overlap between two clusters.
    pub separation: f32,
    /// (b) Free space kept between neighboring clusters.
    pub gap: f32,
    /// (c) Spring constant pulling a coordinator toward its members' centroid.
    pub anchor: f32,
    /// (d) Spring constant pulling loose terminals toward their group's spot.
    pub loose_pull: f32,
    /// (e) Spring constant pulling each run cluster's centroid toward the view center.
    pub gravity: f32,
    /// (e) Cap on the per-node center pull, so far clusters are not flung inward.
    pub max_gravity: f32,
    /// Share of the base node-to-node repulsion kept between nodes of different clusters;
    /// the rest is cancelled so clusters pack by (b) instead of drifting far apart.
    pub cross_repulsion: f32,
}

impl Default for RunClustersParams {
    fn default() -> Self {
        Self {
            members: Vec::new(),
            spring: 0.3,
            separation: 5.0,
            gap: 40.0,
            anchor: 0.5,
            loose_pull: 0.4,
            gravity: 0.5,
            max_gravity: 150.0,
            cross_repulsion: 0.1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GroupKey {
    Run(u32),
    Loose,
}

#[derive(Debug)]
struct Group {
    key: GroupKey,
    /// Positions in the `indices` slice.
    nodes: Vec<usize>,
    coordinator: Option<usize>,
    centroid: Pos2,
    radius: f32,
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
        apply_cluster_forces(params, &pos, &roles, disp, area.center(), k);
    }
}

/// [`RunClusters`] on plain positions; `roles[i]` and `disp[i]` belong to `pos[i]`.
fn apply_cluster_forces(
    params: &RunClustersParams,
    pos: &[Pos2],
    roles: &[Option<Membership>],
    disp: &mut [Vec2],
    center: Pos2,
    k: f32,
) {
    cancel_cross_repulsion(params, pos, roles, disp, k);
    let groups = groups(pos, roles);
    let loose_target = loose_target(&groups, params.gap, center);

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
                let drift = (center - group.centroid) * params.gravity;
                let drift = drift * (params.max_gravity / drift.length().max(1e-3)).min(1.0);
                for &i in &group.nodes {
                    disp[i] += drift;
                }
            }
            GroupKey::Loose => {
                for &i in &group.nodes {
                    disp[i] += (loose_target - pos[i]) * params.loose_pull;
                }
            }
        }
    }

    for (a, ga) in groups.iter().enumerate() {
        for gb in &groups[a + 1..] {
            let delta = ga.centroid - gb.centroid;
            let distance = delta.length();
            let overlap = ga.radius + gb.radius + params.gap - distance;
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
}

/// Takes back part of the base FR repulsion `k² / d` between nodes of different clusters.
fn cancel_cross_repulsion(
    params: &RunClustersParams,
    pos: &[Pos2],
    roles: &[Option<Membership>],
    disp: &mut [Vec2],
    k: f32,
) {
    let cancel = (1.0 - params.cross_repulsion) * k * k;
    if cancel <= 0.0 {
        return;
    }
    for i in 0..pos.len() {
        let Some(gi) = roles[i].map(Membership::group) else {
            continue;
        };
        for j in i + 1..pos.len() {
            if roles[j].is_none_or(|r| r.group() == gi) {
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

fn groups(pos: &[Pos2], roles: &[Option<Membership>]) -> Vec<Group> {
    let mut by_key: HashMap<GroupKey, usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();
    for (i, role) in roles.iter().enumerate() {
        let Some(role) = role else { continue };
        let key = role.group();
        let g = *by_key.entry(key).or_insert_with(|| {
            groups.push(Group {
                key,
                nodes: Vec::new(),
                coordinator: None,
                centroid: Pos2::ZERO,
                radius: 0.0,
            });
            groups.len() - 1
        });
        groups[g].nodes.push(i);
        if matches!(role, Membership::Coordinator(_)) {
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

/// Where the loose group gathers: just left of the leftmost run cluster, level with the view
/// center; the view center when there are no runs.
fn loose_target(groups: &[Group], gap: f32, center: Pos2) -> Pos2 {
    let loose_radius = groups
        .iter()
        .find(|g| g.key == GroupKey::Loose)
        .map_or(0.0, |g| g.radius);
    groups
        .iter()
        .filter(|g| g.key != GroupKey::Loose)
        .map(|g| g.centroid.x - g.radius)
        .reduce(f32::min)
        .map_or(center, |left| {
            Pos2::new(left - gap - loose_radius, center.y)
        })
}

pub(super) fn centroid(points: &[Pos2]) -> Option<Pos2> {
    if points.is_empty() {
        return None;
    }
    let sum = points.iter().fold(Vec2::ZERO, |acc, p| acc + p.to_vec2());
    Some((sum / points.len() as f32).to_pos2())
}

/// Counts calm frames and says when the simulation has settled.
#[derive(Debug, Default, Clone)]
pub struct SettleTracker {
    calm_frames: u32,
}

impl SettleTracker {
    pub fn reset(&mut self) {
        self.calm_frames = 0;
    }

    /// Feeds one step's average displacement; `true` once the layout should pause.
    pub fn observe(&mut self, avg_displacement: Option<f32>) -> bool {
        match avg_displacement {
            Some(avg) if avg < SETTLE_EPSILON => self.calm_frames += 1,
            _ => self.calm_frames = 0,
        }
        self.calm_frames >= SETTLE_FRAMES
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
