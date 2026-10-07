//! OS integration: keyboard hook, text insertion, tray icon, sounds.
//!
//! Every platform module exposes the same free functions and `Ui` handle; `app.rs` only talks to
//! this interface.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;
