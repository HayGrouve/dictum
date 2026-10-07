//! macOS virtual key codes (`kVK_*`, US layout positions) <-> [`Key`].

use objc2_core_graphics::{CGEventFlags, CGEventSource, CGEventSourceStateID, CGKeyCode};

use crate::hotkey::{Hotkey, Key, KeyMatch};

pub const RETURN: CGKeyCode = 0x24;
pub const TAB: CGKeyCode = 0x30;
pub const V: CGKeyCode = 0x09;
pub const LEFT_SHIFT: CGKeyCode = 0x38;
pub const LEFT_COMMAND: CGKeyCode = 0x37;

const FN: CGKeyCode = 0x3F;
const CAPS_LOCK: CGKeyCode = 0x39;

/// Letters and digits by their ANSI key position.
const CHARS: [(CGKeyCode, char); 36] = [
    (0x00, 'a'),
    (0x0B, 'b'),
    (0x08, 'c'),
    (0x02, 'd'),
    (0x0E, 'e'),
    (0x03, 'f'),
    (0x05, 'g'),
    (0x04, 'h'),
    (0x22, 'i'),
    (0x26, 'j'),
    (0x28, 'k'),
    (0x25, 'l'),
    (0x2E, 'm'),
    (0x2D, 'n'),
    (0x1F, 'o'),
    (0x23, 'p'),
    (0x0C, 'q'),
    (0x0F, 'r'),
    (0x01, 's'),
    (0x11, 't'),
    (0x20, 'u'),
    (0x09, 'v'),
    (0x0D, 'w'),
    (0x07, 'x'),
    (0x10, 'y'),
    (0x06, 'z'),
    (0x1D, '0'),
    (0x12, '1'),
    (0x13, '2'),
    (0x14, '3'),
    (0x15, '4'),
    (0x17, '5'),
    (0x16, '6'),
    (0x1A, '7'),
    (0x1C, '8'),
    (0x19, '9'),
];

const FUNCTION_KEYS: [(CGKeyCode, u8); 20] = [
    (0x7A, 1),
    (0x78, 2),
    (0x63, 3),
    (0x76, 4),
    (0x60, 5),
    (0x61, 6),
    (0x62, 7),
    (0x64, 8),
    (0x65, 9),
    (0x6D, 10),
    (0x67, 11),
    (0x6F, 12),
    (0x69, 13),
    (0x6B, 14),
    (0x71, 15),
    (0x6A, 16),
    (0x40, 17),
    (0x4F, 18),
    (0x50, 19),
    (0x5A, 20),
];

/// Modifier keys: key code, the device-dependent flag bit for that side (`NX_DEVICE*KEYMASK`)
/// and the generic flag shared by both sides.
const MODIFIER_FLAGS: [(Key, CGKeyCode, u64, CGEventFlags); 8] = [
    (Key::LCtrl, 0x3B, 0x0000_0001, CGEventFlags::MaskControl),
    (Key::RCtrl, 0x3E, 0x0000_2000, CGEventFlags::MaskControl),
    (Key::LShift, LEFT_SHIFT, 0x0000_0002, CGEventFlags::MaskShift),
    (Key::RShift, 0x3C, 0x0000_0004, CGEventFlags::MaskShift),
    (Key::LMeta, LEFT_COMMAND, 0x0000_0008, CGEventFlags::MaskCommand),
    (Key::RMeta, 0x36, 0x0000_0010, CGEventFlags::MaskCommand),
    (Key::LAlt, 0x3A, 0x0000_0020, CGEventFlags::MaskAlternate),
    (Key::RAlt, 0x3D, 0x0000_0040, CGEventFlags::MaskAlternate),
];

