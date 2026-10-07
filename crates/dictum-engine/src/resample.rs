//! Streaming band-limited resampler (Kaiser-windowed sinc, polyphase table) to 16 kHz.
//!
//! Positions are tracked with exact integer arithmetic so long recordings never drift.

use crate::SAMPLE_RATE;

const PHASES: usize = 512;
const KAISER_BETA: f64 = 8.6;

pub struct Resampler {
    in_rate: u64,
    out_rate: u64,
    /// Filter half-width in input samples.
    half: usize,
    /// `PHASES + 1` kernels of `2 * half` taps; phase `p` is centred at fractional offset `p / PHASES`.
    table: Vec<f32>,
    /// Unconsumed input; `buf[0]` is input sample `buf_start` (may be negative: leading zero padding).
    buf: Vec<f32>,
    buf_start: i64,
    /// Next output sample index.
    next_out: u64,
    /// Total real input samples pushed so far.
    total_in: u64,
}

impl Resampler {
    pub fn new(input_rate: u32) -> Self {
        Self::with_rates(input_rate, SAMPLE_RATE)
    }

    pub fn with_rates(input_rate: u32, output_rate: u32) -> Self {
        assert!(input_rate > 0 && output_rate > 0, "sample rates must be positive");
        let in_rate = u64::from(input_rate);
        let out_rate = u64::from(output_rate);
        let mut r = Self {
            in_rate,
            out_rate,
            half: 0,
            table: Vec::new(),
            buf: Vec::new(),
            buf_start: 0,
            next_out: 0,
            total_in: 0,
        };
        if !r.is_passthrough() {
            let ratio = in_rate as f64 / out_rate as f64;
            // Wider kernels when downsampling keep the transition band narrow in output terms.
            r.half = (16.0 * ratio.max(1.0)).ceil() as usize;
            // Cut-off just below the lower Nyquist frequency, in cycles per input sample.
            let cutoff = 0.5 * (out_rate as f64 / in_rate as f64).min(1.0) * 0.94;
            r.table = build_table(r.half, cutoff);
            r.buf = vec![0.0; r.half];
            r.buf_start = -(r.half as i64);
        }
        r
    }

    fn is_passthrough(&self) -> bool {
        self.in_rate == self.out_rate
    }

    /// Resamples `input`, appending every output sample that is already fully determined.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.total_in += input.len() as u64;
        if self.is_passthrough() {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        self.drain(out, false);
    }

    /// Emits the remaining output samples (zero-padding the tail) and resets the stream position.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if !self.is_passthrough() {
            let pad = self.half + 1;
            self.buf.extend(std::iter::repeat_n(0.0, pad));
            self.drain(out, true);
        }
        *self = Self::with_rates(self.in_rate as u32, self.out_rate as u32);
    }

    fn drain(&mut self, out: &mut Vec<f32>, flushing: bool) {
        let taps = 2 * self.half;
        let expected_total = (self.total_in * self.out_rate).div_ceil(self.in_rate);
        loop {
            if flushing && self.next_out >= expected_total {
                break;
            }
            let num = self.next_out * self.in_rate;
            let center = (num / self.out_rate) as i64;
            let frac = (num % self.out_rate) as f64 / self.out_rate as f64;
            // Taps cover input samples center - half + 1 ..= center + half.
            let first = center - self.half as i64 + 1;
            let last = center + self.half as i64;
            if last - self.buf_start >= self.buf.len() as i64 {
                break;
            }
            let start = (first - self.buf_start) as usize;
            let phase = (frac * PHASES as f64).round() as usize;
            let kernel = &self.table[phase * taps..(phase + 1) * taps];
            let window = &self.buf[start..start + taps];
            let sample: f32 = window.iter().zip(kernel).map(|(x, h)| x * h).sum();
            out.push(sample);
            self.next_out += 1;
        }
        // Drop input that no future output can reach.
        let num = self.next_out * self.in_rate;
        let next_first = (num / self.out_rate) as i64 - self.half as i64 + 1;
        let consumable = (next_first - self.buf_start).clamp(0, self.buf.len() as i64) as usize;
        if consumable > 0 {
            self.buf.drain(..consumable);
            self.buf_start += consumable as i64;
        }
    }
}

