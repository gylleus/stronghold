//! Pixel dwarves, their texture cache, and shared world art settings. Palette and ink
//! width apply to the whole mine; fixed pixel heights apply only to dwarves.

use std::collections::HashMap;

use eframe::egui::{Color32, Painter, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2};
use serde::{Deserialize, Serialize};

use super::sprites::Coat;
#[cfg(test)]
use crate::ui::dwarf::cutout::Finish;
use crate::ui::dwarf::cutout::Outline;
use crate::ui::dwarf::pixel::{self, Mapper};
use crate::ui::dwarf::{self, Action, Look};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtSettings {
    pub pixels: Pixels,
    pub outline: Outline,
    /// A bundled slug or self-contained `hex:` selection; empty for the drawing’s own colours.
    pub palette: String,
}

/// The resolution dwarves are rasterized at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pixels {
    /// A dwarf this many pixels tall, whatever the zoom.
    Height(u32),
    /// One pixel per farm texel, matching the rest of the farm.
    #[default]
    #[serde(other)]
    Farm,
}

impl Pixels {
    pub const CHOICES: [Pixels; 6] = [
        Pixels::Farm,
        Pixels::Height(16),
        Pixels::Height(24),
        Pixels::Height(32),
        Pixels::Height(48),
        Pixels::Height(64),
    ];

    pub fn label(self) -> String {
        match self {
            Pixels::Farm => "Hold pixels".into(),
            Pixels::Height(h) => format!("{h} px"),
        }
    }
}

/// Guild cloth, painted helmets and a shaped crest keep agent identity visible at farm
/// size. The librarian keeps its ink robes; failed dwarves retain their guild.
pub fn dress(look: &mut Look, coat: Coat) {
    if look.role == dwarf::Role::Librarian {
        return;
    }
    look.crest = match coat {
        Coat::Plain => dwarf::Crest::None,
        Coat::Claude => dwarf::Crest::Sun,
        Coat::Codex => dwarf::Crest::Diamond,
    };
    if let Some(color) = look.crest.color() {
        look.tunic = color;
    }
}

/// Pixel dwarves animate at this rate: choppy reads as pixel art.
const PIXEL_FPS: f64 = 12.0;

/// One dwarf to draw this frame.
#[derive(Debug, Clone, Copy)]
pub struct Figure<'a> {
    /// Stable per agent: picks the cached texture and the animation's phase.
    pub key: &'a str,
    pub look: Look,
    pub action: Action,
    /// Head down (falling out of the clan).
    pub upside_down: bool,
    pub facing_right: bool,
    pub alpha: f32,
}

/// A supplied pose can be split around a separately painted prop, using the same
/// rasterizer, pixel scale and texture cache as the other dwarves.
pub(super) struct PosedFigure<'a> {
    pub key: &'a str,
    pub look: &'a Look,
    pub pose: &'a dwarf::Pose,
    pub foot: Pos2,
    pub height: f32,
    pub facing_right: bool,
    pub alpha: f32,
    pub layer: DwarfLayer,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum DwarfLayer {
    Body,
    Hands,
}

/// Draws dwarves for the farm, keeping each pixel dwarf's texture between frames.
#[derive(Default)]
pub struct DwarfArt {
    palette: String,
    mapper: Option<Mapper>,
    textures: HashMap<String, Cached>,
    frame: u64,
}

struct Cached {
    /// The frame and settings the texture was drawn for.
    drawn: String,
    texture: TextureHandle,
    used: u64,
}

impl std::fmt::Debug for DwarfArt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DwarfArt")
            .field("textures", &self.textures.len())
            .finish()
    }
}

