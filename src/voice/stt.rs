//! Speech-to-text: a [`Transcriber`] runs NVIDIA Parakeet TDT (through sherpa-onnx) on a worker
//! thread that loads the model on first use and keeps it warm.

use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use super::audio_in::TARGET_RATE;
use super::models::{self, ParakeetFiles};

/// Why a transcription failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SttError {
    #[error("Speech model not downloaded")]
    ModelMissing,
    #[error("Could not load the speech model: {0}")]
    LoadFailed(String),
}

/// A transcription and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub audio: Duration,
    /// Recognition time (excluding a first-use model load).
    pub took: Duration,
    /// Model load time, when this request loaded it.
    pub load: Option<Duration>,
}

/// A speech recognizer, called on the transcriber's thread.
pub trait SpeechEngine: Send + 'static {
    /// Loads the model if it is not loaded yet; returns how long loading took.
    fn load(&mut self) -> Result<Option<Duration>, SttError>;
    /// Raw text for 16 kHz mono samples; only called after a successful `load`.
    fn recognize(&mut self, samples: &[f32]) -> String;
}

/// Parakeet TDT 0.6B v3 through sherpa-onnx, found with [`models::find_parakeet`] when first
/// needed (so a model downloaded later is picked up).
#[derive(Default)]
pub struct ParakeetEngine {
    files: Option<ParakeetFiles>,
    recognizer: Option<sherpa_rs::transducer::TransducerRecognizer>,
}

impl ParakeetEngine {
    /// Uses `files` instead of searching the model folders.
    pub fn with_files(files: ParakeetFiles) -> Self {
        Self {
            files: Some(files),
            recognizer: None,
        }
    }
}

impl SpeechEngine for ParakeetEngine {
    fn load(&mut self) -> Result<Option<Duration>, SttError> {
        if self.recognizer.is_some() {
            return Ok(None);
        }
        let files = match &self.files {
            Some(files) => files.clone(),
            None => models::find_parakeet().ok_or(SttError::ModelMissing)?,
        };
        let started = Instant::now();
        let threads = thread::available_parallelism().map_or(2, |n| n.get().min(4));
        let path = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let config = sherpa_rs::transducer::TransducerConfig {
            encoder: path(&files.encoder),
            decoder: path(&files.decoder),
            joiner: path(&files.joiner),
            tokens: path(&files.tokens),
            model_type: "nemo_transducer".into(),
            decoding_method: "greedy_search".into(),
            sample_rate: TARGET_RATE as i32,
            feature_dim: 80,
            num_threads: threads as i32,
            provider: Some("cpu".into()),
            ..Default::default()
        };
        let recognizer = sherpa_rs::transducer::TransducerRecognizer::new(config)
            .map_err(|e| SttError::LoadFailed(e.to_string()))?;
        self.recognizer = Some(recognizer);
        let took = started.elapsed();
        log::info!(
            "speech model loaded in {took:.2?} from {}",
            files.encoder.display()
        );
        Ok(Some(took))
    }

    fn recognize(&mut self, samples: &[f32]) -> String {
        let Some(recognizer) = &mut self.recognizer else {
            return String::new();
        };
        recognizer.transcribe(TARGET_RATE, samples)
    }
}

/// Silence added after the audio so the transducer emits its last word.
const TAIL_PADDING: Duration = Duration::from_millis(500);
/// Shorter clips are taps, not speech.
const MIN_SPEECH: Duration = Duration::from_millis(200);
/// Clips whose peak stays under this are silence.
const SILENCE_PEAK: f32 = 1e-3;

