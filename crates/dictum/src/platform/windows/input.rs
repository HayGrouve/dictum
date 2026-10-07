//! Synthetic keyboard input. Every event we inject carries [`MARKER`] so our own hook ignores it.

use std::mem::size_of;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
    MapVirtualKeyW, SendInput, VK_CONTROL, VK_RETURN, VK_SHIFT, VK_TAB, VK_V,
};

use super::keys;

pub const MARKER: usize = 0x4449_4354; // "DICT"

/// Unassigned virtual key, used to stop Windows from treating a Win/Alt press as a lone tap.
const VK_MASK: u16 = 0xE8;

fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: MARKER,
            },
        },
    }
}

fn unicode(unit: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: MARKER,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        bail!("input was blocked (is the target app running as administrator?)");
    }
    Ok(())
}

/// Keeps a held Win/Alt key from opening the Start menu / menu bar when it is released.
pub fn mask_menu_key() {
    let _ = send(&[key(VK_MASK, false), key(VK_MASK, true)]);
}

pub fn paste() -> Result<()> {
    send(&[key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)])
}

/// Types text as Unicode key events. Line breaks become Shift+Enter so chat apps don't send.
pub fn type_text(text: &str) -> Result<()> {
    let mut batch = Vec::with_capacity(128);
    for c in text.chars() {
        match c {
            '\r' => continue,
            '\n' => batch.extend([
                key(VK_SHIFT, false),
                key(VK_RETURN, false),
                key(VK_RETURN, true),
                key(VK_SHIFT, true),
            ]),
            '\t' => batch.extend([key(VK_TAB, false), key(VK_TAB, true)]),
            c => {
                let mut buf = [0u16; 2];
                for &unit in c.encode_utf16(&mut buf).iter() {
                    batch.push(unicode(unit, false));
                    batch.push(unicode(unit, true));
                }
            }
        }
        if batch.len() >= 120 {
            send(&batch)?;
            batch.clear();
            // Give the target app's message queue a moment to keep up.
            sleep(Duration::from_millis(2));
        }
    }
    send(&batch)
}

/// Waits until no modifier is physically held, so our Ctrl+V can't turn into Ctrl+Win+V.
pub fn wait_for_modifiers_released(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while keys::MODIFIERS.iter().any(|&k| keys::is_down(k)) {
        if Instant::now() >= deadline {
            log::warn!("modifier keys still held; inserting anyway");
            return;
        }
        sleep(Duration::from_millis(5));
    }
}
