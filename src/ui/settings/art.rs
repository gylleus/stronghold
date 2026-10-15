//! The Art section applies changes immediately and keeps an offline palette library.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui;

use crate::ui::dwarf::{
    cutout::Outline,
    palettes::{self, Imported, Input, Library},
    pixel,
};
use crate::ui::farm::art::{ArtSettings, Pixels, Preview};

pub const PIXELS_LABEL: &str = "Pixels";
pub const PALETTE_LABEL: &str = "Palette";

pub struct PaletteEditor {
    input: String,
    library: Library,
    path: Option<PathBuf>,
    pending: Option<Receiver<Result<Imported, String>>>,
    message: Option<(bool, String)>,
    preview: Preview,
}

impl PaletteEditor {
    pub fn load(path: Option<PathBuf>) -> Self {
        let loaded = path
            .as_ref()
            .map_or_else(|| Ok(Library::default()), |p| Library::load(p));
        let (library, message, path) = match loaded {
            Ok(library) => (library, None, path),
            Err(e) => (
                Library::default(),
                Some((
                    true,
                    format!("{e} Imports are session-only until the cache is repaired."),
                )),
                None,
            ),
        };
        Self {
            input: String::new(),
            library,
            path,
            pending: None,
            message,
            preview: Preview::default(),
        }
    }

    fn save(&mut self) {
        let result = self
            .path
            .as_ref()
            .ok_or_else(|| "No writable palette cache; recent imports are session-only.".into())
            .and_then(|p| self.library.save(p));
        if let Err(error) = result {
            self.message = Some((true, error));
        }
    }

    fn apply(&mut self, result: Result<Imported, String>, art: &mut ArtSettings) {
        match result {
            Ok(palette) => {
                art.palette = palette.selection();
                self.message = Some((
                    false,
                    format!(
                        "Applied {} · {} colours.",
                        palette.name,
                        palette.colors.len()
                    ),
                ));
                self.library.insert(palette);
                self.save();
            }
            Err(error) => self.message = Some((true, error)),
        }
    }

    fn import(&mut self, ctx: &egui::Context, art: &mut ArtSettings) {
        match Input::parse(&self.input) {
            Err(e) => self.message = Some((true, e)),
            Ok(input) => {
                if let Some(palette) = self.library.cached(&input) {
                    self.apply(Ok(palette), art);
                } else if let Input::Lospec(slug) = input {
                    let (sender, receiver) = mpsc::channel();
                    let ctx = ctx.clone();
                    match std::thread::Builder::new()
                        .name("palette-import".into())
                        .spawn(move || {
                            let _ = sender.send(palettes::fetch(&slug));
                            ctx.request_repaint();
                        }) {
                        Ok(_) => {
                            self.pending = Some(receiver);
                            self.message = None;
                        }
                        Err(e) => {
                            self.message =
                                Some((true, format!("Could not start palette import: {e}")))
                        }
                    }
                }
            }
        }
    }

