//! Clipboard access that leaves no trace: dictated text is excluded from clipboard history and
//! cloud sync, and the user's previous clipboard (all formats) is put back afterwards.

use std::thread::sleep;
use std::time::Duration;

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

/// Puts `text` on the clipboard, hidden from clipboard history and cloud clipboard.
/// Returns the clipboard sequence number after the change.
pub fn set_text(owner: HWND, text: &str) -> Result<u32> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let bytes: Vec<u8> = utf16.iter().flat_map(|u| u.to_le_bytes()).collect();
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
