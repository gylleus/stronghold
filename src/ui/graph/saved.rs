//! Saving and restoring the graph's node positions, zoom/pan and selection between launches.

use std::collections::{BTreeMap, HashMap};

use eframe::egui::{Pos2, Ui, Vec2};
use egui_graphs::MetadataFrame;
use serde::{Deserialize, Serialize};

use super::{FleetGraph, GraphView};
use crate::model::NodeId;

/// Most node positions a [`SavedLayout`] keeps.
pub const MAX_SAVED_POSITIONS: usize = 2_000;

/// Zoom and pan of the graph widget, as egui_graphs stores them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedView {
    pub zoom: f32,
    pub pan: [f32; 2],
}

/// What [`GraphView::export_layout`] saves and [`GraphView::import_layout`] restores.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedLayout {
    /// Canvas position per node present at save time.
    pub positions: BTreeMap<NodeId, [f32; 2]>,
    pub view: Option<SavedView>,
    /// The user had panned, zoomed or dragged, so the saved view wins over auto-fit.
    pub manual_view: bool,
    pub selected: Option<NodeId>,
}

/// Restore state waiting for nodes to appear, plus what export needs from the last frame.
#[derive(Debug, Default)]
pub(super) struct Restore {
    /// Saved positions, each applied when its node is first added and then dropped.
    positions: HashMap<NodeId, Pos2>,
    /// Saved zoom/pan, applied on the next frame.
    view: Option<SavedView>,
    /// Skip the auto-fit when the graph is first populated.
    keep_view: bool,
    /// Selected once its node appears, unless something else is selected first.
    selected: Option<NodeId>,
    /// Zoom/pan seen on the last frame.
    last_view: Option<SavedView>,
    /// The user has navigated since launch (or the restored view was manual).
    manual_view: bool,
}

impl GraphView {
    /// Positions of the nodes in the last snapshot (visible or hidden), zoom/pan and selection.
    pub fn export_layout(&self) -> SavedLayout {
        let positions: Vec<(NodeId, [f32; 2])> = if self.last_snapshot.is_none() {
            // Nothing synced yet: keep what was restored rather than wiping it.
            self.restore
                .positions
                .iter()
                .map(|(id, p)| (id.clone(), [p.x, p.y]))
                .collect()
        } else {
            self.nodes
                .keys()
                .filter_map(|id| {
                    let pos = self
                        .node_position(id)
                        .or_else(|| self.restore.positions.get(id).copied())?;
                    Some((id.clone(), [pos.x, pos.y]))
                })
                .collect()
        };
        let positions = positions
            .into_iter()
            .filter(|(_, [x, y])| x.is_finite() && y.is_finite())
            .take(MAX_SAVED_POSITIONS)
            .collect();
        SavedLayout {
            positions,
            view: self.restore.last_view.or(self.restore.view),
            manual_view: self.restore.manual_view,
            selected: self.selected().or_else(|| self.restore.selected.clone()),
        }
    }

    /// Places nodes at their saved positions when they first appear, and restores the saved
    /// zoom/pan if the user had set it by hand. Call before the first [`GraphView::sync`].
    pub fn import_layout(&mut self, saved: SavedLayout) {
        let manual = saved.manual_view && saved.view.is_some();
        self.restore = Restore {
            positions: saved
                .positions
                .into_iter()
                .filter(|(_, [x, y])| x.is_finite() && y.is_finite())
                .map(|(id, [x, y])| (id, Pos2::new(x, y)))
                .collect(),
            view: saved.view.filter(|_| manual),
            keep_view: manual,
            selected: saved.selected,
            last_view: None,
            manual_view: manual,
        };
    }

    /// Number of saved positions not yet applied.
    pub fn pending_positions(&self) -> usize {
        self.restore.positions.len()
    }

    /// Forgets saved and current positions and lays the graph out from scratch, then fits it.
    pub fn reset_layout(&mut self) {
        let (selection, selected) = (self.selection(), self.selected());
        self.restore = Restore::default();
        self.graph = FleetGraph::new();
        self.index.clear();
        self.edges.clear();
        self.nodes.clear();
        self.stashed.clear();
        self.layout_initialized = false;
        self.focus_requested = None;
        if let Some(snapshot) = self.last_snapshot.take() {
            self.sync(&snapshot);
        }
        self.set_selection(&selection, selected.as_ref());
        self.fit_requested = true;
    }

    /// The saved position for a node being added, consumed so it applies only once.
    pub(super) fn take_saved_position(&mut self, id: &NodeId) -> Option<Pos2> {
        self.restore.positions.remove(id)
    }

    /// Whether the first population of the graph should skip the auto-fit; asked once.
    pub(super) fn take_keep_view(&mut self) -> bool {
        std::mem::take(&mut self.restore.keep_view)
    }

    /// Selects the restored selection once its node is in the graph.
    pub(super) fn apply_saved_selection(&mut self) {
        if let Some(id) = self
            .restore
            .selected
            .take_if(|id| self.index.contains_key(id))
        {
            self.set_selected(Some(&id));
        }
    }

    pub(super) fn drop_saved_selection(&mut self) {
        self.restore.selected = None;
    }

    pub(super) fn mark_manual_view(&mut self) {
        self.restore.manual_view = true;
    }

    /// Applies a restored zoom/pan to the widget (once) and records the current one.
    pub(super) fn sync_saved_view(&mut self, ui: &mut Ui, before_draw: bool) {
        let mut meta = MetadataFrame::new(Some(self.id.clone())).load(ui);
        if before_draw {
            if let Some(view) = self.restore.view.take() {
                meta.zoom = view.zoom;
                meta.pan = Vec2::new(view.pan[0], view.pan[1]);
                meta.save(ui);
            }
            return;
        }
        self.restore.last_view = Some(SavedView {
            zoom: meta.zoom,
            pan: [meta.pan.x, meta.pan.y],
        });
    }
}
