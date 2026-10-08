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

use super::layout::{MATTRESS_TOP, Mark, PIPE_H, PIPE_LIP_W};
use super::nature::Deco;
use crate::model::NodeKind;
use crate::orca::AgentIdentity;

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

/// An ant's body colors. Claude and Codex ants differ in brightness as well as hue (light
/// amber against dark slate with teal glints), so they stay apart under color blindness; red
/// means failed whatever the agent, and amber is kept light so it never reads as red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coat {
    /// No known agent, and the Queen: the sprite's own grey.
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

/// The grey sprite's colors, outline first, then body, shade, highlight, dark and darkest.
const GREY_COATS: [[u8; 3]; 6] = [
    [4, 28, 34],
    [139, 164, 172],
    [111, 128, 133],
    [190, 213, 221],
    [69, 89, 94],
    [55, 71, 77],
];
/// [`GREY_COATS`] recolored for Claude: light amber.
const CLAUDE_COATS: [[u8; 3]; 6] = [
    [44, 22, 6],
    [226, 162, 94],
    [192, 126, 68],
    [250, 212, 156],
    [142, 86, 42],
    [112, 66, 34],
];
/// [`GREY_COATS`] recolored for Codex: dark slate with teal glints.
const CODEX_COATS: [[u8; 3]; 6] = [
    [2, 8, 10],
    [62, 78, 86],
    [46, 60, 66],
    [84, 200, 186],
    [32, 42, 46],
    [24, 32, 36],
];

