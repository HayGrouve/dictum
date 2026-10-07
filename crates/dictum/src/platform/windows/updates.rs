//! Tray → "Check for updates": asks GitHub, confirms with the user, installs and restarts.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNO, MESSAGEBOX_STYLE, MessageBoxW,
};

use super::{Ui, wide};
use crate::update::{self, Version};

/// Longest excerpt of the release notes shown in the confirmation dialog.
const MAX_NOTES: usize = 700;

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
            message(&format!("Dictum {current} is the latest version."), MB_OK | MB_ICONINFORMATION);
            return;
        }
        Err(e) => {
            log::warn!("update check failed: {e:#}");
            let detail = if format!("{e:#}").contains("404") {
                "No release has been published yet.".to_string()
            } else {
                format!("{e:#}")
            };
            message(&format!("Couldn't check for updates.\n\n{detail}"), MB_OK | MB_ICONWARNING);
            return;
        }
    };
    log::info!("update available: {} (running {current})", release.version);
    let mut notes: String = release.notes.chars().take(MAX_NOTES).collect();
    if notes.len() < release.notes.len() {
        notes.push('…');
    }
    let prompt = format!(
        "Dictum {} is available (you have {current}).\n\n{notes}\n\nDownload it and restart Dictum now? \
         Your settings, vocabulary and speech model are kept.",
        release.version
    );
    if message(&prompt, MB_YESNO | MB_ICONQUESTION) != IDYES {
        return;
    }
    let dir = match std::env::current_exe() {
        Ok(exe) => exe.parent().map(|d| d.to_path_buf()),
        Err(_) => None,
    };
    let Some(dir) = dir else {
        message("Couldn't find where Dictum is installed.", MB_OK | MB_ICONWARNING);
        return;
    };
    match update::install(&release, &dir) {
        Ok(()) => {
            log::info!("installed {} into {}; restarting", release.version, dir.display());
            ui.request_restart();
        }
        Err(e) => {
            log::error!("update failed: {e:#}");
            message(&format!("The update failed; Dictum was not changed.\n\n{e:#}"), MB_OK | MB_ICONWARNING);
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
