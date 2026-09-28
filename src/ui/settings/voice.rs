//! The Voice section. A placeholder until the voice feature adds dictation and spoken-reply
//! options: add fields to [`VoiceSettings`] and their widgets to [`voice_section`]; the pane
//! shows Save / Revert once the draft differs from the saved settings and reports the saved
//! value in `PaneOutput::voice`.

use eframe::egui;

use crate::voice::settings::VoiceSettings;

/// Shown while the Voice section has no settings.
pub const PLACEHOLDER: &str = "Dictation and voice settings arrive with the voice feature.";

/// Draws the voice settings form, editing `draft`.
pub fn voice_section(ui: &mut egui::Ui, draft: &mut VoiceSettings) {
    let VoiceSettings {} = draft;
    ui.weak(PLACEHOLDER);
}
