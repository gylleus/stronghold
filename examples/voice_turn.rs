//! A scripted voice turn through the real talk pipeline, with a WAV file in place of the
//! microphone and the King's spoken reply rendered to a WAV file instead of the speakers:
//! WAV → `Recorder` → Parakeet → `Talk` → the King's chat → `SayFilter` → `SentenceStream` →
//! Kokoro → `BufferSink`. Prints the say-line and a latency breakdown.
//!
//! ```text
//! say -o /tmp/q.wav --data-format=LEI16@16000 "How many agents need input?"
//! STRONGHOLD_DATA_DIR=<scratch> cargo run --release --example voice_turn -- /tmp/q.wav reply.wav
//! ```
//!
//! Nothing is downloaded: Kokoro must already be in `$STRONGHOLD_DATA_DIR/speech-models` and
//! Parakeet there or in Orca's data dir. The chat is a real `claude` King session in
//! `$STRONGHOLD_DATA_DIR/chat` (use a scratch data dir: with no Stronghold app on its control
//! socket his actions are refused, and permission cards are denied); `--fake-chat` streams a
//! canned reply instead.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use stronghold::chat::settings::KingSettings;
use stronghold::chat::transcript::Decision;
use stronghold::chat::{ChatConfig, ChatSession, SessionState, SpeechEvent};
use stronghold::voice::audio_in::{
    CaptureGuard, InputSource, RecordError, Recorder, SampleSink, read_wav, resample,
};
use stronghold::voice::audio_out::{AudioSink, BufferSink, write_wav};
use stronghold::voice::dictation::Dictation;
use stronghold::voice::models::kokoro;
use stronghold::voice::stt::Transcriber;
use stronghold::voice::talk::{KingChat, Talk, TalkEvent};
use stronghold::voice::tts::{
    BackendFactory, DEFAULT_VOICE, KokoroSynth, Speaker, SpeakerStatus, Synthesizer, Voice,
};

/// Plays a WAV file into the recorder as if it were the microphone.
struct WavSource {
    samples: Vec<f32>,
    rate: u32,
}

impl InputSource for WavSource {
    fn start(&self, sink: SampleSink) -> Result<Box<dyn CaptureGuard>, RecordError> {
        sink.set_rate(self.rate);
        for chunk in self.samples.chunks(512) {
            sink.push(chunk, 1);
        }
        Ok(Box::new(()))
    }
}

/// A stand-in King: after `delay` his reply streams in small deltas.
struct FakeChat {
    reply: &'static str,
    delay: Duration,
    sent_at: Option<Instant>,
    events: Vec<SpeechEvent>,
    offset: usize,
}

impl KingChat for FakeChat {
    fn can_send(&self) -> bool {
        self.sent_at.is_none()
    }
    fn is_running(&self) -> bool {
        self.sent_at.is_some()
    }
    fn is_up(&self) -> bool {
        true
    }
    fn send_voice(&mut self, text: &str) -> bool {
        eprintln!("[fake chat] got: {text}");
        self.sent_at = Some(Instant::now());
        self.events.push(SpeechEvent::TurnStart { voice: true });
        true
    }
    fn interrupt(&mut self) {}
}

impl FakeChat {
    fn take(&mut self) -> Vec<SpeechEvent> {
        let mut out = std::mem::take(&mut self.events);
        let Some(at) = self.sent_at else {
            return out;
        };
        if at.elapsed() < self.delay || self.offset > self.reply.len() {
            return out;
        }
        let end = (self.offset + 12).min(self.reply.len());
        let end = (end..=self.reply.len())
            .find(|i| self.reply.is_char_boundary(*i))
            .unwrap_or(self.reply.len());
        if self.offset < end {
            out.push(SpeechEvent::Text(self.reply[self.offset..end].to_owned()));
        }
        self.offset = end;
        if end == self.reply.len() {
            out.push(SpeechEvent::BlockEnd);
            out.push(SpeechEvent::TurnEnd);
            self.offset += 1;
        }
        out
    }
}

