//! Integration tests against the real Windows desktop: clipboard, text insertion into a live
//! edit control, and the global keyboard hook. GitHub's Windows runners provide an interactive
//! session; anywhere without one these tests report "SKIPPED" and pass.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, SetFocus, VK_RCONTROL, VK_SPACE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CB_SETCURSEL, CBN_SELCHANGE, CreateWindowExW, DestroyWindow, DispatchMessageW, GetDlgItem,
    GetForegroundWindow, GetWindowTextW, IDCANCEL, IDOK, MSG, PM_REMOVE, PeekMessageW, SW_SHOW,
    SendDlgItemMessageW, SendMessageW, SetForegroundWindow, ShowWindow, TranslateMessage, WM_COMMAND,
    WS_BORDER, WS_POPUP, WS_VISIBLE,
};

use super::settings::{self as window, *};
use super::*;
use crate::config::Config;
use crate::hotkey::{Action, Hotkey, Key};
use crate::indicator::Phase;

/// The clipboard, focus and keyboard are global: run these one at a time.
static DESKTOP: Mutex<()> = Mutex::new(());

/// Kills the test process with a clear message if a desktop test hangs (a stuck SendMessage
/// would otherwise hang CI silently).
struct Watchdog(Option<crossbeam_channel::Sender<()>>);

impl Watchdog {
    fn new(name: &'static str) -> Self {
        let (tx, rx) = crossbeam_channel::bounded::<()>(1);
        std::thread::spawn(move || {
            if let Err(crossbeam_channel::RecvTimeoutError::Timeout) =
                rx.recv_timeout(Duration::from_secs(60))
            {
                eprintln!("WATCHDOG: {name} hung for 60 s; aborting");
                std::process::exit(101);
            }
        });
        Self(Some(tx))
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.0.take();
    }
}

fn step(name: &str) {
    eprintln!("  .. {name}");
}

fn pump_for(duration: Duration) {
    let deadline = Instant::now() + duration;
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while Instant::now() < deadline {
        while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn key_event(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                // Not our marker: the hook must treat these like real key presses.
                dwExtraInfo: 0,
            },
        },
    }
}

fn press(inputs: &[INPUT]) -> bool {
    unsafe {
        SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) as usize == inputs.len()
    }
}

/// A focused, empty top-level edit control, or `None` without an interactive desktop.
fn focused_edit() -> Option<HWND> {
    let class = wide("EDIT");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            wide("").as_ptr(),
            // No system menu: a stray Alt must not put the window into modal menu mode.
            WS_POPUP | WS_BORDER | WS_VISIBLE,
            100,
            100,
            600,
            200,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return None;
    }
    for _ in 0..20 {
        // Synthetic input makes this process the last one to receive input, which lets it take
        // the foreground. An unassigned key has no side effects (Alt would enter menu mode).
        press(&[key_event(0xE8, false), key_event(0xE8, true)]);
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            SetFocus(hwnd);
        }
        pump_for(Duration::from_millis(50));
        if unsafe { GetForegroundWindow() } == hwnd {
            return Some(hwnd);
        }
    }
    unsafe { DestroyWindow(hwnd) };
    None
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = vec![0u16; 4096];
    let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// Inserts on a helper thread (as the app does) while this thread pumps the edit control.
fn insert_and_read(method: InsertMethod, text: &str) -> Option<String> {
    let ui = create_ui(false, false).unwrap();
    step("focus an edit control");
    let edit = focused_edit()?;
    step("insert");
    let worker = {
        let ui = ui.clone();
        let text = text.to_string();
        std::thread::spawn(move || insert_text(&ui, &text, method, true))
    };
    // Keep pumping while joining: the worker may be waiting on a message to this thread.
    while !worker.is_finished() {
        pump_for(Duration::from_millis(20));
    }
    worker.join().unwrap().unwrap();
    step("read back");
    pump_for(Duration::from_millis(200));
    let result = window_text(edit);
    unsafe { DestroyWindow(edit) };
    Some(result)
}

#[test]
fn clipboard_round_trip_preserves_previous_contents() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    let _watchdog = Watchdog::new("clipboard_round_trip");
    let owner = create_message_window().unwrap();
    step("set original");
    if clipboard::set_text(owner, "original clipboard ✓").is_err() {
        eprintln!("SKIPPED: no clipboard");
        return;
    }
    step("save");
    let saved = clipboard::save(owner).unwrap();
    step("set dictated");
    let sequence = clipboard::set_text(owner, "dictated text").unwrap();
    assert_eq!(clipboard::read_text(owner).as_deref(), Some("dictated text"));
    assert_eq!(clipboard::sequence(), sequence);
    step("restore");
    clipboard::restore(owner, &saved).unwrap();
    assert_eq!(clipboard::read_text(owner).as_deref(), Some("original clipboard ✓"));
}