/// Runs `engine` on `samples` (16 kHz mono): skips empty/silent audio and tidies the text.
pub fn transcribe_with(
    engine: &mut dyn SpeechEngine,
    samples: &[f32],
) -> Result<Transcript, SttError> {
    let audio = Duration::from_secs_f64(samples.len() as f64 / f64::from(TARGET_RATE));
    let load = engine.load()?;
    let started = Instant::now();
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let text = if audio < MIN_SPEECH || peak < SILENCE_PEAK {
        String::new()
    } else {
        let pad = (TAIL_PADDING.as_secs_f64() * f64::from(TARGET_RATE)) as usize;
        let mut padded = Vec::with_capacity(samples.len() + pad);
        padded.extend_from_slice(samples);
        padded.resize(samples.len() + pad, 0.0);
        clean_transcript(&engine.recognize(&padded))
    };
    Ok(Transcript {
        text,
        audio,
        took: started.elapsed(),
        load,
    })
}

/// Trims, collapses runs of whitespace and drops spaces before punctuation.
pub fn clean_transcript(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        let glued = word.starts_with([',', '.', '?', '!', ';', ':', '%', ')']);
        if !out.is_empty() && !glued {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

enum Job {
    Warm,
    Transcribe(Vec<f32>),
}

/// A speech-to-text worker thread. Requests are answered in order through [`poll`](Self::poll).
pub struct Transcriber {
    jobs: Sender<Job>,
    results: Receiver<Result<Transcript, SttError>>,
    pending: usize,
}

impl Transcriber {
    /// Starts the worker; `notify` is called after each result (e.g. to repaint).
    pub fn spawn(
        mut engine: Box<dyn SpeechEngine>,
        notify: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let (jobs, job_rx) = unbounded::<Job>();
        let (result_tx, results) = unbounded();
        thread::Builder::new()
            .name("speech-to-text".into())
            .spawn(move || {
                for job in job_rx {
                    match job {
                        Job::Warm => {
                            if let Err(err) = engine.load() {
                                log::info!("speech model not warmed: {err}");
                            }
                        }
                        Job::Transcribe(samples) => {
                            let result = transcribe_with(engine.as_mut(), &samples);
                            if result_tx.send(result).is_err() {
                                break;
                            }
                            if let Some(notify) = &notify {
                                notify();
                            }
                        }
                    }
                }
            })
            .expect("spawn the speech-to-text thread");
        Self {
            jobs,
            results,
            pending: 0,
        }
    }

    /// A transcriber running Parakeet from the model folders.
    pub fn parakeet(notify: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        Self::spawn(Box::new(ParakeetEngine::default()), notify)
    }

    /// Loads the model in the background so the first transcription is fast.
    pub fn warm(&self) {
        let _ = self.jobs.send(Job::Warm);
    }

    /// Queues 16 kHz mono samples.
    pub fn submit(&mut self, samples: Vec<f32>) {
        if self.jobs.send(Job::Transcribe(samples)).is_ok() {
            self.pending += 1;
        }
    }

    /// The next finished transcription.
    pub fn poll(&mut self) -> Option<Result<Transcript, SttError>> {
        let result = self.results.try_recv().ok()?;
        self.pending = self.pending.saturating_sub(1);
        Some(result)
    }

    /// Requests submitted but not yet polled.
    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Transcribes and waits for the answer.
    pub fn transcribe_blocking(&mut self, samples: Vec<f32>) -> Result<Transcript, SttError> {
        self.submit(samples);
        let result = self.results.recv().unwrap_or(Err(SttError::LoadFailed(
            "the speech worker stopped".into(),
        )));
        self.pending = self.pending.saturating_sub(1);
        result
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Returns fixed text, counting loads and recognitions.
    pub(crate) struct FakeEngine {
        pub text: String,
        pub missing: bool,
        pub loaded: bool,
        pub calls: Arc<Mutex<Vec<usize>>>,
    }

    impl FakeEngine {
        pub(crate) fn saying(text: &str) -> Self {
            Self {
                text: text.into(),
                missing: false,
                loaded: false,
                calls: Arc::default(),
            }
        }
    }

    impl SpeechEngine for FakeEngine {
        fn load(&mut self) -> Result<Option<Duration>, SttError> {
            if self.missing {
                return Err(SttError::ModelMissing);
            }
            let first = !self.loaded;
            self.loaded = true;
            Ok(first.then_some(Duration::from_millis(1)))
        }
        fn recognize(&mut self, samples: &[f32]) -> String {
            self.calls.lock().unwrap().push(samples.len());
            self.text.clone()
        }
    }

    fn speech(secs: f32) -> Vec<f32> {
        (0..(16_000.0 * secs) as usize)
            .map(|i| (i as f32 * 0.05).sin() * 0.3)
            .collect()
    }

    #[test]
    fn cleans_spacing() {
        assert_eq!(
            clean_transcript("  Show me   the stalled agents . "),
            "Show me the stalled agents."
        );
        assert_eq!(clean_transcript("yes , no ?"), "yes, no?");
        assert_eq!(clean_transcript("\n\t"), "");
        assert_eq!(clean_transcript("a\nb"), "a b");
    }

    #[test]
    fn empty_and_silent_audio_give_empty_text_without_recognizing() {
        let mut engine = FakeEngine::saying("hallucination");
        let calls = engine.calls.clone();
        assert_eq!(transcribe_with(&mut engine, &[]).unwrap().text, "");
        assert_eq!(transcribe_with(&mut engine, &speech(0.1)).unwrap().text, "");
        assert_eq!(
            transcribe_with(&mut engine, &vec![0.0; 32_000])
                .unwrap()
                .text,
            ""
        );
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn pads_the_tail_and_cleans_the_text() {
        let mut engine = FakeEngine::saying(" hello  world ");
        let calls = engine.calls.clone();
        let t = transcribe_with(&mut engine, &speech(1.0)).unwrap();
        assert_eq!(t.text, "hello world");
        assert_eq!(t.audio, Duration::from_secs(1));
        assert!(t.load.is_some(), "first call loads");
        assert_eq!(*calls.lock().unwrap(), [16_000 + 8_000]);
        let t = transcribe_with(&mut engine, &speech(1.0)).unwrap();
        assert_eq!(t.load, None, "the model stays loaded");
    }

    #[test]
    fn missing_model_is_an_error() {
        let mut engine = FakeEngine::saying("x");
        engine.missing = true;
        assert_eq!(
            transcribe_with(&mut engine, &speech(1.0)),
            Err(SttError::ModelMissing)
        );
    }

    #[test]
    fn worker_answers_in_order() {
        let mut t = Transcriber::spawn(Box::new(FakeEngine::saying("ok")), None);
        t.warm();
        t.submit(speech(0.5));
        t.submit(vec![]);
        assert_eq!(t.pending(), 2);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut got = Vec::new();
        while got.len() < 2 && Instant::now() < deadline {
            if let Some(r) = t.poll() {
                got.push(r.unwrap().text);
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(got, ["ok", ""]);
        assert_eq!(t.pending(), 0);
        assert_eq!(t.transcribe_blocking(speech(0.5)).unwrap().text, "ok");
    }

    /// Transcribes the `say`-generated fixture with the real model (Orca's copy or ours).
    /// Run with `cargo test --release -- --ignored parakeet --nocapture`.
    #[test]
    #[ignore = "needs the 640 MB Parakeet model"]
    fn parakeet_transcribes_the_fixture() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/voice/show_me_the_stalled_agents.wav"
        ))
        .unwrap();
        let (samples, rate) = super::super::audio_in::read_wav(&bytes).unwrap();
        let samples = super::super::audio_in::resample(&samples, rate, TARGET_RATE);
        let mut engine = ParakeetEngine::default();
        let first = transcribe_with(&mut engine, &samples).unwrap();
        let warm = transcribe_with(&mut engine, &samples).unwrap();
        eprintln!(
            "transcript: {:?}\naudio {:.2?}, model load {:.2?}, first run {:.2?}, warm run {:.2?}",
            warm.text, warm.audio, first.load, first.took, warm.took
        );
        let lower = warm.text.to_lowercase();
        for word in ["show", "me", "stalled", "agents"] {
            assert!(
                lower.contains(word),
                "{word:?} missing from {:?}",
                warm.text
            );
        }
    }
}
