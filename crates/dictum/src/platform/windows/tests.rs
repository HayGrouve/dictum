//! Integration tests against the real Windows desktop: clipboard, text insertion into a live
//! edit control, and the global keyboard hook. GitHub's Windows runners provide an interactive
//! session; anywhere without one these tests report "SKIPPED" and pass.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, SetFocus, VK_MENU, VK_RCONTROL,
    VK_SPACE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetWindowTextW, MSG, PM_REMOVE,
    PeekMessageW, SW_SHOW, SetForegroundWindow, ShowWindow, TranslateMessage, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE,
};

use super::*;
use crate::hotkey::{Action, Hotkey, Key};

/// The clipboard, focus and keyboard are global: run these one at a time.
static DESKTOP: Mutex<()> = Mutex::new(());

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
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
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
        // the foreground.
        press(&[key_event(VK_MENU, false), key_event(VK_MENU, true)]);
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
    let ui = create_ui(false).unwrap();
    let edit = focused_edit()?;
    let worker = {
        let ui = ui.clone();
        let text = text.to_string();
        std::thread::spawn(move || insert_text(&ui, &text, method, true))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !worker.is_finished() && Instant::now() < deadline {
        pump_for(Duration::from_millis(20));
    }
    worker.join().unwrap().unwrap();
    pump_for(Duration::from_millis(200));
    let result = window_text(edit);
    unsafe { DestroyWindow(edit) };
    Some(result)
}

#[test]
fn clipboard_round_trip_preserves_previous_contents() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
    let owner = create_message_window().unwrap();
    if clipboard::set_text(owner, "original clipboard ✓").is_err() {
        eprintln!("SKIPPED: no clipboard");
        return;
    }
    let saved = clipboard::save(owner).unwrap();
    let sequence = clipboard::set_text(owner, "dictated text").unwrap();
    assert_eq!(clipboard::read_text(owner).as_deref(), Some("dictated text"));
    assert_eq!(clipboard::sequence(), sequence);
    clipboard::restore(owner, &saved).unwrap();
    assert_eq!(clipboard::read_text(owner).as_deref(), Some("original clipboard ✓"));
}

#[test]
fn paste_inserts_text_and_restores_clipboard() {
    let _desktop = DESKTOP.lock().unwrap_or_else(|e| e.into_inner());
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
