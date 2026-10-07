//! Tray → "Check for updates": asks GitHub, confirms with the user, installs and restarts.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNO, MESSAGEBOX_STYLE, MessageBoxW,
};

use super::{Ui, wide};
use crate::update::{self, Version, text};

/// Runs the whole check on a background thread; a second click while one runs is ignored.
pub fn check(ui: &Ui) {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let ui = ui.clone();
    std::thread::spawn(move || {
        run(&ui);
        RUNNING.store(false, Ordering::SeqCst);
    });
}

fn run(ui: &Ui) {
    let current = Version::current();
    let release = match update::check(current) {
        Ok(Some(release)) => release,
        Ok(None) => {
            message(&text::up_to_date(current), MB_OK | MB_ICONINFORMATION);
            return;
        }
        Err(e) => {
            log::warn!("update check failed: {e:#}");
            message(&text::check_failed(&e), MB_OK | MB_ICONWARNING);
            return;
        }
    };
    log::info!("update available: {} (running {current})", release.version);
    if message(&text::offer(&release, current), MB_YESNO | MB_ICONQUESTION) != IDYES {
        return;
    }
    let Some(dir) = update::install_target() else {
        message(text::NO_TARGET, MB_OK | MB_ICONWARNING);
        return;
    };
    match update::install(&release, &dir) {
        Ok(()) => {
            log::info!("installed {} into {}; restarting", release.version, dir.display());
            ui.request_restart();
        }
        Err(e) => {
            log::error!("update failed: {e:#}");
            message(&text::failed(&e), MB_OK | MB_ICONWARNING);
        }
    }
}

fn message(text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    let (text, title) = (wide(text), wide("Dictum"));
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            style | MB_SETFOREGROUND | MB_TOPMOST,
        )
    }
}
