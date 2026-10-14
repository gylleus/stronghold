//! Generated hammer source, reduced to hard farm texels and the mine's material colours.
//! The shared prop cache applies the selected world palette after this pass.
use std::sync::OnceLock;

use eframe::egui::{Color32, ColorImage, Pos2, Rect, vec2};

use super::world::{GOLD_DARK, INK, IRON, STONE_LIGHT, WOOD, WOOD_DARK, WOOD_LIGHT, rgb};
use crate::ui::dwarf::{
    cutout::Outline,
    pixel::{Mapper, Palette},
};

fn source() -> &'static image::RgbaImage {
    static SOURCE: OnceLock<image::RgbaImage> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let image = image::load_from_memory(include_bytes!(
            "../../../assets/art/tools/cleanup_hammer.png"
        ))
        .expect("bundled hammer PNG")
        .to_rgba8();
        let (mut left, mut top, mut right, mut bottom) = (image.width(), image.height(), 0, 0);
        for (x, y, p) in image.enumerate_pixels() {
            if p[3] >= 128 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
        image::imageops::crop_imm(&image, left, top, right - left + 1, bottom - top + 1).to_image()
    })
}

pub fn render(size: [usize; 2], outline: Outline) -> ColorImage {
    let source = source();
    let [w, h] = size;
    let mut image = ColorImage::filled(size, Color32::TRANSPARENT);
    let pad = outline.pixels().ceil() as usize;
    let scale = ((w.saturating_sub(2 * pad)).max(1) as f32 / source.width() as f32)
        .min((h.saturating_sub(2 * pad)).max(1) as f32 / source.height() as f32);
    let sw = (source.width() as f32 * scale).round().max(1.0) as usize;
    let sh = (source.height() as f32 * scale).round().max(1.0) as usize;
    let ox = w.saturating_sub(sw) / 2;
    let oy = h.saturating_sub(sh) / 2;
    let mut mapper = Mapper::new(&Palette {
        name: "mine materials".into(),
        colors: vec![
            INK,
            IRON,
            STONE_LIGHT,
            rgb(0xc1bba7),
            WOOD,
            WOOD_DARK,
            WOOD_LIGHT,
            GOLD_DARK,
            rgb(0xc8963c),
        ],
    });
    for y in 0..sh.min(h) {
        for x in 0..sw.min(w) {
            let sx = ((x as f32 + 0.5) / sw as f32 * source.width() as f32) as u32;
            let sy = ((y as f32 + 0.5) / sh as f32 * source.height() as f32) as u32;
            let p = source.get_pixel(sx.min(source.width() - 1), sy.min(source.height() - 1));
            if p[3] >= 128 {
                image[(ox + x, oy + y)] = Color32::from_rgb(p[0], p[1], p[2]);
            }
        }
    }
    mapper.apply(&mut image);
    let silhouette = image.clone();
    let radius = outline.pixels();
    let reach = radius.ceil() as isize;
    for y in 0..h {
        for x in 0..w {
            if silhouette[(x, y)].a() != 0 {
                continue;
            }
            'neighbours: for dy in -reach..=reach {
                for dx in -reach..=reach {
                    if (dx * dx + dy * dy) as f32 > radius * radius {
                        continue;
                    }
                    let (sx, sy) = (x as isize + dx, y as isize + dy);
                    if sx >= 0
                        && sy >= 0
                        && sx < w as isize
                        && sy < h as isize
                        && silhouette[(sx as usize, sy as usize)].a() != 0
                    {
                        image[(x, y)] = INK;
                        break 'neighbours;
                    }
                }
            }
        }
    }
    image
}

/// One stroke every 650 ms; contact is shared by the pose, shake and chips.
pub const CYCLE_S: f64 = 0.65;
pub const CONTACT: f32 = 0.44;
pub const IMPACT_SPAN: f32 = 0.34;
const TRAVEL_S: f64 = 0.24;
const LEAVE_S: f64 = 0.28;

pub fn phase(now_s: f64, still: bool) -> f32 {
    if still {
        CONTACT
    } else {
        (now_s / CYCLE_S).rem_euclid(1.0) as f32
    }
}

pub fn impact_life(phase: f32) -> Option<f32> {
    (CONTACT..CONTACT + IMPACT_SPAN)
        .contains(&phase)
        .then_some((phase - CONTACT) / IMPACT_SPAN)
}

#[derive(Clone)]
pub struct Target {
    pub id: String,
    pub impact: Pos2,
    pub ground: f32,
    pub px: f32,
}

/// One worker for the entire batch. Travel and departure never delay actual cleanup.
#[derive(Default)]
pub struct Demolisher {
    target: Option<Target>,
    changed_s: f64,
    from: Pos2,
    foot: Pos2,
    leaving: bool,
    pub frame: Option<Frame>,
}

#[derive(Clone)]
pub struct Frame {
    pub foot: Pos2,
    pub height: f32,
    pub pose: crate::ui::dwarf::Pose,
    pub phase: Option<f32>,
    pub alpha: f32,
    pub target: Target,
}

fn stance(target: &Target) -> (Pos2, f32) {
    let k = 42.0 * target.px / 100.0;
    let reach = 17.5 * k + 30.0 * target.px * 0.9;
    let arm = ((target.impact.y - target.ground + 43.0 * k) / reach)
        .clamp(-0.9, 0.9)
        .acos();
    (
        eframe::egui::pos2(target.impact.x - 5.0 * k - reach * arm.sin(), target.ground),
        arm,
    )
}

