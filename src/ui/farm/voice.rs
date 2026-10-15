//! Karl's live voice travels from the portal to the recipient. No retained geometry or
//! repaint work remains once the short release discharge has finished.

use eframe::egui::{Mesh, Painter, Pos2, Rect, Shape, Vec2, pos2, vec2};

use super::{Placed, art::ArtSettings, layout::Scene, world, zoom::Camera};
use crate::model::NodeId;

const DISCHARGE_S: f64 = 0.24;

#[derive(Default)]
pub(super) struct Voice {
    target: Option<NodeId>,
    stopped: Option<f64>,
}

impl Voice {
    pub fn update(&mut self, target: Option<NodeId>, now_s: f64) {
        if let Some(target) = target {
            self.target = Some(target);
            self.stopped = None;
        } else if self.target.is_some() {
            let stopped = *self.stopped.get_or_insert(now_s);
            if now_s - stopped >= DISCHARGE_S {
                self.target = None;
                self.stopped = None;
            }
        }
    }

    pub fn animating(&self, still: bool) -> bool {
        self.target.is_some() && (!still || self.stopped.is_some())
    }

    pub fn paint(&self, painter: &Painter, scene: &Scene, placed: &[Placed], style: Style<'_>) {
        let Some(target) = &self.target else { return };
        let Some((_, at)) = scene
            .dwarves
            .iter()
            .zip(placed)
            .find(|(d, _)| &d.id == target)
        else {
            // Filtered or removed recipients have no reliable position; never point to
            // another dwarf in their old slot.
            return;
        };
        let px = scene.px;
        let viewport = style.camera.transform(scene.rect).inverse() * scene.rect;
        let safe = viewport.shrink(8.0 * px);
        let from = scene.user_portal_focus();
        let to = at.pos;
        let alpha = self.stopped.map_or(1.0, |t| {
            (1.0 - (style.now_s - t) / DISCHARGE_S).clamp(0.0, 1.0) as f32
        });
        if alpha <= 0.0 {
            return;
        }
        let time = if style.still { 0.0 } else { style.now_s };
        let frame = (time * 10.0).floor() as u32;
        let bolt = bolt(from, to, px, frame);
        let mut dots = Vec::with_capacity(1536);
        let direction = (to - from).normalized();
        for (i, segment) in bolt.windows(2).enumerate() {
            let u = i as f32 / (bolt.len() - 1) as f32;
            let pulse = (u * 3.0 - time as f32 * 1.8).rem_euclid(1.0) < 0.24;
            raster(
                &mut dots,
                segment[0],
                segment[1],
                px,
                if pulse { 1.0 } else { 0.8 },
                viewport,
            );
            // Short, broken forks jump to a new joint each frame, tapering away from the
            // main discharge. Endpoints stay pinned to portal and dwarf.
            if i > 0 && (i as u32 + frame).is_multiple_of(5) {
                let normal = vec2(-direction.y, direction.x);
                let side = if noise(i as u32, frame) < 0.5 {
                    -1.0
                } else {
                    1.0
                };
                let root = segment[0];
                let elbow = root + (direction * 3.0 + normal * side * 9.0) * px;
                let tip = root + (direction * 12.0 + normal * side * 16.0) * px;
                raster(&mut dots, root, elbow, px, 0.5, viewport);
                raster(&mut dots, elbow, tip, px, 0.3, viewport);
            }
        }
        // The pulses and arrowheads travel along the jagged bolt toward the recipient.
        for i in 0..3 {
            let u = ((time as f32 * 0.6) + i as f32 / 3.0).fract();
            let (tip, heading) = super::motion::along(&bolt, u);
            arrow(&mut dots, tip, heading.normalized(), px);
        }
        let tip = edge_tip(safe, to);
        arrow(
            &mut dots,
            tip,
            if safe.contains(to) {
                direction
            } else {
                (to - tip).normalized()
            },
            px,
        );
        let mut mesh = Mesh::default();
        let rim = style.art.outline.pixels() * px;
        // A gentle breath in brightness; reduced motion freezes it with the bolt.
        let alpha = alpha * (0.88 + 0.12 * (time as f32 * std::f32::consts::TAU * 1.6).sin());
        let cyan = super::comms::RUNE_GLOW;
        for dot in &dots {
            mesh.add_colored_rect(
                Rect::from_center_size(dot.at, Vec2::splat(7.0 * px)),
                cyan.gamma_multiply(0.13 * alpha * dot.strength),
            );
        }
        for dot in &dots {
            let width = if dot.strength > 0.5 { 3.0 } else { 1.0 } * px;
            mesh.add_colored_rect(
                Rect::from_center_size(dot.at, Vec2::splat(width + 2.0 * rim)),
                world::INK.gamma_multiply(0.4 * alpha * dot.strength),
            );
        }
        for dot in &dots {
            let width = if dot.strength > 0.5 { 3.0 } else { 1.0 } * px;
            mesh.add_colored_rect(
                Rect::from_center_size(dot.at, Vec2::splat(width)),
                cyan.gamma_multiply(alpha * dot.strength),
            );
        }
        for dot in dots.iter().filter(|d| d.strength > 0.5) {
            mesh.add_colored_rect(
                Rect::from_center_size(dot.at, Vec2::splat(px)),
                world::LIGHT.gamma_multiply(alpha * dot.strength),
            );
        }
        // The hold's WorldArt::map_scene applies the chosen palette to this mesh too.
        painter.add(Shape::mesh(mesh));
    }
}

