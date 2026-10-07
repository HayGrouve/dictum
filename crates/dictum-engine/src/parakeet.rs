//! NVIDIA Parakeet TDT (Token-and-Duration Transducer) inference on ONNX Runtime.
//!
//! Model export: <https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx>
//! - `nemo128.onnx`: waveform -> normalised 128-bin log-mel features
//! - `encoder-model[.int8].onnx`: FastConformer encoder, 8x subsampling (80 ms per frame)
//! - `decoder_joint-model[.int8].onnx`: LSTM prediction network + joint network; the output holds
//!   token logits followed by duration logits (how many frames to skip)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ort::session::{RunOptions, Session};
use ort::value::TensorElementType;
use ort::value::TensorRef;

use crate::session::{self, Source};
use crate::vocab::Vocab;
use crate::{Device, EngineOptions};

/// Upper bound on tokens emitted for a single encoder frame (guards against loops).
const MAX_TOKENS_PER_STEP: usize = 10;
/// Shorter inputs cannot contain a word; skip inference entirely.
const MIN_SAMPLES: usize = crate::SAMPLE_RATE as usize / 10;

pub struct Parakeet {
    preprocessor: Session,
    encoder: Session,
    decoder_joint: Session,
    vocab: Vocab,
    state1_shape: Vec<usize>,
    state2_shape: Vec<usize>,
    targets_are_i64: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Timings {
    pub features: Duration,
    pub encoder: Duration,
    pub decoder: Duration,
}

impl Timings {
    pub fn total(&self) -> Duration {
        self.features + self.encoder + self.decoder
    }
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub text: String,
    pub timings: Timings,
}

/// Stops an in-flight [`Parakeet::transcribe_interruptible`] call from another thread.
pub struct Interrupt {
    triggered: AtomicBool,
    run_options: RunOptions,
}

impl Interrupt {
    pub fn new() -> Result<Self> {
        crate::session::init_runtime();
        Ok(Self { triggered: AtomicBool::new(false), run_options: RunOptions::new()? })
    }

    pub fn trigger(&self) {
        self.triggered.store(true, Ordering::SeqCst);
        let _ = self.run_options.terminate();
    }

    /// Re-arms the interrupt for the next transcription.
    pub fn reset(&self) {
        self.triggered.store(false, Ordering::SeqCst);
        let _ = self.run_options.unterminate();
    }

    pub fn is_triggered(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }
}

impl Parakeet {
    /// Loads the model from `dir`, preferring int8-quantised weights when both variants exist.
    pub fn load(dir: &Path, options: &EngineOptions) -> Result<Self> {
        let pick = |stem: &str| -> Result<PathBuf> {
            let order = if options.device == Device::Gpu {
                // GPUs run fp32 faster than int8 when the full-precision export is available.
                [format!("{stem}.onnx"), format!("{stem}.int8.onnx")]
            } else {
                [format!("{stem}.int8.onnx"), format!("{stem}.onnx")]
            };
            order
                .iter()
                .map(|name| dir.join(name))
                .find(|p| p.is_file())
                .with_context(|| format!("{stem} model not found in {}", dir.display()))
        };
        let vocab_path = dir.join("vocab.txt");
        let vocab = Vocab::parse(
            &std::fs::read_to_string(&vocab_path)
                .with_context(|| format!("failed to read {}", vocab_path.display()))?,
        )?;

        let preprocessor = session::build(Source::File(&dir.join("nemo128.onnx")), 1, Device::Cpu)?;
        let encoder = session::build(Source::File(&pick("encoder-model")?), options.threads, options.device)?;
        // The decoder runs once per emitted token / frame on tiny tensors: threading only adds overhead.
        let decoder_joint = session::build(Source::File(&pick("decoder_joint-model")?), 1, Device::Cpu)?;

        let input_shape = |name: &str| -> Result<(Vec<i64>, TensorElementType)> {
            let input = decoder_joint
                .inputs()
                .iter()
                .find(|i| i.name() == name)
                .with_context(|| format!("decoder input `{name}` missing"))?;
            let shape = input.dtype().tensor_shape().context("decoder input is not a tensor")?;
            let ty = input.dtype().tensor_type().context("decoder input is not a tensor")?;
            Ok((shape.iter().copied().collect(), ty))
        };
        // States are [layers, batch, hidden]; batch is dynamic and always 1 for us.
        let state_shape = |name: &str| -> Result<Vec<usize>> {
            let (dims, _) = input_shape(name)?;
            ensure!(dims.len() == 3, "unexpected rank for `{name}`");
            Ok(vec![dims[0].max(1) as usize, 1, dims[2].max(1) as usize])
        };
        let state1_shape = state_shape("input_states_1")?;
        let state2_shape = state_shape("input_states_2")?;
        let targets_are_i64 = match input_shape("targets")?.1 {
            TensorElementType::Int64 => true,
            TensorElementType::Int32 => false,
            other => bail!("unsupported decoder target type {other:?}"),
        };

        let mut model =
            Self { preprocessor, encoder, decoder_joint, vocab, state1_shape, state2_shape, targets_are_i64 };
        model.warm_up()?;
        Ok(model)
    }

