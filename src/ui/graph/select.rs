//! Node selection: a primary node plus a multi-selection, changed by clicks (Shift/Cmd toggles)
//! and by dragging a selection box over empty space with the primary button.

use std::collections::BTreeSet;

use eframe::egui::{Modifiers, PointerButton, Pos2, Rect, Response, StrokeKind, Ui};
use egui_graphs::MetadataFrame;
use petgraph::stable_graph::NodeIndex;

use super::{GraphView, node_radius};
use crate::model::NodeId;

/// A selection box being dragged, from where the drag started (screen coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BoxSelect {
    origin: Pos2,
    current: Pos2,
}

impl BoxSelect {
    fn rect(&self) -> Rect {
        Rect::from_two_pos(self.origin, self.current)
    }
}

/// Whether `modifiers` add to the selection rather than replace it (Shift or Cmd/Ctrl).
pub fn is_additive(modifiers: Modifiers) -> bool {
    modifiers.shift || modifiers.command
}

/// Whether a circle at `center` with `radius` overlaps `rect`.
pub fn circle_intersects_rect(center: Pos2, radius: f32, rect: Rect) -> bool {
    let closest = Pos2::new(
        center.x.clamp(rect.min.x, rect.max.x),
        center.y.clamp(rect.min.y, rect.max.y),
    );
    (center - closest).length() <= radius
}

impl GraphView {
    /// The selected nodes (all shown with the selection ring). Filtered-out nodes are never
    /// part of it.
    pub fn selection(&self) -> BTreeSet<NodeId> {
        self.graph
            .nodes_iter()
            .filter(|(_, n)| n.selected())
            .map(|(_, n)| n.payload().id.clone())
            .collect()
    }

    /// The primary selected node: the one last clicked or selected by itself, else the first
    /// of the selection.
    pub fn selected(&self) -> Option<NodeId> {
        let selection = self.selection();
        self.primary
            .clone()
            .filter(|p| selection.contains(p))
            .or_else(|| selection.into_iter().next())
    }

    /// Selects `id` alone (or clears the selection), e.g. to mirror a selection made elsewhere.
    pub fn set_selected(&mut self, id: Option<&NodeId>) {
        let ids: BTreeSet<NodeId> = id.cloned().into_iter().collect();
        self.set_selection(&ids, id);
    }

    /// Selects exactly the visible nodes of `ids`, with `primary` as the primary if it is
    /// among them (else the current primary if kept, else the first).
    pub fn set_selection(&mut self, ids: &BTreeSet<NodeId>, primary: Option<&NodeId>) {
        self.drop_saved_selection();
        let indices: Vec<(NodeId, NodeIndex)> =
            self.index.iter().map(|(id, i)| (id.clone(), *i)).collect();
        for (id, idx) in indices {
            if let Some(node) = self.graph.node_mut(idx) {
                node.set_selected(ids.contains(&id));
            }
        }
        let visible = |id: &&NodeId| ids.contains(*id) && self.index.contains_key(*id);
        self.primary = primary
            .filter(visible)
            .or(self.primary.as_ref().filter(visible))
            .or_else(|| ids.iter().find(|id| self.index.contains_key(*id)))
            .cloned();
    }

    /// A click on node `id`: alone it selects just that node (or deselects it if it was the
    /// only one selected); with Shift/Cmd it toggles the node in the selection.
    pub fn click_node(&mut self, id: &NodeId, additive: bool) {
        let mut selection = self.selection();
        if additive {
            if selection.remove(id) {
                let primary = self.selected().filter(|p| p != id);
                self.set_selection(&selection, primary.as_ref());
            } else {
                selection.insert(id.clone());
                self.set_selection(&selection, Some(id));
            }
        } else if selection.len() == 1 && selection.contains(id) {
            self.set_selected(None);
        } else {
            self.set_selected(Some(id));
        }
    }

    /// Visible nodes whose circle overlaps `screen_rect`, for a graph widget at `widget`.
    pub fn nodes_in_screen_rect(&self, ui: &Ui, widget: Rect, screen_rect: Rect) -> Vec<NodeId> {
        let meta = MetadataFrame::new(Some(self.id.clone())).load(ui);
        let origin = widget.left_top().to_vec2() + meta.pan;
        let to_canvas = |p: Pos2| ((p - origin).to_vec2() / meta.zoom).to_pos2();
        let canvas = Rect::from_two_pos(to_canvas(screen_rect.min), to_canvas(screen_rect.max));
        self.graph
            .nodes_iter()
            .filter(|(_, n)| {
                circle_intersects_rect(n.location(), node_radius(n.payload().kind), canvas)
            })
            .map(|(_, n)| n.payload().id.clone())
            .collect()
    }

    /// Clicks and box selection after egui_graphs handled the frame's input. `clicked` is the
    /// node egui_graphs reported clicked. Returns whether the selection changed.
    pub(super) fn selection_input(
        &mut self,
        ui: &mut Ui,
        response: &Response,
        clicked: Option<&NodeId>,
    ) -> bool {
        let additive = ui.input(|i| is_additive(i.modifiers));
        let before = self.selection();
        if let Some(id) = clicked {
            self.click_node(id, additive);
        } else if response.clicked() && self.graph.hovered_node().is_none() && !additive {
            self.set_selected(None);
        }
        self.box_select(ui, response, additive);
        self.selection() != before
    }

    /// Starts, draws and finishes a selection box dragged with the primary button from empty
    /// space. A plain box replaces the selection; with Shift/Cmd it adds to it.
    fn box_select(&mut self, ui: &mut Ui, response: &Response, additive: bool) {
        let pointer = ui.input(|i| i.pointer.latest_pos());
        if response.drag_started_by(PointerButton::Primary) && self.graph.dragged_node().is_none() {
            let origin = ui.input(|i| i.pointer.press_origin());
            if let Some(origin) = origin.or(pointer) {
                self.box_select = Some(BoxSelect {
                    origin,
                    current: pointer.unwrap_or(origin),
                });
            }
        }
        let Some(mut current) = self.box_select else {
            return;
        };
        if let Some(pos) = pointer {
            current.current = pos;
        }
        let released = response.drag_stopped_by(PointerButton::Primary)
            || !ui.input(|i| i.pointer.primary_down());
        if released {
            self.box_select = None;
            let hits = self.nodes_in_screen_rect(ui, response.rect, current.rect());
            let mut selection = if additive {
                self.selection()
            } else {
                BTreeSet::new()
            };
            selection.extend(hits.iter().cloned());
            let primary = self
                .selected()
                .filter(|p| selection.contains(p))
                .or_else(|| hits.first().cloned());
            self.set_selection(&selection, primary.as_ref());
            return;
        }
        self.box_select = Some(current);
        let stroke = ui.visuals().selection.stroke;
        let fill = ui.visuals().selection.bg_fill.gamma_multiply(0.2);
        ui.painter_at(response.rect)
            .rect(current.rect(), 2.0, fill, stroke, StrokeKind::Inside);
        ui.ctx().request_repaint();
    }

    /// The selection box being dragged, in screen coordinates.
    pub fn selection_box(&self) -> Option<Rect> {
        self.box_select.map(|b| b.rect())
    }
}

/// The context menu's close entry for a multi-selection.
pub(super) fn close_selected_text(count: usize) -> String {
    format!("Close {count} selected…")
}
