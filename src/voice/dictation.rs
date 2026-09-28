//! Push-to-talk dictation: press starts recording, release stops it and transcribes, and the
//! text comes out of [`Dictation::tick`]. Where the text goes is up to the caller, so the same
//! machine serves text fields and voice commands.

use std::time::{Duration, Instant};

use super::audio_in::{RecordError, Recorder, Recording};
use super::models;
use super::stt::{SttError, Transcriber, Transcript};

/// A microphone as the dictation machine drives it ([`Recorder`] in the app).
pub trait Capture {
    fn start(&mut self);
    fn stop(&mut self) -> Result<Recording, RecordError>;
    fn cancel(&mut self);
    fn level(&self) -> f32;
    fn is_full(&self) -> bool;
    fn failure(&self) -> Option<RecordError>;
}

impl Capture for Recorder {
    fn start(&mut self) {
        Recorder::start(self);
    }
    fn stop(&mut self) -> Result<Recording, RecordError> {
        Recorder::stop(self)
    }
    fn cancel(&mut self) {
        Recorder::cancel(self);
    }
    fn level(&self) -> f32 {
        Recorder::level(self)
    }
    fn is_full(&self) -> bool {
        Recorder::is_full(self)
    }
    fn failure(&self) -> Option<RecordError> {
        Recorder::failure(self)
    }
}

/// A background recognizer as the dictation machine drives it ([`Transcriber`] in the app).
pub trait Recognize {
    /// Whether the model is on disk (checked before recording, so a missing model is reported
    /// up front).
    fn model_available(&self) -> bool;
    /// Loads the model in the background.
    fn warm(&mut self);
    fn submit(&mut self, samples: Vec<f32>);
    fn poll(&mut self) -> Option<Result<Transcript, SttError>>;
}

impl Recognize for Transcriber {
    fn model_available(&self) -> bool {
        models::find_parakeet().is_some()
    }
    fn warm(&mut self) {
        Transcriber::warm(self);
    }
    fn submit(&mut self, samples: Vec<f32>) {
        Transcriber::submit(self, samples);
    }
    fn poll(&mut self) -> Option<Result<Transcript, SttError>> {
        Transcriber::poll(self)
    }
}

/// Why a dictation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DictationError {
    #[error("{0}")]
    Record(RecordError),
    #[error("{0}")]
    Stt(SttError),
}

impl DictationError {
    /// The model is missing, so the UI should offer the download.
    pub fn is_model_missing(&self) -> bool {
        matches!(self, Self::Stt(SttError::ModelMissing))
    }
}

/// What a dictation produced.
#[derive(Debug, Clone, PartialEq)]
pub enum DictationOutput {
    /// The transcript; empty when nothing was said.
    Text(String),
    Failed(DictationError),
}

/// What the machine is doing, for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Recording; `latched` recordings (started from a button) stop on the next press
    /// instead of on release.
    Recording {
        latched: bool,
    },
    Transcribing,
}

/// The dictation state machine.
pub struct Dictation<C, R> {
    capture: C,
    recognizer: R,
    phase: Phase,
    since: Option<Instant>,
    warmed: bool,
    /// Results still due from the recognizer for cancelled transcriptions.
    discard: usize,
    /// A failure to report on the next tick.
    pending_error: Option<DictationError>,
}

/// The app's dictation: the default microphone and Parakeet.
pub type MicDictation = Dictation<Recorder, Transcriber>;

impl MicDictation {
    pub fn microphone(notify: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        Self::new(Recorder::microphone(), Transcriber::parakeet(notify))
    }
}

impl<C: Capture, R: Recognize> Dictation<C, R> {
    pub fn new(capture: C, recognizer: R) -> Self {
        Self {
            capture,
            recognizer,
            phase: Phase::Idle,
            since: None,
            warmed: false,
            discard: 0,
            pending_error: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording { .. })
    }

    /// Input level in 0..=1 while recording.
    pub fn level(&self) -> f32 {
        if self.is_recording() {
            self.capture.level()
        } else {
            0.0
        }
    }

    /// Time since recording started.
    pub fn elapsed(&self) -> Duration {
        self.since.map_or(Duration::ZERO, |s| s.elapsed())
    }

    /// The push-to-talk key went down. Key repeats are ignored; a press while a latched
    /// recording runs finishes it. Fails at once when the model is missing.
    pub fn press(&mut self, repeat: bool) -> Result<(), DictationError> {
        if repeat {
            return Ok(());
        }
        match self.phase {
            Phase::Idle => self.begin(false),
            Phase::Recording { latched: true } => {
                self.finish();
                Ok(())
            }
            Phase::Recording { latched: false } | Phase::Transcribing => Ok(()),
        }
    }

    /// The push-to-talk key came up: stop recording and transcribe.
    pub fn release(&mut self) {
        if self.phase == (Phase::Recording { latched: false }) {
            self.finish();
        }
    }

