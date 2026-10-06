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
use crate::notify::Severity;
use crate::orca::AgentIdentity;
use crate::ui::{art, icons};

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
    /// Parked ([`crate::park`]): drawn dimmed and asleep, in the sleeping room.
    pub parked: bool,
    /// Badge letter of the agent running in the node's terminal (see [`agent_marker`]).
    pub agent_marker: Option<char>,
    /// An attendant whose run has no live coordinator: drawn with a dashed warning ring.
    pub coordinator_gone: bool,
    /// The agent has a PR in the review queue: drawn with a "PR" badge.
    pub pr_ready: bool,
    /// Unread notifications about the node: drawn as a count badge.
    pub unread: Option<UnreadBadge>,
}

/// A node's unread notifications: how many, and the most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnreadBadge {
    pub count: u32,
    pub severity: Severity,
}

/// Label font size in canvas units; scaled by zoom and clamped on screen.
const LABEL_SIZE: f32 = 12.0;
const LABEL_MIN_PX: f32 = 6.0;
const LABEL_MAX_PX: f32 = 16.0;
/// Opacity of a worker right after release; it then fades linearly to [`FADE_END_ALPHA`].
pub const FADE_START_ALPHA: f32 = 0.6;
/// Opacity of a released worker [`RELEASED_FADE_MS`] after release.
pub const FADE_END_ALPHA: f32 = 0.1;
/// Opacity of a parked node: asleep, out of the live fleet.
pub const PARKED_ALPHA: f32 = 0.5;
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
/// State glyph font size in node radii.
const GLYPH_SCALE: f32 = 1.3;
/// Width of the kind icon and its gap before a node label, in label font sizes.
const LABEL_ICON_EM: f32 = 1.35;

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
        NodeKind::Attendant => 7.0,
        NodeKind::Worker => 10.0,
        NodeKind::Terminal => 9.0,
        NodeKind::Subagent => 6.0,
        NodeKind::Queen => 24.0,
        NodeKind::Envoy => 13.0,
    }
}

pub fn envoy_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(105, 208, 215)
    } else {
        Color32::from_rgb(20, 109, 121)
    }
}

/// Fill of the queen: a muted gold, set apart from every state color in both themes (see the
/// `queen_color_is_distinct` test) and at least 3:1 against the panel background.
pub fn queen_color(dark_mode: bool) -> Color32 {
    if dark_mode {
        Color32::from_rgb(0xd9, 0xc3, 0x8c)
    } else {
        Color32::from_rgb(0x8c, 0x7a, 0x2e)
    }
}

/// Opacity of an attendant's fill: attendants are plumbing, drawn muted.
pub const ATTENDANT_ALPHA: f32 = 0.65;
/// Radius of the dashed ring around an attendant whose coordinator is gone, in node radii.
const GONE_RING: f32 = 1.7;

/// Corners of a diamond (a square on its tip) of circumradius `radius` around `center`.
pub fn diamond(center: Pos2, radius: f32) -> Vec<Pos2> {
    vec![
        center + Vec2::new(0.0, -radius),
        center + Vec2::new(radius, 0.0),
        center + Vec2::new(0.0, radius),
        center + Vec2::new(-radius, 0.0),
    ]
}

/// A dashed circle: the "coordinator gone" marker around an attendant.
fn dashed_ring(center: Pos2, radius: f32, stroke: Stroke) -> Vec<Shape> {
    let points: Vec<Pos2> = (0..=32)
        .map(|i| center + Vec2::angled(i as f32 / 32.0 * std::f32::consts::TAU) * radius)
        .collect();
    let dash = (radius * 0.35).max(2.0);
    Shape::dashed_line(&points, stroke, dash, dash * 0.8)
}

/// Screen radius of the queen's fleet-state badge on a queen of screen radius `radius`.
fn queen_badge_radius(radius: f32) -> f32 {
    (radius * 0.38).clamp(4.0, 10.0)
}

/// Where the queen's fleet-state badge sits relative to the queen's center.
fn queen_badge_offset(radius: f32) -> Vec2 {
    Vec2::new(radius, -radius) * 0.8
}

/// Size of the Queen's avatar image in node radii: its hexagon spans 60 of its 64 units.
const QUEEN_ART_SCALE: f32 = 2.0 * 32.0 / 30.0;

