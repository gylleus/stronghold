//! Talk-to-the-King UI: the pill inside the window (with buttons) and the small always-on-top
//! overlay window shown while Myrstack is in the background ("● Listening…" with a level
//! meter, then transcribing, thinking and speaking). The overlay never takes focus or clicks.

use std::time::Duration;

use eframe::egui::{self, Color32, RichText};

use super::dictation::level_meter;
use super::settings::voice::{speech_download_label, voice_download_label};
use crate::voice::talk::TalkPhase;

/// Size of the overlay window, in points.
pub const OVERLAY_SIZE: egui::Vec2 = egui::vec2(340.0, 40.0);
/// Gap between the top of the screen and the overlay (clears the menu bar).
const OVERLAY_TOP: f32 = 38.0;

/// A message the pill shows between turns.
#[derive(Debug, Clone, PartialEq)]
pub enum TalkNotice<'a> {
    Info(&'a str),
    Error(&'a str),
    /// Parakeet is not on disk: offer its download.
    SpeechModelMissing,
    /// Kokoro is not on disk: offer its download.
    VoiceMissing,
    Downloading {
        voice: bool,
        progress: Option<f32>,
    },
    DownloadFailed {
        voice: bool,
        error: &'a str,
    },
}

/// What the pill and overlay show.
#[derive(Debug, Clone, PartialEq)]
pub struct TalkView<'a> {
    pub phase: TalkPhase,
    /// Input level in 0..=1 while listening.
    pub level: f32,
    pub elapsed: Duration,
    /// The talk hotkey's label, e.g. `⌘⇧E`.
    pub hotkey: &'a str,
    pub notice: Option<TalkNotice<'a>>,
}

impl TalkView<'_> {
    /// Whether there is anything to show.
    pub fn visible(&self) -> bool {
        self.phase != TalkPhase::Idle || self.notice.is_some()
    }
}

/// A pill button the user clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TalkAction {
    /// Finish a latched recording.
    Send,
    Cancel,
    StopSpeaking,
    DownloadSpeechModel,
    DownloadVoice,
    Dismiss,
}

/// The main line of text for `view`, as both the pill and the overlay say it.
pub fn status_text(view: &TalkView<'_>) -> String {
    match view.phase {
        TalkPhase::Listening { .. } => "Listening…".into(),
        TalkPhase::Transcribing => "Transcribing…".into(),
        TalkPhase::Waiting => "Waiting for the King…".into(),
        TalkPhase::Thinking => "The King is thinking…".into(),
        TalkPhase::Speaking => "The King is speaking".into(),
        TalkPhase::Idle => match &view.notice {
            Some(TalkNotice::Info(t) | TalkNotice::Error(t)) => (*t).to_owned(),
            Some(TalkNotice::SpeechModelMissing) => {
                "Talking to the King needs the Parakeet speech model.".into()
            }
            Some(TalkNotice::VoiceMissing) => {
                "The King's voice is not downloaded, so his reply is text only.".into()
            }
            Some(TalkNotice::Downloading { voice: true, .. }) => {
                "Downloading the King's voice…".into()
            }
            Some(TalkNotice::Downloading { voice: false, .. }) => {
                "Downloading the speech model…".into()
            }
            Some(TalkNotice::DownloadFailed { voice, error }) => format!(
                "{} download failed: {error}",
                if *voice { "King voice" } else { "Speech model" }
            ),
            None => String::new(),
        },
    }
}

/// The hint after the status, e.g. how to send or stop.
pub fn hint_text(view: &TalkView<'_>) -> Option<String> {
    let key = view.hotkey;
    match view.phase {
        TalkPhase::Listening { latched: false } => Some(format!("release {key} to send")),
        TalkPhase::Listening { latched: true } => Some(format!("{key} sends · Esc cancels")),
        TalkPhase::Speaking => Some(format!("{key} or Esc stops")),
        _ => None,
    }
}

fn red() -> Color32 {
    Color32::from_rgb(230, 70, 70)
}

