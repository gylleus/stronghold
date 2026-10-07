//! The worktree saw's sound: a short chainsaw rev, synthesized (no audio asset to bundle),
//! written once to the data dir and played with `afplay` like the other sounds.

use std::path::PathBuf;

const RATE: u32 = 22_050;
const SECONDS: f32 = 1.3;
const FILE: &str = "chainsaw.wav";

/// A 16-bit mono WAV of a two-stroke engine revving up, cutting and letting off: a sawtooth
/// at the firing rate with a rasp of noise riding on each stroke, softly clipped.
pub fn wav() -> Vec<u8> {
    let n = (RATE as f32 * SECONDS) as usize;
    let mut phase = 0.0f32;
    let mut seed = 0x2545_f491u32;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let rev = smoothstep(0.0, 0.25, t) - 0.4 * smoothstep(1.0, SECONDS, t);
        let hz = 48.0 + 80.0 * rev + 4.0 * (t * 37.0).sin();
        phase = (phase + hz / RATE as f32).fract();
        let saw = 2.0 * phase - 1.0;
        let buzz = if (phase * 2.0).fract() < 0.5 {
            0.5
        } else {
            -0.5
        };
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
        let rasp = noise * (0.25 + 0.35 * (1.0 - phase)) * (0.4 + 0.6 * rev);
        let x = (2.4 * (0.55 * saw + 0.25 * buzz + rasp)).tanh();
        let envelope = smoothstep(0.0, 0.03, t) * (1.0 - smoothstep(SECONDS - 0.15, SECONDS, t));
        samples.push((x * envelope * 0.5 * f32::from(i16::MAX)) as i16);
    }
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn smoothstep(from: f32, to: f32, t: f32) -> f32 {
    let u = ((t - from) / (to - from)).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// `<data dir>/chainsaw.wav`, written when missing or stale.
fn path() -> std::io::Result<PathBuf> {
    let path = crate::claude::hook::data_dir().join(FILE);
    let bytes = wav();
    if std::fs::metadata(&path).map_or(true, |m| m.len() != bytes.len() as u64) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, bytes)?;
    }
    Ok(path)
}

/// Plays the rev at `volume` (0–1) without waiting for it.
pub fn play(volume: f32) {
    let played = path()
        .map_err(anyhow::Error::from)
        .and_then(|p| crate::notify::play_sound(&p, volume));
    if let Err(e) = played {
        log::warn!("chainsaw sound: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_is_a_short_mono_pcm_clip() {
        let wav = wav();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let data = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(data + 44, wav.len());
        let seconds = data as f32 / 2.0 / RATE as f32;
        assert!((1.0..2.0).contains(&seconds), "{seconds} s");
        assert_eq!(
            wav,
            super::wav(),
            "the same every time, so the file is written once"
        );
        let loudest = wav[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(loudest > 8_000 && loudest < i16::MAX as u16, "{loudest}");
    }
}
