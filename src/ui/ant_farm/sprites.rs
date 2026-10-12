//! The mine's art entry point. All props use the dwarf cutout tools in `world` and
//! `nature`, rasterized at one image pixel per farm texel with NEAREST filtering.
//! Granite, worked stone and alpine turf are procedural `ColorImage`s. No farm art
//! is loaded from assets. Palette mapping covers textures and the final scene's shapes.

use super::layout::{MATTRESS_TOP, Mark, PIPE_H, PIPE_LIP_W};
use super::nature::Deco;
use super::world::{GOLD, INK, IRON, LIGHT, Prop, STONE, STONE_LIGHT, WOOD_DARK, WorldArt, rgb};
use crate::orca::AgentIdentity;
use eframe::egui::{
    self, Color32, ColorImage, Painter, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2,
};
use std::cell::RefCell;
const TILE: usize = 128;

/// The agent colours used for dwarf livery. Failed always takes precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coat {
    /// No known agent, and the Queen: the dwarf's own colours.
    Plain,
    Claude,
    Codex,
    Failed,
}

impl Coat {
    pub fn new(agent: Option<&AgentIdentity>, failed: bool) -> Self {
        match agent {
            _ if failed => Coat::Failed,
            Some(AgentIdentity::Claude) => Coat::Claude,
            Some(AgentIdentity::Codex) => Coat::Codex,
            _ => Coat::Plain,
        }
    }
}

/// The farm's palette, picked by the local time of day.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub sky_top: Color32,
    pub sky_bottom: Color32,
    pub label: Color32,
    pub label_shadow: Color32,
}

impl Palette {
    pub const NIGHT: Self = Self {
        sky_top: Color32::from_rgb(0x12, 0x14, 0x2b),
        sky_bottom: Color32::from_rgb(0x2a, 0x2a, 0x4e),
        label: Color32::from_rgb(0xf3, 0xe9, 0xd2),
        label_shadow: Color32::from_rgb(0x1a, 0x10, 0x08),
    };
    pub const DAY: Self = Self {
        sky_top: rgb(0x8199aa),
        sky_bottom: rgb(0xbdc7bf),
        label: Color32::from_rgb(0xfb, 0xf4, 0xe4),
        label_shadow: Color32::from_rgb(0x2b, 0x1a, 0x0c),
    };

    /// The palette at `daylight` (0 night, 1 day; see [`crate::ui::daylight`]).
    pub fn new(daylight: f32) -> Self {
        let mix = |night, day| crate::ui::daylight::mix(night, day, daylight);
        let (n, d) = (Self::NIGHT, Self::DAY);
        Self {
            sky_top: mix(n.sky_top, d.sky_top),
            sky_bottom: mix(n.sky_bottom, d.sky_bottom),
            label: mix(n.label, d.label),
            label_shadow: mix(n.label_shadow, d.label_shadow),
        }
    }
}

