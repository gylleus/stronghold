//! Talking to the King: hold the talk hotkey to record, release to transcribe and send the
//! text to his chat as a voice message, and hear his reply as it streams in.
//!
//! [`Talk`] is the state machine. It borrows the app's one [`Dictation`] machine (one
//! microphone, one loaded Parakeet model) for the recording, so while it owns a recording the
//! ⌘E dictation code must leave that machine alone (see [`Talk::owns_dictation`]). The chat
//! and the speaker sit behind [`KingChat`] and [`Say`], so tests drive it with fakes.
//!
//! Behaviour:
//! - A press stops him speaking (and silences the rest of that reply) and starts recording.
//!   A press that only interrupted him and heard nothing sends nothing and says nothing.
//! - A transcript is sent as soon as the chat can take it. If he is mid-turn, that turn is
//!   interrupted first: the user talking over the King is a new request, like in a conversation, and a
//!   turn waiting on a permission card would otherwise never end. If the chat does not
//!   become ready within [`SEND_TIMEOUT`], the text comes back as [`TalkEvent::NotSent`].
//! - Replies to voice messages are always spoken; other replies only with "Speak replies".
//!   Only the reply's `<say>…</say>` line is spoken (see [`SayFilter`]), or without one its
//!   first sentences; never tool calls. A new turn stops the old reply.
//! - Nothing here downloads anything: without the voice model a reply that should be spoken
//!   produces [`TalkEvent::VoiceMissing`] instead.

use std::time::{Duration, Instant};

use super::dictation::{Capture, Dictation, DictationError, DictationOutput, Phase, Recognize};
use super::tts::{SentenceStream, Speaker};
use crate::chat::say::{FALLBACK_SENTENCES, SayFilter, Spoken};
use crate::chat::{ChatSession, SessionState, SpeechEvent};

/// How long a transcript waits for the chat before it is given back.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// The speaker as the talk machine drives it ([`Speaker`] in the app).
pub trait Say {
    fn enqueue(&self, sentence: String);
    fn stop(&self);
    fn is_speaking(&self) -> bool;
}

impl Say for Speaker {
    fn enqueue(&self, sentence: String) {
        Speaker::enqueue(self, sentence);
    }
    fn stop(&self) {
        Speaker::stop(self);
    }
    fn is_speaking(&self) -> bool {
        Speaker::is_speaking(self)
    }
}

/// The King's chat as the talk machine drives it ([`ChatSession`] in the app).
pub trait KingChat {
    /// Ready and not in a turn.
    fn can_send(&self) -> bool;
    /// A turn is running.
    fn is_running(&self) -> bool;
    /// The process is up (a turn can be interrupted and will end).
    fn is_up(&self) -> bool;
    fn send_voice(&mut self, text: &str) -> bool;
    fn interrupt(&mut self);
}

impl KingChat for ChatSession {
    fn can_send(&self) -> bool {
        ChatSession::can_send(self)
    }
    fn is_running(&self) -> bool {
        self.transcript.running
    }
    fn is_up(&self) -> bool {
        self.state == SessionState::Ready
    }
    fn send_voice(&mut self, text: &str) -> bool {
        ChatSession::send_voice(self, text)
    }
    fn interrupt(&mut self) {
        ChatSession::interrupt(self);
    }
}

/// What the machine is doing, for the overlay and pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TalkPhase {
    Idle,
    /// Recording; `latched` (started from the palette) stops on the next press.
    Listening {
        latched: bool,
    },
    Transcribing,
    /// The transcript waits for the King to finish or start.
    Waiting,
    /// His reply to a voice message is on its way; nothing to hear yet.
    Thinking,
    Speaking,
}

/// Something the app should show or act on.
#[derive(Debug, Clone, PartialEq)]
pub enum TalkEvent {
    /// The voice message went to the chat.
    Sent(String),
    /// The recording had no words in it.
    NoSpeech,
    Failed(DictationError),
    /// The chat never became ready; the text is handed back so it is not lost.
    NotSent(String),
    /// A reply should be spoken but the voice model is not installed.
    VoiceMissing,
}

