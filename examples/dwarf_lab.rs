//! A workbench for the code-drawn dwarves (`myrstack::ui::dwarf`): one dwarf to dress and pose
//! on the left, a crowd of seeded dwarves at work on the right. Pixel mode renders them small
//! and maps them to a palette (the bundled Lospec palettes, `pixel::BUNDLED`); the palette view shows
//! the dressed dwarf in every palette at once.
//!
//! `cargo run --release --example dwarf_lab [--shot out.png] [--action mine|study|walk|still]
//! [--t SECS] [--paper] [--pixel HEIGHT] [--palette NAME] [--palettes]`: `--t` freezes time
//! (for comparable screenshots); `--paper` starts with the paper finish; `--pixel` starts in
//! pixel mode with dwarves HEIGHT pixels tall; `--palette` picks a palette by file name (such
//! as `endesga-32`); `--palettes` opens the palette view; with `--shot`, it saves a screenshot
//! after a few frames and exits.

use std::collections::HashMap;
use std::time::Instant;

use eframe::egui::{self, Color32, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2};
use myrstack::ui::dwarf::cutout::Finish;
use myrstack::ui::dwarf::pixel::{self, Mapper, Palette};
use myrstack::ui::dwarf::{self, Action, Beard, HAIRS, Hat, Look, SKINS, TUNICS, Tool};

const WINDOW: [f32; 2] = [1440.0, 860.0];
const CROWD: usize = 15;
/// Dwarf heights in pixel mode.
const HEIGHTS: [usize; 7] = [16, 24, 32, 40, 48, 64, 96];

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let flag = |f: &str| args.iter().any(|a| a == f);
    let action = match value("--action").as_deref() {
        Some("study") => Action::Study,
        Some("walk") => Action::Walk,
        Some("still") => Action::Still,
        _ => Action::Mine,
    };
    let frozen = value("--t").and_then(|t| t.parse::<f64>().ok());
    let shot = value("--shot");
    let finish = if flag("--paper") {
        Finish::PAPER
    } else {
        Finish::INK
    };
    let pixel_height = value("--pixel").and_then(|h| h.parse::<usize>().ok());
    let palette = value("--palette")
        .and_then(|name| pixel::BUNDLED.iter().position(|(file, _)| *file == name))
        .map_or(0, |i| i + 1);
    let palette_view = flag("--palettes");
    eframe::run_native(
        "dwarf lab",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            ..Default::default()
        },
        Box::new(move |cc| {
            myrstack::ui::theme::install(&cc.egui_ctx);
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            let palettes: Vec<Palette> = pixel::bundled();
            Ok(Box::new(Lab {
                seed: 7,
                look: Look::from_seed(7),
                action,
                facing_right: true,
                finish,
                crowd_mixed: true,
                pixel: Pixels {
                    on: pixel_height.is_some() || palette_view,
                    height: pixel_height.unwrap_or(48),
                    palette,
                    fps: 12,
                    thick_outline: true,
                    blend: false,
                    mappers: palettes.iter().map(Mapper::new).collect(),
                    palettes,
                    textures: HashMap::new(),
                },
                palette_view,
                frozen,
                start: Instant::now(),
                shot,
                frames: 0,
            }))
        }),
    )
}

struct Lab {
    seed: u64,
    look: Look,
    action: Action,
    facing_right: bool,
    finish: Finish,
    /// Each crowd dwarf does its own thing, or all do the lab's action.
    crowd_mixed: bool,
    pixel: Pixels,
    /// Show the dressed dwarf in every palette instead of the stage and crowd.
    palette_view: bool,
    frozen: Option<f64>,
    start: Instant,
    shot: Option<String>,
    frames: u32,
}

/// Pixel mode: dwarves rasterized small, optionally in a palette.
struct Pixels {
    on: bool,
    /// Dwarf height in pixels.
    height: usize,
    /// 0 for no palette, else 1 + an index into `palettes`.
    palette: usize,
    /// Animation frames per second: pixel art reads better choppy.
    fps: u32,
    /// Outline at least a pixel wide, so it survives the small size.
    thick_outline: bool,
    /// Average 4×4 samples per pixel (soft edges between colours) instead of one (hard).
    blend: bool,
    palettes: Vec<Palette>,
    mappers: Vec<Mapper>,
    /// Per slot: the frame and settings it was last drawn for, and its texture.
    textures: HashMap<String, (String, TextureHandle)>,
}

