//! A workbench for the code-drawn paper-cutout dwarves (`myrstack::ui::dwarf`): one dwarf to
//! dress and pose on the left, a crowd of seeded dwarves at work on the right.
//!
//! `cargo run --example dwarf_lab [--shot out.png] [--action mine|study|walk|still] [--t SECS]`:
//! `--t` freezes time (for comparable screenshots); `--paper` starts with the paper finish; with
//! `--shot`, it saves a screenshot after a few frames and exits.

use std::time::Instant;

use eframe::egui::{self, Color32, Pos2, Rect, pos2, vec2};
use myrstack::ui::dwarf::cutout::Finish;
use myrstack::ui::dwarf::{self, Action, Beard, HAIRS, Hat, Look, SKINS, TUNICS, Tool};

const WINDOW: [f32; 2] = [1440.0, 860.0];
const CROWD: usize = 15;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let action = match value("--action").as_deref() {
        Some("study") => Action::Study,
        Some("walk") => Action::Walk,
        Some("still") => Action::Still,
        _ => Action::Mine,
    };
    let frozen = value("--t").and_then(|t| t.parse::<f64>().ok());
    let shot = value("--shot");
    let finish = if args.iter().any(|a| a == "--paper") {
        Finish::PAPER
    } else {
        Finish::INK
    };
    eframe::run_native(
        "dwarf lab",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size(WINDOW),
            ..Default::default()
        },
        Box::new(move |cc| {
            myrstack::ui::theme::install(&cc.egui_ctx);
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(Lab {
                seed: 7,
                look: Look::from_seed(7),
                action,
                facing_right: true,
                finish,
                crowd_mixed: true,
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
    frozen: Option<f64>,
    start: Instant,
    shot: Option<String>,
    frames: u32,
}

impl eframe::App for Lab {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t = self
            .frozen
            .unwrap_or_else(|| self.start.elapsed().as_secs_f64());
        egui::Panel::left("dress")
            .resizable(false)
            .exact_size(280.0)
            .show(ui, |ui| self.controls(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter_at(rect);
                backdrop(&painter, rect);
                let finish = self.finish;
                // The dressed dwarf, big.
                let stage = Rect::from_min_size(rect.min, vec2(rect.width() * 0.42, rect.height()));
                let foot = pos2(stage.center().x, stage.bottom() - 120.0);
                ground(&painter, stage, foot.y);
                dwarf::paint(
                    &painter,
                    foot,
                    stage.height() * 0.6,
                    &self.look,
                    &dwarf::pose(self.action, t),
                    self.facing_right,
                    &finish,
                );
                // The crowd: a different dwarf from each seed.
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
                    let foot = Pos2::new(
                        crowd.left() + (c as f32 + 0.5) * cell.x,
                        crowd.top() + (r as f32 + 0.86) * cell.y,
                    );
                    dwarf::paint(
                        &painter,
                        foot,
                        cell.y * 0.68,
                        &look,
                        &dwarf::pose(action, t + seed as f64 * 0.37),
                        !seed.is_multiple_of(3),
                        &finish,
                    );
                }
            });
        self.screenshot(ui);
        ui.ctx().request_repaint();
    }
}

impl Lab {
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
