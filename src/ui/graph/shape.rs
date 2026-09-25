//! Node rendering: per-kind shape and size, color by state, label under the node.

use eframe::egui::epaint::{CircleShape, RectShape, TextShape};
use eframe::egui::{
    Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2,
};
use egui_graphs::{DisplayNode, DrawContext, NodeProps};
use petgraph::EdgeType;
use petgraph::stable_graph::IndexType;

use crate::model::{NodeId, NodeKind, NodeState};

/// Per-node data stored in the egui_graphs graph. Kept small: the library clones it per edge
/// per frame.
#[derive(Debug, Clone, PartialEq)]
pub struct NodePayload {
    pub id: NodeId,
    pub kind: NodeKind,
    pub state: NodeState,
    /// Released workers are drawn faded.
    pub faded: bool,
}

/// Label font size in canvas units; scaled by zoom and clamped on screen.
const LABEL_SIZE: f32 = 12.0;
const LABEL_MIN_PX: f32 = 6.0;
const LABEL_MAX_PX: f32 = 16.0;
const FADED_ALPHA: f32 = 0.35;

pub fn node_radius(kind: NodeKind) -> f32 {
    match kind {
        NodeKind::Coordinator => 16.0,
        NodeKind::Worker => 10.0,
        NodeKind::Terminal => 9.0,
        NodeKind::Subagent => 6.0,
    }
}

/// Fill color of a node in the given state.
pub fn state_color(state: NodeState, dark_mode: bool) -> Color32 {
    let (dark, light) = match state {
        NodeState::Working => ((0x4c, 0xc3, 0x6a), (0x1e, 0x8e, 0x3e)),
        NodeState::Idle => ((0x8a, 0x8f, 0x98), (0x6b, 0x72, 0x80)),
        NodeState::NeedsInput => ((0xf2, 0xb1, 0x34), (0xc2, 0x7c, 0x0e)),
        NodeState::Stalled => ((0xf0, 0x7b, 0x3f), (0xd9, 0x5d, 0x1e)),
        NodeState::Done => ((0x5b, 0x8d, 0xef), (0x2f, 0x6f, 0xd0)),
        NodeState::Failed => ((0xe5, 0x48, 0x4d), (0xc6, 0x28, 0x28)),
    };
    let (r, g, b) = if dark_mode { dark } else { light };
    Color32::from_rgb(r, g, b)
}

#[derive(Debug, Clone)]
pub struct FleetNodeShape {
    pos: Pos2,
    kind: NodeKind,
    state: NodeState,
    faded: bool,
    selected: bool,
    dragged: bool,
    hovered: bool,
    label: String,
}

impl From<NodeProps<NodePayload>> for FleetNodeShape {
    fn from(props: NodeProps<NodePayload>) -> Self {
        let mut shape = Self {
            pos: props.location(),
            kind: props.payload.kind,
            state: props.payload.state,
            faded: props.payload.faded,
            selected: false,
            dragged: false,
            hovered: false,
            label: String::new(),
        };
        shape.apply(&props);
        shape
    }
}

impl FleetNodeShape {
    fn radius(&self) -> f32 {
        node_radius(self.kind)
    }

    fn apply(&mut self, props: &NodeProps<NodePayload>) {
        self.pos = props.location();
        self.kind = props.payload.kind;
        self.state = props.payload.state;
        self.faded = props.payload.faded;
        self.selected = props.selected;
        self.dragged = props.dragged;
        self.hovered = props.hovered;
        if self.label != props.label {
            self.label.clone_from(&props.label);
        }
    }
}

impl<E: Clone, Ty: EdgeType, Ix: IndexType> DisplayNode<NodePayload, E, Ty, Ix> for FleetNodeShape {
    fn closest_boundary_point(&self, dir: Vec2) -> Pos2 {
        self.pos + dir.normalized() * self.radius()
    }

    fn shapes(&mut self, ctx: &DrawContext) -> Vec<Shape> {
        let visuals = ctx.ctx.global_style().visuals.clone();
        let center = ctx.meta.canvas_to_screen_pos(self.pos);
        let radius = ctx.meta.canvas_to_screen_size(self.radius());
        let alpha = if self.faded { FADED_ALPHA } else { 1.0 };
        let fill = state_color(self.state, visuals.dark_mode).gamma_multiply(alpha);

        let stroke = if self.selected || self.dragged {
            Stroke::new(3.0, visuals.strong_text_color())
        } else if self.hovered {
            Stroke::new(2.0, visuals.text_color())
        } else {
            Stroke::new(1.0, visuals.extreme_bg_color.gamma_multiply(alpha))
        };

        let mut shapes = Vec::with_capacity(3);
        match self.kind {
            NodeKind::Terminal => {
                let rect = Rect::from_center_size(center, Vec2::splat(radius * 1.8));
                shapes.push(
                    RectShape::new(
                        rect,
                        CornerRadius::same((radius * 0.3).clamp(1.0, 255.0) as u8),
                        fill,
                        stroke,
                        StrokeKind::Outside,
                    )
                    .into(),
                );
            }
            kind => {
                if kind == NodeKind::Coordinator {
                    shapes.push(Shape::circle_stroke(
                        center,
                        radius * 1.3,
                        Stroke::new((radius * 0.15).max(1.0), fill),
                    ));
                }
                shapes.push(
                    CircleShape {
                        center,
                        radius,
                        fill,
                        stroke,
                    }
                    .into(),
                );
            }
        }

        let label_px = ctx.meta.canvas_to_screen_size(LABEL_SIZE);
        let emphasized = self.selected || self.hovered || self.dragged;
        if !self.label.is_empty() && (label_px >= LABEL_MIN_PX || emphasized) {
            let size = label_px.clamp(LABEL_MIN_PX, LABEL_MAX_PX);
            let color = if emphasized {
                visuals.strong_text_color()
            } else {
                visuals.text_color().gamma_multiply(alpha)
            };
            let galley = ctx.ctx.fonts_mut(|f| {
                f.layout_no_wrap(
                    self.label.clone(),
                    FontId::new(size, FontFamily::Proportional),
                    color,
                )
            });
            let below = if self.kind == NodeKind::Coordinator {
                radius * 1.3
            } else {
                radius
            };
            let pos = Pos2::new(center.x - galley.size().x / 2.0, center.y + below + 3.0);
            shapes.push(TextShape::new(pos, galley, color).into());
        }
        shapes
    }

    fn update(&mut self, props: &NodeProps<NodePayload>) {
        self.apply(props);
    }

    fn is_inside(&self, pos: Pos2) -> bool {
        (pos - self.pos).length() <= self.radius() + 2.0
    }
}
