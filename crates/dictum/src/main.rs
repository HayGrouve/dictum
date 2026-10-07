//! Dictum: hold a hotkey, speak, release — the text appears wherever you are typing.
//! Recognition runs locally (NVIDIA Parakeet TDT on ONNX Runtime); nothing leaves the machine and
//! nothing is stored.

#![cfg_attr(all(windows, not(debug_assertions), not(test)), windows_subsystem = "windows")]
// The platform-independent core is built (and unit-tested) everywhere, but only wired up on
// supported platforms; the settings window and updater are Windows-only so far.
#![cfg_attr(not(windows), allow(dead_code))]

mod config;
mod hotkey;
mod icons;
mod indicator;
mod pipeline;
mod settings;
mod sounds;
mod ui;
mod update;

#[cfg(any(windows, target_os = "macos"))]
mod app;
#[cfg(any(windows, target_os = "macos"))]
mod audio;
#[cfg(any(windows, target_os = "macos"))]
mod logging;
#[cfg(any(windows, target_os = "macos"))]
mod paths;
#[cfg(any(windows, target_os = "macos"))]
mod platform;

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    if let Err(e) = app::run() {
        log::error!("fatal: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("Dictum runs on Windows and macOS.");
    std::process::exit(1);
}