pub struct Sprites {
    pub world: RefCell<WorldArt>,
    rock: TextureHandle,
    tunnel: TextureHandle,
    turf: TextureHandle,
    palette: String,
    px: f32,
    now_s: f64,
}
impl Sprites {
    pub fn load(ctx: &egui::Context) -> Self {
        let tile = |name: &str, image: ColorImage| {
            ctx.load_texture(
                format!("mine/{name}"),
                image,
                TextureOptions::NEAREST_REPEAT,
            )
        };
        Self {
            world: RefCell::new(WorldArt::default()),
            rock: tile("granite", rock_tile(false)),
            tunnel: tile("worked-stone", rock_tile(true)),
            turf: tile("alpine-turf", turf_tile()),
            palette: String::new(),
            px: 1.0,
            now_s: 0.0,
        }
    }
    pub fn begin_frame(&mut self, palette: &str, px: f32, now_s: f64) {
        self.px = px;
        self.now_s = now_s;
        self.world.borrow_mut().begin_frame(palette);
        if self.palette != palette {
            self.palette = palette.to_owned();
            let mut rock = rock_tile(false);
            let mut tunnel = rock_tile(true);
            let mut turf = turf_tile();
            self.world.borrow_mut().map_stone(&mut rock, false);
            self.world.borrow_mut().map_stone(&mut tunnel, true);
            self.world.borrow_mut().map(&mut turf);
            for (texture, image) in [
                (&mut self.rock, rock),
                (&mut self.tunnel, tunnel),
                (&mut self.turf, turf),
            ] {
                texture.set(image, TextureOptions::NEAREST_REPEAT);
            }
        }
    }
    pub fn prop(&self, painter: &Painter, prop: Prop, rect: Rect, alpha: f32) {
        self.world
            .borrow_mut()
            .paint(painter, prop, rect, self.px, prop.frame(self.now_s), alpha);
    }
    pub fn standing(&self, painter: &Painter, prop: Prop, foot: Pos2, px: f32, alpha: f32) {
        let [w, h] = prop.size().map(|n| n as f32 * px);
        self.prop(
            painter,
            prop,
            Rect::from_min_size(foot - vec2(w / 2.0, h), vec2(w, h)),
            alpha,
        );
    }
    pub fn deco(&self, painter: &Painter, deco: Deco, foot: Pos2, px: f32, alpha: f32) {
        self.standing(painter, Prop::Nature(deco), foot, px, alpha);
    }
    pub fn roots(&self, painter: &Painter, foot: Pos2, _width: f32, px: f32, _seed: usize) {
        self.standing(painter, Prop::Roots, foot + vec2(0.0, 12.0 * px), px, 1.0);
    }
    pub fn entrance(&self, painter: &Painter, x: f32, y: f32, px: f32) {
        self.standing(painter, Prop::Entrance, pos2(x, y + px), px, 1.0);
    }
    pub fn star(&self, painter: &Painter, at: Pos2, px: f32, bright: bool, alpha: f32) {
        let color = LIGHT.gamma_multiply(alpha);
        if bright {
            self.sparkle(painter, at, px, color, false);
        } else {
            painter.rect_filled(Rect::from_center_size(at, vec2(px, px)), 0.0, color);
        }
    }
    pub fn ground(&self, painter: &Painter, rect: Rect, surface_y: f32, px: f32, palette: Palette) {
        // Flat bands retain the selected palette even across the day/night gradient.
        let height = surface_y - rect.top();
        for i in 0..8 {
            let band = Rect::from_min_max(
                pos2(rect.left(), rect.top() + height * i as f32 / 8.0),
                pos2(rect.right(), rect.top() + height * (i + 1) as f32 / 8.0),
            );
            painter.rect_filled(
                band,
                0.0,
                palette
                    .sky_top
                    .lerp_to_gamma(palette.sky_bottom, i as f32 / 7.0),
            );
        }
        let rock = Rect::from_min_max(pos2(rect.left(), surface_y), rect.max);
        tiled(painter, self.rock.id(), rock, px);
    }
    /// Foreground alpine turf, painted after the distant peaks so its blades stay visible.
    pub fn surface_edge(&self, painter: &Painter, rect: Rect, surface_y: f32, px: f32) {
        let turf = Rect::from_min_max(
            pos2(rect.left(), surface_y - 3.0 * px),
            pos2(rect.right(), surface_y + 3.0 * px),
        );
        let span = TILE as f32 * px;
        painter.image(
            self.turf.id(),
            turf,
            Rect::from_min_max(
                pos2(turf.left() / span, 0.0),
                pos2(turf.right() / span, 6.0 / TILE as f32),
            ),
            Color32::WHITE,
        );
    }
    pub fn peaks(&self, painter: &Painter, scene: &super::layout::Scene) {
        let px = scene.px;
        let span = 192.0 * px;
        let n = (scene.rect.width() / span).ceil() as usize;
        for i in 0..n {
            self.prop(
                painter,
                Prop::Peaks,
                Rect::from_min_size(
                    pos2(
                        scene.rect.left() + i as f32 * span,
                        scene.surface_y - 52.0 * px,
                    ),
                    vec2(span, 52.0 * px),
                ),
                1.0,
            );
        }
    }
    pub fn passage(&self, painter: &Painter, rect: Rect, px: f32) {
        painter.rect_filled(rect.expand(px), 0.0, INK);
        tiled(painter, self.tunnel.id(), rect, px);
        // The freshly cut upper edge catches a little light.
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(rect.width(), px)),
            0.0,
            rgb(0x686761),
        );
    }
    pub fn shoring(&self, painter: &Painter, shaft: Rect, px: f32) {
        let mut y = shaft.top() + 6.0 * px;
        while y + 10.0 * px < shaft.bottom() {
            self.prop(
                painter,
                Prop::Shoring,
                Rect::from_min_size(pos2(shaft.left(), y), vec2(shaft.width(), 10.0 * px)),
                1.0,
            );
            y += 24.0 * px;
        }
    }
    pub fn gallery(&self, painter: &Painter, rect: Rect, px: f32) {
        let floor = rect.bottom() - 2.0 * px;
        let mut x = rect.left() + 4.0 * px;
        while x < rect.right() - 2.0 * px {
            painter.rect_filled(
                Rect::from_min_size(pos2(x, floor - 2.0 * px), vec2(3.0 * px, 4.0 * px)),
                0.0,
                WOOD_DARK,
            );
            x += 8.0 * px;
        }
        for y in [floor - 2.0 * px, floor + px] {
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), px)),
                0.0,
                IRON,
            );
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), px * 0.5)),
                0.0,
                STONE_LIGHT,
            );
        }
        x = rect.left() + 30.0 * px;
        while x < rect.right() - 20.0 * px {
            self.prop(
                painter,
                Prop::Support,
                Rect::from_min_size(
                    pos2(x - 18.0 * px, rect.top()),
                    vec2(36.0 * px, rect.height() - px),
                ),
                1.0,
            );
            let foot = pos2(x + 24.0 * px, rect.bottom() - 3.0 * px);
            self.glow(painter, foot - vec2(0.0, 11.0 * px), 12.0 * px, GOLD, 0.32);
            self.standing(painter, Prop::Torch, foot, px, 1.0);
            x += 96.0 * px;
        }
    }
    pub fn throne_hall(&self, painter: &Painter, rect: Rect, px: f32) {
        let floor = rect.bottom() - 2.0 * px;
        self.standing(
            painter,
            Prop::Throne,
            pos2(rect.center().x, floor),
            px * 0.9,
            1.0,
        );
        for side in [-1.0, 1.0] {
            self.standing(
                painter,
                Prop::Banner,
                pos2(
                    rect.center().x + side * (rect.width() / 2.0 - 9.0 * px),
                    rect.top() + 18.0 * px,
                ),
                px * 0.65,
                1.0,
            );
            let foot = pos2(
                rect.center().x + side * (rect.width() / 2.0 - 8.0 * px),
                floor,
            );
            self.glow(painter, foot - vec2(0.0, 15.0 * px), 12.0 * px, GOLD, 0.4);
            self.standing(painter, Prop::Brazier, foot, px * 0.65, 1.0);
        }
    }
    pub fn bed(&self, painter: &Painter, rect: Rect, _px: f32) {
        self.prop(painter, Prop::Bunk, rect, 1.0);
    }
    pub fn blanket(&self, painter: &Painter, bed: Rect, cell: Rect, px: f32, alpha: f32) {
        let top = bed.bottom() - (MATTRESS_TOP + 3.0) * px;
        let body = Rect::from_min_max(
            pos2(cell.left() + 0.45 * cell.width(), top),
            pos2(bed.right() - px, bed.bottom() - (MATTRESS_TOP - 2.0) * px),
        );
        self.prop(painter, Prop::Blanket, body, alpha);
    }
    pub fn bubble(&self, painter: &Painter, anchor: Pos2, size: f32, mark: Mark, alpha: f32) {
        let mark = match mark {
            Mark::Alert => 0,
            Mark::Sleep => 1,
            Mark::Stalled => 2,
            Mark::Failed => 3,
            Mark::PrReady => 4,
        };
        self.prop(
            painter,
            Prop::Bubble(mark),
            Rect::from_min_size(anchor - vec2(size / 2.0, size), vec2(size, size)),
            alpha,
        );
    }
    pub fn pile(&self, painter: &Painter, rect: Rect, variant: u8) {
        self.prop(painter, Prop::Treasure(variant % 3), rect, 1.0);
    }
    /// Riveted brass pneumatic tube, with a name plate. Only a departing message stretches it.
    pub fn pipe(
        &self,
        painter: &Painter,
        foot: Pos2,
        px: f32,
        stretch: f32,
        text: &str,
        font: egui::FontId,
    ) {
        let rect = Rect::from_min_size(
            foot - vec2((PIPE_LIP_W + 2.0) * px / 2.0, (PIPE_H + stretch) * px),
            vec2((PIPE_LIP_W + 2.0) * px, (PIPE_H + stretch + 1.0) * px),
        );
        self.prop(painter, Prop::Tube, rect, 1.0);
        let room = 12.0 * px;
        let mut galley = painter.layout_no_wrap(text.to_owned(), font.clone(), INK);
        if galley.size().x > room {
            galley = painter.layout_no_wrap(
                text.to_owned(),
                egui::FontId::new((font.size * room / galley.size().x).max(6.0), font.family),
                INK,
            );
        }
        painter.galley(
            pos2(
                foot.x - galley.size().x / 2.0,
                rect.top() + rect.height() * 0.62 - galley.size().y / 2.0,
            ),
            galley,
            INK,
        );
    }
    /// A pebble arcs onto the helmet; a white tink appears at impact, then it falls away.
    pub fn bonk(&self, painter: &Painter, cell: Rect, px: f32, phase: usize) {
        let t = (phase % 12) as f32 / 12.0;
        let hit = cell.center_top() + vec2(2.0 * px, 3.0 * px);
        let at = if t < 0.65 {
            let u = t / 0.65;
            hit + vec2(
                -10.0 * (1.0 - u),
                -12.0 * (1.0 - u) - 8.0 * (u * std::f32::consts::PI).sin(),
            ) * px
        } else {
            let u = (t - 0.65) / 0.35;
            hit + vec2(5.0 * u, -4.0 * u + 10.0 * u * u) * px
        };
        self.prop(
            painter,
            Prop::Pebble,
            Rect::from_center_size(at, vec2(4.0, 4.0) * px),
            1.0,
        );
        if (0.6..0.85).contains(&t) {
            self.sparkle(painter, hit, px, Color32::WHITE, true);
        }
    }
    /// A soft pixel glow around `center`: stacked translucent squares, brightest inside.
    pub fn glow(&self, painter: &Painter, center: Pos2, radius: f32, color: Color32, alpha: f32) {
        for i in (1..=4).rev() {
            let r = radius * i as f32 / 4.0;
            let a = alpha * 0.12 * (5 - i) as f32;
            painter.rect_filled(
                Rect::from_center_size(center, vec2(2.0 * r, 2.0 * r)),
                r * 0.6,
                color.gamma_multiply(a),
            );
        }
    }

    /// A four-pointed pixel sparkle at `center`.
    pub fn sparkle(&self, painter: &Painter, center: Pos2, px: f32, color: Color32, big: bool) {
        let dot = |dx: f32, dy: f32| {
            painter.rect_filled(
                Rect::from_center_size(center + vec2(dx * px, dy * px), vec2(px, px)),
                0.0,
                color,
            );
        };
        dot(0.0, 0.0);
        for d in if big { 1..=2 } else { 1..=1 } {
            let d = d as f32;
            dot(d, 0.0);
            dot(-d, 0.0);
            dot(0.0, d);
            dot(0.0, -d);
        }
    }

    /// A ring of the Queen's call at `radius` around `center`, drawn as pixel dashes.
    pub fn ring(&self, painter: &Painter, center: Pos2, radius: f32, px: f32, color: Color32) {
        let dashes = ((radius * std::f32::consts::TAU) / (6.0 * px)).clamp(12.0, 400.0) as usize;
        for i in 0..dashes {
            if i % 3 == 2 {
                continue;
            }
            let angle = i as f32 / dashes as f32 * std::f32::consts::TAU;
            let p = center + vec2(angle.cos(), angle.sin()) * radius;
            let snapped = pos2((p.x / px).round() * px, (p.y / px).round() * px);
            painter.rect_filled(
                Rect::from_center_size(snapped, vec2(2.0 * px, 2.0 * px)),
                0.0,
                color,
            );
        }
    }

    pub fn snore(&self, painter: &Painter, pos: Pos2, px: f32, alpha: f32, _palette: Palette) {
        self.standing(painter, Prop::Snore, pos, px, alpha);
    }
    /// Granite chips and an occasional gold spark fly back from an active digger.
    pub fn dirt(
        &self,
        painter: &Painter,
        cell: Rect,
        facing_right: bool,
        px: f32,
        now_s: f64,
        seed: f32,
    ) {
        let back = if facing_right { -1.0 } else { 1.0 };
        let face = cell.center_bottom() - vec2(0.0, 2.0 * px);
        for i in 0..6 {
            let t = ((now_s / 0.9) + i as f64 / 6.0 + seed as f64).fract() as f32;
            let h = noise(i, (seed * 97.0) as usize, 11);
            let p = face
                + vec2(
                    back * (8.0 + (h % 9) as f32) * t,
                    -(24.0 + (h / 9 % 9) as f32) * t + 22.0 * t * t,
                ) * px;
            let p = pos2((p.x / px).round() * px, (p.y / px).round() * px);
            if i == 0 {
                self.sparkle(painter, p, px, LIGHT.gamma_multiply(1.0 - t), false);
            } else {
                self.prop(
                    painter,
                    Prop::Pebble,
                    Rect::from_center_size(p, vec2(3.0, 3.0) * px),
                    1.0 - t * t,
                );
            }
        }
    }
}
fn tiled(painter: &Painter, texture: egui::TextureId, rect: Rect, px: f32) {
    let span = TILE as f32 * px;
    painter.image(
        texture,
        rect,
        Rect::from_min_max(
            (rect.min.to_vec2() / span).to_pos2(),
            (rect.max.to_vec2() / span).to_pos2(),
        ),
        Color32::WHITE,
    );
}
/// A small deterministic hash for tile noise.
fn noise(x: usize, y: usize, salt: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9e37_79b1) ^ (y as u32).wrapping_mul(0x85eb_ca77) ^ salt;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h
}

