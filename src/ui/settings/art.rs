//! The Art section: what the ant farm's figures look like. Ants or code-drawn dwarves, smooth
//! or as pixels, in their own colours or a fixed palette. Changes apply at once.

use eframe::egui;

use crate::ui::ant_farm::art::{ArtSettings, Pixels};
use crate::ui::dwarf::pixel;

pub const DWARVES_LABEL: &str = "Dwarves instead of ants in the ant farm";
pub const PIXELS_LABEL: &str = "Dwarf pixels";
pub const PALETTE_LABEL: &str = "Palette";

/// Draws the section; returns the new settings when one was changed.
pub(super) fn section(ui: &mut egui::Ui, applied: &ArtSettings) -> Option<ArtSettings> {
    let mut art = applied.clone();
    ui.checkbox(&mut art.dwarves, DWARVES_LABEL).on_hover_text(
        "Draw every agent as a dwarf, drawn in code: the Queen as the King, workers in \
         their agent's colours. Off: the ant sprites.",
    );
    ui.add_enabled_ui(art.dwarves, |ui| {
        egui::Grid::new("art_settings")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(PIXELS_LABEL);
                ui.horizontal_wrapped(|ui| {
                    for choice in Pixels::CHOICES {
                        ui.selectable_value(&mut art.pixels, choice, choice.label())
                            .on_hover_text(match choice {
                                Pixels::Off => "Smooth shapes at any zoom.",
                                Pixels::Farm => "One pixel per farm pixel, like the rest of the farm.",
                                Pixels::Height(_) => "Rendered this many pixels tall, then scaled.",
                            });
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
                            ui.selectable_value(
                                &mut art.palette,
                                file.to_string(),
                                PALETTE_NAMES[i],
                            );
                        }
                    });
                ui.end_row();
            });
        ui.weak(
            "Palettes from Lospec; each colour is swapped for its nearest in the palette. \
             Try them all side by side with `cargo run --release --example dwarf_lab -- --palettes`.",
        );
    });
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
