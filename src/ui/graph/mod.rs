//! Force-directed fleet graph: incremental sync from snapshots, state colors, click to select.

mod edge;
mod fit;
#[cfg(test)]
mod fit_tests;
mod layout;
mod saved;
#[cfg(test)]
mod saved_tests;
mod select;
#[cfg(test)]
mod select_tests;
mod shape;
#[cfg(test)]
mod tests;
mod tooltip;
#[cfg(test)]
mod visibility_tests;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Pos2, Rect, Ui, Vec2};
use egui_graphs::{
    AnimatedState, GraphChange, MetadataFrame, SettingsInteraction, SettingsNavigation,
    SettingsStyle, get_layout_state, set_layout_state,
};
use petgraph::Directed;
use petgraph::stable_graph::{DefaultIx, EdgeIndex, NodeIndex};

use crate::claude::Usage;
use crate::model::{EdgeKind, FleetSnapshot, Node, NodeId, NodeKind};
use layout::centroid;

pub use edge::FleetEdgeShape;
pub use layout::{
    ClusterNode, Extras, FleetLayout, FleetLayoutState, Membership, RunClusters, RunClustersParams,
    SettleTracker, memberships,
};
pub use saved::{MAX_SAVED_POSITIONS, SavedLayout, SavedView};
pub use select::{circle_intersects_rect, is_additive};
pub use shape::{
    ALL_STATES, FleetNodeShape, NodePayload, StateGlyph, agent_marker, contrast_ratio, fade_alpha,
    hexagon, legend, node_radius, queen_color, state_color, state_glyph,
};
pub use tooltip::{format_ago, kind_label, preview_line, state_label};

/// The egui_graphs graph the view renders.
pub type FleetGraph =
    egui_graphs::Graph<NodePayload, EdgeKind, Directed, DefaultIx, FleetNodeShape, FleetEdgeShape>;

/// Key under which egui_graphs stores zoom/pan and layout state in egui memory.
pub const DEFAULT_GRAPH_ID: &str = "myrstack_graph";

/// Spawn distance of a new node from its run's existing nodes, in canvas units.
const SPAWN_NEAR: f32 = 40.0;
/// Spawn distance of a new node without relatives from the layout center.
const SPAWN_CENTER: f32 = 80.0;
/// Zoom factor per scrolled point.
const SCROLL_ZOOM_SPEED: f32 = 0.0015;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 10.0;
/// Margin around focused nodes, as a fraction of their bounding box.
const FOCUS_PADDING: f32 = 0.3;
/// Largest zoom a focus may pick, so a single node isn't blown up.
const FOCUS_MAX_ZOOM: f32 = 2.0;

/// What happened in the graph this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphResponse {
    /// The primary selected node after this frame.
    pub selected: Option<NodeId>,
    /// Every selected node after this frame (includes `selected`).
    pub selection: BTreeSet<NodeId>,
    /// The node under the pointer.
    pub hovered: Option<NodeId>,
    /// The node clicked this frame.
    pub clicked: Option<NodeId>,
    /// Nodes to close: "Close session…" (one node) or "Close N selected…" was picked in a
    /// node's context menu. Empty if neither was.
    pub close_requested: Vec<NodeId>,
    /// Nodes to whip now: "Whip" (one node) or "Whip N selected" was picked in a node's
    /// context menu. Empty if neither was.
    pub whip_requested: Vec<NodeId>,
}

type EdgeKey = (NodeId, NodeId, EdgeKind);

/// What a node's context menu asked for this frame.
#[derive(Debug, Default)]
struct MenuPick {
    close: Vec<NodeId>,
    whip: Vec<NodeId>,
}

