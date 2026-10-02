//! The ant farm's art: the one place that knows what the farm looks like. The view hands it
//! positions and poses from [`super::layout`]; it draws pixel-art sprites from
//! `assets/art/farm` (see `assets/LICENSES.md`) and generates the soil tiles.
//!
//! To change the art, replace the PNGs (16×16 cells; the ant sheets stack two walk frames),
//! the tile generators below or the surface's pixel maps in [`super::nature`]; nothing outside
//! these files draws farm art.

use eframe::egui::{
    self, Color32, ColorImage, Painter, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2,
};

use super::layout::Mark;
use super::nature::Deco;

const ANT_GREY: &[u8] = include_bytes!("../../../assets/art/farm/ant_grey.png");
const ANT_RED: &[u8] = include_bytes!("../../../assets/art/farm/ant_red.png");
const CROWN: &[u8] = include_bytes!("../../../assets/art/farm/crown.png");
const COIN: &[u8] = include_bytes!("../../../assets/art/farm/coin.png");
const COIN_PILE: &[u8] = include_bytes!("../../../assets/art/farm/coin_pile.png");
const EMOTE_ALERT: &[u8] = include_bytes!("../../../assets/art/farm/emote_exclamation.png");
const EMOTE_SLEEP: &[u8] = include_bytes!("../../../assets/art/farm/emote_sleeps.png");
const EMOTE_WHIP: &[u8] = include_bytes!("../../../assets/art/farm/emote_anger.png");
const EMOTE_FAILED: &[u8] = include_bytes!("../../../assets/art/farm/emote_cross.png");
const EMOTE_PR: &[u8] = include_bytes!("../../../assets/art/farm/emote_star.png");

/// Frames stacked in an ant sheet.
pub const ANT_FRAMES: usize = 2;
/// Side of a generated soil tile in texels.
const TILE: usize = 32;

/// How to draw one ant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AntLook {
    pub red: bool,
    /// Walk frame, wrapped to [`ANT_FRAMES`].
    pub frame: usize,
    pub facing_right: bool,
    /// Belly up (failed).
    pub upside_down: bool,
    pub alpha: f32,
}

/// Something carried or stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Crown,
    /// A crumb carried to storage.
    Crumb,
    /// One PR's crumbs on the storage pile.
    Pile,
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
        sky_top: Color32::from_rgb(0x7f, 0xc8, 0xf0),
        sky_bottom: Color32::from_rgb(0xc8, 0xec, 0xff),
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

/// The loaded textures; create once per egui context with [`Sprites::load`].
pub struct Sprites {
    ant_grey: TextureHandle,
    ant_red: TextureHandle,
    crown: TextureHandle,
    coin: TextureHandle,
    pile: TextureHandle,
    alert: TextureHandle,
    sleep: TextureHandle,
    whip: TextureHandle,
    failed: TextureHandle,
    pr: TextureHandle,
    soil: TextureHandle,
    tunnel: TextureHandle,
    grass: TextureHandle,
    nature: std::collections::HashMap<Deco, TextureHandle>,
}

impl Sprites {
    pub fn load(ctx: &egui::Context) -> Self {
        let png = |name: &str, bytes: &[u8]| {
            ctx.load_texture(
                format!("ant_farm/{name}"),
                decode(bytes),
                TextureOptions::NEAREST,
            )
        };
        let tile = |name: &str, image: ColorImage| {
            ctx.load_texture(
                format!("ant_farm/{name}"),
                image,
                TextureOptions::NEAREST_REPEAT,
            )
        };
        Self {
            ant_grey: png("ant_grey", ANT_GREY),
            ant_red: png("ant_red", ANT_RED),
            crown: png("crown", CROWN),
            coin: png("coin", COIN),
            pile: png("pile", COIN_PILE),
            alert: png("alert", EMOTE_ALERT),
            sleep: png("sleep", EMOTE_SLEEP),
            whip: png("whip", EMOTE_WHIP),
            failed: png("failed", EMOTE_FAILED),
            pr: png("pr", EMOTE_PR),
            soil: tile("soil", soil_tile(SOIL)),
            tunnel: tile("tunnel", soil_tile(TUNNEL)),
            grass: tile("grass", grass_tile()),
            nature: Deco::ALL
                .into_iter()
                .map(|d| {
                    let name = format!("ant_farm/nature/{d:?}");
                    (
                        d,
                        ctx.load_texture(name, d.image(), TextureOptions::NEAREST),
                    )
                })
                .collect(),
        }
    }