/// The leading mark and status line, shared by pill and overlay.
fn status_line(ui: &mut egui::Ui, view: &TalkView<'_>) {
    match view.phase {
        TalkPhase::Listening { .. } => {
            ui.label(RichText::new("●").color(red()));
            ui.label(RichText::new(status_text(view)).strong());
            level_meter(ui, view.level);
            let secs = view.elapsed.as_secs();
            ui.weak(format!("{}:{:02}", secs / 60, secs % 60));
        }
        TalkPhase::Transcribing | TalkPhase::Waiting | TalkPhase::Thinking => {
            ui.spinner();
            ui.label(status_text(view));
        }
        TalkPhase::Speaking => {
            ui.label("🔊");
            ui.label(RichText::new(status_text(view)).strong());
        }
        TalkPhase::Idle => {
            let error = matches!(
                view.notice,
                Some(TalkNotice::Error(_) | TalkNotice::DownloadFailed { .. })
            );
            if error {
                ui.label(RichText::new("⚠").color(red()));
            }
            if let Some(TalkNotice::Downloading { .. }) = view.notice {
                ui.spinner();
            }
            ui.add(egui::Label::new(status_text(view)).wrap());
        }
    }
}

/// The pill's contents: status plus buttons.
pub fn pill_contents(ui: &mut egui::Ui, view: &TalkView<'_>) -> Option<TalkAction> {
    let mut action = None;
    let mut button = |ui: &mut egui::Ui, label: &str, a: TalkAction| {
        if ui.small_button(label).clicked() {
            action = Some(a);
        }
    };
    ui.label(RichText::new("King").small().weak());
    status_line(ui, view);
    if let Some(hint) = hint_text(view) {
        ui.weak(hint);
    }
    match (view.phase, &view.notice) {
        (TalkPhase::Listening { latched: true }, _) => {
            button(ui, "Send", TalkAction::Send);
            button(ui, "Cancel", TalkAction::Cancel);
        }
        (TalkPhase::Listening { latched: false }, _) => {}
        (TalkPhase::Transcribing | TalkPhase::Waiting, _) => {
            button(ui, "Cancel", TalkAction::Cancel);
        }
        (TalkPhase::Thinking, _) => {}
        (TalkPhase::Speaking, _) => button(ui, "⏹ Stop", TalkAction::StopSpeaking),
        (TalkPhase::Idle, Some(TalkNotice::SpeechModelMissing)) => {
            button(
                ui,
                &speech_download_label(),
                TalkAction::DownloadSpeechModel,
            );
            button(ui, "✕", TalkAction::Dismiss);
        }
        (TalkPhase::Idle, Some(TalkNotice::VoiceMissing)) => {
            button(ui, &voice_download_label(), TalkAction::DownloadVoice);
            button(ui, "✕", TalkAction::Dismiss);
        }
        (TalkPhase::Idle, Some(TalkNotice::Downloading { progress, .. })) => {
            if let Some(p) = progress {
                ui.add(
                    egui::ProgressBar::new(*p)
                        .desired_width(140.0)
                        .show_percentage(),
                );
            }
        }
        (TalkPhase::Idle, Some(TalkNotice::DownloadFailed { voice, .. })) => {
            let retry = if *voice {
                TalkAction::DownloadVoice
            } else {
                TalkAction::DownloadSpeechModel
            };
            button(ui, "Retry", retry);
            button(ui, "✕", TalkAction::Dismiss);
        }
        (TalkPhase::Idle, Some(TalkNotice::Info(_) | TalkNotice::Error(_))) => {
            button(ui, "✕", TalkAction::Dismiss);
        }
        (TalkPhase::Idle, None) => {}
    }
    action
}

/// Draws the pill near the bottom of the window, above the dictation pill.
pub fn pill(ctx: &egui::Context, view: &TalkView<'_>) -> Option<TalkAction> {
    egui::Area::new(egui::Id::new("talk_pill"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -96.0))
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .corner_radius(18.0)
                .inner_margin(egui::Margin::symmetric(14, 8))
                .show(ui, |ui| ui.horizontal(|ui| pill_contents(ui, view)).inner)
                .inner
        })
        .inner
}

/// The overlay's contents: status only, no buttons (it lets clicks through).
pub fn overlay_contents(ui: &mut egui::Ui, view: &TalkView<'_>) {
    ui.horizontal_centered(|ui| {
        ui.add_space(6.0);
        status_line(ui, view);
        if let Some(hint) = hint_text(view) {
            ui.weak(hint);
        }
    });
}

/// Where the overlay goes: top centre of the screen the main window is on.
pub fn overlay_position(monitor: Option<egui::Vec2>) -> egui::Pos2 {
    let width = monitor.map_or(1440.0, |m| m.x);
    egui::pos2(((width - OVERLAY_SIZE.x) / 2.0).max(0.0), OVERLAY_TOP)
}