/// One dwarf to draw: who, doing what, where and how big.
struct Sprite<'a> {
    slot: String,
    look: &'a Look,
    action: Action,
    /// Seconds into his animation.
    t: f64,
    facing_right: bool,
    foot: Pos2,
    /// On-screen height in points.
    height: f32,
}

impl Pixels {
    /// The canvas a dwarf `height` pixels tall is drawn on, and where his feet go on it.
    fn canvas(&self, facing_right: bool) -> ([usize; 2], Pos2) {
        let h = self.height as f32;
        let size = [(h * 1.25).ceil() as usize, (h * 1.4).ceil() as usize];
        let x = if facing_right { 0.42 } else { 0.58 };
        (size, pos2(size[0] as f32 * x, size[1] as f32 - h * 0.06))
    }

    /// Draws `sprite` as pixels in `palette` (0 for none), refreshing its texture when the
    /// frame or the settings changed.
    fn draw(&mut self, painter: &egui::Painter, sprite: &Sprite, finish: &Finish, palette: usize) {
        let fps = self.fps.max(1) as f64;
        let t = (sprite.t * fps).floor() / fps;
        let pose = dwarf::pose(sprite.action, t);
        let (size, foot) = self.canvas(sprite.facing_right);
        let finish = Finish {
            outline: if self.thick_outline {
                finish.outline.max(100.0 / self.height as f32)
            } else {
                finish.outline
            },
            ..*finish
        };
        let key = format!(
            "{t:.3} {} {palette} {:?} {:?} {:?} {} {}",
            self.height, sprite.look, finish, sprite.action, sprite.facing_right, self.blend
        );
        let slot = format!("{} {palette}", sprite.slot);
        let fresh = self.textures.get(&slot).is_some_and(|(k, _)| *k == key);
        if !fresh {
            let drawing = dwarf::draw(
                foot,
                self.height as f32,
                sprite.look,
                &pose,
                sprite.facing_right,
                &finish,
            );
            let region = Rect::from_min_size(Pos2::ZERO, vec2(size[0] as f32, size[1] as f32));
            let samples = if self.blend { 4 } else { 1 };
            let mut image = pixel::rasterize(&drawing.shapes, region, size, samples);
            if palette > 0 {
                self.mappers[palette - 1].apply(&mut image);
            }
            match self.textures.get_mut(&slot) {
                Some((k, tex)) => {
                    tex.set(image, TextureOptions::NEAREST);
                    *k = key;
                }
                None => {
                    let tex = painter
                        .ctx()
                        .load_texture(&slot, image, TextureOptions::NEAREST);
                    self.textures.insert(slot.clone(), (key, tex));
                }
            }
        }
        let tex = &self.textures[&slot].1;
        // Whole screen pixels per art pixel, so every pixel is the same size.
        let ppp = painter.ctx().pixels_per_point();
        let scale = ((sprite.height * ppp / self.height as f32).floor().max(1.0)) / ppp;
        let shown = vec2(size[0] as f32, size[1] as f32) * scale;
        let min = sprite.foot - foot.to_vec2() * scale;
        let min = pos2((min.x * ppp).round() / ppp, (min.y * ppp).round() / ppp);
        painter.image(
            tex.id(),
            Rect::from_min_size(min, shown),
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        let drawing = dwarf::draw(
            foot,
            self.height as f32,
            sprite.look,
            &pose,
            sprite.facing_right,
            &finish,
        );
        if let Some((at, h)) = drawing.question {
            dwarf::question(painter, min + at.to_vec2() * scale, h * scale);
        }
    }
}

impl eframe::App for Lab {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t = self
            .frozen
            .unwrap_or_else(|| self.start.elapsed().as_secs_f64());
        egui::Panel::left("dress")
            .resizable(false)
            .exact_size(290.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui));
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter_at(rect);
                backdrop(&painter, rect);
                if self.palette_view {
                    self.palettes(&painter, rect, t);
                } else {
                    self.stage(&painter, rect, t);
                }
            });
        self.screenshot(ui);
        ui.ctx().request_repaint();
    }
}

impl Lab {
    fn sprite(&mut self, painter: &egui::Painter, sprite: Sprite, palette: usize) {
        if self.pixel.on {
            self.pixel.draw(painter, &sprite, &self.finish, palette);
        } else {
            dwarf::paint(
                painter,
                sprite.foot,
                sprite.height,
                sprite.look,
                &dwarf::pose(sprite.action, sprite.t),
                sprite.facing_right,
                &self.finish,
            );
        }
    }

