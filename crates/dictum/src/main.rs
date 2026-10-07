//! Dictum: hold a hotkey, speak, release — the text appears wherever you are typing.
//! Recognition runs locally (NVIDIA Parakeet TDT on ONNX Runtime); nothing leaves the machine and
//! nothing is stored.

#![cfg_attr(all(windows, not(debug_assertions), not(test)), windows_subsystem = "windows")]
// The platform-independent core is built (and unit-tested) everywhere, but only wired up on
// supported platforms.
#![cfg_attr(not(windows), allow(dead_code))]

mod config;
mod hotkey;
mod icons;
mod pipeline;
mod sounds;
mod ui;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod logging;
#[cfg(windows)]
mod paths;
#[cfg(windows)]
mod platform;

#[cfg(windows)]
fn main() {
    if let Err(e) = app::run() {
        log::error!("fatal: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Dictum currently runs on Windows; macOS support is planned (see docs/ARCHITECTURE.md).");
    std::process::exit(1);
}
