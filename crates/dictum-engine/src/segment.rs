//! Turns one recording into transcription-ready segments.
//!
//! While the user talks, 16 kHz audio is scored by the VAD window by window:
//! - Once a segment is long enough, the next natural pause becomes a cut point and the finished
//!   part is handed off ([`SegmentEvent::Segment`]), so long dictations are transcribed while the
//!   user keeps talking.
//! - Whenever the speaker pauses, a [`SegmentEvent::Pause`] snapshot shows what the final segment
//!   would be if they stopped now. Transcribing it speculatively means that, in the common case
//!   of "speak, pause, release", the text is ready the moment the hotkey is released: the final
//!   segment carries the same [`SegmentKey`] and its transcript can be reused.
//!
//! Every segment is trimmed to its speech (plus padding); a recording with no speech yields
//! nothing.

use anyhow::Result;

use crate::vad::{VAD_WINDOW, Vad};

#[derive(Debug, Clone)]
pub struct SegmenterConfig {
    /// VAD probability at which speech starts.
    pub threshold: f32,
    /// VAD probability below which speech ends (hysteresis).
    pub neg_threshold: f32,
    /// Audio kept before the first / after the last speech window of a segment.
    pub pad_before_ms: u32,
    pub pad_after_ms: u32,
    /// Segments are only split once they are at least this long...
    pub min_segment_ms: u32,
    /// ...and only at a pause at least this long.
    pub split_silence_ms: u32,
    /// Hard cap: split at the quietest point if nobody pauses for this long.
    pub max_segment_ms: u32,
    /// Fewer speech windows than this in a segment counts as no speech (coughs, clicks).
    pub min_speech_windows: usize,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            threshold: 0.4,
            neg_threshold: 0.25,
            pad_before_ms: 300,
            pad_after_ms: 400,
            min_segment_ms: 10_000,
            split_silence_ms: 600,
            max_segment_ms: 40_000,
            min_speech_windows: 3,
        }
    }
}

/// Identifies the exact audio of a segment within a recording (absolute sample range).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SegmentKey {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub enum SegmentEvent {
    /// A finished part of the recording.
    Segment(FinalSegment),
    /// The speaker paused; this is the final segment if the recording ends now.
    Pause(FinalSegment),
}

#[derive(Debug, Clone)]
pub struct FinalSegment {
    pub key: SegmentKey,
    pub audio: Vec<f32>,
}

pub struct Segmenter {
    cfg: SegmenterConfig,
    /// Audio of the current (not yet emitted) segment; always starts on a window boundary.
    audio: Vec<f32>,
    /// Absolute sample index of `audio[0]` within the recording.
    offset: usize,
    /// One VAD probability per complete window of `audio`.
    probs: Vec<f32>,
    in_speech: bool,
    silence_run: usize,
    speech_windows: usize,
    any_speech: bool,
}

