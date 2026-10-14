//! The Art section: dwarf pixel size and a palette for the whole mine. Changes apply at once.

use eframe::egui;

use crate::ui::dwarf::{cutout::Outline, pixel};
use crate::ui::farm::art::{ArtSettings, Pixels};

pub const PIXELS_LABEL: &str = "Pixels";
pub const PALETTE_LABEL: &str = "Palette";

/// Draws the section; returns the new settings when one was changed.
pub(super) fn section(ui: &mut egui::Ui, applied: &ArtSettings) -> Option<ArtSettings> {
    let mut art = applied.clone();
    egui::Grid::new("art_settings")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(PIXELS_LABEL);
            ui.horizontal_wrapped(|ui| {
                for choice in Pixels::CHOICES {
                    ui.selectable_value(&mut art.pixels, choice, choice.label())
                        .on_hover_text(match choice {
                            Pixels::Farm => "One pixel per hold pixel, like the rest of the hold.",
                            Pixels::Height(_) => "Dwarves render this many pixels tall. The world keeps the hold's grid.",
                        });
                }
            });
            ui.end_row();
            ui.label("Outline");
            ui.horizontal_wrapped(|ui| {
                for outline in Outline::ALL {
                    ui.selectable_value(&mut art.outline, outline, outline.label())
                        .on_hover_text("Ink width on dwarves and mine cutouts, measured on their pixel grid.");
                }
            });
            ui.end_row();
            ui.label(PALETTE_LABEL);
            let current = pixel::BUNDLED
                .iter()
                .position(|(name, _)| *name == art.palette)
                .map_or("Own colours", |i| PALETTE_NAMES[i]);
            egui::ComboBox::from_id_salt("art_palette")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut art.palette, String::new(), "Own colours");
                    for (i, (file, _)) in pixel::BUNDLED.iter().enumerate() {
                        ui.selectable_value(&mut art.palette, file.to_string(), PALETTE_NAMES[i]);
                    }
                });
            ui.end_row();
        });
    ui.weak(
            "Palettes from Lospec; each colour is swapped for its nearest in the palette. \
             Try them all side by side with `cargo run --release --example dwarf_lab -- --palettes`.",
        );
    (art != *applied).then_some(art)
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
}
