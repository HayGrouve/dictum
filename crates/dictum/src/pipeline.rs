//! The dictation pipeline, independent of any OS:
//!
//! ```text
//! mic chunks ──► Dictation (controller thread) ──jobs──► Worker (engine thread) ──► Done{text}
//!                resample → VAD → segmenter               transcribe / reuse speculation
//! ```
//!
//! While recording, finished segments are transcribed immediately and every pause triggers a
//! speculative transcription of "everything so far". When the hotkey is released the final
//! segment usually matches the last speculation, so its text is already there. Superseded
//! speculations are skipped, or interrupted mid-inference if they are already running.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use dictum_engine::text::{self, TextOptions};
use dictum_engine::{
    FinalSegment, Interrupt, Parakeet, Resampler, SegmentEvent, SegmentKey, Segmenter, SegmenterConfig, Vad,
};

pub type SessionId = u64;

/// Speech recognition as the worker needs it.
pub trait Transcriber: Send {
    /// `Ok(None)` means the interrupt fired before the result was ready.
    fn transcribe(&mut self, audio: &[f32], interrupt: Option<&Interrupt>) -> Result<Option<String>>;
}

impl Transcriber for Parakeet {
    fn transcribe(&mut self, audio: &[f32], interrupt: Option<&Interrupt>) -> Result<Option<String>> {
        let started = Instant::now();
        let result = match interrupt {
            Some(interrupt) => self.transcribe_interruptible(audio, interrupt)?,
            None => Some(Parakeet::transcribe(self, audio)?),
        };
        if let Some(t) = &result {
            log::debug!(
                "transcribed {:.1}s of audio in {:.0?} (features {:.0?}, encoder {:.0?}, decoder {:.0?})",
                audio.len() as f64 / dictum_engine::SAMPLE_RATE as f64,
                started.elapsed(),
                t.timings.features,
                t.timings.encoder,
                t.timings.decoder
            );
        }
        Ok(result.map(|t| t.text))
    }
}

#[derive(Debug)]
pub enum Job {
    Speculate { session: SessionId, generation: u64, segment: FinalSegment },
    Segment { session: SessionId, segment: FinalSegment },
    Finish { session: SessionId, segment: Option<FinalSegment> },
    Abort { session: SessionId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub session: SessionId,
    /// Final text to insert; `None` if nothing was said.
    pub text: Option<String>,
    /// Time from the end of the recording to the text being ready.
    pub latency: Duration,
}

/// Coordinates speculative jobs between the controller and the worker.
pub struct SpecControl {
    state: Mutex<SpecState>,
    interrupt: Interrupt,
}

#[derive(Default)]
struct SpecState {
    generation: u64,
    running: bool,
}

impl SpecControl {
    pub fn new() -> Result<Arc<Self>> {
        Ok(Arc::new(Self { state: Mutex::default(), interrupt: Interrupt::new()? }))
    }

    /// Invalidates every queued or running speculation; returns the new generation.
    fn supersede(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        if state.running {
            self.interrupt.trigger();
        }
        state.generation
    }

    /// Worker side: claims the right to run a speculation of `generation`.
    fn begin(&self, generation: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.generation != generation {
            return false;
        }
        state.running = true;
        self.interrupt.reset();
        true
    }

    fn end(&self) {
        self.state.lock().unwrap().running = false;
    }
}

/// Controller-side state of the recording in progress.
struct Recording {
    session: SessionId,
    resampler: Resampler,
    segmenter: Segmenter,
    /// Key of the last speculation sent for this recording.
    speculated: Option<SegmentKey>,
    resampled: Vec<f32>,
    samples: u64,
}

pub struct Dictation {
    jobs: Sender<Job>,
    spec: Arc<SpecControl>,
    vad: Option<Vad>,
    segmenter: SegmenterConfig,
    next_session: SessionId,
    current: Option<Recording>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Text will arrive from the worker for this session.
    Pending(SessionId),
    /// Nothing was said; nothing will be inserted.
    NoSpeech,
}

impl Dictation {
    pub fn new(jobs: Sender<Job>, spec: Arc<SpecControl>, vad: Option<Vad>) -> Self {
        Self { jobs, spec, vad, segmenter: SegmenterConfig::default(), next_session: 1, current: None }
    }