/// Corners of a flat-topped hexagon of circumradius `radius` around `center`.
pub fn hexagon(center: Pos2, radius: f32) -> Vec<Pos2> {
    (0..6)
        .map(|i| center + Vec2::angled(i as f32 * std::f32::consts::FRAC_PI_3) * radius)
        .collect()
}

/// A crown outline for the queen, fitting a node of screen radius `radius`.
fn crown_shape(center: Pos2, radius: f32, color: Color32) -> Shape {
    let s = radius * 0.42;
    let p = |x: f32, y: f32| center + Vec2::new(x, y) * s;
    let points = vec![
        p(-0.9, 0.55),
        p(-0.9, -0.45),
        p(-0.45, 0.05),
        p(0.0, -0.7),
        p(0.45, 0.05),
        p(0.9, -0.45),
        p(0.9, 0.55),
    ];
    Shape::closed_line(points, Stroke::new((radius * 0.1).max(1.0), color))
}

/// Fill color of a node in the given state; at least 3:1 contrast against egui's default
/// panel background of the theme.
/// Font size of a node label on screen at `zoom`, or `None` when it is too small to draw.
fn label_px(zoom: f32, emphasized: bool) -> Option<f32> {
    let px = LABEL_SIZE * zoom;
    (px >= LABEL_MIN_PX || emphasized).then(|| px.clamp(LABEL_MIN_PX, LABEL_MAX_PX))
}

/// A node label led by its kind's icon, both at `px`.
fn label_job(
    ctx: &egui::Context,
    icon: char,
    label: &str,
    px: f32,
    color: Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let format = |family| egui::TextFormat::simple(FontId::new(px, family), color);
    job.append(&icon.to_string(), 0.0, format(icons::family(ctx)));
    job.append(
        label,
        px * (LABEL_ICON_EM - 1.0),
        format(FontFamily::Proportional),
    );
    job
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
    if payload.kind == NodeKind::Queen {
        half = half.max(queen_badge_offset(radius).x + queen_badge_radius(radius) + 2.0);
    }
    if payload.coordinator_gone {
        half = half.max(radius * GONE_RING + 2.0);
    }
    let mut extent = Rect::from_center_size(Pos2::ZERO, Vec2::splat(2.0 * half));
    if label != Vec2::ZERO
        && let Some(px) = label_px(zoom, selected)
    {
        let mut size = label * (px / LABEL_MAX_PX) + Vec2::splat(2.0);
        if payload.kind != NodeKind::Queen {
            size.x += px * LABEL_ICON_EM;
        }
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

/// The mark drawn inside a node so its state does not rely on color alone: the state's
/// Phosphor icon (see [`icons::state_icon`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateGlyph(pub char);

pub fn state_glyph(state: NodeState) -> StateGlyph {
    StateGlyph(icons::state_icon(state))
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

/// `glyph` centered on `center`, fitting a node of screen radius `radius`. The font size is
/// rounded to half pixels so zooming reuses a few rasterized sizes instead of one per frame.
pub fn glyph_shape(
    ctx: &egui::Context,
    glyph: StateGlyph,
    center: Pos2,
    radius: f32,
    color: Color32,
) -> Shape {
    let px = ((radius * GLYPH_SCALE * 2.0).round() / 2.0).max(1.0);
    let font = FontId::new(px, icons::family(ctx));
    let galley = ctx.fonts_mut(|f| f.layout_no_wrap(glyph.0.to_string(), font, color));
    // Centered on the drawn glyph: the icon font's line box is not centered on its icons.
    let visual = galley
        .rows
        .first()
        .map_or(Rect::from_min_size(Pos2::ZERO, galley.size()), |row| {
            row.visuals.mesh_bounds.translate(row.pos.to_vec2())
        });
    TextShape::new(center - visual.center().to_vec2(), galley, color).into()
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
                ui.label(egui::RichText::new(state_label(state)).small().weak());
            });
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.add(art::queen_avatar(text_height));
            ui.label(
                egui::RichText::new("queen (fleet hub; its badge shows the most urgent state)")
                    .small()
                    .weak(),
            );
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(text_height), Sense::hover());
            let fill = state_color(NodeState::Idle, dark).gamma_multiply(ATTENDANT_ALPHA);
            let stroke = Stroke::new(1.0, ui.visuals().extreme_bg_color);
            ui.painter().add(Shape::convex_polygon(
                diamond(rect.center(), rect.width() / 2.0),
                fill,
                stroke,
            ));
            ui.label(
                egui::RichText::new("attendant (forwards a run's events to its coordinator)")
                    .small()
                    .weak(),
            );
        });
    });
}