#[test]
fn paste_inserts_text_and_restores_clipboard() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    let _watchdog = Watchdog::new("paste_inserts_text");
    let owner = create_message_window().unwrap();
    if clipboard::set_text(owner, "keep me").is_err() {
        eprintln!("SKIPPED: no clipboard");
        return;
    }
    let text = "Hello from Dictum — ünïcödé ✓, line one. ";
    let Some(inserted) = insert_and_read(InsertMethod::Paste, text) else {
        eprintln!("SKIPPED: no interactive desktop");
        return;
    };
    assert_eq!(inserted, text);
    assert_eq!(clipboard::read_text(owner).as_deref(), Some("keep me"), "clipboard restored");
}

#[test]
fn typing_inserts_text_without_touching_clipboard() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    let _watchdog = Watchdog::new("typing_inserts_text");
    let owner = create_message_window().unwrap();
    let _ = clipboard::set_text(owner, "untouched");
    let text = "Typed by Dictum: ünïcödé ✓ 😀 ";
    let Some(inserted) = insert_and_read(InsertMethod::Type, text) else {
        eprintln!("SKIPPED: no interactive desktop");
        return;
    };
    assert_eq!(inserted, text);
    assert_eq!(clipboard::read_text(owner).as_deref(), Some("untouched"));
}

#[test]
fn keyboard_hook_drives_hotkey_actions() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    let _watchdog = Watchdog::new("keyboard_hook");
    let (tx, rx) = crossbeam_channel::unbounded();
    let machine = Machine::new(Hotkey::parse("right_ctrl").unwrap(), Key::Space, Key::Escape);
    let _hook = hook::install(machine, tx).unwrap();
    let next = || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            pump_for(Duration::from_millis(10));
            if let Ok(Command::Hotkey(action)) = rx.try_recv() {
                return Some(action);
            }
        }
        None
    };

    if !press(&[key_event(VK_RCONTROL, false)]) {
        eprintln!("SKIPPED: input injection unavailable");
        return;
    }
    let Some(first) = next() else {
        press(&[key_event(VK_RCONTROL, true)]);
        eprintln!("SKIPPED: hook received no events (no interactive desktop)");
        return;
    };
    assert_eq!(first, Action::Start);
    assert!(hotkey_held(&Hotkey::parse("right_ctrl").unwrap()), "held key is visible to the watchdog");
    std::thread::sleep(Duration::from_millis(400));
    press(&[key_event(VK_SPACE, false), key_event(VK_SPACE, true)]);
    assert_eq!(next(), Some(Action::LockHandsFree));
    press(&[key_event(VK_RCONTROL, true)]);
    pump_for(Duration::from_millis(100));
    assert!(rx.try_recv().is_err(), "releasing in hands-free mode keeps recording");
    press(&[key_event(VK_RCONTROL, false)]);
    assert_eq!(next(), Some(Action::Stop));
    press(&[key_event(VK_RCONTROL, true)]);
    pump_for(Duration::from_millis(50));
}

#[test]
fn hotkey_state_is_read_from_the_keyboard() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    assert!(!hotkey_held(&Hotkey::parse("ctrl+shift+f13").unwrap()));
}

/// A config file of its own, removed afterwards.
struct TempConfig(std::path::PathBuf);