/// The pending transcript.
#[derive(Debug)]
struct Pending {
    text: String,
    since: Instant,
    interrupted: bool,
}

/// How the current turn's reply is spoken.
#[derive(Debug, Default)]
struct ReplySpeech {
    /// The turn's reply is spoken at all.
    speak: bool,
    /// The turn started with a voice message.
    voice: bool,
    /// The user stopped the King: the rest of this reply stays silent.
    muted: bool,
    /// The turn is still running.
    running: bool,
    /// Picks the say-line out of the reply.
    filter: SayFilter,
    stream: SentenceStream,
    /// Sentences handed to the speaker.
    spoken: usize,
}

/// The talk-to-the-King state machine.
pub struct Talk<S> {
    speaker: S,
    /// The dictation machine's recording or transcription is ours.
    active: bool,
    /// The current recording began by interrupting his speech.
    began_by_interrupting: bool,
    pending: Option<Pending>,
    reply: Option<ReplySpeech>,
    speak_replies: bool,
    voice_installed: bool,
    send_timeout: Duration,
}

impl<S: Say> Talk<S> {
    pub fn new(speaker: S) -> Self {
        Self {
            speaker,
            active: false,
            began_by_interrupting: false,
            pending: None,
            reply: None,
            speak_replies: false,
            voice_installed: true,
            send_timeout: SEND_TIMEOUT,
        }
    }

    pub fn speaker(&self) -> &S {
        &self.speaker
    }

    /// Speak every reply, not only replies to voice messages.
    pub fn set_speak_replies(&mut self, on: bool) {
        self.speak_replies = on;
    }

    pub fn speak_replies(&self) -> bool {
        self.speak_replies
    }

    /// Whether the voice model is on disk; without it nothing is spoken.
    pub fn set_voice_installed(&mut self, installed: bool) {
        self.voice_installed = installed;
    }

    #[cfg(test)]
    fn set_send_timeout(&mut self, timeout: Duration) {
        self.send_timeout = timeout;
    }

    /// The dictation machine is recording or transcribing for us: ⌘E dictation must neither
    /// tick nor release it.
    pub fn owns_dictation(&self) -> bool {
        self.active
    }

    /// The talk hotkey went down.
    pub fn press<C: Capture, R: Recognize>(
        &mut self,
        dictation: &mut Dictation<C, R>,
    ) -> Result<(), DictationError> {
        if self.active {
            // A second press finishes a latched recording; a repeat is ignored.
            if dictation.phase() == (Phase::Recording { latched: true }) {
                dictation.finish();
            }
            return Ok(());
        }
        let interrupted = self.stop_speaking();
        if dictation.phase() != Phase::Idle {
            // ⌘E dictation is using the microphone.
            return Ok(());
        }
        dictation.press(false)?;
        self.active = true;
        self.began_by_interrupting = interrupted;
        Ok(())
    }

    /// The talk hotkey came up: stop recording and transcribe.
    pub fn release<C: Capture, R: Recognize>(&mut self, dictation: &mut Dictation<C, R>) {
        if self.active {
            dictation.release();
        }
    }

    /// Starts a recording that runs until the next press or [`finish`](Self::finish) (the
    /// palette's "Talk to the King").
    pub fn start_latched<C: Capture, R: Recognize>(
        &mut self,
        dictation: &mut Dictation<C, R>,
    ) -> Result<(), DictationError> {
        if self.active || dictation.phase() != Phase::Idle {
            return Ok(());
        }
        self.stop_speaking();
        dictation.start_latched()?;
        self.active = true;
        self.began_by_interrupting = false;
        Ok(())
    }

    /// Stops a latched recording and transcribes it.
    pub fn finish<C: Capture, R: Recognize>(&mut self, dictation: &mut Dictation<C, R>) {
        if self.active {
            dictation.finish();
        }
    }

