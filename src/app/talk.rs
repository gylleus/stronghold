//! The app side of talking to the King: the global hold-to-talk hotkey, sending transcripts
//! to his chat as voice messages, speaking his replies, the pill and the background overlay,
//! and the two speech models (downloaded only when Karl clicks, prepared at start once there).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

use super::{StrongholdApp, repaint_once};
use crate::hotkey::{Hotkey, TalkKey};
use crate::ui::settings::voice::TEST_SENTENCE;
use crate::ui::settings::{ModelStatus, VoiceModel, VoiceStatus};
use crate::ui::talk::{self as ui_talk, TalkAction, TalkNotice, TalkView};
use crate::voice::dictation::DictationError;
use crate::voice::models::{self, Download, DownloadStatus, kokoro};
use crate::voice::settings::VoiceSettings;
use crate::voice::talk::{Talk, TalkEvent, TalkPhase};
use crate::voice::tts::{DEFAULT_VOICE, Speaker, SpeakerConfig, SpeakerStatus};

/// How long an info or error notice stays up.
const NOTICE_TIME: Duration = Duration::from_secs(6);
/// Meter refresh while listening.
const METER_REPAINT: Duration = Duration::from_millis(50);
/// Refresh while transcribing, waiting or speaking (to notice when it ends).
const BUSY_REPAINT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq)]
enum Notice {
    Info(String),
    Error(String),
    SpeechModelMissing,
    VoiceMissing,
    DownloadFailed { voice: bool, error: String },
}

pub(super) struct AppTalk {
    talk: Talk<Speaker>,
    /// The saved voice settings.
    settings: VoiceSettings,
    /// "Test voice" changed the speaker's voice; restore `settings` at the next turn.
    testing: bool,
    notice: Option<(Notice, Instant)>,
    speech_download: Option<Download>,
    voice_download: Option<kokoro::Fetch>,
    voice_installed: bool,
    /// The talk hotkey string registered, and why registration failed.
    registered: Option<String>,
    hotkey_error: Option<String>,
    /// When the last voice message went out, for the first-token log line.
    sent_at: Option<Instant>,
    /// Bring the window forward on the next frame (a prompt needs a click).
    bring_forward: bool,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

fn speaker_config() -> SpeakerConfig {
    SpeakerConfig {
        allow_download: false,
        ..SpeakerConfig::default()
    }
}

fn voice_installed() -> bool {
    let config = speaker_config();
    kokoro::installed(&config.models_root, &config.release).is_some()
}

/// Whether Parakeet is on disk: `Some(true)` for Orca's copy, `Some(false)` for Stronghold's.
pub(super) fn speech_model_found() -> Option<bool> {
    models::find_parakeet().map(|files| {
        models::orca_parakeet_dir().is_some_and(|orca| files.encoder.starts_with(orca))
    })
}

impl AppTalk {
    pub(super) fn new(ctx: &egui::Context, settings: &VoiceSettings) -> Self {
        let installed = voice_installed();
        let mut talk = Talk::new(Speaker::new(speaker_config()));
        talk.set_voice_installed(installed);
        let mut this = Self {
            talk,
            settings: settings.clone(),
            testing: false,
            notice: None,
            speech_download: None,
            voice_download: None,
            voice_installed: installed,
            registered: None,
            hotkey_error: None,
            sent_at: None,
            bring_forward: false,
            repaint: repaint_once(ctx).into(),
        };
        this.apply_to_speaker(settings);
        this.talk.set_speak_replies(settings.speak_replies);
        if installed {
            // Load Kokoro and open the output device now, so the first reply is quick.
            this.talk.speaker().prepare();
        }
        this
    }

    fn apply_to_speaker(&self, settings: &VoiceSettings) {
        let speaker = self.talk.speaker();
        if !speaker.set_voice_by_name(&settings.voice) {
            speaker.set_voice_by_name(DEFAULT_VOICE);
        }
        speaker.set_speed(settings.speed());
        speaker.set_volume(settings.volume());
    }

