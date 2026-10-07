//! Global keyboard event tap (`CGEventTap`) feeding the hotkey state machine.
//!
//! The tap runs on its own thread with its own run loop, so a busy main thread (menus, the
//! indicator) never delays key events. macOS disables taps that take too long; the callback only
//! updates the state machine and forwards actions over a channel.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CGKeyCode, CGPreflightListenEventAccess, CGRequestListenEventAccess,
    CGRequestPostEventAccess,
};

use super::{Ui, input, keys};
use crate::app::Command;
use crate::hotkey::{Key, Machine};

struct TapState {
    machine: Machine,
    commands: Sender<Command>,
}

static STATE: Mutex<Option<TapState>> = Mutex::new(None);

thread_local! {
    /// The tap, so the callback can switch it back on after macOS disabled it.
    static PORT: RefCell<Option<CFRetained<CFMachPort>>> = const { RefCell::new(None) };
}

/// macOS 27 renamed the Accessibility list to "Device Control and Data Access".
pub const PERMISSION_HINT: &str = "allow Dictum in System Settings → Privacy & Security → \
    Accessibility (Device Control and Data Access) and Input Monitoring";

/// Starts listening on a background thread. Until macOS lets us listen and type (Accessibility
/// and Input Monitoring), the menu bar shows what to allow and we retry every second.
pub fn start(machine: Machine, commands: Sender<Command>, ui: Ui) {
    *STATE.lock().unwrap() = Some(TapState { machine, commands });
    std::thread::Builder::new()
        .name("keyboard".into())
        .spawn(move || run(&ui))
        .expect("failed to spawn thread");
}

fn run(ui: &Ui) {
    let mut asked = false;
    let port = loop {
        // Accessibility is checked too: without it the hotkey would work but pasting would not.
        if input::accessibility_granted()
            && let Some(port) = create()
        {
            break port;
        }
        if !asked {
            asked = true;
            log::warn!(
                "keyboard access not granted yet (accessibility: {}, input monitoring: {}); asking",
                input::accessibility_granted(),
                CGPreflightListenEventAccess()
            );
            // Each shows the system prompt once; afterwards they only report the state.
            if !input::accessibility_granted() {
                CGRequestPostEventAccess();
            }
            if !CGPreflightListenEventAccess() {
                CGRequestListenEventAccess();
            }
            ui.set_blocked(Some(PERMISSION_HINT.to_string()));
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    if asked {
        log::info!("keyboard access granted");
        ui.set_blocked(None);
    }
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
        log::error!("failed to attach the keyboard tap");
        return;
    };
    let Some(run_loop) = CFRunLoop::current() else { return };
    run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    CGEvent::tap_enable(&port, true);
    PORT.with(|p| *p.borrow_mut() = Some(port));
    log::info!("listening for the hotkey");
    CFRunLoop::run();
}

fn create() -> Option<CFRetained<CFMachPort>> {
    let mask: CGEventMask =
        (1 << CGEventType::KeyDown.0) | (1 << CGEventType::KeyUp.0) | (1 << CGEventType::FlagsChanged.0);
    // SAFETY: the callback matches `CGEventTapCallBack` and uses no user data.
    unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(callback),
            std::ptr::null_mut(),
        )
    }
}

/// Puts the state machine back to idle after the app ended a dictation on its own.
pub fn reset() {
    if let Some(state) = STATE.lock().unwrap().as_mut() {
        state.machine.resync(keys::is_down);
        state.machine.reset();
    }
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    _user_info: *mut c_void,
) -> *mut CGEvent {
    let pass = event.as_ptr();
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        log::warn!("keyboard tap was disabled by macOS; enabling it again");
        PORT.with(|p| {
            if let Some(port) = p.borrow().as_ref() {
                CGEvent::tap_enable(port, true);
            }
        });
        return pass;
    }
    // SAFETY: macOS hands us a valid event for the duration of the callback.
    let event = unsafe { event.as_ref() };
    if CGEvent::integer_value_field(Some(event), CGEventField::EventSourceUserData) == input::MARKER {
        return pass;
    }
    let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode) as CGKeyCode;
    let (key, pressed) = match kind {
        CGEventType::KeyDown => (keys::from_keycode(code), true),
        CGEventType::KeyUp => (keys::from_keycode(code), false),
        CGEventType::FlagsChanged => {
            let Some(pressed) = keys::modifier_pressed(code, CGEvent::flags(Some(event))) else {
                return pass;
            };
            (keys::from_keycode(code), pressed)
        }
        _ => return pass,
    };
    if handle(key, pressed) { std::ptr::null_mut() } else { pass }
}

/// Returns whether to swallow the event.
fn handle(key: Key, pressed: bool) -> bool {
    let Ok(mut guard) = STATE.lock() else { return false };
    let Some(state) = guard.as_mut() else { return false };
    if pressed {
        // Recover from key-ups we never saw (secure input, lock screen, ...).
        state.machine.resync(|k| k == key || keys::is_down(k));
    }
    let outcome = state.machine.on_key(key, pressed, Instant::now());
    if let Some(action) = outcome.action {
        let _ = state.commands.send(Command::Hotkey(action));
    }
    outcome.swallow
}
