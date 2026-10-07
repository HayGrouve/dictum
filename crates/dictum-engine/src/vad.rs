//! Silero VAD (v6, MIT licensed) — a 2 MB model that scores 32 ms windows of 16 kHz audio.

use anyhow::{Context, Result, ensure};
use ort::session::Session;
use ort::value::TensorRef;

use crate::Device;
use crate::session::{self, Source};

static MODEL: &[u8] = include_bytes!("../assets/silero_vad.onnx");

/// Samples per VAD window (32 ms at 16 kHz).
pub const VAD_WINDOW: usize = 512;
const CONTEXT: usize = 64;
const STATE_LEN: usize = 2 * 128;

pub struct Vad {
    session: Session,
    state: Vec<f32>,
    input: Vec<f32>,
}

impl Vad {
    pub fn new() -> Result<Self> {
        let session = session::build(Source::Memory(MODEL), 1, Device::Cpu)?;
        Ok(Self { session, state: vec![0.0; STATE_LEN], input: vec![0.0; CONTEXT + VAD_WINDOW] })
    }

    /// Forgets all history; call between unrelated recordings.
    pub fn reset(&mut self) {
        self.state.fill(0.0);
        self.input.fill(0.0);
    }

    /// Speech probability of the next 512-sample window.
    pub fn process(&mut self, window: &[f32]) -> Result<f32> {
        ensure!(window.len() == VAD_WINDOW, "VAD expects {VAD_WINDOW}-sample windows");
        // Input = last 64 samples of the previous window followed by this window.
        self.input.copy_within(VAD_WINDOW.., 0);
        self.input[CONTEXT..].copy_from_slice(window);

        let sr = [crate::SAMPLE_RATE as i64];
        let outputs = self.session.run(ort::inputs![
            "input" => TensorRef::from_array_view(([1usize, CONTEXT + VAD_WINDOW], &self.input[..]))?,
            "state" => TensorRef::from_array_view(([2usize, 1, 128], &self.state[..]))?,
            "sr" => TensorRef::from_array_view((Vec::<usize>::new(), &sr[..]))?,
        ])?;
        let (_, prob) = outputs["output"].try_extract_tensor::<f32>()?;
        let prob = *prob.first().context("VAD returned no output")?;
        let (_, state) = outputs["stateN"].try_extract_tensor::<f32>()?;
        ensure!(state.len() == STATE_LEN, "unexpected VAD state size {}", state.len());
        self.state.copy_from_slice(state);
        Ok(prob)
    }
}
