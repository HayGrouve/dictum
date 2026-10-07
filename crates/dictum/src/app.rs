//! Wires the pieces together:
//!
//! ```text
//! keyboard hook ─Command─► controller ─Job─► worker ─Done─► output
//! (main thread)            mic + VAD          Parakeet       wait for keys up, insert text
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, select};
use dictum_engine::model::PARAKEET_TDT_V3_INT8;
use dictum_engine::{Parakeet, Vad};

use crate::audio::Mic;
use crate::config::{Bindings, Config};
use crate::hotkey::{Action, Hotkey};
use crate::paths::Paths;
use crate::pipeline::{Dictation, Done, SpecControl, Stopped, Worker};
use crate::platform::{self, Ui};
use crate::ui::{Cue, Status};

#[derive(Debug)]
pub enum Command {
    Hotkey(Action),
    Quit,
}

/// Turns app state into the tray icon + tooltip.
pub struct Board {
    ui: Ui,
    hotkey: String,
    state: Mutex<BoardState>,
}

#[derive(Default)]
struct BoardState {
    loading: Option<String>,
    error: Option<String>,
    recording: bool,
    pending: usize,
}

impl Board {
    fn update(&self, f: impl FnOnce(&mut BoardState)) {
        let mut state = self.state.lock().unwrap();
        f(&mut state);
        let (status, text) = if let Some(error) = &state.error {
            (Status::Error, format!("Dictum: {error}"))
        } else if let Some(loading) = &state.loading {
            (Status::Loading, format!("Dictum: {loading}"))
        } else if state.recording {
            (Status::Recording, "Dictum: listening…".to_string())
        } else if state.pending > 0 {
            (Status::Transcribing, "Dictum: transcribing…".to_string())
        } else {
            (Status::Ready, format!("Dictum: hold {} to dictate", self.hotkey))
        };
        self.ui.set_status(status, text);
    }
}

pub fn run() -> Result<()> {
    let paths = Paths::new()?;
    crate::logging::init(&paths.log_file).context("failed to open the log file")?;
    let Some(_instance) = platform::single_instance() else {
        log::info!("another instance is already running");
        return Ok(());
    };
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));

    let (config, config_error) = match Config::load_or_create(&paths.config_file) {
        Ok(c) => (c, None),
        Err(e) => {
            log::error!("{e:#}");
            (Config::default(), Some(format!("settings error, using defaults ({e})")))
        }
    };
    let bindings: Bindings = config.bindings().or_else(|_| Config::default().bindings())?;
    log::info!(
        "hotkey: {} (hands-free: {:?}, cancel: {:?})",
        bindings.hotkey,
        bindings.hands_free,
        bindings.cancel
    );

    let ui = platform::create_ui(config.sounds)?;
    let board = Arc::new(Board {
        ui: ui.clone(),
        hotkey: bindings.hotkey.to_string(),
        state: Mutex::new(BoardState {
            loading: Some("starting…".into()),
            error: config_error,
            ..Default::default()
        }),
    });
    board.update(|_| {});

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<Command>();
    let (job_tx, job_rx) = crossbeam_channel::unbounded();
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<Done>();
    let spec = SpecControl::new()?;
    let ready = Arc::new(AtomicBool::new(false));

    // Worker: make sure the model is there, load it, then transcribe jobs forever.
    {
        let config = config.clone();
        let board = board.clone();
        let ready = ready.clone();
        let spec = spec.clone();
        let models_dir = config.model_dir.clone().unwrap_or(paths.models_dir.clone());
        spawn("worker", move || {
            // Retry with backoff: the first run may happen offline or on a flaky connection.
            let mut delay = Duration::from_secs(15);
            let engine = loop {
                match load_engine(&config, &models_dir, &board) {
                    Ok(engine) => break engine,
                    Err(e) => {
                        log::error!("{e:#}; retrying in {}s", delay.as_secs());
                        board.update(|s| {
                            s.loading = None;
                            s.error = Some(format!("{e}; retrying in {}s", delay.as_secs()));
                        });
                        std::thread::sleep(delay);
                        delay = (delay * 2).min(Duration::from_secs(300));
                        board.update(|s| s.error = None);
                    }
                }
            };
            ready.store(true, Ordering::SeqCst);
            board.update(|s| s.loading = None);
            Worker::new(engine, spec, config.text_options(), config.trailing_space).run(job_rx, done_tx);
        });
    }

    // Output: insert finished dictations in order.
    {
        let config = config.clone();
        let board = board.clone();
        let ui = ui.clone();
        spawn("output", move || {
            for done in done_rx {
                if let Some(text) = done.text {
                    platform::wait_for_modifiers_released(Duration::from_millis(1500));
                    let started = Instant::now();
                    match platform::insert_text(&ui, &text, config.insert_method, config.restore_clipboard) {
                        Ok(()) => log::info!(
                            "inserted {} chars (text ready {:.0?} after release, insert took {:.0?})",
                            text.chars().count(),
                            done.latency,
                            started.elapsed()
                        ),
                        Err(e) => {
                            log::error!("failed to insert text: {e:#}");
                            ui.cue(Cue::Error);
                        }
                    }
                } else {
                    log::info!("nothing recognised");
                }
                board.update(|s| s.pending = s.pending.saturating_sub(1));
            }
        });
    }

    // Controller: mic + recording state machine.
    {
        let board = board.clone();
        let ui = ui.clone();
        let hotkey = bindings.hotkey.clone();
        let config = config.clone();
        spawn("controller", move || {
            let vad = match Vad::new() {
                Ok(vad) => Some(vad),
                Err(e) => {
                    log::error!("VAD unavailable, recordings won't be trimmed: {e:#}");
                    None
                }
            };
            let dictation = Dictation::new(job_tx, spec, vad);
            let mic = Mic::new(&config.microphone);
            Controller { config, hotkey, ui, board, ready, mic, dictation, mode: Mode::Idle }.run(cmd_rx);
        });
    }

    let restart = platform::run(&ui, bindings, cmd_tx, &paths)?;
    drop(_instance);
    if restart {
        log::info!("restarting");
        platform::relaunch();
    } else {
        log::info!("quitting");
    }
    Ok(())
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().name(name.into()).spawn(f).expect("failed to spawn thread");
}

