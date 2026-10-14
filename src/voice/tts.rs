//! Text-to-speech: the King's voice (Kokoro via sherpa-onnx).
//!
//! [`Speaker`] owns a pipeline thread that loads the model on first use (downloading it if
//! needed), synthesizes one sentence at a time and hands the audio to an [`AudioSink`], so the
//! first sentence plays while later ones synthesize. [`SentenceStream`] turns the King's
//! streamed markdown reply into speakable sentences to [`Speaker::enqueue`].

pub mod speakable;

use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};

use super::audio_out::{AudioSink, CpalSink, resample};
use super::models::kokoro::{self, KokoroPaths, ModelRelease, Progress};
pub use speakable::{
    SentenceStream, clean_inline, speakable_sentences, speakable_text, split_sentences,
};

/// Mono audio as synthesized.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Audio {
    pub fn duration_secs(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate.max(1) as f32
    }
}

/// Text in, audio out.
pub trait Synthesizer: Send {
    fn synthesize(&mut self, text: &str, voice: i32, speed: f32) -> anyhow::Result<Audio>;
}

/// Kokoro through sherpa-onnx.
pub struct KokoroSynth {
    tts: sherpa_rs::tts::KokoroTts,
}

impl KokoroSynth {
    /// Loads the model (a second or two). `threads` is onnxruntime's intra-op thread count.
    pub fn load(paths: &KokoroPaths, threads: i32) -> anyhow::Result<Self> {
        // sherpa-onnx exits the process on a bad config, so check the files up front.
        anyhow::ensure!(
            paths.is_complete(),
            "incomplete Kokoro model in {}",
            paths.dir.display()
        );
        let s = |p: &PathBuf| p.to_string_lossy().into_owned();
        let config = sherpa_rs::tts::KokoroTtsConfig {
            model: s(&paths.model),
            voices: s(&paths.voices),
            tokens: s(&paths.tokens),
            data_dir: s(&paths.espeak_data),
            lexicon: s(&paths.lexicon),
            dict_dir: s(&paths.dict),
            lang: "en-us".into(),
            length_scale: 1.0,
            onnx_config: sherpa_rs::OnnxConfig {
                num_threads: threads,
                ..Default::default()
            },
            ..Default::default()
        };
        Ok(KokoroSynth {
            tts: sherpa_rs::tts::KokoroTts::new(config),
        })
    }
}

impl Synthesizer for KokoroSynth {
    fn synthesize(&mut self, text: &str, voice: i32, speed: f32) -> anyhow::Result<Audio> {
        let audio = self
            .tts
            .create(text, voice, speed)
            .map_err(|e| anyhow::anyhow!("kokoro: {e}"))?;
        Ok(Audio {
            samples: audio.samples,
            sample_rate: audio.sample_rate,
        })
    }
}

/// A Kokoro voice (speaker id and name such as `af_heart`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub id: i32,
    pub name: String,
}

impl Voice {
    /// Language from the name's first letter (`a` American English, `b` British English, …).
    pub fn language(&self) -> &'static str {
        match self.name.chars().next() {
            Some('a') => "American English",
            Some('b') => "British English",
            Some('e') => "Spanish",
            Some('f') => "French",
            Some('h') => "Hindi",
            Some('i') => "Italian",
            Some('j') => "Japanese",
            Some('p') => "Portuguese",
            Some('z') => "Mandarin",
            _ => "Unknown",
        }
    }

    pub fn is_english(&self) -> bool {
        self.name.starts_with(['a', 'b'])
    }

    /// `af_heart` → `Heart (American English, female)`.
    pub fn label(&self) -> String {
        let (tag, name) = self.name.split_once('_').unwrap_or(("", &self.name));
        let gender = match tag.chars().nth(1) {
            Some('f') => ", female",
            Some('m') => ", male",
            _ => "",
        };
        let mut chars = name.chars();
        let name: String = chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        format!("{name} ({}{gender})", self.language())
    }
}

