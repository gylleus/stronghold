//! Node rendering: per-kind shape and size, color and glyph by state, a static ring around
//! working nodes and a ring for nodes waiting for input that pulses when they start waiting and
//! again every [`NEEDS_INPUT_REMINDER_S`], agent badge, fade-out of released workers, label under
//! the node.

use std::time::Duration;

use eframe::egui::epaint::{CircleShape, RectShape, TextShape};
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2,
};
use egui_graphs::{DisplayNode, DrawContext, NodeProps};
use petgraph::EdgeType;
use petgraph::stable_graph::IndexType;

use crate::model::{NodeId, NodeKind, NodeState, RELEASED_FADE_MS};
use crate::orca::AgentIdentity;

use super::tooltip::state_label;

/// Per-node data stored in the egui_graphs graph. Kept small: the library clones it per edge
/// per frame.
#[derive(Debug, Clone, PartialEq)]
pub struct NodePayload {
    pub id: NodeId,
    pub kind: NodeKind,
    pub state: NodeState,
    /// Released workers are drawn faded.
    pub faded: bool,
    /// When the worker was released; drives the fade-out (see [`fade_alpha`]).
    pub released_at_ms: Option<i64>,
    /// Badge letter of the agent running in the node's terminal (see [`agent_marker`]).
    pub agent_marker: Option<char>,
}

/// Label font size in canvas units; scaled by zoom and clamped on screen.
const LABEL_SIZE: f32 = 12.0;
const LABEL_MIN_PX: f32 = 6.0;
const LABEL_MAX_PX: f32 = 16.0;
/// Opacity of a worker right after release; it then fades linearly to [`FADE_END_ALPHA`].
pub const FADE_START_ALPHA: f32 = 0.6;
/// Opacity of a released worker [`RELEASED_FADE_MS`] after release.
pub const FADE_END_ALPHA: f32 = 0.1;
/// Screen scale of a hovered node.
const HOVER_SCALE: f32 = 1.15;
/// Pulse period of nodes waiting for input.
const NEEDS_INPUT_PULSE_S: f64 = 2.4;
/// Pulses a node shows right after it starts waiting for input; its ring is static after them.
const NEEDS_INPUT_FIRST_PULSES: f64 = 4.0;
/// While a node keeps waiting, it pulses once more every this many seconds as a reminder.
pub const NEEDS_INPUT_REMINDER_S: f64 = 30.0;
/// Point of the pulse wave at which the static ring of working nodes is drawn.
const WORKING_RING_WAVE: f32 = 0.3;
/// Repaint interval while a node pulses (10 fps): the pulse is slow, so a low rate looks
/// smooth enough. Pulses are short (see [`needs_input_wave`]), so this rate is rarely needed.
const ANIMATION_FRAME: Duration = Duration::from_millis(100);
/// Repaint interval while fading nodes are visible; the fade is slow.
const FADE_FRAME: Duration = Duration::from_secs(1);
/// Minimum on-screen node radius for drawing the state glyph and agent badge.
const DETAIL_MIN_PX: f32 = 5.0;

pub const ALL_STATES: [NodeState; 6] = [
    NodeState::Working,
    NodeState::Idle,
    NodeState::NeedsInput,
    NodeState::Stalled,
    NodeState::Done,
    NodeState::Failed,
];

pub fn node_radius(kind: NodeKind) -> f32 {
    match kind {
        NodeKind::Coordinator => 16.0,
        NodeKind::Worker => 10.0,
        NodeKind::Terminal => 9.0,
        NodeKind::Subagent => 6.0,
    }
}

/// Fill color of a node in the given state; at least 3:1 contrast against egui's default
/// panel background of the theme.
/// Font size of a node label on screen at `zoom`, or `None` when it is too small to draw.
fn label_px(zoom: f32, emphasized: bool) -> Option<f32> {
    let px = LABEL_SIZE * zoom;
    (px >= LABEL_MIN_PX || emphasized).then(|| px.clamp(LABEL_MIN_PX, LABEL_MAX_PX))
}

/// Size of `label` drawn at [`LABEL_MAX_PX`]; [`node_extent`] scales it to the drawn size.
pub fn label_size_at_max(ctx: &egui::Context, label: &str) -> Vec2 {
    if label.is_empty() {
        return Vec2::ZERO;
    }
    ctx.fonts_mut(|f| {
        f.layout_no_wrap(
            label.to_owned(),
            FontId::new(LABEL_MAX_PX, FontFamily::Proportional),
            Color32::WHITE,
        )
    })
    .size()
}