const MS_PER_WINDOW: u32 = (VAD_WINDOW as u32 * 1000) / crate::SAMPLE_RATE;

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Self {
        Self {
            cfg,
            audio: Vec::new(),
            offset: 0,
            probs: Vec::new(),
            in_speech: false,
            silence_run: 0,
            speech_windows: 0,
            any_speech: false,
        }
    }

    /// True once any speech has been detected in this recording.
    pub fn heard_speech(&self) -> bool {
        self.any_speech
    }

    /// Feeds 16 kHz audio and reports finished segments and pauses.
    /// Without a VAD nothing is split, trimmed or reported.
    pub fn push(&mut self, samples: &[f32], vad: Option<&mut Vad>) -> Result<Vec<SegmentEvent>> {
        self.audio.extend_from_slice(samples);
        let mut events = Vec::new();
        let Some(vad) = vad else {
            return Ok(events);
        };
        while self.audio.len() >= (self.probs.len() + 1) * VAD_WINDOW {
            let start = self.probs.len() * VAD_WINDOW;
            let p = vad.process(&self.audio[start..start + VAD_WINDOW])?;
            match self.observe(p) {
                Some(Decision::Cut(windows)) => {
                    if let Some(segment) = self.split_off(windows) {
                        events.push(SegmentEvent::Segment(segment));
                    }
                }
                Some(Decision::Pause) => {
                    if let Some(snapshot) = self.snapshot() {
                        events.push(SegmentEvent::Pause(snapshot));
                    }
                }
                None => {}
            }
        }
        Ok(events)
    }

    /// Ends the recording and returns the final segment, or `None` if it contains no speech.
    pub fn finish(&mut self, vad: Option<&mut Vad>) -> Result<Option<FinalSegment>> {
        let Some(vad) = vad else {
            let audio = std::mem::take(&mut self.audio);
            let key = SegmentKey { start: self.offset, end: self.offset + audio.len() };
            self.offset = key.end;
            return Ok((!audio.is_empty()).then_some(FinalSegment { key, audio }));
        };
        // Score everything not yet scored, zero-padding the last partial window. Cuts are not
        // taken here: the whole remainder is the final segment.
        while self.audio.len() > self.probs.len() * VAD_WINDOW {
            let start = self.probs.len() * VAD_WINDOW;
            let end = (start + VAD_WINDOW).min(self.audio.len());
            let mut window = self.audio[start..end].to_vec();
            window.resize(VAD_WINDOW, 0.0);
            let p = vad.process(&window)?;
            self.observe(p);
        }
        let windows = self.probs.len();
        Ok(self.split_off(windows))
    }

    /// Records one window's probability and decides whether to cut or report a pause.
    fn observe(&mut self, p: f32) -> Option<Decision> {
        if self.in_speech {
            self.in_speech = p >= self.cfg.neg_threshold;
        } else {
            self.in_speech = p >= self.cfg.threshold;
        }
        self.probs.push(p);
        if self.in_speech {
            self.silence_run = 0;
            self.speech_windows += 1;
            if self.speech_windows >= self.cfg.min_speech_windows {
                self.any_speech = true;
            }
        } else {
            self.silence_run += 1;
        }

        let windows = self.probs.len();
        let duration_ms = windows as u32 * MS_PER_WINDOW;
        let has_speech = self.speech_windows >= self.cfg.min_speech_windows;
        let silence_ms = self.silence_run as u32 * MS_PER_WINDOW;
        if has_speech && duration_ms >= self.cfg.min_segment_ms && silence_ms >= self.cfg.split_silence_ms {
            // Cut right after the trailing padding: the segment then has the same key as the
            // pause snapshot taken moments ago, so its speculative transcript can be reused.
            let keep = self.pause_windows().min(self.silence_run);
            return Some(Decision::Cut(windows - self.silence_run + keep));
        }
        // Report a pause once the trailing padding is fully captured, so the snapshot equals the
        // final segment if nothing else is said.
        if has_speech && self.silence_run == self.pause_windows() {
            return Some(Decision::Pause);
        }
        if duration_ms >= self.cfg.max_segment_ms {
            // Nobody paused: cut at the least speech-like window of the last two seconds.
            let lookback = (2_000 / MS_PER_WINDOW) as usize;
            let from = windows.saturating_sub(lookback);
            let quietest = (from..windows)
                .min_by(|&a, &b| self.probs[a].total_cmp(&self.probs[b]))
                .unwrap_or(windows - 1);
            return Some(Decision::Cut(quietest + 1));
        }
        None
    }

    fn pause_windows(&self) -> usize {
        (self.cfg.pad_after_ms.div_ceil(MS_PER_WINDOW) + 1) as usize
    }

    /// The trimmed current segment, without consuming it.
    fn snapshot(&self) -> Option<FinalSegment> {
        let scored = (self.probs.len() * VAD_WINDOW).min(self.audio.len());
        let range = speech_range(&self.audio[..scored], &self.probs, &self.cfg)?;
        Some(FinalSegment {
            key: SegmentKey { start: self.offset + range.start, end: self.offset + range.end },
            audio: self.audio[range].to_vec(),
        })
    }

    /// Removes the first `windows` windows from the current segment and returns their trimmed
    /// speech, if any.
    fn split_off(&mut self, windows: usize) -> Option<FinalSegment> {
        let samples = (windows * VAD_WINDOW).min(self.audio.len());
        let audio: Vec<f32> = self.audio.drain(..samples).collect();
        let probs: Vec<f32> = self.probs.drain(..windows.min(self.probs.len())).collect();
        let offset = self.offset;
        self.offset += samples;

        // Speech statistics for what remains of the current segment.
        self.speech_windows = 0;
        let mut state = false;
        for &p in &self.probs {
            state = if state { p >= self.cfg.neg_threshold } else { p >= self.cfg.threshold };
            if state {
                self.speech_windows += 1;
            }
        }
        self.silence_run = self.silence_run.min(self.probs.len());

        let range = speech_range(&audio, &probs, &self.cfg)?;
        Some(FinalSegment {
            key: SegmentKey { start: offset + range.start, end: offset + range.end },
            audio: audio[range].to_vec(),
        })
    }
}