    fn notify(&mut self, notice: Notice) {
        self.notice = Some((notice, Instant::now()));
    }

    pub(super) fn owns_dictation(&self) -> bool {
        self.talk.owns_dictation()
    }

    fn start_speech_download(&mut self) {
        if self
            .speech_download
            .as_ref()
            .is_some_and(Download::is_running)
        {
            return;
        }
        let repaint = self.repaint.clone();
        log::info!("downloading the speech model from {}", models::PARAKEET_URL);
        self.speech_download = Some(models::download_parakeet(Some(Box::new(move || repaint()))));
        self.notice = None;
    }

    fn start_voice_download(&mut self) {
        if self
            .voice_download
            .as_ref()
            .is_some_and(kokoro::Fetch::is_running)
        {
            return;
        }
        let config = speaker_config();
        log::info!("downloading the King's voice from {}", config.release.url);
        let repaint = self.repaint.clone();
        self.voice_download = Some(kokoro::Fetch::start(
            config.models_root,
            config.release,
            Some(Box::new(move || repaint())),
        ));
        self.notice = None;
    }

    /// The pill's notice: a running download, else the current notice.
    fn notice_view(&self) -> Option<TalkNotice<'_>> {
        if let Some(d) = self.voice_download.as_ref().filter(|d| d.is_running()) {
            return Some(TalkNotice::Downloading {
                voice: true,
                progress: d.progress(),
            });
        }
        if let Some(d) = self.speech_download.as_ref().filter(|d| d.is_running()) {
            return Some(TalkNotice::Downloading {
                voice: false,
                progress: d.progress(),
            });
        }
        Some(match &self.notice.as_ref()?.0 {
            Notice::Info(t) => TalkNotice::Info(t),
            Notice::Error(t) => TalkNotice::Error(t),
            Notice::SpeechModelMissing => TalkNotice::SpeechModelMissing,
            Notice::VoiceMissing => TalkNotice::VoiceMissing,
            Notice::DownloadFailed { voice, error } => TalkNotice::DownloadFailed {
                voice: *voice,
                error,
            },
        })
    }

    /// Takes finished downloads: a model that arrived is prepared right away.
    fn poll_downloads(&mut self) -> bool {
        let mut speech_ready = false;
        if let Some(d) = &self.speech_download {
            match d.status() {
                DownloadStatus::Done => {
                    self.speech_download = None;
                    speech_ready = true;
                    let key = self.settings.talk_hotkey_label();
                    self.notify(Notice::Info(format!(
                        "Speech model ready: hold {key} to talk to the King"
                    )));
                }
                DownloadStatus::Failed(error) => {
                    self.speech_download = None;
                    self.notify(Notice::DownloadFailed {
                        voice: false,
                        error,
                    });
                }
                DownloadStatus::Cancelled => self.speech_download = None,
                DownloadStatus::Downloading { .. } | DownloadStatus::Extracting => {}
            }
        }
        if let Some(d) = &self.voice_download {
            match d.status() {
                DownloadStatus::Done => {
                    self.voice_download = None;
                    self.voice_installed = true;
                    self.talk.set_voice_installed(true);
                    self.talk.speaker().prepare();
                    self.notify(Notice::Info("The King's voice is ready".into()));
                }
                DownloadStatus::Failed(error) => {
                    self.voice_download = None;
                    self.notify(Notice::DownloadFailed { voice: true, error });
                }
                _ => {}
            }
        }
        speech_ready
    }

