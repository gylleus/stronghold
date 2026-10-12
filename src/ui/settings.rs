//! The Settings window: a section list on the left (Queen, Envoy, Whip, Notifications, Voice,
//! Art, General) and the
//! chosen section's form. It owns the settings file: every save writes the whole file with the
//! edited section replaced. Opened from the top bar's gear, the palette's "Settings…" and ⌘,;
//! the "Whip…" button and "Whip settings…" open it on the Whip section.

pub mod art;
pub mod envoy;
pub mod general;
pub mod notifications;
pub mod queen;
pub mod voice;
pub mod whip;

use std::path::PathBuf;

use eframe::egui::{self, RichText};

use crate::chat::settings::QueenSettings;
use crate::notify::{NotifySettings, Severity, Sound};
use crate::spawn::SpawnSettings;
use crate::voice::settings::VoiceSettings;
use crate::whip::{Settings, WhipSettings, WhipStatus};

pub use general::GeneralInfo;
pub use notifications::NotifyStatus;
pub use queen::QueenStatus;
pub use voice::{ModelStatus, VoiceModel, VoiceStatus};

/// A section of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Queen,
    Envoy,
    Whip,
    Notifications,
    Voice,
    Art,
    General,
}

impl Section {
    pub const ALL: [Section; 7] = [
        Section::Queen,
        Section::Envoy,
        Section::Whip,
        Section::Notifications,
        Section::Voice,
        Section::Art,
        Section::General,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Queen => "Queen",
            Section::Envoy => "Envoy",
            Section::Whip => "Whip",
            Section::Notifications => "Notifications",
            Section::Voice => "Voice",
            Section::Art => "Art",
            Section::General => "General",
        }
    }
}

/// What the window shows besides the settings themselves.
pub struct PaneContext<'a> {
    pub whip_status: &'a WhipStatus,
    pub now_ms: i64,
    pub queen: QueenStatus<'a>,
    /// Paths and versions for the General section.
    pub general: &'a GeneralInfo,
    /// Voices and speech model states for the Voice section.
    pub voice: VoiceStatus<'a>,
    /// Delivery state and sound names for the Notifications section.
    pub notify: NotifyStatus<'a>,
}

/// What the user changed this frame.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PaneOutput {
    /// Saved Envoy preferences; safety ON also applies to the current session.
    pub envoy: Option<crate::envoy::settings::EnvoySettings>,
    /// Saved whip settings.
    pub whip: Option<WhipSettings>,
    /// Saved Queen settings (the Queen restarts when her model or effort changed).
    pub queen: Option<QueenSettings>,
    /// The Queen's auto mode switch was flipped (applies at once, like the chat header's).
    pub auto_mode: Option<bool>,
    /// Saved voice settings (from the Voice section, or the Queen's "Speak replies").
    pub voice: Option<VoiceSettings>,
    /// "Test voice": speak a sample with these (unsaved) settings.
    pub test_voice: Option<VoiceSettings>,
    /// A speech model's download button.
    pub download: Option<VoiceModel>,
    /// "Whip all stalled now" was clicked.
    pub whip_stalled: bool,
    /// Saved notification settings.
    pub notifications: Option<NotifySettings>,
    /// A sound's "Test": play it at this volume (percent).
    pub test_sound: Option<(Sound, u8)>,
    /// "Send a test notification" of this severity, delivered with these (unsaved) settings.
    pub test_notification: Option<(Severity, NotifySettings)>,
}

/// Window state: the applied settings, a draft per section and the file they live in.
pub struct SettingsPane {
    pub open: bool,
    pub section: Section,
    /// Settings as loaded or last saved.
    applied: Settings,
    path: Option<PathBuf>,
    /// Result of the last load or save: `(is_error, text)`.
    message: Option<(bool, String)>,
    whip_draft: WhipSettings,
    queen_draft: queen::QueenDraft,
    voice_draft: VoiceSettings,
    notify_draft: NotifySettings,
    envoy_draft: envoy::EnvoyDraft,
    /// "Whip all stalled now" was clicked; see [`SettingsPane::take_whip_stalled`].
    whip_stalled: bool,
}