    /// A piece of scenery standing on `foot` (the middle of its bottom edge).
    pub fn deco(&self, painter: &Painter, deco: Deco, foot: Pos2, px: f32, alpha: f32) {
        let Some(texture) = self.nature.get(&deco) else {
            return;
        };
        let [w, h] = deco.extent(px);
        let size = vec2(w, h);
        let rect = Rect::from_min_size(foot - vec2(size.x / 2.0, size.y), size);
        painter.image(
            texture.id(),
            rect,
            unit_uv(),
            Color32::WHITE.gamma_multiply(alpha),
        );
    }

    /// Roots spreading into the soil under a plant `width` points wide at `foot`.
    pub fn roots(&self, painter: &Painter, foot: Pos2, width: f32, px: f32, seed: usize) {
        let color = Color32::from_rgb(0x5a, 0x38, 0x1c);
        let strands = 3;
        for s in 0..strands {
            let h = noise(s, seed, 5);
            let lean = (s as f32 - 1.0) * 0.6 + (h % 3) as f32 * 0.2 - 0.2;
            let len = 4 + (h % 5) as usize;
            for i in 0..len {
                let x = foot.x + lean * i as f32 * px + ((h >> i) & 1) as f32 * px;
                let y = foot.y + (i as f32 + 2.0) * px;
                if (x - foot.x).abs() > width {
                    break;
                }
                let p = pos2((x / px).round() * px, y);
                painter.rect_filled(Rect::from_min_size(p, vec2(px, px)), 0.0, color);
            }
        }
    }

    /// The anthill's mound of dug-out dirt around the shaft at `x` on the surface.
    pub fn mound(&self, painter: &Painter, x: f32, surface_y: f32, a: f32, px: f32) {
        let dirt = Color32::from_rgb(0x9c, 0x6a, 0x3c);
        let light = Color32::from_rgb(0xb5, 0x80, 0x4c);
        let hole = 0.6 * a;
        for (step, (w, color)) in [(1.4, dirt), (1.0, light), (0.7, dirt)].iter().enumerate() {
            let y = surface_y - (step as f32 + 1.0) * 2.0 * px;
            for side in [-1.0, 1.0] {
                let inner = x + side * hole;
                let outer = inner + side * w * a * 0.5;
                let rect = Rect::from_two_pos(pos2(inner, y), pos2(outer, y + 2.0 * px));
                painter.rect_filled(rect, 0.0, *color);
            }
        }
    }

    /// A star: a dot, or a little cross while it twinkles.
    pub fn star(&self, painter: &Painter, at: Pos2, px: f32, bright: bool, alpha: f32) {
        let color = Color32::from_rgb(0xfb, 0xf4, 0xd0).gamma_multiply(alpha);
        if bright {
            self.sparkle(painter, at, px, color, false);
        } else {
            painter.rect_filled(
                Rect::from_center_size(at, vec2(px, px)),
                0.0,
                color.gamma_multiply(0.7),
            );
        }
    }

    /// Sky above `surface_y` and soil below it, filling `rect`.
    pub fn ground(&self, painter: &Painter, rect: Rect, surface_y: f32, px: f32, palette: Palette) {
        let sky = Rect::from_min_max(rect.min, pos2(rect.right(), surface_y));
        let mut mesh = egui::Mesh::default();
        let i = mesh.vertices.len() as u32;
        for (p, c) in [
            (sky.left_top(), palette.sky_top),
            (sky.right_top(), palette.sky_top),
            (sky.right_bottom(), palette.sky_bottom),
            (sky.left_bottom(), palette.sky_bottom),
        ] {
            mesh.colored_vertex(p, c);
        }
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
        painter.add(mesh);
        let soil = Rect::from_min_max(pos2(rect.left(), surface_y), rect.max);
        tiled(painter, self.soil.id(), soil, px);
        let grass = Rect::from_min_max(
            pos2(rect.left(), surface_y - 2.0 * px),
            pos2(rect.right(), surface_y + 4.0 * px),
        );
        let span = TILE as f32 * px;
        let uv = Rect::from_min_max(
            pos2(grass.left() / span, 0.0),
            pos2(grass.right() / span, grass.height() / span),
        );
        painter.image(self.grass.id(), grass, uv, Color32::WHITE);
    }

    /// A passage dug out of the soil.
    pub fn passage(&self, painter: &Painter, rect: Rect, px: f32) {
        let edge = Rect::from_min_max(rect.min - vec2(px, px), rect.max + vec2(px, px));
        painter.rect_filled(edge, 0.0, Color32::from_rgb(0x3a, 0x22, 0x12));
        tiled(painter, self.tunnel.id(), rect, px);
    }