/// Shows the overlay window for this frame (call from `ui`, not `logic`): undecorated,
/// always on top, not focused on creation and transparent to the mouse.
pub fn overlay(ctx: &egui::Context, view: &TalkView<'_>) {
    let monitor = ctx.input(|i| i.viewport().monitor_size);
    let builder = egui::ViewportBuilder::default()
        .with_title("Myrstack: talking to the King")
        .with_decorations(false)
        .with_always_on_top()
        .with_active(false)
        .with_mouse_passthrough(true)
        .with_resizable(false)
        .with_taskbar(false)
        .with_inner_size(OVERLAY_SIZE)
        .with_position(overlay_position(monitor));
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("king_talk_overlay"),
        builder,
        |ui, _class| {
            egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                overlay_contents(ui, view);
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_util::{click_label, text_center};

    fn view(phase: TalkPhase, notice: Option<TalkNotice<'static>>) -> TalkView<'static> {
        TalkView {
            phase,
            level: 0.6,
            elapsed: Duration::from_secs(3),
            hotkey: "⌘⇧E",
            notice,
        }
    }

    fn render(draw: impl Fn(&egui::Context)) -> (egui::Context, egui::FullOutput) {
        let ctx = egui::Context::default();
        let mut last = None;
        for i in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 900.0),
                )),
                time: Some(f64::from(i) / 60.0),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| draw(ui.ctx()));
            out.textures_delta.clear();
            last = Some(out);
        }
        (ctx, last.unwrap())
    }

    #[test]
    fn overlay_and_pill_render_every_phase_headless() {
        let phases = [
            (TalkPhase::Listening { latched: false }, "Listening…"),
            (TalkPhase::Listening { latched: true }, "Listening…"),
            (TalkPhase::Transcribing, "Transcribing…"),
            (TalkPhase::Waiting, "Waiting for the King…"),
            (TalkPhase::Thinking, "The King is thinking…"),
            (TalkPhase::Speaking, "The King is speaking"),
        ];
        for (phase, text) in phases {
            let v = view(phase, None);
            assert!(v.visible());
            // Headless contexts embed viewports, so the overlay draws as a window.
            let (_, out) = render(|ctx| overlay(ctx, &v));
            assert!(text_center(&out, text).is_some(), "overlay {phase:?}");
            let (_, out) = render(|ctx| {
                pill(ctx, &v);
            });
            assert!(text_center(&out, text).is_some(), "pill {phase:?}");
        }
        let (_, out) = render(|ctx| overlay(ctx, &view(TalkPhase::Speaking, None)));
        assert!(text_center(&out, "⌘⇧E or Esc stops").is_some());
        assert!(!view(TalkPhase::Idle, None).visible());
    }

    #[test]
    fn missing_model_prompts_only_report_the_click() {
        let v = view(TalkPhase::Idle, Some(TalkNotice::VoiceMissing));
        let label = voice_download_label();
        let action = click_label(&label, |ui| pill(ui.ctx(), &v));
        assert_eq!(action, Some(TalkAction::DownloadVoice));

        let v = view(TalkPhase::Idle, Some(TalkNotice::SpeechModelMissing));
        let label = speech_download_label();
        let action = click_label(&label, |ui| pill(ui.ctx(), &v));
        assert_eq!(action, Some(TalkAction::DownloadSpeechModel));

        let v = view(TalkPhase::Speaking, None);
        let action = click_label("⏹ Stop", |ui| pill(ui.ctx(), &v));
        assert_eq!(action, Some(TalkAction::StopSpeaking));

        let v = view(TalkPhase::Listening { latched: true }, None);
        let action = click_label("Send", |ui| pill(ui.ctx(), &v));
        assert_eq!(action, Some(TalkAction::Send));
    }

    #[test]
    fn texts_and_position() {
        let failed = view(
            TalkPhase::Idle,
            Some(TalkNotice::DownloadFailed {
                voice: true,
                error: "offline",
            }),
        );
        assert_eq!(status_text(&failed), "King voice download failed: offline");
        assert_eq!(
            hint_text(&view(TalkPhase::Listening { latched: false }, None)).as_deref(),
            Some("release ⌘⇧E to send")
        );
        let pos = overlay_position(Some(egui::vec2(1512.0, 982.0)));
        assert_eq!(
            pos,
            egui::pos2((1512.0 - OVERLAY_SIZE.x) / 2.0, OVERLAY_TOP)
        );
    }
}