pub(super) struct Style<'a> {
    pub camera: Camera,
    pub art: &'a ArtSettings,
    pub now_s: f64,
    pub still: bool,
}

struct Dot {
    at: Pos2,
    strength: f32,
}

/// A deterministic flicker, independent of the app's random state.
fn noise(joint: u32, frame: u32) -> f32 {
    let mut n = joint.wrapping_mul(0x9e37_79b1) ^ frame.wrapping_mul(0x85eb_ca77);
    n ^= n >> 16;
    n = n.wrapping_mul(0x7feb_352d);
    n ^= n >> 15;
    (n & 0xffff) as f32 / 65535.0
}

/// Straight, irregular segments rather than a sine wave: alternating sharp knees, with
/// uneven spacing and amplitude. At most 32 segments, irrespective of camera zoom.
fn bolt(from: Pos2, to: Pos2, px: f32, frame: u32) -> Vec<Pos2> {
    let delta = to - from;
    let distance = delta.length();
    let normal = vec2(-delta.y, delta.x).normalized();
    let joints = (distance / (12.0 * px)).ceil().clamp(4.0, 32.0) as usize;
    let width = (distance * 0.12).min(9.0 * px);
    let mut points = Vec::with_capacity(joints + 1);
    points.push(from);
    for i in 1..joints {
        let seed = i as u32;
        let u = (i as f32 + (noise(seed + 71, frame) - 0.5) * 0.6) / joints as f32;
        let side = if i % 2 == 0 { -1.0 } else { 1.0 };
        let bend = side * width * (0.25 + 0.75 * noise(seed, frame));
        points.push(from + delta * u + normal * bend);
    }
    points.push(to);
    points
}

fn raster(dots: &mut Vec<Dot>, from: Pos2, to: Pos2, px: f32, strength: f32, view: Rect) {
    let delta = to - from;
    let steps = (delta.x.abs().max(delta.y.abs()) / px)
        .ceil()
        .clamp(1.0, 64.0) as usize;
    for i in 0..=steps {
        let at = snap(from.lerp(to, i as f32 / steps as f32), px);
        if view.expand(5.0 * px).contains(at) {
            dots.push(Dot { at, strength });
        }
    }
}

fn snap(at: Pos2, px: f32) -> Pos2 {
    pos2((at.x / px).round() * px, (at.y / px).round() * px)
}

/// Keep the arrow on screen along the bearing to an off-screen recipient.
fn edge_tip(view: Rect, to: Pos2) -> Pos2 {
    if view.contains(to) {
        return to;
    }
    let delta = to - view.center();
    let scale = (view.width() * 0.5 / delta.x.abs()).min(view.height() * 0.5 / delta.y.abs());
    view.center() + delta * scale
}

fn arrow(dots: &mut Vec<Dot>, tip: Pos2, direction: Vec2, px: f32) {
    let normal = vec2(-direction.y, direction.x);
    for step in 0..5 {
        for side in [-1.0, 1.0] {
            let at = tip + (-direction + normal * side) * step as f32 * px;
            dots.push(Dot {
                at: snap(at, px),
                strength: 1.0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_cancel_and_restart_leave_no_idle_animation() {
        let mut voice = Voice::default();
        assert!(!voice.animating(false));
        voice.update(Some(NodeId::king()), 1.0);
        assert!(voice.animating(false));
        assert!(!voice.animating(true));
        voice.update(None, 2.0);
        assert!(voice.animating(true));
        voice.update(None, 2.1);
        assert_eq!(voice.stopped, Some(2.0));
        voice.update(Some(NodeId::king()), 2.2);
        assert_eq!(voice.stopped, None);
        voice.update(None, 3.0);
        voice.update(None, 3.25);
        assert!(voice.target.is_none());
        assert!(!voice.animating(false));
    }

    #[test]
    fn offscreen_arrow_keeps_the_recipients_bearing() {
        let view = Rect::from_min_max(pos2(10.0, 20.0), pos2(210.0, 120.0));
        for to in [pos2(500.0, 70.0), pos2(110.0, -80.0), pos2(-90.0, 270.0)] {
            let tip = edge_tip(view, to);
            assert!(view.contains(tip));
            assert!(
                ((tip - view.center()).normalized() - (to - view.center()).normalized()).length()
                    < 0.001
            );
        }
        assert_eq!(edge_tip(view, view.center()), view.center());
    }
}