/// Broad, quiet rock planes with faint strata; small marks read as chisel cuts in tunnels.
pub fn rock_tile(worked: bool) -> ColorImage {
    let base = if worked { rgb(0x46484a) } else { rgb(0x625f59) };
    let dark = if worked { rgb(0x424447) } else { rgb(0x5e5b56) };
    let light = if worked { rgb(0x4f5253) } else { rgb(0x69665e) };
    let mut image = ColorImage::filled([TILE, TILE], base);
    for y in 0..TILE {
        for x in 0..TILE {
            let plane = noise(x / 12, y / 8, 9) % 9;
            image[(x, y)] = if plane == 0 {
                light
            } else if plane == 1 {
                dark
            } else {
                base
            };
            let strata = (y + (x / 12) % 3) % 16;
            if strata == 0 && noise(x / 5, y / 16, 7).is_multiple_of(3) {
                image[(x, y)] = dark;
            }
            if worked {
                if (x + y * 3) % 17 == 0 && y % 7 < 2 {
                    image[(x, y)] = light;
                }
            } else {
                let n = noise(x / 2, y / 2, 31);
                if n.is_multiple_of(151) {
                    image[(x, y)] = if y % 2 == 0 { light } else { dark };
                }
                // One thin interrupted vein, with a fork and one jewel glint per repeat.
                if (20..35).contains(&x) && y == 42 - (x - 20) / 3 && x % 6 != 0 {
                    image[(x, y)] = GOLD_DARK_TILE;
                }
                if (28..32).contains(&x) && y == 40 + (x - 28) {
                    image[(x, y)] = GOLD_DARK_TILE;
                }
                if x == 48 && y == 19 {
                    image[(x, y)] = rgb(0x78a19c);
                }
                if x == 49 && y == 20 {
                    image[(x, y)] = rgb(0x426665);
                }
            }
        }
    }
    image
}
const GOLD_DARK_TILE: Color32 = rgb(0x9b8656);
pub fn turf_tile() -> ColorImage {
    let mut image = ColorImage::filled([TILE, TILE], Color32::TRANSPARENT);
    for x in 0..TILE {
        let blade = 1 + (noise(x, 0, 3) % 3) as usize;
        for y in 3 - blade..6 {
            image[(x, y)] = if y >= 4 {
                STONE
            } else if x % 13 == 0 {
                rgb(0x95758a)
            } else if x % 3 == 0 {
                rgb(0x7f8968)
            } else {
                rgb(0x525e48)
            };
        }
    }
    image
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_use_the_selected_palette_and_keep_alpha() {
        for palette in crate::ui::dwarf::pixel::bundled() {
            for mut image in [rock_tile(false), rock_tile(true), turf_tile()] {
                let before = image.clone();
                crate::ui::dwarf::pixel::Mapper::new(&palette).apply(&mut image);
                assert_ne!(image, before);
                for (a, b) in image.pixels.iter().zip(before.pixels) {
                    assert_eq!(a.a(), b.a());
                    assert!(a.a() == 0 || palette.colors.contains(a));
                }
            }
        }
    }

    #[test]
    fn failed_wins_over_the_agent() {
        assert_eq!(Coat::new(Some(&AgentIdentity::Claude), true), Coat::Failed);
        assert_eq!(Coat::new(Some(&AgentIdentity::Codex), false), Coat::Codex);
        assert_eq!(Coat::new(None, false), Coat::Plain);
    }
}
