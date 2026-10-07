//! macOS integration: `NSApplication` run loop + menu bar icon on the main thread, a `CGEventTap`
//! for the hotkey, pasteboard/`CGEventPost` text insertion and `NSSound` cues.
//!
//! Other threads reach the main thread through the main dispatch queue.

mod autostart;
mod clipboard;
mod indicator;
mod input;
mod keys;
mod settings;
mod tap;

use std::cell::RefCell;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType, NSSound,
};
use objc2_foundation::{NSData, NSPoint};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub use input::wait_for_modifiers_released;
pub use keys::hotkey_held;

use crate::app::Command;
use crate::config::{Bindings, InsertMethod};
use crate::hotkey::Machine;
use crate::indicator::{Meter, Phase};
use crate::paths::Paths;
use crate::ui::{Cue, Status};

/// Handle to the UI owned by the main thread; cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct Ui {
    shared: Arc<Shared>,
}

struct Shared {
    sounds: bool,
    indicator: bool,
    status: Mutex<(Status, String)>,
    /// Why Dictum can't work right now (missing permission); overrides the status.
    blocked: Mutex<Option<String>>,
    meter: Arc<Mutex<Meter>>,
}

/// Runs `f` on the main thread soon.
fn main_async(f: impl FnOnce() + Send + 'static) {
    DispatchQueue::main().exec_async(f);
}

/// Runs `f` on the main thread and waits for its result.
fn on_main<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    if MainThreadMarker::new().is_some() {
        return f();
    }
    let mut result = None;
    DispatchQueue::main().exec_sync(|| result = Some(f()));
    result.expect("main-thread task did not run")
}

impl Ui {
    pub fn set_status(&self, status: Status, tooltip: String, phase: Phase) {
        *self.shared.status.lock().unwrap() = (status, tooltip);
        let indicator = self.shared.indicator;
        main_async(move || {
            refresh_tray();
            if indicator {
                indicator::set_phase(phase);
            }
        });
    }

    /// Microphone samples for the indicator's level bars.
    pub fn level(&self, samples: &[f32]) {
        if self.shared.indicator {
            self.shared.meter.lock().unwrap().feed(samples);
        }
    }

    pub fn cue(&self, cue: Cue) {
        if self.shared.sounds {
            main_async(move || play(cue));
        }
    }

    /// Tells the hotkey state machine that the app ended the dictation by itself.
    pub fn reset_hotkey(&self) {
        tap::reset();
    }

    /// Quits and starts Dictum again, like menu → Restart.
    pub fn request_restart(&self) {
        main_async(|| quit(true));
    }

    fn set_blocked(&self, reason: Option<String>) {
        *self.shared.blocked.lock().unwrap() = reason;
        main_async(refresh_tray);
    }
}

thread_local! {
    static SOUNDS: RefCell<Vec<(Cue, Retained<NSSound>)>> = const { RefCell::new(Vec::new()) };
}

fn play(cue: Cue) {
    SOUNDS.with(|sounds| {
        let mut sounds = sounds.borrow_mut();
        if !sounds.iter().any(|(c, _)| *c == cue) {
            let data = NSData::with_bytes(&crate::sounds::wav(cue));
            match NSSound::initWithData(NSSound::alloc(), &data) {
                Some(sound) => sounds.push((cue, sound)),
                None => return log::warn!("could not load the {cue:?} sound"),
            }
        }
        if let Some((_, sound)) = sounds.iter().find(|(c, _)| *c == cue) {
            if sound.isPlaying() {
                sound.stop();
            }
            sound.play();
        }
    });
}

/// Keeps other instances out while alive.
pub struct InstanceGuard(File);

pub fn single_instance() -> Option<InstanceGuard> {
    let dir = dirs::data_local_dir()?.join("Dictum");
    std::fs::create_dir_all(&dir).ok()?;
    let file = File::options().create(true).truncate(false).write(true).open(dir.join("dictum.lock")).ok()?;
    // Released by the OS when the file is closed, including on a crash.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    locked.then_some(InstanceGuard(file))
}

/// The `.app` bundle we run from, if any.
fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    (exe.parent()?.ends_with("Contents/MacOS") && bundle.extension().is_some_and(|e| e == "app"))
        .then(|| bundle.to_path_buf())
}

