//! End-to-end accuracy and latency checks against real speech.
//!
//! Needs the model: set `DICTUM_MODEL_DIR` to a folder (the model is downloaded into it on first
//! use, ~670 MB). Without it these tests print a notice and pass.
//!
//! `cargo test --release -p dictum-engine --test accuracy -- --nocapture`

mod common;

use std::time::Instant;

use common::*;
use dictum_engine::{
    EngineOptions, Interrupt, Parakeet, SegmentEvent, Segmenter, SegmenterConfig, Vad, text,
};

fn engine() -> Option<Parakeet> {
    let Some(dir) = model_dir() else {
        eprintln!("DICTUM_MODEL_DIR not set; skipping model test");
        return None;
    };
    let started = Instant::now();
    let engine = Parakeet::load(&dir, &EngineOptions::default()).expect("load model");
    eprintln!("model loaded + warmed up in {:.2?}", started.elapsed());
    Some(engine)
}

#[test]
fn transcribes_reference_set_accurately_and_fast() {
    let Some(mut engine) = engine() else { return };
    let (mut errors, mut words) = (0, 0);
    let (mut audio_secs, mut compute_secs) = (0.0, 0.0);
    for (id, reference) in references() {
        let audio = load_audio(&audio_path(&id));
        let started = Instant::now();
        let transcript = engine.transcribe(&audio).unwrap();
        let elapsed = started.elapsed().as_secs_f64();
        let secs = audio.len() as f64 / 16_000.0;
        audio_secs += secs;
        compute_secs += elapsed;

        let (r, h) = (normalize(&reference), normalize(&transcript.text));
        let e = word_errors(&r, &h);
        errors += e;
        words += r.len();
        eprintln!(
            "{id}: {secs:5.1}s audio in {:4.0} ms (features {:.0?}, encoder {:.0?}, decoder {:.0?}), {e} errors\n    {}",
            elapsed * 1000.0,
            transcript.timings.features,
            transcript.timings.encoder,
            transcript.timings.decoder,
            transcript.text
        );
    }
    let wer = errors as f64 / words as f64;
    let rtf = compute_secs / audio_secs;
    let cpus = std::thread::available_parallelism().map_or(0, |n| n.get());
    eprintln!("WER {:.2}% over {words} words; real-time factor {rtf:.3} ({cpus} logical CPUs)", wer * 100.0);
    assert!(wer < 0.05, "WER too high: {:.2}%", wer * 100.0);
    assert!(rtf < 0.5, "inference far too slow: RTF {rtf:.3}");
}

/// Terms a web developer might add; none of them occur in the reference set.
const DEV_VOCABULARY: &[&str] = &[
    "Claude Code",
    "Claude",
    "Convex",
    "Vercel",
    "shadcn",
    "TanStack Query",
    "TanStack",
    "Clerk",
    "Next.js",
    "subagent",
    "MCP",
    "pnpm",
    "Turborepo",
    "AI SDK",
    "tsc",
    "ESLint",
    "GitHub",
    "Opus",
    "Sonnet",
    "Haiku",
    "diff",
    "Tailwind",
    "React",
    "TypeScript",
    "Zod",
    "tRPC",
    "Drizzle",
    "Supabase",
    "Vite",
    "LLM",
    "RAG",
    "API",
    "OAuth",
    "webhook",
    "JSON",
    "npm",
    "WSL",
    "Rust",
    "cargo",
];