enum Decision {
    /// Split the segment after this many windows.
    Cut(usize),
    /// The speaker paused long enough for a meaningful snapshot.
    Pause,
}

/// Sample range of `audio` holding speech (plus padding), if any.
fn speech_range(audio: &[f32], probs: &[f32], cfg: &SegmenterConfig) -> Option<std::ops::Range<usize>> {
    let mut state = false;
    let mut first = None;
    let mut last = 0;
    let mut count = 0;
    for (i, &p) in probs.iter().enumerate() {
        state = if state { p >= cfg.neg_threshold } else { p >= cfg.threshold };
        if state {
            first.get_or_insert(i);
            last = i;
            count += 1;
        }
    }
    let first = first?;
    if count < cfg.min_speech_windows {
        return None;
    }
    let ms = |ms: u32| (ms as usize * crate::SAMPLE_RATE as usize) / 1000;
    let start = (first * VAD_WINDOW).saturating_sub(ms(cfg.pad_before_ms));
    let end = ((last + 1) * VAD_WINDOW + ms(cfg.pad_after_ms)).min(audio.len());
    (start < end).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Run {
        /// (first, last) window index of each early segment.
        cuts: Vec<(usize, usize)>,
        cut_keys: Vec<SegmentKey>,
        pauses: Vec<SegmentKey>,
        tail: Option<FinalSegment>,
    }

    /// Drives the segmenter with synthetic probabilities instead of a real VAD.
    fn run(cfg: SegmenterConfig, probs: &[f32]) -> Run {
        let mut seg = Segmenter::new(cfg);
        let (mut cuts, mut cut_keys, mut pauses) = (Vec::new(), Vec::new(), Vec::new());
        for (i, &p) in probs.iter().enumerate() {
            // Mark each window's audio with its index so we can see what was kept.
            seg.audio.extend(std::iter::repeat_n(i as f32, VAD_WINDOW));
            match seg.observe(p) {
                Some(Decision::Cut(windows)) => {
                    if let Some(s) = seg.split_off(windows) {
                        cuts.push((s.audio[0] as usize, *s.audio.last().unwrap() as usize));
                        cut_keys.push(s.key);
                    }
                }
                Some(Decision::Pause) => pauses.extend(seg.snapshot().map(|s| s.key)),
                None => {}
            }
        }
        let windows = seg.probs.len();
        let tail = seg.split_off(windows);
        Run { cuts, cut_keys, pauses, tail }
    }

    fn windows_for(ms: u32) -> usize {
        (ms / MS_PER_WINDOW) as usize
    }

    #[test]
    fn silence_only_yields_nothing() {
        let r = run(SegmenterConfig::default(), &vec![0.05; 100]);
        assert!(r.cuts.is_empty() && r.pauses.is_empty());
        assert!(r.tail.is_none());
    }

    #[test]
    fn a_click_is_not_speech() {
        let mut probs = vec![0.0; 50];
        probs[20] = 0.9;
        assert!(run(SegmenterConfig::default(), &probs).tail.is_none());
    }

    #[test]
    fn short_dictation_is_trimmed_with_padding() {
        let cfg = SegmenterConfig::default();
        let mut probs = vec![0.0; 60];
        probs.extend(vec![0.9; 40]);
        probs.extend(vec![0.0; 60]);
        let r = run(cfg.clone(), &probs);
        assert!(r.cuts.is_empty());
        let pad = |ms: u32| ms as usize * 16;
        let tail = r.tail.unwrap();
        assert_eq!(tail.audio.len(), 40 * VAD_WINDOW + pad(cfg.pad_before_ms) + pad(cfg.pad_after_ms));
        assert_eq!(tail.key.start, 60 * VAD_WINDOW - pad(cfg.pad_before_ms));
    }

    #[test]
    fn pause_snapshot_matches_final_segment() {
        let mut probs = vec![0.0; 10];
        probs.extend(vec![0.9; 40]);
        probs.extend(vec![0.0; 40]);
        let r = run(SegmenterConfig::default(), &probs);
        assert_eq!(r.pauses.len(), 1);
        assert_eq!(Some(r.pauses[0]), r.tail.map(|t| t.key));
    }

    #[test]
    fn speech_after_a_pause_changes_the_key() {
        let mut probs = vec![0.9; 40];
        probs.extend(vec![0.0; 20]);
        probs.extend(vec![0.9; 10]);
        let r = run(SegmenterConfig::default(), &probs);
        assert_eq!(r.pauses.len(), 1);
        assert_ne!(Some(r.pauses[0]), r.tail.map(|t| t.key));
    }

    #[test]
    fn long_dictation_splits_at_a_pause() {
        let cfg = SegmenterConfig::default();
        let speech = windows_for(cfg.min_segment_ms) + 10;
        let pause = windows_for(cfg.split_silence_ms) + 2;
        let mut probs = vec![0.9; speech];
        probs.extend(vec![0.0; pause]);
        probs.extend(vec![0.9; 30]);
        let r = run(cfg, &probs);
        assert_eq!(r.cuts.len(), 1, "one split expected");
        assert_eq!(r.cuts[0].0, 0);
        assert!(r.cuts[0].1 >= speech - 1 && r.cuts[0].1 < speech + pause, "cut inside the pause");
        let tail = r.tail.unwrap();
        assert!(tail.key.start >= speech * VAD_WINDOW, "tail keys are absolute positions");
        assert_eq!(r.pauses.len(), 1, "the pause before the split; the tail never pauses");
        assert_eq!(r.pauses[0], r.cut_keys[0]);
    }

    #[test]
    fn split_reuses_the_pause_snapshot_key() {
        let cfg = SegmenterConfig::default();
        let mut probs = vec![0.9; windows_for(cfg.min_segment_ms) + 5];
        probs.extend(vec![0.0; 40]);
        let r = run(cfg, &probs);
        assert_eq!(r.cut_keys.len(), 1);
        assert_eq!(r.pauses.first(), r.cut_keys.first());
        assert!(r.tail.is_none());
    }

    #[test]
    fn short_pauses_do_not_split() {
        let cfg = SegmenterConfig::default();
        let mut probs = Vec::new();
        for _ in 0..40 {
            probs.extend(vec![0.9; 20]);
            probs.extend(vec![0.0; 5]); // 160 ms breath
        }
        let r = run(cfg, &probs);
        assert!(r.cuts.is_empty());
        assert!(r.tail.is_some());
    }

    #[test]
    fn nonstop_speech_is_force_split() {
        let cfg = SegmenterConfig::default();
        let total = windows_for(cfg.max_segment_ms) * 2 + 10;
        let mut probs = vec![0.9; total];
        probs[windows_for(cfg.max_segment_ms) - 5] = 0.3;
        let r = run(cfg.clone(), &probs);
        assert!(!r.cuts.is_empty());
        // The first forced cut lands right after the quietest window.
        assert_eq!(r.cuts[0].1, windows_for(cfg.max_segment_ms) - 5);
    }
}