/// Sets up the app (menu bar only, no Dock icon) and the on-screen indicator if wanted. Must be
/// called on the main thread, which later calls [`run`].
pub fn create_ui(sounds: bool, show_indicator: bool) -> Result<Ui> {
    let mtm = MainThreadMarker::new().context("the UI must be created on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let meter = Arc::new(Mutex::new(Meter::default()));
    let indicator = show_indicator
        && match indicator::create(mtm, meter.clone()) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("{e:#}");
                false
            }
        };
    Ok(Ui {
        shared: Arc::new(Shared {
            sounds,
            indicator,
            status: Mutex::new((Status::Loading, "Dictum".into())),
            blocked: Mutex::new(None),
            meter,
        }),
    })
}

/// Main-thread-only state.
struct MainState {
    ui: Ui,
    commands: Sender<Command>,
    config_file: PathBuf,
    log_file: PathBuf,
    tray: TrayIcon,
    icons: Vec<(Status, Icon)>,
    status_item: MenuItem,
    settings_item: MenuItem,
    log_item: MenuItem,
    autostart_item: CheckMenuItem,
    restart_item: MenuItem,
    quit_item: MenuItem,
    /// The app's main menu: never shown, but it gives text fields Cmd+C/V/X/A/Z.
    _main_menu: Menu,
    restart: bool,
}

thread_local! {
    static MAIN: RefCell<Option<MainState>> = const { RefCell::new(None) };
}

fn refresh_tray() {
    MAIN.with(|m| {
        let m = m.borrow();
        let Some(main) = m.as_ref() else { return };
        let (status, text) = match main.ui.shared.blocked.lock().unwrap().clone() {
            Some(reason) => (Status::Error, format!("Dictum: {reason}")),
            None => main.ui.shared.status.lock().unwrap().clone(),
        };
        if let Some((_, icon)) = main.icons.iter().find(|(s, _)| *s == status) {
            let _ = main.tray.set_icon(Some(icon.clone()));
        }
        let _ = main.tray.set_tooltip(Some(&text));
        main.status_item.set_text(text.trim_start_matches("Dictum: "));
    });
}

/// Opens a file with its default app (`open -t`: the default text editor).
fn open(path: &Path, text_editor: bool) {
    let mut command = std::process::Command::new("/usr/bin/open");
    if text_editor {
        command.arg("-t");
    }
    if let Err(e) = command.arg(path).spawn() {
        log::error!("failed to open {}: {e}", path.display());
    }
}

fn on_menu(id: &MenuId) {
    let action = MAIN.with(|m| {
        let m = m.borrow();
        let main = m.as_ref()?;
        Some(if id == main.settings_item.id() {
            settings::open(&main.config_file);
            None
        } else if id == main.log_item.id() {
            open(&main.log_file, false);
            None
        } else if id == main.autostart_item.id() {
            let enable = main.autostart_item.is_checked();
            if let Err(e) = autostart::set(enable) {
                log::error!("{e:#}");
                main.autostart_item.set_checked(!enable);
            }
            None
        } else if id == main.restart_item.id() {
            Some(true)
        } else if id == main.quit_item.id() {
            Some(false)
        } else {
            None
        })
    });
    if let Some(Some(restart)) = action {
        quit(restart);
    }
}

/// The settings window saved: restart if the config file changed.
fn settings_saved(restart: bool) {
    // The window can change "Start at login" too.
    MAIN.with(|m| {
        if let Some(main) = m.borrow().as_ref() {
            main.autostart_item.set_checked(autostart::is_enabled());
        }
    });
    if restart {
        quit(true);
    }
}

/// Ends [`run`]; it returns `restart`.
fn quit(restart: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    MAIN.with(|m| {
        if let Some(main) = m.borrow_mut().as_mut() {
            main.restart = restart;
            let _ = main.commands.send(Command::Quit);
        }
    });
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    // `stop` takes effect after the next event; post one so that happens now.
    let event =
        NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(),
            0.0,
            0,
            None,
            0,
            0,
            0,
        );
    if let Some(event) = event {
        app.postEvent_atStart(&event, true);
    }
}

