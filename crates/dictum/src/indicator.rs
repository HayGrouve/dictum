//! The on-screen dictation indicator: a small pill with live level bars while listening, drawn
//! in software (anti-aliased with signed distance fields) so every platform shows the same thing.

use std::collections::VecDeque;
use std::time::Duration;

/// What the indicator shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Hidden,
    /// Hotkey held: a red dot.
    Listening,
    /// Locked hands-free: a red stop square (press the hotkey to finish).
    HandsFree,
    /// Waiting for the text: amber, with a travelling wave.
    Transcribing,
}

/// Size in logical pixels (96 DPI); multiply by the display scale.
pub const WIDTH: f32 = 112.0;
pub const HEIGHT: f32 = 34.0;
/// Frames per second while visible.
pub const FPS: u32 = 30;
/// Transcription that finishes sooner than this never shows the amber state (no flicker).
pub const TRANSCRIBING_DELAY: Duration = Duration::from_millis(150);

const BARS: usize = 13;
const RED: [f32; 3] = [0xe5 as f32, 0x48 as f32, 0x4d as f32];
const AMBER: [f32; 3] = [0xf5 as f32, 0x9e as f32, 0x0b as f32];
const WHITE: [f32; 3] = [0xf4 as f32, 0xf4 as f32, 0xf5 as f32];
const BACKGROUND: [f32; 3] = [0x1c as f32, 0x1c as f32, 0x1f as f32];
const BORDER: [f32; 3] = [0x52 as f32, 0x52 as f32, 0x5b as f32];

/// Recent loudness for the bars: the loudest chunk of each frame, newest last.
pub struct Meter {
    peak: f32,
    history: VecDeque<f32>,
}

impl Default for Meter {
    fn default() -> Self {
        Self { peak: 0.0, history: std::iter::repeat_n(0.0, BARS).collect() }
    }
}

impl Meter {
    /// Takes a chunk of microphone samples.
    pub fn feed(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        self.peak = self.peak.max(level(rms));
    }