/// Interactive force-directed view of a [`FleetSnapshot`].
///
/// Keeps node positions across [`GraphView::sync`] calls: only added nodes get new positions.
pub struct GraphView {
    graph: FleetGraph,
    index: HashMap<NodeId, NodeIndex>,
    edges: HashMap<EdgeKey, EdgeIndex>,
    /// Latest data per node, for tooltips.
    nodes: HashMap<NodeId, Node>,
    /// Run ID → objective, for tooltips.
    objectives: HashMap<String, String>,
    /// Claude token usage per node, for tooltips.
    usage: HashMap<NodeId, Usage>,
    id: String,
    /// Canvas point new unrelated nodes spawn around (the layout's gravity center).
    anchor: Pos2,
    layout_initialized: bool,
    wake_layout: bool,
    settle: SettleTracker,
    fit_requested: bool,
    /// Keep fitting the graph to the view while its first layout settles, until the user
    /// pans, zooms or drags.
    auto_fit: bool,
    /// Widget size the current zoom/pan was fitted to; `None` once the user navigates. The
    /// graph is fitted again whenever the widget changes size while this is set.
    fitted_size: Option<Vec2>,
    /// The graph widget's top-left corner on the last frame it was shown.
    last_top_left: Option<Pos2>,
    /// Nodes filtered out: kept out of the egui_graphs graph (so neither drawn nor laid out).
    hidden: BTreeSet<NodeId>,
    /// Positions of hidden nodes, restored when they are shown again.
    stashed: HashMap<NodeId, Pos2>,
    /// The last synced snapshot, re-applied when the hidden set changes.
    last_snapshot: Option<FleetSnapshot>,
    /// Nodes to pan/zoom onto on the next frame.
    focus_requested: Option<Vec<NodeId>>,
    /// Layout restored from a previous launch; see [`GraphView::import_layout`].
    restore: saved::Restore,
    /// Node to pan onto, keeping the zoom, on the next frame.
    center_requested: Option<NodeId>,
    /// Node whose context menu is open (the one right-clicked last).
    context_node: Option<NodeId>,
    /// The primary of the selected nodes; see [`GraphView::selected`].
    primary: Option<NodeId>,
    /// Selection box being dragged.
    box_select: Option<select::BoxSelect>,
}

impl Default for GraphView {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphView {
    pub fn new() -> Self {
        Self::with_id(DEFAULT_GRAPH_ID)
    }

    /// A view whose zoom/pan/layout state is stored under `id`; use distinct IDs for views
    /// shown at the same time.
    pub fn with_id(id: impl Into<String>) -> Self {
        Self {
            graph: FleetGraph::new(),
            index: HashMap::new(),
            edges: HashMap::new(),
            nodes: HashMap::new(),
            objectives: HashMap::new(),
            usage: HashMap::new(),
            id: id.into(),
            anchor: Pos2::new(640.0, 400.0),
            layout_initialized: false,
            wake_layout: false,
            settle: SettleTracker::default(),
            fit_requested: false,
            auto_fit: false,
            fitted_size: None,
            last_top_left: None,
            hidden: BTreeSet::new(),
            stashed: HashMap::new(),
            last_snapshot: None,
            focus_requested: None,
            restore: saved::Restore::default(),
            center_requested: None,
            context_node: None,
            primary: None,
            box_select: None,
        }
    }

    /// Claude token usage per node, shown in tooltips.
    pub fn set_usage(&mut self, usage: HashMap<NodeId, Usage>) {
        self.usage = usage;
    }