    pub fn is_recording(&self) -> bool {
        self.current.is_some()
    }

    /// Seconds of audio in the current recording.
    pub fn recorded(&self) -> Duration {
        self.current.as_ref().map_or(Duration::ZERO, |r| {
            Duration::from_secs_f64(r.samples as f64 / dictum_engine::SAMPLE_RATE as f64)
        })
    }

    pub fn start(&mut self, input_rate: u32) {
        if self.current.is_some() {
            self.cancel();
        }
        if let Some(vad) = &mut self.vad {
            vad.reset();
        }
        let session = self.next_session;
        self.next_session += 1;
        self.current = Some(Recording {
            session,
            resampler: Resampler::new(input_rate),
            segmenter: Segmenter::new(self.segmenter.clone()),
            speculated: None,
            resampled: Vec::new(),
            samples: 0,
        });
    }

    /// Feeds mono samples at the rate given to [`Self::start`].
    pub fn feed(&mut self, samples: &[f32]) -> Result<()> {
        let Some(rec) = &mut self.current else { return Ok(()) };
        rec.resampled.clear();
        rec.resampler.push(samples, &mut rec.resampled);
        rec.samples += rec.resampled.len() as u64;
        let events = rec.segmenter.push(&rec.resampled, self.vad.as_mut())?;
        let session = rec.session;
        for event in events {
            match event {
                SegmentEvent::Pause(segment) => {
                    let generation = self.spec.supersede();
                    rec.speculated = Some(segment.key);
                    send(&self.jobs, Job::Speculate { session, generation, segment });
                }
                SegmentEvent::Segment(segment) => {
                    if rec.speculated.take() != Some(segment.key) {
                        self.spec.supersede();
                    }
                    send(&self.jobs, Job::Segment { session, segment });
                }
            }
        }
        Ok(())
    }

    /// Ends the recording and asks the worker for the final text.
    pub fn stop(&mut self) -> Result<Stopped> {
        let Some(mut rec) = self.current.take() else { return Ok(Stopped::NoSpeech) };
        rec.resampled.clear();
        rec.resampler.flush(&mut rec.resampled);
        rec.segmenter.push(&rec.resampled, None)?;
        let segment = rec.segmenter.finish(self.vad.as_mut())?;
        if segment.as_ref().map(|s| s.key) != rec.speculated {
            self.spec.supersede();
        }
        let heard = segment.is_some() || rec.segmenter.heard_speech();
        send(&self.jobs, Job::Finish { session: rec.session, segment });
        Ok(if heard { Stopped::Pending(rec.session) } else { Stopped::NoSpeech })
    }

    /// Discards the recording and any work queued for it.
    pub fn cancel(&mut self) {
        if let Some(rec) = self.current.take() {
            self.spec.supersede();
            send(&self.jobs, Job::Abort { session: rec.session });
        }
    }
}

fn send(jobs: &Sender<Job>, job: Job) {
    if jobs.send(job).is_err() {
        log::error!("transcription worker is gone");
    }
}

#[derive(Default)]
struct SessionState {
    parts: Vec<String>,
    speculation: Option<(SegmentKey, String)>,
}

pub struct Worker<T: Transcriber> {
    transcriber: T,
    spec: Arc<SpecControl>,
    text: TextOptions,
    trailing_space: bool,
    sessions: HashMap<SessionId, SessionState>,
}

impl<T: Transcriber> Worker<T> {
    pub fn new(transcriber: T, spec: Arc<SpecControl>, text: TextOptions, trailing_space: bool) -> Self {
        Self { transcriber, spec, text, trailing_space, sessions: HashMap::new() }
    }