/// Voices of Kokoro v1.0 in speaker-id order (as in its `speaker_names` metadata); used
/// until the model is on disk.
pub const KOKORO_V1_0_VOICES: &[&str] = &[
    "af_alloy",
    "af_aoede",
    "af_bella",
    "af_heart",
    "af_jessica",
    "af_kore",
    "af_nicole",
    "af_nova",
    "af_river",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_echo",
    "am_eric",
    "am_fenrir",
    "am_liam",
    "am_michael",
    "am_onyx",
    "am_puck",
    "am_santa",
    "bf_alice",
    "bf_emma",
    "bf_isabella",
    "bf_lily",
    "bm_daniel",
    "bm_fable",
    "bm_george",
    "bm_lewis",
    "ef_dora",
    "em_alex",
    "ff_siwis",
    "hf_alpha",
    "hf_beta",
    "hm_omega",
    "hm_psi",
    "if_sara",
    "im_nicola",
    "jf_alpha",
    "jf_gongitsune",
    "jf_nezumi",
    "jf_tebukuro",
    "jm_kumo",
    "pf_dora",
    "pm_alex",
    "pm_santa",
    "zf_xiaobei",
    "zf_xiaoni",
    "zf_xiaoxiao",
    "zf_xiaoyi",
    "zm_yunjian",
    "zm_yunxi",
    "zm_yunxia",
    "zm_yunyang",
    "em_santa",
];

/// Kokoro's best-rated English voice.
pub const DEFAULT_VOICE: &str = "af_heart";

fn voices_from_names<S: AsRef<str>>(names: &[S]) -> Vec<Voice> {
    names
        .iter()
        .enumerate()
        .map(|(id, n)| Voice {
            id: id as i32,
            name: n.as_ref().to_owned(),
        })
        .collect()
}

/// What the speaker is doing about its model.
#[derive(Debug, Clone, PartialEq)]
pub enum SpeakerStatus {
    /// Nothing loaded yet; the first `speak` / `prepare` loads it.
    NotLoaded,
    Downloading {
        done: u64,
        total: u64,
    },
    Verifying,
    Unpacking,
    Loading,
    Ready,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct SpeakerConfig {
    /// Where models are downloaded to (`<data dir>/speech-models`).
    pub models_root: PathBuf,
    pub release: ModelRelease,
    /// onnxruntime threads for synthesis.
    pub threads: i32,
    /// Download the model when it is missing. Off in the app, which asks Karl first; the
    /// backend then fails with [`kokoro::NOT_INSTALLED`] until the model is installed.
    pub allow_download: bool,
}

impl Default for SpeakerConfig {
    fn default() -> Self {
        SpeakerConfig {
            models_root: kokoro::speech_models_dir(),
            release: kokoro::DEFAULT,
            threads: 4,
            allow_download: true,
        }
    }
}

/// Builds the synthesizer and the sink; runs on the pipeline thread on first use.
pub type BackendFactory = Box<
    dyn FnMut(&dyn Fn(SpeakerStatus)) -> anyhow::Result<(Box<dyn Synthesizer>, Arc<dyn AudioSink>)>
        + Send,
>;

enum Cmd {
    Say { text: String, generation: u64 },
    Prepare,
}

/// Sentences handed to the pipeline for the current generation and not yet played out.
#[derive(Debug, Default)]
struct Queue {
    generation: u64,
    unfinished: usize,
}

struct Shared {
    queue: Mutex<Queue>,
    sink: Mutex<Option<Arc<dyn AudioSink>>>,
    status: Mutex<SpeakerStatus>,
    voice: AtomicI32,
    speed: AtomicU32,
    volume: AtomicU32,
}

/// Speaks text with Kokoro. Cheap to create: the model loads on the pipeline thread the
/// first time it is needed.
pub struct Speaker {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    voices: Vec<Voice>,
}

impl Speaker {
    /// A speaker using Kokoro and the default audio output.
    pub fn new(config: SpeakerConfig) -> Self {
        let voices = kokoro::installed(&config.models_root, &config.release)
            .and_then(|p| kokoro::read_speaker_names(&p.model).ok())
            .map(|names| voices_from_names(&names))
            .unwrap_or_else(|| voices_from_names(KOKORO_V1_0_VOICES));
        Self::with_backend(kokoro_backend(config), voices)
    }

