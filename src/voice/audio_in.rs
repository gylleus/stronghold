//! Microphone capture: a [`Recorder`] records the default input device on its own thread and
//! hands back 16 kHz mono samples for speech-to-text.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};

/// Sample rate the recognizer takes.
pub const TARGET_RATE: u32 = 16_000;
/// Longest recording kept; capture stops adding samples past it.
pub const MAX_RECORDING: Duration = Duration::from_secs(120);
/// A recording at least this long with nothing but exact zeros means macOS muted the
/// microphone, which it does when access is denied.
const SILENT_PERMISSION_CHECK: Duration = Duration::from_millis(400);

/// Why recording failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    #[error("No microphone found. Connect one or pick an input in System Settings › Sound.")]
    NoInputDevice,
    #[error("{}", permission_help())]
    PermissionDenied,
    #[error("Microphone error: {0}")]
    Device(String),
    #[error("Not recording")]
    NotRecording,
}

/// What to tell the user when macOS blocks the microphone.
pub fn permission_help() -> &'static str {
    "No sound from the microphone: macOS may be blocking access. Allow it in System Settings › \
     Privacy & Security › Microphone. Run from a terminal, macOS asks for the terminal app's \
     permission; the bundled Myrstack.app asks for its own."
}

/// A finished recording, resampled to [`TARGET_RATE`] mono.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub samples: Vec<f32>,
    /// Capture stopped at [`MAX_RECORDING`].
    pub truncated: bool,
}

impl Recording {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(TARGET_RATE))
    }
}

// ---------------------------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------------------------

/// Where captured audio goes: the source pushes interleaved frames from its callback.
#[derive(Clone)]
pub struct SampleSink {
    shared: Arc<Shared>,
}

struct Shared {
    /// Mono samples at `rate`.
    buffer: Mutex<Vec<f32>>,
    rate: AtomicU32,
    /// Smoothed input level in 0..=1, as `f32` bits.
    level: AtomicU32,
    /// Any sample was not exactly zero.
    heard: AtomicBool,
    full: AtomicBool,
    error: Mutex<Option<RecordError>>,
    max: Duration,
}

impl SampleSink {
    fn new(max: Duration) -> Self {
        Self {
            shared: Arc::new(Shared {
                buffer: Mutex::new(Vec::new()),
                rate: AtomicU32::new(0),
                level: AtomicU32::new(0),
                heard: AtomicBool::new(false),
                full: AtomicBool::new(false),
                error: Mutex::new(None),
                max,
            }),
        }
    }

    /// Sets the stream's sample rate; call before the first [`push`](Self::push).
    pub fn set_rate(&self, rate: u32) {
        self.shared.rate.store(rate, Ordering::Relaxed);
        let cap = (f64::from(rate) * self.shared.max.as_secs_f64()) as usize;
        let mut buffer = self
            .shared
            .buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        buffer.reserve(cap.min(rate as usize * 10));
    }

    /// Adds interleaved frames with `channels` channels.
    pub fn push(&self, interleaved: &[f32], channels: u16) {
        let mono = downmix(interleaved, channels);
        if mono.iter().any(|s| *s != 0.0) {
            self.shared.heard.store(true, Ordering::Relaxed);
        }
        let old = f32::from_bits(self.shared.level.load(Ordering::Relaxed));
        let level = smooth_level(old, meter_level(&mono));
        self.shared.level.store(level.to_bits(), Ordering::Relaxed);

        let rate = self.shared.rate.load(Ordering::Relaxed);
        let max = (f64::from(rate) * self.shared.max.as_secs_f64()) as usize;
        let mut buffer = self
            .shared
            .buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let room = max.saturating_sub(buffer.len());
        if mono.len() >= room {
            self.shared.full.store(true, Ordering::Relaxed);
        }
        buffer.extend_from_slice(&mono[..mono.len().min(room)]);
    }

    /// Reports a stream failure (e.g. the device went away).
    pub fn fail(&self, error: RecordError) {
        let mut slot = self
            .shared
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slot.get_or_insert(error);
    }
}

/// Keeps a started capture running until dropped.
pub trait CaptureGuard {}
impl<T> CaptureGuard for T {}

