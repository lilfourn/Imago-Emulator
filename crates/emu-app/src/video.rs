//! Turns core frames into GPUI images.
//!
//! GPUI samples images with linear filtering and offers no nearest-neighbour
//! option, which blurs pixel art. Enlarging each frame by an integer factor
//! on the CPU first, then letting GPUI's linear filter cover the remaining
//! fractional scale, gives the "sharp bilinear" look: crisp pixels with
//! smooth, even edges at any window size.

use std::sync::Arc;

use emu_core::Frame;
use gpui::RenderImage;

/// Largest integer pre-scale. Beyond this the remaining linear blur is
/// invisible and the upload cost keeps growing.
const MAX_FACTOR: usize = 6;

/// The integer factor to enlarge a `source_width`-pixel frame by before
/// drawing it `drawn_width` logical pixels wide on a display with
/// `scale_factor` device pixels per logical pixel.
pub fn prescale_factor(source_width: u16, drawn_width: f32, scale_factor: f32) -> usize {
    let device_per_source = drawn_width * scale_factor / f32::from(source_width.max(1));
    // Saturating float-to-int conversion; the result is clamped anyway.
    (device_per_source.floor() as usize).clamp(1, MAX_FACTOR)
}

/// `frame` as BGRA bytes, enlarged `factor` times with nearest-neighbour
/// sampling.
pub fn upscale_bgra(frame: &Frame, factor: usize) -> Vec<u8> {
    let size = frame.size();
    let row_bytes = usize::from(size.width) * factor * 4;
    let mut out = Vec::with_capacity(row_bytes * usize::from(size.height) * factor);
    for row in frame.rows() {
        let start = out.len();
        for pixel in row {
            let bgra = pixel.to_le_bytes();
            for _ in 0..factor {
                out.extend_from_slice(&bgra);
            }
        }
        for _ in 1..factor {
            out.extend_from_within(start..start + row_bytes);
        }
    }
    out
}

/// A GPUI image of `frame` enlarged `factor` times.
pub fn render_image(frame: &Frame, factor: usize) -> Arc<RenderImage> {
    let size = frame.size();
    let scaled = |length: u16| {
        u32::try_from(usize::from(length) * factor).expect("factor is at most MAX_FACTOR")
    };
    let buffer = image::RgbaImage::from_raw(
        scaled(size.width),
        scaled(size.height),
        upscale_bgra(frame, factor),
    )
    .expect("upscale_bgra returns exactly width * height * 4 bytes");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::{ScreenSize, rgb};

    #[test]
    fn upscale_repeats_pixels_and_rows() {
        let mut frame = Frame::new(ScreenSize::new(2, 1));
        frame
            .pixels_mut()
            .copy_from_slice(&[rgb(1, 2, 3), rgb(4, 5, 6)]);
        let bytes = upscale_bgra(&frame, 2);
        let row = [3, 2, 1, 255, 3, 2, 1, 255, 6, 5, 4, 255, 6, 5, 4, 255];
        assert_eq!(bytes, [row, row].concat());
    }

    #[test]
    fn prescale_factor_floors_device_pixels_per_source_pixel() {
        assert_eq!(prescale_factor(256, 512.0, 2.0), 4);
        assert_eq!(prescale_factor(256, 700.0, 1.0), 2);
        assert_eq!(prescale_factor(256, 100.0, 1.0), 1);
        assert_eq!(prescale_factor(160, 4000.0, 2.0), MAX_FACTOR);
    }
}
