//! The on-screen dictation indicator: a borderless, click-through panel that never takes focus,
//! shown at the bottom of the screen the user is working on. Lives on the main thread; other
//! threads send it the phase and feed the level meter.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dispatch2::{DispatchQueue, DispatchTime};
use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSImage, NSImageScaling, NSImageView, NSPanel, NSScreen,
    NSStatusWindowLevel, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::CFData;
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::indicator::{self, FPS, HEIGHT, Meter, Phase, TRANSCRIBING_DELAY, WIDTH};

/// Gap between the indicator and the bottom of the visible area (above the Dock), points.
const MARGIN: f64 = 20.0;

struct Indicator {
    panel: Retained<NSPanel>,
    view: Retained<NSImageView>,
    meter: Arc<Mutex<Meter>>,
    phase: Phase,
    /// What was shown before transcription started; kept briefly so fast results don't flicker.
    previous: Phase,
    phase_since: Instant,
    shown_since: Instant,
    scale: f32,
}

thread_local! {
    static INDICATOR: RefCell<Option<Indicator>> = const { RefCell::new(None) };
}

/// Bumped whenever the indicator is shown, so only the newest frame timer keeps running.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Creates the (hidden) indicator panel.
pub fn create(mtm: MainThreadMarker, meter: Arc<Mutex<Meter>>) -> Result<()> {
    let size = NSSize::new(f64::from(WIDTH), f64::from(HEIGHT));
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        mtm.alloc(),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setIgnoresMouseEvents(true);
    panel.setHidesOnDeactivate(false);
    panel.setLevel(NSStatusWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    unsafe { panel.setReleasedWhenClosed(false) };
    let view = NSImageView::initWithFrame(mtm.alloc(), frame);
    view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
    panel.setContentView(Some(&view));
    INDICATOR.with(|i| {
        *i.borrow_mut() = Some(Indicator {
            panel,
            view,
            meter,
            phase: Phase::Hidden,
            previous: Phase::Listening,
            phase_since: Instant::now(),
            shown_since: Instant::now(),
            scale: 2.0,
        })
    });
    Ok(())
}

pub fn set_phase(phase: Phase) {
    let Some(was) = INDICATOR.with(|i| {
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
            ind.scale = place(&ind.panel);
        }
        Some(was)
    }) else {
        return;
    };
    if phase == Phase::Hidden {
        GENERATION.fetch_add(1, Ordering::SeqCst);
        INDICATOR.with(|i| {
            if let Some(ind) = i.borrow().as_ref() {
                ind.panel.orderOut(None);
            }
        });
    } else if was == Phase::Hidden {
        draw();
        INDICATOR.with(|i| {
            if let Some(ind) = i.borrow().as_ref() {
                ind.panel.orderFrontRegardless();
            }
        });
        schedule(GENERATION.fetch_add(1, Ordering::SeqCst) + 1);
    }
}

/// Moves the panel to the bottom centre of the screen the user is working on; returns that
/// screen's scale.
fn place(panel: &NSPanel) -> f32 {
    let Some(mtm) = MainThreadMarker::new() else { return 2.0 };
    let Some(screen) = NSScreen::mainScreen(mtm) else { return 2.0 };
    let area = screen.visibleFrame();
    let x = area.origin.x + (area.size.width - f64::from(WIDTH)) / 2.0;
    let y = area.origin.y + MARGIN;
    panel.setFrameOrigin(NSPoint::new(x.round(), y.round()));
    screen.backingScaleFactor() as f32
}

/// Draws the next frame every 1/FPS s while this generation is the visible one.
fn schedule(generation: u64) {
    let Ok(when) = DispatchTime::try_from(Duration::from_millis(1000 / u64::from(FPS))) else { return };
    let _ = DispatchQueue::main().after(when, move || {
        if GENERATION.load(Ordering::SeqCst) == generation {
            draw();
            schedule(generation);
        }
    });
}

fn draw() {
    INDICATOR.with(|i| {
        let i = i.borrow();
        let Some(ind) = i.as_ref() else { return };
        if ind.phase == Phase::Hidden {
            return;
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
        let (width, height, pixels) = indicator::render(phase, &bars, ind.shown_since.elapsed(), ind.scale);
        match image(width, height, &pixels) {
            Ok(image) => ind.view.setImage(Some(&image)),
            Err(e) => log::warn!("{e:#}"),
        }
    });
}

/// Premultiplied BGRA pixels, top-down rows, as an image of the indicator's size in points.
fn image(width: u32, height: u32, pixels: &[u8]) -> Result<Retained<NSImage>> {
    let data = CFData::from_bytes(pixels);
    let provider =
        CGDataProvider::with_cf_data(Some(&data)).context("failed to wrap the indicator pixels")?;
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    let info = CGBitmapInfo(CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::PremultipliedFirst.0);
    let image = unsafe {
        CGImage::new(
            width as usize,
            height as usize,
            8,
            32,
            width as usize * 4,
            space.as_deref(),
            info,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .context("failed to create the indicator image")?;
    let size = NSSize::new(f64::from(WIDTH), f64::from(HEIGHT));
    Ok(NSImage::initWithCGImage_size(NSImage::alloc(), &image, size))
}