/// An audio input. `start` runs on the recorder's capture thread, which keeps the returned
/// guard (it need not be `Send`) until the recording ends.
pub trait InputSource: Send + Sync + 'static {
    fn start(&self, sink: SampleSink) -> Result<Box<dyn CaptureGuard>, RecordError>;
}

/// The default input device, through cpal.
#[derive(Debug, Default, Clone, Copy)]
pub struct CpalSource;

impl InputSource for CpalSource {
    fn start(&self, sink: SampleSink) -> Result<Box<dyn CaptureGuard>, RecordError> {
        use cpal::SampleFormat as F;
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(RecordError::NoInputDevice)?;
        let supported = device.default_input_config().map_err(device_error)?;
        let config = supported.config();
        let channels = config.channels;
        sink.set_rate(config.sample_rate);
        let stream = match supported.sample_format() {
            F::F32 => build_stream::<f32>(&device, config, channels, sink),
            F::I16 => build_stream::<i16>(&device, config, channels, sink),
            F::I32 => build_stream::<i32>(&device, config, channels, sink),
            F::U16 => build_stream::<u16>(&device, config, channels, sink),
            F::U8 => build_stream::<u8>(&device, config, channels, sink),
            F::I8 => build_stream::<i8>(&device, config, channels, sink),
            F::F64 => build_stream::<f64>(&device, config, channels, sink),
            other => Err(RecordError::Device(format!(
                "unsupported sample format {other:?}"
            ))),
        }?;
        stream.play().map_err(device_error)?;
        Ok(Box::new(stream))
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: u16,
    sink: SampleSink,
) -> Result<cpal::Stream, RecordError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;

    let error_sink = sink.clone();
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                scratch.clear();
                scratch.extend(data.iter().map(|s| s.to_sample::<f32>()));
                sink.push(&scratch, channels);
            },
            move |err: cpal::Error| {
                // A reroute to a new default device keeps the stream running.
                if err.kind() != cpal::ErrorKind::DeviceChanged {
                    error_sink.fail(device_error(err));
                }
            },
            Some(Duration::from_secs(3)),
        )
        .map_err(device_error)
}

