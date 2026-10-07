//! Global low-level keyboard hook (WH_KEYBOARD_LL) feeding the hotkey state machine.
//!
//! The hook procedure runs on the main thread inside its message loop and must return quickly,
//! so it only updates the state machine and forwards actions over a channel.

use std::cell::RefCell;
use std::time::Instant;

use anyhow::{Result, bail};
use crossbeam_channel::Sender;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use super::{input, keys};
use crate::app::Command;
use crate::hotkey::{Action, Machine};

struct HookState {
    machine: Machine,
    commands: Sender<Command>,
    mask_menu: bool,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

pub struct Hook(HHOOK);

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe { UnhookWindowsHookEx(self.0) };
        STATE.with(|s| s.borrow_mut().take());
    }
}

/// Installs the hook for the calling thread's message loop.
pub fn install(machine: Machine, commands: Sender<Command>) -> Result<Hook> {
    let mask_menu = machine.hotkey().needs_menu_mask();
    STATE.with(|s| *s.borrow_mut() = Some(HookState { machine, commands, mask_menu }));
    let hook =
        unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(std::ptr::null()), 0) };
    if hook.is_null() {
        bail!("failed to install the keyboard hook");
    }
    Ok(Hook(hook))
}

/// Puts the state machine back to idle after the app ended a dictation on its own.
pub fn reset() {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.machine.resync(keys::is_down);
            state.machine.reset();
        }
    });
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if event.dwExtraInfo != input::MARKER && event.scanCode != keys::ALTGR_FAKE_CTRL_SCAN {
            let pressed = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            if handle(keys::from_vk(event.vkCode), pressed) {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Returns whether to swallow the event.
fn handle(key: crate::hotkey::Key, pressed: bool) -> bool {
    let result = STATE.with(|s| {
        let mut guard = s.try_borrow_mut().ok()?;
        let state = guard.as_mut()?;
        if pressed {
            // Recover from key-ups we never saw (secure desktop, lock screen, ...).
            state.machine.resync(|k| k == key || keys::is_down(k));
        }
        let outcome = state.machine.on_key(key, pressed, Instant::now());
        if let Some(action) = outcome.action {
            let _ = state.commands.send(Command::Hotkey(action));
        }
        let mask = state.mask_menu && outcome.action == Some(Action::Start);
        Some((outcome.swallow, mask))
    });
    let Some((swallow, mask)) = result else { return false };
    if mask {
        input::mask_menu_key();
    }
    swallow
}