impl DwarfArt {
    /// Call once per frame before drawing: drops the textures of dwarves no longer drawn.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        let frame = self.frame;
        self.textures.retain(|_, c| c.used + 2 >= frame);
    }

    fn mapper(&mut self, name: &str) -> Option<&mut Mapper> {
        if self.palette != name {
            self.palette = name.to_owned();
            self.mapper = Mapper::selection(name);
        }
        self.mapper.as_mut()
    }

    /// Draws `figure` standing on the bottom of `cell` (a dwarf's cell), `farm_px` points per
    /// farm texel, at `now_s`.
    pub fn paint(
        &mut self,
        painter: &Painter,
        art: &ArtSettings,
        figure: &Figure,
        cell: Rect,
        farm_px: f32,
        now_s: f64,
    ) {
        let phase = (dwarf::seed_of(figure.key) % 1000) as f64 * 0.013;
        let pose = |t: f64| {
            let mut pose = dwarf::pose(figure.action, t + phase);
            if figure.upside_down {
                pose.roll = std::f32::consts::PI;
                pose.hop = 45.0;
            }
            pose
        };
        let t = (now_s * PIXEL_FPS).floor() / PIXEL_FPS;
        self.paint_posed(
            painter,
            art,
            &PosedFigure {
                key: figure.key,
                look: &figure.look,
                pose: &pose(t),
                foot: cell.center_bottom(),
                height: cell.height() * 0.95,
                facing_right: figure.facing_right,
                alpha: figure.alpha,
                layer: DwarfLayer::Body,
            },
            farm_px,
        );
    }

    pub(super) fn paint_posed(
        &mut self,
        painter: &Painter,
        art: &ArtSettings,
        figure: &PosedFigure,
        farm_px: f32,
    ) {
        let palette = &art.palette;
        let h = match art.pixels {
            Pixels::Height(h) => h.clamp(8, 256) as f32,
            Pixels::Farm => (figure.height / farm_px).round().max(8.0),
        };
        let size = [(h * 1.5).ceil() as usize, (h * 1.45).ceil() as usize];
        let canvas_foot = pos2(size[0] as f32 * 0.5, size[1] as f32 - h * 0.08);
        let finish = art.outline.finish(100.0 / (h * figure.look.role.scale()));
        let pose = figure.pose;
        let drawing = match figure.layer {
            DwarfLayer::Body => dwarf::draw(
                canvas_foot,
                h,
                figure.look,
                pose,
                figure.facing_right,
                &finish,
            ),
            DwarfLayer::Hands => dwarf::Drawing {
                shapes: dwarf::held_prop(canvas_foot, h, figure.look, pose, &finish).1,
                question: None,
            },
        };
        let drawn = format!(
            "{h} {palette} {:?} {pose:?} {} {:?} {:?}",
            figure.look, figure.facing_right, art.outline, figure.layer
        );
        let frame = self.frame;
        let stale = self
            .textures
            .get(figure.key)
            .is_none_or(|c| c.drawn != drawn);
        if stale {
            let region = Rect::from_min_size(Pos2::ZERO, vec2(size[0] as f32, size[1] as f32));
            let mut image = pixel::rasterize(&drawing.shapes, region, size, 1);
            if let Some(mapper) = self.mapper(palette) {
                mapper.apply(&mut image);
            }
            match self.textures.get_mut(figure.key) {
                Some(c) => {
                    c.texture.set(image, TextureOptions::NEAREST);
                    c.drawn = drawn;
                }
                None => {
                    let texture = painter.ctx().load_texture(
                        format!("dwarf/{}", figure.key),
                        image,
                        TextureOptions::NEAREST,
                    );
                    self.textures.insert(
                        figure.key.to_string(),
                        Cached {
                            drawn,
                            texture,
                            used: frame,
                        },
                    );
                }
            }
        }
        let Some(cached) = self.textures.get_mut(figure.key) else {
            return;
        };
        cached.used = frame;
        let scale = figure.height / h;
        let min = figure.foot - canvas_foot.to_vec2() * scale;
        painter.image(
            cached.texture.id(),
            Rect::from_min_size(min, vec2(size[0] as f32, size[1] as f32) * scale),
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE.gamma_multiply(figure.alpha),
        );
        if let Some((at, q)) = drawing.question {
            dwarf::question(painter, min + at.to_vec2() * scale, q * scale);
        }
    }
}

/// A small live sample using the same dwarf, stone, props and palette mapping as the hold.
#[derive(Default)]
pub struct Preview {
    dwarves: DwarfArt,
    world: super::world::WorldArt,
}

