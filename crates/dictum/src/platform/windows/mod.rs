//! Windows integration: hidden message window + tray icon on the main thread, a low-level
//! keyboard hook for the hotkey, clipboard/SendInput text insertion and PlaySound cues.

mod autostart;
mod clipboard;
mod hook;
mod input;
mod keys;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossbeam_channel::Sender;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, SW_SHOWNORMAL, TranslateMessage, WM_APP, WM_DESTROYCLIPBOARD,
    WM_RENDERALLFORMATS, WM_RENDERFORMAT, WNDCLASSW,
};

pub use input::wait_for_modifiers_released;
pub use keys::hotkey_held;

use crate::app::Command;
use crate::config::{Bindings, InsertMethod};
use crate::hotkey::Machine;
use crate::paths::Paths;
use crate::ui::{Cue, Status};

const WM_STATUS: u32 = WM_APP + 1;
const WM_RESET_HOTKEY: u32 = WM_APP + 2;

/// Handle to the UI owned by the main thread; cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct Ui {
    hwnd: usize,
    shared: Arc<Shared>,
}

struct Shared {
    sounds: bool,
    status: Mutex<(Status, String)>,
}

impl Ui {
    fn hwnd(&self) -> HWND {
        self.hwnd as HWND
    }

    pub fn set_status(&self, status: Status, tooltip: String) {
        *self.shared.status.lock().unwrap() = (status, tooltip);
        unsafe { PostMessageW(self.hwnd(), WM_STATUS, 0, 0) };
    }

    pub fn cue(&self, cue: Cue) {
        if self.shared.sounds {
            play(cue);
        }
    }

    /// Tells the hotkey state machine that the app ended the dictation by itself.
    pub fn reset_hotkey(&self) {
        unsafe { PostMessageW(self.hwnd(), WM_RESET_HOTKEY, 0, 0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn play(cue: Cue) {
    static WAVS: OnceLock<Vec<(Cue, Vec<u8>)>> = OnceLock::new();
    let wavs = WAVS.get_or_init(|| {
        [Cue::Start, Cue::Stop, Cue::Lock, Cue::Cancel, Cue::Error]
            .into_iter()
            .map(|c| (c, crate::sounds::wav(c)))
            .collect()
    });
    if let Some((_, wav)) = wavs.iter().find(|(c, _)| *c == cue) {
        // The buffers live for the whole process, as SND_MEMORY|SND_ASYNC requires.
        unsafe {
            PlaySoundW(wav.as_ptr().cast(), std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT)
        };
    }
}

/// Keeps other instances out while alive.
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub fn single_instance() -> Option<InstanceGuard> {
    let name = wide("Local\\Dictum.SingleInstance");
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return None;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return None;
    }
    Some(InstanceGuard(handle))
}

/// Creates the hidden window that receives status updates and owns the clipboard.
/// Must be called on the thread that later calls [`run`].
pub fn create_ui(sounds: bool) -> Result<Ui> {
    let hwnd = create_message_window()?;
    Ok(Ui {
        hwnd: hwnd as usize,
        shared: Arc::new(Shared { sounds, status: Mutex::new((Status::Loading, "Dictum".into())) }),
    })
}

fn create_message_window() -> Result<HWND> {
    let class = wide("DictumMessageWindow");
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc); // fails harmlessly if already registered
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Dictum").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            bail!("failed to create the message window");
        }
        Ok(hwnd)
    }
}

/// Main-thread-only state.
struct MainState {
    ui: Ui,
    tray: Option<TrayIcon>,
    status_item: MenuItem,
    icons: Vec<(Status, Icon)>,
}