impl SettingsPane {
    /// Loads the settings from `path` (defaults when `None` or missing).
    pub fn load(path: Option<PathBuf>) -> Self {
        let (applied, message) = match &path {
            Some(p) => {
                let loaded = Settings::load_from(p);
                (loaded.settings, loaded.warning.map(|w| (true, w)))
            }
            None => (
                Settings::default(),
                Some((true, "no config directory; settings are not saved".into())),
            ),
        };
        Self {
            open: false,
            section: Section::default(),
            whip_draft: applied.whip.clone(),
            queen_draft: queen::QueenDraft::new(&applied.queen),
            voice_draft: applied.voice.clone(),
            notify_draft: applied.notifications.clone(),
            envoy_draft: envoy::EnvoyDraft::new(&applied.envoy),
            applied,
            path,
            message,
            whip_stalled: false,
        }
    }

    /// The whip settings in effect.
    pub fn whip(&self) -> &WhipSettings {
        &self.applied.whip
    }

    /// The saved Queen settings.
    pub fn queen(&self) -> &QueenSettings {
        &self.applied.queen
    }

    pub fn envoy(&self) -> &crate::envoy::settings::EnvoySettings {
        &self.applied.envoy
    }

    pub fn voice(&self) -> &VoiceSettings {
        &self.applied.voice
    }

    pub fn spawn(&self) -> &SpawnSettings {
        &self.applied.spawn
    }

    pub fn art(&self) -> &crate::ui::ant_farm::art::ArtSettings {
        &self.applied.art
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Opens the window on `section` with fresh drafts of the applied settings.
    pub fn open_section(&mut self, section: Section) {
        self.whip_draft = self.applied.whip.clone();
        self.queen_draft = queen::QueenDraft::new(&self.applied.queen);
        self.voice_draft = self.applied.voice.clone();
        self.notify_draft = self.applied.notifications.clone();
        self.envoy_draft = envoy::EnvoyDraft::new(&self.applied.envoy);
        self.section = section;
        self.open = true;
    }

    /// Opens the window on the Whip section (the old whip window's entry points).
    pub fn open_window(&mut self) {
        self.open_section(Section::Whip);
    }

    /// Opens the window on the section it showed last (gear button, ⌘,, "Settings…").
    pub fn open_settings(&mut self) {
        self.open_section(self.section);
    }

    /// Whether "Whip all stalled now" was clicked since the last call.
    pub fn take_whip_stalled(&mut self) -> bool {
        std::mem::take(&mut self.whip_stalled)
    }

    /// Validates, saves and applies `whip` as if saved from the window. Returns the status
    /// message on success; on a validation error nothing changes.
    pub fn apply(&mut self, whip: WhipSettings) -> Result<String, String> {
        let draft = std::mem::replace(&mut self.whip_draft, whip);
        if self.save_whip().is_some() {
            Ok(self
                .message
                .as_ref()
                .map(|m| m.1.clone())
                .unwrap_or_default())
        } else {
            self.whip_draft = draft;
            Err(self
                .message
                .as_ref()
                .map_or_else(|| "invalid settings".into(), |m| m.1.clone()))
        }
    }

    /// Top-bar gear that opens the window.
    pub fn gear_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("⚙")
            .on_hover_text("Settings: Queen model, whip, voice, paths (⌘,)")
            .clicked()
        {
            self.open_settings();
        }
    }

    /// Top-bar "Whip…" button plus mode badge; opens the window on the Whip section.
    pub fn button(&mut self, ui: &mut egui::Ui) {
        if ui.button("Whip…").clicked() {
            self.open_window();
        }
        whip::mode_badge(ui, &self.applied.whip);
    }