/// Screen-space area a node drawn at `zoom` covers, relative to its center: the shape, its
/// rings (pulse, coordinator, selection), agent badge and label. `label` is the label's size
/// from [`label_size_at_max`]. Mirrors [`FleetNodeShape`]'s drawing, ignoring the hover scale.
pub fn node_extent(payload: &NodePayload, label: Vec2, selected: bool, zoom: f32) -> Rect {
    let radius = node_radius(payload.kind) * zoom;
    let outer = if payload.kind == NodeKind::Coordinator {
        radius * 1.3
    } else {
        radius
    };
    let pulse = match payload.state {
        NodeState::Working => 1.12 + 0.35 * WORKING_RING_WAVE,
        NodeState::NeedsInput => 1.12 + 0.7,
        _ => 1.0,
    };
    // Stroke half-widths are generous so anti-aliasing stays inside too.
    let mut half = (outer * pulse + (outer * 0.16).max(1.0)).max(outer + (radius * 0.15).max(1.5));
    half = half.max(outer + (radius * 0.25).clamp(3.0, 6.0) + 2.0);
    if payload.agent_marker.is_some() && radius >= DETAIL_MIN_PX {
        half = half.max(radius * 0.8 + 10.0);
    }
    let mut extent = Rect::from_center_size(Pos2::ZERO, Vec2::splat(2.0 * half));
    if label != Vec2::ZERO
        && let Some(px) = label_px(zoom, selected)
    {
        let size = label * (px / LABEL_MAX_PX) + Vec2::splat(2.0);
        let top = outer + 3.0;
        extent = extent.union(Rect::from_min_size(Pos2::new(-size.x / 2.0, top), size));
    }
    extent
}