    /// A speaker with its synthesizer and sink supplied by `factory` (tests, other engines).
    pub fn with_backend(factory: BackendFactory, voices: Vec<Voice>) -> Self {
        let voice = voices
            .iter()
            .find(|v| v.name == DEFAULT_VOICE)
            .map_or(0, |v| v.id);
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            sink: Mutex::new(None),
            status: Mutex::new(SpeakerStatus::NotLoaded),
            voice: AtomicI32::new(voice),
            speed: AtomicU32::new(1.0f32.to_bits()),
            volume: AtomicU32::new(1.0f32.to_bits()),
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("tts".into())
            .spawn(move || pipeline(rx, worker, factory))
            .expect("spawn tts thread");
        Speaker { tx, shared, voices }
    }

    /// Stops whatever is playing and speaks `markdown` (cleaned and split into sentences).
    pub fn speak(&self, markdown: &str) {
        self.stop();
        for sentence in speakable_sentences(markdown) {
            self.enqueue(sentence);
        }
    }

    /// Queues one already-speakable sentence (e.g. from a [`SentenceStream`]) after whatever
    /// is queued.
    pub fn enqueue(&self, sentence: impl Into<String>) {
        let text = sentence.into();
        if text.trim().is_empty() {
            return;
        }
        let generation = {
            let mut q = self.shared.queue.lock().unwrap();
            q.unfinished += 1;
            q.generation
        };
        let _ = self.tx.send(Cmd::Say { text, generation });
    }

    /// Stops immediately: flushes queued audio and drops sentences not yet synthesized.
    pub fn stop(&self) {
        let mut q = self.shared.queue.lock().unwrap();
        q.generation += 1;
        q.unfinished = 0;
        if let Some(sink) = self.shared.sink.lock().unwrap().as_ref() {
            sink.clear();
        }
    }

    /// Whether anything is queued, synthesizing or playing.
    pub fn is_speaking(&self) -> bool {
        if self.shared.queue.lock().unwrap().unfinished > 0 {
            return true;
        }
        self.shared
            .sink
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.pending() > 0)
    }

    /// Loads the model (downloading it if needed) ahead of the first `speak`.
    pub fn prepare(&self) {
        let _ = self.tx.send(Cmd::Prepare);
    }

    pub fn status(&self) -> SpeakerStatus {
        self.shared.status.lock().unwrap().clone()
    }

    /// Selects a voice by speaker id; applies from the next sentence synthesized.
    pub fn set_voice(&self, id: i32) {
        self.shared.voice.store(id, Ordering::Relaxed);
    }

    /// Selects a voice by name (`af_heart`); false if the model has no such voice.
    pub fn set_voice_by_name(&self, name: &str) -> bool {
        match self.voices.iter().find(|v| v.name == name) {
            Some(v) => {
                self.set_voice(v.id);
                true
            }
            None => false,
        }
    }

    pub fn voice(&self) -> i32 {
        self.shared.voice.load(Ordering::Relaxed)
    }

    /// Speaking rate, 1.0 normal; clamped to 0.5–2.0.
    pub fn set_speed(&self, speed: f32) {
        let speed = if speed.is_finite() {
            speed.clamp(0.5, 2.0)
        } else {
            1.0
        };
        self.shared.speed.store(speed.to_bits(), Ordering::Relaxed);
    }

    pub fn speed(&self) -> f32 {
        f32::from_bits(self.shared.speed.load(Ordering::Relaxed))
    }

    /// Output gain, 0.0–1.0.
    pub fn set_volume(&self, volume: f32) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.shared
            .volume
            .store(volume.to_bits(), Ordering::Relaxed);
    }

    pub fn voices(&self) -> &[Voice] {
        &self.voices
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        // Dropping `tx` ends the pipeline thread once it finishes its current sentence.
        self.stop();
    }
}