    /// Runs a short dummy inference so the first real request doesn't pay for lazy initialisation.
    fn warm_up(&mut self) -> Result<()> {
        let t =
            (0..crate::SAMPLE_RATE as usize).map(|i| 0.01 * ((i as f32) * 0.07).sin()).collect::<Vec<_>>();
        self.transcribe(&t)?;
        Ok(())
    }

    /// Transcribes mono 16 kHz audio in the range [-1, 1].
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<Transcript> {
        Ok(self.run(samples, None)?.unwrap_or_default())
    }

    /// Like [`Self::transcribe`], but returns `Ok(None)` as soon as possible once `interrupt`
    /// is triggered.
    pub fn transcribe_interruptible(
        &mut self,
        samples: &[f32],
        interrupt: &Interrupt,
    ) -> Result<Option<Transcript>> {
        match self.run(samples, Some(interrupt)) {
            Err(_) if interrupt.is_triggered() => Ok(None),
            result => result,
        }
    }

    fn run(&mut self, samples: &[f32], interrupt: Option<&Interrupt>) -> Result<Option<Transcript>> {
        let mut timings = Timings::default();
        if samples.len() < MIN_SAMPLES {
            return Ok(Some(Transcript { text: String::new(), timings }));
        }

        // 1. Features.
        let started = Instant::now();
        let lens = [samples.len() as i64];
        let pre = self.preprocessor.run(ort::inputs![
            "waveforms" => TensorRef::from_array_view(([1usize, samples.len()], samples))?,
            "waveforms_lens" => TensorRef::from_array_view(([1usize], &lens[..]))?,
        ])?;
        let (feat_shape, features) = pre["features"].try_extract_tensor::<f32>()?;
        let feat_shape: Vec<usize> = feat_shape.iter().map(|&d| d as usize).collect();
        let (_, feat_lens) = pre["features_lens"].try_extract_tensor::<i64>()?;
        let feat_len = [feat_lens[0]];
        timings.features = started.elapsed();

        // 2. Encoder.
        let started = Instant::now();
        let inputs = ort::inputs![
            "audio_signal" => TensorRef::from_array_view((feat_shape, features))?,
            "length" => TensorRef::from_array_view(([1usize], &feat_len[..]))?,
        ];
        let enc = match interrupt {
            Some(interrupt) => self.encoder.run_with_options(inputs, &interrupt.run_options)?,
            None => self.encoder.run(inputs)?,
        };
        let (enc_shape, encoded) = enc["outputs"].try_extract_tensor::<f32>()?;
        ensure!(enc_shape.len() == 3, "unexpected encoder output rank");
        let (dim, frames) = (enc_shape[1] as usize, enc_shape[2] as usize);
        let (_, enc_lens) = enc["encoded_lengths"].try_extract_tensor::<i64>()?;
        let valid_frames = (enc_lens[0].max(0) as usize).min(frames);
        // [1, dim, frames] -> [frames, dim] so each frame is contiguous for the decoder.
        let mut by_frame = vec![0f32; valid_frames * dim];
        for d in 0..dim {
            let row = &encoded[d * frames..d * frames + valid_frames];
            for (t, &v) in row.iter().enumerate() {
                by_frame[t * dim + d] = v;
            }
        }
        drop(enc);
        drop(pre);
        timings.encoder = started.elapsed();

        // 3. Greedy TDT decoding.
        let started = Instant::now();
        let Some(tokens) = self.decode(&by_frame, dim, valid_frames, interrupt)? else {
            return Ok(None);
        };
        let text = self.vocab.decode(&tokens);
        timings.decoder = started.elapsed();

        Ok(Some(Transcript { text, timings }))
    }

