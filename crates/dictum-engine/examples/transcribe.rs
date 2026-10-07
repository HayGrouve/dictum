//! Transcribe audio files from the command line and report timings.
//!
//! cargo run --release -p dictum-engine --example transcribe -- [--gpu] <model-dir> <file.wav|file.flac>...

#[path = "../tests/common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::time::Instant;

use dictum_engine::{Device, EngineOptions, Parakeet};

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let device = if let Some(i) = args.iter().position(|a| a == "--gpu") {
        args.remove(i);
        Device::Gpu
    } else {
        Device::Cpu
    };
    if args.len() < 2 {
        eprintln!("usage: transcribe [--gpu] <model-dir> <audio>...");
        std::process::exit(2);
    }
    let dir = PathBuf::from(&args[0]);
    let started = Instant::now();
    let mut engine = Parakeet::load(&dir, &EngineOptions { device, threads: 0 })?;
    println!("loaded in {:.2?}", started.elapsed());
    for file in &args[1..] {
        let audio = common::load_audio(file.as_ref());
        let started = Instant::now();
        let t = engine.transcribe(&audio)?;
        println!(
            "{file} ({:.1}s) in {:.0?} [features {:.0?}, encoder {:.0?}, decoder {:.0?}]\n  {}",
            audio.len() as f64 / 16_000.0,
            started.elapsed(),
            t.timings.features,
            t.timings.encoder,
            t.timings.decoder,
            t.text
        );
    }
    Ok(())
}