    /// The Voice section's model rows and voices.
    pub(super) fn voice_status(&self, speech_model_found: Option<bool>) -> VoiceStatus<'_> {
        let speech_model = match (&self.speech_download, speech_model_found) {
            (Some(d), _) if d.is_running() => match d.status() {
                DownloadStatus::Extracting => ModelStatus::Unpacking,
                _ => ModelStatus::Downloading(d.progress()),
            },
            (_, Some(true)) => ModelStatus::Installed("Installed (Orca's copy)".into()),
            (_, Some(false)) => ModelStatus::Installed("Installed".into()),
            (_, None) => match &self.notice {
                Some((
                    Notice::DownloadFailed {
                        voice: false,
                        error,
                    },
                    _,
                )) => ModelStatus::Failed(error.clone()),
                _ => ModelStatus::Missing,
            },
        };
        let voice_model = match &self.voice_download {
            Some(d) if d.is_running() => match d.status() {
                DownloadStatus::Extracting => ModelStatus::Unpacking,
                _ => ModelStatus::Downloading(d.progress()),
            },
            _ if self.voice_installed => {
                ModelStatus::Installed(match self.talk.speaker().status() {
                    SpeakerStatus::Ready => "Installed · loaded".into(),
                    SpeakerStatus::Loading => "Installed · loading…".into(),
                    SpeakerStatus::Failed(e) => format!("Installed · failed to load: {e}"),
                    _ => "Installed".into(),
                })
            }
            _ => match &self.notice {
                Some((Notice::DownloadFailed { voice: true, error }, _)) => {
                    ModelStatus::Failed(error.clone())
                }
                _ => ModelStatus::Missing,
            },
        };
        VoiceStatus {
            voices: self.talk.speaker().voices(),
            speech_model,
            voice_model,
            hotkey_error: self.hotkey_error.as_deref(),
        }
    }
}

impl StrongholdApp {
    /// Registers the talk hotkey and loads the speech-to-text model in the background when it
    /// is on disk (Kokoro was started in [`AppTalk::new`]). Call once after construction.
    pub(super) fn talk_startup(&mut self) {
        self.register_talk_hotkey();
        if self.dictation.machine_mut().prepare() {
            log::info!("talk: loading the speech model in the background");
        }
    }

    fn register_talk_hotkey(&mut self) {
        let wanted = self.talk.settings.talk_hotkey.clone();
        if self.talk.registered.as_deref() == Some(wanted.as_str()) {
            return;
        }
        let Some(hotkey) = &mut self.hotkey else {
            self.talk.hotkey_error = Some("Global hotkeys are unavailable".into());
            return;
        };
        let parsed = self.talk.settings.talk_hotkey();
        match hotkey.set_talk_hotkey(parsed) {
            Ok(()) => {
                self.talk.registered = Some(wanted);
                self.talk.hotkey_error = None;
            }
            Err(err) => {
                log::warn!("talk hotkey unavailable: {err:#}");
                self.talk.registered = None;
                self.talk.hotkey_error = Some(format!(
                    "{} could not be registered (another app may use it): {err:#}",
                    self.talk.settings.talk_hotkey_label()
                ));
            }
        }
    }

    /// Applies saved voice settings: voice, speed, volume, speak replies and the hotkey.
    pub(super) fn apply_voice_settings(&mut self, settings: &VoiceSettings) {
        self.talk.settings = settings.clone();
        self.talk.testing = false;
        self.talk.apply_to_speaker(settings);
        self.talk.talk.set_speak_replies(settings.speak_replies);
        self.register_talk_hotkey();
    }

    /// Says the test sentence with `draft`'s voice, speed and volume.
    pub(super) fn test_voice(&mut self, draft: &VoiceSettings) {
        self.talk.talk.stop_speaking();
        self.talk.apply_to_speaker(draft);
        self.talk.testing = true;
        self.talk.talk.speaker().speak(TEST_SENTENCE);
    }

    pub(super) fn download_voice_model(&mut self, model: VoiceModel) {
        match model {
            VoiceModel::Speech => self.talk.start_speech_download(),
            VoiceModel::Voice => self.talk.start_voice_download(),
        }
    }

    pub(super) fn talk_hotkey_label(&self) -> String {
        self.talk.settings.talk_hotkey_label()
    }