    /// The dressed dwarf big on the left, a crowd of seeded dwarves on the right.
    fn stage(&mut self, painter: &egui::Painter, rect: Rect, t: f64) {
        let stage = Rect::from_min_size(rect.min, vec2(rect.width() * 0.42, rect.height()));
        let foot = pos2(stage.center().x, stage.bottom() - 120.0);
        ground(painter, stage, foot.y);
        let look = self.look;
        let palette = self.pixel.palette;
        self.sprite(
            painter,
            Sprite {
                slot: "main".into(),
                look: &look,
                action: self.action,
                t,
                facing_right: self.facing_right,
                foot,
                height: stage.height() * 0.6,
            },
            palette,
        );
        let crowd = Rect::from_min_max(pos2(stage.right(), rect.top()), rect.max);
        let (cols, rows) = (5, CROWD.div_ceil(5));
        let cell = vec2(crowd.width() / cols as f32, crowd.height() / rows as f32);
        for i in 0..CROWD {
            let seed = 1000 + i as u64;
            let look = Look::from_seed(seed);
            let action = if self.crowd_mixed {
                Action::ALL[(seed as usize * 7 + 3) % Action::ALL.len()]
            } else {
                self.action
            };
            let (c, r) = (i % cols, i / cols);
            self.sprite(
                painter,
                Sprite {
                    slot: format!("crowd {i}"),
                    look: &look,
                    action,
                    t: t + seed as f64 * 0.37,
                    facing_right: !seed.is_multiple_of(3),
                    foot: Pos2::new(
                        crowd.left() + (c as f32 + 0.5) * cell.x,
                        crowd.top() + (r as f32 + 0.86) * cell.y,
                    ),
                    height: cell.y * 0.68,
                },
                palette,
            );
        }
    }

