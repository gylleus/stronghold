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
        let mut dots = Vec::with_capacity(1024);
        let direction = (to - from).normalized();
        let normal = vec2(-direction.y, direction.x);
        let distance = from.distance(to);
        let count = (distance / px).ceil().clamp(1.0, 1024.0) as usize;
        let frame = (time * 12.0).floor() as f32;
        for i in 0..=count {
            let u = i as f32 / count as f32;
            // A pinned zigzag, changing only at the pixel sprites' frame rate.
            let bend = (u * 31.0 + frame * 1.7).sin() * (u * std::f32::consts::PI).sin() * 2.5 * px;
            let at = from.lerp(to, u) + normal * bend;
            if viewport.expand(4.0 * px).contains(at) {
                let pulse = (u * 3.0 - time as f32 * 1.8).rem_euclid(1.0) < 0.18;
                dots.push((snap(at, px), pulse));
            }
        }
        // Traveling chevrons make the direction readable even without watching the shimmer.
        for i in 0..3 {
            let u = ((time as f32 * 0.6) + i as f32 / 3.0).fract();
            arrow(&mut dots, from.lerp(to, u), direction, px);
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
        for (at, _) in &dots {
            mesh.add_colored_rect(
                Rect::from_center_size(*at, Vec2::splat(px + 2.0 * rim)),
                world::INK.gamma_multiply(0.7 * alpha),
            );
        }
        for (at, bright) in dots {
            mesh.add_colored_rect(
                Rect::from_center_size(at, Vec2::splat(px)),
                if bright {
                    world::LIGHT
                } else {
                    super::comms::RUNE_GLOW
                }
                .gamma_multiply(alpha),
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

fn arrow(dots: &mut Vec<(Pos2, bool)>, tip: Pos2, direction: Vec2, px: f32) {
    let normal = vec2(-direction.y, direction.x);
    for step in 0..5 {
        for side in [-1.0, 1.0] {
            let at = tip + (-direction + normal * side) * step as f32 * px;
            dots.push((snap(at, px), true));
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
