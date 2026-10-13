//! The Voice section: the talk-to-the-King hotkey, speaking replies, the King's voice
//! (voice, speed, volume, a test button) and the two speech models. Edits go to the draft;
//! the pane shows Save / Revert once it differs from the saved settings.

use eframe::egui::{self, RichText};

use super::PaneOutput;
use crate::ui::dictation::SHORTCUT as DICTATION_SHORTCUT;
use crate::voice::models::{PARAKEET_SIZE_LABEL, kokoro};
use crate::voice::settings::{TALK_HOTKEYS, VoiceSettings, hotkey_label};
use crate::voice::tts::Voice;

/// Said by the "Test voice" button.
pub const TEST_SENTENCE: &str = "Hi Karl. This is how I will sound when I answer you.";

/// A speech model's state, for the Models rows.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ModelStatus {
    #[default]
    Missing,
    /// On disk; the text says where or whether it is loaded.
    Installed(String),
    Downloading(Option<f32>),
    Unpacking,
    Failed(String),
}

/// Which model a download button asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceModel {
    /// Parakeet speech-to-text.
    Speech,
    /// Kokoro, the King's voice.
    Voice,
}

/// What the section shows besides the settings.
#[derive(Debug, Clone, Default)]
pub struct VoiceStatus<'a> {
    /// Kokoro voices by id.
    pub voices: &'a [Voice],
    pub speech_model: ModelStatus,
    pub voice_model: ModelStatus,
    /// Why the talk hotkey could not be registered.
    pub hotkey_error: Option<&'a str>,
}

/// Label of the Kokoro download button.
pub fn voice_download_label() -> String {
    format!("Download King voice ({})", kokoro::SIZE_LABEL)
}

/// Label of the Parakeet download button.
pub fn speech_download_label() -> String {
    format!("Download speech model ({PARAKEET_SIZE_LABEL})")
}

/// Draws the voice settings form, editing `draft`.
pub fn voice_section(
    ui: &mut egui::Ui,
    draft: &mut VoiceSettings,
    status: &VoiceStatus<'_>,
    out: &mut PaneOutput,
) {
    ui.label(RichText::new("Talk to the King").strong());
    egui::Grid::new("voice_talk")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Hold to talk");
            hotkey_picker(ui, &mut draft.talk_hotkey);
            ui.end_row();

            ui.label("Speak replies");
            ui.checkbox(&mut draft.speak_replies, "Speak all of the King's replies")
                .on_hover_text("Replies to voice messages are always spoken");
            ui.end_row();

            ui.label("Dictation");
            ui.label(format!(
                "Hold {DICTATION_SHORTCUT} in Myrstack to dictate into the focused field"
            ));
            ui.end_row();
        });
    ui.weak(format!(
        "Hold {} anywhere, even with Myrstack in the background, and release to send. His \
         reply to a voice message is spoken; press the hotkey, Esc or any key in Myrstack to \
         stop him.",
        draft.talk_hotkey_label()
    ));
    if let Some(err) = status.hotkey_error {
        ui.colored_label(ui.visuals().error_fg_color, err);
    }

    ui.separator();
    ui.label(RichText::new("The King's voice").strong());
    egui::Grid::new("voice_voice")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Voice");
            voice_picker(ui, &mut draft.voice, status.voices);
            ui.end_row();

            ui.label("Speed");
            ui.add(egui::Slider::new(&mut draft.speed_percent, 50..=200).suffix("%"));
            ui.end_row();

            ui.label("Volume");
            ui.add(egui::Slider::new(&mut draft.volume_percent, 0..=100).suffix("%"));
            ui.end_row();
        });
    let installed = matches!(status.voice_model, ModelStatus::Installed(_));
    if ui
        .add_enabled(installed, egui::Button::new("▶ Test voice"))
        .on_hover_text(format!("Says \"{TEST_SENTENCE}\" with the settings above"))
        .on_disabled_hover_text("Download the King's voice first")
        .clicked()
    {
        out.test_voice = Some(draft.clone());
    }

    ui.separator();
    ui.label(RichText::new("Speech models").strong());
    ui.weak("Downloaded only when you ask. Both run on this Mac.");
    egui::Grid::new("voice_models")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Speech recognition (Parakeet)");
            model_row(
                ui,
                &status.speech_model,
                &speech_download_label(),
                VoiceModel::Speech,
                out,
            );
            ui.end_row();

            ui.label("King's voice (Kokoro)");
            model_row(
                ui,
                &status.voice_model,
                &voice_download_label(),
                VoiceModel::Voice,
                out,
            );
            ui.end_row();
        });
}

fn hotkey_picker(ui: &mut egui::Ui, hotkey: &mut String) {
    egui::ComboBox::from_id_salt("voice_talk_hotkey")
        .selected_text(hotkey_label(hotkey))
        .width(140.0)
        .show_ui(ui, |ui| {
            for (value, label) in TALK_HOTKEYS {
                if ui.selectable_label(hotkey == value, *label).clicked() {
                    *hotkey = (*value).to_owned();
                }
            }
        });
}

/// English voices first, then the rest, each list in model order.
pub fn ordered_voices(voices: &[Voice]) -> Vec<&Voice> {
    let (mut english, other): (Vec<&Voice>, Vec<&Voice>) =
        voices.iter().partition(|v| v.is_english());
    english.extend(other);
    english
}

fn voice_picker(ui: &mut egui::Ui, name: &mut String, voices: &[Voice]) {
    let selected = voices
        .iter()
        .find(|v| v.name == *name)
        .map_or_else(|| name.clone(), Voice::label);
    egui::ComboBox::from_id_salt("voice_voice")
        .selected_text(selected)
        .width(260.0)
        .height(360.0)
        .show_ui(ui, |ui| {
            let mut english = true;
            for voice in ordered_voices(voices) {
                if english && !voice.is_english() {
                    english = false;
                    ui.separator();
                    ui.weak("Other languages (read English text with an accent)");
                }
                if ui
                    .selectable_label(voice.name == *name, voice.label())
                    .clicked()
                {
                    name.clone_from(&voice.name);
                }
            }
        });
}

fn model_row(
    ui: &mut egui::Ui,
    status: &ModelStatus,
    download_label: &str,
    model: VoiceModel,
    out: &mut PaneOutput,
) {
    ui.horizontal(|ui| match status {
        ModelStatus::Installed(detail) => {
            ui.label(RichText::new("✔").color(egui::Color32::from_rgb(0x4c, 0xaf, 0x50)));
            ui.label(detail);
        }
        ModelStatus::Missing => {
            ui.weak("Not downloaded");
            if ui.button(download_label).clicked() {
                out.download = Some(model);
            }
        }
        ModelStatus::Downloading(progress) => {
            ui.spinner();
            match progress {
                Some(p) => {
                    ui.add(
                        egui::ProgressBar::new(*p)
                            .desired_width(160.0)
                            .show_percentage(),
                    );
                }
                None => {
                    ui.label("Downloading…");
                }
            }
        }
        ModelStatus::Unpacking => {
            ui.spinner();
            ui.label("Unpacking…");
        }
        ModelStatus::Failed(err) => {
            ui.colored_label(ui.visuals().error_fg_color, format!("Failed: {err}"));
            if ui.button("Retry").clicked() {
                out.download = Some(model);
            }
        }
    });
}
