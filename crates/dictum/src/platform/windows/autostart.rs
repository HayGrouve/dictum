//! "Start with Windows" via the per-user Run key.

use anyhow::{Result, bail};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "Dictum";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn command() -> Result<String> {
    Ok(format!("\"{}\"", std::env::current_exe()?.display()))
}

pub fn is_enabled() -> bool {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(VALUE).as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return false;
    }
    let len = (size as usize / 2).saturating_sub(1);
    let current = String::from_utf16_lossy(&buf[..len]);
    command().is_ok_and(|c| c.eq_ignore_ascii_case(&current))
}

pub fn set(enabled: bool) -> Result<()> {
    let status = unsafe {
        if enabled {
            let value = wide(&command()?);
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(VALUE).as_ptr(),
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide(VALUE).as_ptr())
        }
    };
    if status != 0 && !(status == 2 && !enabled) {
        bail!("registry update failed (error {status})");
    }
    Ok(())
}
