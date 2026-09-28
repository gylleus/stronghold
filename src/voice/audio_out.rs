//! Audio playback.
//!
//! [`AudioSink`] is where synthesized speech goes: mono `f32` samples at the sink's rate,
//! appended to a play queue that can be flushed instantly. [`CpalSink`] plays through the
//! default output device; [`BufferSink`] only collects (tests, rendering to WAV).

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

/// Somewhere to play mono audio.
pub trait AudioSink: Send + Sync {
    /// Rate the samples passed to [`push`](Self::push) must have.
    fn sample_rate(&self) -> u32;
    /// Appends samples to the play queue.
    fn push(&self, samples: &[f32]);
    /// Drops everything not yet played.
    fn clear(&self);
    /// Samples queued but not yet played.
    fn pending(&self) -> usize;
}

/// A sink that plays nothing: pushes accumulate until taken or cleared.
#[derive(Debug)]
pub struct BufferSink {
    rate: u32,
    queue: Mutex<Vec<f32>>,
}

impl BufferSink {
    pub fn new(rate: u32) -> Self {
        BufferSink {
            rate,
            queue: Mutex::new(Vec::new()),
        }
    }

    /// Takes the queued samples, as if they had been played.
    pub fn take(&self) -> Vec<f32> {
        std::mem::take(&mut *self.queue.lock().unwrap())
    }
}

impl AudioSink for BufferSink {
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn push(&self, samples: &[f32]) {
        self.queue.lock().unwrap().extend_from_slice(samples);
    }
    fn clear(&self) {
        self.queue.lock().unwrap().clear();
    }
    fn pending(&self) -> usize {
        self.queue.lock().unwrap().len()
    }
}

/// Pause the output stream after this long with nothing to play.
const IDLE_PAUSE: Duration = Duration::from_secs(3);

/// Plays through the system's default output device.
///
/// The cpal stream lives on its own thread (it is not `Send` on every platform); it is
/// paused while idle so the device is not held open, and resumed by the next push.
pub struct CpalSink {
    rate: u32,
    queue: Arc<Mutex<VecDeque<f32>>>,
    paused: Arc<AtomicBool>,
    wake: Sender<()>,
}

impl CpalSink {
    /// Opens the default output device at its default rate.
    pub fn open_default() -> anyhow::Result<Self> {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let paused = Arc::new(AtomicBool::new(false));
        let (wake_tx, wake_rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let (q, p) = (queue.clone(), paused.clone());
        std::thread::Builder::new()
            .name("audio-out".into())
            .spawn(move || stream_thread(q, p, wake_rx, ready_tx))?;
        let rate = ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("audio output thread exited"))??;
        Ok(CpalSink {
            rate,
            queue,
            paused,
            wake: wake_tx,
        })
    }
}

impl AudioSink for CpalSink {
    fn sample_rate(&self) -> u32 {
        self.rate
    }
    fn push(&self, samples: &[f32]) {
        self.queue.lock().unwrap().extend(samples);
        if self.paused.load(Ordering::Acquire) {
            let _ = self.wake.send(());
        }
    }
    fn clear(&self) {
        self.queue.lock().unwrap().clear();
    }
    fn pending(&self) -> usize {
        self.queue.lock().unwrap().len()
    }
}