pub fn state_color(state: NodeState, dark_mode: bool) -> Color32 {
    let (dark, light) = match state {
        NodeState::Working => (0x38bdf8, 0x0284c7),
        NodeState::Idle => (0x8b929c, 0x6b7280),
        NodeState::NeedsInput => (0xfbbf24, 0xb86e00),
        NodeState::Stalled => (0xfb7a3c, 0xd4480f),
        NodeState::Done => (0x5fae7a, 0x2f8552),
        NodeState::Failed => (0xf04c4c, 0xc81e1e),
    };
    let rgb: u32 = if dark_mode { dark } else { light };
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The mark drawn inside a node so its state does not rely on color alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateGlyph {
    /// Working: a small play triangle.
    Play,
    /// Idle: a short dash.
    Dash,
    /// Needs input: "!".
    Exclamation,
    /// Stalled: pause bars.
    Pause,
    /// Done: a check mark.
    Check,
    /// Failed: a cross.
    Cross,
}

pub fn state_glyph(state: NodeState) -> StateGlyph {
    match state {
        NodeState::Working => StateGlyph::Play,
        NodeState::Idle => StateGlyph::Dash,
        NodeState::NeedsInput => StateGlyph::Exclamation,
        NodeState::Stalled => StateGlyph::Pause,
        NodeState::Done => StateGlyph::Check,
        NodeState::Failed => StateGlyph::Cross,
    }
}

/// Opacity of a node: 1 unless released, then fading linearly from [`FADE_START_ALPHA`] to
/// [`FADE_END_ALPHA`] over [`RELEASED_FADE_MS`].
pub fn fade_alpha(released_at_ms: Option<i64>, now_ms: i64) -> f32 {
    let Some(at) = released_at_ms else {
        return 1.0;
    };
    let t = ((now_ms - at) as f32 / RELEASED_FADE_MS as f32).clamp(0.0, 1.0);
    FADE_START_ALPHA + (FADE_END_ALPHA - FADE_START_ALPHA) * t
}

/// A smooth 0→1→0 wave with the given period in seconds; 0 at `time == 0`.
pub fn pulse(time: f64, period_s: f64) -> f32 {
    let phase = (time / period_s).fract();
    (0.5 - 0.5 * (phase * std::f64::consts::TAU).cos()) as f32
}

/// The NeedsInput ring's pulse wave `waiting_s` seconds after the node started waiting, or
/// `None` while the ring is static (drawn like wave 0, its brightest point, so pulses start and
/// end without a jump). The node pulses [`NEEDS_INPUT_FIRST_PULSES`] times, then once every
/// [`NEEDS_INPUT_REMINDER_S`]: noticeable, while the UI redraws at the animation rate only for a
/// small share of a long wait.
pub fn needs_input_wave(waiting_s: f64) -> Option<f32> {
    let first = NEEDS_INPUT_PULSE_S * NEEDS_INPUT_FIRST_PULSES;
    let waiting_s = waiting_s.max(0.0);
    if waiting_s < first {
        return Some(pulse(waiting_s, NEEDS_INPUT_PULSE_S));
    }
    // Each reminder pulse ends its interval.
    let pulse_start = NEEDS_INPUT_REMINDER_S - NEEDS_INPUT_PULSE_S;
    let t = (waiting_s - first) % NEEDS_INPUT_REMINDER_S;
    (t >= pulse_start).then(|| pulse(t - pulse_start, NEEDS_INPUT_PULSE_S))
}

/// When the NeedsInput ring of a node waiting `waiting_s` seconds next changes: the next
/// animation frame while it pulses, else the start of the next reminder pulse.
pub fn needs_input_repaint(waiting_s: f64) -> Duration {
    if needs_input_wave(waiting_s).is_some() {
        return ANIMATION_FRAME;
    }
    let first = NEEDS_INPUT_PULSE_S * NEEDS_INPUT_FIRST_PULSES;
    let t = (waiting_s - first) % NEEDS_INPUT_REMINDER_S;
    let until_pulse = NEEDS_INPUT_REMINDER_S - NEEDS_INPUT_PULSE_S - t;
    Duration::from_secs_f64(until_pulse.max(0.0)).max(ANIMATION_FRAME)
}

/// Whether nodes in this state are animated. Working nodes get a static ring instead: many
/// terminals count as working (e.g. an idle spinner keeps their output fresh), and pulsing
/// them all would keep the UI redrawing.
pub fn is_animated(state: NodeState) -> bool {
    state == NodeState::NeedsInput
}

/// Badge letter for an agent: "C" for Claude, "X" for Codex, else the name's first letter.
pub fn agent_marker(agent: Option<&AgentIdentity>) -> Option<char> {
    match agent? {
        AgentIdentity::Claude => Some('C'),
        AgentIdentity::Codex => Some('X'),
        AgentIdentity::Other(name) => name.chars().next().map(|c| c.to_ascii_uppercase()),
    }
}

/// WCAG 2 relative luminance of an opaque color.
pub fn relative_luminance(color: Color32) -> f64 {
    let channel = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// WCAG 2 contrast ratio between two opaque colors, from 1 to 21.
pub fn contrast_ratio(a: Color32, b: Color32) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Near-black or white, whichever reads better on `fill`.
pub fn glyph_color(fill: Color32) -> Color32 {
    let dark = Color32::from_gray(20);
    if contrast_ratio(fill, dark) >= contrast_ratio(fill, Color32::WHITE) {
        dark
    } else {
        Color32::WHITE
    }
}

/// Vector shapes of `glyph` centered on `center`, fitting a node of screen radius `radius`.
pub fn glyph_shapes(glyph: StateGlyph, center: Pos2, radius: f32, color: Color32) -> Vec<Shape> {
    let s = radius * 0.45;
    let stroke = Stroke::new((radius * 0.18).max(1.0), color);
    let p = |x: f32, y: f32| center + Vec2::new(x, y) * s;
    match glyph {
        StateGlyph::Play => vec![Shape::convex_polygon(
            vec![p(-0.55, -0.75), p(0.8, 0.0), p(-0.55, 0.75)],
            color,
            Stroke::NONE,
        )],
        StateGlyph::Dash => vec![Shape::line_segment([p(-0.7, 0.0), p(0.7, 0.0)], stroke)],
        StateGlyph::Exclamation => vec![
            Shape::line_segment([p(0.0, -0.95), p(0.0, 0.3)], stroke),
            Shape::circle_filled(p(0.0, 0.85), stroke.width * 0.65, color),
        ],
        StateGlyph::Pause => vec![
            Shape::line_segment([p(-0.4, -0.75), p(-0.4, 0.75)], stroke),
            Shape::line_segment([p(0.4, -0.75), p(0.4, 0.75)], stroke),
        ],
        StateGlyph::Check => vec![Shape::line(
            vec![p(-0.8, 0.05), p(-0.25, 0.6), p(0.85, -0.6)],
            stroke,
        )],
        StateGlyph::Cross => vec![
            Shape::line_segment([p(-0.7, -0.7), p(0.7, 0.7)], stroke),
            Shape::line_segment([p(-0.7, 0.7), p(0.7, -0.7)], stroke),
        ],
    }
}

/// A row of state swatches (color + glyph) with their names, for the app to place.
pub fn legend(ui: &mut Ui) {
    let dark = ui.visuals().dark_mode;
    let text_height = ui.text_style_height(&egui::TextStyle::Body);
    ui.horizontal_wrapped(|ui| {
        for state in ALL_STATES {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(text_height), Sense::hover());
                paint_swatch(ui.painter(), rect, state, dark);
                ui.label(state_label(state));
            });
            ui.add_space(6.0);
        }
    });
}