fn load_engine(config: &Config, models_dir: &std::path::Path, board: &Board) -> Result<Parakeet> {
    let spec = &PARAKEET_TDT_V3_INT8;
    let dir = spec.dir_in(models_dir);
    if !spec.is_installed(&dir) {
        log::info!("downloading speech model to {}", dir.display());
        let total_mb = spec.total_size() / 1_000_000;
        let mut last_percent = u64::MAX;
        spec.download(&dir, &AtomicBool::new(false), |done, total| {
            let percent = done * 100 / total.max(1);
            if percent != last_percent {
                last_percent = percent;
                board.update(|s| {
                    s.loading = Some(format!("downloading speech model ({percent}% of {total_mb} MB)…"))
                });
            }
        })
        .context("could not download the speech model")?;
    }
    board.update(|s| s.loading = Some("loading speech model…".into()));
    let started = Instant::now();
    let engine = Parakeet::load(&dir, &config.engine_options())?;
    log::info!("speech model ready in {:.2?}", started.elapsed());
    Ok(engine)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    Holding { since: Instant },
    HandsFree,
}

/// Taps shorter than this are treated as accidental.
const MIN_HOLD: Duration = Duration::from_millis(250);
/// Hotkey physically up this long without a release event from the hook: stop anyway.
const MISSED_RELEASE: Duration = Duration::from_millis(500);

struct Controller {
    config: Config,
    hotkey: Hotkey,
    ui: Ui,
    board: Arc<Board>,
    ready: Arc<AtomicBool>,
    mic: Mic,
    dictation: Dictation,
    mode: Mode,
}