impl TempConfig {
    fn new(name: &str, text: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dictum-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), text).unwrap();
        Self(dir)
    }

    fn path(&self) -> std::path::PathBuf {
        self.0.join("config.toml")
    }

    fn text(&self) -> String {
        std::fs::read_to_string(self.path()).unwrap()
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn click(dialog: HWND, id: i32) {
    unsafe { SendMessageW(dialog, WM_COMMAND, id as usize, 0) };
}

const SETTINGS_FILE: &str = "# Mine\nhotkey = \"right_ctrl\"\nthreads = 2\nvocabulary = [\"Vercel\"]\n\n[replacements]\n\"get hub\" = \"GitHub\"\n";

#[test]
fn settings_window_saves_changes_into_the_file() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    let _watchdog = Watchdog::new("settings_window_saves");
    let config = TempConfig::new("settings-save", SETTINGS_FILE);
    window::open(&config.path());
    let dialog = window::window().expect("settings window opens");
    pump_for(Duration::from_millis(300));

    step("fields show the file");
    assert_eq!(text(dialog, ID_HOTKEY), "Right Ctrl");
    assert!(checked(dialog, ID_SOUNDS) && checked(dialog, ID_FILLERS) && !checked(dialog, ID_COMMANDS));
    assert_eq!(selection(dialog, ID_MICROPHONE), Some(0), "system default");
    assert_eq!(selection(dialog, ID_INSERT), Some(0), "paste");
    assert_eq!(text(dialog, ID_VOCABULARY), "Vercel");
    assert_eq!(text(dialog, ID_REPLACEMENTS), "get hub = GitHub");

    step("choosing typing disables the clipboard option");
    let restore = unsafe { GetDlgItem(dialog, ID_RESTORE) };
    assert_ne!(unsafe { IsWindowEnabled(restore) }, 0);
    unsafe { SendDlgItemMessageW(dialog, ID_INSERT, CB_SETCURSEL, 1, 0) };
    click(dialog, ID_INSERT | (CBN_SELCHANGE as i32) << 16);
    assert_eq!(unsafe { IsWindowEnabled(restore) }, 0);

    step("edit and save");
    set_text(dialog, ID_HOTKEY, "Ctrl+Shift+Space");
    check(dialog, ID_SOUNDS, false);
    check(dialog, ID_COMMANDS, true);
    set_text(dialog, ID_VOCABULARY, "Vercel\r\nClaude Code\r\n");
    click(dialog, IDOK);
    assert_eq!(window::take_saved(), Some(true), "saved and asks for a restart");
    assert!(window::window().is_none(), "window closed");

    let text = config.text();
    assert!(text.starts_with("# Mine\nhotkey = \"ctrl+shift+space\"\nthreads = 2\n"), "{text}");
    let saved = Config::parse(&text).unwrap();
    assert!(!saved.sounds && saved.voice_commands && saved.remove_fillers);
    assert_eq!(saved.insert_method, InsertMethod::Type);
    assert_eq!(saved.vocabulary, ["Vercel", "Claude Code"]);
    assert_eq!(saved.replacements.get("get hub").map(String::as_str), Some("GitHub"));
    assert_eq!(saved.threads, 2);
}

#[test]
fn settings_window_rejects_an_invalid_hotkey() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    let _watchdog = Watchdog::new("settings_window_rejects");
    let config = TempConfig::new("settings-reject", SETTINGS_FILE);
    window::open(&config.path());
    let dialog = window::window().expect("settings window opens");
    set_text(dialog, ID_HOTKEY, "ctrl+banana");

    click(dialog, IDOK);
    let warnings = WARNINGS.with(|w| w.take());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("“ctrl+banana” is not a valid hotkey"), "{}", warnings[0]);
    assert_eq!(window::take_saved(), None);
    assert_eq!(window::window(), Some(dialog), "still open for a fix");
    assert_eq!(config.text(), SETTINGS_FILE, "file untouched");

    click(dialog, IDCANCEL);
    assert!(window::window().is_none());
    assert_eq!(window::take_saved(), None);
    assert_eq!(config.text(), SETTINGS_FILE);
}

#[test]
fn indicator_shows_without_taking_focus() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, IsWindowVisible, WS_EX_NOACTIVATE, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    let _watchdog = Watchdog::new("indicator");
    let ui = create_ui(false, true).unwrap();
    let indicator = ui.indicator as HWND;
    assert!(!indicator.is_null(), "indicator window created");
    let ex_style = unsafe { GetWindowLongPtrW(indicator, GWL_EXSTYLE) } as u32;
    let wanted = WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST;
    assert_eq!(ex_style & wanted, wanted, "click-through, topmost, never activated");
    let Some(edit) = focused_edit() else {
        eprintln!("SKIPPED: no interactive desktop");
        return;
    };

    step("listening");
    ui.set_status(Status::Recording, "Dictum: listening…".into(), Phase::Listening);
    ui.level(&[0.1; 480]);
    pump_for(Duration::from_millis(300));
    assert_ne!(unsafe { IsWindowVisible(indicator) }, 0, "shown");
    assert_eq!(unsafe { GetForegroundWindow() }, edit, "focus stays in the app being dictated into");

    step("hands-free, then transcribing, then done");
    ui.set_status(Status::Recording, "Dictum: listening…".into(), Phase::HandsFree);
    pump_for(Duration::from_millis(100));
    ui.set_status(Status::Transcribing, "Dictum: transcribing…".into(), Phase::Transcribing);
    pump_for(Duration::from_millis(300));
    assert_ne!(unsafe { IsWindowVisible(indicator) }, 0);
    assert_eq!(unsafe { GetForegroundWindow() }, edit);
    ui.set_status(Status::Ready, "Dictum".into(), Phase::Hidden);
    pump_for(Duration::from_millis(100));
    assert_eq!(unsafe { IsWindowVisible(indicator) }, 0, "hidden when idle");
    unsafe { DestroyWindow(edit) };
}
