//! Shared cutout renderer for the mine. All coordinates are farm texels; textures have
//! hard alpha, a configurable ink outline and NEAREST sampling, just like the dwarves.

use super::nature::Deco;
use crate::ui::dwarf::{
    cutout::{self, Finish, Outline, Piece, Xf, blob, capsule, ellipse},
    pixel::{self, Mapper},
};
use eframe::egui::{
    self, Color32, ColorImage, Painter, Pos2, Rect, Shape, TextureHandle, TextureOptions, pos2,
    vec2,
};
use std::collections::HashMap;

pub const fn rgb(n: u32) -> Color32 {
    Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)
}
pub const INK: Color32 = Finish::INK.outline_color;
pub const STONE: Color32 = rgb(0x706e68);
pub const STONE_DARK: Color32 = rgb(0x484b4c);
pub const STONE_LIGHT: Color32 = rgb(0x93958c);
pub const WOOD: Color32 = rgb(0x87613e);
pub const WOOD_DARK: Color32 = rgb(0x503c2b);
pub const WOOD_LIGHT: Color32 = rgb(0xb08b59);
pub const IRON: Color32 = rgb(0x4c535a);
pub const GOLD: Color32 = rgb(0xe0a830);
pub const GOLD_DARK: Color32 = rgb(0x8e612b);
pub const LIGHT: Color32 = rgb(0xffdfa0);
pub const RED: Color32 = rgb(0xa33536);
pub const BLUE: Color32 = rgb(0x69aec4);
pub const PARCHMENT: Color32 = rgb(0xeee0b9);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Prop {
    Nature(Deco),
    Cave(u8),
    Masonry,
    Peaks,
    Entrance,
    PitheadWinch,
    Spoil,
    Rubble(u8, u8),
    CleanupHammer,
    Bookcase,
    ArchiveCabinet,
    Lectern,
    Soot,
    Portal,
    Scroll,
    Support,
    Shoring,
    Torch,
    MineCart,
    CartYoke,
    ToolRack,
    Throne,
    Banner,
    Brazier,
    Chest,
    Treasure(u8),
    Bunk,
    Blanket,
    RuneStone,
    CrackedStone,
    ReceivingStone,
    Basin,
    ScryingBasin,
    Tablets,
    Bubble(u8),
    Snore,
    Pebble,
}

impl Prop {
    pub fn size(self) -> [usize; 2] {
        match self {
            Self::Nature(d) => d.size(),
            Self::Cave(_) | Self::Masonry => [96, 64],
            Self::Peaks => [192, 52],
            Self::Entrance => [40, 28],
            Self::PitheadWinch => [24, 18],
            Self::Spoil => [26, 10],
            Self::Rubble(_, _) => [36, 22],
            Self::CleanupHammer => [26, 40],
            Self::Bookcase => [30, 34],
            Self::ArchiveCabinet => [20, 26],
            Self::Lectern => [22, 22],
            Self::Soot => [14, 20],
            Self::Portal => [32, 40],
            Self::Scroll => [18, 12],
            Self::Support => [36, 22],
            Self::Shoring => [20, 10],
            Self::Torch => [12, 18],
            Self::MineCart => [26, 14],
            Self::CartYoke => [26, 29],
            Self::ToolRack => [23, 21],
            Self::Throne => [26, 32],
            Self::Banner => [14, 24],
            Self::Brazier => [16, 22],
            Self::Chest => [26, 22],
            Self::Treasure(_) => [18, 14],
            Self::Bunk => [24, 12],
            Self::Blanket => [12, 8],
            Self::RuneStone | Self::CrackedStone | Self::ReceivingStone => [30, 43],
            Self::Basin | Self::ScryingBasin => [28, 24],
            Self::Tablets => [17, 25],
            Self::Bubble(_) => [16, 16],
            Self::Snore => [7, 7],
            Self::Pebble => [5, 5],
        }
    }

    /// The open portal's veil, fires, work and moving cart wheels change frame.
    /// A cart is drawn only during travel.
    pub fn frame(self, now_s: f64) -> u8 {
        match self {
            Self::Portal => (now_s * 4.0).floor().rem_euclid(PORTAL_PHASES as f64) as u8,
            Self::MineCart
            | Self::Torch
            | Self::Brazier
            | Self::Entrance
            | Self::ScryingBasin
            | Self::ReceivingStone => (now_s * 6.0).floor().rem_euclid(4.0) as u8,
            _ => 0,
        }
    }
}

const PORTAL_PHASES: u8 = 8;

/// Eight veil positions at each of four message-flare strengths, all cached as frames.
/// Reduced motion freezes both the open veil and the mail pulse.
pub fn portal_frame(now_s: f64, flare: u8, still: bool) -> u8 {
    if still {
        0
    } else {
        Prop::Portal.frame(now_s) + flare.min(3) * PORTAL_PHASES
    }
}

#[derive(Hash, PartialEq, Eq)]
struct Key {
    prop: Prop,
    size: [usize; 2],
    palette: String,
    outline: Outline,
    frame: u8,
}
struct Cached {
    texture: TextureHandle,
    used: u64,
}
#[derive(Default)]
pub struct WorldArt {
    textures: HashMap<Key, Cached>,
    palette: String,
    outline: Outline,
    mapper: Option<Mapper>,
    generation: u64,
}

impl WorldArt {
    pub fn begin_frame(&mut self, palette: &str, outline: Outline) {
        self.generation += 1;
        self.outline = outline;
        if self.palette != palette {
            self.palette = palette.to_owned();
            self.mapper = Mapper::selection(palette);
        }
        // Retain all four animation frames, but release old sizes/palettes after resizing.
        self.textures.retain(|k, c| {
            k.palette == palette && k.outline == outline && c.used + 120 >= self.generation
        });
    }

    pub fn map(&mut self, image: &mut ColorImage) {
        if let Some(m) = &mut self.mapper {
            m.apply(image);
        }
    }

    /// Keep walls separate from the dwarf's ink, and uncut rock separate from walls.
    /// Tiny palettes can otherwise map all three materials to the same colour. Lift the
    /// source material only when necessary, then use the same nearest-colour Mapper.
    pub fn map_stone(&mut self, image: &mut ColorImage, worked: bool) {
        let Some(mapper) = &mut self.mapper else {
            return;
        };
        fn distinct(mapper: &mut Mapper, c: Color32, avoid: &[Color32]) -> Color32 {
            for step in 0..=32 {
                let mapped = mapper.map(c.lerp_to_gamma(Color32::WHITE, step as f32 / 32.0));
                if !avoid.contains(&mapped) {
                    return mapped;
                }
            }
            mapper.map(c)
        }
        let ink = mapper.map(INK);
        let floor = distinct(mapper, super::excavation::WALL, &[ink]);
        let avoid = if worked { vec![ink] } else { vec![ink, floor] };
        let mut colors = HashMap::new();
        for c in &mut image.pixels {
            let source = *c;
            if source.a() == 0 {
                continue;
            }
            if source == INK {
                *c = ink;
                continue;
            }
            *c = *colors
                .entry(source)
                .or_insert_with(|| distinct(mapper, source, &avoid));
        }
    }