    /// Draws the window if open.
    pub fn show(&mut self, ctx: &egui::Context, cx: &PaneContext<'_>) -> PaneOutput {
        let mut out = PaneOutput::default();
        if !self.open {
            return out;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(680.0)
            .default_height(460.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(110.0);
                        for section in Section::ALL {
                            let text = RichText::new(section.title()).size(15.0);
                            ui.selectable_value(&mut self.section, section, text);
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, true])
                            .show(ui, |ui| self.section_ui(ui, cx, &mut out));
                    });
                });
            });
        self.open = open;
        out
    }

    fn section_ui(&mut self, ui: &mut egui::Ui, cx: &PaneContext<'_>, out: &mut PaneOutput) {
        ui.heading(self.section.title());
        ui.add_space(4.0);
        match self.section {
            Section::Queen => queen::section(self, ui, &cx.queen, out),
            Section::Envoy => envoy::section(self, ui, out),
            Section::Whip => {
                out.whip = whip::form(self, ui);
                ui.separator();
                whip::status_view(ui, cx.whip_status, cx.now_ms);
                if crate::ui::whip_now::whip_stalled_button(ui) {
                    self.whip_stalled = true;
                    out.whip_stalled = true;
                }
            }
            Section::Notifications => notifications::section(self, ui, &cx.notify, out),
            Section::Voice => {
                voice::voice_section(ui, &mut self.voice_draft, &cx.voice, out);
                ui.separator();
                let changed = self.voice_draft != self.applied.voice;
                let (save, revert) = save_row(ui, self, changed, "Save");
                if save {
                    out.voice = self.save_voice();
                }
                if revert {
                    self.voice_draft = self.applied.voice.clone();
                }
            }
            Section::Art => {
                if let Some(next) = art::section(ui, &self.applied.art) {
                    let mut settings = self.applied.clone();
                    settings.art = next;
                    self.commit(settings);
                }
            }
            Section::General => {
                let focus_new = self.applied.spawn.focus_new;
                if let Some(on) = general::section(ui, cx.general, self.path.as_deref(), focus_new)
                {
                    self.set_focus_new(on);
                }
            }
        }
        self.message_ui(ui);
    }

    fn message_ui(&self, ui: &mut egui::Ui) {
        if let Some((is_error, text)) = &self.message {
            let color = if *is_error {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            };
            ui.colored_label(color, text);
        }
    }

    /// Writes `next` to the file and makes it the applied settings.
    fn commit(&mut self, next: Settings) {
        self.message = match &self.path {
            Some(path) => match next.save_to(path) {
                Ok(()) => Some((false, "Saved.".into())),
                Err(e) => Some((true, format!("Applied, but saving failed: {e}"))),
            },
            None => Some((
                true,
                "Applied for this session only (no settings path).".into(),
            )),
        };
        self.applied = next;
    }

    /// Validates and writes the whip draft, then makes it the applied settings.
    fn save_whip(&mut self) -> Option<WhipSettings> {
        if let Err(e) = whip::validate_draft(&self.whip_draft) {
            self.message = Some((true, e));
            return None;
        }
        let mut next = self.applied.clone();
        next.whip = self.whip_draft.clone();
        self.commit(next);
        Some(self.applied.whip.clone())
    }

    /// Validates and writes the Queen draft, then makes it the applied settings.
    fn save_queen(&mut self) -> Option<QueenSettings> {
        let queen = self.queen_draft.settings.normalized();
        if let Err(e) = queen.validate() {
            self.message = Some((true, e));
            return None;
        }
        let mut next = self.applied.clone();
        next.queen = queen;
        self.commit(next);
        self.queen_draft = queen::QueenDraft::new(&self.applied.queen);
        Some(self.applied.queen.clone())
    }

    /// Saves "Speak replies" at once (the Queen section's switch and the palette), keeping
    /// the rest of the voice draft.
    pub fn set_speak_replies(&mut self, on: bool) -> VoiceSettings {
        let mut next = self.applied.clone();
        next.voice.speak_replies = on;
        self.commit(next);
        self.voice_draft.speak_replies = on;
        self.applied.voice.clone()
    }

    /// Saves "Focus new ants when they're spawned" at once.
    pub fn set_focus_new(&mut self, on: bool) -> SpawnSettings {
        let mut next = self.applied.clone();
        next.spawn.focus_new = on;
        self.commit(next);
        self.applied.spawn.clone()
    }

    fn save_voice(&mut self) -> Option<VoiceSettings> {
        if let Err(e) = self.voice_draft.validate() {
            self.message = Some((true, e));
            return None;
        }
        let mut next = self.applied.clone();
        next.voice = self.voice_draft.clone();
        self.commit(next);
        Some(self.applied.voice.clone())
    }
}

/// A Save / Revert row, enabled when `changed`, with the settings path. Returns
/// `(save clicked, revert clicked)`.
fn save_row(
    ui: &mut egui::Ui,
    pane: &SettingsPane,
    changed: bool,
    save_label: &str,
) -> (bool, bool) {
    ui.horizontal(|ui| {
        let save = ui
            .add_enabled(changed, egui::Button::new(save_label))
            .clicked();
        let revert = ui
            .add_enabled(changed, egui::Button::new("Revert"))
            .clicked();
        if let Some(path) = &pane.path {
            ui.weak(path.display().to_string());
        }
        (save, revert)
    })
    .inner
}

#[cfg(test)]
mod tests;