pub fn from_keycode(code: CGKeyCode) -> Key {
    if let Some(&(key, ..)) = MODIFIER_FLAGS.iter().find(|m| m.1 == code) {
        return key;
    }
    if let Some(&(_, c)) = CHARS.iter().find(|(k, _)| *k == code) {
        return Key::Char(c);
    }
    if let Some(&(_, n)) = FUNCTION_KEYS.iter().find(|(k, _)| *k == code) {
        return Key::F(n);
    }
    match code {
        FN => Key::Fn,
        CAPS_LOCK => Key::CapsLock,
        0x31 => Key::Space,
        0x35 => Key::Escape,
        RETURN => Key::Enter,
        TAB => Key::Tab,
        0x33 => Key::Backspace,
        other => Key::Other(u32::from(other)),
    }
}

pub fn to_keycode(key: Key) -> Option<CGKeyCode> {
    if let Some(&(_, code, ..)) = MODIFIER_FLAGS.iter().find(|m| m.0 == key) {
        return Some(code);
    }
    Some(match key {
        Key::Fn => FN,
        Key::CapsLock => CAPS_LOCK,
        Key::Space => 0x31,
        Key::Escape => 0x35,
        Key::Enter => RETURN,
        Key::Tab => TAB,
        Key::Backspace => 0x33,
        Key::F(n) => FUNCTION_KEYS.iter().find(|(_, f)| *f == n)?.0,
        Key::Char(c) => CHARS.iter().find(|(_, ch)| *ch == c)?.0,
        Key::Other(code) => CGKeyCode::try_from(code).ok()?,
        _ => return None,
    })
}

/// For a `FlagsChanged` event: whether the modifier with this key code is now down. `None` for
/// key codes that are not modifiers.
pub fn modifier_pressed(code: CGKeyCode, flags: CGEventFlags) -> Option<bool> {
    match code {
        FN => return Some(flags.contains(CGEventFlags::MaskSecondaryFn)),
        // Caps Lock reports its lock state, not the key: treat each change as a press and release.
        CAPS_LOCK => return Some(flags.contains(CGEventFlags::MaskAlphaShift)),
        _ => {}
    }
    let &(_, _, bit, generic) = MODIFIER_FLAGS.iter().find(|m| m.1 == code)?;
    if flags.0 & bit != 0 {
        return Some(true);
    }
    // Some virtual keyboards only set the generic flag; then it stands for this side.
    let either_side = MODIFIER_FLAGS.iter().any(|m| m.3 == generic && flags.0 & m.2 != 0);
    Some(!either_side && flags.contains(generic))
}

/// Physical state right now.
pub fn is_down(key: Key) -> bool {
    let state = CGEventSourceStateID::HIDSystemState;
    let by_code = to_keycode(key).is_some_and(|code| CGEventSource::key_state(state, code));
    // Fn has no reliable key state on every keyboard; its flag is.
    by_code || (key == Key::Fn && CGEventSource::flags_state(state).contains(CGEventFlags::MaskSecondaryFn))
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

pub const MODIFIERS: [Key; 9] =
    [Key::LCtrl, Key::RCtrl, Key::LShift, Key::RShift, Key::LAlt, Key::RAlt, Key::LMeta, Key::RMeta, Key::Fn];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for code in 0..0x80 {
            let key = from_keycode(code);
            assert_eq!(to_keycode(key), Some(code), "key code {code:#x}");
        }
    }

    #[test]
    fn modifier_sides() {
        let left_cmd = CGEventFlags(CGEventFlags::MaskCommand.0 | 0x08);
        assert_eq!(modifier_pressed(LEFT_COMMAND, left_cmd), Some(true));
        assert_eq!(modifier_pressed(0x36, left_cmd), Some(false), "right Cmd released, left held");
        assert_eq!(modifier_pressed(LEFT_COMMAND, CGEventFlags::empty()), Some(false));
        assert_eq!(modifier_pressed(LEFT_COMMAND, CGEventFlags::MaskCommand), Some(true), "generic only");
        assert_eq!(modifier_pressed(FN, CGEventFlags::MaskSecondaryFn), Some(true));
        assert_eq!(modifier_pressed(0x31, CGEventFlags::empty()), None);
    }
}