    pub fn map_scene(
        &mut self,
        ctx: &egui::Context,
        layer: egui::LayerId,
        start: egui::layers::ShapeIdx,
    ) {
        let Some(mapper) = &mut self.mapper else {
            return;
        };
        ctx.graphics_mut(|g| {
            let list = g.entry(layer);
            for i in start.0..list.next_idx().0 {
                list.mutate_shape(egui::layers::ShapeIdx(i), |s| mapper.shape(&mut s.shape));
            }
        });
    }

    fn texture(
        &mut self,
        painter: &Painter,
        prop: Prop,
        size: [usize; 2],
        frame: u8,
    ) -> egui::TextureId {
        let key = Key {
            prop,
            size,
            palette: self.palette.clone(),
            outline: self.outline,
            frame,
        };
        if !self.textures.contains_key(&key) {
            let mut image = render(prop, size, frame, self.outline);
            if matches!(prop, Prop::Cave(_) | Prop::Masonry) {
                self.map_stone(&mut image, true);
            } else {
                self.map(&mut image);
            }
            let texture = painter.ctx().load_texture(
                format!("mine/{prop:?}/{size:?}/{frame}/{}", self.palette),
                image,
                TextureOptions::NEAREST,
            );
            self.textures.insert(
                key,
                Cached {
                    texture,
                    used: self.generation,
                },
            );
        }
        let key = Key {
            prop,
            size,
            palette: self.palette.clone(),
            outline: self.outline,
            frame,
        };
        let cached = self.textures.get_mut(&key).expect("prop just cached");
        cached.used = self.generation;
        cached.texture.id()
    }

    pub fn paint(
        &mut self,
        painter: &Painter,
        prop: Prop,
        rect: Rect,
        px: f32,
        frame: u8,
        alpha: f32,
    ) {
        let size = [
            (rect.width() / px).round().max(1.0) as usize,
            (rect.height() / px).round().max(1.0) as usize,
        ];
        let texture = self.texture(painter, prop, size, frame);
        let rect = Rect::from_min_size(
            pos2(
                (rect.left() / px).round() * px,
                (rect.top() / px).round() * px,
            ),
            vec2(size[0] as f32, size[1] as f32) * px,
        );
        painter.image(
            texture,
            rect,
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE.gamma_multiply(alpha),
        );
    }
    pub fn paint_turned(
        &mut self,
        painter: &Painter,
        prop: Prop,
        rect: Rect,
        px: f32,
        (angle, pivot): (f32, Pos2),
        alpha: f32,
    ) {
        let size = [
            (rect.width() / px).round().max(1.0) as usize,
            (rect.height() / px).round().max(1.0) as usize,
        ];
        let texture = self.texture(painter, prop, size, 0);
        let rect = Rect::from_min_size(
            pos2(
                (rect.left() / px).round() * px,
                (rect.top() / px).round() * px,
            ),
            vec2(size[0] as f32, size[1] as f32) * px,
        );
        let mut mesh = egui::Mesh::with_texture(texture);
        mesh.add_rect_with_uv(
            rect,
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE.gamma_multiply(alpha),
        );
        mesh.rotate(egui::emath::Rot2::from_angle(angle), pivot);
        painter.add(mesh);
    }
}

pub fn render(prop: Prop, size: [usize; 2], frame: u8, outline: Outline) -> ColorImage {
    if prop == Prop::CleanupHammer {
        return super::hammer::render(size, outline);
    }
    let room = matches!(prop, Prop::Cave(_) | Prop::Masonry);
    let [w, h] = if room { size } else { prop.size() }.map(|n| n as f32);
    let mut c = Canvas {
        shapes: Vec::new(),
        finish: outline.finish((w / size[0] as f32).max(h / size[1] as f32)),
    };
    match prop {
        Prop::Cave(seed) => super::excavation::cave(&mut c, w, h, seed),
        Prop::Masonry => super::excavation::masonry(&mut c, w, h),
        _ => draw(&mut c, prop, frame),
    }
    // Scale shapes first: rasterize expects square texels even when a prop is stretched.
    let sx = size[0] as f32 / w;
    let sy = size[1] as f32 / h;
    for shape in &mut c.shapes {
        if let Shape::Mesh(mesh) = shape {
            for v in &mut std::sync::Arc::make_mut(mesh).vertices {
                v.pos.x *= sx;
                v.pos.y *= sy;
            }
        } else if let Shape::Path(path) = shape {
            for p in &mut path.points {
                p.x *= sx;
                p.y *= sy;
            }
            path.stroke.width *= sx.min(sy);
        }
    }
    pixel::rasterize(
        &c.shapes,
        Rect::from_min_size(Pos2::ZERO, vec2(size[0] as f32, size[1] as f32)),
        size,
        1,
    )
}

/// A few pieces in the same ink finish as a dwarf. Details are painted inside the cut.
pub struct Canvas {
    pub shapes: Vec<Shape>,
    finish: Finish,
}
impl Canvas {
    pub fn cut(&mut self, points: Vec<Pos2>, fill: Color32) {
        cutout::draw(
            &mut self.shapes,
            Piece::default().cut(points, fill),
            Xf::IDENTITY,
            &self.finish,
        );
    }
    pub fn paint(&mut self, points: Vec<Pos2>, fill: Color32) {
        cutout::draw(
            &mut self.shapes,
            Piece::default().paint(points, fill),
            Xf::IDENTITY,
            &self.finish,
        );
    }
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, fill: Color32) {
        self.cut(rect(x, y, w, h), fill);
    }
    pub fn detail(&mut self, x: f32, y: f32, w: f32, h: f32, fill: Color32) {
        self.paint(rect(x, y, w, h), fill);
    }
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), r: f32, color: Color32) {
        self.paint(capsule(pos2(a.0, a.1), pos2(b.0, b.1), r), color);
    }
    pub fn oval(&mut self, x: f32, y: f32, rx: f32, ry: f32, color: Color32) {
        self.cut(ellipse(pos2(x, y), rx, ry, 16), color);
    }
}
pub fn pts(p: &[(f32, f32)]) -> Vec<Pos2> {
    p.iter().map(|&(x, y)| pos2(x, y)).collect()
}
pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Vec<Pos2> {
    pts(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)])
}

