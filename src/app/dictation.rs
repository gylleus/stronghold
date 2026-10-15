//! The app side of ⌘E dictation: runs the push-to-talk keys, puts the transcript into the
//! focused text field (else the King's chat), and shows the pill and the model download.

use std::time::{Duration, Instant};

use eframe::egui;

use super::{StrongholdApp, repaint_once};
use crate::ui::dictation::{self as ui_dictation, DictationKey, KeyState, Pill, PillAction};
use crate::voice::dictation::{DictationError, DictationOutput, MicDictation, Phase};
use crate::voice::models::{self, Download, DownloadStatus};

/// How long a notice stays up.
const NOTICE_TIME: Duration = Duration::from_secs(6);
/// Meter refresh while recording.
const METER_REPAINT: Duration = Duration::from_millis(50);

/// What the pill shows between dictations.
#[derive(Debug, Clone, PartialEq)]
enum Notice {
    Info(String),
    Error(String),
    ModelMissing,
    DownloadFailed(String),
}

pub(super) struct AppDictation {
    machine: MicDictation,
    /// The text field focused when the recording started.
    target: Option<egui::Id>,
    notice: Option<(Notice, Instant)>,
    download: Option<Download>,
    repaint: std::sync::Arc<dyn Fn() + Send + Sync>,
}

impl AppDictation {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        Self {
            machine: MicDictation::microphone(Some(repaint_once(ctx))),
            target: None,
            notice: None,
            download: None,
            repaint: repaint_once(ctx).into(),
        }
    }

    /// The dictation machine, shared with talking to the King (one microphone, one model).
    pub(super) fn machine_mut(&mut self) -> &mut MicDictation {
        &mut self.machine
    }

    pub(super) fn machine(&self) -> &MicDictation {
        &self.machine
    }

    fn notify(&mut self, notice: Notice) {
        self.notice = Some((notice, Instant::now()));
    }

    fn fail(&mut self, err: &DictationError) {
        log::warn!("dictation: {err}");
        if err.is_model_missing() {
            self.notify(Notice::ModelMissing);
        } else {
            self.notify(Notice::Error(err.to_string()));
        }
    }

    fn start_download(&mut self) {
        if self.download.as_ref().is_some_and(Download::is_running) {
            return;
        }
        let repaint = self.repaint.clone();
        log::info!("downloading the speech model from {}", models::PARAKEET_URL);
        self.download = Some(models::download_parakeet(Some(Box::new(move || repaint()))));
        self.notice = None;
    }

    /// The pill for the current state, if one shows.
    fn pill(&self) -> Option<Pill<'_>> {
        match self.machine.phase() {
            Phase::Recording { latched } => {
                return Some(Pill::Listening {
                    level: self.machine.level(),
                    elapsed: self.machine.elapsed(),
                    latched,
                });
            }
            Phase::Transcribing => return Some(Pill::Transcribing),
            Phase::Idle => {}
        }
        if let Some(download) = &self.download {
            match download.status() {
                DownloadStatus::Downloading { .. } => {
                    return Some(Pill::Downloading {
                        progress: download.progress(),
                        unpacking: false,
                    });
                }
                DownloadStatus::Extracting => {
                    return Some(Pill::Downloading {
                        progress: None,
                        unpacking: true,
                    });
                }
                DownloadStatus::Failed(_) => {
                    // Shown from `notice`, which `poll_download` sets.
                }
                DownloadStatus::Done | DownloadStatus::Cancelled => {}
            }
        }
        match &self.notice {
            Some((Notice::Info(text), _)) => Some(Pill::Notice { text, error: false }),
            Some((Notice::Error(text), _)) => Some(Pill::Notice { text, error: true }),
            Some((Notice::ModelMissing, _)) => Some(Pill::ModelMissing),
            Some((Notice::DownloadFailed(err), _)) => Some(Pill::DownloadFailed(err)),
            None => None,
        }
    }

    fn poll_download(&mut self) {
        let Some(download) = &self.download else {
            return;
        };
        match download.status() {
            DownloadStatus::Done => {
                self.download = None;
                self.notify(Notice::Info(
                    "Speech model ready: hold ⌘E to dictate".to_owned(),
                ));
            }
            DownloadStatus::Failed(err) => {
                self.download = None;
                self.notify(Notice::DownloadFailed(err));
            }
            DownloadStatus::Cancelled => self.download = None,
            DownloadStatus::Downloading { .. } | DownloadStatus::Extracting => {}
        }
    }
}