    /// Applies a snapshot: adds new nodes and edges, removes vanished ones and updates the rest
    /// in place, keeping their positions.
    pub fn sync(&mut self, snapshot: &FleetSnapshot) {
        let was_empty = self.index.is_empty();
        let mut topology_changed = false;

        let in_snapshot: HashSet<&NodeId> = snapshot.nodes.iter().map(|n| &n.id).collect();
        self.stashed.retain(|id, _| in_snapshot.contains(id));
        self.nodes.retain(|id, _| in_snapshot.contains(id));
        let gone: Vec<NodeId> = self
            .index
            .keys()
            .filter(|id| !in_snapshot.contains(id) || self.hidden.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            if let Some(idx) = self.index.remove(&id)
                && let Some(removed) = self.graph.remove_node(idx)
                && in_snapshot.contains(&id)
            {
                self.stashed.insert(id, removed.location());
            }
            topology_changed = true;
        }
        self.edges.retain(|(from, to, _), _| {
            self.index.contains_key(from) && self.index.contains_key(to)
        });

        self.objectives = snapshot
            .runs
            .iter()
            .filter_map(|r| Some((r.id.clone(), r.objective.clone()?)))
            .collect();

        let mut added: Vec<&Node> = Vec::new();
        for node in &snapshot.nodes {
            if self.hidden.contains(&node.id) {
                self.nodes.insert(node.id.clone(), node.clone());
                continue;
            }
            match self.index.get(&node.id) {
                Some(&idx) => {
                    if let Some(graph_node) = self.graph.node_mut(idx) {
                        let payload = payload(node);
                        if *graph_node.payload() != payload {
                            *graph_node.payload_mut() = payload;
                        }
                        if graph_node.props().label != node.label {
                            graph_node.set_label(node.label.clone());
                        }
                    }
                    self.nodes.insert(node.id.clone(), node.clone());
                }
                None => added.push(node),
            }
        }
        // Coordinators first so their workers can spawn next to them.
        added.sort_by_key(|n| n.kind != NodeKind::Coordinator);
        let mut restored = 0;
        for node in added {
            let saved = self.take_saved_position(&node.id);
            restored += usize::from(saved.is_some());
            let location = match self.stashed.remove(&node.id).or(saved) {
                Some(pos) => pos,
                None => self.spawn_position(node, snapshot),
            };
            let label = node.label.clone();
            let idx = self.graph.add_node_custom(payload(node), |n| {
                n.set_location(location);
                n.set_label(label);
            });
            self.index.insert(node.id.clone(), idx);
            self.nodes.insert(node.id.clone(), node.clone());
            topology_changed = true;
        }

        let wanted_edges: HashSet<EdgeKey> = snapshot
            .edges
            .iter()
            .filter(|e| self.index.contains_key(&e.from) && self.index.contains_key(&e.to))
            .map(|e| (e.from.clone(), e.to.clone(), e.kind))
            .collect();
        let stale: Vec<EdgeKey> = self
            .edges
            .keys()
            .filter(|k| !wanted_edges.contains(*k))
            .cloned()
            .collect();
        for key in stale {
            if let Some(idx) = self.edges.remove(&key) {
                self.graph.remove_edge(idx);
            }
            topology_changed = true;
        }
        for key in wanted_edges {
            if self.edges.contains_key(&key) {
                continue;
            }
            let (from, to) = (self.index[&key.0], self.index[&key.1]);
            let idx = self.graph.add_edge(from, to, key.2);
            self.edges.insert(key, idx);
            topology_changed = true;
        }

        if topology_changed {
            self.wake_layout = true;
        }
        if was_empty && !self.index.is_empty() && !self.take_keep_view() {
            self.auto_fit = true;
        }
        if restored > 0 {
            log::debug!("graph: placed {restored} nodes at saved positions");
        }
        self.apply_saved_selection();
        self.last_snapshot = Some(snapshot.clone());
    }

    /// Hides `hidden` nodes (and their edges) and shows all others, without moving anything:
    /// hidden nodes keep their last position and reappear there. Filtering alone does not wake
    /// the layout.
    pub fn set_hidden(&mut self, hidden: &BTreeSet<NodeId>) {
        if *hidden == self.hidden {
            return;
        }
        self.hidden = hidden.clone();
        if let Some(snapshot) = self.last_snapshot.take() {
            let (wake, auto_fit) = (self.wake_layout, self.auto_fit);
            self.sync(&snapshot);
            self.wake_layout = wake;
            self.auto_fit = auto_fit;
        }
    }

    /// The nodes currently hidden by [`GraphView::set_hidden`].
    pub fn hidden(&self) -> &BTreeSet<NodeId> {
        &self.hidden
    }

    pub fn is_visible(&self, id: &NodeId) -> bool {
        self.index.contains_key(id)
    }

    /// Pans and zooms onto the visible nodes of `ids` on the next frame.
    pub fn focus_nodes(&mut self, ids: &[NodeId]) {
        self.focus_requested = Some(ids.to_vec());
    }

    /// Pans onto `id`, if visible, on the next frame, keeping the zoom.
    pub fn center_on(&mut self, id: &NodeId) {
        self.center_requested = Some(id.clone());
    }

    /// Applies a pending [`GraphView::center_on`] to the pan of a graph widget of `view_size`.
    fn apply_center(&mut self, ui: &mut Ui, view_size: Vec2) -> bool {
        let Some(id) = self.center_requested.take() else {
            return false;
        };
        let Some(bounds) = self.nodes_bounds(std::slice::from_ref(&id)) else {
            return false;
        };
        let mut meta = MetadataFrame::new(Some(self.id.clone())).load(ui);
        meta.pan = center_pan(bounds.center(), view_size, meta.zoom);
        meta.save(ui);
        self.auto_fit = false;
        ui.ctx().request_repaint();
        true
    }