fn timber(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32) {
    c.rect(x, y, w, h, WOOD);
    c.detail(x, y, 1.0, h, WOOD_LIGHT);
    c.detail(x + w - 1.0, y, 1.0, h, WOOD_DARK);
    if h > w {
        c.line(
            (x + w * 0.5, y + 2.0),
            (x + w * 0.7, y + h - 2.0),
            0.45,
            WOOD_DARK,
        );
    }
    for yy in [y + 1.0, y + h - 2.0] {
        c.detail(x, yy, w, 1.0, IRON);
    }
}
fn flame(c: &mut Canvas, x: f32, y: f32, frame: u8, s: f32) {
    let lean = [0.0, 1.0, -1.0, 1.5][frame as usize % 4];
    let p = |q: &[(f32, f32)]| {
        q.iter()
            .map(|&(a, b)| pos2(x + a * s, y + b * s))
            .collect::<Vec<_>>()
    };
    c.cut(
        blob(
            &p(&[
                (-3.0, 0.0),
                (-3.5, -4.0),
                (-1.0, -7.0),
                (lean, -10.0),
                (3.5, -4.0),
                (3.0, 0.0),
            ]),
            4,
        ),
        rgb(0xe77932),
    );
    c.paint(
        blob(&p(&[(-2.0, -1.0), (0.5, -7.0), (2.0, -3.0), (1.0, 0.0)]), 4),
        GOLD,
    );
    c.paint(p(&[(-1.0, 0.0), (0.0, -4.0), (1.5, 0.0)]), LIGHT);
}
fn gem(c: &mut Canvas, x: f32, y: f32, s: f32, color: Color32) {
    c.cut(
        pts(&[
            (x - s, y),
            (x - s * 0.5, y - s),
            (x + s * 0.5, y - s),
            (x + s, y),
            (x, y + s),
        ]),
        color,
    );
    c.paint(
        pts(&[(x - s, y), (x - s * 0.5, y - s), (x, y), (x, y + s)]),
        LIGHT,
    );
    c.line((x, y), (x + s, y), 0.4, INK);
}
fn rune(c: &mut Canvas, x: f32, y: f32, color: Color32) {
    c.line((x, y), (x, y + 5.0), 0.6, color);
    c.line((x, y), (x + 3.0, y + 2.0), 0.6, color);
    c.line((x + 3.0, y + 2.0), (x, y + 3.0), 0.6, color);
}
fn coins(c: &mut Canvas, x: f32, y: f32) {
    for (dx, dy) in [
        (1.0, 0.0),
        (5.0, -2.0),
        (10.0, 0.0),
        (7.0, 2.0),
        (2.0, 3.0),
        (12.0, 3.0),
    ] {
        c.oval(x + dx, y + dy, 2.4, 1.3, GOLD);
        c.detail(x + dx - 1.0, y + dy - 1.0, 2.0, 1.0, LIGHT);
    }
}
fn draw(c: &mut Canvas, prop: Prop, frame: u8) {
    match prop {
        Prop::Cave(_) | Prop::Masonry => unreachable!("rooms draw at their actual size"),
        Prop::CleanupHammer => unreachable!("hammer uses its generated sprite"),
        Prop::Rubble(health, seed) => super::rubble::draw(c, health, seed),
        Prop::Bookcase | Prop::ArchiveCabinet | Prop::Lectern => super::ledger::draw(c, prop),
        Prop::Nature(d) => super::nature::draw(c, d),
        Prop::Peaks => {
            // Matching edge heights make the distant range repeat without a seam.
            c.paint(
                pts(&[
                    (0.0, 40.0),
                    (18.0, 22.0),
                    (36.0, 34.0),
                    (59.0, 7.0),
                    (83.0, 32.0),
                    (115.0, 0.0),
                    (141.0, 29.0),
                    (166.0, 13.0),
                    (192.0, 40.0),
                    (192.0, 52.0),
                    (0.0, 52.0),
                ]),
                rgb(0x718994),
            );
            for (x, y, w) in [
                (18.0, 22.0, 9.0),
                (59.0, 7.0, 13.0),
                (115.0, 0.0, 15.0),
                (166.0, 13.0, 11.0),
            ] {
                c.paint(
                    pts(&[
                        (x - w, y + w),
                        (x, y),
                        (x + w, y + w),
                        (x + 3.0, y + w - 3.0),
                        (x - 1.0, y + w + 1.0),
                        (x - 5.0, y + w - 4.0),
                    ]),
                    rgb(0xc5d0ca),
                );
                c.paint(
                    pts(&[
                        (x, y + 2.0),
                        (x + 2.0, y + 17.0),
                        (x + 20.0, 48.0),
                        (x + 35.0, 50.0),
                    ]),
                    rgb(0x627884),
                );
            }
            c.paint(
                pts(&[
                    (0.0, 45.0),
                    (26.0, 37.0),
                    (51.0, 47.0),
                    (87.0, 35.0),
                    (119.0, 45.0),
                    (149.0, 35.0),
                    (176.0, 44.0),
                    (192.0, 45.0),
                    (192.0, 52.0),
                    (0.0, 52.0),
                ]),
                rgb(0x596d70),
            );
        }
        Prop::Support => {
            timber(c, 2.0, 3.0, 3.0, 18.0);
            timber(c, 31.0, 3.0, 3.0, 18.0);
            c.cut(pts(&[(4.0, 3.0), (11.0, 3.0), (4.0, 10.0)]), WOOD_DARK);
            c.cut(pts(&[(31.0, 3.0), (25.0, 3.0), (31.0, 10.0)]), WOOD_DARK);
            timber(c, 1.0, 1.0, 34.0, 3.0);
        }
        Prop::Shoring => {
            timber(c, 1.0, 1.0, 3.0, 8.0);
            timber(c, 16.0, 1.0, 3.0, 8.0);
            timber(c, 1.0, 2.0, 18.0, 2.0);
        }
        Prop::Soot => {
            let stain = rgb(0x343630);
            c.paint(
                pts(&[
                    (3.0, 1.0),
                    (6.0, 3.0),
                    (9.0, 1.0),
                    (8.0, 6.0),
                    (11.0, 8.0),
                    (9.0, 12.0),
                    (12.0, 15.0),
                    (9.0, 19.0),
                    (4.0, 19.0),
                    (2.0, 15.0),
                    (4.0, 11.0),
                    (2.0, 7.0),
                    (4.0, 5.0),
                ]),
                stain,
            );
            c.detail(1.0, 4.0, 1.0, 3.0, stain);
            c.detail(11.0, 5.0, 1.0, 2.0, stain);
        }
        Prop::Spoil => {
            c.cut(
                pts(&[
                    (1.0, 8.0),
                    (4.0, 5.0),
                    (9.0, 5.0),
                    (12.0, 1.0),
                    (17.0, 3.0),
                    (20.0, 6.0),
                    (23.0, 6.0),
                    (25.0, 9.0),
                ]),
                STONE_DARK,
            );
            for (x, y) in [(6.0, 6.0), (12.0, 4.0), (17.0, 6.0), (21.0, 8.0)] {
                c.paint(
                    pts(&[
                        (x - 3.0, y + 1.0),
                        (x - 1.0, y - 2.0),
                        (x + 2.0, y - 1.0),
                        (x + 3.0, y + 1.0),
                    ]),
                    STONE,
                );
                c.line((x - 1.0, y - 1.0), (x + 1.0, y - 1.0), 0.5, STONE_LIGHT);
            }
        }
        Prop::PitheadWinch => {
            // Hand-cranked drum with rope, a ratchet and splayed timber feet.
            for x in [5.0, 18.0] {
                c.cut(
                    pts(&[
                        (x - 3.0, 16.0),
                        (x - 1.0, 4.0),
                        (x + 1.0, 4.0),
                        (x + 3.0, 16.0),
                    ]),
                    WOOD_DARK,
                );
                c.line((x - 1.0, 6.0), (x - 2.0, 15.0), 0.5, WOOD);
            }
            c.rect(4.0, 5.0, 15.0, 7.0, WOOD);
            for x in [7.0, 9.0, 11.0, 13.0, 15.0] {
                c.line((x, 5.0), (x, 11.0), 0.5, WOOD_LIGHT);
            }
            c.line((14.0, 11.0), (22.0, 16.0), 0.6, WOOD_LIGHT);
            c.rect(3.0, 4.0, 2.0, 9.0, IRON);
            c.rect(18.0, 4.0, 2.0, 9.0, IRON);
            c.line((19.0, 8.0), (22.0, 8.0), 0.7, IRON);
            c.line((22.0, 8.0), (22.0, 3.0), 0.8, IRON);
            c.line((21.0, 3.0), (23.0, 3.0), 0.8, WOOD);
        }
        Prop::Entrance => {
            c.cut(
                pts(&[
                    (1.0, 27.0),
                    (3.0, 15.0),
                    (8.0, 7.0),
                    (17.0, 2.0),
                    (28.0, 4.0),
                    (35.0, 12.0),
                    (39.0, 27.0),
                ]),
                STONE,
            );
            c.paint(
                pts(&[
                    (3.0, 24.0),
                    (9.0, 9.0),
                    (19.0, 3.0),
                    (25.0, 5.0),
                    (12.0, 11.0),
                    (9.0, 26.0),
                ]),
                STONE_LIGHT,
            );
            c.rect(12.0, 9.0, 17.0, 18.0, INK);
            timber(c, 10.0, 8.0, 4.0, 19.0);
            timber(c, 28.0, 8.0, 4.0, 19.0);
            timber(c, 9.0, 7.0, 24.0, 4.0);
            c.line((32.0, 12.0), (37.0, 12.0), 0.8, IRON);
            c.line((37.0, 12.0), (37.0, 16.0), 0.6, IRON);
            c.rect(34.0, 16.0, 5.0, 7.0, IRON);
            c.detail(
                35.0,
                17.0,
                3.0,
                4.0,
                if frame.is_multiple_of(2) { LIGHT } else { GOLD },
            );
            c.detail(16.0, 26.0, 11.0, 1.0, STONE_LIGHT);
        }
        Prop::Portal => {
            let flare = (frame / PORTAL_PHASES).min(3) as f32 / 3.0;
            let angle =
                (frame % PORTAL_PHASES) as f32 / PORTAL_PHASES as f32 * std::f32::consts::TAU;
            let veil = rgb(0x254657).lerp_to_gamma(BLUE, flare * 0.7);
            // A deep doorway, a luminous inner threshold and wedge-cut arch stones.
            c.cut(
                pts(&[
                    (4.0, 35.0),
                    (4.0, 14.0),
                    (7.0, 7.0),
                    (12.0, 3.0),
                    (20.0, 3.0),
                    (25.0, 7.0),
                    (28.0, 14.0),
                    (28.0, 35.0),
                ]),
                STONE_DARK,
            );
            c.cut(
                pts(&[
                    (9.0, 33.0),
                    (9.0, 15.0),
                    (11.0, 11.0),
                    (16.0, 8.0),
                    (21.0, 11.0),
                    (23.0, 15.0),
                    (23.0, 33.0),
                ]),
                BLUE.lerp_to_gamma(LIGHT, flare),
            );
            c.paint(
                pts(&[
                    (11.0, 32.0),
                    (11.0, 16.0),
                    (13.0, 13.0),
                    (16.0, 11.0),
                    (20.0, 14.0),
                    (21.0, 17.0),
                    (21.0, 32.0),
                ]),
                veil,
            );
            // Two curling ribbons move inside the opening; the stonework stays still.
            // Movement changes the silhouette of the light so even tiny palettes show it.
            for (offset, color) in [
                (0.0, BLUE.lerp_to_gamma(LIGHT, flare)),
                (
                    std::f32::consts::PI,
                    rgb(0x408698).lerp_to_gamma(BLUE, flare),
                ),
            ] {
                let points: Vec<_> = (0..=12)
                    .map(|i| {
                        let t = i as f32 / 12.0;
                        let a = angle + offset + t * 3.8;
                        let r = 1.0 - t * 0.75;
                        pos2(16.0 + 4.0 * r * a.cos(), 22.0 + 8.0 * r * a.sin())
                    })
                    .collect();
                for segment in points.windows(2) {
                    c.paint(capsule(segment[0], segment[1], 0.8), color);
                }
            }
            for x in [4.0, 24.0] {
                for y in [15.0, 24.0] {
                    c.rect(x, y, 4.0, 8.0, STONE);
                    c.detail(x, y, 1.0, 7.0, STONE_LIGHT);
                    let ink = GOLD_DARK.lerp_to_gamma(LIGHT, flare);
                    c.line((x + 2.0, y + 2.0), (x + 2.0, y + 6.0), 0.5, ink);
                    c.line((x + 2.0, y + 2.0), (x + 3.0, y + 3.5), 0.5, ink);
                }
            }
            for points in [
                vec![(4.0, 13.0), (7.0, 6.0), (11.0, 9.0), (8.0, 14.0)],
                vec![(8.0, 5.0), (13.0, 2.0), (14.0, 7.0), (11.0, 9.0)],
                vec![(19.0, 2.0), (24.0, 5.0), (21.0, 9.0), (18.0, 7.0)],
                vec![(25.0, 6.0), (28.0, 13.0), (24.0, 14.0), (21.0, 9.0)],
            ] {
                c.cut(pts(&points), STONE);
            }
            c.cut(
                pts(&[(13.0, 2.0), (19.0, 2.0), (18.0, 8.0), (14.0, 8.0)]),
                STONE_LIGHT,
            );
            c.paint(
                pts(&[(16.0, 3.0), (17.5, 5.0), (16.0, 7.0), (14.5, 5.0)]),
                GOLD.lerp_to_gamma(LIGHT, flare),
            );
            c.rect(2.0, 33.0, 28.0, 5.0, STONE_DARK);
            c.detail(5.0, 33.0, 22.0, 5.0, GOLD);
            c.detail(5.0, 33.0, 22.0, 1.0, LIGHT);
            if flare > 0.0 {
                for (x, y) in [(14.0, 18.0), (18.0, 26.0), (15.0, 30.0)] {
                    c.detail(x, y - flare * 2.0, 1.0, 2.0, LIGHT);
                }
            }
        }
        Prop::Scroll => {
            c.rect(3.0, 3.0, 12.0, 7.0, PARCHMENT);
            for x in [3.0, 15.0] {
                c.cut(capsule(pos2(x, 2.0), pos2(x, 9.0), 1.6), PARCHMENT);
                c.detail(x - 1.0, 3.0, 1.0, 6.0, rgb(0xb09b74));
            }
            c.detail(8.0, 3.0, 1.0, 7.0, WOOD_DARK);
            c.detail(4.0, 6.0, 10.0, 1.0, WOOD_DARK);
            c.oval(9.0, 7.0, 2.0, 2.0, RED);
            c.detail(8.0, 6.0, 1.0, 1.0, rgb(0xf17d60));
        }
        Prop::CartYoke => {
            c.cut(
                pts(&[
                    (2.0, 26.0),
                    (2.0, 6.0),
                    (8.0, 2.0),
                    (18.0, 2.0),
                    (24.0, 6.0),
                    (24.0, 26.0),
                    (22.0, 26.0),
                    (22.0, 7.0),
                    (17.0, 4.0),
                    (9.0, 4.0),
                    (4.0, 7.0),
                    (4.0, 26.0),
                ]),
                IRON,
            );
            c.oval(13.0, 2.0, 2.0, 1.5, STONE_LIGHT);
        }
        Prop::MineCart => {
            // Open passenger tub, drawn over the rider's boots and lower body.
            c.cut(
                pts(&[(1.0, 2.0), (25.0, 2.0), (22.0, 9.0), (5.0, 9.0)]),
                IRON,
            );
            c.detail(5.0, 6.0, 17.0, 3.0, rgb(0x353b3c));
            c.line((2.0, 2.0), (24.0, 2.0), 0.65, STONE_LIGHT);
            for x in [5.0, 21.0] {
                c.detail(x, 4.0, 1.0, 1.0, STONE_LIGHT);
            }
            for x in [7.0, 20.0] {
                c.oval(x, 10.0, 3.0, 3.0, rgb(0x353b3c));
                if frame.is_multiple_of(2) {
                    c.line((x - 1.5, 10.0), (x + 1.5, 10.0), 0.5, STONE);
                } else {
                    c.line((x, 8.5), (x, 11.5), 0.5, STONE);
                }
            }
        }
        Prop::ToolRack => {
            timber(c, 2.0, 7.0, 19.0, 2.0);
            // A pick, shovel and loop of rope, hung from pegs on the rock wall.
            c.cut(capsule(pos2(6.0, 4.0), pos2(8.0, 19.0), 0.7), WOOD);
            c.cut(
                pts(&[
                    (1.0, 5.0),
                    (4.0, 2.0),
                    (10.0, 2.0),
                    (13.0, 6.0),
                    (9.0, 4.0),
                    (4.0, 4.0),
                ]),
                IRON,
            );
            c.cut(capsule(pos2(17.0, 3.0), pos2(16.0, 14.0), 0.7), WOOD);
            c.cut(
                pts(&[
                    (13.0, 13.0),
                    (19.0, 13.0),
                    (19.0, 17.0),
                    (16.0, 20.0),
                    (13.0, 17.0),
                ]),
                IRON,
            );
            c.line((14.0, 14.0), (14.0, 17.0), 0.45, STONE_LIGHT);
            c.cut(ellipse(pos2(5.0, 13.0), 2.5, 3.5, 12), WOOD_DARK);
            c.paint(
                ellipse(pos2(5.0, 13.0), 1.0, 2.0, 10),
                super::excavation::WALL,
            );
        }
        Prop::Torch => {
            c.rect(4.0, 10.0, 4.0, 6.0, IRON);
            c.line((6.0, 15.0), (8.0, 10.0), 1.0, STONE_LIGHT);
            flame(c, 6.0, 10.0, frame, 0.85);
            c.rect(3.0, 10.0, 6.0, 2.0, IRON);
        }
        Prop::Brazier => {
            c.cut(
                pts(&[(3.0, 20.0), (7.0, 14.0), (9.0, 14.0), (13.0, 20.0)]),
                IRON,
            );
            flame(c, 8.0, 11.0, frame, 0.9);
            c.cut(
                pts(&[(2.0, 11.0), (14.0, 11.0), (12.0, 16.0), (4.0, 16.0)]),
                IRON,
            );
            c.detail(3.0, 11.0, 10.0, 1.0, STONE_LIGHT);
            for x in [4.0, 8.0, 12.0] {
                c.detail(x, 12.0, 1.0, 2.0, GOLD);
            }
        }
        Prop::Throne => {
            c.cut(
                pts(&[
                    (4.0, 25.0),
                    (4.0, 5.0),
                    (9.0, 1.0),
                    (17.0, 1.0),
                    (22.0, 5.0),
                    (22.0, 25.0),
                ]),
                STONE,
            );
            c.rect(7.0, 5.0, 12.0, 18.0, STONE_DARK);
            c.line((8.0, 17.0), (8.0, 6.0), 0.6, GOLD);
            c.line((18.0, 6.0), (18.0, 17.0), 0.6, GOLD);
            c.paint(
                pts(&[
                    (10.0, 7.0),
                    (10.0, 3.0),
                    (13.0, 5.0),
                    (16.0, 3.0),
                    (16.0, 7.0),
                ]),
                GOLD,
            );
            c.rect(5.0, 20.0, 16.0, 5.0, RED);
            c.detail(6.0, 20.0, 14.0, 1.0, rgb(0xd35b4c));
            for x in [2.0, 20.0] {
                c.rect(x, 17.0, 4.0, 12.0, STONE);
                c.detail(x, 17.0, 4.0, 2.0, STONE_LIGHT);
            }
            c.rect(1.0, 29.0, 24.0, 2.0, STONE_LIGHT);
        }
        Prop::Banner => {
            c.cut(
                pts(&[
                    (2.0, 3.0),
                    (12.0, 3.0),
                    (12.0, 20.0),
                    (7.0, 23.0),
                    (2.0, 20.0),
                ]),
                RED,
            );
            c.detail(2.0, 4.0, 1.0, 15.0, GOLD);
            c.detail(11.0, 4.0, 1.0, 15.0, GOLD);
            c.rect(1.0, 1.0, 12.0, 2.0, GOLD_DARK);
            c.paint(
                pts(&[
                    (4.0, 10.0),
                    (4.0, 6.0),
                    (6.0, 8.0),
                    (7.0, 5.0),
                    (8.0, 8.0),
                    (10.0, 6.0),
                    (10.0, 10.0),
                ]),
                GOLD,
            );
            c.line((5.0, 18.0), (9.0, 12.0), 0.7, GOLD);
            c.paint(
                blob(
                    &pts(&[
                        (4.0, 14.0),
                        (6.0, 11.0),
                        (10.0, 12.0),
                        (11.0, 15.0),
                        (8.0, 13.0),
                    ]),
                    3,
                ),
                LIGHT,
            );
        }
        Prop::Chest => {
            c.cut(
                pts(&[
                    (3.0, 11.0),
                    (3.0, 4.0),
                    (7.0, 1.0),
                    (21.0, 2.0),
                    (23.0, 11.0),
                ]),
                WOOD,
            );
            c.rect(5.0, 4.0, 15.0, 6.0, WOOD_DARK);
            for x in [4.0, 20.0] {
                c.detail(x, 3.0, 2.0, 8.0, IRON);
            }
            coins(c, 5.0, 11.0);
            c.rect(2.0, 12.0, 22.0, 7.0, WOOD);
            for x in [4.0, 20.0] {
                c.rect(x, 12.0, 2.0, 7.0, IRON);
                c.detail(x, 14.0, 1.0, 1.0, STONE_LIGHT);
            }
            c.rect(11.0, 12.0, 3.0, 3.0, GOLD);
            coins(c, 8.0, 17.0);
        }
        Prop::Treasure(kind) => {
            c.cut(
                blob(
                    &pts(&[
                        (1.0, 11.0),
                        (5.0, 6.0),
                        (9.0, 4.0),
                        (15.0, 8.0),
                        (17.0, 12.0),
                    ]),
                    4,
                ),
                GOLD_DARK,
            );
            coins(c, 2.0, 9.0);
            gem(
                c,
                10.0,
                5.0,
                3.0,
                [RED, rgb(0x4da475), rgb(0x508acd)][kind as usize % 3],
            );
        }
        Prop::Bunk => {
            timber(c, 1.0, 1.0, 2.0, 10.0);
            timber(c, 21.0, 4.0, 2.0, 7.0);
            timber(c, 2.0, 8.0, 20.0, 2.0);
            c.rect(3.0, 5.0, 18.0, 3.0, rgb(0xb69c56));
            for x in (4..21).step_by(3) {
                c.line((x as f32, 6.0), (x as f32 + 1.0, 7.0), 0.4, LIGHT);
            }
            c.oval(6.0, 4.0, 3.0, 1.5, rgb(0xc2ab76));
        }
        Prop::Blanket => {
            c.rect(1.0, 1.0, 10.0, 6.0, rgb(0x6e4846));
            for x in [3.0, 8.0] {
                c.detail(x, 1.0, 2.0, 6.0, rgb(0xad8b63));
            }
            for y in [2.0, 5.0] {
                c.detail(1.0, y, 10.0, 1.0, rgb(0x3b454a));
            }
            c.detail(1.0, 1.0, 1.0, 6.0, PARCHMENT);
        }
        Prop::RuneStone | Prop::CrackedStone | Prop::ReceivingStone => {
            c.cut(
                pts(&[
                    (4.0, 40.0),
                    (6.0, 8.0),
                    (12.0, 2.0),
                    (21.0, 3.0),
                    (25.0, 10.0),
                    (27.0, 40.0),
                ]),
                if prop == Prop::CrackedStone {
                    STONE_DARK
                } else {
                    STONE
                },
            );
            c.paint(
                pts(&[
                    (6.0, 37.0),
                    (8.0, 10.0),
                    (13.0, 5.0),
                    (15.0, 6.0),
                    (10.0, 13.0),
                    (9.0, 37.0),
                ]),
                STONE_LIGHT,
            );
            c.paint(
                pts(&[(22.0, 7.0), (25.0, 12.0), (26.0, 38.0), (21.0, 37.0)]),
                STONE_DARK,
            );
            // Recessed rune panel, chipped border and fine tool marks on the stone.
            c.line((11.0, 8.0), (10.0, 34.0), 0.5, STONE_DARK);
            c.line((10.0, 34.0), (19.0, 36.0), 0.5, STONE_DARK);
            c.line((12.0, 8.0), (19.0, 7.0), 0.5, STONE_LIGHT);
            for (x, y) in [(8.0, 16.0), (21.0, 18.0), (9.0, 27.0), (21.0, 31.0)] {
                c.line((x, y), (x + 1.0, y - 2.0), 0.45, STONE_DARK);
            }
            c.paint(
                pts(&[
                    (5.0, 36.0),
                    (8.0, 35.0),
                    (10.0, 38.0),
                    (7.0, 40.0),
                    (4.0, 39.0),
                ]),
                rgb(0x52604a),
            );
            c.rect(2.0, 39.0, 26.0, 3.0, STONE_DARK);
            let glow = match prop {
                Prop::ReceivingStone => {
                    if frame.is_multiple_of(2) {
                        LIGHT
                    } else {
                        GOLD
                    }
                }
                Prop::CrackedStone => INK,
                _ => rgb(0x8ad8dc),
            };
            rune(c, 15.0, 10.0, glow);
            c.line((14.0, 20.0), (14.0, 25.0), 0.6, glow);
            c.line((12.0, 21.0), (16.0, 24.0), 0.6, glow);
            c.line((12.0, 24.0), (16.0, 21.0), 0.6, glow);
            c.line((16.0, 29.0), (13.0, 32.0), 0.6, glow);
            c.line((13.0, 32.0), (16.0, 34.0), 0.6, glow);
            if prop == Prop::CrackedStone {
                c.line((22.0, 6.0), (16.0, 17.0), 0.6, INK);
                c.line((16.0, 17.0), (20.0, 23.0), 0.6, INK);
                c.line((20.0, 23.0), (12.0, 34.0), 0.6, INK);
            }
        }
        Prop::Basin | Prop::ScryingBasin => {
            c.rect(8.0, 12.0, 12.0, 8.0, STONE);
            c.detail(17.0, 13.0, 3.0, 7.0, STONE_DARK);
            c.rect(5.0, 20.0, 18.0, 3.0, STONE_LIGHT);
            c.cut(
                blob(
                    &pts(&[
                        (2.0, 8.0),
                        (26.0, 8.0),
                        (23.0, 15.0),
                        (14.0, 17.0),
                        (5.0, 15.0),
                    ]),
                    4,
                ),
                STONE,
            );
            c.oval(14.0, 7.0, 12.0, 4.0, STONE_LIGHT);
            c.oval(
                14.0,
                7.0,
                9.0,
                2.5,
                if prop == Prop::ScryingBasin {
                    BLUE
                } else {
                    rgb(0x314951)
                },
            );
            if prop == Prop::ScryingBasin {
                c.line((9.0, 7.0), (16.0, 6.0), 0.6, LIGHT);
                for x in [8.0, 19.0] {
                    rune(c, x, 1.0 + (frame % 2) as f32, BLUE);
                }
            }
            rune(c, 13.0, 12.0, GOLD_DARK);
        }
        Prop::Tablets => {
            for (x, y) in [(2.0, 17.0), (1.0, 11.0), (3.0, 5.0)] {
                c.cut(
                    pts(&[
                        (x, y + 2.0),
                        (x + 2.0, y),
                        (x + 11.0, y),
                        (x + 12.0, y + 7.0),
                        (x, y + 8.0),
                    ]),
                    STONE,
                );
                c.detail(x + 1.0, y + 6.0, 10.0, 1.0, STONE_DARK);
                rune(c, x + 5.0, y + 1.0, GOLD);
            }
        }
        Prop::Bubble(mark) => {
            c.cut(
                blob(
                    &pts(&[
                        (2.0, 2.0),
                        (13.0, 2.0),
                        (14.0, 6.0),
                        (13.0, 11.0),
                        (8.0, 12.0),
                        (5.0, 14.0),
                        (5.0, 11.0),
                        (1.0, 10.0),
                    ]),
                    3,
                ),
                PARCHMENT,
            );
            match mark {
                0 => {
                    c.rect(7.0, 3.0, 2.0, 5.0, rgb(0xc2612b));
                    c.rect(7.0, 10.0, 2.0, 1.0, rgb(0xc2612b));
                }
                1 => {
                    z(c, 3.0, 4.0, 4.0, rgb(0x497690));
                    z(c, 9.0, 6.0, 3.0, rgb(0x497690));
                }
                2 => {
                    // A comic puff of anger, deliberately a different silhouette from ×.
                    c.cut(
                        blob(
                            &pts(&[
                                (3.0, 7.0),
                                (4.0, 4.0),
                                (6.0, 4.0),
                                (8.0, 3.0),
                                (10.0, 4.0),
                                (12.0, 6.0),
                                (11.0, 8.0),
                                (9.0, 9.0),
                                (7.0, 8.0),
                                (5.0, 10.0),
                                (5.0, 8.0),
                            ]),
                            3,
                        ),
                        RED,
                    );
                    c.line((5.0, 5.0), (7.0, 6.0), 0.5, LIGHT);
                }
                3 => {
                    c.line((5.0, 4.0), (10.0, 9.0), 1.0, RED);
                    c.line((10.0, 4.0), (5.0, 9.0), 1.0, RED);
                }
                _ => {
                    gem(c, 7.0, 7.0, 3.0, GOLD);
                    c.line((11.0, 3.0), (11.0, 6.0), 0.5, GOLD);
                    c.line((10.0, 4.0), (13.0, 4.0), 0.5, GOLD);
                }
            }
        }
        Prop::Snore => z(c, 1.0, 1.0, 5.0, rgb(0x497690)),
        Prop::Pebble => {
            c.cut(
                pts(&[(1.0, 2.0), (2.0, 1.0), (4.0, 2.0), (3.0, 4.0), (1.0, 3.0)]),
                STONE,
            );
            c.detail(2.0, 1.0, 1.0, 1.0, STONE_LIGHT);
        }
    }
}
fn z(c: &mut Canvas, x: f32, y: f32, s: f32, color: Color32) {
    c.line((x, y), (x + s, y), 0.6, color);
    c.line((x + s, y), (x, y + s), 0.6, color);
    c.line((x, y + s), (x + s, y + s), 0.6, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn props() -> Vec<Prop> {
        let mut props = vec![
            Prop::Peaks,
            Prop::Entrance,
            Prop::PitheadWinch,
            Prop::Spoil,
            Prop::CleanupHammer,
            Prop::Bookcase,
            Prop::ArchiveCabinet,
            Prop::Lectern,
            Prop::Soot,
            Prop::Portal,
            Prop::Scroll,
            Prop::Support,
            Prop::Shoring,
            Prop::Torch,
            Prop::MineCart,
            Prop::CartYoke,
            Prop::ToolRack,
            Prop::Throne,
            Prop::Banner,
            Prop::Brazier,
            Prop::Chest,
            Prop::Bunk,
            Prop::Blanket,
            Prop::RuneStone,
            Prop::CrackedStone,
            Prop::ReceivingStone,
            Prop::Basin,
            Prop::ScryingBasin,
            Prop::Tablets,
            Prop::Cave(0),
            Prop::Cave(3),
            Prop::Masonry,
            Prop::Snore,
            Prop::Pebble,
        ];
        props.extend((0..3).flat_map(|health| (0..4).map(move |seed| Prop::Rubble(health, seed))));
        props.extend(Deco::ALL.map(Prop::Nature));
        props.extend((0..3).map(Prop::Treasure));
        props.extend((0..5).map(Prop::Bubble));
        props
    }

    #[test]
    fn portal_veil_moves_without_mail_and_freezes_for_reduced_motion() {
        let image = |time, flare, still| {
            render(
                Prop::Portal,
                Prop::Portal.size(),
                portal_frame(time, flare, still),
                Outline::Fine,
            )
        };
        let resting = image(0.0, 0, false);
        assert_ne!(resting, image(0.5, 0, false));
        assert_eq!(resting, image(2.0, 0, false), "seamless two-second loop");
        for phase in 0..PORTAL_PHASES {
            let time = phase as f64 / 4.0;
            let moving = image(time, 0, false);
            assert_eq!(image(time, 0, true), resting);
            assert_eq!(image(time, 3, true), resting);
            assert_ne!(moving, image(time, 3, false), "message flare");
            for y in 0..40 {
                for x in 0..32 {
                    if !(10..=22).contains(&x) || !(11..=33).contains(&y) {
                        assert_eq!(moving[(x, y)], resting[(x, y)], "stone must not move");
                    }
                }
            }
        }
        for palette in pixel::bundled() {
            let mut a = resting.clone();
            let mut b = image(0.5, 0, false);
            let mut mapper = Mapper::new(&palette);
            mapper.apply(&mut a);
            mapper.apply(&mut b);
            assert_ne!(a, b, "{} must retain visible movement", palette.name);
        }
    }

    #[test]
    fn every_prop_builds_at_farm_sizes_in_every_palette() {
        for prop in props() {
            for scale in [0.75, 1.0, 2.0] {
                let size = prop.size().map(|n| (n as f32 * scale).round() as usize);
                for outline in Outline::ALL {
                    for frame in 0..=if prop == Prop::Portal {
                        4 * PORTAL_PHASES - 1
                    } else {
                        prop.frame(0.5)
                    } {
                        let image = render(prop, size, frame, outline);
                        assert_eq!(image.size, size);
                        assert!(
                            image.pixels.iter().any(|p| p.a() == 255),
                            "{prop:?} {size:?}"
                        );
                        assert!(image.pixels.iter().all(|p| p.a() == 0 || p.a() == 255));
                        for palette in pixel::bundled() {
                            let mut mapped = image.clone();
                            Mapper::new(&palette).apply(&mut mapped);
                            assert!(
                                mapped
                                    .pixels
                                    .iter()
                                    .filter(|p| p.a() > 0)
                                    .all(|p| palette.colors.contains(p)),
                                "{prop:?} {}",
                                palette.name
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn only_portal_fire_work_receipts_and_travelling_cart_wheels_animate() {
        for prop in props() {
            let active = matches!(
                prop,
                Prop::MineCart
                    | Prop::Portal
                    | Prop::Entrance
                    | Prop::Torch
                    | Prop::Brazier
                    | Prop::ReceivingStone
                    | Prop::ScryingBasin
            );
            assert_eq!(prop.frame(0.0) != prop.frame(0.5), active, "{prop:?}");
            assert_eq!(
                render(prop, prop.size(), 0, Outline::Fine)
                    != render(prop, prop.size(), 3, Outline::Fine),
                active,
                "{prop:?}"
            );
        }
    }

    #[test]
    fn small_palettes_keep_ink_walls_and_rock_separate() {
        let custom = crate::ui::dwarf::palettes::Imported::from_json(
            "ice-cream-gb",
            include_str!("../../../docs/assets/tests/custom-palettes/ice-cream-gb.json"),
        )
        .unwrap()
        .selection();
        for name in pixel::BUNDLED
            .iter()
            .map(|(name, _)| *name)
            .chain([custom.as_str()])
        {
            let mut world = WorldArt::default();
            world.begin_frame(name, Outline::Fine);
            let mut ink = ColorImage::filled([1, 1], INK);
            let mut wall = ColorImage::filled([1, 1], super::super::excavation::WALL);
            let mut rock = ColorImage::filled([1, 1], rgb(0x6b6a61));
            world.map(&mut ink);
            world.map_stone(&mut wall, true);
            world.map_stone(&mut rock, false);
            assert_ne!(wall.pixels[0], ink.pixels[0], "{name}: silhouette");
            assert_ne!(wall.pixels[0], rock.pixels[0], "{name}: excavated wall");
        }
    }

    #[test]
    fn cave_corners_stay_open_and_ink_survives_palette_mapping() {
        for (name, _) in pixel::BUNDLED {
            let mut world = WorldArt::default();
            world.begin_frame(name, Outline::Fine);
            for size in [[48, 36], [96, 64], [150, 42]] {
                let mut image = render(Prop::Cave(2), size, 0, Outline::Fine);
                let before = image.clone();
                world.map_stone(&mut image, true);
                assert_eq!(image[(0, 0)].a(), 0, "{name}");
                assert_eq!(image[(size[0] / 2, size[1] / 2)].a(), 255);
                for (a, b) in image.pixels.iter().zip(before.pixels) {
                    assert_eq!(a.a(), b.a(), "{name}: alpha");
                }
                let mut ink = ColorImage::filled([1, 1], INK);
                world.map(&mut ink);
                assert!(image.pixels.contains(&ink.pixels[0]), "{name}: outline");
                assert_ne!(
                    image[(size[0] / 2, size[1] / 2)],
                    ink.pixels[0],
                    "{name}: wall"
                );
            }
        }
    }

    #[test]
    fn rotated_hammer_shares_the_palette_cache_with_ledger_and_rubble() {
        let ctx = egui::Context::default();
        let mut world = WorldArt::default();
        for name in ["endesga-32", "oil-6"] {
            world.begin_frame(name, Outline::Bold);
            let mut output = ctx.run_ui(Default::default(), |ui| {
                for prop in [Prop::CleanupHammer, Prop::Bookcase, Prop::Rubble(0, 2)] {
                    let [w, h] = prop.size().map(|n| n as f32);
                    let rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(w, h));
                    world.paint_turned(ui.painter(), prop, rect, 1.0, (0.7, rect.center()), 1.0);
                    world.paint(ui.painter(), prop, rect, 1.0, 0, 1.0);
                }
            });
            assert_eq!(world.textures.len(), 3, "rotation reuses the same texture");
            let palette = pixel::Palette::from_lospec(
                pixel::BUNDLED.iter().find(|(n, _)| *n == name).unwrap().1,
            )
            .unwrap();
            for cached in world.textures.values() {
                let deltas = &output.textures_delta.set[&cached.texture.id()];
                for delta in deltas {
                    let egui::ImageData::Color(image) = &delta.image;
                    assert!(
                        image
                            .pixels
                            .iter()
                            .filter(|p| p.a() > 0)
                            .all(|p| palette.colors.contains(p))
                    );
                    assert_eq!(delta.options, TextureOptions::NEAREST);
                }
            }
            output.textures_delta.clear();
        }
    }

    #[test]
    fn outline_switch_rebuilds_stationary_props() {
        let ctx = egui::Context::default();
        let mut world = WorldArt::default();
        let mut previous = None;
        for outline in Outline::ALL {
            world.begin_frame("oil-6", outline);
            let mut output = ctx.run_ui(Default::default(), |ui| {
                world.paint(
                    ui.painter(),
                    Prop::Portal,
                    Rect::from_min_size(pos2(10.0, 10.0), vec2(32.0, 40.0)),
                    1.0,
                    0,
                    1.0,
                );
            });
            let texture = world.textures.values().next().unwrap().texture.id();
            assert_ne!(Some(texture), previous);
            assert_eq!(world.textures.len(), 1);
            assert!(
                output
                    .textures_delta
                    .set
                    .iter()
                    .any(|(id, _)| *id == texture)
            );
            output.textures_delta.clear();
            previous = Some(texture);
        }
        assert_ne!(
            render(Prop::Portal, [32, 40], 0, Outline::Fine),
            render(Prop::Portal, [32, 40], 0, Outline::Bold)
        );
    }

    #[test]
    fn palette_switch_rebuilds_cached_props_and_tiles() {
        let ctx = egui::Context::default();
        let mut sprites = super::super::sprites::Sprites::load(&ctx);
        for name in ["endesga-32", "oil-6", ""] {
            sprites.begin_frame(
                &super::super::art::ArtSettings {
                    palette: name.into(),
                    ..Default::default()
                },
                2.0,
                0.0,
            );
            let mut output = ctx.run_ui(Default::default(), |ui| {
                let painter = ui.ctx().layer_painter(egui::LayerId::background());
                sprites.standing(&painter, Prop::Portal, pos2(50.0, 70.0), 2.0, 1.0);
            });
            output.textures_delta.clear();
            let world = sprites.world.borrow();
            assert!(world.textures.keys().all(|k| k.palette == name));
            assert_eq!(world.textures.len(), 1);
        }
    }
}
