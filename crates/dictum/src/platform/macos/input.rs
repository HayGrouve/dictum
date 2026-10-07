//! Synthetic keyboard input. Every event we post carries [`MARKER`] so our own event tap ignores
//! it.

use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGKeyCode,
};

use super::keys;

pub const MARKER: i64 = 0x4449_4354; // "DICT"

/// `CGEventKeyboardSetUnicodeString` takes at most this many UTF-16 units per event.
const MAX_UNITS: usize = 20;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Whether Dictum is allowed under Privacy & Security → Accessibility, which posting key events
/// needs. Unlike `CGPreflightPostEventAccess`, this sees changes without a restart.
pub fn accessibility_granted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

fn event(code: CGKeyCode, down: bool, flags: CGEventFlags) -> Result<CFRetained<CGEvent>> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let event = CGEvent::new_keyboard_event(source.as_deref(), code, down)
        .context("failed to create a keyboard event")?;
    // Set explicitly: otherwise the event inherits whatever modifiers are physically held.
    CGEvent::set_flags(Some(&event), flags);
    CGEvent::set_integer_value_field(Some(&event), CGEventField::EventSourceUserData, MARKER);
    Ok(event)
}

fn post(events: &[CFRetained<CGEvent>]) -> Result<()> {
    if !accessibility_granted() {
        bail!(
            "Dictum may not send keystrokes: allow it under System Settings → Privacy & Security → \
             Accessibility (Device Control and Data Access)"
        );
    }
    for event in events {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
    }
    Ok(())
}

fn tap(code: CGKeyCode, flags: CGEventFlags) -> Result<[CFRetained<CGEvent>; 2]> {
    Ok([event(code, true, flags)?, event(code, false, flags)?])
}

pub fn paste() -> Result<()> {
    let cmd = CGEventFlags::MaskCommand;
    post(&[
        event(keys::LEFT_COMMAND, true, cmd)?,
        event(keys::V, true, cmd)?,
        event(keys::V, false, cmd)?,
        event(keys::LEFT_COMMAND, false, CGEventFlags::empty())?,
    ])
}

fn unicode(units: &[u16]) -> Result<[CFRetained<CGEvent>; 2]> {
    let [down, up] = tap(0, CGEventFlags::empty())?;
    for event in [&down, &up] {
        // SAFETY: `units` is valid for its length for the duration of the call.
        unsafe { CGEvent::keyboard_set_unicode_string(Some(event), units.len() as _, units.as_ptr()) };
    }
    Ok([down, up])
}

/// Types text as Unicode key events. Line breaks become Shift+Return so chat apps don't send.
pub fn type_text(text: &str) -> Result<()> {
    let mut pending: Vec<u16> = Vec::with_capacity(MAX_UNITS);
    let flush = |pending: &mut Vec<u16>| -> Result<()> {
        if !pending.is_empty() {
            post(&unicode(pending)?)?;
            pending.clear();
            // Give the target app a moment to keep up.
            sleep(Duration::from_millis(2));
        }
        Ok(())
    };
    for c in text.chars() {
        match c {
            '\r' => continue,
            '\n' => {
                flush(&mut pending)?;
                let shift = CGEventFlags::MaskShift;
                let [down, up] = tap(keys::RETURN, shift)?;
                post(&[
                    event(keys::LEFT_SHIFT, true, shift)?,
                    down,
                    up,
                    event(keys::LEFT_SHIFT, false, CGEventFlags::empty())?,
                ])?;
            }
            '\t' => {
                flush(&mut pending)?;
                post(&tap(keys::TAB, CGEventFlags::empty())?)?;
            }
            c => {
                let mut buf = [0u16; 2];
                let units = c.encode_utf16(&mut buf);
                if pending.len() + units.len() > MAX_UNITS {
                    flush(&mut pending)?;
                }
                pending.extend_from_slice(units);
            }
        }
    }
    flush(&mut pending)
}

/// Waits until no modifier is physically held, so our Cmd+V can't turn into Ctrl+Cmd+V.
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