    /// Pans and zooms onto a run's visible coordinator and workers on the next frame.
    pub fn focus_run(&mut self, run_id: &str) {
        let mut ids: Vec<NodeId> = self
            .nodes
            .values()
            .filter(|n| n.run_id.as_deref() == Some(run_id))
            .map(|n| n.id.clone())
            .collect();
        ids.sort();
        self.focus_nodes(&ids);
    }

    /// Bounding box, in canvas coordinates, of the visible nodes of `ids` including their radii.
    pub fn nodes_bounds(&self, ids: &[NodeId]) -> Option<Rect> {
        let mut bounds: Option<Rect> = None;
        for id in ids {
            let Some(node) = self.index.get(id).and_then(|&i| self.graph.node(i)) else {
                continue;
            };
            let r = node_radius(node.payload().kind);
            let rect = Rect::from_center_size(node.location(), Vec2::splat(2.0 * r));
            bounds = Some(bounds.map_or(rect, |b| b.union(rect)));
        }
        bounds
    }

    /// Applies a pending focus request to the zoom/pan of a graph widget of `view_size`.
    fn apply_focus(&mut self, ui: &mut Ui, view_size: Vec2) -> bool {
        let Some(ids) = self.focus_requested.take() else {
            return false;
        };
        let Some(bounds) = self.nodes_bounds(&ids) else {
            return false;
        };
        let (zoom, pan) = focus_transform(bounds, view_size, FOCUS_PADDING, FOCUS_MAX_ZOOM);
        let mut meta = MetadataFrame::new(Some(self.id.clone())).load(ui);
        meta.zoom = zoom;
        meta.pan = pan;
        meta.save(ui);
        self.auto_fit = false;
        ui.ctx().request_repaint();
        true
    }

    /// Draws the graph filling the available space and handles interaction.
    pub fn show(&mut self, ui: &mut Ui) -> GraphResponse {
        let id = Some(self.id.clone());
        self.anchor = ui.ctx().content_rect().center();
        self.prepare_layout(ui);
        self.sync_saved_view(ui, true);

        // The rect egui_graphs allocates below; fitting to it before drawing keeps this frame
        // inside the widget even right after a panel opened or the window resized.
        let rect = ui.available_rect_before_wrap();
        let resized = self.fitted_size.is_some_and(|size| size != rect.size());
        let fit = (std::mem::take(&mut self.fit_requested) || self.auto_fit || resized)
            && self.apply_fit(ui, rect);
        // egui_graphs would otherwise repaint every frame even with the layout paused.
        let animating = fit
            || self.graph.dragged_node().is_some()
            || get_layout_state::<FleetLayoutState>(ui, id.clone()).is_running();

        // Selection is handled here (see `select.rs`): left-drag from empty space draws a
        // selection box, left-drag from a node moves it and right- or middle-drag pans.
        let interaction = SettingsInteraction::new()
            .with_dragging_enabled(true)
            .with_hover_enabled(true)
            .with_node_clicking_enabled(true)
            .with_node_drag_button(Some(egui::PointerButton::Primary));
        let navigation = SettingsNavigation::new()
            .with_zoom_and_pan_enabled(true)
            .with_fit_to_screen_enabled(false)
            .with_pan_buttons(&[egui::PointerButton::Secondary, egui::PointerButton::Middle]);
        let style = SettingsStyle::new().with_labels_always(true);

        let response = egui_graphs::GraphView::<FleetLayoutState, FleetLayout>::new()
            .with_id(id.clone())
            .with_interactions(&interaction)
            .with_navigations(&navigation)
            .with_styles(&style)
            .with_continuous_repaint(animating)
            .show(ui, &mut self.graph);
        self.last_top_left = Some(response.response.rect.left_top());

        let mut clicked = None;
        let mut user_navigated = false;
        let mut dragging = self.graph.dragged_node().is_some();
        for change in &response.changes {
            match change {
                GraphChange::NodeClicked { node } => clicked = self.node_id(*node),
                GraphChange::Panned { .. } | GraphChange::Zoomed { .. } if !fit => {
                    user_navigated = true;
                }
                GraphChange::NodeDragStarted { .. } | GraphChange::NodeMoved { .. } => {
                    dragging = true;
                }
                _ => {}
            }
        }
        if self.scroll_zoom(ui, &response.response) {
            user_navigated = true;
        }
        if self.apply_focus(ui, response.response.rect.size()) {
            user_navigated = true;
        }
        if self.apply_center(ui, response.response.rect.size()) {
            user_navigated = true;
        }
        let running = self.track_settling(ui, dragging);
        if user_navigated || dragging || !running {
            self.auto_fit = false;
        }
        if user_navigated {
            self.fitted_size = None;
        }
        if user_navigated || dragging {
            self.mark_manual_view();
        }
        if clicked.is_some() {
            self.drop_saved_selection();
        }
        self.selection_input(ui, &response.response, clicked.as_ref());
        self.sync_saved_view(ui, false);
        if running && !animating {
            ui.ctx().request_repaint();
        }

        let hovered = self.graph.hovered_node().and_then(|idx| self.node_id(idx));
        if let Some(node) = hovered.as_ref().and_then(|id| self.nodes.get(id))
            && !dragging
        {
            let objective = node
                .run_id
                .as_ref()
                .and_then(|r| self.objectives.get(r))
                .map(String::as_str);
            let now_ms = now_ms();
            let usage = self.usage.get(&node.id);
            response.response.clone().on_hover_ui_at_pointer(|ui| {
                tooltip::node_tooltip(ui, node, objective, usage, now_ms);
            });
        }

        let menu = self.node_context_menu(&response.response, hovered.as_ref());

        GraphResponse {
            selected: self.selected(),
            selection: self.selection(),
            hovered,
            clicked,
            close_requested: menu.close,
            whip_requested: menu.whip,
        }
    }