    pub(super) fn is_speaking(&self) -> bool {
        self.talk.talk.is_speaking()
    }

    pub(super) fn speak_replies(&self) -> bool {
        self.talk.talk.speak_replies()
    }

    pub(super) fn stop_speaking(&mut self) {
        self.talk.talk.stop_speaking();
    }

    /// Flips "Speak replies", saving it like the King section's switch.
    pub(super) fn toggle_speak_replies(&mut self) {
        let on = !self.whip_window.voice().speak_replies;
        let saved = self.whip_window.set_speak_replies(on);
        self.apply_voice_settings(&saved);
    }

    /// The palette's "Talk to the King": records until the hotkey or the pill's Send.
    pub(super) fn start_talk(&mut self) {
        let result = self.talk.talk.start_latched(self.dictation.machine_mut());
        if let Err(err) = result {
            self.talk_failed(&err, true);
        }
    }

    /// Esc while talk owns the dictation machine.
    pub(super) fn cancel_talk(&mut self) {
        self.talk.talk.cancel(self.dictation.machine_mut());
    }

    fn talk_failed(&mut self, err: &DictationError, focused: bool) {
        log::warn!("talk: {err}");
        if err.is_model_missing() {
            self.talk.notify(Notice::SpeechModelMissing);
            // The download needs a click: bring the window up if Karl is elsewhere.
            if !focused {
                self.bring_forward_to_king();
            }
        } else {
            self.talk.notify(Notice::Error(err.to_string()));
        }
    }

    /// Shows the window with the King's chat open.
    fn bring_forward_to_king(&mut self) {
        self.select_king();
        self.talk.bring_forward = true;
    }