fn build_table(half: usize, cutoff: f64) -> Vec<f32> {
    let taps = 2 * half;
    let mut table = Vec::with_capacity((PHASES + 1) * taps);
    let norm = bessel_i0(KAISER_BETA);
    for phase in 0..=PHASES {
        let frac = phase as f64 / PHASES as f64;
        let start = table.len();
        let mut sum = 0.0;
        for k in 0..taps {
            // Distance from the output position to tap k (tap k is input sample center - half + 1 + k).
            let t = (k as f64 - half as f64 + 1.0) - frac;
            let x = t / half as f64;
            let window =
                if x.abs() >= 1.0 { 0.0 } else { bessel_i0(KAISER_BETA * (1.0 - x * x).sqrt()) / norm };
            let h = 2.0 * cutoff * sinc(2.0 * cutoff * t) * window;
            sum += h;
            table.push(h as f32);
        }
        // Unity DC gain for every phase.
        for h in &mut table[start..] {
            *h = (*h as f64 / sum) as f32;
        }
    }
    table
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..64 {
        term *= half / k as f64;
        let t2 = term * term;
        sum += t2;
        if t2 < 1e-12 * sum {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, seconds: f64) -> Vec<f32> {
        let n = (rate as f64 * seconds) as usize;
        (0..n).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32).collect()
    }

    fn resample_all(rate: u32, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut r = Resampler::new(rate);
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            r.push(c, &mut out);
        }
        r.flush(&mut out);
        out
    }

    /// Amplitude of `freq` in `signal` (single-bin DFT), ignoring the edges.
    fn tone_amplitude(signal: &[f32], rate: u32, freq: f64) -> f64 {
        let s = &signal[signal.len() / 10..signal.len() * 9 / 10];
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &x) in s.iter().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64;
            re += x as f64 * ph.cos();
            im += x as f64 * ph.sin();
        }
        2.0 * (re * re + im * im).sqrt() / s.len() as f64
    }

    #[test]
    fn output_length_matches_ratio() {
        for rate in [8_000, 16_000, 22_050, 44_100, 48_000, 96_000] {
            let input = vec![0.1f32; rate as usize * 3 / 2];
            let out = resample_all(rate, &input, 441);
            assert_eq!(out.len(), 24_000, "rate {rate}");
        }
    }

    #[test]
    fn preserves_in_band_tone() {
        for rate in [44_100, 48_000] {
            let out = resample_all(rate, &sine(rate, 1_000.0, 1.0), 480);
            let amp = tone_amplitude(&out, 16_000, 1_000.0);
            assert!((amp - 1.0).abs() < 0.02, "rate {rate}: amplitude {amp}");
        }
    }

    #[test]
    fn rejects_out_of_band_tone() {
        // 12 kHz cannot be represented at 16 kHz; it must not alias into the speech band.
        let out = resample_all(48_000, &sine(48_000, 12_000.0, 1.0), 480);
        let aliased = tone_amplitude(&out, 16_000, 4_000.0);
        assert!(aliased < 0.01, "aliased amplitude {aliased}");
    }

    #[test]
    fn chunking_does_not_change_output() {
        let input = sine(44_100, 440.0, 0.5);
        let a = resample_all(44_100, &input, 1);
        let b = resample_all(44_100, &input, 4_096);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6));
    }

    #[test]
    fn passthrough_at_16k() {
        let input = sine(16_000, 300.0, 0.1);
        assert_eq!(resample_all(16_000, &input, 100), input);
    }

    #[test]
    fn reusable_after_flush() {
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        r.push(&vec![0.5; 4_800], &mut out);
        r.flush(&mut out);
        assert_eq!(out.len(), 1_600);
        out.clear();
        r.push(&vec![0.5; 4_800], &mut out);
        r.flush(&mut out);
        assert_eq!(out.len(), 1_600);
    }
}
