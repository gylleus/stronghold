//! The Notifications section: macOS and in-app pop-ups, a sound per severity with a volume and
//! "Test" buttons, the severity thresholds, quiet mode, and a test notification.

use std::path::PathBuf;

use eframe::egui::{self, RichText};

use super::{PaneOutput, SettingsPane, save_row};
use crate::notify::{NotifySettings, Severity, Sound};

/// What the section shows besides the settings.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotifyStatus<'a> {
    /// Notifications go through `UNUserNotificationCenter` (running from `Myrstack.app`).
    pub native: bool,
    /// The last error posting a notification or playing a sound.
    pub last_failure: Option<&'a str>,
    /// macOS alert sound names.
    pub system_sounds: &'a [String],
}

const CUSTOM: &str = "Custom file…";

pub(super) fn section(
    pane: &mut SettingsPane,
    ui: &mut egui::Ui,
    status: &NotifyStatus<'_>,
    out: &mut PaneOutput,
) {
    let d = &mut pane.notify_draft;
    ui.checkbox(&mut d.quiet, "Quiet mode (do not disturb)")
        .on_hover_text(
            "Only urgent notifications pop up or play a sound; the rest wait in the inbox",
        );
    ui.add_space(6.0);

    ui.label(RichText::new("Pop-ups").strong());
    ui.checkbox(&mut d.native_popups, "macOS notifications");
    if !status.native {
        ui.weak(
            "Myrstack isn't running from Myrstack.app: macOS shows these under Script Editor and \
             clicking them can't reach Myrstack.",
        );
    }
    ui.checkbox(&mut d.in_app_popups, "Pop-ups in the Myrstack window");
    severity_row(ui, "Pop up from", "notify_popup_min", &mut d.popup_min);
    ui.add_space(6.0);

    ui.label(RichText::new("Sounds").strong());
    severity_row(ui, "Play sounds from", "notify_sound_min", &mut d.sound_min);
    let volume = d.volume_percent;
    egui::Grid::new("notify_sounds")
        .num_columns(3)
        .show(ui, |ui| {
            for severity in Severity::ALL {
                ui.label(severity.title());
                let sound = d.sounds.get_mut(severity);
                sound_picker(ui, severity, sound, status.system_sounds);
                if ui
                    .add_enabled(!sound.is_none(), egui::Button::new("▶ Test"))
                    .clicked()
                {
                    out.test_sound = Some((sound.clone(), volume));
                }
                ui.end_row();
            }
        });
    ui.horizontal(|ui| {
        ui.label("Volume");
        ui.add(egui::Slider::new(&mut d.volume_percent, 0..=100).suffix(" %"));
    });
    ui.checkbox(
        &mut d.pop_sounds,
        "Soft sound when an agent is done or needs input",
    )
    .on_hover_text("Plays with its pop in the farm; not in quiet mode");
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("Send a test notification:");
        for severity in Severity::ALL {
            if ui.button(format!("Test {}", severity.name())).clicked() {
                out.test_notification = Some((severity, d.clone()));
            }
        }
    })
    .response
    .on_hover_text("Goes through the whole flow with the settings above, saved or not");
    if let Some(failure) = status.last_failure {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("Last delivery problem: {failure}"),
        );
    }
    ui.weak(
        "Agents raise notifications with the myrstack MCP tool notify; clicking one selects \
         its agent and opens its link. The inbox is the bell in the top bar.",
    );
    ui.separator();

    let changed = pane.notify_draft != pane.applied.notifications;
    let (save, revert) = save_row(ui, pane, changed, "Save");
    if save {
        out.notifications = pane.save_notifications();
    }
    if revert {
        pane.notify_draft = pane.applied.notifications.clone();
    }
}

fn severity_row(ui: &mut egui::Ui, label: &str, id: &str, value: &mut Severity) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(id)
            .selected_text(value.title())
            .show_ui(ui, |ui| {
                for severity in Severity::ALL {
                    ui.selectable_value(value, severity, severity.title());
                }
            });
    });
}

fn sound_label(sound: &Sound) -> String {
    match sound {
        Sound::None => "None".into(),
        Sound::System(name) => name.clone(),
        Sound::File(_) => CUSTOM.into(),
    }
}

fn sound_picker(ui: &mut egui::Ui, severity: Severity, sound: &mut Sound, system: &[String]) {
    ui.vertical(|ui| {
        egui::ComboBox::from_id_salt(("notify_sound", severity.name()))
            .selected_text(sound_label(sound))
            .show_ui(ui, |ui| {
                ui.selectable_value(sound, Sound::None, "None");
                for name in system {
                    ui.selectable_value(sound, Sound::System(name.clone()), name);
                }
                let is_file = matches!(sound, Sound::File(_));
                if ui.selectable_label(is_file, CUSTOM).clicked() && !is_file {
                    *sound = Sound::File(PathBuf::new());
                }
            });
        if let Sound::File(path) = sound {
            let mut text = path.display().to_string();
            let edit = egui::TextEdit::singleline(&mut text)
                .hint_text("/path/to/sound.aiff (aiff, wav, mp3, m4a)")
                .desired_width(260.0);
            if ui.add(edit).changed() {
                *path = PathBuf::from(text.trim());
            }
        }
    });
}

impl SettingsPane {
    /// The notification settings in effect.
    pub fn notifications(&self) -> &NotifySettings {
        &self.applied.notifications
    }

    /// Saves quiet mode at once (the inbox menu and the palette), keeping the rest of the draft.
    pub fn set_quiet(&mut self, on: bool) -> NotifySettings {
        let mut next = self.applied.clone();
        next.notifications.quiet = on;
        self.commit(next);
        self.notify_draft.quiet = on;
        self.applied.notifications.clone()
    }

    pub(super) fn save_notifications(&mut self) -> Option<NotifySettings> {
        if let Err(e) = self.notify_draft.validate() {
            self.message = Some((true, e));
            return None;
        }
        let mut next = self.applied.clone();
        next.notifications = self.notify_draft.clone();
        self.commit(next);
        Some(self.applied.notifications.clone())
    }
}
