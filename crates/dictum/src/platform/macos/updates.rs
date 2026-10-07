//! Menu → "Check for updates": asks GitHub, confirms with the user, installs and restarts.

use std::sync::atomic::{AtomicBool, Ordering};

use objc2::MainThreadMarker;
use objc2_app_kit::NSAlertStyle;

use super::Ui;
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
            message(&text::up_to_date(current), NSAlertStyle::Informational, &[]);
            return;
        }
        Err(e) => {
            log::warn!("update check failed: {e:#}");
            message(&text::check_failed(&e), NSAlertStyle::Warning, &[]);
            return;
        }
    };
    log::info!("update available: {} (running {current})", release.version);
    if !message(&text::offer(&release, current), NSAlertStyle::Informational, &["Update", "Not Now"]) {
        return;
    }
    let Some(bundle) = update::install_target() else {
        message(text::NO_TARGET, NSAlertStyle::Warning, &[]);
        return;
    };
    match update::install(&release, &bundle) {
        Ok(()) => {
            log::info!("installed {} into {}; restarting", release.version, bundle.display());
            ui.request_restart();
        }
        Err(e) => {
            log::error!("update failed: {e:#}");
            message(&text::failed(&e), NSAlertStyle::Warning, &[]);
        }
    }
}

/// Shows `text` on the main thread; returns whether the first button was chosen.
fn message(text: &str, style: NSAlertStyle, buttons: &[&str]) -> bool {
    let text = text.to_string();
    let buttons: Vec<String> = buttons.iter().map(|b| b.to_string()).collect();
    super::on_main(move || {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let buttons: Vec<&str> = buttons.iter().map(String::as_str).collect();
        super::alert(mtm, &text, style, &buttons)
    })
}