fn paint_swatch(painter: &Painter, rect: Rect, state: NodeState, dark: bool) {
    let fill = state_color(state, dark);
    let radius = rect.width() / 2.0;
    painter.circle_filled(rect.center(), radius, fill);
    painter.add(glyph_shape(
        painter.ctx(),
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
    parked: bool,
    agent_marker: Option<char>,
    coordinator_gone: bool,
    pr_ready: bool,
    unread: Option<UnreadBadge>,
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
            parked: false,
            agent_marker: None,
            coordinator_gone: false,
            pr_ready: false,
            unread: None,
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
        self.parked = props.payload.parked;
        self.agent_marker = props.payload.agent_marker;
        self.coordinator_gone = props.payload.coordinator_gone;
        self.pr_ready = props.payload.pr_ready;
        self.unread = props.payload.unread;
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

    /// The queen: her avatar (a crowned ant on a gold hexagon; a plain hexagon when tiny or
    /// not loaded), a badge in the fleet state's color and glyph, and her label. It never
    /// pulses; the nodes it summarizes do.
    fn queen_shapes(&self, ctx: &DrawContext) -> Vec<Shape> {
        let style = ctx.ctx.global_style();
        let visuals = &style.visuals;
        let center = ctx.meta.canvas_to_screen_pos(self.pos);
        let radius = ctx.meta.canvas_to_screen_size(self.radius());
        let fill = queen_color(visuals.dark_mode);
        let stroke = if self.dragged || self.hovered {
            Stroke::new(1.5, visuals.text_color())
        } else {
            Stroke::new(1.0, visuals.extreme_bg_color)
        };
        let avatar = (radius >= DETAIL_MIN_PX)
            .then(|| art::queen_texture(ctx.ctx))
            .flatten();
        let mut shapes = Vec::with_capacity(8);
        match avatar {
            Some(texture) => {
                let rect = Rect::from_center_size(center, Vec2::splat(radius * QUEEN_ART_SCALE));
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                shapes.push(Shape::image(texture, rect, uv, Color32::WHITE));
                if stroke.width > 1.0 {
                    shapes.push(Shape::closed_line(hexagon(center, radius), stroke));
                }
            }
            None => {
                shapes.push(Shape::convex_polygon(hexagon(center, radius), fill, stroke));
                if radius >= DETAIL_MIN_PX {
                    shapes.push(crown_shape(center, radius, glyph_color(fill)));
                }
            }
        }
        if self.selected {
            let ring = radius + (radius * 0.25).clamp(3.0, 6.0);
            shapes.push(Shape::circle_stroke(
                center,
                ring,
                Stroke::new(2.5, visuals.selection.stroke.color),
            ));
        }
        if radius >= DETAIL_MIN_PX {
            let badge_r = queen_badge_radius(radius);
            let badge = center + queen_badge_offset(radius);
            let state_fill = state_color(self.state, visuals.dark_mode);
            shapes.push(Shape::circle_filled(
                badge,
                badge_r + 1.5,
                visuals.panel_fill,
            ));
            shapes.push(Shape::circle_filled(badge, badge_r, state_fill));
            shapes.push(glyph_shape(
                ctx.ctx,
                state_glyph(self.state),
                badge,
                badge_r,
                glyph_color(state_fill),
            ));
        }
        let emphasized = self.selected || self.hovered || self.dragged;
        if let Some(size) = label_px(ctx.meta.zoom, emphasized).filter(|_| !self.label.is_empty()) {
            let color = visuals.strong_text_color();
            let galley = ctx.ctx.fonts_mut(|f| {
                f.layout_no_wrap(
                    self.label.clone(),
                    FontId::new(size, FontFamily::Proportional),
                    color,
                )
            });
            let pos = Pos2::new(center.x - galley.size().x / 2.0, center.y + radius + 3.0);
            shapes.push(TextShape::new(pos, galley, color).into());
        }
        shapes
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

/// "z Z" rising from a parked node's top left, the sleeping room's mark.
fn snores(ctx: &DrawContext, center: Pos2, radius: f32, color: Color32) -> Vec<Shape> {
    [(0.55, 0.9, "z"), (0.85, 1.25, "Z")]
        .into_iter()
        .map(|(size, rise, text)| {
            let px = (radius * size).clamp(6.0, 16.0);
            let galley = ctx.ctx.fonts_mut(|f| {
                f.layout_no_wrap(
                    text.to_owned(),
                    FontId::new(px, FontFamily::Proportional),
                    color,
                )
            });
            let pos = center + Vec2::new(-radius * rise, -radius * rise) - galley.size() / 2.0;
            TextShape::new(pos, galley, color).into()
        })
        .collect()
}

/// GitHub's pull request purple, for the "PR" badge.
pub fn pr_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0xa3, 0x71, 0xf7)
    } else {
        Color32::from_rgb(0x82, 0x50, 0xdf)
    }
}

/// A "PR" pill below-right of a node whose agent has a PR ready for review.
fn pr_badge(ctx: &DrawContext, center: Pos2, radius: f32, alpha: f32) -> Vec<Shape> {
    let dark = ctx.ctx.global_style().visuals.dark_mode;
    let height = (radius * 0.8).clamp(8.0, 14.0);
    let galley = ctx.ctx.fonts_mut(|f| {
        f.layout_no_wrap(
            "PR".to_owned(),
            FontId::new(height * 0.75, FontFamily::Proportional),
            Color32::WHITE.gamma_multiply(alpha),
        )
    });
    let size = Vec2::new(galley.size().x + height * 0.6, height);
    let rect = Rect::from_center_size(center + Vec2::new(radius, radius) * 0.85, size);
    let text_pos = rect.center() - galley.size() / 2.0;
    vec![
        RectShape::new(
            rect,
            CornerRadius::same((height / 2.0) as u8),
            pr_color(dark).gamma_multiply(alpha),
            Stroke::new(
                1.0,
                ctx.ctx
                    .global_style()
                    .visuals
                    .panel_fill
                    .gamma_multiply(alpha),
            ),
            StrokeKind::Outside,
        )
        .into(),
        TextShape::new(text_pos, galley, Color32::WHITE.gamma_multiply(alpha)).into(),
    ]
}

/// A round count badge at the node's top right, in the color of the most severe unread
/// notification.
fn unread_badge(ctx: &DrawContext, center: Pos2, radius: f32, unread: UnreadBadge) -> Vec<Shape> {
    let dark = ctx.ctx.global_style().visuals.dark_mode;
    let size = (radius * 0.75).clamp(9.0, 15.0);
    let text = if unread.count > 9 {
        "9+".to_owned()
    } else {
        unread.count.to_string()
    };
    let galley = ctx.ctx.fonts_mut(|f| {
        f.layout_no_wrap(
            text,
            FontId::new(size * 0.72, FontFamily::Proportional),
            Color32::WHITE,
        )
    });
    let width = (galley.size().x + size * 0.45).max(size);
    let rect = Rect::from_center_size(
        center + Vec2::new(radius, -radius) * 0.8,
        Vec2::new(width, size),
    );
    let text_pos = rect.center() - galley.size() / 2.0;
    vec![
        RectShape::new(
            rect,
            CornerRadius::same((size / 2.0) as u8),
            crate::ui::notifications::severity_color(unread.severity, dark),
            Stroke::new(1.0, ctx.ctx.global_style().visuals.panel_fill),
            StrokeKind::Outside,
        )
        .into(),
        TextShape::new(text_pos, galley, Color32::WHITE).into(),
    ]
}

impl<E: Clone, Ty: EdgeType, Ix: IndexType> DisplayNode<NodePayload, E, Ty, Ix> for FleetNodeShape {
    fn closest_boundary_point(&self, dir: Vec2) -> Pos2 {
        self.pos + dir.normalized() * self.radius()
    }

    fn shapes(&mut self, ctx: &DrawContext) -> Vec<Shape> {
        if self.kind == NodeKind::Queen {
            return self.queen_shapes(ctx);
        }
        let style = ctx.ctx.global_style();
        let visuals = &style.visuals;
        let center = ctx.meta.canvas_to_screen_pos(self.pos);
        let radius = ctx.meta.canvas_to_screen_size(self.radius());
        let alpha = fade_alpha(self.released_at_ms, super::now_ms());
        let alpha = if self.parked {
            alpha.min(PARKED_ALPHA)
        } else {
            alpha
        };
        let fill = if self.kind == NodeKind::Envoy {
            envoy_color(visuals.dark_mode)
        } else {
            state_color(self.state, visuals.dark_mode)
        }
        .gamma_multiply(alpha);
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
        if self.released_at_ms.is_none() && !self.parked {
            shapes.extend(self.pulse_ring(waiting_s, center, outer, fill));
        }
        match self.kind {
            NodeKind::Envoy => {
                let points = (0..3)
                    .map(|i| {
                        center
                            + Vec2::angled(
                                -std::f32::consts::FRAC_PI_2
                                    + i as f32 * std::f32::consts::TAU / 3.0,
                            ) * radius
                    })
                    .collect();
                shapes.push(Shape::convex_polygon(points, fill, stroke));
            }
            NodeKind::Attendant => {
                shapes.push(Shape::convex_polygon(
                    diamond(center, radius * 1.2),
                    fill.gamma_multiply(ATTENDANT_ALPHA),
                    stroke,
                ));
                if self.coordinator_gone {
                    let warn = Stroke::new((radius * 0.18).max(1.0), visuals.warn_fg_color);
                    shapes.extend(dashed_ring(center, radius * GONE_RING, warn));
                }
            }
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
            let opaque = if self.kind == NodeKind::Envoy {
                envoy_color(visuals.dark_mode)
            } else {
                state_color(self.state, visuals.dark_mode)
            };
            let glyph = if self.parked {
                StateGlyph(icons::MOON)
            } else {
                state_glyph(self.state)
            };
            shapes.push(glyph_shape(
                ctx.ctx,
                glyph,
                center,
                radius,
                glyph_color(opaque).gamma_multiply(alpha),
            ));
            if self.parked {
                shapes.extend(snores(ctx, center, outer, visuals.text_color()));
            }
            shapes.extend(self.agent_badge(ctx, center, radius, alpha));
            if self.pr_ready {
                shapes.extend(pr_badge(ctx, center, radius, alpha));
            }
            if let Some(unread) = self.unread {
                shapes.extend(unread_badge(ctx, center, radius, unread));
            }
        }

        let emphasized = self.selected || self.hovered || self.dragged;
        let label_px = label_px(ctx.meta.zoom, emphasized).filter(|_| !self.label.is_empty());
        if let Some(size) = label_px {
            let color = if emphasized {
                visuals.strong_text_color()
            } else {
                visuals.text_color().gamma_multiply(alpha)
            };
            let job = label_job(
                ctx.ctx,
                icons::kind_icon(self.kind),
                &self.label,
                size,
                color,
            );
            let galley = ctx.ctx.fonts_mut(|f| f.layout_job(job));
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
    fn queen_color_contrasts_and_is_distinct() {
        for (dark, visuals) in [
            (true, egui::Visuals::dark()),
            (false, egui::Visuals::light()),
        ] {
            let queen = queen_color(dark);
            for bg in [visuals.panel_fill, visuals.extreme_bg_color] {
                let ratio = contrast_ratio(queen, bg);
                assert!(ratio >= 3.0, "queen dark={dark} contrast {ratio:.2}");
            }
            assert!(contrast_ratio(glyph_color(queen), queen) >= 3.0);
            for state in ALL_STATES {
                let other = state_color(state, dark);
                let distance = [
                    (queen.r(), other.r()),
                    (queen.g(), other.g()),
                    (queen.b(), other.b()),
                ]
                .iter()
                .map(|(a, b)| (f32::from(*a) - f32::from(*b)).powi(2))
                .sum::<f32>()
                .sqrt();
                assert!(
                    distance >= 60.0,
                    "queen too close to {state:?} (dark={dark}): {distance:.0}"
                );
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