#[test]
fn vocabulary_does_not_change_ordinary_speech() {
    let Some(mut plain) = engine() else { return };
    let ids: Vec<String> = references().into_iter().map(|(id, _)| id).collect();
    let audio: Vec<Vec<f32>> = ids.iter().map(|id| load_audio(&audio_path(id))).collect();
    let expected: Vec<String> = audio.iter().map(|a| plain.transcribe(a).unwrap().text).collect();
    drop(plain);

    let options = EngineOptions {
        vocabulary: DEV_VOCABULARY.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    let mut boosted = Parakeet::load(&model_dir().unwrap(), &options).expect("load model");
    for ((id, audio), expected) in ids.iter().zip(&audio).zip(&expected) {
        let text = boosted.transcribe(audio).unwrap().text;
        assert_eq!(&text, expected, "{id}: vocabulary changed the transcript");
    }
}

struct Dictation {
    text: String,
    early_segments: usize,
    pauses: usize,
    /// Transcriptions skipped because a speculative transcript matched.
    reused: usize,
    /// Transcription work left after the recording ended.
    work_at_release: std::time::Duration,
}

/// Feeds audio like a microphone would (10 ms at a time), transcribing finished segments right
/// away and speculatively transcribing at every pause, the way the app does.
fn dictate(engine: &mut Parakeet, vad: &mut Vad, audio: &[f32]) -> Dictation {
    vad.reset();
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    let mut parts = Vec::new();
    let (mut early_segments, mut pauses, mut reused) = (0, 0, 0);
    let mut speculation: Option<(dictum_engine::SegmentKey, String)> = None;
    let transcribe = |engine: &mut Parakeet,
                      segment: dictum_engine::FinalSegment,
                      spec: &mut Option<(dictum_engine::SegmentKey, String)>,
                      reused: &mut usize| {
        match spec.take() {
            Some((key, text)) if key == segment.key => {
                *reused += 1;
                text
            }
            _ => engine.transcribe(&segment.audio).unwrap().text,
        }
    };
    for chunk in audio.chunks(160) {
        for event in segmenter.push(chunk, Some(vad)).unwrap() {
            match event {
                SegmentEvent::Segment(segment) => {
                    early_segments += 1;
                    parts.push(transcribe(engine, segment, &mut speculation, &mut reused));
                }
                SegmentEvent::Pause(snapshot) => {
                    pauses += 1;
                    let text = engine.transcribe(&snapshot.audio).unwrap().text;
                    speculation = Some((snapshot.key, text));
                }
            }
        }
    }
    let started = Instant::now();
    if let Some(tail) = segmenter.finish(Some(vad)).unwrap() {
        parts.push(transcribe(engine, tail, &mut speculation, &mut reused));
    }
    Dictation {
        text: text::join_segments(parts),
        early_segments,
        pauses,
        reused,
        work_at_release: started.elapsed(),
    }
}

#[test]
fn long_dictation_is_split_and_stays_accurate() {
    let Some(mut engine) = engine() else { return };
    let mut vad = Vad::new().unwrap();

    // ~150 s of continuous "dictation": every reference utterance separated by 0.8 s pauses.
    let mut audio = Vec::new();
    let mut reference = Vec::new();
    for (id, text) in &references() {
        audio.extend(load_audio(&audio_path(id)));
        audio.extend(std::iter::repeat_n(0.0f32, 12_800));
        reference.extend(normalize(text));
    }

    let d = dictate(&mut engine, &mut vad, &audio);
    let wer = word_errors(&reference, &normalize(&d.text)) as f64 / reference.len() as f64;
    eprintln!(
        "{:.0}s dictation: {} early segments, {} pauses, {} reused speculations, work at release {:.0?}, WER {:.2}%",
        audio.len() as f64 / 16_000.0,
        d.early_segments,
        d.pauses,
        d.reused,
        d.work_at_release,
        wer * 100.0
    );
    assert!(d.early_segments >= 3, "expected the long dictation to be split");
    assert!(d.reused >= 1, "speculative transcripts should be reused for splits");
    assert!(wer < 0.06, "WER too high after segmentation: {:.2}%", wer * 100.0);
}

#[test]
fn speaking_then_pausing_makes_the_final_text_ready_early() {
    let Some(mut engine) = engine() else { return };
    let mut vad = Vad::new().unwrap();
    for id in ["1272-128104-0000", "jfk"] {
        let mut audio = load_audio(&audio_path(id));
        audio.extend(std::iter::repeat_n(0.0f32, 16_000)); // the user waits a second, then releases
        let d = dictate(&mut engine, &mut vad, &audio);
        eprintln!("{id}: work at release {:.0?}, reused {}: {}", d.work_at_release, d.reused, d.text);
        assert!(d.reused >= 1, "{id}: speculative transcript was not reused");
        assert!(d.work_at_release.as_millis() < 50, "{id}: transcription happened after release");
    }
}

#[test]
fn silence_and_noise_produce_no_text() {
    let Some(mut engine) = engine() else { return };
    let mut vad = Vad::new().unwrap();

    let silence = vec![0.0f32; 32_000];
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    segmenter.push(&silence, Some(&mut vad)).unwrap();
    assert!(segmenter.finish(Some(&mut vad)).unwrap().is_none(), "silence must be dropped by the VAD");
    assert!(!segmenter.heard_speech());

    // Even if something slips past the VAD, the recogniser must not invent words from hiss.
    let mut seed = 0x2545_f491u32;
    let hiss: Vec<f32> = (0..32_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32 - 0.5) * 0.01
        })
        .collect();
    let text = engine.transcribe(&hiss).unwrap().text;
    assert!(text.split_whitespace().count() <= 1, "hallucinated: {text:?}");
}

#[test]
fn interrupt_stops_transcription() {
    let Some(mut engine) = engine() else { return };
    let audio = load_audio(&audio_path("jfk"));
    let interrupt = Interrupt::new().unwrap();
    interrupt.trigger();
    assert!(engine.transcribe_interruptible(&audio, &interrupt).unwrap().is_none());
    interrupt.reset();
    let text = engine.transcribe_interruptible(&audio, &interrupt).unwrap().unwrap().text;
    assert!(text.contains("fellow Americans"), "{text}");
}