enum King {
    Real(Box<ChatSession>),
    Fake(FakeChat),
}

impl King {
    fn chat(&mut self) -> &mut dyn KingChat {
        match self {
            King::Real(c) => c.as_mut(),
            King::Fake(c) => c,
        }
    }

    fn take_speech(&mut self) -> Vec<SpeechEvent> {
        match self {
            King::Real(c) => {
                c.poll();
                // Nobody is here to approve his actions.
                if let Some(card) = c.transcript.pending_permission() {
                    let id = card.request.request_id.clone();
                    eprintln!("[chat] denying {}", card.request.tool_name);
                    c.decide(&id, Decision::Deny);
                }
                c.take_speech()
            }
            King::Fake(c) => c.take(),
        }
    }
}

fn ms(d: Duration) -> String {
    format!("{} ms", d.as_millis())
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fake = args.iter().any(|a| a == "--fake-chat");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let Some(input) = files.first() else {
        bail!("usage: voice_turn <question.wav> [reply.wav] [--fake-chat]");
    };
    let output = PathBuf::from(files.get(1).map_or("reply.wav", |s| s.as_str()));
    if std::env::var_os("STRONGHOLD_DATA_DIR").is_none() {
        bail!("set STRONGHOLD_DATA_DIR to a scratch data dir");
    }

    // The question, as the microphone would hear it.
    let (samples, rate) = read_wav(&std::fs::read(input)?).map_err(anyhow::Error::msg)?;
    eprintln!(
        "question: {:.2} s of audio at {rate} Hz from {input}",
        samples.len() as f32 / rate as f32
    );

    // His voice, rendered into a buffer. Loaded up front, as the app does at start.
    let root = kokoro::speech_models_dir();
    let paths = kokoro::installed(&root, &kokoro::DEFAULT).context(
        "Kokoro is not installed in $STRONGHOLD_DATA_DIR/speech-models (not downloading)",
    )?;
    let voices: Vec<Voice> = kokoro::read_speaker_names(&paths.model)?
        .into_iter()
        .enumerate()
        .map(|(id, name)| Voice {
            id: id as i32,
            name,
        })
        .collect();
    let sink = Arc::new(BufferSink::new(24_000));
    let backend_sink = sink.clone();
    let factory: BackendFactory = Box::new(move |_status: &dyn Fn(SpeakerStatus)| {
        let synth = KokoroSynth::load(&paths, 4)?;
        Ok((
            Box::new(synth) as Box<dyn Synthesizer>,
            backend_sink.clone() as Arc<dyn AudioSink>,
        ))
    });
    let speaker = Speaker::with_backend(factory, voices);
    speaker.set_voice_by_name(DEFAULT_VOICE);
    let t = Instant::now();
    speaker.prepare();
    while speaker.status() != SpeakerStatus::Ready {
        if let SpeakerStatus::Failed(e) = speaker.status() {
            bail!("kokoro: {e}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let kokoro_load = t.elapsed();

    // Parakeet, warmed as the app does at start.
    let mut transcriber = Transcriber::parakeet(None);
    let t = Instant::now();
    transcriber
        .transcribe_blocking(vec![0.0; 8_000])
        .context("load Parakeet")?;
    let parakeet_load = t.elapsed();
    let recorder = Recorder::new(Arc::new(WavSource { samples, rate }));
    let mut dictation = Dictation::new(recorder, transcriber);

    let mut king = if fake {
        King::Fake(FakeChat {
            reply: "<say>Two agents need you: the reviewer and the docs one. Want the list?</say>\n\n\
                    - `term:term_7c25` reviewer on run 7c25\n- `term:term_d0c5` docs worker",
            delay: Duration::from_millis(800),
            sent_at: None,
            events: Vec::new(),
            offset: 0,
        })
    } else {
        let config = ChatConfig::from_env(&KingSettings::default());
        eprintln!("starting the King's chat in {}", config.dir.display());
        let mut chat = ChatSession::spawn(config, None);
        let t = Instant::now();
        while chat.state != SessionState::Ready {
            chat.poll();
            if let SessionState::Unavailable(e) = &chat.state {
                bail!("chat unavailable: {e}");
            }
            if t.elapsed() > Duration::from_secs(60) {
                bail!("chat did not start");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("chat ready in {}", ms(t.elapsed()));
        King::Real(Box::new(chat))
    };

    let mut talk = Talk::new(speaker);
    // Press and let go: the whole question is already in the recorder.
    talk.press(&mut dictation)?;
    talk.release(&mut dictation);
    let released = Instant::now();

    let (mut sent, mut first_text, mut first_sentence, mut first_audio, mut ended) =
        (None, None, None, None, None);
    let mut reply = String::new();
    let mut last_pending = 0;
    let mut stable_since = Instant::now();
    loop {
        for event in talk.tick(&mut dictation, king.chat()) {
            match event {
                TalkEvent::Sent(text) => {
                    eprintln!("transcript: {text:?}");
                    sent = Some(released.elapsed());
                }
                other => bail!("talk: {other:?}"),
            }
        }
        for speech in king.take_speech() {
            if let SpeechEvent::Text(t) = &speech {
                first_text.get_or_insert(released.elapsed());
                reply.push_str(t);
            }
            if speech == SpeechEvent::TurnEnd {
                ended.get_or_insert(released.elapsed());
            }
            talk.on_speech(&speech);
            if first_sentence.is_none() && talk.is_speaking() {
                first_sentence = Some(released.elapsed());
            }
        }
        let pending = sink.pending();
        if pending > 0 {
            first_audio.get_or_insert(released.elapsed());
        }
        if pending != last_pending {
            last_pending = pending;
            stable_since = Instant::now();
        }
        // Done once the turn is over and synthesis has stopped adding audio.
        let quiet = stable_since.elapsed() > Duration::from_secs(3);
        if ended.is_some() && quiet && (pending > 0 || !talk.is_speaking()) {
            break;
        }
        if released.elapsed() > Duration::from_secs(180) {
            bail!("timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let audio = sink.take();
    write_wav(&output, &audio, 24_000)?;

    let show = |d: Option<Duration>| d.map_or("never".into(), ms);
    println!("reply text: {reply:?}");
    let say = stronghold::chat::say::split_say(&reply).say;
    println!(
        "say-line: {}",
        say.as_deref()
            .map_or("none (fallback: first sentences)".into(), |s| format!(
                "{s:?} ({} words)",
                s.split_whitespace().count()
            ))
    );
    println!(
        "reply audio: {:.1} s written to {}",
        audio.len() as f32 / 24_000.0,
        output.display()
    );
    println!(
        "model load (at app start): Parakeet {}, Kokoro {}",
        ms(parakeet_load),
        ms(kokoro_load)
    );
    println!("release → transcript sent:   {}", show(sent));
    println!("release → first reply text:  {}", show(first_text));
    println!("release → first sentence:    {}", show(first_sentence));
    println!("release → first audio:       {}", show(first_audio));
    println!("release → turn finished:     {}", show(ended));
    if let (Some(s), Some(f)) = (sent, first_text) {
        println!("  of which the King's first token: {}", ms(f - s));
    }
    if let (Some(f), Some(a)) = (first_sentence, first_audio) {
        println!("  of which first-sentence synthesis: {}", ms(a - f));
    }
    // Check the rendered reply by listening to it with Parakeet instead of the speakers.
    let heard = Transcriber::parakeet(None)
        .transcribe_blocking(resample(&audio, 24_000, 16_000))
        .context("transcribe the reply")?;
    println!("reply audio heard back as: {:?}", heard.text);
    Ok(())
}