    /// Right-click menu of the node under the pointer; returns the nodes to close or whip, if
    /// picked. On a node of a multi-selection it acts on the whole selection.
    fn node_context_menu(
        &mut self,
        response: &egui::Response,
        hovered: Option<&NodeId>,
    ) -> MenuPick {
        if response.secondary_clicked() {
            self.context_node = hovered.cloned();
        }
        let Some(node) = self.context_node.as_ref().and_then(|id| self.nodes.get(id)) else {
            return MenuPick::default();
        };
        let selection = self.selection();
        let mut picked = Vec::new();
        let mut whip = Vec::new();
        if selection.len() > 1 && selection.contains(&node.id) {
            response.context_menu(|ui| {
                let title = format!("{} selected", selection.len());
                ui.label(egui::RichText::new(title).strong());
                ui.separator();
                if ui
                    .button(format!("Whip {} selected", selection.len()))
                    .on_hover_text(
                        "Send each selected agent a Myrstack nudge now (the Queen and \
                         subagents are skipped)",
                    )
                    .clicked()
                {
                    whip = selection.iter().cloned().collect();
                    ui.close();
                }
                let close = ui
                    .button(select::close_selected_text(selection.len()))
                    .on_hover_text(
                        "Kill the selected sessions in Orca (asks first; the Queen and \
                         subagents are skipped)",
                    );
                if close.clicked() {
                    picked = selection.iter().cloned().collect();
                    ui.close();
                }
            });
            return MenuPick {
                close: picked,
                whip,
            };
        }
        response.context_menu(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(&node.label).strong()).truncate());
            ui.separator();
            let refusal = match node.kind {
                NodeKind::Subagent => Some(crate::close::SUBAGENT_REFUSAL),
                NodeKind::Queen => Some(crate::close::QUEEN_REFUSAL),
                NodeKind::Coordinator | NodeKind::Worker | NodeKind::Terminal => None,
            };
            let whip_refusal = match node.kind {
                NodeKind::Subagent => Some("Subagents have no terminal to nudge."),
                NodeKind::Queen => Some("The Queen is Myrstack itself."),
                NodeKind::Coordinator | NodeKind::Worker | NodeKind::Terminal => None,
            };
            if ui
                .add_enabled(whip_refusal.is_none(), egui::Button::new("Whip"))
                .on_hover_text("Send this agent a Myrstack nudge now")
                .on_disabled_hover_text(whip_refusal.unwrap_or_default())
                .clicked()
            {
                whip = vec![node.id.clone()];
                ui.close();
            }
            let close = ui
                .add_enabled(refusal.is_none(), egui::Button::new("Close session…"))
                .on_hover_text("Kill this session in Orca (asks first)")
                .on_disabled_hover_text(refusal.unwrap_or_default());
            if close.clicked() {
                picked = vec![node.id.clone()];
                ui.close();
            }
        });
        MenuPick {
            close: picked,
            whip,
        }
    }

    /// Fits the whole graph into the view on the next frame.
    pub fn fit_to_screen(&mut self) {
        self.fit_requested = true;
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    pub fn contains_edge(&self, from: &NodeId, to: &NodeId) -> bool {
        match (self.index.get(from), self.index.get(to)) {
            (Some(&a), Some(&b)) => self.graph.edges_connecting(a, b).next().is_some(),
            _ => false,
        }
    }

    /// The petgraph index of a node; stable for as long as the node stays in the snapshot.
    pub fn node_index(&self, id: &NodeId) -> Option<NodeIndex> {
        self.index.get(id).copied()
    }

    /// A node's position in canvas coordinates; for a hidden node, where it will reappear.
    pub fn node_position(&self, id: &NodeId) -> Option<Pos2> {
        match self.node_index(id) {
            Some(idx) => Some(self.graph.node(idx)?.location()),
            None => self.stashed.get(id).copied(),
        }
    }

    pub fn set_node_position(&mut self, id: &NodeId, pos: Pos2) {
        if let Some(node) = self.index.get(id).and_then(|&i| self.graph.node_mut(i)) {
            node.set_location(pos);
            self.wake_layout = true;
        }
    }

    /// The underlying egui_graphs graph.
    pub fn graph(&self) -> &FleetGraph {
        &self.graph
    }

    fn node_id(&self, idx: NodeIndex) -> Option<NodeId> {
        Some(self.graph.node(idx)?.payload().id.clone())
    }

    /// Where a new node starts: the queen at the layout center; others next to their
    /// coordinator, their run's other nodes or their parent edge, else near the layout center.
    fn spawn_position(&self, node: &Node, snapshot: &FleetSnapshot) -> Pos2 {
        if node.kind == NodeKind::Queen {
            return self.anchor;
        }
        let near = self.relative_position(node, snapshot);
        let radius = if near.is_some() {
            SPAWN_NEAR
        } else {
            SPAWN_CENTER
        };
        near.unwrap_or(self.anchor) + jitter(&node.id, radius)
    }

    fn relative_position(&self, node: &Node, snapshot: &FleetSnapshot) -> Option<Pos2> {
        let placed = |id: &NodeId| self.node_position(id);
        if let Some(run_id) = &node.run_id {
            let coordinator = snapshot
                .runs
                .iter()
                .find(|r| &r.id == run_id)
                .and_then(|r| r.coordinator.as_ref())
                .filter(|c| **c != node.id)
                .and_then(placed);
            if coordinator.is_some() {
                return coordinator;
            }
            let siblings: Vec<Pos2> = self
                .nodes
                .values()
                .filter(|n| n.run_id.as_ref() == Some(run_id) && n.id != node.id)
                .filter_map(|n| placed(&n.id))
                .collect();
            if let Some(center) = centroid(&siblings) {
                return Some(center);
            }
        }
        let parent = snapshot
            .edges
            .iter()
            .filter(|e| e.to == node.id && e.kind != EdgeKind::Queen)
            .find_map(|e| placed(&e.from));
        if parent.is_some() || node.run_id.is_some() {
            return parent;
        }
        // Loose terminals join the other loose terminals, or start their group left of the runs.
        let loose: Vec<Pos2> = self
            .nodes
            .values()
            .filter(|n| n.run_id.is_none() && n.kind == NodeKind::Terminal && n.id != node.id)
            .filter_map(|n| placed(&n.id))
            .collect();
        centroid(&loose).or_else(|| {
            let placed: Vec<Pos2> = self.nodes.keys().filter_map(placed).collect();
            let left = placed.iter().map(|p| p.x).reduce(f32::min)?;
            Some(Pos2::new(left - SPAWN_CENTER, centroid(&placed)?.y))
        })
    }

    /// Run-cluster membership of every node, by graph index.
    fn memberships(&self) -> Vec<Option<Membership>> {
        let parents: HashMap<&NodeId, usize> = self
            .edges
            .keys()
            .filter(|(_, _, kind)| *kind == EdgeKind::Subagent)
            .filter_map(|(from, to, _)| Some((to, self.index.get(from)?.index())))
            .collect();
        let nodes: Vec<ClusterNode> = self
            .index
            .iter()
            .filter_map(|(id, idx)| {
                let node = self.nodes.get(id)?;
                Some(ClusterNode {
                    index: idx.index(),
                    run: node.run_id.as_deref(),
                    kind: node.kind,
                    parent: parents.get(id).copied(),
                })
            })
            .collect();
        memberships(&nodes)
    }

    /// A fresh layout state for the current graph.
    fn layout_state(&self) -> FleetLayoutState {
        layout::state_with(self.memberships())
    }

    fn prepare_layout(&mut self, ui: &mut Ui) {
        let id = Some(self.id.clone());
        if !self.layout_initialized {
            set_layout_state(ui, self.layout_state(), id.clone());
            self.layout_initialized = true;
        }
        if std::mem::take(&mut self.wake_layout) {
            let mut state = get_layout_state::<FleetLayoutState>(ui, id.clone());
            state.extras.0.params.members = self.memberships();
            state.set_running(true);
            set_layout_state(ui, state, id);
            self.settle.reset();
        }
    }

    /// Pauses the simulation once it has settled; keeps it running while a node is dragged.
    /// Returns whether it is still running.
    fn track_settling(&mut self, ui: &mut Ui, dragging: bool) -> bool {
        let id = Some(self.id.clone());
        let mut state = get_layout_state::<FleetLayoutState>(ui, id.clone());
        let running = if dragging {
            self.settle.reset();
            true
        } else if state.is_running() {
            !self.settle.observe(state.last_avg_displacement())
        } else {
            false
        };
        if running != state.is_running() {
            state.set_running(running);
            set_layout_state(ui, state, id);
        }
        running
    }

    /// Zooms around the pointer on plain scroll (egui_graphs itself only zooms on
    /// ctrl+scroll and pinch). Returns whether it zoomed.
    fn scroll_zoom(&self, ui: &mut Ui, response: &egui::Response) -> bool {
        let Some(pointer) = response.hover_pos() else {
            return false;
        };
        let (scroll, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta(), i.zoom_delta()));
        if zoom_delta != 1.0 || scroll.y == 0.0 {
            return false;
        }
        let mut meta = MetadataFrame::new(Some(self.id.clone())).load(ui);
        let new_zoom = (meta.zoom * (scroll.y * SCROLL_ZOOM_SPEED).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
        let local = pointer - response.rect.left_top();
        meta.pan = local - (local - meta.pan) * (new_zoom / meta.zoom);
        meta.zoom = new_zoom;
        meta.save(ui);
        true
    }
}

