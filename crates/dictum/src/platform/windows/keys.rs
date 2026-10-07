//! Virtual-key codes <-> [`Key`].

use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::hotkey::{Hotkey, Key, KeyMatch};

/// Scan code Windows uses for the fake Left Ctrl it sends along with AltGr.
pub const ALTGR_FAKE_CTRL_SCAN: u32 = 0x21D;

pub fn from_vk(vk: u32) -> Key {
    match vk {
        0xA2 | 0x11 => Key::LCtrl,
        0xA3 => Key::RCtrl,
        0xA0 | 0x10 => Key::LShift,
        0xA1 => Key::RShift,
        0xA4 | 0x12 => Key::LAlt,
        0xA5 => Key::RAlt,
        0x5B => Key::LMeta,
        0x5C => Key::RMeta,
        0x14 => Key::CapsLock,
        0x20 => Key::Space,
        0x1B => Key::Escape,
        0x0D => Key::Enter,
        0x09 => Key::Tab,
        0x08 => Key::Backspace,
        0x70..=0x87 => Key::F((vk - 0x70 + 1) as u8),
        0x30..=0x39 => Key::Char(char::from(b'0' + (vk - 0x30) as u8)),
        0x41..=0x5A => Key::Char(char::from(b'a' + (vk - 0x41) as u8)),
        other => Key::Other(other),
    }
}

pub fn to_vk(key: Key) -> Option<u16> {
    Some(match key {
        Key::LCtrl => 0xA2,
        Key::RCtrl => 0xA3,
        Key::LShift => 0xA0,
        Key::RShift => 0xA1,
        Key::LAlt => 0xA4,
        Key::RAlt => 0xA5,
        Key::LMeta => 0x5B,
        Key::RMeta => 0x5C,
        Key::CapsLock => 0x14,
        Key::Space => 0x20,
        Key::Escape => 0x1B,
        Key::Enter => 0x0D,
        Key::Tab => 0x09,
        Key::Backspace => 0x08,
        Key::F(n) if (1..=24).contains(&n) => 0x70 + u16::from(n) - 1,
        Key::Char(c @ '0'..='9') => c as u16,
        Key::Char(c @ 'a'..='z') => c.to_ascii_uppercase() as u16,
        Key::Other(code) => u16::try_from(code).ok()?,
        Key::Fn | Key::F(_) | Key::Char(_) => return None,
    })
}

/// Physical state right now (not the message-queue state).
pub fn is_down(key: Key) -> bool {
    to_vk(key).is_some_and(|vk| unsafe { GetAsyncKeyState(i32::from(vk)) } < 0)
}

fn match_down(m: KeyMatch) -> bool {
    let any = |keys: &[Key]| keys.iter().any(|&k| is_down(k));
    match m {
        KeyMatch::Ctrl => any(&[Key::LCtrl, Key::RCtrl]),
        KeyMatch::Shift => any(&[Key::LShift, Key::RShift]),
        KeyMatch::Alt => any(&[Key::LAlt, Key::RAlt]),
        KeyMatch::Meta => any(&[Key::LMeta, Key::RMeta]),
        KeyMatch::Exact(k) => is_down(k),
    }
}

pub fn hotkey_held(hotkey: &Hotkey) -> bool {
    hotkey.combo.iter().all(|&m| match_down(m))
}

pub const MODIFIERS: [Key; 8] =
    [Key::LCtrl, Key::RCtrl, Key::LShift, Key::RShift, Key::LAlt, Key::RAlt, Key::LMeta, Key::RMeta];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for vk in (0x08u32..=0xA5).filter(|&v| !matches!(v, 0x10..=0x12)) {
            let key = from_vk(vk);
            assert_eq!(to_vk(key).map(u32::from), Some(vk), "vk {vk:#x}");
        }
    }
}