    /// Processes jobs until the channel closes.
    pub fn run(mut self, jobs: Receiver<Job>, done: Sender<Done>) {
        for job in jobs {
            if let Some(result) = self.handle(job) {
                let _ = done.send(result);
            }
        }
    }

    fn handle(&mut self, job: Job) -> Option<Done> {
        match job {
            Job::Speculate { session, generation, segment } => {
                if !self.spec.begin(generation) {
                    return None; // superseded while queued
                }
                let result = self.transcriber.transcribe(&segment.audio, Some(&self.spec.interrupt));
                self.spec.end();
                match result {
                    Ok(Some(text)) => {
                        self.sessions.entry(session).or_default().speculation = Some((segment.key, text));
                    }
                    Ok(None) => log::debug!("speculation superseded mid-run"),
                    Err(e) => log::warn!("speculative transcription failed: {e:#}"),
                }
                None
            }
            Job::Segment { session, segment } => {
                let text = self.text_for(session, &segment);
                let state = self.sessions.entry(session).or_default();
                state.parts.push(text);
                None
            }
            Job::Finish { session, segment } => {
                let started = Instant::now();
                if let Some(segment) = &segment {
                    let text = self.text_for(session, segment);
                    self.sessions.entry(session).or_default().parts.push(text);
                }
                let state = self.sessions.remove(&session).unwrap_or_default();
                let joined = text::join_segments(state.parts);
                let mut processed = text::process(&joined, &self.text);
                if processed.is_empty() {
                    return Some(Done { session, text: None, latency: started.elapsed() });
                }
                if self.trailing_space && !processed.ends_with(char::is_whitespace) {
                    processed.push(' ');
                }
                Some(Done { session, text: Some(processed), latency: started.elapsed() })
            }
            Job::Abort { session } => {
                self.sessions.remove(&session);
                None
            }
        }
    }

