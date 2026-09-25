//! Edge rendering: thin, low-contrast lines without arrowheads; edges of the selected or
//! hovered node are highlighted, edges to released workers fade with them. Queen edges are
//! fainter and thinner still, so the hub's spokes stay in the background until picked.
//! Edges to attendants are dashed.

use eframe::egui::{Color32, Pos2, Shape, Stroke, Vec2};
use egui_graphs::{DisplayEdge, DisplayNode, DrawContext, EdgeProps, Node};
use petgraph::EdgeType;
use petgraph::stable_graph::IndexType;

use super::shape::{FleetNodeShape, NodePayload, fade_alpha};
use crate::model::NodeKind;

/// Opacity of an edge whose endpoints are not highlighted.
pub const EDGE_ALPHA: f32 = 0.28;
/// Screen width of a plain edge, in points.
const EDGE_WIDTH: f32 = 1.0;
/// Screen width of a highlighted edge, in points.
const HIGHLIGHT_WIDTH: f32 = 2.0;
/// Opacity of a queen edge whose endpoints are not highlighted.
pub const QUEEN_EDGE_ALPHA: f32 = 0.1;
/// Screen width of a plain queen edge, in points.
const QUEEN_EDGE_WIDTH: f32 = 0.6;
/// Screen width of a highlighted queen edge, in points.
const QUEEN_HIGHLIGHT_WIDTH: f32 = 1.2;
/// Pointer distance (canvas units) within which an edge counts as hovered.
const HIT_DISTANCE: f32 = 3.0;

#[derive(Debug, Clone)]
pub struct FleetEdgeShape;

impl<E: Clone> From<EdgeProps<E>> for FleetEdgeShape {
    fn from(_: EdgeProps<E>) -> Self {
        Self
    }
}

type FleetNode<E, Ty, Ix> = Node<NodePayload, E, Ty, Ix, FleetNodeShape>;

/// Whether an edge between nodes with these interaction states is highlighted.
pub fn edge_highlighted(start: (bool, bool), end: (bool, bool)) -> bool {
    start.0 || start.1 || end.0 || end.1
}

/// Distance from `p` to the segment `a`–`b`.
pub fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq == 0.0 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

fn endpoints<E: Clone, Ty: EdgeType, Ix: IndexType>(
    start: &FleetNode<E, Ty, Ix>,
    end: &FleetNode<E, Ty, Ix>,
) -> Option<(Pos2, Pos2)> {
    let dir: Vec2 = end.location() - start.location();
    if dir.length_sq() == 0.0 {
        return None;
    }
    let from = DisplayNode::<NodePayload, E, Ty, Ix>::closest_boundary_point(start.display(), dir);
    let to = DisplayNode::<NodePayload, E, Ty, Ix>::closest_boundary_point(end.display(), -dir);
    Some((from, to))
}

fn interaction<E: Clone, Ty: EdgeType, Ix: IndexType>(n: &FleetNode<E, Ty, Ix>) -> (bool, bool) {
    (n.selected() || n.dragged(), n.hovered())
}

impl<E: Clone, Ty: EdgeType, Ix: IndexType> DisplayEdge<NodePayload, E, Ty, Ix, FleetNodeShape>
    for FleetEdgeShape
{
    fn shapes(
        &mut self,
        start: &FleetNode<E, Ty, Ix>,
        end: &FleetNode<E, Ty, Ix>,
        ctx: &DrawContext,
    ) -> Vec<Shape> {
        let Some((from, to)) = endpoints(start, end) else {
            return Vec::new();
        };
        let visuals = &ctx.ctx.global_style().visuals;
        let now = super::now_ms();
        let fade = fade_alpha(start.payload().released_at_ms, now)
            .min(fade_alpha(end.payload().released_at_ms, now));
        let queen =
            start.payload().kind == NodeKind::Queen || end.payload().kind == NodeKind::Queen;
        let (width, highlight_width, alpha) = if queen {
            (QUEEN_EDGE_WIDTH, QUEEN_HIGHLIGHT_WIDTH, QUEEN_EDGE_ALPHA)
        } else {
            (EDGE_WIDTH, HIGHLIGHT_WIDTH, EDGE_ALPHA)
        };
        let stroke = if edge_highlighted(interaction(start), interaction(end)) {
            Stroke::new(
                highlight_width,
                visuals.selection.stroke.color.gamma_multiply(fade),
            )
        } else {
            let color: Color32 = visuals.text_color().gamma_multiply(alpha * fade);
            Stroke::new(width, color)
        };
        let points = [
            ctx.meta.canvas_to_screen_pos(from),
            ctx.meta.canvas_to_screen_pos(to),
        ];
        if !queen && end.payload().kind == NodeKind::Attendant {
            return Shape::dashed_line(&points, stroke, 4.0, 3.0);
        }
        vec![Shape::line_segment(points, stroke)]
    }

    fn update(&mut self, _: &EdgeProps<E>) {}

    fn is_inside(
        &self,
        start: &FleetNode<E, Ty, Ix>,
        end: &FleetNode<E, Ty, Ix>,
        pos: Pos2,
    ) -> bool {
        endpoints(start, end).is_some_and(|(a, b)| distance_to_segment(pos, a, b) <= HIT_DISTANCE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_when_either_end_is_selected_or_hovered() {
        assert!(!edge_highlighted((false, false), (false, false)));
        assert!(edge_highlighted((true, false), (false, false)));
        assert!(edge_highlighted((false, true), (false, false)));
        assert!(edge_highlighted((false, false), (true, false)));
        assert!(edge_highlighted((false, false), (false, true)));
    }

    #[test]
    fn segment_distance() {
        let (a, b) = (Pos2::new(0.0, 0.0), Pos2::new(10.0, 0.0));
        assert_eq!(distance_to_segment(Pos2::new(5.0, 3.0), a, b), 3.0);
        assert_eq!(distance_to_segment(Pos2::new(-4.0, 3.0), a, b), 5.0);
        assert_eq!(distance_to_segment(Pos2::new(13.0, 4.0), a, b), 5.0);
        assert_eq!(distance_to_segment(Pos2::new(3.0, 4.0), a, a), 5.0);
    }
}