/// Starts a fresh copy of Dictum (call after releasing the single-instance guard).
pub fn relaunch() {
    let result = match bundle_path() {
        Some(bundle) => std::process::Command::new("/usr/bin/open").arg("-n").arg(bundle).spawn(),
        None => std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn()),
    };
    if let Err(e) = result {
        log::error!("restart failed: {e}");
    }
}

/// Menu bar apps have no visible menu, but text fields find their edit shortcuts in it.
fn edit_menu() -> tray_icon::menu::Result<Menu> {
    let app = Submenu::with_items("Dictum", true, &[&PredefinedMenuItem::close_window(None)])?;
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    )?;
    Menu::with_items(&[&app, &edit])
}

/// Runs the main-thread event loop until the user quits. Returns `true` to restart.
pub fn run(ui: &Ui, bindings: Bindings, commands: Sender<Command>, paths: &Paths) -> Result<bool> {
    let mtm = MainThreadMarker::new().context("the event loop must run on the main thread")?;
    tap::start(
        Machine::new(bindings.hotkey, bindings.hands_free, bindings.cancel),
        commands.clone(),
        ui.clone(),
    );

    let status_item = MenuItem::new("Starting…", false, None);
    let settings_item = MenuItem::new("Settings…", true, None);
    let log_item = MenuItem::new("Open log", true, None);
    let autostart_item = CheckMenuItem::new("Start at login", true, autostart::is_enabled(), None);
    let restart_item = MenuItem::new("Restart", true, None);
    let quit_item = MenuItem::new("Quit Dictum", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &status_item,
        &PredefinedMenuItem::separator(),
        &settings_item,
        &log_item,
        &autostart_item,
        &PredefinedMenuItem::separator(),
        &restart_item,
        &quit_item,
    ])
    .context("failed to build the menu")?;

    let icons: Vec<(Status, Icon)> =
        [Status::Loading, Status::Ready, Status::Recording, Status::Transcribing, Status::Error]
            .into_iter()
            .filter_map(|s| {
                Icon::from_rgba(crate::icons::rgba(s), crate::icons::SIZE, crate::icons::SIZE)
                    .ok()
                    .map(|i| (s, i))
            })
            .collect();
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Dictum")
        .with_icon(icons[0].1.clone())
        .build()
        .context("failed to create the menu bar icon")?;

    let main_menu = edit_menu().context("failed to build the main menu")?;
    main_menu.init_for_nsapp();

    MAIN.with(|m| {
        *m.borrow_mut() = Some(MainState {
            ui: ui.clone(),
            commands,
            config_file: paths.config_file.clone(),
            log_file: paths.log_file.clone(),
            tray,
            icons,
            status_item,
            settings_item,
            log_item,
            autostart_item,
            restart_item,
            quit_item,
            _main_menu: main_menu,
            restart: false,
        });
    });
    // Menu clicks arrive inside AppKit's menu tracking; handle them once that has finished.
    MenuEvent::set_event_handler(Some(|event: MenuEvent| main_async(move || on_menu(&event.id))));
    refresh_tray();

    NSApplication::sharedApplication(mtm).run();

    MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
    // Removes the menu bar icon before the process exits.
    let restart = MAIN.with(|m| m.borrow_mut().take()).is_some_and(|main| main.restart);
    Ok(restart)
}

pub fn insert_text(_ui: &Ui, text: &str, method: InsertMethod, restore_clipboard: bool) -> Result<()> {
    match method {
        InsertMethod::Type => input::type_text(text),
        InsertMethod::Paste => {
            if !restore_clipboard {
                on_main(|| clipboard::set_text(text))?;
                return input::paste();
            }
            on_main(|| clipboard::save_and_offer(text))?;
            clipboard::arm();
            input::paste()?;
            // Restore as soon as the target app has read the text (or give up waiting).
            if clipboard::wait_consumed(Duration::from_millis(1500)) {
                std::thread::sleep(Duration::from_millis(30));
            } else {
                log::warn!("the focused app did not read the pasted text");
            }
            if let Err(e) = on_main(clipboard::restore) {
                // The text is already in place; only the old clipboard couldn't be put back.
                log::warn!("could not restore the clipboard: {e:#}");
            }
            Ok(())
        }
    }
}