    /// Text of a segment, reusing the session's speculation when it covered exactly this audio.
    fn text_for(&mut self, session: SessionId, segment: &FinalSegment) -> String {
        let state = self.sessions.entry(session).or_default();
        if let Some((key, text)) = state.speculation.take()
            && key == segment.key
        {
            return text;
        }
        match self.transcriber.transcribe(&segment.audio, None) {
            Ok(text) => text.unwrap_or_default(),
            Err(e) => {
                log::error!("transcription failed: {e:#}");
                String::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct Calls {
        speculative: AtomicUsize,
        direct: AtomicUsize,
    }

    /// Returns "w<number of samples>"; can block the first speculation until it is interrupted.
    struct Fake {
        calls: Arc<Calls>,
        block_first_speculation: bool,
    }

    impl Transcriber for Fake {
        fn transcribe(&mut self, audio: &[f32], interrupt: Option<&Interrupt>) -> Result<Option<String>> {
            match interrupt {
                Some(interrupt) => {
                    let first = self.calls.speculative.fetch_add(1, Ordering::SeqCst) == 0;
                    if first && self.block_first_speculation {
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while !interrupt.is_triggered() {
                            assert!(Instant::now() < deadline, "speculation was never interrupted");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        return Ok(None);
                    }
                }
                None => {
                    self.calls.direct.fetch_add(1, Ordering::SeqCst);
                }
            }
            Ok(Some(format!("w{}", audio.len())))
        }
    }

    fn jfk() -> Vec<f32> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../dictum-engine/tests/fixtures/jfk.wav");
        let mut reader = hound::WavReader::open(path).unwrap();
        reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
    }

    struct Rig {
        dictation: Dictation,
        done: Receiver<Done>,
        calls: Arc<Calls>,
        worker: std::thread::JoinHandle<()>,
    }

    fn rig(block_first_speculation: bool) -> Rig {
        let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded();
        let (done_tx, done_rx) = crossbeam_channel::unbounded();
        let spec = SpecControl::new().unwrap();
        let calls = Arc::new(Calls::default());
        let fake = Fake { calls: calls.clone(), block_first_speculation };
        let worker = Worker::new(fake, spec.clone(), TextOptions::default(), true);
        let worker = std::thread::spawn(move || worker.run(jobs_rx, done_tx));
        let dictation = Dictation::new(jobs_tx, spec, Some(Vad::new().unwrap()));
        Rig { dictation, done: done_rx, calls, worker }
    }

    fn feed(d: &mut Dictation, audio: &[f32]) {
        for chunk in audio.chunks(480) {
            d.feed(chunk).unwrap();
        }
    }

    /// Stops the worker; returns (speculative, direct) transcription counts.
    fn finish(rig: Rig) -> (usize, usize) {
        drop(rig.dictation);
        rig.worker.join().unwrap();
        (rig.calls.speculative.load(Ordering::SeqCst), rig.calls.direct.load(Ordering::SeqCst))
    }

    #[test]
    fn pause_then_release_reuses_speculation() {
        let mut rig = rig(false);
        rig.dictation.start(16_000);
        let mut audio = jfk()[..16_000 * 7].to_vec(); // stay under the split threshold
        audio.extend(vec![0.0; 16_000]);
        feed(&mut rig.dictation, &audio);
        let Stopped::Pending(session) = rig.dictation.stop().unwrap() else { panic!("speech expected") };
        let done = rig.done.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(done.session, session);
        assert!(done.text.as_deref().unwrap().starts_with('w'));
        assert!(done.text.unwrap().ends_with(' '), "trailing space added");
        let (speculative, direct) = finish(rig);
        assert!(speculative >= 1);
        assert_eq!(direct, 0, "the final segment must reuse the speculative transcript");
    }

    #[test]
    fn speech_until_release_transcribes_tail() {
        let mut rig = rig(false);
        rig.dictation.start(16_000);
        feed(&mut rig.dictation, &jfk()[..16_000 * 4]); // released mid-sentence
        assert!(matches!(rig.dictation.stop().unwrap(), Stopped::Pending(_)));
        let done = rig.done.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(done.text.is_some());
        assert_eq!(finish(rig).1, 1, "the tail is transcribed once");
    }

    #[test]
    fn silence_produces_nothing() {
        let mut rig = rig(false);
        rig.dictation.start(48_000);
        feed(&mut rig.dictation, &vec![0.0; 48_000 * 2]);
        assert_eq!(rig.dictation.stop().unwrap(), Stopped::NoSpeech);
        let done = rig.done.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(done.text, None);
        assert_eq!(finish(rig), (0, 0));
    }

    #[test]
    fn resumed_speech_interrupts_running_speculation() {
        let mut rig = rig(true);
        rig.dictation.start(16_000);
        let speech = jfk();
        let mut audio = speech[..16_000 * 3].to_vec();
        audio.extend(vec![0.0; 16_000]); // pause → speculation starts and blocks
        feed(&mut rig.dictation, &audio);
        std::thread::sleep(Duration::from_millis(50));
        feed(&mut rig.dictation, &speech[16_000 * 3..16_000 * 5]); // keeps talking, releases
        rig.dictation.stop().unwrap();
        // The blocked speculation must be interrupted so the final text still arrives.
        let done = rig.done.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(done.text.is_some());
        let (speculative, _) = finish(rig);
        assert!(speculative >= 1);
    }

    #[test]
    fn cancel_discards_everything() {
        let mut rig = rig(false);
        rig.dictation.start(16_000);
        feed(&mut rig.dictation, &jfk());
        rig.dictation.cancel();
        assert!(!rig.dictation.is_recording());
        assert!(rig.done.recv_timeout(Duration::from_millis(300)).is_err(), "no output after cancel");
        finish(rig);
    }

    #[test]
    fn device_rate_is_resampled() {
        let mut rig = rig(false);
        rig.dictation.start(48_000);
        feed(&mut rig.dictation, &vec![0.0; 48_000]);
        let recorded = rig.dictation.recorded();
        assert!((recorded.as_secs_f64() - 1.0).abs() < 0.01, "{recorded:?}");
        rig.dictation.cancel();
        finish(rig);
    }
}