/// How to draw one ant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AntLook {
    pub kind: NodeKind,
    pub coat: Coat,
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
    officer_cap: TextureHandle,
    ant_grey: TextureHandle,
    ant_red: TextureHandle,
    ant_claude: TextureHandle,
    ant_codex: TextureHandle,
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
        let recolored = |name: &str, coats: [[u8; 3]; 6]| {
            ctx.load_texture(
                format!("ant_farm/{name}"),
                recolor(decode(ANT_GREY), &GREY_COATS, &coats),
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
            officer_cap: ctx.load_texture(
                "ant_farm/officer_cap",
                officer_cap(),
                TextureOptions::NEAREST,
            ),
            ant_grey: png("ant_grey", ANT_GREY),
            ant_red: png("ant_red", ANT_RED),
            ant_claude: recolored("ant_claude", CLAUDE_COATS),
            ant_codex: recolored("ant_codex", CODEX_COATS),
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

    /// A piece of scenery at `k` points per map texel (rather than its own scale), standing on
    /// `foot`, turned `angle` radians about `pivot`, multiplied by `tint`.
    pub fn deco_turned(
        &self,
        painter: &Painter,
        deco: Deco,
        foot: Pos2,
        k: f32,
        (angle, pivot): (f32, Pos2),
        tint: Color32,
    ) {
        let Some(texture) = self.nature.get(&deco) else {
            return;
        };
        let [w, h] = deco.size();
        let size = vec2(w as f32 * k, h as f32 * k);
        let rect = Rect::from_min_size(foot - vec2(size.x / 2.0, size.y), size);
        let mut mesh = egui::Mesh::with_texture(texture.id());
        mesh.add_rect_with_uv(rect, unit_uv(), tint);
        if angle != 0.0 {
            mesh.rotate(egui::emath::Rot2::from_angle(angle), pivot);
        }
        painter.add(mesh);
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

    /// A star: a dot, or a little cross for a bright one.
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

    /// A wooden bed in the resting area filling `rect` (see [`super::layout::BED_W`]): a
    /// headboard on the left, where a sleeper's head lies, a mattress and a pillow.
    pub fn bed(&self, painter: &Painter, rect: Rect, px: f32) {
        let wood = Color32::from_rgb(0x8a, 0x4b, 0x1c);
        let dark = Color32::from_rgb(0x5a, 0x30, 0x12);
        let sheet = Color32::from_rgb(0xe8, 0xdc, 0xc0);
        let pillow = Color32::from_rgb(0xfb, 0xf4, 0xe6);
        let at = |x: f32, y: f32, w: f32, h: f32, color: Color32| {
            let min = rect.min + vec2(x * px, y * px);
            painter.rect_filled(Rect::from_min_size(min, vec2(w * px, h * px)), 0.0, color);
        };
        let (w, h) = (rect.width() / px, rect.height() / px);
        let mattress = h - MATTRESS_TOP;
        // Legs, the side rail, the mattress on it, the boards at either end.
        at(1.0, h - 1.0, 1.0, 1.0, dark);
        at(w - 2.0, h - 1.0, 1.0, 1.0, dark);
        at(0.0, h - 3.0, w, 2.0, wood);
        at(0.0, h - 2.0, w, 1.0, dark);
        at(2.0, mattress, w - 3.0, 3.0, sheet);
        at(
            2.0,
            mattress + 2.0,
            w - 3.0,
            1.0,
            Color32::from_rgb(0xc8, 0xb8, 0x98),
        );
        at(0.0, 0.0, 2.0, h - 1.0, wood);
        at(0.0, 0.0, 2.0, 1.0, dark);
        at(w - 1.0, mattress - 1.0, 1.0, h - mattress, wood);
        at(3.0, mattress - 2.0, 4.0, 2.0, pillow);
    }

    /// The blanket over the back half of the sleeper at `cell` lying on the bed at `bed`,
    /// its head and kicking legs left out.
    pub fn blanket(&self, painter: &Painter, bed: Rect, cell: Rect, px: f32, alpha: f32) {
        let quilt = Color32::from_rgb(0x4a, 0x6f, 0xb8).gamma_multiply(alpha);
        let fold = Color32::from_rgb(0x9c, 0xb8, 0xe8).gamma_multiply(alpha);
        let top = bed.bottom() - (MATTRESS_TOP + 3.0) * px;
        let body = Rect::from_min_max(
            pos2(cell.left() + 0.45 * cell.width(), top),
            pos2(bed.right() - px, bed.bottom() - (MATTRESS_TOP - 2.0) * px),
        );
        painter.rect_filled(body, 0.0, quilt);
        let edge = Rect::from_min_size(body.min, vec2(px, body.height()));
        painter.rect_filled(edge, 0.0, fold);
    }

    /// An ant filling `cell`.
    pub fn ant(&self, painter: &Painter, cell: Rect, look: AntLook) {
        let texture = match look.coat {
            Coat::Plain => &self.ant_grey,
            Coat::Claude => &self.ant_claude,
            Coat::Codex => &self.ant_codex,
            Coat::Failed => &self.ant_red,
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
        if matches!(look.kind, NodeKind::Coordinator | NodeKind::Attendant) {
            // Share the body's UVs so the cap follows its gait, facing and belly-up poses.
            painter.image(
                self.officer_cap.id(),
                cell,
                uv,
                Color32::WHITE.gamma_multiply(look.alpha),
            );
        }
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

    /// Karl's green warp pipe standing on the surface at `foot` (the middle of its base),
    /// `stretch` texels taller while it pops a message out, with `text` on its body.
    pub fn pipe(
        &self,
        painter: &Painter,
        foot: Pos2,
        px: f32,
        stretch: f32,
        text: &str,
        font: egui::FontId,
    ) {
        let outline = Color32::from_rgb(0x10, 0x3a, 0x12);
        let green = Color32::from_rgb(0x3c, 0xb0, 0x3c);
        let light = Color32::from_rgb(0x9a, 0xe8, 0x6a);
        let shade = Color32::from_rgb(0x24, 0x7c, 0x28);
        let lip_w = PIPE_LIP_W * px;
        let body_w = lip_w - 4.0 * px;
        let lip_h = 7.0 * px;
        let top = foot.y - (PIPE_H + stretch) * px;
        let lip = Rect::from_min_size(pos2(foot.x - lip_w / 2.0, top), vec2(lip_w, lip_h));
        let body = Rect::from_min_max(
            pos2(foot.x - body_w / 2.0, lip.bottom()),
            pos2(foot.x + body_w / 2.0, foot.y + px),
        );
        // Each piece: outline, green, a highlight down the left, shade down the right.
        for part in [body, lip] {
            painter.rect_filled(part.expand(px), 0.0, outline);
            painter.rect_filled(part, 0.0, green);
            let stripe = |x: f32, w: f32| {
                Rect::from_min_size(
                    pos2(part.left() + x * px, part.top()),
                    vec2(w * px, part.height()),
                )
            };
            painter.rect_filled(stripe(2.0, 2.0), 0.0, light);
            painter.rect_filled(stripe(5.0, 1.0), 0.0, light);
            let right = part.width() / px;
            painter.rect_filled(stripe(right - 4.0, 2.0), 0.0, shade);
        }
        // The dark opening on top and the seam under the lip.
        painter.rect_filled(
            Rect::from_min_size(lip.min + vec2(2.0 * px, 0.0), vec2(lip_w - 4.0 * px, px)),
            0.0,
            outline,
        );
        painter.rect_filled(
            Rect::from_min_size(pos2(body.left(), lip.bottom()), vec2(body_w, px)),
            0.0,
            outline,
        );
        // The name across the body, shrunk to fit it.
        let room = body_w - 4.0 * px;
        let mut galley = painter.layout_no_wrap(text.to_string(), font.clone(), outline);
        if galley.size().x > room {
            let size = (font.size * room / galley.size().x).max(6.0);
            galley = painter.layout_no_wrap(
                text.to_string(),
                egui::FontId::new(size, font.family),
                outline,
            );
        }
        let at = pos2(
            foot.x - galley.size().x / 2.0,
            (lip.bottom() + foot.y) / 2.0 - galley.size().y / 2.0,
        );
        painter.galley(at, galley, outline);
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

    /// A builder's hard hat and hammer on the ant at `cell`; the hammer is raised on even
    /// `step`s and strikes the face on odd ones, with a spark.
    pub fn builder(&self, painter: &Painter, cell: Rect, facing_right: bool, px: f32, step: usize) {
        let hat = Color32::from_rgb(0xf2, 0xc1, 0x2e);
        let brim = Color32::from_rgb(0xc8, 0x8e, 0x1a);
        let wood = Color32::from_rgb(0x8a, 0x4b, 0x1c);
        let iron = Color32::from_rgb(0x8f, 0x96, 0x9e);
        // Texel `(x, y)` of the left-facing sprite, mirrored when the ant faces right.
        let dot = |x: i32, y: i32, color: Color32| {
            let x = if facing_right { 15 - x } else { x };
            let min = cell.min + vec2(x as f32 * px, y as f32 * px);
            painter.rect_filled(Rect::from_min_size(min, vec2(px, px)), 0.0, color);
        };
        for x in 2..=4 {
            dot(x, 5, hat);
        }
        for x in 1..=5 {
            dot(x, 6, hat);
        }
        for x in 0..=6 {
            dot(x, 7, brim);
        }
        let raised = step.is_multiple_of(2);
        type Texels = &'static [(i32, i32)];
        let (handle, head): (Texels, Texels) = if raised {
            (
                &[(1, 11), (0, 10), (-1, 9), (-2, 8)],
                &[(-4, 6), (-3, 6), (-2, 6), (-4, 7), (-3, 7), (-2, 7)],
            )
        } else {
            (
                &[(1, 12), (0, 12), (-1, 12), (-2, 12)],
                &[(-4, 11), (-3, 11), (-4, 12), (-3, 12), (-4, 13), (-3, 13)],
            )
        };
        for &(x, y) in handle {
            dot(x, y, wood);
        }
        for &(x, y) in head {
            dot(x, y, iron);
        }
        if !raised {
            let spark = Color32::from_rgb(0xff, 0xf0, 0x8a);
            dot(-6, 11, spark);
            dot(-5, 14, spark);
        }
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

/// A peaked officer's cap above the left-facing head. Its brass band identifies rank
/// without recoloring the agent or occupying the space reserved for state bubbles.
fn officer_cap() -> ColorImage {
    let rows = [
        b"..#####." as &[u8],
        b".#sssss#",
        b".#sssss#",
        b".#bbbbb#",
        b"######..",
    ];
    let mut image = ColorImage::filled([16, 16 * ANT_FRAMES], Color32::TRANSPARENT);
    for frame in 0..ANT_FRAMES {
        // The second walk frame lowers the head by one texel.
        for (y, row) in rows.iter().enumerate() {
            for (x, pixel) in row.iter().enumerate() {
                image[(x, frame * 16 + 4 + frame + y)] = match pixel {
                    b'#' => Color32::from_rgb(0x04, 0x1c, 0x22),
                    b's' => Color32::from_rgb(0x36, 0x48, 0x58),
                    b'b' => Color32::from_rgb(0xed, 0xcf, 0x8b),
                    _ => Color32::TRANSPARENT,
                };
            }
        }
    }
    image
}

/// `image` with each color of `from` swapped for the same slot of `to`; others kept.
fn recolor(mut image: ColorImage, from: &[[u8; 3]], to: &[[u8; 3]]) -> ColorImage {
    for pixel in &mut image.pixels {
        let [r, g, b, a] = pixel.to_srgba_unmultiplied();
        if let Some(i) = from.iter().position(|c| *c == [r, g, b]) {
            let [r, g, b] = to[i];
            *pixel = Color32::from_rgba_unmultiplied(r, g, b, a);
        }
    }
    image
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::graph::contrast_ratio;

    #[test]
    fn coats_cover_every_color_of_the_grey_ant() {
        let image = decode(ANT_GREY);
        for pixel in image.pixels.iter().filter(|p| p.a() > 0) {
            let [r, g, b, _] = pixel.to_srgba_unmultiplied();
            assert!(GREY_COATS.contains(&[r, g, b]), "unmapped {pixel:?}");
        }
    }

    /// Claude and Codex ants must differ in brightness, not only hue.
    #[test]
    fn claude_and_codex_differ_in_brightness() {
        let body = |coats: [[u8; 3]; 6]| {
            let [r, g, b] = coats[1];
            Color32::from_rgb(r, g, b)
        };
        let ratio = contrast_ratio(body(CLAUDE_COATS), body(CODEX_COATS));
        assert!(ratio > 3.0, "{ratio}");
    }

    #[test]
    fn failed_wins_over_the_agent() {
        assert_eq!(Coat::new(Some(&AgentIdentity::Claude), true), Coat::Failed);
        assert_eq!(Coat::new(Some(&AgentIdentity::Codex), false), Coat::Codex);
        assert_eq!(Coat::new(None, false), Coat::Plain);
    }
}