/// Owns the stream until the sink (and with it the wake sender) is dropped.
fn stream_thread(
    queue: Arc<Mutex<VecDeque<f32>>>,
    paused: Arc<AtomicBool>,
    wake: Receiver<()>,
    ready: Sender<anyhow::Result<u32>>,
) {
    let stream = match build_stream(queue.clone()) {
        Ok((stream, rate)) => {
            let _ = ready.send(Ok(rate));
            stream
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut idle_since: Option<Instant> = None;
    loop {
        match wake.recv_timeout(Duration::from_millis(500)) {
            Ok(()) => {
                if paused.swap(false, Ordering::AcqRel)
                    && let Err(e) = stream.play()
                {
                    log::warn!("audio output: resume failed: {e}");
                }
                idle_since = None;
            }
            Err(RecvTimeoutError::Timeout) => {
                if paused.load(Ordering::Acquire) {
                    continue;
                }
                if !queue.lock().unwrap().is_empty() {
                    idle_since = None;
                    continue;
                }
                let since = *idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= IDLE_PAUSE {
                    // Set first so a push racing with the pause still sends a wake.
                    paused.store(true, Ordering::Release);
                    if !queue.lock().unwrap().is_empty() {
                        paused.store(false, Ordering::Release);
                        continue;
                    }
                    if let Err(e) = stream.pause() {
                        log::warn!("audio output: pause failed: {e}");
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn build_stream(queue: Arc<Mutex<VecDeque<f32>>>) -> anyhow::Result<(cpal::Stream, u32)> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no audio output device"))?;
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let rate = config.sample_rate;
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, queue)?,
        SampleFormat::I16 => build::<i16>(&device, config, queue)?,
        SampleFormat::I32 => build::<i32>(&device, config, queue)?,
        SampleFormat::U16 => build::<u16>(&device, config, queue)?,
        SampleFormat::F64 => build::<f64>(&device, config, queue)?,
        other => anyhow::bail!("unsupported output sample format {other}"),
    };
    stream.play()?;
    Ok((stream, rate))
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    queue: Arc<Mutex<VecDeque<f32>>>,
) -> anyhow::Result<cpal::Stream> {
    let channels = usize::from(config.channels).max(1);
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut queue = queue.lock().unwrap();
            for frame in out.chunks_mut(channels) {
                let v = T::from_sample(queue.pop_front().unwrap_or(0.0));
                frame.fill(v);
            }
        },
        |e| log::warn!("audio output error: {e}"),
        None,
    )?;
    Ok(stream)
}

/// Band-limited resampling (windowed sinc) of mono audio from `from` Hz to `to` Hz.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    const ZERO_CROSSINGS: f64 = 16.0;
    let ratio = f64::from(to) / f64::from(from);
    // Cutoff relative to the input Nyquist: below the output Nyquist when downsampling.
    let cutoff = ratio.min(1.0) * 0.95;
    let half = (ZERO_CROSSINGS / cutoff).ceil();
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let last = input.len() as i64 - 1;
    (0..out_len)
        .map(|j| {
            let t = j as f64 / ratio;
            let lo = ((t - half).ceil() as i64).max(0);
            let hi = ((t + half).floor() as i64).min(last);
            let mut acc = 0.0f64;
            for (k, &x) in input
                .iter()
                .enumerate()
                .take(hi as usize + 1)
                .skip(lo as usize)
            {
                let d = t - k as f64;
                acc += f64::from(x) * cutoff * sinc(cutoff * d) * blackman(d / half);
            }
            acc as f32
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

/// Blackman window over `u` in [-1, 1].
fn blackman(u: f64) -> f64 {
    if u.abs() >= 1.0 {
        return 0.0;
    }
    let a = std::f64::consts::PI * u;
    0.42 + 0.5 * a.cos() + 0.08 * (2.0 * a).cos()
}

/// Writes mono samples as a 16-bit PCM WAV file.
pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> io::Result<()> {
    let mut out = io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&1u16.to_le_bytes())?; // mono
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * 2).to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        out.write_all(&v.to_le_bytes())?;
    }
    out.flush()
}

/// Root-mean-square level of `samples`.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    #[test]
    fn resample_is_identity_at_equal_rates() {
        let x = sine(440.0, 24_000, 100);
        assert_eq!(resample(&x, 24_000, 24_000), x);
        assert!(resample(&[], 24_000, 48_000).is_empty());
    }

    #[test]
    fn upsampling_preserves_a_tone() {
        let x = sine(440.0, 24_000, 24_000);
        let y = resample(&x, 24_000, 48_000);
        assert_eq!(y.len(), 48_000);
        let want = sine(440.0, 48_000, 48_000);
        // Away from the edges the result matches the ideal tone at the new rate.
        let err = y[1000..47_000]
            .iter()
            .zip(&want[1000..47_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 0.01, "max error {err}");
    }

    #[test]
    fn resampling_to_44_1k_keeps_level_and_length() {
        let x = sine(1000.0, 24_000, 24_000);
        let y = resample(&x, 24_000, 44_100);
        assert_eq!(y.len(), 44_100);
        let (a, b) = (rms(&x[500..23_500]), rms(&y[1000..43_000]));
        assert!((a - b).abs() < 0.01, "rms {a} vs {b}");
    }

    #[test]
    fn downsampling_filters_tones_above_the_new_nyquist() {
        // 10 kHz is above 16 kHz's Nyquist (8 kHz): it must not alias into the output.
        let x = sine(10_000.0, 48_000, 48_000);
        let y = resample(&x, 48_000, 16_000);
        assert_eq!(y.len(), 16_000);
        assert!(rms(&y[500..15_500]) < 0.02);
    }

    #[test]
    fn buffer_sink_queues_and_clears() {
        let sink = BufferSink::new(48_000);
        sink.push(&[0.1, 0.2]);
        sink.push(&[0.3]);
        assert_eq!(sink.pending(), 3);
        sink.clear();
        assert_eq!(sink.pending(), 0);
        sink.push(&[0.5]);
        assert_eq!(sink.take(), [0.5]);
        assert_eq!(sink.pending(), 0);
    }

    #[test]
    fn writes_a_wav_header_and_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        write_wav(&path, &[0.0, 1.0, -1.0], 24_000).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 6);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            24_000
        );
        assert_eq!(i16::from_le_bytes([bytes[46], bytes[47]]), i16::MAX);
    }
}
