//! The on-screen dictation indicator: a layered, click-through window that never takes focus,
//! shown at the bottom of the screen the user is working on. Lives on the main thread; other
//! threads post the phase to it and feed the level meter.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST,
    MONITORINFO, MonitorFromWindow, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetForegroundWindow, HTTRANSPARENT, KillTimer, MA_NOACTIVATE,
    RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SetTimer, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_APP,
    WM_MOUSEACTIVATE, WM_NCHITTEST, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use super::wide;
use crate::indicator::{self, FPS, Meter, Phase, TRANSCRIBING_DELAY};

/// Posted with the new phase in `wparam`.
pub const WM_PHASE: u32 = WM_APP + 20;
const TIMER: usize = 1;
/// Gap between the indicator and the bottom of the work area (above the taskbar), logical px.
const MARGIN: f32 = 20.0;

struct Indicator {
    hwnd: HWND,
    meter: Arc<Mutex<Meter>>,
    phase: Phase,
    /// What was shown before transcription started; kept briefly so fast results don't flicker.
    previous: Phase,
    phase_since: Instant,
    shown_since: Instant,
    /// Bottom-centre anchor on the chosen monitor (physical px) and its scale.
    anchor: (i32, i32),
    scale: f32,
}

thread_local! {
    static INDICATOR: RefCell<Option<Indicator>> = const { RefCell::new(None) };
}

/// Creates the (hidden) indicator window on the calling thread, which must pump messages.
pub fn create(meter: Arc<Mutex<Meter>>) -> Result<HWND> {
    let class = wide("DictumIndicator");
    let hwnd = unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc); // fails harmlessly if already registered
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            wide("Dictum").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        bail!("failed to create the indicator window");
    }
    INDICATOR.with(|i| {
        *i.borrow_mut() = Some(Indicator {
            hwnd,
            meter,
            phase: Phase::Hidden,
            previous: Phase::Listening,
            phase_since: Instant::now(),
            shown_since: Instant::now(),
            anchor: (0, 0),
            scale: 1.0,
        })
    });
    Ok(hwnd)
}

pub fn phase_to_wparam(phase: Phase) -> WPARAM {
    match phase {
        Phase::Hidden => 0,
        Phase::Listening => 1,
        Phase::HandsFree => 2,
        Phase::Transcribing => 3,
    }
}

fn phase_from_wparam(wparam: WPARAM) -> Phase {
    match wparam {
        1 => Phase::Listening,
        2 => Phase::HandsFree,
        3 => Phase::Transcribing,
        _ => Phase::Hidden,
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PHASE => {
            set_phase(phase_from_wparam(wparam));
            0
        }
        WM_TIMER => {
            draw();
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn set_phase(phase: Phase) {
    let Some((hwnd, was)) = INDICATOR.with(|i| {
        let mut i = i.borrow_mut();
        let ind = i.as_mut()?;
        let was = ind.phase;
        if phase == was {
            return None;
        }
        if was != Phase::Transcribing && was != Phase::Hidden {
            ind.previous = was;
        }
        ind.phase = phase;
        ind.phase_since = Instant::now();
        if was == Phase::Hidden {
            ind.shown_since = Instant::now();
            ind.meter.lock().unwrap().reset();
            (ind.anchor, ind.scale) = placement();
        }
        Some((ind.hwnd, was))
    }) else {
        return;
    };
    if phase == Phase::Hidden {
        unsafe {
            KillTimer(hwnd, TIMER);
            ShowWindow(hwnd, SW_HIDE);
        }
    } else if was == Phase::Hidden {
        draw();
        unsafe {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(hwnd, TIMER, 1000 / FPS, None);
        }
    }
}

/// The monitor the user is working on (where the foreground window is): bottom-centre of its
/// work area, and its display scale.
fn placement() -> ((i32, i32), f32) {
    unsafe {
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96, 96);
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x.max(96) as f32 / 96.0;
        let work = info.rcWork;
        let x = (work.left + work.right) / 2;
        let y = work.bottom - (MARGIN * scale).round() as i32;
        ((x, y), scale)
    }
}

fn draw() {
    let Some((hwnd, frame, anchor)) = INDICATOR.with(|i| {
        let i = i.borrow();
        let ind = i.as_ref()?;
        if ind.phase == Phase::Hidden {
            return None;
        }
        let phase = if ind.phase == Phase::Transcribing && ind.phase_since.elapsed() < TRANSCRIBING_DELAY {
            ind.previous
        } else {
            ind.phase
        };
        let bars = {
            let mut meter = ind.meter.lock().unwrap();
            meter.tick();
            meter.bars()
        };
        Some((ind.hwnd, indicator::render(phase, &bars, ind.shown_since.elapsed(), ind.scale), ind.anchor))
    }) else {
        return;
    };
    let (width, height, pixels) = frame;
    let origin = POINT { x: anchor.0 - width as i32 / 2, y: anchor.1 - height as i32 };
    if let Err(e) = present(hwnd, origin, width, height, &pixels) {
        log::warn!("{e:#}");
    }
}

/// Puts premultiplied BGRA pixels on the layered window.
fn present(hwnd: HWND, origin: POINT, width: u32, height: u32, pixels: &[u8]) -> Result<()> {
    unsafe {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            DeleteDC(dc);
            bail!("failed to create the indicator bitmap");
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
        let old = SelectObject(dc, bitmap);
        let size = SIZE { cx: width as i32, cy: height as i32 };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let ok = UpdateLayeredWindow(
            hwnd,
            std::ptr::null_mut(),
            &origin,
            &size,
            dc,
            &source,
            0,
            &blend,
            ULW_ALPHA,
        );
        SelectObject(dc, old);
        DeleteObject(bitmap);
        DeleteDC(dc);
        if ok == 0 {
            bail!("failed to draw the indicator");
        }
    }
    Ok(())
}