    /// Drops the recording, transcription or unsent message. Returns whether there was one.
    pub fn cancel<C: Capture, R: Recognize>(&mut self, dictation: &mut Dictation<C, R>) -> bool {
        let mut cancelled = self.pending.take().is_some();
        if self.active {
            cancelled |= dictation.cancel();
            self.active = false;
        }
        cancelled
    }

    /// Stops him speaking now and keeps the rest of this reply silent. Returns whether he was
    /// speaking.
    pub fn stop_speaking(&mut self) -> bool {
        let speaking = self.speaker.is_speaking();
        self.speaker.stop();
        if let Some(reply) = &mut self.reply {
            reply.muted = true;
        }
        speaking
    }

    pub fn is_speaking(&self) -> bool {
        self.speaker.is_speaking()
    }

    /// Advances the machine: collects the transcript and sends it when the chat can take it.
    /// Call every frame (also while the window is hidden).
    pub fn tick<C: Capture, R: Recognize, Q: KingChat + ?Sized>(
        &mut self,
        dictation: &mut Dictation<C, R>,
        chat: &mut Q,
    ) -> Vec<TalkEvent> {
        let mut events = Vec::new();
        if self.active {
            match dictation.tick() {
                Some(DictationOutput::Text(text)) => {
                    self.active = false;
                    if text.trim().is_empty() {
                        if !self.began_by_interrupting {
                            events.push(TalkEvent::NoSpeech);
                        }
                    } else {
                        self.queue(text);
                    }
                }
                Some(DictationOutput::Failed(err)) => {
                    self.active = false;
                    events.push(TalkEvent::Failed(err));
                }
                // Cancelled from elsewhere (the dictation pill or Esc).
                None if dictation.phase() == Phase::Idle => self.active = false,
                None => {}
            }
        }
        if let Some(pending) = &mut self.pending {
            if chat.can_send() {
                let text = std::mem::take(&mut pending.text);
                self.pending = None;
                if chat.send_voice(&text) {
                    events.push(TalkEvent::Sent(text));
                } else {
                    events.push(TalkEvent::NotSent(text));
                }
            } else if chat.is_running() && chat.is_up() && !pending.interrupted {
                chat.interrupt();
                pending.interrupted = true;
            } else if pending.since.elapsed() >= self.send_timeout {
                let text = std::mem::take(&mut pending.text);
                self.pending = None;
                events.push(TalkEvent::NotSent(text));
            }
        }
        events
    }

    fn queue(&mut self, text: String) {
        match &mut self.pending {
            // Said twice before he could take the first: send both.
            Some(pending) => {
                pending.text.push(' ');
                pending.text.push_str(&text);
            }
            None => {
                self.pending = Some(Pending {
                    text,
                    since: Instant::now(),
                    interrupted: false,
                });
            }
        }
    }

    /// Feeds one chat speech event (turn starts and reply text, in order).
    pub fn on_speech(&mut self, event: &SpeechEvent) -> Option<TalkEvent> {
        match event {
            SpeechEvent::TurnStart { voice } => {
                self.speaker.stop();
                let wanted = *voice || self.speak_replies;
                let speak = wanted && self.voice_installed;
                self.reply = Some(ReplySpeech {
                    speak,
                    voice: *voice,
                    running: true,
                    ..ReplySpeech::default()
                });
                return (wanted && !speak).then_some(TalkEvent::VoiceMissing);
            }
            SpeechEvent::Text(delta) => {
                let reply = self.reply.as_mut()?;
                let spoken = reply.filter.push(delta);
                self.speak_filtered(spoken);
            }
            SpeechEvent::BlockEnd => {
                let reply = self.reply.as_mut()?;
                let spoken = reply.filter.block_end();
                self.speak_filtered(spoken);
            }
            SpeechEvent::TurnEnd => {
                let reply = self.reply.as_mut()?;
                let spoken = reply.filter.turn_end();
                reply.running = false;
                self.speak_filtered(spoken);
            }
        }
        None
    }