impl Demolisher {
    pub fn update(&mut self, target: Option<Target>, now_s: f64, still: bool, away: bool) {
        if away {
            *self = Self::default();
        }
        let changed = self.target.as_ref().map(|t| &t.id) != target.as_ref().map(|t| &t.id);
        if let Some(target) = target {
            let (to, _) = stance(&target);
            if changed || self.leaving {
                self.from = if self.frame.is_some() {
                    self.foot
                } else {
                    to - vec2(45.0 * target.px, 0.0)
                };
                self.changed_s = now_s;
            }
            self.target = Some(target);
            self.leaving = false;
        } else if self.target.is_some() && !self.leaving {
            self.from = self.foot;
            self.changed_s = now_s;
            self.leaving = true;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let age = (now_s - self.changed_s).max(0.0);
        if self.leaving && (still || age >= LEAVE_S) {
            *self = Self::default();
            return;
        }
        let (to, contact_arm) = stance(&target);
        let travelling = !still && (self.leaving || age < TRAVEL_S);
        let phase = (!travelling).then(|| phase(age - TRAVEL_S, still));
        let pose = if travelling {
            let mut pose = crate::ui::dwarf::pose(crate::ui::dwarf::Action::Walk, age * 1.6);
            pose.arm = 2.1;
            pose.back_arm = 2.1;
            pose.stowed = true;
            pose.two_handed = true;
            pose.lean = 0.0;
            pose.stretch = 0.0;
            pose
        } else {
            crate::ui::dwarf::demolition(phase.unwrap(), contact_arm)
        };
        self.foot = if self.leaving {
            self.from + vec2(age as f32 * 90.0 * target.px, 0.0)
        } else if still {
            to
        } else {
            self.from.lerp(to, (age / TRAVEL_S).min(1.0) as f32)
        };
        self.frame = Some(Frame {
            foot: self.foot,
            height: 42.0 * target.px,
            pose,
            phase,
            alpha: if self.leaving {
                1.0 - (age / LEAVE_S) as f32
            } else {
                1.0
            },
            target,
        });
    }
}

/// The sledge stays attached to the fist, with both hands painted over its handle.
pub fn paint(
    painter: &eframe::egui::Painter,
    frame: &Frame,
    prop: impl FnOnce(Rect, (f32, Pos2), f32),
) {
    use crate::ui::dwarf::{self, Beard, Garment, Hat, Look, Tool};
    let look = Look {
        girth: 1.35,
        beard: Beard::Bushy,
        hat: Hat::Bandana,
        garment: Garment::Apron,
        tool: Tool::None,
        ..Look::from_seed(47)
    };
    let (grip, hands) = dwarf::held_prop(frame.foot, frame.height, &look, &frame.pose);
    let mut painter = painter.clone();
    painter.multiply_opacity(frame.alpha);
    painter.extend(
        dwarf::draw(
            frame.foot,
            frame.height,
            &look,
            &frame.pose,
            true,
            &dwarf::cutout::Finish::INK,
        )
        .shapes,
    );
    let scale = frame.target.px * 0.9;
    let size = vec2(26.0, 40.0) * scale;
    let pivot = grip.origin;
    let angle = grip.x.y.atan2(grip.x.x);
    let rect = Rect::from_min_size(pivot - vec2(size.x / 2.0, size.y - 4.0 * scale), size);
    prop(rect, (angle, pivot), frame.alpha);
    painter.extend(hands);
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;
    #[test]
    fn sprite_is_hard_alpha_and_outline_changes_it() {
        let fine = render([26, 40], Outline::Fine);
        assert!(fine.pixels.iter().all(|p| p.a() == 0 || p.a() == 255));
        assert!(fine.pixels.iter().filter(|p| p.a() == 255).count() > 150);
        assert_ne!(fine, render([26, 40], Outline::Bold));
    }
    #[test]
    fn sledge_reaches_each_heap_from_a_grounded_stance() {
        for px in [0.75, 1.0, 2.0] {
            for rock_height in [4.0, 10.0, 20.0] {
                let target = Target {
                    id: "rock".into(),
                    impact: pos2(100.0, 100.0 - rock_height * px),
                    ground: 100.0,
                    px,
                };
                let (foot, arm) = stance(&target);
                let pose = crate::ui::dwarf::demolition(CONTACT, arm);
                let look = crate::ui::dwarf::Look::from_seed(47);
                let (grip, _) = crate::ui::dwarf::held_prop(foot, 42.0 * px, &look, &pose);
                let tip = grip.origin + grip.y.normalized() * (-30.0 * px * 0.9);
                assert!(tip.distance(target.impact) < 0.001);
                assert_eq!(foot.y, target.ground);
            }
        }
    }

    #[test]
    fn worker_arrives_before_striking_and_leaves_with_his_tool() {
        let target = Target {
            id: "rock".into(),
            impact: pos2(100.0, 90.0),
            ground: 100.0,
            px: 1.0,
        };
        let mut worker = Demolisher::default();
        worker.update(Some(target.clone()), 0.0, false, false);
        assert!(worker.frame.as_ref().unwrap().phase.is_none());
        worker.update(
            Some(target.clone()),
            TRAVEL_S + CYCLE_S * f64::from(CONTACT),
            false,
            false,
        );
        assert!((worker.frame.as_ref().unwrap().phase.unwrap() - CONTACT).abs() < 0.001);
        worker.update(None, 1.0, false, false);
        assert!(worker.frame.as_ref().unwrap().phase.is_none());
        worker.update(None, 1.0 + LEAVE_S + 0.01, false, false);
        assert!(worker.frame.is_none());
        worker.update(Some(target), 2.0, true, false);
        assert_eq!(worker.frame.as_ref().unwrap().phase, Some(CONTACT));
        worker.update(None, 2.1, true, false);
        assert!(worker.frame.is_none());
    }
}