fn payload(node: &Node) -> NodePayload {
    NodePayload {
        id: node.id.clone(),
        kind: node.kind,
        state: node.state,
        faded: node.released_at_ms.is_some(),
        released_at_ms: node.released_at_ms,
        agent_marker: node
            .terminal_handle
            .as_ref()
            .and_then(|_| agent_marker(node.agent.as_ref())),
    }
}

/// A deterministic offset of length `radius` in a direction derived from `id`.
fn jitter(id: &NodeId, radius: f32) -> Vec2 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    let angle = (hasher.finish() % 3_600) as f32 / 3_600.0 * std::f32::consts::TAU;
    Vec2::angled(angle) * radius
}

/// Zoom and pan (widget-local, as egui_graphs stores them) that center `bounds` in a view of
/// `view_size` with `padding` (fraction of the bounds) around it, zooming in at most to
/// `max_zoom`.
pub fn focus_transform(bounds: Rect, view_size: Vec2, padding: f32, max_zoom: f32) -> (f32, Vec2) {
    let size = (bounds.size() * (1.0 + padding)).max(Vec2::splat(1.0));
    let zoom = (view_size.x / size.x)
        .min(view_size.y / size.y)
        .clamp(MIN_ZOOM, max_zoom.max(MIN_ZOOM));
    let pan = view_size / 2.0 - bounds.center().to_vec2() * zoom;
    (zoom, pan)
}

/// Widget-local pan that puts canvas point `point` in the middle of a view of `view_size` at
/// `zoom`.
pub fn center_pan(point: Pos2, view_size: Vec2, zoom: f32) -> Vec2 {
    view_size / 2.0 - point.to_vec2() * zoom
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