    /// Turns the filter's output into sentences for the speaker.
    fn speak_filtered(&mut self, spoken: Spoken) {
        let Some(reply) = &mut self.reply else {
            return;
        };
        let mut sentences = reply.stream.push(&spoken.text);
        if spoken.flush {
            sentences.extend(reply.stream.finish());
        }
        self.say(sentences);
    }

    fn say(&mut self, sentences: Vec<String>) {
        let Some(reply) = &mut self.reply else {
            return;
        };
        if !reply.speak || reply.muted {
            return;
        }
        for sentence in sentences {
            if reply.filter.is_fallback() && reply.spoken >= FALLBACK_SENTENCES {
                return;
            }
            reply.spoken += 1;
            self.speaker.enqueue(sentence);
        }
    }

    /// What the machine is doing now.
    pub fn phase<C: Capture, R: Recognize>(&self, dictation: &Dictation<C, R>) -> TalkPhase {
        if self.active {
            match dictation.phase() {
                Phase::Recording { latched } => return TalkPhase::Listening { latched },
                Phase::Transcribing => return TalkPhase::Transcribing,
                Phase::Idle => {}
            }
        }
        if self.pending.is_some() {
            return TalkPhase::Waiting;
        }
        if self.speaker.is_speaking() {
            return TalkPhase::Speaking;
        }
        match &self.reply {
            Some(r) if r.voice && r.speak && r.running && !r.muted => TalkPhase::Thinking,
            _ => TalkPhase::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;
    use crate::voice::audio_in::{RecordError, Recording};
    use crate::voice::stt::{SttError, Transcript};

    #[derive(Clone, Default)]
    struct Mic {
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Capture for Mic {
        fn start(&mut self) {
            self.log.borrow_mut().push("start".into());
        }
        fn stop(&mut self) -> Result<Recording, RecordError> {
            self.log.borrow_mut().push("stop".into());
            Ok(Recording {
                samples: vec![0.1; 16_000],
                truncated: false,
            })
        }
        fn cancel(&mut self) {
            self.log.borrow_mut().push("cancel".into());
        }
        fn level(&self) -> f32 {
            0.5
        }
        fn is_full(&self) -> bool {
            false
        }
        fn failure(&self) -> Option<RecordError> {
            None
        }
    }

    #[derive(Clone)]
    struct Stt {
        available: Rc<Cell<bool>>,
        ready: Rc<RefCell<VecDeque<Result<Transcript, SttError>>>>,
    }

    impl Recognize for Stt {
        fn model_available(&self) -> bool {
            self.available.get()
        }
        fn warm(&mut self) {}
        fn submit(&mut self, _samples: Vec<f32>) {}
        fn poll(&mut self) -> Option<Result<Transcript, SttError>> {
            self.ready.borrow_mut().pop_front()
        }
    }

    impl Stt {
        fn answer(&self, text: &str) {
            self.ready.borrow_mut().push_back(Ok(Transcript {
                text: text.into(),
                audio: Duration::from_secs(1),
                took: Duration::from_millis(20),
                load: None,
            }));
        }
    }

    /// Speaks instantly; `playing` stays on until the test clears it or `stop` runs.
    #[derive(Clone, Default)]
    struct Voice {
        said: Rc<RefCell<Vec<String>>>,
        stops: Rc<Cell<usize>>,
        playing: Rc<Cell<bool>>,
    }

    impl Say for Voice {
        fn enqueue(&self, sentence: String) {
            self.said.borrow_mut().push(sentence);
            self.playing.set(true);
        }
        fn stop(&self) {
            self.stops.set(self.stops.get() + 1);
            self.playing.set(false);
        }
        fn is_speaking(&self) -> bool {
            self.playing.get()
        }
    }

    #[derive(Default)]
    struct Chat {
        up: bool,
        running: bool,
        sent: Vec<String>,
        interrupts: usize,
    }

    impl KingChat for Chat {
        fn can_send(&self) -> bool {
            self.up && !self.running
        }
        fn is_running(&self) -> bool {
            self.running
        }
        fn is_up(&self) -> bool {
            self.up
        }
        fn send_voice(&mut self, text: &str) -> bool {
            if !self.can_send() {
                return false;
            }
            self.sent.push(text.to_owned());
            self.running = true;
            true
        }
        fn interrupt(&mut self) {
            self.interrupts += 1;
        }
    }

    struct Rig {
        talk: Talk<Voice>,
        dict: Dictation<Mic, Stt>,
        stt: Stt,
        mic: Mic,
        voice: Voice,
        chat: Chat,
    }

    fn rig() -> Rig {
        let mic = Mic::default();
        let stt = Stt {
            available: Rc::new(Cell::new(true)),
            ready: Rc::default(),
        };
        let voice = Voice::default();
        Rig {
            talk: Talk::new(voice.clone()),
            dict: Dictation::new(mic.clone(), stt.clone()),
            stt,
            mic,
            voice,
            chat: Chat {
                up: true,
                ..Chat::default()
            },
        }
    }

    impl Rig {
        fn tick(&mut self) -> Vec<TalkEvent> {
            self.talk.tick(&mut self.dict, &mut self.chat)
        }
        fn phase(&self) -> TalkPhase {
            self.talk.phase(&self.dict)
        }
        fn said(&self) -> Vec<String> {
            self.voice.said.borrow().clone()
        }
        /// Records and transcribes `text` with a press and release.
        fn say_to_king(&mut self, text: &str) -> Vec<TalkEvent> {
            self.talk.press(&mut self.dict).unwrap();
            self.talk.release(&mut self.dict);
            self.stt.answer(text);
            self.tick()
        }
    }

    fn deltas(r: &mut Rig, parts: &[&str]) {
        for p in parts {
            assert_eq!(r.talk.on_speech(&SpeechEvent::Text((*p).into())), None);
        }
    }

    #[test]
    fn press_record_release_transcribe_send_and_speak_the_reply() {
        let mut r = rig();
        assert_eq!(r.phase(), TalkPhase::Idle);
        r.talk.press(&mut r.dict).unwrap();
        assert!(r.talk.owns_dictation());
        assert_eq!(r.phase(), TalkPhase::Listening { latched: false });
        assert_eq!(r.tick(), []);
        r.talk.release(&mut r.dict);
        assert_eq!(r.phase(), TalkPhase::Transcribing);
        assert_eq!(r.tick(), [], "still transcribing");
        r.stt.answer("How many agents need input?");
        assert_eq!(
            r.tick(),
            [TalkEvent::Sent("How many agents need input?".into())]
        );
        assert!(!r.talk.owns_dictation());
        assert_eq!(r.chat.sent, ["How many agents need input?"]);
        assert_eq!(*r.mic.log.borrow(), ["start", "stop"]);

        // The chat starts the voice turn; his say-line streams in and is spoken by sentence,
        // the details after it are not.
        assert_eq!(
            r.talk.on_speech(&SpeechEvent::TurnStart { voice: true }),
            None
        );
        assert_eq!(r.phase(), TalkPhase::Thinking);
        deltas(
            &mut r,
            &[
                "\n<sa",
                "y>**Two** agents ",
                "need input. The search",
                " one is",
            ],
        );
        assert_eq!(r.said(), ["Two agents need input."]);
        assert_eq!(r.phase(), TalkPhase::Speaking);
        deltas(
            &mut r,
            &[
                " waiting for a review.</s",
                "ay>\n\n- `term:term_ab12` run 7c25",
            ],
        );
        r.talk.on_speech(&SpeechEvent::BlockEnd);
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert_eq!(
            r.said(),
            [
                "Two agents need input.",
                "The search one is waiting for a review."
            ]
        );
        r.voice.playing.set(false);
        assert_eq!(r.phase(), TalkPhase::Idle);
    }

    #[test]
    fn typed_turns_speak_only_with_speak_replies() {
        let mut r = rig();
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: false });
        deltas(&mut r, &["Status: all green. "]);
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert!(r.said().is_empty(), "typed turns are silent by default");

        r.talk.set_speak_replies(true);
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: false });
        deltas(&mut r, &["Status: all green."]);
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert_eq!(r.said(), ["Status: all green."]);
        assert_eq!(r.phase(), TalkPhase::Speaking);
    }

    #[test]
    fn a_new_turn_stops_the_old_reply() {
        let mut r = rig();
        r.talk.set_speak_replies(true);
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: false });
        deltas(&mut r, &["<say>First reply sentence. And more"]);
        assert!(r.voice.playing.get());
        let stops = r.voice.stops.get();
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: false });
        assert_eq!(r.voice.stops.get(), stops + 1);
        assert!(!r.voice.playing.get());
        // The old turn's unfinished sentence is not spoken into the new one.
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert_eq!(r.said(), ["First reply sentence."]);
    }

    #[test]
    fn stopping_silences_the_rest_of_the_reply() {
        let mut r = rig();
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: true });
        deltas(&mut r, &["<say>One. Two"]);
        assert!(r.talk.is_speaking());
        assert!(r.talk.stop_speaking());
        assert!(!r.talk.is_speaking());
        deltas(&mut r, &[". Three.</say>"]);
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert_eq!(r.said(), ["One."]);
        assert!(!r.talk.stop_speaking(), "nothing left to stop");
    }

    #[test]
    fn pressing_while_she_speaks_stops_her_and_a_tap_sends_nothing() {
        let mut r = rig();
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: true });
        deltas(&mut r, &["Long answer. "]);
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert!(r.talk.is_speaking());
        r.talk.press(&mut r.dict).unwrap();
        assert!(!r.talk.is_speaking(), "the press stopped him");
        assert_eq!(r.phase(), TalkPhase::Listening { latched: false });
        r.talk.release(&mut r.dict);
        r.stt.answer("");
        assert_eq!(r.tick(), [], "a tap to stop him is not a failed message");
        assert!(r.chat.sent.is_empty());

        // Without interrupting him, an empty recording is reported.
        assert_eq!(r.say_to_king("  "), [TalkEvent::NoSpeech]);
        assert!(r.chat.sent.is_empty());
    }

    #[test]
    fn a_busy_chat_is_interrupted_then_sent_to() {
        let mut r = rig();
        r.chat.running = true;
        assert_eq!(r.say_to_king("Stop that and check run 7."), []);
        assert_eq!(r.chat.interrupts, 1);
        assert_eq!(r.phase(), TalkPhase::Waiting);
        assert_eq!(r.tick(), []);
        assert_eq!(r.chat.interrupts, 1, "interrupted once");
        r.chat.running = false;
        assert_eq!(
            r.tick(),
            [TalkEvent::Sent("Stop that and check run 7.".into())]
        );
        assert_eq!(r.chat.sent, ["Stop that and check run 7."]);
    }

    #[test]
    fn a_chat_that_never_gets_ready_hands_the_text_back() {
        let mut r = rig();
        r.chat.up = false;
        r.talk.set_send_timeout(Duration::ZERO);
        assert_eq!(
            r.say_to_king("Anyone there?"),
            [TalkEvent::NotSent("Anyone there?".into())]
        );
        assert_eq!(
            r.chat.interrupts, 0,
            "nothing to interrupt while it is down"
        );
        assert_eq!(r.phase(), TalkPhase::Idle);
    }

    #[test]
    fn two_messages_before_she_is_ready_go_together() {
        let mut r = rig();
        r.chat.up = false;
        assert_eq!(r.say_to_king("First."), []);
        assert_eq!(r.say_to_king("Second."), []);
        r.chat.up = true;
        assert_eq!(r.tick(), [TalkEvent::Sent("First. Second.".into())]);
    }

    #[test]
    fn errors_and_missing_models() {
        let mut r = rig();
        r.stt.available.set(false);
        let err = r.talk.press(&mut r.dict).unwrap_err();
        assert!(err.is_model_missing());
        assert!(!r.talk.owns_dictation());
        assert!(
            r.mic.log.borrow().is_empty(),
            "nothing recorded or downloaded"
        );

        r.stt.available.set(true);
        r.talk.press(&mut r.dict).unwrap();
        r.talk.release(&mut r.dict);
        r.stt
            .ready
            .borrow_mut()
            .push_back(Err(SttError::LoadFailed("bad".into())));
        assert!(matches!(
            r.tick().as_slice(),
            [TalkEvent::Failed(DictationError::Stt(
                SttError::LoadFailed(_)
            ))]
        ));
        assert!(!r.talk.owns_dictation());

        // Without the voice model the reply is not spoken, and the app hears about it.
        r.talk.set_voice_installed(false);
        assert_eq!(
            r.talk.on_speech(&SpeechEvent::TurnStart { voice: true }),
            Some(TalkEvent::VoiceMissing)
        );
        deltas(&mut r, &["Hello there."]);
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert!(r.said().is_empty());
        assert_eq!(r.phase(), TalkPhase::Idle);
        // A silent typed turn needs no voice.
        assert_eq!(
            r.talk.on_speech(&SpeechEvent::TurnStart { voice: false }),
            None
        );
    }

    #[test]
    fn latched_recording_cancel_and_sharing_with_dictation() {
        let mut r = rig();
        r.talk.start_latched(&mut r.dict).unwrap();
        r.talk.release(&mut r.dict);
        assert_eq!(r.phase(), TalkPhase::Listening { latched: true });
        r.talk.press(&mut r.dict).unwrap();
        assert_eq!(r.phase(), TalkPhase::Transcribing, "a press finishes it");
        assert!(r.talk.cancel(&mut r.dict));
        assert_eq!(r.phase(), TalkPhase::Idle);
        r.stt.answer("dropped");
        assert_eq!(r.tick(), []);
        assert!(r.chat.sent.is_empty());

        // While ⌘E dictation has the microphone, the talk key does nothing.
        r.dict.press(false).unwrap();
        r.talk.press(&mut r.dict).unwrap();
        assert!(!r.talk.owns_dictation());
        r.dict.cancel();

        // A recording cancelled from the dictation pill frees the machine.
        r.talk.press(&mut r.dict).unwrap();
        r.dict.cancel();
        assert_eq!(r.tick(), []);
        assert!(!r.talk.owns_dictation());
    }

    #[test]
    fn only_the_say_line_is_spoken_across_blocks() {
        let mut r = rig();
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: true });
        // Narration before a tool call is held back, not spoken.
        deltas(&mut r, &["Let me check the clan."]);
        r.talk.on_speech(&SpeechEvent::BlockEnd);
        assert!(r.said().is_empty());
        deltas(
            &mut r,
            &[
                "<say>All quiet. Want the list?</say>",
                "\n\n| a | b |\n| 1 | 2 |\n",
            ],
        );
        r.talk.on_speech(&SpeechEvent::BlockEnd);
        deltas(&mut r, &["More text in a later block."]);
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert_eq!(r.said(), ["All quiet.", "Want the list?"]);
    }

    #[test]
    fn without_a_say_line_the_first_two_sentences_are_spoken() {
        let mut r = rig();
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: true });
        let long = format!(
            "Two agents need input. The docs one waits on a review. {}",
            "Then a long tail of details nobody should hear. ".repeat(6)
        );
        deltas(&mut r, &[&long[..100]]);
        assert!(r.said().is_empty(), "a tag may still come");
        deltas(&mut r, &[&long[100..]]);
        assert_eq!(
            r.said(),
            ["Two agents need input.", "The docs one waits on a review."]
        );
        r.talk.on_speech(&SpeechEvent::BlockEnd);
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert_eq!(r.said().len(), 2);

        // A short reply without a tag is spoken at the end of the turn.
        let mut r = rig();
        r.talk.on_speech(&SpeechEvent::TurnStart { voice: true });
        deltas(&mut r, &["Done. All ", "green. Three.", " Four."]);
        assert!(r.said().is_empty());
        r.talk.on_speech(&SpeechEvent::TurnEnd);
        assert_eq!(r.said(), ["Done.", "All green."]);
    }
}