impl Preview {
    pub fn show(&mut self, ui: &mut eframe::egui::Ui, art: &ArtSettings) {
        use super::world::{Prop, rgb};
        use eframe::egui::{Align2, FontId, Sense};
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width().min(520.0), 156.0), Sense::hover());
        let painter = ui.painter_at(rect);
        let start = ui
            .ctx()
            .graphics(|g| g.get(painter.layer_id()).map(|l| l.next_idx()))
            .unwrap_or(eframe::egui::layers::ShapeIdx(0));
        self.world.begin_frame(&art.palette, art.outline);
        self.dwarves.begin_frame();
        painter.rect_filled(rect, 4.0, rgb(0x22242e));
        let cave =
            Rect::from_min_size(rect.min + vec2(8.0, 20.0), vec2(rect.width() - 16.0, 112.0));
        self.world.paint(&painter, Prop::Cave(0), cave, 2.0, 0, 1.0);
        let now = ui.input(|i| i.time);
        for (i, (look, action)) in [
            (Look::king(), Action::Still),
            (Look::for_role(dwarf::Role::Miner, 7), Action::Mine),
        ]
        .into_iter()
        .enumerate()
        {
            self.dwarves.paint(
                &painter,
                art,
                &Figure {
                    key: if i == 0 {
                        "preview-king"
                    } else {
                        "preview-miner"
                    },
                    look,
                    action,
                    upside_down: false,
                    facing_right: true,
                    alpha: 1.0,
                },
                Rect::from_min_size(
                    rect.min + vec2(50.0 + i as f32 * 90.0, 42.0),
                    vec2(70.0, 80.0),
                ),
                2.0,
                now,
            );
        }
        let chest = Rect::from_min_size(rect.min + vec2(260.0, 82.0), vec2(48.0, 40.0));
        self.world.paint(&painter, Prop::Chest, chest, 2.0, 0, 1.0);
        let plate = Rect::from_min_size(rect.min + vec2(10.0, 132.0), vec2(210.0, 21.0));
        painter.rect_filled(plate, 0.0, rgb(0x1a1008));
        painter.text(
            plate.center(),
            Align2::CENTER_CENTER,
            "The hold · stone, ink & gold",
            FontId::proportional(13.0),
            rgb(0xf3e9d2),
        );
        self.world.map_scene(ui.ctx(), painter.layer_id(), start);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_crests_remain_distinct_at_small_sizes_in_every_palette() {
        // Exercise the real rasterizer, including asleep/failed poses and the
        // apron/vests that obscure tunic colours. Tiny palettes may collapse the hues.
        for height in [16.0, 24.0] {
            for role in [
                dwarf::Role::Miner,
                dwarf::Role::Foreman,
                dwarf::Role::Apprentice,
            ] {
                for action in [
                    Action::Still,
                    Action::Mine,
                    Action::Sleep,
                    Action::KnockedOut,
                ] {
                    let image = |coat| {
                        let mut look = Look::for_role(role, 7);
                        dress(&mut look, coat);
                        let size = [(height * 1.5) as usize, (height * 1.5) as usize];
                        let finish = Finish {
                            outline: 100.0 / height,
                            ..Finish::INK
                        };
                        let drawing = dwarf::draw(
                            pos2(height * 0.7, height * 1.4),
                            height,
                            &look,
                            &dwarf::pose(action, 0.3),
                            true,
                            &finish,
                        );
                        pixel::rasterize(
                            &drawing.shapes,
                            Rect::from_min_size(Pos2::ZERO, vec2(size[0] as f32, size[1] as f32)),
                            size,
                            1,
                        )
                    };
                    let claude = image(Coat::Claude);
                    let codex = image(Coat::Codex);
                    assert_ne!(claude, codex, "{height} {role:?} {action:?}");
                    for palette in pixel::bundled() {
                        let mut a = claude.clone();
                        let mut b = codex.clone();
                        let mut mapper = Mapper::new(&palette);
                        mapper.apply(&mut a);
                        mapper.apply(&mut b);
                        assert_ne!(a, b, "{} {height} {role:?} {action:?}", palette.name);
                    }
                }
            }
        }
    }

    /// Older settings files have no `art` section, or only part of one.
    #[test]
    fn missing_fields_take_defaults() {
        let art: ArtSettings = serde_json::from_str(r#"{"dwarves": true}"#).unwrap();
        assert_eq!(
            art,
            ArtSettings {
                pixels: Pixels::Farm,
                palette: String::new(),
                outline: Outline::Fine,
            }
        );
    }

    #[test]
    fn removed_settings_are_ignored_and_unknown_pixels_use_farm() {
        let settings: crate::whip::settings::Settings = serde_json::from_str(
            r#"{"art":{"dwarves":false,"pixels":"off","palette":"apollo","paper":true},"unused":42}"#,
        ).unwrap();
        assert_eq!(settings.art.pixels, Pixels::Farm);
        assert_eq!(settings.art.palette, "apollo");
        let saved = serde_json::to_value(&settings.art).unwrap();
        assert!(saved.get("dwarves").is_none());
        assert!(saved.get("paper").is_none());
        let future: ArtSettings = serde_json::from_str(r#"{"pixels":"unknown"}"#).unwrap();
        assert_eq!(future.pixels, Pixels::Farm);
    }

    #[test]
    fn pixel_heights_round_trip() {
        for pixels in Pixels::CHOICES {
            let art = ArtSettings {
                pixels,
                ..Default::default()
            };
            let json = serde_json::to_string(&art).unwrap();
            assert_eq!(serde_json::from_str::<ArtSettings>(&json).unwrap(), art);
        }
    }

    #[test]
    fn outline_choices_persist_and_change_dwarf_pixels() {
        let mut previous = None;
        for outline in Outline::ALL {
            let art = ArtSettings {
                outline,
                ..Default::default()
            };
            let json = serde_json::to_string(&art).unwrap();
            assert_eq!(serde_json::from_str::<ArtSettings>(&json).unwrap(), art);
            let d = dwarf::draw(
                pos2(40.0, 68.0),
                48.0,
                &Look::king(),
                &dwarf::pose(Action::Still, 0.0),
                true,
                &outline.finish(100.0 / 48.0),
            );
            let image = pixel::rasterize(
                &d.shapes,
                Rect::from_min_size(Pos2::ZERO, vec2(80.0, 80.0)),
                [80, 80],
                1,
            );
            assert!(image.pixels.iter().all(|c| c.a() == 0 || c.a() == 255));
            let ink = image
                .pixels
                .iter()
                .filter(|c| **c == Finish::INK.outline_color)
                .count();
            if let Some(before) = previous {
                assert!(ink > before, "{outline:?}");
            }
            previous = Some(ink);
        }
    }

    #[test]
    fn changing_outline_refreshes_a_stationary_dwarf() {
        let ctx = eframe::egui::Context::default();
        let mut renderer = DwarfArt::default();
        let figure = Figure {
            key: "king",
            look: Look::king(),
            action: Action::Still,
            upside_down: false,
            facing_right: true,
            alpha: 1.0,
        };
        for outline in Outline::ALL {
            renderer.begin_frame();
            let mut output = ctx.run_ui(Default::default(), |ui| {
                renderer.paint(
                    ui.painter(),
                    &ArtSettings {
                        outline,
                        ..Default::default()
                    },
                    &figure,
                    Rect::from_min_size(pos2(50.0, 50.0), vec2(48.0, 48.0)),
                    1.0,
                    0.0,
                );
            });
            let texture = renderer.textures["king"].texture.id();
            assert!(
                output
                    .textures_delta
                    .set
                    .iter()
                    .any(|(id, _)| *id == texture),
                "{outline:?}"
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn every_bundled_palette_has_a_mapper() {
        let mut art = DwarfArt::default();
        for (name, _) in pixel::BUNDLED {
            assert!(art.mapper(name).is_some(), "{name}");
        }
        assert!(art.mapper("").is_none());
    }
    #[test]
    fn imported_palette_switch_rebuilds_stationary_dwarf_texture() {
        let ctx = eframe::egui::Context::default();
        let mut renderer = DwarfArt::default();
        let figure = Figure {
            key: "king",
            look: Look::king(),
            action: Action::Still,
            upside_down: false,
            facing_right: true,
            alpha: 1.0,
        };
        let mut previous = None;
        for selection in [
            "hex:7c3f58-eb6b6f-f9a875-fff6d3",
            "hex:264653-2a9d8f-e9c46a",
            "oil-6",
        ] {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                renderer.paint(
                    ui.painter(),
                    &ArtSettings {
                        palette: selection.into(),
                        ..Default::default()
                    },
                    &figure,
                    Rect::from_min_size(pos2(50.0, 50.0), vec2(48.0, 48.0)),
                    1.0,
                    0.0,
                );
            });
            let texture = renderer.textures["king"].texture.id();
            let egui_image = &output.textures_delta.set[&texture][0].image;
            if let Some(before) = previous {
                assert!(
                    egui_image != &before,
                    "palette change must update the texture pixels"
                );
            }
            previous = Some(egui_image.clone());
            output.textures_delta.clear();
        }
    }
}
