//! Dictum's local speech-to-text engine.
//!
//! - [`Parakeet`]: NVIDIA Parakeet TDT 0.6B (v3, multilingual) running on ONNX Runtime.
//! - [`Vad`] + [`Segmenter`]: Silero VAD used to trim silence, skip empty recordings and split
//!   long dictations at natural pauses so they can be transcribed while the user keeps talking.
//! - [`Resampler`]: streaming windowed-sinc resampler from the device rate to 16 kHz.
//! - [`text`]: lightweight, deterministic clean-up of the recognised text.
//!
//! Everything here is platform independent; the app crate owns audio capture, hotkeys and text
//! insertion.

mod boost;
pub mod model;
mod parakeet;
mod resample;
mod segment;
mod session;
pub mod text;
mod vad;
mod vocab;

pub use parakeet::{Interrupt, Parakeet, Timings, Transcript};
pub use resample::Resampler;
pub use segment::{FinalSegment, SegmentEvent, SegmentKey, Segmenter, SegmenterConfig};
pub use vad::Vad;

/// Sample rate expected by every model in this crate.
pub const SAMPLE_RATE: u32 = 16_000;

/// Where inference runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Device {
    /// CPU only. Fast enough for real-time dictation on any modern machine.
    #[default]
    Cpu,
    /// Offload the acoustic encoder to the GPU: DirectML on Windows, CoreML on macOS.
    /// Falls back to the CPU if the accelerator cannot be initialised.
    Gpu,
}

#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    pub device: Device,
    /// Threads for the acoustic encoder. `0` lets ONNX Runtime pick (one per physical core).
    pub threads: usize,
    /// Terms to favour when the audio is ambiguous, e.g. "Claude Code", "Vercel".
    pub vocabulary: Vec<String>,
}
