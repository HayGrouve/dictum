#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use dictum_engine::Resampler;
use dictum_engine::model::PARAKEET_TDT_V3_INT8;

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Model directory from `DICTUM_MODEL_DIR` (downloaded on demand), or `None` to skip.
pub fn model_dir() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("DICTUM_MODEL_DIR")?);
    let dir = PARAKEET_TDT_V3_INT8.dir_in(&root);
    if !PARAKEET_TDT_V3_INT8.is_installed(&dir) {
        eprintln!("downloading model into {}", dir.display());
        PARAKEET_TDT_V3_INT8
            .download(&dir, &AtomicBool::new(false), |_, _| {})
            .expect("model download failed");
    }
    Some(dir)
}

/// Loads a WAV or FLAC file as mono 16 kHz f32.
pub fn load_audio(path: &Path) -> Vec<f32> {
    let (rate, channels, interleaved) = match path.extension().and_then(|e| e.to_str()) {
        Some("wav") => {
            let mut reader = hound::WavReader::open(path).expect("open wav");
            let spec = reader.spec();
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            let samples: Vec<f32> = match spec.sample_format {
                hound::SampleFormat::Int => {
                    reader.samples::<i32>().map(|s| s.unwrap() as f32 * scale).collect()
                }
                hound::SampleFormat::Float => reader.samples::<f32>().map(Result::unwrap).collect(),
            };
            (spec.sample_rate, spec.channels as usize, samples)
        }
        Some("flac") => {
            let mut reader = claxon::FlacReader::open(path).expect("open flac");
            let info = reader.streaminfo();
            let scale = 1.0 / (1u64 << (info.bits_per_sample - 1)) as f32;
            let samples = reader.samples().map(|s| s.unwrap() as f32 * scale).collect();
            (info.sample_rate, info.channels as usize, samples)
        }
        other => panic!("unsupported audio format {other:?}"),
    };
    let mono: Vec<f32> =
        interleaved.chunks(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32).collect();
    let mut resampler = Resampler::new(rate);
    let mut out = Vec::new();
    resampler.push(&mono, &mut out);
    resampler.flush(&mut out);
    out
}

/// (id, reference transcript) pairs.
pub fn references() -> Vec<(String, String)> {
    std::fs::read_to_string(fixtures().join("transcripts.tsv"))
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(id, text)| (id.to_string(), text.to_string()))
        .collect()
}

pub fn audio_path(id: &str) -> PathBuf {
    let wav = fixtures().join(format!("{id}.wav"));
    if wav.exists() { wav } else { fixtures().join(format!("{id}.flac")) }
}

/// Lower-case words without punctuation; "Mr." and "mister" compare equal, as do British and
/// American "-ise"/"-ize" spellings.
pub fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace(['-', '—'], " ")
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '\'')
        .collect::<String>()
        .split_whitespace()
        .map(|w| match w {
            "mr" => "mister".to_string(),
            "mrs" => "missus".to_string(),
            w => w.replace("ising", "izing").replace("ise", "ize"),
        })
        .collect()
}

/// Word-level edit distance.
pub fn word_errors(reference: &[String], hypothesis: &[String]) -> usize {
    let mut prev: Vec<usize> = (0..=hypothesis.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut cur = vec![i + 1; hypothesis.len() + 1];
        for (j, h) in hypothesis.iter().enumerate() {
            let sub = prev[j] + usize::from(r != h);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[hypothesis.len()]
}