fn paint_swatch(painter: &Painter, rect: Rect, state: NodeState, dark: bool) {
    let fill = state_color(state, dark);
    let radius = rect.width() / 2.0;
    painter.circle_filled(rect.center(), radius, fill);
    painter.extend(glyph_shapes(
        state_glyph(state),
        rect.center(),
        radius,
        glyph_color(fill),
    ));
}

#[derive(Debug, Clone)]
pub struct FleetNodeShape {
    pos: Pos2,
    kind: NodeKind,
    state: NodeState,
    released_at_ms: Option<i64>,
    agent_marker: Option<char>,
    selected: bool,
    dragged: bool,
    hovered: bool,
    label: String,
    /// egui time at which this node was first drawn waiting for input; drives its pulse.
    waiting_since: Option<f64>,
}

impl From<NodeProps<NodePayload>> for FleetNodeShape {
    fn from(props: NodeProps<NodePayload>) -> Self {
        let mut shape = Self {
            pos: props.location(),
            kind: props.payload.kind,
            state: props.payload.state,
            released_at_ms: None,
            agent_marker: None,
            selected: false,
            dragged: false,
            hovered: false,
            label: String::new(),
            waiting_since: None,
        };
        shape.apply(&props);
        shape
    }
}

impl FleetNodeShape {
    /// Canvas radius, enlarged while hovered.
    fn radius(&self) -> f32 {
        let scale = if self.hovered { HOVER_SCALE } else { 1.0 };
        node_radius(self.kind) * scale
    }

    fn apply(&mut self, props: &NodeProps<NodePayload>) {
        self.pos = props.location();
        self.kind = props.payload.kind;
        self.state = props.payload.state;
        self.released_at_ms = props.payload.released_at_ms;
        self.agent_marker = props.payload.agent_marker;
        self.selected = props.selected;
        self.dragged = props.dragged;
        self.hovered = props.hovered;
        if self.label != props.label {
            self.label.clone_from(&props.label);
        }
    }

    /// Seconds this node has been waiting for input at egui time `time`, or `None` if it is not
    /// waiting.
    fn waiting_s(&mut self, time: f64) -> Option<f64> {
        if !is_animated(self.state) {
            self.waiting_since = None;
            return None;
        }
        Some(time - *self.waiting_since.get_or_insert(time))
    }

    /// Asks for the next frame at which this node's pulse or fade changes on screen.
    fn schedule_repaint(
        &self,
        ctx: &DrawContext,
        center: Pos2,
        extent: f32,
        waiting_s: Option<f64>,
    ) {
        let visible = ctx.painter.clip_rect().expand(extent).contains(center);
        if !visible {
            return;
        }
        if self.released_at_ms.is_some() {
            ctx.ctx.request_repaint_after(FADE_FRAME);
        } else if let Some(waiting_s) = waiting_s {
            ctx.ctx
                .request_repaint_after(needs_input_repaint(waiting_s));
        }
    }

    fn pulse_ring(
        &self,
        waiting_s: Option<f64>,
        center: Pos2,
        outer: f32,
        fill: Color32,
    ) -> Option<Shape> {
        let (wave, spread, max_alpha, width) = match self.state {
            NodeState::Working => (WORKING_RING_WAVE, 0.35, 0.45, 0.10),
            NodeState::NeedsInput => {
                let wave = waiting_s.and_then(needs_input_wave).unwrap_or(0.0);
                (wave, 0.7, 0.8, 0.16)
            }
            _ => return None,
        };
        let radius = outer * (1.12 + spread * wave);
        let alpha = max_alpha * (1.0 - 0.6 * wave);
        Some(Shape::circle_stroke(
            center,
            radius,
            Stroke::new((outer * width).max(1.0), fill.gamma_multiply(alpha)),
        ))
    }