fn device_error(err: cpal::Error) -> RecordError {
    match err.kind() {
        cpal::ErrorKind::PermissionDenied => RecordError::PermissionDenied,
        cpal::ErrorKind::DeviceNotAvailable => RecordError::NoInputDevice,
        _ => RecordError::Device(err.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// Recorder
// ---------------------------------------------------------------------------------------------

/// Push-to-talk capture: `start`, then `stop` (or `cancel`). Capture runs on its own thread.
pub struct Recorder {
    source: Arc<dyn InputSource>,
    max: Duration,
    active: Option<Active>,
}

struct Active {
    sink: SampleSink,
    stop: Sender<()>,
    started: Receiver<Result<(), RecordError>>,
    thread: JoinHandle<()>,
    since: Instant,
}

impl Recorder {
    pub fn new(source: Arc<dyn InputSource>) -> Self {
        Self {
            source,
            max: MAX_RECORDING,
            active: None,
        }
    }

    /// Records the default microphone.
    pub fn microphone() -> Self {
        Self::new(Arc::new(CpalSource))
    }

    pub fn with_max(mut self, max: Duration) -> Self {
        self.max = max;
        self
    }

    /// Starts capturing (a no-op while already recording). Opening the device happens on the
    /// capture thread; a failure shows up in [`failure`](Self::failure) and [`stop`](Self::stop).
    pub fn start(&mut self) {
        if self.active.is_some() {
            return;
        }
        let sink = SampleSink::new(self.max);
        let (stop_tx, stop_rx) = bounded::<()>(1);
        let (started_tx, started_rx) = bounded(1);
        let source = self.source.clone();
        let thread_sink = sink.clone();
        let thread = thread::Builder::new()
            .name("audio-capture".into())
            .spawn(move || match source.start(thread_sink) {
                Ok(guard) => {
                    let _ = started_tx.send(Ok(()));
                    let _ = stop_rx.recv();
                    drop(guard);
                }
                Err(err) => {
                    let _ = started_tx.send(Err(err));
                }
            })
            .expect("spawn the audio capture thread");
        self.active = Some(Active {
            sink,
            stop: stop_tx,
            started: started_rx,
            thread,
            since: Instant::now(),
        });
    }

    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    /// How long the current recording has run.
    pub fn elapsed(&self) -> Duration {
        self.active
            .as_ref()
            .map_or(Duration::ZERO, |a| a.since.elapsed())
    }

    /// Input level in 0..=1 for a meter.
    pub fn level(&self) -> f32 {
        self.active.as_ref().map_or(0.0, |a| {
            f32::from_bits(a.sink.shared.level.load(Ordering::Relaxed))
        })
    }

    /// The recording reached its maximum length.
    pub fn is_full(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|a| a.sink.shared.full.load(Ordering::Relaxed))
    }

    /// A failure so far: the device could not open or the stream broke.
    pub fn failure(&self) -> Option<RecordError> {
        let active = self.active.as_ref()?;
        if let Ok(Err(err)) = active.started.try_recv() {
            active.sink.fail(err);
        }
        active
            .sink
            .shared
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stops capturing and returns the audio at 16 kHz mono.
    pub fn stop(&mut self) -> Result<Recording, RecordError> {
        let failure = self.failure();
        let active = self.active.take().ok_or(RecordError::NotRecording)?;
        let elapsed = active.since.elapsed();
        let _ = active.stop.try_send(());
        let _ = active.thread.join();
        if let Some(err) = failure {
            return Err(err);
        }
        if let Ok(Err(err)) = active.started.try_recv() {
            return Err(err);
        }
        let shared = &active.sink.shared;
        let raw =
            std::mem::take(&mut *shared.buffer.lock().unwrap_or_else(PoisonError::into_inner));
        if !shared.heard.load(Ordering::Relaxed) && elapsed >= SILENT_PERMISSION_CHECK {
            return Err(RecordError::PermissionDenied);
        }
        let rate = shared.rate.load(Ordering::Relaxed);
        Ok(Recording {
            samples: resample(&raw, rate, TARGET_RATE),
            truncated: shared.full.load(Ordering::Relaxed),
        })
    }

    /// Stops capturing and drops the audio.
    pub fn cancel(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = active.stop.try_send(());
            let _ = active.thread.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.cancel();
    }
}

// ---------------------------------------------------------------------------------------------
// Signal helpers
// ---------------------------------------------------------------------------------------------

/// Averages interleaved channels into mono.
pub fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => interleaved.to_vec(),
        n => {
            let n = usize::from(n);
            interleaved
                .chunks_exact(n)
                .map(|frame| frame.iter().sum::<f32>() / n as f32)
                .collect()
        }
    }
}

/// Zero crossings on each side of a windowed-sinc tap.
const SINC_ZEROS: f64 = 8.0;

/// Resamples mono audio with a Hann-windowed sinc filter (low-passed below the lower
/// Nyquist rate, so downsampling does not alias).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || from == 0 || to == 0 || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let cutoff = ratio.min(1.0);
    let half_width = SINC_ZEROS / cutoff;
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let last = input.len() as isize - 1;
    (0..out_len)
        .map(|i| {
            let center = i as f64 / ratio;
            let lo = (center - half_width).ceil().max(0.0) as isize;
            let hi = ((center + half_width).floor() as isize).min(last);
            let (mut acc, mut norm) = (0.0f64, 0.0f64);
            for k in lo..=hi {
                let x = k as f64 - center;
                let window = 0.5 + 0.5 * (std::f64::consts::PI * x / half_width).cos();
                let w = cutoff * sinc(cutoff * x) * window;
                acc += w * f64::from(input[k as usize]);
                norm += w;
            }
            if norm.abs() > 1e-9 {
                (acc / norm) as f32
            } else {
                0.0
            }
        })
        .collect()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Meter value in 0..=1 for a block: its RMS on a -60..0 dBFS scale.
pub fn meter_level(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let rms = (block.iter().map(|s| s * s).sum::<f32>() / block.len() as f32).sqrt();
    if rms <= 1e-6 {
        return 0.0;
    }
    ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Fast attack, slow release.
fn smooth_level(old: f32, new: f32) -> f32 {
    if new > old {
        new
    } else {
        old * 0.8 + new * 0.2
    }
}

/// Reads a PCM WAV file (8/16/24/32-bit integer or 32-bit float) as mono samples and its
/// sample rate.
pub fn read_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let mut pos = 12;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32_at(bytes, pos + 4) as usize;
        let body = pos + 8;
        let end = (body + len).min(bytes.len());
        match id {
            b"fmt " if len >= 16 => {
                format = Some((
                    u16_at(bytes, body),
                    u16_at(bytes, body + 2),
                    u32_at(bytes, body + 4),
                    u16_at(bytes, body + 14),
                ));
            }
            b"data" => {
                let (tag, channels, rate, bits) = format.ok_or("data before fmt")?;
                let data = &bytes[body..end];
                let samples: Vec<f32> = match (tag, bits) {
                    (1, 8) => data
                        .iter()
                        .map(|b| (f32::from(*b) - 128.0) / 128.0)
                        .collect(),
                    (1, 16) => data
                        .chunks_exact(2)
                        .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
                        .collect(),
                    (1, 24) => data
                        .chunks_exact(3)
                        .map(|c| i32::from_le_bytes([0, c[0], c[1], c[2]]) as f32 / 2_147_483_648.0)
                        .collect(),
                    (1, 32) => data
                        .chunks_exact(4)
                        .map(|c| {
                            i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0
                        })
                        .collect(),
                    (3, 32) => data
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect(),
                    _ => return Err(format!("unsupported WAV format {tag}/{bits}-bit")),
                };
                return Ok((downmix(&samples, channels), rate));
            }
            _ => {}
        }
        pos = body + len + (len & 1);
    }
    Err("no data chunk".into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A source that plays fixed frames on start, or fails.
    pub(crate) struct FakeSource {
        pub rate: u32,
        pub channels: u16,
        pub frames: Vec<f32>,
        pub fail: Option<RecordError>,
    }

    impl FakeSource {
        pub(crate) fn tone(rate: u32, channels: u16, secs: f32) -> Self {
            let n = (rate as f32 * secs) as usize;
            let frames = (0..n)
                .flat_map(|i| {
                    let s = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                    std::iter::repeat_n(s, usize::from(channels))
                })
                .collect();
            Self {
                rate,
                channels,
                frames,
                fail: None,
            }
        }
    }

    impl InputSource for FakeSource {
        fn start(&self, sink: SampleSink) -> Result<Box<dyn CaptureGuard>, RecordError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            sink.set_rate(self.rate);
            for chunk in self.frames.chunks(512 * usize::from(self.channels)) {
                sink.push(chunk, self.channels);
            }
            Ok(Box::new(()))
        }
    }

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(
            downmix(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2),
            [0.5, 0.5, 0.0]
        );
        assert_eq!(downmix(&[0.1, 0.2], 1), [0.1, 0.2]);
        assert_eq!(downmix(&[0.3, 0.3, 0.3, 0.9], 3), [0.3]);
        assert!(downmix(&[1.0], 0).is_empty());
    }

    fn tone(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn resample_keeps_length_ratio_and_passband() {
        let input = tone(48_000, 440.0, 0.5);
        let out = resample(&input, 48_000, 16_000);
        assert_eq!(out.len(), 8_000);
        // A 440 Hz tone survives with its amplitude and matches the tone at 16 kHz.
        let expected = tone(16_000, 440.0, 0.5);
        let mid = &out[500..7_500];
        assert!((rms(mid) - rms(&expected[500..7_500])).abs() < 0.02);
        let err = mid
            .iter()
            .zip(&expected[500..7_500])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 0.02, "max error {err}");

        let up = resample(&tone(8_000, 300.0, 0.25), 8_000, 16_000);
        assert_eq!(up.len(), 4_000);
        assert_eq!(resample(&input, 16_000, 16_000), input);
        assert!(resample(&[], 44_100, 16_000).is_empty());
    }

    #[test]
    fn resample_filters_out_what_the_target_rate_cannot_hold() {
        // 12 kHz is above 16 kHz's Nyquist limit: it must not alias into the passband.
        let out = resample(&tone(48_000, 12_000.0, 0.5), 48_000, 16_000);
        assert!(rms(&out[500..7_500]) < 0.05, "aliased rms {}", rms(&out));
    }

    #[test]
    fn meter_maps_rms_to_a_db_scale() {
        assert_eq!(meter_level(&[]), 0.0);
        assert_eq!(meter_level(&[0.0; 64]), 0.0);
        assert!((meter_level(&[1.0; 64]) - 1.0).abs() < 1e-6);
        let quiet = meter_level(&[0.01; 64]);
        assert!(quiet > 0.2 && quiet < 0.5, "{quiet}");
        assert!(smooth_level(0.2, 0.9) == 0.9);
        assert!(smooth_level(0.9, 0.0) < 0.9);
    }

    fn wait_for_capture(recorder: &Recorder) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while recorder.level() == 0.0 && recorder.failure().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn recorder_captures_downmixes_and_resamples() {
        let mut recorder = Recorder::new(Arc::new(FakeSource::tone(48_000, 2, 1.0)));
        assert!(!recorder.is_recording());
        assert_eq!(recorder.stop(), Err(RecordError::NotRecording));
        recorder.start();
        assert!(recorder.is_recording());
        wait_for_capture(&recorder);
        assert!(recorder.level() > 0.5);
        let recording = recorder.stop().unwrap();
        assert!(!recorder.is_recording());
        assert_eq!(recording.samples.len(), 16_000);
        assert!(!recording.truncated);
        assert!((recording.duration().as_secs_f32() - 1.0).abs() < 1e-3);
        assert!((rms(&recording.samples[100..15_900]) - 0.5 / 2f32.sqrt()).abs() < 0.02);
    }

    #[test]
    fn recorder_caps_the_length() {
        let mut recorder = Recorder::new(Arc::new(FakeSource::tone(16_000, 1, 2.0)))
            .with_max(Duration::from_millis(500));
        recorder.start();
        wait_for_capture(&recorder);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !recorder.is_full() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(recorder.is_full());
        let recording = recorder.stop().unwrap();
        assert_eq!(recording.samples.len(), 8_000);
        assert!(recording.truncated);
    }

    #[test]
    fn recorder_reports_start_failures() {
        let mut recorder = Recorder::new(Arc::new(FakeSource {
            rate: 16_000,
            channels: 1,
            frames: vec![],
            fail: Some(RecordError::NoInputDevice),
        }));
        recorder.start();
        wait_for_capture(&recorder);
        assert_eq!(recorder.failure(), Some(RecordError::NoInputDevice));
        assert_eq!(recorder.stop(), Err(RecordError::NoInputDevice));
        assert!(!recorder.is_recording());
    }

    #[test]
    fn digital_silence_means_the_microphone_is_blocked() {
        let mut source = FakeSource::tone(16_000, 1, 1.0);
        source.frames.iter_mut().for_each(|s| *s = 0.0);
        let mut recorder = Recorder::new(Arc::new(source));
        recorder.start();
        thread::sleep(SILENT_PERMISSION_CHECK + Duration::from_millis(20));
        assert_eq!(recorder.stop(), Err(RecordError::PermissionDenied));

        // A short tap is not enough to tell.
        let mut source = FakeSource::tone(16_000, 1, 0.1);
        source.frames.iter_mut().for_each(|s| *s = 0.0);
        let mut recorder = Recorder::new(Arc::new(source));
        recorder.start();
        assert!(recorder.stop().is_ok());
    }

    #[test]
    fn cancel_drops_the_audio_and_allows_a_new_start() {
        let mut recorder = Recorder::new(Arc::new(FakeSource::tone(16_000, 1, 0.2)));
        recorder.start();
        recorder.start();
        recorder.cancel();
        assert!(!recorder.is_recording());
        assert_eq!(recorder.level(), 0.0);
        recorder.start();
        wait_for_capture(&recorder);
        assert_eq!(recorder.stop().unwrap().samples.len(), 3_200);
    }

    #[test]
    fn reads_the_speech_fixture() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/voice/show_me_the_stalled_agents.wav"
        ))
        .unwrap();
        let (samples, rate) = read_wav(&bytes).unwrap();
        assert_eq!(rate, 16_000);
        assert!(samples.len() > 16_000, "over a second of speech");
        assert!(rms(&samples) > 0.01);
        assert!(read_wav(b"RIFF0000WAVE").is_err());
        assert!(read_wav(b"nope").is_err());
    }
}