impl StrongholdApp {
    /// Recording alone lights the stream: transcription, errors and cancellation discharge it.
    pub(super) fn sync_voice_target(&mut self, ctx: &egui::Context) {
        let recipient = if self.dictation.machine.is_recording() && !self.talk.owns_dictation() {
            self.dictation_destination(ctx).node()
        } else {
            None
        };
        let target = recording_target(
            self.dictation.machine.phase(),
            self.talk.owns_dictation(),
            recipient.as_ref(),
        );
        self.farm.set_voice_target(target, ctx.input(|i| i.time));
    }

    /// Runs dictation for this frame: takes ⌘E/Esc before other shortcuts see them, delivers a
    /// finished transcript and draws the pill. Call first in `ui`.
    pub(super) fn dictation_frame(&mut self, ctx: &egui::Context) {
        if self.talk.owns_dictation() {
            // Talking to the King has the machine: his talk code ticks it, and the window
            // is usually not focused, so no focus-loss release either. Esc still cancels.
            let busy = KeyState {
                holding: false,
                busy: true,
            };
            for key in ui_dictation::take_frame_keys(ctx, busy) {
                if key == DictationKey::Cancel {
                    self.cancel_talk();
                }
            }
            return;
        }
        let phase = self.dictation.machine.phase();
        let mut state = KeyState {
            holding: phase == (Phase::Recording { latched: false }),
            busy: phase != Phase::Idle,
        };
        // A window that loses focus never sees the key-up.
        if state.holding && !ctx.input(|i| i.focused) {
            self.dictation.machine.release();
            state = KeyState::default();
        }
        for key in ui_dictation::take_frame_keys(ctx, state) {
            self.dictation_key(ctx, key);
        }
        if let Some(output) = self.dictation.machine.tick() {
            match output {
                DictationOutput::Text(text) if text.is_empty() => {
                    self.dictation.target = None;
                    self.dictation
                        .notify(Notice::Info("No speech heard".to_owned()));
                }
                DictationOutput::Text(text) => self.deliver_dictation(ctx, &text),
                DictationOutput::Failed(err) => {
                    self.dictation.target = None;
                    self.dictation.fail(&err);
                }
            }
        }
        self.dictation.poll_download();
        if let Some((notice, at)) = &self.dictation.notice
            && matches!(notice, Notice::Info(_) | Notice::Error(_))
        {
            let left = NOTICE_TIME.saturating_sub(at.elapsed());
            if left.is_zero() {
                self.dictation.notice = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
        match self.dictation.machine.phase() {
            Phase::Recording { .. } => ctx.request_repaint_after(METER_REPAINT),
            Phase::Transcribing => ctx.request_repaint_after(Duration::from_millis(100)),
            Phase::Idle => {}
        }
        let Some(pill) = self.dictation.pill() else {
            return;
        };
        let Some(action) = ui_dictation::pill(ctx, &pill) else {
            return;
        };
        let d = &mut self.dictation;
        match action {
            PillAction::Stop => d.machine.finish(),
            PillAction::Cancel => {
                d.machine.cancel();
                d.target = None;
            }
            PillAction::Download => d.start_download(),
            PillAction::CancelDownload => {
                if let Some(download) = &d.download {
                    download.cancel();
                }
            }
            PillAction::Dismiss => d.notice = None,
        }
    }

    fn dictation_key(&mut self, ctx: &egui::Context, key: DictationKey) {
        let d = &mut self.dictation;
        match key {
            DictationKey::Press { repeat } => {
                let starting = !repeat && d.machine.phase() == Phase::Idle;
                if starting {
                    d.target = ctx.memory(|m| m.focused());
                    d.notice = None;
                }
                if let Err(err) = d.machine.press(repeat) {
                    d.target = None;
                    d.fail(&err);
                }
            }
            DictationKey::Release => d.machine.release(),
            DictationKey::Cancel => {
                d.machine.cancel();
                d.target = None;
            }
        }
    }

    /// Starts a dictation from the palette: it records until ⌘E or the pill's Stop, and the
    /// text goes to the King's chat (the palette's own field is gone by then).
    pub(super) fn start_dictation(&mut self) {
        let d = &mut self.dictation;
        d.target = None;
        d.notice = None;
        if let Err(err) = d.machine.start_latched() {
            d.fail(&err);
        }
    }

    /// Puts `text` into the field focused when dictation started: the chat's or a terminal's
    /// input at its cursor, any other focused field as typed text, else the King's chat.
    fn deliver_dictation(&mut self, ctx: &egui::Context, text: &str) {
        let destination = self.dictation_destination(ctx);
        self.dictation.target = None;
        match destination {
            Destination::King { open } => {
                if open {
                    self.open_chat();
                }
                ui_dictation::insert_at_cursor(
                    ctx,
                    crate::ui::chat::input_id(),
                    self.chat.draft_mut(),
                    text,
                );
            }
            Destination::Terminal { handle, .. } => {
                ui_dictation::insert_at_cursor(
                    ctx,
                    crate::ui::terminal::input_id(&handle),
                    self.input.draft_mut(&handle),
                    text,
                );
            }
            Destination::Field => ui_dictation::type_into_focused(ctx, text),
        }
    }

    /// Share the existing transcript routing with the hold, so changing focus or selection
    /// cannot make the stream promise a different recipient from the one that gets the text.
    fn dictation_destination(&self, ctx: &egui::Context) -> Destination {
        let focused = ctx.memory(|m| m.focused());
        let terminal = self.selected.as_ref().and_then(|id| {
            let node = self.snapshot.as_ref()?.node(id)?;
            Some((id, crate::ui::terminal::watch_handle(node)?))
        });
        destination(
            self.dictation.target.or(focused),
            focused,
            ctx.text_edit_focused(),
            terminal,
        )
    }
}

#[derive(Debug, PartialEq)]
enum Destination {
    King {
        open: bool,
    },
    Terminal {
        node: crate::model::NodeId,
        handle: String,
    },
    Field,
}

impl Destination {
    fn node(&self) -> Option<crate::model::NodeId> {
        match self {
            Self::King { .. } => Some(crate::model::NodeId::king()),
            Self::Terminal { node, .. } => Some(node.clone()),
            Self::Field => None,
        }
    }
}

fn destination(
    target: Option<egui::Id>,
    focused: Option<egui::Id>,
    text_edit_focused: bool,
    terminal: Option<(&crate::model::NodeId, &str)>,
) -> Destination {
    match target {
        Some(id) if id == crate::ui::chat::input_id() => Destination::King { open: false },
        Some(id) if terminal.is_some_and(|(_, h)| crate::ui::terminal::input_id(h) == id) => {
            let (node, handle) = terminal.expect("matched terminal input");
            Destination::Terminal {
                node: node.clone(),
                handle: handle.to_owned(),
            }
        }
        Some(id) if focused == Some(id) && text_edit_focused => Destination::Field,
        _ => Destination::King { open: true },
    }
}

fn recording_target(
    phase: Phase,
    king_owns: bool,
    recipient: Option<&crate::model::NodeId>,
) -> Option<crate::model::NodeId> {
    if !matches!(phase, Phase::Recording { .. }) {
        return None;
    }
    if king_owns {
        Some(crate::model::NodeId::king())
    } else {
        recipient.cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeId;

    #[test]
    fn stream_and_transcript_share_focused_terminal_chat_and_fallback_routes() {
        let dwarf = NodeId("term:worker".into());
        let terminal = Some((&dwarf, "worker"));
        let input = crate::ui::terminal::input_id("worker");
        assert_eq!(
            destination(Some(input), None, false, terminal).node(),
            Some(dwarf)
        );
        let chat = crate::ui::chat::input_id();
        assert_eq!(
            destination(Some(chat), Some(chat), true, None),
            Destination::King { open: false }
        );
        assert_eq!(
            destination(None, None, false, None),
            Destination::King { open: true }
        );
        let other = egui::Id::new("settings field");
        assert_eq!(
            destination(Some(other), Some(other), true, None).node(),
            None
        );
        // A closed/reselected terminal follows the existing fallback, not its old dwarf.
        assert_eq!(
            destination(Some(input), None, false, None),
            Destination::King { open: true }
        );
    }

    #[test]
    fn only_recording_lights_the_captured_recipient_and_king_talk_overrides_it() {
        let dwarf = NodeId("term:worker".into());
        for latched in [false, true] {
            let phase = Phase::Recording { latched };
            assert_eq!(
                recording_target(phase, false, Some(&dwarf)),
                Some(dwarf.clone())
            );
            assert_eq!(
                recording_target(phase, true, Some(&dwarf)),
                Some(NodeId::king())
            );
            assert_eq!(recording_target(phase, false, None), None);
        }
        for phase in [Phase::Idle, Phase::Transcribing] {
            for king_owns in [false, true] {
                assert_eq!(recording_target(phase, king_owns, Some(&dwarf)), None);
            }
        }
    }
}