    fn agent_badge(&self, ctx: &DrawContext, center: Pos2, radius: f32, alpha: f32) -> Vec<Shape> {
        let Some(letter) = self.agent_marker else {
            return Vec::new();
        };
        let visuals = &ctx.ctx.global_style().visuals;
        let badge_r = (radius * 0.45).clamp(5.0, 9.0);
        let badge_center = center + Vec2::new(radius, -radius) * 0.8;
        let bg = visuals.panel_fill.gamma_multiply(alpha);
        let fg = visuals.strong_text_color().gamma_multiply(alpha);
        let galley = ctx.ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                letter.to_string(),
                FontId::new(badge_r * 1.4, FontFamily::Monospace),
                fg,
            )
        });
        let text_pos = badge_center - galley.size() / 2.0;
        vec![
            Shape::circle_filled(badge_center, badge_r, bg),
            Shape::circle_stroke(badge_center, badge_r, Stroke::new(1.0, fg)),
            TextShape::new(text_pos, galley, fg).into(),
        ]
    }
}

impl<E: Clone, Ty: EdgeType, Ix: IndexType> DisplayNode<NodePayload, E, Ty, Ix> for FleetNodeShape {
    fn closest_boundary_point(&self, dir: Vec2) -> Pos2 {
        self.pos + dir.normalized() * self.radius()
    }