    /// The dressed dwarf once per palette, and once without one.
    fn palettes(&mut self, painter: &egui::Painter, rect: Rect, t: f64) {
        let n = self.pixel.palettes.len() + 1;
        let cols = 6;
        let rows = n.div_ceil(cols);
        let cell = vec2(rect.width() / cols as f32, rect.height() / rows as f32);
        let look = self.look;
        for palette in 0..n {
            let (c, r) = (palette % cols, palette / cols);
            let min = rect.min + vec2(c as f32 * cell.x, r as f32 * cell.y);
            let name = match palette {
                0 => "No palette".to_string(),
                p => {
                    let pal = &self.pixel.palettes[p - 1];
                    format!("{} ({})", pal.name, pal.colors.len())
                }
            };
            painter.text(
                min + vec2(10.0, 8.0),
                egui::Align2::LEFT_TOP,
                name,
                egui::FontId::proportional(13.0),
                Color32::from_gray(220),
            );
            self.sprite(
                painter,
                Sprite {
                    slot: "palette".into(),
                    look: &look,
                    action: self.action,
                    t,
                    facing_right: self.facing_right,
                    foot: min + vec2(cell.x * 0.45, cell.y * 0.94),
                    height: cell.y * 0.62,
                },
                palette,
            );
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Dwarf");
        ui.horizontal(|ui| {
            ui.label("Seed");
            if ui.add(egui::DragValue::new(&mut self.seed)).changed() {
                self.look = Look::from_seed(self.seed);
            }
            if ui.button("Random").clicked() {
                self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1) % 100_000;
                self.look = Look::from_seed(self.seed);
            }
        });
        ui.separator();
        pick(ui, "Beard", &mut self.look.beard, &Beard::ALL);
        pick(ui, "Hat", &mut self.look.hat, &Hat::ALL);
        pick(ui, "Tool", &mut self.look.tool, &Tool::ALL);
        ui.separator();
        swatches(ui, "Skin", &mut self.look.skin, &SKINS);
        swatches(ui, "Hair", &mut self.look.hair, &HAIRS);
        swatches(ui, "Tunic", &mut self.look.tunic, &TUNICS);
        ui.horizontal(|ui| {
            ui.label("Trousers");
            ui.color_edit_button_srgba(&mut self.look.trousers);
        });
        ui.add(egui::Slider::new(&mut self.look.nose, 0.7..=1.5).text("Nose"));
        ui.add(egui::Slider::new(&mut self.look.girth, 0.85..=1.2).text("Girth"));
        ui.separator();
        ui.heading("Doing");
        ui.horizontal_wrapped(|ui| {
            for a in Action::ALL {
                ui.selectable_value(&mut self.action, a, format!("{a:?}"));
            }
        });
        ui.checkbox(&mut self.facing_right, "Facing right");
        ui.checkbox(&mut self.crowd_mixed, "Crowd does mixed things");
        ui.separator();
        ui.heading("Pixels");
        ui.checkbox(&mut self.pixel.on, "Pixel mode");
        ui.add_enabled_ui(self.pixel.on, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Height");
                for h in HEIGHTS {
                    ui.selectable_value(&mut self.pixel.height, h, format!("{h}"));
                }
            });
            let current = match self.pixel.palette {
                0 => "None".to_string(),
                p => self.pixel.palettes[p - 1].name.clone(),
            };
            egui::ComboBox::from_label("Palette")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.pixel.palette, 0, "None");
                    for (i, p) in self.pixel.palettes.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.pixel.palette,
                            i + 1,
                            format!("{} ({})", p.name, p.colors.len()),
                        );
                    }
                });
            ui.add(egui::Slider::new(&mut self.pixel.fps, 4..=60).text("Frames/s"));
            ui.checkbox(&mut self.pixel.thick_outline, "Outline at least 1 px");
            ui.checkbox(&mut self.pixel.blend, "Blend edges (4×4 samples per pixel)");
        });
        if ui
            .checkbox(&mut self.palette_view, "Every palette at once")
            .changed()
            && self.palette_view
        {
            self.pixel.on = true;
        }
        ui.separator();
        ui.heading("Finish");
        ui.horizontal(|ui| {
            for (name, preset) in [("Ink", Finish::INK), ("Paper", Finish::PAPER)] {
                if ui.selectable_label(self.finish == preset, name).clicked() {
                    self.finish = preset;
                }
            }
        });
        ui.add(egui::Slider::new(&mut self.finish.outline, 0.0..=3.0).text("Outline"));
        ui.horizontal(|ui| {
            ui.label("Outline colour");
            ui.color_edit_button_srgba(&mut self.finish.outline_color);
        });
        let mut shadow = self.finish.shadow_color.a() > 0;
        if ui.checkbox(&mut shadow, "Drop shadow").changed() {
            self.finish.shadow = Finish::PAPER.shadow;
            self.finish.shadow_color = if shadow {
                Finish::PAPER.shadow_color
            } else {
                Color32::TRANSPARENT
            };
        }
        ui.checkbox(&mut self.finish.pins, "Pins at the joints");
    }

    fn screenshot(&mut self, ui: &egui::Ui) {
        let Some(path) = &self.shot else { return };
        self.frames += 1;
        if self.frames == 20 {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            image::save_buffer(path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
                .expect("save screenshot");
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn pick<T: Copy + PartialEq + std::fmt::Debug>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut T,
    all: &[T],
) {
    egui::ComboBox::from_label(label)
        .selected_text(format!("{value:?}"))
        .show_ui(ui, |ui| {
            for v in all {
                ui.selectable_value(value, *v, format!("{v:?}"));
            }
        });
}

fn swatches(ui: &mut egui::Ui, label: &str, value: &mut Color32, all: &[Color32]) {
    ui.horizontal(|ui| {
        ui.label(label);
        for c in all {
            let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::click());
            ui.painter().rect_filled(rect, 3.0, *c);
            if c == value {
                ui.painter().rect_stroke(
                    rect,
                    3.0,
                    egui::Stroke::new(2.0, Color32::WHITE),
                    egui::StrokeKind::Outside,
                );
            }
            if response.clicked() {
                *value = *c;
            }
        }
        ui.color_edit_button_srgba(value);
    });
}

/// Dark rock, lit a little towards the middle.
fn backdrop(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, 0.0, Color32::from_rgb(0x1c, 0x18, 0x16));
    let glow = Color32::from_rgba_unmultiplied(0x6a, 0x40, 0x20, 18);
    for i in 0..6 {
        let r = rect.height() * (0.25 + i as f32 * 0.08);
        painter.circle_filled(rect.center(), r, glow);
    }
}

/// A strip of floor under the big dwarf.
fn ground(painter: &egui::Painter, stage: Rect, y: f32) {
    painter.rect_filled(
        Rect::from_min_max(pos2(stage.left(), y), pos2(stage.right(), y + 14.0)),
        0.0,
        Color32::from_rgb(0x2e, 0x24, 0x1e),
    );
}