    /// An ant filling `cell`.
    pub fn ant(&self, painter: &Painter, cell: Rect, look: AntLook) {
        let texture = if look.red {
            &self.ant_red
        } else {
            &self.ant_grey
        };
        let frame = look.frame % ANT_FRAMES;
        let h = 1.0 / ANT_FRAMES as f32;
        let (y0, y1) = (frame as f32 * h, (frame + 1) as f32 * h);
        let (y0, y1) = if look.upside_down { (y1, y0) } else { (y0, y1) };
        let (x0, x1) = if look.facing_right {
            (1.0, 0.0)
        } else {
            (0.0, 1.0)
        };
        let uv = Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
        painter.image(
            texture.id(),
            cell,
            uv,
            Color32::WHITE.gamma_multiply(look.alpha),
        );
    }

    /// A speech bubble with `mark`, its tail on `anchor` (the top of the ant's head).
    pub fn bubble(&self, painter: &Painter, anchor: Pos2, size: f32, mark: Mark, alpha: f32) {
        let texture = match mark {
            Mark::Alert => &self.alert,
            Mark::Sleep => &self.sleep,
            Mark::Whip => &self.whip,
            Mark::Failed => &self.failed,
            Mark::PrReady => &self.pr,
        };
        let rect = Rect::from_center_size(anchor - vec2(0.0, size / 2.0), vec2(size, size));
        painter.image(
            texture.id(),
            rect,
            unit_uv(),
            Color32::WHITE.gamma_multiply(alpha),
        );
    }

    pub fn item(&self, painter: &Painter, rect: Rect, item: Item, alpha: f32) {
        let texture = match item {
            Item::Crown => &self.crown,
            Item::Crumb => &self.coin,
            Item::Pile => &self.pile,
        };
        painter.image(
            texture.id(),
            rect,
            unit_uv(),
            Color32::WHITE.gamma_multiply(alpha),
        );
    }

    /// A wooden sign on the surface at `foot` (the post's base) reading `text`.
    pub fn sign(
        &self,
        painter: &Painter,
        foot: Pos2,
        px: f32,
        text: &str,
        font: egui::FontId,
        palette: Palette,
    ) {
        let wood = Color32::from_rgb(0x9c, 0x6a, 0x3a);
        let dark = Color32::from_rgb(0x4a, 0x2c, 0x14);
        let galley = painter.layout_no_wrap(text.to_string(), font, palette.label_shadow);
        let board = Rect::from_min_size(
            foot + vec2(2.0 * px, -14.0 * px - galley.size().y),
            galley.size() + vec2(6.0 * px, 4.0 * px),
        );
        let post = Rect::from_min_max(
            pos2(board.center().x - px, board.bottom()),
            pos2(board.center().x + px, foot.y),
        );
        painter.rect_filled(post.expand(px / 2.0), 0.0, dark);
        painter.rect_filled(post, 0.0, wood);
        painter.rect_filled(board.expand(px), 0.0, dark);
        painter.rect_filled(board, 0.0, wood);
        painter.galley(
            board.min + vec2(3.0 * px, 2.0 * px),
            galley,
            palette.label_shadow,
        );
    }