fn kokoro_backend(config: SpeakerConfig) -> BackendFactory {
    Box::new(move |status: &dyn Fn(SpeakerStatus)| {
        if !config.allow_download
            && kokoro::installed(&config.models_root, &config.release).is_none()
        {
            anyhow::bail!(kokoro::NOT_INSTALLED);
        }
        let paths = kokoro::ensure(&config.models_root, &config.release, |p| {
            status(match p {
                Progress::Downloading { done, total } => SpeakerStatus::Downloading { done, total },
                Progress::Verifying => SpeakerStatus::Verifying,
                Progress::Unpacking => SpeakerStatus::Unpacking,
            })
        })?;
        status(SpeakerStatus::Loading);
        let synth = KokoroSynth::load(&paths, config.threads)?;
        let sink = CpalSink::open_default()?;
        Ok((
            Box::new(synth) as Box<dyn Synthesizer>,
            Arc::new(sink) as Arc<dyn AudioSink>,
        ))
    })
}

fn pipeline(rx: Receiver<Cmd>, shared: Arc<Shared>, mut factory: BackendFactory) {
    let set_status = |s: SpeakerStatus| *shared.status.lock().unwrap() = s;
    let mut backend: Option<(Box<dyn Synthesizer>, Arc<dyn AudioSink>)> = None;
    for cmd in rx {
        if backend.is_none() {
            match factory(&set_status) {
                Ok(b) => {
                    *shared.sink.lock().unwrap() = Some(b.1.clone());
                    backend = Some(b);
                    set_status(SpeakerStatus::Ready);
                }
                Err(e) => {
                    log::warn!("tts: {e:#}");
                    set_status(SpeakerStatus::Failed(format!("{e:#}")));
                }
            }
        }
        let Cmd::Say { text, generation } = cmd else {
            continue;
        };
        let current = |shared: &Shared| shared.queue.lock().unwrap().generation == generation;
        let Some((synth, sink)) = backend.as_mut().filter(|_| current(&shared)) else {
            finish_one(&shared, generation);
            continue;
        };
        let voice = shared.voice.load(Ordering::Relaxed);
        let speed = f32::from_bits(shared.speed.load(Ordering::Relaxed));
        let audio = match synth.synthesize(&text, voice, speed) {
            Ok(a) => a,
            Err(e) => {
                log::warn!("tts: synthesis failed: {e:#}");
                finish_one(&shared, generation);
                continue;
            }
        };
        let mut samples = resample(&audio.samples, audio.sample_rate, sink.sample_rate());
        let volume = f32::from_bits(shared.volume.load(Ordering::Relaxed));
        if volume != 1.0 {
            samples.iter_mut().for_each(|s| *s *= volume);
        }
        // Under the queue lock, so a concurrent stop() either sees this audio and clears it or
        // bumps the generation first and it is never pushed.
        let mut q = shared.queue.lock().unwrap();
        if q.generation == generation {
            sink.push(&samples);
            q.unfinished = q.unfinished.saturating_sub(1);
        }
    }
}

fn finish_one(shared: &Shared, generation: u64) {
    let mut q = shared.queue.lock().unwrap();
    if q.generation == generation {
        q.unfinished = q.unfinished.saturating_sub(1);
    }
}

const SAY_USAGE: &str = "usage: stronghold say [--wav <file>] [--voice <name>] [--speed <x>] \
                         [--volume <0-1>] <markdown text…>";

/// `stronghold say …`: speaks (or renders to a WAV file) text with the King's voice,
/// downloading the model on first use.
pub fn run_cli(args: &[String]) -> Option<anyhow::Result<()>> {
    let (cmd, rest) = args.split_first()?;
    (cmd == "say").then(|| say(rest))
}