    fn decode(
        &mut self,
        frames: &[f32],
        dim: usize,
        count: usize,
        interrupt: Option<&Interrupt>,
    ) -> Result<Option<Vec<usize>>> {
        let vocab_size = self.vocab.len();
        let blank = self.vocab.blank();
        let mut state1 = vec![0f32; self.state1_shape.iter().product()];
        let mut state2 = vec![0f32; self.state2_shape.iter().product()];
        let target_len_i32 = [1i32];
        let target_len_i64 = [1i64];

        let mut tokens = Vec::new();
        let mut last = blank;
        let mut t = 0;
        let mut emitted = 0;
        while t < count {
            if interrupt.is_some_and(Interrupt::is_triggered) {
                return Ok(None);
            }
            let frame = &frames[t * dim..(t + 1) * dim];
            let target_i32 = [last as i32];
            let target_i64 = [last as i64];
            let (targets, target_length) = if self.targets_are_i64 {
                (
                    TensorRef::from_array_view(([1usize, 1], &target_i64[..]))?.into_dyn(),
                    TensorRef::from_array_view(([1usize], &target_len_i64[..]))?.into_dyn(),
                )
            } else {
                (
                    TensorRef::from_array_view(([1usize, 1], &target_i32[..]))?.into_dyn(),
                    TensorRef::from_array_view(([1usize], &target_len_i32[..]))?.into_dyn(),
                )
            };
            let out = self.decoder_joint.run(ort::inputs![
                "encoder_outputs" => TensorRef::from_array_view(([1usize, dim, 1], frame))?,
                "targets" => targets,
                "target_length" => target_length,
                "input_states_1" => TensorRef::from_array_view((self.state1_shape.clone(), &state1[..]))?,
                "input_states_2" => TensorRef::from_array_view((self.state2_shape.clone(), &state2[..]))?,
            ])?;
            let (_, logits) = out["outputs"].try_extract_tensor::<f32>()?;
            ensure!(logits.len() > vocab_size, "decoder output too small");
            let token = argmax(&logits[..vocab_size]);
            let skip = argmax(&logits[vocab_size..]);

            if token != blank {
                let (_, s1) = out["output_states_1"].try_extract_tensor::<f32>()?;
                let (_, s2) = out["output_states_2"].try_extract_tensor::<f32>()?;
                state1.copy_from_slice(s1);
                state2.copy_from_slice(s2);
                tokens.push(token);
                last = token;
                emitted += 1;
            }
            if skip > 0 {
                t += skip;
                emitted = 0;
            } else if token == blank || emitted >= MAX_TOKENS_PER_STEP {
                t += 1;
                emitted = 0;
            }
        }
        Ok(Some(tokens))
    }
}

fn argmax(values: &[f32]) -> usize {
    let mut best = 0;
    let mut best_value = f32::NEG_INFINITY;
    for (i, &v) in values.iter().enumerate() {
        if v > best_value {
            best = i;
            best_value = v;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::argmax;

    #[test]
    fn argmax_picks_first_maximum() {
        assert_eq!(argmax(&[0.1, 0.9, 0.9, -1.0]), 1);
        assert_eq!(argmax(&[-3.0]), 0);
    }
}