    /// Starts a recording that runs until [`press`](Self::press) or [`finish`](Self::finish),
    /// for a button or palette entry.
    pub fn start_latched(&mut self) -> Result<(), DictationError> {
        match self.phase {
            Phase::Idle => self.begin(true),
            _ => Ok(()),
        }
    }

    fn begin(&mut self, latched: bool) -> Result<(), DictationError> {
        if !self.recognizer.model_available() {
            return Err(DictationError::Stt(SttError::ModelMissing));
        }
        if !self.warmed {
            self.recognizer.warm();
            self.warmed = true;
        }
        self.capture.start();
        self.phase = Phase::Recording { latched };
        self.since = Some(Instant::now());
        Ok(())
    }

    /// Stops recording and hands the audio to the recognizer.
    pub fn finish(&mut self) {
        if !self.is_recording() {
            return;
        }
        self.since = None;
        match self.capture.stop() {
            Ok(recording) => {
                self.recognizer.submit(recording.samples);
                self.phase = Phase::Transcribing;
            }
            Err(err) => {
                self.phase = Phase::Idle;
                self.pending_error = Some(DictationError::Record(err));
            }
        }
    }

    /// Drops the recording or the transcription in flight. Returns whether there was one.
    pub fn cancel(&mut self) -> bool {
        match self.phase {
            Phase::Idle => false,
            Phase::Recording { .. } => {
                self.capture.cancel();
                self.since = None;
                self.phase = Phase::Idle;
                true
            }
            Phase::Transcribing => {
                self.discard += 1;
                self.phase = Phase::Idle;
                true
            }
        }
    }