thread_local! {
    static MAIN: std::cell::RefCell<Option<MainState>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_STATUS => {
            MAIN.with(|m| {
                if let Some(main) = m.borrow().as_ref() {
                    let (status, text) = main.ui.shared.status.lock().unwrap().clone();
                    if let Some(tray) = &main.tray {
                        if let Some((_, icon)) = main.icons.iter().find(|(s, _)| *s == status) {
                            let _ = tray.set_icon(Some(icon.clone()));
                        }
                        let _ = tray.set_tooltip(Some(&text));
                    }
                    main.status_item.set_text(text.trim_start_matches("Dictum: "));
                }
            });
            0
        }
        WM_RESET_HOTKEY => {
            hook::reset();
            0
        }
        WM_RENDERFORMAT => {
            clipboard::render_requested(wparam as u32);
            0
        }
        WM_RENDERALLFORMATS => {
            clipboard::render_all(hwnd);
            0
        }
        WM_DESTROYCLIPBOARD => {
            clipboard::ownership_lost();
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Opens a file with its default app. Runs off the main thread: the keyboard hook lives there and
/// Windows drops hooks that stop responding, while ShellExecute can block for a while.
fn open(path: &Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || open_blocking(&path));
}

fn open_blocking(path: &Path) {
    let file = wide(&path.display().to_string());
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        // No app is associated with the file type: fall back to Notepad.
        let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
    }
}

/// Starts a fresh copy of Dictum (call after releasing the single-instance guard).
pub fn relaunch() {
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn()) {
        Ok(_) => {}
        Err(e) => log::error!("restart failed: {e}"),
    }
}

/// Runs the main-thread event loop until the user quits. Returns `true` to restart.
pub fn run(ui: &Ui, bindings: Bindings, commands: Sender<Command>, paths: &Paths) -> Result<bool> {
    let machine = Machine::new(bindings.hotkey, bindings.hands_free, bindings.cancel);
    let _hook = hook::install(machine, commands.clone())?;

    let status_item = MenuItem::new("Starting…", false, None);
    let settings_item = MenuItem::new("Settings…", true, None);
    let log_item = MenuItem::new("Open log", true, None);
    let autostart_item = CheckMenuItem::new("Start with Windows", true, autostart::is_enabled(), None);
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
    .context("failed to build the tray menu")?;

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
        .context("failed to create the tray icon")?;

    MAIN.with(|m| {
        *m.borrow_mut() = Some(MainState { ui: ui.clone(), tray: Some(tray), status_item, icons });
    });
    unsafe { PostMessageW(ui.hwnd(), WM_STATUS, 0, 0) };

    let mut restart = false;
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got == 0 || got == -1 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        while TrayIconEvent::receiver().try_recv().is_ok() {}
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id();
            if id == settings_item.id() {
                open(&paths.config_file);
            } else if id == log_item.id() {
                open(&paths.log_file);
            } else if id == autostart_item.id() {
                // Registry writes are quick; the checkbox already shows the new state.
                let enable = autostart_item.is_checked();
                if let Err(e) = autostart::set(enable) {
                    log::error!("{e:#}");
                    autostart_item.set_checked(!enable);
                }
            } else if id == restart_item.id() {
                restart = true;
                let _ = commands.send(Command::Quit);
                unsafe { PostQuitMessage(0) };
            } else if id == quit_item.id() {
                let _ = commands.send(Command::Quit);
                unsafe { PostQuitMessage(0) };
            }
        }
    }
    // Removes the tray icon before the process exits.
    MAIN.with(|m| m.borrow_mut().take());
    Ok(restart)
}

pub fn insert_text(ui: &Ui, text: &str, method: InsertMethod, restore_clipboard: bool) -> Result<()> {
    match method {
        InsertMethod::Type => input::type_text(text),
        InsertMethod::Paste => {
            let owner = ui.hwnd();
            if !restore_clipboard {
                clipboard::set_text(owner, text)?;
                return input::paste();
            }
            let saved = match clipboard::save(owner) {
                Ok(saved) => saved,
                Err(e) => {
                    log::warn!("could not save the clipboard, it won't be restored: {e:#}");
                    clipboard::set_text(owner, text)?;
                    return input::paste();
                }
            };
            clipboard::offer_text(owner, text)?;
            clipboard::arm();
            input::paste()?;
            // Restore as soon as the target app has read the text (or give up waiting).
            let (consumed, sequence) = clipboard::wait_consumed(Duration::from_millis(1500));
            if !consumed {
                log::warn!("the focused app did not read the pasted text");
            }
            if clipboard::sequence() == sequence {
                clipboard::restore(owner, &saved)?;
            } else {
                log::debug!("clipboard changed after paste; not restoring");
            }
            Ok(())
        }
    }
}
