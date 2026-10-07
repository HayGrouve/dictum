//! Platform-independent hotkey logic: parsing hotkey specs and turning raw key events into
//! dictation actions (hold-to-talk, hands-free lock, cancel).

use std::collections::HashSet;
use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// A physical key, as reported by the platform keyboard hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    LCtrl,
    RCtrl,
    LShift,
    RShift,
    LAlt,
    RAlt,
    LMeta,
    RMeta,
    Fn,
    CapsLock,
    Space,
    Escape,
    Enter,
    Tab,
    Backspace,
    F(u8),
    Char(char),
    /// Anything else, by platform key code.
    Other(u32),
}

/// One element of a hotkey combo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMatch {
    Ctrl,
    Shift,
    Alt,
    /// Windows key / Command key.
    Meta,
    Exact(Key),
}

impl KeyMatch {
    pub fn matches(self, key: Key) -> bool {
        match self {
            KeyMatch::Ctrl => matches!(key, Key::LCtrl | Key::RCtrl),
            KeyMatch::Shift => matches!(key, Key::LShift | Key::RShift),
            KeyMatch::Alt => matches!(key, Key::LAlt | Key::RAlt),
            KeyMatch::Meta => matches!(key, Key::LMeta | Key::RMeta),
            KeyMatch::Exact(k) => k == key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkey {
    pub combo: Vec<KeyMatch>,
}

impl Hotkey {
    /// Parses specs like `ctrl+win`, `right_ctrl`, `ctrl+shift+space`, `f13`, `fn`.
    pub fn parse(spec: &str) -> Result<Self> {
        let mut combo = Vec::new();
        for part in spec.split('+').map(|p| p.trim().to_ascii_lowercase()) {
            if part.is_empty() {
                bail!("empty key in hotkey `{spec}`");
            }
            let m = match part.as_str() {
                "ctrl" | "control" => KeyMatch::Ctrl,
                "shift" => KeyMatch::Shift,
                "alt" | "option" => KeyMatch::Alt,
                "win" | "super" | "meta" | "cmd" | "command" => KeyMatch::Meta,
                other => KeyMatch::Exact(parse_key(other)?),
            };
            if combo.contains(&m) {
                bail!("`{part}` appears twice in hotkey `{spec}`");
            }
            combo.push(m);
        }
        if combo.is_empty() {
            bail!("hotkey is empty");
        }
        Ok(Self { combo })
    }

    pub fn contains(&self, key: Key) -> bool {
        self.combo.iter().any(|m| m.matches(key))
    }

    fn satisfied_by(&self, down: &HashSet<Key>) -> bool {
        self.combo.iter().all(|m| down.iter().any(|&k| m.matches(k)))
    }

    /// Whether releasing this combo would open the Start menu / a menu bar (Win, Alt).
    pub fn needs_menu_mask(&self) -> bool {
        self.combo.iter().any(|m| {
            matches!(
                m,
                KeyMatch::Meta
                    | KeyMatch::Alt
                    | KeyMatch::Exact(Key::LMeta | Key::RMeta | Key::LAlt | Key::RAlt)
            )
        })
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self
            .combo
            .iter()
            .map(|m| match m {
                KeyMatch::Ctrl => "Ctrl".to_string(),
                KeyMatch::Shift => "Shift".to_string(),
                KeyMatch::Alt => if cfg!(target_os = "macos") { "Option" } else { "Alt" }.to_string(),
                KeyMatch::Meta => if cfg!(target_os = "macos") { "Cmd" } else { "Win" }.to_string(),
                KeyMatch::Exact(k) => key_name(*k),
            })
            .collect();
        f.write_str(&names.join("+"))
    }
}

pub fn parse_key(name: &str) -> Result<Key> {
    let name = name.trim().to_ascii_lowercase();
    Ok(match name.as_str() {
        "left_ctrl" | "lctrl" => Key::LCtrl,
        "right_ctrl" | "rctrl" => Key::RCtrl,
        "left_shift" | "lshift" => Key::LShift,
        "right_shift" | "rshift" => Key::RShift,
        "left_alt" | "lalt" | "left_option" => Key::LAlt,
        "right_alt" | "ralt" | "altgr" | "right_option" => Key::RAlt,
        "left_win" | "lwin" | "left_cmd" => Key::LMeta,
        "right_win" | "rwin" | "right_cmd" => Key::RMeta,
        "fn" | "globe" => Key::Fn,
        "capslock" | "caps_lock" => Key::CapsLock,
        "space" => Key::Space,
        "escape" | "esc" => Key::Escape,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        n if n.len() > 1
            && n.starts_with('f')
            && n[1..].parse::<u8>().is_ok_and(|x| (1..=24).contains(&x)) =>
        {
            Key::F(n[1..].parse().unwrap())
        }
        n if n.chars().count() == 1 => Key::Char(n.chars().next().unwrap()),
        other => bail!("unknown key `{other}`"),
    })
}

fn key_name(key: Key) -> String {
    match key {
        Key::LCtrl => "Left Ctrl".into(),
        Key::RCtrl => "Right Ctrl".into(),
        Key::LShift => "Left Shift".into(),
        Key::RShift => "Right Shift".into(),
        Key::LAlt => "Left Alt".into(),
        Key::RAlt => "Right Alt".into(),
        Key::LMeta => "Left Win".into(),
        Key::RMeta => "Right Win".into(),
        Key::Fn => "Fn".into(),
        Key::CapsLock => "Caps Lock".into(),
        Key::Space => "Space".into(),
        Key::Escape => "Esc".into(),
        Key::Enter => "Enter".into(),
        Key::Tab => "Tab".into(),
        Key::Backspace => "Backspace".into(),
        Key::F(n) => format!("F{n}"),
        Key::Char(c) => c.to_uppercase().to_string(),
        Key::Other(code) => format!("key {code}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Hotkey pressed: start recording (hold-to-talk).
    Start,
    /// Hotkey released (or pressed again in hands-free mode): transcribe and insert.
    Stop,
    /// Hands-free key pressed while holding the hotkey: keep recording after release.
    LockHandsFree,
    /// Cancel key: discard the recording.
    Cancel,
    /// The hotkey turned out to be part of another shortcut: discard silently.
    Abort,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub action: Option<Action>,
    /// Hide this key event from other applications.
    pub swallow: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Holding {
        since: Instant,
    },
    HandsFree {
        released: bool,
    },
    /// Ignore everything until the combo is fully released.
    WaitRelease,
}

/// Another key pressed this soon after the hotkey means it was a different shortcut
/// (e.g. Ctrl+Win+Right to switch desktops), not dictation.
const SHORTCUT_WINDOW: Duration = Duration::from_millis(300);

pub struct Machine {
    hotkey: Hotkey,
    hands_free: Key,
    cancel: Key,
    down: HashSet<Key>,
    swallowed: HashSet<Key>,
    state: State,
}

impl Machine {
    pub fn new(hotkey: Hotkey, hands_free: Key, cancel: Key) -> Self {
        Self {
            hotkey,
            hands_free,
            cancel,
            down: HashSet::new(),
            swallowed: HashSet::new(),
            state: State::Idle,
        }
    }

    pub fn hotkey(&self) -> &Hotkey {
        &self.hotkey
    }

    /// Drops keys the platform reports as no longer held (missed key-up events, e.g. after the
    /// secure desktop or a lock screen took the keyboard).
    pub fn resync(&mut self, is_down: impl Fn(Key) -> bool) {
        self.down.retain(|&k| is_down(k));
        let down = &self.down;
        self.swallowed.retain(|k| down.contains(k));
        if !self.hotkey.satisfied_by(&self.down) {
            match self.state {
                State::WaitRelease => self.state = State::Idle,
                State::HandsFree { .. } => self.state = State::HandsFree { released: true },
                _ => {}
            }
        }
    }

    /// The app finished the dictation on its own (timeout, error): forget hands-free mode.
    pub fn reset(&mut self) {
        self.state = if self.hotkey.satisfied_by(&self.down) { State::WaitRelease } else { State::Idle };
    }

    pub fn on_key(&mut self, key: Key, pressed: bool, now: Instant) -> Outcome {
        if pressed {
            if !self.down.insert(key) {
                // Auto-repeat.
                return Outcome { action: None, swallow: self.swallowed.contains(&key) };
            }
            // A fresh press: only hide its release if we decide to hide this press.
            self.swallowed.remove(&key);
        } else {
            self.down.remove(&key);
            if self.swallowed.remove(&key) {
                let mut outcome = self.on_release(key);
                outcome.swallow = true;
                return outcome;
            }
            return self.on_release(key);
        }

        let active = self.hotkey.satisfied_by(&self.down);
        match self.state {
            State::Idle => {
                let only_combo_keys = self.down.iter().all(|&k| self.hotkey.contains(k));
                if active && only_combo_keys && self.hotkey.contains(key) {
                    self.state = State::Holding { since: now };
                    return self.act(Action::Start, false);
                }
            }
            State::Holding { since } => {
                if self.hotkey.contains(key) {
                    return Outcome::default();
                }
                if key == self.cancel {
                    self.state = State::WaitRelease;
                    return self.swallow(key, Action::Cancel);
                }
                if key == self.hands_free {
                    self.state = State::HandsFree { released: false };
                    return self.swallow(key, Action::LockHandsFree);
                }
                if now.duration_since(since) < SHORTCUT_WINDOW {
                    self.state = State::WaitRelease;
                    return self.act(Action::Abort, false);
                }
            }
            State::HandsFree { released } => {
                if key == self.cancel {
                    self.state = if active { State::WaitRelease } else { State::Idle };
                    return self.swallow(key, Action::Cancel);
                }
                if released && active && self.hotkey.contains(key) {
                    self.state = State::WaitRelease;
                    return self.act(Action::Stop, false);
                }
            }
            State::WaitRelease => {}
        }
        Outcome::default()
    }

    fn on_release(&mut self, key: Key) -> Outcome {
        let active = self.hotkey.satisfied_by(&self.down);
        match self.state {
            State::Holding { .. } if self.hotkey.contains(key) && !active => {
                self.state = State::Idle;
                self.act(Action::Stop, false)
            }
            State::HandsFree { released: false } if !active => {
                self.state = State::HandsFree { released: true };
                Outcome::default()
            }
            State::WaitRelease if !active => {
                self.state = State::Idle;
                Outcome::default()
            }
            _ => Outcome::default(),
        }
    }

    fn act(&self, action: Action, swallow: bool) -> Outcome {
        Outcome { action: Some(action), swallow }
    }

    fn swallow(&mut self, key: Key, action: Action) -> Outcome {
        self.swallowed.insert(key);
        self.act(action, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        m: Machine,
        t: Instant,
    }

    impl Harness {
        fn new(spec: &str) -> Self {
            Self { m: Machine::new(Hotkey::parse(spec).unwrap(), Key::Space, Key::Escape), t: Instant::now() }
        }
        fn wait(&mut self, ms: u64) {
            self.t += Duration::from_millis(ms);
        }
        fn down(&mut self, k: Key) -> Outcome {
            self.m.on_key(k, true, self.t)
        }
        fn up(&mut self, k: Key) -> Outcome {
            self.m.on_key(k, false, self.t)
        }
        fn act(&mut self, k: Key, pressed: bool) -> Option<Action> {
            self.m.on_key(k, pressed, self.t).action
        }
    }

    #[test]
    fn parses_specs() {
        assert_eq!(Hotkey::parse("Ctrl+Win").unwrap().combo, vec![KeyMatch::Ctrl, KeyMatch::Meta]);
        assert_eq!(Hotkey::parse("right_ctrl").unwrap().combo, vec![KeyMatch::Exact(Key::RCtrl)]);
        assert_eq!(
            Hotkey::parse("ctrl + shift + space").unwrap().combo,
            vec![KeyMatch::Ctrl, KeyMatch::Shift, KeyMatch::Exact(Key::Space)]
        );
        assert_eq!(Hotkey::parse("f13").unwrap().combo, vec![KeyMatch::Exact(Key::F(13))]);
        assert!(Hotkey::parse("ctrl+").is_err());
        assert!(Hotkey::parse("ctrl+ctrl").is_err());
        assert!(Hotkey::parse("hyper").is_err());
        assert!(Hotkey::parse("f99").is_err());
    }

    #[test]
    fn displays_specs() {
        if cfg!(not(target_os = "macos")) {
            assert_eq!(Hotkey::parse("ctrl+win").unwrap().to_string(), "Ctrl+Win");
        }
        assert_eq!(Hotkey::parse("right_ctrl").unwrap().to_string(), "Right Ctrl");
    }

    #[test]
    fn menu_mask_only_for_win_and_alt() {
        assert!(Hotkey::parse("ctrl+win").unwrap().needs_menu_mask());
        assert!(Hotkey::parse("right_alt").unwrap().needs_menu_mask());
        assert!(!Hotkey::parse("right_ctrl").unwrap().needs_menu_mask());
    }

    #[test]
    fn hold_to_talk_either_order() {
        let mut h = Harness::new("ctrl+win");
        assert_eq!(h.act(Key::LCtrl, true), None);
        assert_eq!(h.act(Key::LMeta, true), Some(Action::Start));
        h.wait(2_000);
        assert_eq!(h.act(Key::LMeta, true), None, "auto-repeat is ignored");
        assert_eq!(h.act(Key::LCtrl, false), Some(Action::Stop));
        assert_eq!(h.act(Key::LMeta, false), None);

        assert_eq!(h.act(Key::RMeta, true), None);
        assert_eq!(h.act(Key::RCtrl, true), Some(Action::Start));
        h.wait(500);
        assert_eq!(h.act(Key::RMeta, false), Some(Action::Stop));
        assert_eq!(h.act(Key::RCtrl, false), None);
    }

    #[test]
    fn other_shortcut_aborts() {
        let mut h = Harness::new("ctrl+win");
        h.down(Key::LCtrl);
        assert_eq!(h.act(Key::LMeta, true), Some(Action::Start));
        h.wait(80);
        let o = h.down(Key::Char('d'));
        assert_eq!(o, Outcome { action: Some(Action::Abort), swallow: false });
        assert_eq!(h.act(Key::Char('d'), false), None);
        assert_eq!(h.act(Key::LMeta, false), None);
        assert_eq!(h.act(Key::LCtrl, false), None);
        // Works again afterwards.
        h.down(Key::LCtrl);
        assert_eq!(h.act(Key::LMeta, true), Some(Action::Start));
    }

    #[test]
    fn stray_key_late_in_dictation_is_ignored() {
        let mut h = Harness::new("ctrl+win");
        h.down(Key::LCtrl);
        h.down(Key::LMeta);
        h.wait(1_500);
        assert_eq!(h.act(Key::Char('x'), true), None);
        assert_eq!(h.act(Key::LCtrl, false), Some(Action::Stop));
    }

    #[test]
    fn does_not_start_when_extra_modifier_is_held() {
        let mut h = Harness::new("ctrl+win");
        h.down(Key::LShift);
        h.down(Key::LCtrl);
        assert_eq!(h.act(Key::LMeta, true), None);
    }

    #[test]
    fn hands_free_lock_and_stop() {
        let mut h = Harness::new("ctrl+win");
        h.down(Key::LCtrl);
        assert_eq!(h.act(Key::LMeta, true), Some(Action::Start));
        let o = h.down(Key::Space);
        assert_eq!(o, Outcome { action: Some(Action::LockHandsFree), swallow: true });
        assert!(h.up(Key::Space).swallow, "space release is hidden too");
        assert_eq!(h.act(Key::LMeta, false), None, "keeps recording after release");
        assert_eq!(h.act(Key::LCtrl, false), None);
        h.wait(10_000);
        assert_eq!(h.act(Key::Char('a'), true), None, "typing does not stop hands-free mode");
        h.up(Key::Char('a'));
        h.down(Key::LCtrl);
        assert_eq!(h.act(Key::LMeta, true), Some(Action::Stop));
        assert_eq!(h.act(Key::LMeta, false), None);
        assert_eq!(h.act(Key::LCtrl, false), None);
        // Next press starts a fresh dictation.
        h.down(Key::LCtrl);
        assert_eq!(h.act(Key::LMeta, true), Some(Action::Start));
    }

    #[test]
    fn escape_cancels_and_is_swallowed() {
        let mut h = Harness::new("ctrl+win");
        h.down(Key::LCtrl);
        h.down(Key::LMeta);
        let o = h.down(Key::Escape);
        assert_eq!(o, Outcome { action: Some(Action::Cancel), swallow: true });
        assert!(h.up(Key::Escape).swallow);
        assert_eq!(h.act(Key::LCtrl, false), None, "release after cancel does nothing");
        assert_eq!(h.act(Key::LMeta, false), None);
        assert_eq!(h.down(Key::Escape), Outcome::default(), "escape is untouched when idle");
    }

    #[test]
    fn escape_cancels_hands_free() {
        let mut h = Harness::new("right_ctrl");
        h.down(Key::RCtrl);
        h.down(Key::Space);
        h.up(Key::Space);
        h.up(Key::RCtrl);
        assert_eq!(h.act(Key::Escape, true), Some(Action::Cancel));
        h.up(Key::Escape);
        assert_eq!(h.act(Key::RCtrl, true), Some(Action::Start));
    }

    #[test]
    fn single_key_hotkey() {
        let mut h = Harness::new("right_ctrl");
        assert_eq!(h.act(Key::RCtrl, true), Some(Action::Start));
        h.wait(900);
        assert_eq!(h.act(Key::RCtrl, false), Some(Action::Stop));
        assert_eq!(h.act(Key::LCtrl, true), None, "left ctrl is a different key");
    }

    #[test]
    fn resync_recovers_from_missed_key_up() {
        let mut h = Harness::new("ctrl+win");
        h.down(Key::LCtrl);
        assert_eq!(h.act(Key::LMeta, true), Some(Action::Start));
        h.down(Key::Space); // hands-free
        // Key-ups were swallowed by the secure desktop; the platform says nothing is held now.
        h.m.resync(|_| false);
        h.down(Key::LCtrl);
        assert_eq!(h.act(Key::LMeta, true), Some(Action::Stop));
        // The lost Space release must not cause a later Space release to be hidden.
        h.up(Key::LMeta);
        h.up(Key::LCtrl);
        h.down(Key::Space);
        assert!(!h.up(Key::Space).swallow);
    }
}