    /// Advances one frame: the loudest level since the last one becomes the newest bar.
    pub fn tick(&mut self) {
        self.history.pop_front();
        self.history.push_back(self.peak);
        self.peak = 0.0;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn bars(&self) -> Vec<f32> {
        self.history.iter().copied().collect()
    }
}

/// RMS to 0..1 on a decibel scale: a quiet room stays near 0, normal speech lands around 0.5–0.9.
pub fn level(rms: f32) -> f32 {
    let db = 20.0 * rms.max(1e-6).log10();
    ((db + 55.0) / 40.0).clamp(0.0, 1.0)
}

/// One frame as premultiplied BGRA, top-down rows (what Windows layered windows take).
/// `scale` is the display scale (DPI / 96); `elapsed` drives the animations.
pub fn render(phase: Phase, bars: &[f32], elapsed: Duration, scale: f32) -> (u32, u32, Vec<u8>) {
    let width = (WIDTH * scale).round() as u32;
    let height = (HEIGHT * scale).round() as u32;
    let t = elapsed.as_secs_f32();
    let (cy, radius) = (HEIGHT / 2.0, HEIGHT / 2.0);
    let accent = if phase == Phase::Transcribing { AMBER } else { RED };
    // A slow pulse on the dot while listening.
    let dot_alpha = if phase == Phase::Listening { 0.8 + 0.2 * (t * 4.0).cos() } else { 1.0 };

    let bars_left = 30.0;
    let bars_right = WIDTH - 13.0;
    let pitch = (bars_right - bars_left) / BARS as f32;
    let bar_half_width = 1.4;
    let max_bar = HEIGHT - 14.0;
    let heights: Vec<f32> = (0..BARS)
        .map(|i| {
            let level = if phase == Phase::Transcribing {
                0.15 + 0.3 * (0.5 + 0.5 * (t * 7.0 - i as f32 * 0.7).sin())
            } else {
                bars.get(i).copied().unwrap_or(0.0)
            };
            3.0 + level.clamp(0.0, 1.0) * (max_bar - 3.0)
        })
        .collect();
    let bar_color = if phase == Phase::Transcribing { AMBER } else { WHITE };

    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for py in 0..height {
        for px in 0..width {
            // Pixel centre in logical units; coverage from the distance in physical pixels.
            let (x, y) = ((px as f32 + 0.5) / scale, (py as f32 + 0.5) / scale);
            let cover = |distance: f32| (0.5 - distance * scale).clamp(0.0, 1.0);
            let mut pixel = [0.0f32; 4];

            let center = (WIDTH / 2.0, cy);
            let outer = rounded_rect(x, y, center, (WIDTH / 2.0, HEIGHT / 2.0), radius);
            over(&mut pixel, BORDER, 0.9 * cover(outer));
            let inner = rounded_rect(x, y, center, (WIDTH / 2.0 - 1.0, HEIGHT / 2.0 - 1.0), radius - 1.0);
            over(&mut pixel, BACKGROUND, 0.96 * cover(inner));

            let mark = if phase == Phase::HandsFree {
                rounded_rect(x, y, (17.0, cy), (4.5, 4.5), 1.5)
            } else {
                ((x - 17.0).powi(2) + (y - cy).powi(2)).sqrt() - 5.0
            };
            over(&mut pixel, accent, dot_alpha * cover(mark));

            let slot = ((x - bars_left) / pitch).floor();
            if slot >= 0.0 && (slot as usize) < BARS {
                let i = slot as usize;
                let bar_x = bars_left + (i as f32 + 0.5) * pitch;
                let half_height = heights[i] / 2.0;
                let bar = rounded_rect(x, y, (bar_x, cy), (bar_half_width, half_height), bar_half_width);
                over(&mut pixel, bar_color, 0.95 * cover(bar));
            }

            let [r, g, b, a] = pixel;
            out.extend_from_slice(&[
                b.round() as u8,
                g.round() as u8,
                r.round() as u8,
                (a * 255.0).round() as u8,
            ]);
        }
    }
    (width, height, out)
}

/// Signed distance from (x, y) to a rounded rectangle; negative inside.
fn rounded_rect(x: f32, y: f32, center: (f32, f32), half: (f32, f32), radius: f32) -> f32 {
    let qx = (x - center.0).abs() - (half.0 - radius);
    let qy = (y - center.1).abs() - (half.1 - radius);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius
}

/// Composites `color` at `alpha` over a premultiplied pixel (colour 0–255, alpha 0–1).
fn over(pixel: &mut [f32; 4], color: [f32; 3], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    for c in 0..3 {
        pixel[c] = color[c] * alpha + pixel[c] * (1.0 - alpha);
    }
    pixel[3] = alpha + pixel[3] * (1.0 - alpha);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(frame: &(u32, u32, Vec<u8>), x: u32, y: u32) -> u8 {
        frame.2[((y * frame.0 + x) * 4 + 3) as usize]
    }

    #[test]
    fn levels_follow_loudness() {
        assert_eq!(level(0.0), 0.0);
        assert!(level(0.001) < 0.1, "quiet room");
        assert!((0.4..=0.95).contains(&level(0.05)), "speech");
        assert_eq!(level(1.0), 1.0);

        let mut meter = Meter::default();
        meter.feed(&[0.0; 480]);
        meter.tick();
        meter.feed(&[0.001; 480]);
        meter.feed(&vec![0.1; 480]);
        meter.tick();
        let bars = meter.bars();
        assert_eq!(bars.len(), BARS);
        assert_eq!(bars[BARS - 2], 0.0);
        assert!((bars[BARS - 1] - level(0.1)).abs() < 1e-4, "loudest chunk of the frame");
        meter.tick();
        assert_eq!(*meter.bars().last().unwrap(), 0.0);
    }

    #[test]
    fn frames_scale_and_have_rounded_corners() {
        for scale in [1.0, 1.5, 2.0] {
            let frame = render(Phase::Listening, &[0.5; BARS], Duration::ZERO, scale);
            let (w, h, pixels) = &frame;
            assert_eq!((*w, *h), ((WIDTH * scale).round() as u32, (HEIGHT * scale).round() as u32));
            assert_eq!(pixels.len() as u32, w * h * 4);
            assert_eq!(alpha_at(&frame, 0, 0), 0, "corner is transparent");
            assert!(alpha_at(&frame, w / 2, h / 2) > 240, "body is opaque");
            // Premultiplied: no channel exceeds alpha.
            assert!(pixels.chunks(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
        }
    }

    #[test]
    fn louder_means_taller_bars() {
        let lit = |level: f32| {
            let (w, h, pixels) = render(Phase::Listening, &[level; BARS], Duration::ZERO, 1.0);
            // Count bright pixels in the bar area.
            (0..h)
                .flat_map(|y| (40..w - 13).map(move |x| (x, y)))
                .filter(|&(x, y)| pixels[((y * w + x) * 4) as usize] > 200)
                .count()
        };
        assert!(lit(0.0) > 0, "silent bars are still visible dots");
        assert!(lit(0.5) > lit(0.0) * 2);
        assert!(lit(1.0) > lit(0.5));
    }

    #[test]
    fn phases_look_different() {
        let frame = |phase| render(phase, &[0.3; BARS], Duration::from_millis(500), 1.0).2;
        assert_ne!(frame(Phase::Listening), frame(Phase::HandsFree));
        assert_ne!(frame(Phase::Listening), frame(Phase::Transcribing));
    }
}