    pub fn section(&mut self, ui: &mut egui::Ui, applied: &ArtSettings) -> Option<ArtSettings> {
        let mut art = applied.clone();
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(result) => {
                    self.pending = None;
                    self.apply(result, &mut art);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.message = Some((true, "Palette download stopped; please retry.".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        egui::Grid::new("art_settings")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(PIXELS_LABEL);
                ui.horizontal_wrapped(|ui| {
                    for choice in Pixels::CHOICES {
                        ui.selectable_value(&mut art.pixels, choice, choice.label());
                    }
                });
                ui.end_row();
                ui.label("Outline");
                ui.horizontal_wrapped(|ui| {
                    for outline in Outline::ALL {
                        ui.selectable_value(&mut art.outline, outline, outline.label());
                    }
                });
                ui.end_row();
                ui.label(PALETTE_LABEL);
                egui::ComboBox::from_id_salt("art_palette")
                    .selected_text(self.library.name(&art.palette))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut art.palette, String::new(), "Own colours");
                        for (i, (file, _)) in pixel::BUNDLED.iter().enumerate() {
                            ui.selectable_value(
                                &mut art.palette,
                                file.to_string(),
                                PALETTE_NAMES[i],
                            );
                        }
                        if !self.library.imports.is_empty() {
                            ui.separator();
                            ui.weak("Imported · available offline");
                            for palette in self.library.imports.iter().rev() {
                                ui.selectable_value(
                                    &mut art.palette,
                                    palette.selection(),
                                    &palette.name,
                                );
                            }
                        }
                    });
                ui.end_row();
            });
        ui.separator();
        ui.label("Import a palette");
        ui.weak(
            "Lospec name or URL · Coolors URL · hex colours separated by spaces, commas or dashes",
        );
        let mut submit = false;
        ui.add_enabled_ui(self.pending.is_none(), |ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .desired_width(f32::INFINITY)
                    .hint_text("ice-cream-gb  or  264653-2a9d8f-e9c46a"),
            );
            submit = (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("Import & apply").clicked();
        });
        if submit {
            self.import(ui.ctx(), &mut art);
        }
        if self.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Fetching from Lospec…");
                if ui.button("Cancel").clicked() {
                    self.pending = None;
                }
            });
        }
        if let Some((error, message)) = &self.message {
            ui.colored_label(
                if *error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                },
                message,
            );
        }
        if !self.library.recent.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.weak("Recent");
                for selection in &self.library.recent {
                    ui.selectable_value(
                        &mut art.palette,
                        selection.clone(),
                        self.library.name(selection),
                    );
                }
            });
        }
        if art.palette != applied.palette {
            // An explicit choice wins over a fetch still in flight.
            self.pending = None;
            self.library.remember(&art.palette);
            self.save();
        }
        ui.separator();
        ui.label(format!(
            "Live preview · {}",
            self.library.name(&art.palette)
        ));
        if let Some(palette) = palettes::resolve(&art.palette) {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
                for color in &palette.colors {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, *color);
                    response.on_hover_text(format!(
                        "#{:02x}{:02x}{:02x}",
                        color.r(),
                        color.g(),
                        color.b()
                    ));
                }
            });
            if art.palette.starts_with("hex:")
                && pixel::Mapper::imported(&palette).support_shades(&palette) > 0
            {
                ui.weak("Contrast support shades added for readable labels and outlines.");
            }
        }
        self.preview.show(ui, &art);
        if let Some(palette) = self
            .library
            .imports
            .iter()
            .find(|p| p.selection() == art.palette)
        {
            if let Some(author) = &palette.author {
                ui.weak(format!("Palette by {author}"));
            }
            if palette.source.starts_with("https://lospec.com/") {
                ui.hyperlink_to("View on Lospec", &palette.source);
            }
        }
        (art != *applied).then_some(art)
    }
}
/// Display names of [`pixel::BUNDLED`], in order.
const PALETTE_NAMES: [&str; 16] = [
    "Endesga 32",
    "Jehkoba64",
    "Resurrect 64",
    "Apollo",
    "AAP-64",
    "DawnBringer 32",
    "Zughy 32",
    "Journey",
    "Lospec500",
    "Vinik24",
    "Fantasy 24",
    "Sweetie 16",
    "PICO-8",
    "NA16",
    "SLSO8",
    "Oil 6",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The names line up with the bundled palettes they stand for.
    #[test]
    fn palette_names_match_the_bundle() {
        for (palette, name) in pixel::bundled().iter().zip(PALETTE_NAMES) {
            assert_eq!(palette.name, name);
        }
    }
    #[test]
    fn hex_import_preserves_art_controls_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("palettes.json");
        let mut editor = PaletteEditor::load(Some(path.clone()));
        let ctx = egui::Context::default();
        let mut art = ArtSettings {
            pixels: Pixels::Height(24),
            outline: Outline::Bold,
            palette: "oil-6".into(),
        };
        editor.input = "https://coolors.co/264653-2a9d8f-e9c46a".into();
        editor.import(&ctx, &mut art);
        assert_eq!(art.pixels, Pixels::Height(24));
        assert_eq!(art.outline, Outline::Bold);
        assert!(art.palette.starts_with("hex:"));
        let reloaded = PaletteEditor::load(Some(path));
        assert_eq!(reloaded.library.recent, vec![art.palette.clone()]);
        let original = art.clone();
        editor.input = "#bad-input".into();
        editor.import(&ctx, &mut art);
        assert_eq!(art, original);
        assert!(editor.message.as_ref().unwrap().0);
    }

    #[test]
    fn failed_download_keeps_the_current_palette_and_displays_the_error() {
        let ctx = egui::Context::default();
        let mut editor = PaletteEditor::load(None);
        let art = ArtSettings {
            palette: "oil-6".into(),
            ..Default::default()
        };
        let (sender, receiver) = mpsc::channel();
        editor.pending = Some(receiver);
        sender
            .send(Err("Lospec: network unavailable".into()))
            .unwrap();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            assert!(editor.section(ui, &art).is_none());
        });
        output.textures_delta.clear();
        assert!(editor.pending.is_none());
        assert_eq!(
            editor.message,
            Some((true, "Lospec: network unavailable".into()))
        );
    }
}