    /// Advances the machine: stops a recording that failed or hit its length limit and
    /// collects the transcript. Call every frame.
    pub fn tick(&mut self) -> Option<DictationOutput> {
        if let Some(err) = self.pending_error.take() {
            return Some(DictationOutput::Failed(err));
        }
        if self.is_recording() {
            if let Some(err) = self.capture.failure() {
                self.capture.cancel();
                self.since = None;
                self.phase = Phase::Idle;
                return Some(DictationOutput::Failed(DictationError::Record(err)));
            }
            if self.capture.is_full() {
                self.finish();
            }
        }
        while let Some(result) = self.recognizer.poll() {
            if self.discard > 0 {
                self.discard -= 1;
                continue;
            }
            if self.phase == Phase::Transcribing {
                self.phase = Phase::Idle;
            }
            return Some(match result {
                Ok(transcript) => {
                    log::info!(
                        "dictation: {:.1?} of audio transcribed in {:.0?}{}",
                        transcript.audio,
                        transcript.took,
                        transcript
                            .load
                            .map(|l| format!(" (model loaded in {l:.1?})"))
                            .unwrap_or_default()
                    );
                    DictationOutput::Text(transcript.text)
                }
                Err(err) => DictationOutput::Failed(DictationError::Stt(err)),
            });
        }
        if let Some(err) = self.pending_error.take() {
            return Some(DictationOutput::Failed(err));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    #[derive(Default)]
    struct Log(Rc<RefCell<Vec<String>>>);

    impl Log {
        fn push(&self, s: impl Into<String>) {
            self.0.borrow_mut().push(s.into());
        }
    }

    struct FakeCapture {
        log: Log,
        samples: usize,
        stop_error: Option<RecordError>,
        failure: Option<RecordError>,
        full: bool,
    }

    impl Capture for FakeCapture {
        fn start(&mut self) {
            self.log.push("start");
        }
        fn stop(&mut self) -> Result<Recording, RecordError> {
            self.log.push("stop");
            match self.stop_error.take() {
                Some(err) => Err(err),
                None => Ok(Recording {
                    samples: vec![0.1; self.samples],
                    truncated: false,
                }),
            }
        }
        fn cancel(&mut self) {
            self.log.push("cancel");
        }
        fn level(&self) -> f32 {
            0.5
        }
        fn is_full(&self) -> bool {
            self.full
        }
        fn failure(&self) -> Option<RecordError> {
            self.failure.clone()
        }
    }

    struct FakeRecognizer {
        log: Log,
        available: bool,
        /// Answers released by the test, in submit order.
        ready: VecDeque<Result<Transcript, SttError>>,
    }

    impl Recognize for FakeRecognizer {
        fn model_available(&self) -> bool {
            self.available
        }
        fn warm(&mut self) {
            self.log.push("warm");
        }
        fn submit(&mut self, samples: Vec<f32>) {
            self.log.push(format!("submit {}", samples.len()));
        }
        fn poll(&mut self) -> Option<Result<Transcript, SttError>> {
            self.ready.pop_front()
        }
    }

    fn transcript(text: &str) -> Result<Transcript, SttError> {
        Ok(Transcript {
            text: text.into(),
            audio: Duration::from_secs(1),
            took: Duration::from_millis(10),
            load: None,
        })
    }

    fn machine() -> (
        Dictation<FakeCapture, FakeRecognizer>,
        Rc<RefCell<Vec<String>>>,
    ) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let d = Dictation::new(
            FakeCapture {
                log: Log(log.clone()),
                samples: 16_000,
                stop_error: None,
                failure: None,
                full: false,
            },
            FakeRecognizer {
                log: Log(log.clone()),
                available: true,
                ready: VecDeque::new(),
            },
        );
        (d, log)
    }

    #[test]
    fn press_record_release_transcribe_text() {
        let (mut d, log) = machine();
        assert_eq!(d.phase(), Phase::Idle);
        d.press(false).unwrap();
        assert_eq!(d.phase(), Phase::Recording { latched: false });
        assert_eq!(d.level(), 0.5);
        assert_eq!(d.tick(), None);
        d.release();
        assert_eq!(d.phase(), Phase::Transcribing);
        assert_eq!(d.level(), 0.0);
        assert_eq!(d.tick(), None, "still transcribing");
        d.recognizer
            .ready
            .push_back(transcript("Show me the stalled agents."));
        assert_eq!(
            d.tick(),
            Some(DictationOutput::Text("Show me the stalled agents.".into()))
        );
        assert_eq!(d.phase(), Phase::Idle);
        assert_eq!(*log.borrow(), ["warm", "start", "stop", "submit 16000"]);

        // The model is warmed once.
        d.press(false).unwrap();
        d.release();
        assert_eq!(log.borrow()[4..], ["start", "stop", "submit 16000"]);
    }

    #[test]
    fn key_repeats_and_extra_presses_are_ignored() {
        let (mut d, log) = machine();
        d.press(true).unwrap();
        assert_eq!(d.phase(), Phase::Idle, "a repeat never starts a recording");
        d.press(false).unwrap();
        for _ in 0..5 {
            d.press(true).unwrap();
        }
        d.press(false).unwrap();
        assert_eq!(d.phase(), Phase::Recording { latched: false });
        d.release();
        d.release();
        d.press(false).unwrap();
        assert_eq!(
            d.phase(),
            Phase::Transcribing,
            "no new recording while transcribing"
        );
        assert_eq!(*log.borrow(), ["warm", "start", "stop", "submit 16000"]);
    }

    #[test]
    fn cancel_while_recording_or_transcribing() {
        let (mut d, log) = machine();
        assert!(!d.cancel());
        d.press(false).unwrap();
        assert!(d.cancel());
        assert_eq!(d.phase(), Phase::Idle);
        d.release();
        assert_eq!(
            d.phase(),
            Phase::Idle,
            "a release after cancel does nothing"
        );
        assert_eq!(log.borrow()[2], "cancel");

        // A cancelled transcription's result is dropped; the next one comes through.
        d.press(false).unwrap();
        d.release();
        assert!(d.cancel());
        d.press(false).unwrap();
        d.release();
        d.recognizer.ready.push_back(transcript("old"));
        d.recognizer.ready.push_back(transcript("new"));
        assert_eq!(d.tick(), Some(DictationOutput::Text("new".into())));
        assert_eq!(d.tick(), None);
    }

    #[test]
    fn missing_model_fails_before_recording() {
        let (mut d, log) = machine();
        d.recognizer.available = false;
        let err = d.press(false).unwrap_err();
        assert!(err.is_model_missing());
        assert_eq!(d.phase(), Phase::Idle);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn recording_errors_come_out_of_tick() {
        let (mut d, _) = machine();
        d.press(false).unwrap();
        d.capture.failure = Some(RecordError::NoInputDevice);
        assert_eq!(
            d.tick(),
            Some(DictationOutput::Failed(DictationError::Record(
                RecordError::NoInputDevice
            )))
        );
        assert_eq!(d.phase(), Phase::Idle);

        d.capture.failure = None;
        d.capture.stop_error = Some(RecordError::PermissionDenied);
        d.press(false).unwrap();
        d.release();
        assert_eq!(d.phase(), Phase::Idle);
        assert_eq!(
            d.tick(),
            Some(DictationOutput::Failed(DictationError::Record(
                RecordError::PermissionDenied
            )))
        );

        d.press(false).unwrap();
        d.release();
        d.recognizer
            .ready
            .push_back(Err(SttError::LoadFailed("bad".into())));
        assert!(matches!(
            d.tick(),
            Some(DictationOutput::Failed(DictationError::Stt(
                SttError::LoadFailed(_)
            )))
        ));
    }

    #[test]
    fn latched_recording_stops_on_the_next_press_or_when_full() {
        let (mut d, _) = machine();
        d.start_latched().unwrap();
        d.release();
        assert_eq!(d.phase(), Phase::Recording { latched: true });
        d.press(false).unwrap();
        assert_eq!(d.phase(), Phase::Transcribing);

        let (mut d, _) = machine();
        d.press(false).unwrap();
        d.capture.full = true;
        assert_eq!(d.tick(), None);
        assert_eq!(
            d.phase(),
            Phase::Transcribing,
            "a full recording stops by itself"
        );
    }
}
