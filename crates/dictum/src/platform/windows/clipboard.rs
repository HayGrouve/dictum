//! Clipboard access that leaves no trace: dictated text is excluded from clipboard history and
//! cloud sync, and the user's previous clipboard (all formats) is put back afterwards.
//!
//! For pasting, the text is offered with *delayed rendering*: Windows asks our window for the
//! data (WM_RENDERFORMAT) at the moment the target app reads it. That tells us exactly when the
//! paste has been consumed, so the previous clipboard can be restored right after — instead of
//! guessing a delay and risking a slow app pasting the restored contents.

use std::sync::{Condvar, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardSequenceNumber,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;
/// Formats whose handles are not HGLOBALs (GDI objects, metafiles, owner-drawn); Windows
/// synthesises most of them from formats we do keep (e.g. CF_BITMAP from CF_DIB).
const UNCOPYABLE: &[u32] = &[2, 3, 9, 14, 0x80, 0x82, 0x83, 0x8E];

/// A copy of everything on the clipboard.
pub struct Saved {
    formats: Vec<(u32, Vec<u8>)>,
}

struct Open;

impl Open {
    fn new(owner: HWND) -> Result<Self> {
        // Another app may hold the clipboard for a moment.
        for _ in 0..25 {
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Open);
            }
            sleep(Duration::from_millis(8));
        }
        bail!("the clipboard is locked by another application")
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

pub fn sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

pub fn save(owner: HWND) -> Result<Saved> {
    let _open = Open::new(owner)?;
    let mut formats = Vec::new();
    let mut format = 0;
    loop {
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        if UNCOPYABLE.contains(&format) {
            continue;
        }
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            continue;
        }
        if let Some(bytes) = unsafe { read_global(handle) } {
            formats.push((format, bytes));
        }
    }
    Ok(Saved { formats })
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().chain(Some(0)).flat_map(|u| u.to_le_bytes()).collect()
}

/// Text waiting to be rendered on request, and whether that happened.
struct Deferred {
    text: Option<Vec<u8>>,
    /// Ctrl+V has been sent; renders from now on are the paste itself.
    armed: bool,
    rendered: bool,
    /// Something (e.g. a clipboard manager) read the text before the paste was sent.
    rendered_early: bool,
    /// Clipboard sequence number once our text is fully on the clipboard.
    sequence: u32,
}

impl Deferred {
    const fn new(text: Option<Vec<u8>>) -> Self {
        Self { text, armed: false, rendered: false, rendered_early: false, sequence: 0 }
    }
}

static DEFERRED: Mutex<Deferred> = Mutex::new(Deferred::new(None));
static RENDERED: Condvar = Condvar::new();

/// Offers `text` with delayed rendering (see module docs), hidden from clipboard history.
///
/// No lock may be held across clipboard calls: EmptyClipboard synchronously sends
/// WM_DESTROYCLIPBOARD to the previous owner, which is often our own window.
pub fn offer_text(owner: HWND, text: &str) -> Result<()> {
    let open = Open::new(owner)?;
    unsafe { EmptyClipboard() };
    *DEFERRED.lock().unwrap() = Deferred::new(Some(utf16_bytes(text)));
    unsafe { SetClipboardData(CF_UNICODETEXT, std::ptr::null_mut()) };
    mark_private();
    drop(open);
    let mut deferred = DEFERRED.lock().unwrap();
    if !deferred.rendered_early {
        deferred.sequence = sequence();
    }
    Ok(())
}

/// Call right before sending Ctrl+V.
pub fn arm() {
    DEFERRED.lock().unwrap().armed = true;
}

/// WM_RENDERFORMAT handler (the clipboard is already open by the requesting app).
pub fn render_requested(format: u32) {
    if format != CF_UNICODETEXT {
        return;
    }
    let mut deferred = DEFERRED.lock().unwrap();
    if let Some(bytes) = deferred.text.take() {
        if let Err(e) = set(CF_UNICODETEXT, &bytes) {
            log::error!("failed to render clipboard text: {e:#}");
        }
        if deferred.armed {
            deferred.rendered = true;
        } else {
            deferred.rendered_early = true;
        }
        deferred.sequence = sequence();
        RENDERED.notify_all();
    }
}