impl Controller {
    fn run(mut self, commands: Receiver<Command>) {
        self.mic.warm_up();
        let chunks = self.mic.chunks().clone();
        let mut released_since: Option<Instant> = None;
        loop {
            select! {
                recv(commands) -> cmd => match cmd {
                    Ok(Command::Hotkey(action)) => self.on_action(action),
                    Ok(Command::Quit) | Err(_) => break,
                },
                recv(chunks) -> chunk => {
                    if let Ok(chunk) = chunk
                        && self.dictation.is_recording()
                            && let Err(e) = self.dictation.feed(&chunk) {
                                log::error!("audio processing failed: {e:#}");
                            }
                },
                default(Duration::from_millis(100)) => {}
            }

            if !self.dictation.is_recording() {
                continue;
            }
            // Safety net: a missed key-up must not leave the mic on forever. Normal releases
            // arrive from the hook within milliseconds, long before this fires.
            if matches!(self.mode, Mode::Holding { .. }) && !platform::hotkey_held(&self.hotkey) {
                let since = *released_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= MISSED_RELEASE {
                    log::warn!("hotkey no longer held but no release seen; stopping");
                    released_since = None;
                    self.ui.reset_hotkey();
                    self.on_action(Action::Stop);
                }
            } else {
                released_since = None;
            }
            if self.dictation.recorded().as_secs() >= u64::from(self.config.max_recording_secs) {
                log::warn!("maximum recording length reached");
                self.ui.reset_hotkey();
                self.on_action(Action::Stop);
            }
        }
    }

    fn on_action(&mut self, action: Action) {
        match action {
            Action::Start => self.start(),
            Action::LockHandsFree => {
                if self.dictation.is_recording() {
                    self.mode = Mode::HandsFree;
                    self.ui.cue(Cue::Lock);
                }
            }
            Action::Stop => self.stop(),
            Action::Cancel => {
                if self.dictation.is_recording() {
                    self.discard();
                    self.ui.cue(Cue::Cancel);
                    log::info!("recording cancelled");
                }
            }
            Action::Abort => {
                if self.dictation.is_recording() {
                    self.discard();
                    log::debug!("hotkey was part of another shortcut");
                }
            }
        }
    }

    fn start(&mut self) {
        if !self.ready.load(Ordering::SeqCst) {
            self.ui.cue(Cue::Error);
            log::warn!("dictation requested before the speech model is ready");
            return;
        }
        match self.mic.start() {
            Ok(rate) => {
                self.dictation.start(rate);
                self.mode = Mode::Holding { since: Instant::now() };
                self.board.update(|s| s.recording = true);
                self.ui.cue(Cue::Start);
            }
            Err(e) => {
                log::error!("cannot record: {e:#}");
                self.ui.reset_hotkey();
                self.ui.cue(Cue::Error);
                self.board.update(|s| s.error = Some(format!("microphone unavailable ({e})")));
            }
        }
    }

    fn stop(&mut self) {
        if !self.dictation.is_recording() {
            return;
        }
        if let Mode::Holding { since } = self.mode
            && since.elapsed() < MIN_HOLD
        {
            self.discard();
            return;
        }
        self.mic.stop();
        // Audio captured right before the release is still in the channel.
        while let Ok(chunk) = self.mic.chunks().try_recv() {
            let _ = self.dictation.feed(&chunk);
        }
        let recorded = self.dictation.recorded();
        self.mode = Mode::Idle;
        let stopped = self.dictation.stop();
        self.board.update(|s| {
            s.recording = false;
            s.error = None;
            // Every successful stop queues exactly one result for the output thread.
            if stopped.is_ok() {
                s.pending += 1;
            }
        });
        match stopped {
            Ok(Stopped::Pending(_)) => {
                self.ui.cue(Cue::Stop);
                log::info!("recorded {:.1}s", recorded.as_secs_f64());
            }
            Ok(Stopped::NoSpeech) => {
                self.ui.cue(Cue::Cancel);
                log::info!("recorded {:.1}s without speech", recorded.as_secs_f64());
            }
            Err(e) => {
                self.ui.cue(Cue::Error);
                log::error!("failed to finish recording: {e:#}");
            }
        }
        if let Some(name) = self.mic.device_name() {
            log::debug!("microphone was {name}");
        }
    }

    fn discard(&mut self) {
        self.mic.stop();
        self.dictation.cancel();
        self.mode = Mode::Idle;
        self.board.update(|s| s.recording = false);
    }
}