    /// The whip's lash poking a stalled ant at `cell`: a pixel line from above and behind.
    pub fn whip(&self, painter: &Painter, cell: Rect, px: f32, phase: usize) {
        let color = Color32::from_rgb(0x8a, 0x4b, 0x1c);
        let tip = cell.right_top() + vec2(-3.0 * px, 4.0 * px + (phase % 2) as f32 * px);
        for i in 0..6 {
            let p = tip + vec2(i as f32 * px, -(i as f32) * px * 0.8);
            painter.rect_filled(Rect::from_min_size(p, vec2(px, px)), 0.0, color);
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

    /// A small "z" drifting up from a sleeper, at `pos`.
    pub fn snore(&self, painter: &Painter, pos: Pos2, px: f32, alpha: f32, palette: Palette) {
        let font = egui::FontId::monospace(6.0 + 2.5 * px);
        let shadow = palette.label_shadow.gamma_multiply(alpha);
        let color = palette.label.gamma_multiply(alpha);
        painter.text(
            pos + vec2(1.0, 1.0),
            egui::Align2::CENTER_CENTER,
            "z",
            font.clone(),
            shadow,
        );
        painter.text(pos, egui::Align2::CENTER_CENTER, "z", font, color);
    }

    /// Dirt flying from the digger's feet at `now_s`: clods kicked up and back behind it,
    /// falling in arcs and fading. `seed` staggers diggers so they don't throw in step.
    pub fn dirt(
        &self,
        painter: &Painter,
        cell: Rect,
        facing_right: bool,
        px: f32,
        now_s: f64,
        seed: f32,
    ) {
        const CLODS: usize = 8;
        const LIFE_S: f64 = 0.9;
        let shades = [
            Color32::from_rgb(0xb0, 0x7a, 0x45),
            Color32::from_rgb(0x8a, 0x5a, 0x34),
            Color32::from_rgb(0xc8, 0x94, 0x5c),
        ];
        let back = if facing_right { -1.0 } else { 1.0 };
        // The sprite's feet: the bottom rows of the cell, a little behind its middle.
        let face = pos2(cell.center().x + back * 2.0 * px, cell.top() + 13.0 * px);
        for i in 0..CLODS {
            let k = i as f32 / CLODS as f32;
            let t = ((now_s / LIFE_S) + f64::from(k) + f64::from(seed)).fract() as f32;
            // Each clod has its own throw, from a hash of its slot.
            let h = noise(i, (seed * 97.0) as usize, 11);
            let vx = 8.0 + (h % 9) as f32;
            let vy = 24.0 + (h / 9 % 9) as f32;
            let x = back * vx * t;
            let y = -vy * t + 22.0 * t * t;
            let p = face + vec2(x * px, y * px);
            let snapped = pos2((p.x / px).round() * px, (p.y / px).round() * px);
            let size = if h.is_multiple_of(3) { px } else { 2.0 * px };
            let color = shades[(h % 3) as usize].gamma_multiply(1.0 - t * t);
            painter.rect_filled(Rect::from_min_size(snapped, vec2(size, size)), 0.0, color);
        }
    }
}

fn unit_uv() -> Rect {
    Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0))
}

/// Fills `rect` with a repeating tile at `px` points per texel, anchored to the screen so
/// tiles line up across passages.
fn tiled(painter: &Painter, texture: egui::TextureId, rect: Rect, px: f32) {
    let span = TILE as f32 * px;
    let uv = Rect::from_min_max(
        (rect.min.to_vec2() / span).to_pos2(),
        (rect.max.to_vec2() / span).to_pos2(),
    );
    painter.image(texture, rect, uv, Color32::WHITE);
}

fn decode(bytes: &[u8]) -> ColorImage {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .expect("bundled farm sprite is a valid PNG")
        .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, image.as_flat_samples().as_slice())
}

/// Soil shades, darkest last; the last is used sparsely for pebbles.
const SOIL: [Color32; 4] = [
    Color32::from_rgb(0x8a, 0x5a, 0x34),
    Color32::from_rgb(0x7c, 0x4f, 0x2d),
    Color32::from_rgb(0x96, 0x66, 0x3c),
    Color32::from_rgb(0x5e, 0x3a, 0x1f),
];
const TUNNEL: [Color32; 4] = [
    Color32::from_rgb(0x3f, 0x27, 0x16),
    Color32::from_rgb(0x46, 0x2c, 0x19),
    Color32::from_rgb(0x38, 0x22, 0x13),
    Color32::from_rgb(0x2c, 0x1a, 0x0e),
];

/// A small deterministic hash for tile noise.
fn noise(x: usize, y: usize, salt: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9e37_79b1) ^ (y as u32).wrapping_mul(0x85eb_ca77) ^ salt;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h
}

/// A seamless tile of chunky dithered soil in `shades`.
pub fn soil_tile(shades: [Color32; 4]) -> ColorImage {
    let mut image = ColorImage::filled([TILE, TILE], shades[0]);
    for y in 0..TILE {
        for x in 0..TILE {
            let n = noise(x / 2, y / 2, 7) % 16;
            let shade = match n {
                0 => shades[3],
                1..=4 => shades[1],
                5..=7 => shades[2],
                _ => shades[0],
            };
            image[(x, y)] = shade;
        }
    }
    image
}

/// A seamless strip of grass over the topsoil.
pub fn grass_tile() -> ColorImage {
    let dirt = SOIL[0];
    let greens = [
        Color32::from_rgb(0x4c, 0x9a, 0x3b),
        Color32::from_rgb(0x3b, 0x7d, 0x2e),
    ];
    let mut image = ColorImage::filled([TILE, TILE], Color32::TRANSPARENT);
    for x in 0..TILE {
        let blade = 1 + (noise(x, 0, 3) % 2) as usize;
        for y in 0..6 {
            image[(x, y)] = if y < 2 - blade.min(2) {
                Color32::TRANSPARENT
            } else if y < 4 {
                greens[(x + y) % 2]
            } else {
                dirt
            };
        }
    }
    image
}