fn say(args: &[String]) -> anyhow::Result<()> {
    let mut wav = None;
    let mut voice = DEFAULT_VOICE.to_owned();
    let mut speed = 1.0f32;
    let mut volume = 1.0f32;
    let mut words = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| anyhow::anyhow!("{SAY_USAGE}"));
        match arg.as_str() {
            "--wav" => wav = Some(PathBuf::from(value()?)),
            "--voice" => voice = value()?.clone(),
            "--speed" => speed = value()?.parse()?,
            "--volume" => volume = value()?.parse()?,
            "-h" | "--help" => {
                println!("{SAY_USAGE}");
                return Ok(());
            }
            _ => words.push(arg.as_str()),
        }
    }
    let text = words.join(" ");
    anyhow::ensure!(!text.trim().is_empty(), "{SAY_USAGE}");
    let config = SpeakerConfig::default();
    let paths = kokoro::ensure(&config.models_root, &config.release, |p| {
        if let Progress::Downloading { done, total } = p {
            eprint!("\rdownloading Kokoro: {} / {} MB", done >> 20, total >> 20);
        }
    })?;
    eprintln!();
    let names = kokoro::read_speaker_names(&paths.model)?;
    let voice_id = names
        .iter()
        .position(|n| *n == voice)
        .ok_or_else(|| anyhow::anyhow!("unknown voice {voice}; voices: {}", names.join(", ")))?;
    match wav {
        Some(path) => {
            let mut synth = KokoroSynth::load(&paths, config.threads)?;
            let mut samples = Vec::new();
            let mut rate = 24_000;
            for sentence in speakable_sentences(&text) {
                let audio = synth.synthesize(&sentence, voice_id as i32, speed)?;
                rate = audio.sample_rate;
                samples.extend(audio.samples.iter().map(|s| s * volume));
            }
            super::audio_out::write_wav(&path, &samples, rate)?;
            eprintln!(
                "wrote {:.1}s to {}",
                samples.len() as f32 / rate as f32,
                path.display()
            );
        }
        None => {
            let speaker = Speaker::new(config);
            speaker.set_voice(voice_id as i32);
            speaker.set_speed(speed);
            speaker.set_volume(volume);
            speaker.speak(&text);
            std::thread::sleep(std::time::Duration::from_millis(50));
            while speaker.is_speaking() {
                if let SpeakerStatus::Failed(e) = speaker.status() {
                    anyhow::bail!(e);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // Let the device drain its last buffer.
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::voice::audio_out::{BufferSink, rms};

    /// Emits a 440 Hz tone of 50 ms per character after `delay`, at 24 kHz.
    struct FakeSynth {
        delay: Duration,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Synthesizer for FakeSynth {
        fn synthesize(&mut self, text: &str, _voice: i32, _speed: f32) -> anyhow::Result<Audio> {
            std::thread::sleep(self.delay);
            self.log.lock().unwrap().push(text.to_owned());
            let n = text.len() * 1200;
            Ok(Audio {
                samples: (0..n).map(|i| (i as f32 * 0.115).sin() * 0.5).collect(),
                sample_rate: 24_000,
            })
        }
    }

    fn fake_speaker(delay: Duration) -> (Speaker, Arc<BufferSink>, Arc<Mutex<Vec<String>>>) {
        let sink = Arc::new(BufferSink::new(48_000));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (s, l) = (sink.clone(), log.clone());
        let factory: BackendFactory = Box::new(move |_status: &dyn Fn(SpeakerStatus)| {
            Ok((
                Box::new(FakeSynth {
                    delay,
                    log: l.clone(),
                }) as Box<dyn Synthesizer>,
                s.clone() as Arc<dyn AudioSink>,
            ))
        });
        let speaker = Speaker::with_backend(factory, voices_from_names(KOKORO_V1_0_VOICES));
        (speaker, sink, log)
    }

    fn wait_until(mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn speaks_sentences_in_order_resampled_to_the_sink() {
        let (speaker, sink, log) = fake_speaker(Duration::ZERO);
        speaker.speak("**Hi.** Second one.");
        wait_until(|| log.lock().unwrap().len() == 2);
        wait_until(|| sink.pending() == ("Hi.".len() + "Second one.".len()) * 2400);
        assert_eq!(*log.lock().unwrap(), ["Hi.", "Second one."]);
        assert!(speaker.is_speaking(), "audio still queued in the sink");
        assert!(rms(&sink.take()) > 0.1);
        assert!(!speaker.is_speaking());
        assert_eq!(speaker.status(), SpeakerStatus::Ready);
    }

    #[test]
    fn stop_flushes_audio_and_drops_queued_sentences_immediately() {
        let (speaker, sink, log) = fake_speaker(Duration::from_millis(40));
        for i in 0..10 {
            speaker.enqueue(format!("Sentence number {i}."));
        }
        wait_until(|| sink.pending() > 0);
        assert!(speaker.is_speaking());
        let t = Instant::now();
        speaker.stop();
        let latency = t.elapsed();
        assert!(!speaker.is_speaking());
        assert_eq!(sink.pending(), 0);
        assert!(latency < Duration::from_millis(5), "stop took {latency:?}");
        // The sentence being synthesized during stop() is discarded; nothing else is made.
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(sink.pending(), 0);
        assert!(log.lock().unwrap().len() <= 2, "{:?}", log.lock().unwrap());
        // And the speaker keeps working afterwards.
        speaker.enqueue("Again.");
        wait_until(|| sink.pending() > 0);
    }

    #[test]
    fn first_sentence_is_queued_while_later_ones_synthesize() {
        let (speaker, sink, log) = fake_speaker(Duration::from_millis(30));
        speaker.speak("One. Two. Three. Four.");
        wait_until(|| sink.pending() > 0);
        assert!(
            log.lock().unwrap().len() < 4,
            "playback started before all synthesized"
        );
        wait_until(|| log.lock().unwrap().len() == 4);
    }

    #[test]
    fn settings_and_voices() {
        let (speaker, _sink, _log) = fake_speaker(Duration::ZERO);
        assert_eq!(speaker.voices().len(), 54);
        assert_eq!(speaker.voice(), 3, "af_heart by default");
        assert!(speaker.set_voice_by_name("bm_george"));
        assert_eq!(speaker.voice(), 26);
        assert!(!speaker.set_voice_by_name("nope"));
        speaker.set_speed(9.0);
        assert_eq!(speaker.speed(), 2.0);
        let heart = &speaker.voices()[3];
        assert_eq!(heart.label(), "Heart (American English, female)");
        assert!(heart.is_english());
        assert_eq!(
            speaker.voices()[26].label(),
            "George (British English, male)"
        );
    }

    /// With downloads off (as in the app) a missing model fails fast and nothing is fetched.
    #[test]
    fn missing_model_is_not_downloaded_when_downloads_are_off() {
        let root = tempfile::tempdir().unwrap();
        let speaker = Speaker::new(SpeakerConfig {
            models_root: root.path().to_owned(),
            allow_download: false,
            ..SpeakerConfig::default()
        });
        speaker.prepare();
        speaker.speak("Hello.");
        wait_until(|| matches!(speaker.status(), SpeakerStatus::Failed(_)));
        assert_eq!(
            speaker.status(),
            SpeakerStatus::Failed(kokoro::NOT_INSTALLED.into())
        );
        wait_until(|| !speaker.is_speaking());
        let entries: Vec<_> = std::fs::read_dir(root.path()).unwrap().collect();
        assert!(entries.is_empty(), "nothing written: {entries:?}");
        assert_eq!(speaker.voices().len(), 54, "the voice list is known anyway");
    }

    #[test]
    fn a_failing_backend_reports_and_retries() {
        let attempts = Arc::new(Mutex::new(0));
        let a = attempts.clone();
        let factory: BackendFactory = Box::new(move |_status: &dyn Fn(SpeakerStatus)| {
            *a.lock().unwrap() += 1;
            anyhow::bail!("no model")
        });
        let speaker = Speaker::with_backend(factory, Vec::new());
        speaker.speak("Hello.");
        wait_until(|| matches!(speaker.status(), SpeakerStatus::Failed(_)));
        wait_until(|| !speaker.is_speaking());
        speaker.speak("Again.");
        wait_until(|| *attempts.lock().unwrap() == 2);
    }

    /// Needs the model: set `STRONGHOLD_DATA_DIR` to a dir whose `speech-models/` has (or may
    /// download) it. Renders to `$STRONGHOLD_TTS_WAV` if set.
    /// `cargo test --release --lib kokoro_synthesizes -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn kokoro_synthesizes_plausible_speech() {
        let release = match std::env::var("STRONGHOLD_TTS_MODEL").as_deref() {
            Ok("int8") => kokoro::KOKORO_INT8_V1_0,
            _ => kokoro::DEFAULT,
        };
        let root = kokoro::speech_models_dir();
        let t = Instant::now();
        let paths = kokoro::ensure(&root, &release, |_| {}).expect("model");
        eprintln!("model ready in {:?}: {}", t.elapsed(), paths.dir.display());
        let names = kokoro::read_speaker_names(&paths.model).unwrap();
        eprintln!("{} voices: {}", names.len(), names.join(", "));
        assert_eq!(names, KOKORO_V1_0_VOICES);

        let t = Instant::now();
        let threads = std::env::var("STRONGHOLD_TTS_THREADS")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(SpeakerConfig::default().threads);
        let mut synth = KokoroSynth::load(&paths, threads).unwrap();
        eprintln!("load ({threads} threads): {:?}", t.elapsed());
        let text = "The clan has three runs active, and terminal ab12 is waiting for you.";
        // Warm-up, then measure.
        synth.synthesize("Hello.", 3, 1.0).unwrap();
        let t = Instant::now();
        let first = synth.synthesize("Run 7c25 finished.", 3, 1.0).unwrap();
        let ttfa = t.elapsed();
        let t = Instant::now();
        let audio = synth.synthesize(text, 3, 1.0).unwrap();
        let took = t.elapsed().as_secs_f32();
        let secs = audio.duration_secs();
        eprintln!(
            "short sentence: {ttfa:?} for {:.2}s audio; long: {took:.2}s for {secs:.2}s audio, RTF {:.3}",
            first.duration_secs(),
            took / secs
        );
        assert_eq!(audio.sample_rate, 24_000);
        // ~13 words: 2.5–8 s of speech.
        assert!((2.5..8.0).contains(&secs), "duration {secs}");
        assert!(rms(&audio.samples) > 0.02, "silent");
        if let Some(path) = std::env::var_os("STRONGHOLD_TTS_WAV") {
            crate::voice::audio_out::write_wav(path.as_ref(), &audio.samples, 24_000).unwrap();
        }
    }

    /// Plays through the default output device, quietly. Needs the model (see above).
    /// `cargo test --release --lib speaker_plays_aloud -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn speaker_plays_aloud() {
        let speaker = Speaker::new(SpeakerConfig::default());
        speaker.set_volume(0.15);
        let t = Instant::now();
        speaker.prepare();
        wait_until_for(Duration::from_secs(120), || {
            speaker.status() == SpeakerStatus::Ready
        });
        eprintln!("prepare: {:?}", t.elapsed());
        let t = Instant::now();
        speaker.speak("Hello Karl. Terminal ab12 is waiting for you.");
        let sink = speaker.shared.sink.lock().unwrap().clone().unwrap();
        wait_until(|| sink.pending() > 0);
        eprintln!(
            "time to first audio: {:?} (device rate {} Hz)",
            t.elapsed(),
            sink.sample_rate()
        );
        wait_until_for(Duration::from_secs(20), || !speaker.is_speaking());
        eprintln!("done after {:?}", t.elapsed());
    }

    fn wait_until_for(limit: Duration, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + limit;
        while !f() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