    fn shapes(&mut self, ctx: &DrawContext) -> Vec<Shape> {
        let style = ctx.ctx.global_style();
        let visuals = &style.visuals;
        let center = ctx.meta.canvas_to_screen_pos(self.pos);
        let radius = ctx.meta.canvas_to_screen_size(self.radius());
        let alpha = fade_alpha(self.released_at_ms, super::now_ms());
        let fill = state_color(self.state, visuals.dark_mode).gamma_multiply(alpha);
        let outer = if self.kind == NodeKind::Coordinator {
            radius * 1.3
        } else {
            radius
        };
        let waiting_s = self.waiting_s(ctx.ctx.input(|i| i.time));
        self.schedule_repaint(ctx, center, outer * 2.0, waiting_s);

        let stroke = if self.dragged || self.hovered {
            Stroke::new(1.5, visuals.text_color())
        } else {
            Stroke::new(1.0, visuals.extreme_bg_color.gamma_multiply(alpha))
        };

        let mut shapes = Vec::with_capacity(8);
        if self.released_at_ms.is_none() {
            shapes.extend(self.pulse_ring(waiting_s, center, outer, fill));
        }
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
                        outer,
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
        if self.selected {
            let ring = outer + (radius * 0.25).clamp(3.0, 6.0);
            shapes.push(Shape::circle_stroke(
                center,
                ring,
                Stroke::new(2.5, visuals.selection.stroke.color),
            ));
        }
        if radius >= DETAIL_MIN_PX {
            let opaque = state_color(self.state, visuals.dark_mode);
            shapes.extend(glyph_shapes(
                state_glyph(self.state),
                center,
                radius,
                glyph_color(opaque).gamma_multiply(alpha),
            ));
            shapes.extend(self.agent_badge(ctx, center, radius, alpha));
        }

        let emphasized = self.selected || self.hovered || self.dragged;
        let label_px = label_px(ctx.meta.zoom, emphasized).filter(|_| !self.label.is_empty());
        if let Some(size) = label_px {
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
            let pos = Pos2::new(center.x - galley.size().x / 2.0, center.y + outer + 3.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_contrasts_with_panel_background() {
        for (dark, visuals) in [
            (true, egui::Visuals::dark()),
            (false, egui::Visuals::light()),
        ] {
            for bg in [visuals.panel_fill, visuals.extreme_bg_color] {
                for state in ALL_STATES {
                    let ratio = contrast_ratio(state_color(state, dark), bg);
                    assert!(
                        ratio >= 3.0,
                        "{state:?} dark={dark} contrast {ratio:.2} against {bg:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn glyphs_are_readable_on_their_fill() {
        for dark in [true, false] {
            for state in ALL_STATES {
                let fill = state_color(state, dark);
                let ratio = contrast_ratio(glyph_color(fill), fill);
                assert!(
                    ratio >= 3.0,
                    "{state:?} dark={dark} glyph contrast {ratio:.2}"
                );
            }
        }
    }

    #[test]
    fn states_have_distinct_glyphs() {
        let glyphs: std::collections::HashSet<_> = ALL_STATES
            .iter()
            .map(|s| format!("{:?}", state_glyph(*s)))
            .collect();
        assert_eq!(glyphs.len(), ALL_STATES.len());
    }

    #[test]
    fn contrast_ratio_extremes() {
        assert!((contrast_ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 1e-6);
        assert!((contrast_ratio(Color32::RED, Color32::RED) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fade_alpha_decreases_over_fade_window() {
        let at = 1_000_000;
        assert_eq!(fade_alpha(None, at), 1.0);
        assert_eq!(fade_alpha(Some(at), at), FADE_START_ALPHA);
        assert_eq!(fade_alpha(Some(at), at - 5_000), FADE_START_ALPHA);
        let half = fade_alpha(Some(at), at + RELEASED_FADE_MS / 2);
        assert!((half - (FADE_START_ALPHA + FADE_END_ALPHA) / 2.0).abs() < 1e-4);
        assert!((fade_alpha(Some(at), at + RELEASED_FADE_MS) - FADE_END_ALPHA).abs() < 1e-6);
        assert!((fade_alpha(Some(at), at + 10 * RELEASED_FADE_MS) - FADE_END_ALPHA).abs() < 1e-6);
        let mut last = f32::MAX;
        for step in 0..=10 {
            let a = fade_alpha(Some(at), at + RELEASED_FADE_MS * step / 10);
            assert!(a < last);
            last = a;
        }
    }

    #[test]
    fn pulse_is_a_bounded_wave() {
        assert_eq!(pulse(0.0, 2.0), 0.0);
        assert!((pulse(1.0, 2.0) - 1.0).abs() < 1e-6);
        assert!(pulse(2.0, 2.0) < 1e-6);
        for i in 0..100 {
            let v = pulse(f64::from(i) * 0.137, 1.6);
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn needs_input_pulses_first_then_reminds() {
        let first = NEEDS_INPUT_PULSE_S * NEEDS_INPUT_FIRST_PULSES;
        // Pulsing right away, then static until the next reminder.
        assert_eq!(needs_input_wave(0.0), Some(0.0));
        assert!((needs_input_wave(NEEDS_INPUT_PULSE_S / 2.0).unwrap() - 1.0).abs() < 1e-6);
        assert!(needs_input_wave(first - 0.05).is_some());
        assert_eq!(needs_input_wave(first + 1.0), None);
        let reminder = first + NEEDS_INPUT_REMINDER_S - NEEDS_INPUT_PULSE_S;
        assert_eq!(needs_input_wave(reminder - 0.1), None);
        assert!(
            (needs_input_wave(reminder + NEEDS_INPUT_PULSE_S / 2.0).unwrap() - 1.0).abs() < 1e-6
        );
        assert_eq!(needs_input_wave(first + NEEDS_INPUT_REMINDER_S + 1.0), None);
        // Over a long wait the ring animates only a small share of the time.
        let steps: u32 = 10_000;
        let animated = (0..steps)
            .filter(|i| needs_input_wave(first + f64::from(*i) * 0.01).is_some())
            .count();
        assert!(animated * 10 <= steps as usize, "{animated} of {steps}");
    }

    #[test]
    fn needs_input_repaint_waits_for_the_next_pulse() {
        let first = NEEDS_INPUT_PULSE_S * NEEDS_INPUT_FIRST_PULSES;
        assert_eq!(needs_input_repaint(1.0), ANIMATION_FRAME);
        let wait = needs_input_repaint(first + 1.0).as_secs_f64();
        assert!((wait - (NEEDS_INPUT_REMINDER_S - NEEDS_INPUT_PULSE_S - 1.0)).abs() < 1e-6);
        let reminder = first + NEEDS_INPUT_REMINDER_S - NEEDS_INPUT_PULSE_S;
        assert_eq!(needs_input_repaint(reminder - 0.01), ANIMATION_FRAME);
        assert_eq!(needs_input_repaint(reminder + 0.5), ANIMATION_FRAME);
    }

    #[test]
    fn only_active_states_animate() {
        let animated: Vec<_> = ALL_STATES.into_iter().filter(|s| is_animated(*s)).collect();
        assert_eq!(animated, [NodeState::NeedsInput]);
    }

    #[test]
    fn agent_markers() {
        assert_eq!(agent_marker(Some(&AgentIdentity::Claude)), Some('C'));
        assert_eq!(agent_marker(Some(&AgentIdentity::Codex)), Some('X'));
        assert_eq!(
            agent_marker(Some(&AgentIdentity::Other("gemini".into()))),
            Some('G')
        );
        assert_eq!(
            agent_marker(Some(&AgentIdentity::Other(String::new()))),
            None
        );
        assert_eq!(agent_marker(None), None);
    }

    #[test]
    fn legend_renders_headless() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), legend)
            .drop_without_applying_deltas();
    }
}
