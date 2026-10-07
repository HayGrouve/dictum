//! Tray icons drawn at runtime: a white microphone on a coloured disc, one colour per state.

use crate::ui::Status;

pub const SIZE: u32 = 32;

fn color(status: Status) -> [u8; 3] {
    match status {
        Status::Ready => [0x4b, 0x55, 0x63],
        Status::Recording => [0xe5, 0x48, 0x4d],
        Status::Transcribing => [0xf5, 0x9e, 0x0b],
        Status::Loading => [0x3e, 0x63, 0xdd],
        Status::Error => [0x7c, 0x3a, 0xed],
    }
}

/// RGBA pixels, `SIZE` x `SIZE`.
pub fn rgba(status: Status) -> Vec<u8> {
    const SS: u32 = 4; // supersampling per axis
    let [r, g, b] = color(status);
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (mut disc, mut glyph) = (0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    if in_disc(px, py) {
                        disc += 1;
                        if in_mic(px, py) {
                            glyph += 1;
                        }
                    }
                }
            }
            let n = (SS * SS) as f32;
            let alpha = disc as f32 / n;
            let white = if disc > 0 { glyph as f32 / disc as f32 } else { 0.0 };
            let mix = |c: u8| (c as f32 * (1.0 - white) + 255.0 * white).round() as u8;
            out.extend_from_slice(&[mix(r), mix(g), mix(b), (alpha * 255.0).round() as u8]);
        }
    }
    out
}

fn in_disc(x: f32, y: f32) -> bool {
    let (cx, cy, r) = (16.0, 16.0, 15.5);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn in_mic(x: f32, y: f32) -> bool {
    // Capsule body.
    let body = {
        let (cx, top, bottom, r) = (16.0, 8.5, 15.5, 3.5);
        let cy = y.clamp(top, bottom);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    // U-shaped holder: lower half of a ring.
    let holder = {
        let (cx, cy) = (16.0, 15.0);
        let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        y >= cy && (5.4..=7.0).contains(&d)
    };
    let stem = (15.2..=16.8).contains(&x) && (21.5..=24.5).contains(&y);
    let base = (12.5..=19.5).contains(&x) && (23.5..=25.0).contains(&y);
    body || holder || stem || base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_have_expected_shape() {
        for status in [Status::Ready, Status::Recording, Status::Transcribing, Status::Loading, Status::Error]
        {
            let px = rgba(status);
            assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
            assert_eq!(px[3], 0, "corners are transparent");
            let center = ((12 * SIZE + 16) * 4) as usize;
            assert_eq!(&px[center..center + 4], &[255, 255, 255, 255], "mic body is white");
        }
        assert_ne!(rgba(Status::Ready), rgba(Status::Recording));
    }
}
