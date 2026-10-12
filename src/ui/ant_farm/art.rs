//! The farm's code-drawn pixel dwarves, optionally in a fixed palette. The
//! `art` section of `settings.json`, edited in the Settings window's Art section.
//!
//! Give every new field a serde default (the struct is `#[serde(default)]`) so older settings
//! files keep loading.

use std::collections::HashMap;

use eframe::egui::{Color32, Painter, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2};
use serde::{Deserialize, Serialize};

use super::sprites::Coat;
use crate::ui::dwarf::cutout::Finish;
use crate::ui::dwarf::pixel::{self, Mapper};
use crate::ui::dwarf::{self, Action, Look};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtSettings {
    pub pixels: Pixels,
    /// A bundled palette's file name ([`pixel::BUNDLED`]); empty for the drawing's own colours.
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
            Pixels::Farm => "Farm pixels".into(),
            Pixels::Height(h) => format!("{h} px"),
        }
    }
}

/// A dwarf's tunic for an agent's coat: Claude amber, Codex slate, failed red; `None` keeps
/// his own.
pub fn livery(coat: Coat) -> Option<Color32> {
    match coat {
        Coat::Plain => None,
        Coat::Claude => Some(Color32::from_rgb(0xd0, 0x8a, 0x3e)),
        Coat::Codex => Some(Color32::from_rgb(0x3a, 0x4c, 0x54)),
        Coat::Failed => Some(Color32::from_rgb(0xa8, 0x2c, 0x26)),
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
    /// Head down (falling out of the fleet).
    pub upside_down: bool,
    pub facing_right: bool,
    pub alpha: f32,
}

/// Draws dwarves for the farm, keeping each pixel dwarf's texture between frames.
pub struct DwarfArt {
    names: Vec<&'static str>,
    mappers: Vec<Mapper>,
    textures: HashMap<String, Cached>,
    frame: u64,
}

struct Cached {
    /// The frame and settings the texture was drawn for.
    drawn: String,
    texture: TextureHandle,
    used: u64,
}

impl Default for DwarfArt {
    fn default() -> Self {
        Self {
            names: pixel::BUNDLED.iter().map(|(name, _)| *name).collect(),
            mappers: pixel::bundled().iter().map(Mapper::new).collect(),
            textures: HashMap::new(),
            frame: 0,
        }
    }
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
        let i = self.names.iter().position(|n| *n == name)?;
        self.mappers.get_mut(i)
    }

    /// Draws `figure` standing on the bottom of `cell` (an ant's cell), `farm_px` points per
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
        let height = cell.height() * 0.95;
        let foot = cell.center_bottom();
        let phase = (dwarf::seed_of(figure.key) % 1000) as f64 * 0.013;
        let pose = |t: f64| {
            let mut pose = dwarf::pose(figure.action, t + phase);
            if figure.upside_down {
                pose.roll = std::f32::consts::PI;
                pose.hop = 45.0;
            }
            pose
        };
        let palette = art.palette.clone();
        let h = match art.pixels {
            Pixels::Height(h) => h.clamp(8, 256) as f32,
            _ => (height / farm_px).round().max(8.0),
        };
        let t = (now_s * PIXEL_FPS).floor() / PIXEL_FPS;
        let pose = pose(t);
        let size = [(h * 1.5).ceil() as usize, (h * 1.45).ceil() as usize];
        let canvas_foot = pos2(size[0] as f32 * 0.5, size[1] as f32 - h * 0.08);
        let finish = Finish {
            outline: Finish::INK.outline.max(100.0 / h),
            ..Finish::INK
        };
        let drawing = dwarf::draw(
            canvas_foot,
            h,
            &figure.look,
            &pose,
            figure.facing_right,
            &finish,
        );
        let drawn = format!(
            "{h} {palette} {:?} {pose:?} {}",
            figure.look, figure.facing_right
        );
        let frame = self.frame;
        let stale = self
            .textures
            .get(figure.key)
            .is_none_or(|c| c.drawn != drawn);
        if stale {
            let region = Rect::from_min_size(Pos2::ZERO, vec2(size[0] as f32, size[1] as f32));
            let mut image = pixel::rasterize(&drawing.shapes, region, size, 1);
            if let Some(mapper) = self.mapper(&palette) {
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
        let scale = height / h;
        let min = foot - canvas_foot.to_vec2() * scale;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Older settings files have no `art` section, or only part of one.
    #[test]
    fn missing_fields_take_defaults() {
        let art: ArtSettings = serde_json::from_str(r#"{"dwarves": true}"#).unwrap();
        assert_eq!(
            art,
            ArtSettings {
                pixels: Pixels::Farm,
                palette: String::new(),
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
    fn every_bundled_palette_has_a_mapper() {
        let mut art = DwarfArt::default();
        for (name, _) in pixel::BUNDLED {
            assert!(art.mapper(name).is_some(), "{name}");
        }
        assert!(art.mapper("").is_none());
    }
}