/// WM_RENDERALLFORMATS handler: our window is going away while still owning delayed data.
pub fn render_all(owner: HWND) {
    let pending = DEFERRED.lock().unwrap().text.is_some();
    if pending && let Ok(_open) = Open::new(owner) {
        render_requested(CF_UNICODETEXT);
    }
}

/// WM_DESTROYCLIPBOARD handler: someone else replaced the clipboard.
pub fn ownership_lost() {
    DEFERRED.lock().unwrap().text = None;
}

/// Waits until the paste target has read the offered text. Returns whether it did, and the
/// clipboard sequence number to compare against before restoring.
pub fn wait_consumed(timeout: Duration) -> (bool, u32) {
    let deadline = Instant::now() + timeout;
    let mut deferred = DEFERRED.lock().unwrap();
    if deferred.rendered_early {
        // We can't observe the real paste any more; fall back to a generous fixed delay.
        let sequence = deferred.sequence;
        drop(deferred);
        log::debug!("clipboard text was read before pasting (clipboard manager?)");
        sleep(Duration::from_millis(600));
        return (true, sequence);
    }
    while !deferred.rendered {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        deferred = RENDERED.wait_timeout(deferred, deadline - now).unwrap().0;
    }
    (deferred.rendered, deferred.sequence)
}

/// Puts `text` on the clipboard immediately, hidden from clipboard history and cloud clipboard.
/// Returns the clipboard sequence number after the change.
pub fn set_text(owner: HWND, text: &str) -> Result<u32> {
    let bytes = utf16_bytes(text);
    {
        let _open = Open::new(owner)?;
        unsafe { EmptyClipboard() };
        set(CF_UNICODETEXT, &bytes)?;
        mark_private();
    }
    Ok(sequence())
}

pub fn restore(owner: HWND, saved: &Saved) -> Result<()> {
    let _open = Open::new(owner)?;
    unsafe { EmptyClipboard() };
    for (format, bytes) in &saved.formats {
        if let Err(e) = set(*format, bytes) {
            log::debug!("could not restore clipboard format {format}: {e}");
        }
    }
    // The original content is already in the user's history; don't add a duplicate.
    mark_private();
    Ok(())
}

fn mark_private() {
    let zero = 0u32.to_le_bytes();
    for (name, data) in [
        ("ExcludeClipboardContentFromMonitorProcessing", &[][..]),
        ("CanIncludeInClipboardHistory", &zero[..]),
        ("CanUploadToCloudClipboard", &zero[..]),
    ] {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let format = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
        if format != 0 {
            let _ = set(format, if data.is_empty() { &zero[..1] } else { data });
        }
    }
}

fn set(format: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if handle.is_null() {
            bail!("out of memory");
        }
        let ptr = GlobalLock(handle) as *mut u8;
        if ptr.is_null() {
            GlobalFree(handle);
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        GlobalUnlock(handle);
        if SetClipboardData(format, handle as HANDLE).is_null() {
            GlobalFree(handle);
            bail!("SetClipboardData failed for format {format}");
        }
    }
    Ok(())
}

unsafe fn read_global(handle: HANDLE) -> Option<Vec<u8>> {
    unsafe {
        let size = GlobalSize(handle);
        if size == 0 {
            return None;
        }
        let ptr = GlobalLock(handle) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        GlobalUnlock(handle);
        Some(bytes)
    }
}

/// Current clipboard text (tests only).
#[cfg(test)]
pub(super) fn read_text(owner: HWND) -> Option<String> {
    let _open = Open::new(owner).ok()?;
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
    if handle.is_null() {
        return None;
    }
    let bytes = unsafe { read_global(handle) }?;
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}
