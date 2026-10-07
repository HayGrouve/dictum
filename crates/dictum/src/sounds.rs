//! Audio cues synthesised as in-memory WAV files (no assets to ship).

use crate::ui::Cue;

const RATE: u32 = 24_000;
const VOLUME: f32 = 0.18;

/// (frequency Hz, duration ms) segments for each cue.
fn notes(cue: Cue) -> &'static [(f32, u32)] {
    match cue {
        Cue::Start => &[(784.0, 45), (1047.0, 60)],
        Cue::Stop => &[(1047.0, 45), (784.0, 60)],
        Cue::Lock => &[(1047.0, 40), (1047.0, 40)],
        Cue::Cancel => &[(523.0, 50), (392.0, 70)],
        Cue::Error => &[(330.0, 160)],
    }
}

pub fn wav(cue: Cue) -> Vec<u8> {
    let mut samples: Vec<i16> = Vec::new();
    for &(freq, ms) in notes(cue) {
        let n = (RATE * ms / 1000) as usize;
        let fade = (RATE as usize * 6 / 1000).min(n / 2);
        for i in 0..n {
            let env = if i < fade {
                i as f32 / fade as f32
            } else if i >= n - fade {
                (n - i) as f32 / fade as f32
            } else {
                1.0
            };
            let s = (2.0 * std::f32::consts::PI * freq * i as f32 / RATE as f32).sin();
            samples.push((s * env * VOLUME * i16::MAX as f32) as i16);
        }
    }
    encode(&samples)
}

fn encode(samples: &[i16]) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_valid_wavs() {
        for cue in [Cue::Start, Cue::Stop, Cue::Lock, Cue::Cancel, Cue::Error] {
            let bytes = wav(cue);
            let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
            assert_eq!(reader.spec().sample_rate, RATE);
            let ms = reader.duration() * 1000 / RATE;
            assert!((60..=200).contains(&ms), "{cue:?} lasts {ms} ms");
        }
    }
}