    /// Runs talking to the King for this frame: hotkey events, the talk machine, his reply
    /// text into the speaker, downloads. Runs in `logic`, so also while the window is hidden.
    pub(super) fn talk_logic(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused == Some(true));
        let keys = self
            .hotkey
            .as_mut()
            .map(Hotkey::take_talk_keys)
            .unwrap_or_default();
        for key in keys {
            match key {
                TalkKey::Press => {
                    self.talk.notice = None;
                    let result = self.talk.talk.press(self.dictation.machine_mut());
                    if let Err(err) = result {
                        self.talk_failed(&err, focused);
                    }
                }
                TalkKey::Release => self.talk.talk.release(self.dictation.machine_mut()),
            }
        }
        let events = {
            let machine = self.dictation.machine_mut();
            // Split borrows: the dictation machine and the chat session are separate fields.
            let chat = self.chat.session_mut();
            self.talk.talk.tick(machine, chat)
        };
        for event in events {
            self.talk_event(event, focused);
        }
        let speak_replies = self.talk.talk.speak_replies();
        self.chat.session_mut().set_speak_replies(speak_replies);
        for speech in self.chat.session_mut().take_speech() {
            if let crate::chat::SpeechEvent::TurnStart { .. } = speech
                && self.talk.testing
            {
                self.talk.testing = false;
                let settings = self.talk.settings.clone();
                self.talk.apply_to_speaker(&settings);
            }
            if let crate::chat::SpeechEvent::Text(_) = speech
                && let Some(at) = self.talk.sent_at.take()
            {
                log::info!("talk: first reply text {:.0?} after sending", at.elapsed());
            }
            let voice_turn = matches!(speech, crate::chat::SpeechEvent::TurnStart { voice: true });
            if self.talk.talk.on_speech(&speech) == Some(TalkEvent::VoiceMissing) {
                self.talk.notify(Notice::VoiceMissing);
                if voice_turn && !focused {
                    self.bring_forward_to_king();
                }
            }
        }
        if self.talk.poll_downloads() {
            self.dictation.machine_mut().prepare();
        }
        if let Some((notice, at)) = &self.talk.notice
            && matches!(notice, Notice::Info(_) | Notice::Error(_))
        {
            let left = NOTICE_TIME.saturating_sub(at.elapsed());
            if left.is_zero() {
                self.talk.notice = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
        match self.talk.talk.phase(self.dictation.machine()) {
            TalkPhase::Listening { .. } => ctx.request_repaint_after(METER_REPAINT),
            TalkPhase::Idle => {}
            _ => ctx.request_repaint_after(BUSY_REPAINT),
        }
        if std::mem::take(&mut self.talk.bring_forward) {
            for command in [
                egui::ViewportCommand::Minimized(false),
                egui::ViewportCommand::Visible(true),
                egui::ViewportCommand::Focus,
            ] {
                ctx.send_viewport_cmd(command);
            }
        }
    }

    fn talk_event(&mut self, event: TalkEvent, focused: bool) {
        match event {
            TalkEvent::Sent(text) => {
                log::info!("talk: sent a voice message ({} chars)", text.len());
                self.talk.sent_at = Some(Instant::now());
                self.traffic.push(super::chat::karl_to_king());
                // His panel shows the conversation when Karl looks; no focus is taken.
                if self.selected.as_ref() != Some(&crate::model::NodeId::king()) {
                    self.select_king();
                }
            }
            TalkEvent::NoSpeech => self.talk.notify(Notice::Info("No speech heard".into())),
            TalkEvent::Failed(err) => self.talk_failed(&err, focused),
            TalkEvent::NotSent(text) => {
                log::warn!("talk: the King's chat was not ready; message kept as a draft");
                let draft = self.chat.draft_mut();
                if !draft.is_empty() {
                    draft.push(' ');
                }
                draft.push_str(&text);
                self.open_chat();
                self.talk.notify(Notice::Error(
                    "The King was not ready; your message is in his chat box".into(),
                ));
            }
            TalkEvent::VoiceMissing => self.talk.notify(Notice::VoiceMissing),
        }
    }

    /// Draws the pill (window focused) or the overlay (in the background), and stops him
    /// speaking on any key or click in the app. Call early in `ui`.
    pub(super) fn talk_ui(&mut self, ctx: &egui::Context) {
        if self.talk.talk.is_speaking() {
            let (key, click, escape) = ctx.input(|i| {
                let key = i
                    .events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { pressed: true, .. }));
                let click = i.pointer.any_pressed();
                (key, click, i.key_pressed(egui::Key::Escape))
            });
            if key || click {
                self.talk.talk.stop_speaking();
                if escape {
                    // Esc only stops him; it does not also close panels.
                    ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                }
            }
        }
        let focused = ctx.input(|i| i.viewport().focused == Some(true));
        let machine = self.dictation.machine();
        let phase = self.talk.talk.phase(machine);
        let hotkey = self.talk.settings.talk_hotkey_label();
        let view = TalkView {
            phase,
            level: machine.level(),
            elapsed: machine.elapsed(),
            hotkey: &hotkey,
            notice: self.talk.notice_view(),
        };
        if !view.visible() {
            return;
        }
        if !focused {
            // Only live phases and short notices float over other apps; prompts that need a
            // click wait in the window.
            let floats = phase != TalkPhase::Idle
                || matches!(
                    view.notice,
                    Some(TalkNotice::Info(_) | TalkNotice::Error(_))
                );
            if floats {
                ui_talk::overlay(ctx, &view);
            }
            return;
        }
        let Some(action) = ui_talk::pill(ctx, &view) else {
            return;
        };
        match action {
            TalkAction::Send => self.talk.talk.finish(self.dictation.machine_mut()),
            TalkAction::Cancel => self.cancel_talk(),
            TalkAction::StopSpeaking => self.stop_speaking(),
            TalkAction::DownloadSpeechModel => self.talk.start_speech_download(),
            TalkAction::DownloadVoice => self.talk.start_voice_download(),
            TalkAction::Dismiss => self.talk.notice = None,
        }
    }
}
