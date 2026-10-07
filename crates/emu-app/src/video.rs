//! Turns core frames into GPUI images.
//!
//! GPUI samples images with linear filtering and offers no nearest-neighbour
//! option, which blurs pixel art. Enlarging each frame by an integer factor
//! on the CPU first, then letting GPUI's linear filter cover the remaining
//! fractional scale, gives the "sharp bilinear" look: crisp pixels with
//! smooth, even edges at any window size.

use std::borrow::Cow;
use std::sync::Arc;

use emu_core::{Frame, ScreenSize};
use gpui::RenderImage;

use crate::layout::Rotation;

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

/// `frame` turned by `rotation`, for screens drawn on their side.
pub fn turn(frame: &Frame, rotation: Rotation) -> Frame {
    let size = frame.size();
    let width = usize::from(size.width);
    let source = frame.pixels();
    let mut turned = Frame::new(rotation.turn_size(size));
    for (row, line) in (0u16..).zip(turned.rows_mut()) {
        for (column, pixel) in (0u16..).zip(line) {
            let (x, y) = rotation.source_pixel(size, column, row);
            *pixel = source[usize::from(y) * width + usize::from(x)];
        }
    }
    turned
}

/// `frame` at `size`, a whole fraction of its own: every pixel of a
/// frame drawn at a higher 3D resolution keeps the middle of its square.
/// Frames already at `size`, or not a whole multiple of it, come back as
/// they are.
pub fn shrink(frame: &Frame, size: ScreenSize) -> Cow<'_, Frame> {
    let own = frame.size();
    let factor = usize::from(own.width / size.width.max(1));
    let whole = factor > 1
        && own == ScreenSize::new(size.width * factor as u16, size.height * factor as u16);
    if !whole {
        return Cow::Borrowed(frame);
    }
    let width = usize::from(own.width);
    let source = frame.pixels();
    let mut small = Frame::new(size);
    for (row, line) in small.rows_mut().enumerate() {
        let y = row * factor + factor / 2;
        for (column, pixel) in line.iter_mut().enumerate() {
            *pixel = source[y * width + column * factor + factor / 2];
        }
    }
    Cow::Owned(small)
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
    fn turning_moves_pixels_round_the_frame() {
        // a b c
        // d e f
        let mut frame = Frame::new(ScreenSize::new(3, 2));
        let [a, b, c, d, e, f] = [1, 2, 3, 4, 5, 6].map(|value| rgb(value, 0, 0));
        frame.pixels_mut().copy_from_slice(&[a, b, c, d, e, f]);
        let pixels = |rotation| {
            let turned = turn(&frame, rotation);
            let size = turned.size();
            ((size.width, size.height), turned.pixels().to_vec())
        };
        assert_eq!(pixels(Rotation::Upright), ((3, 2), vec![a, b, c, d, e, f]));
        assert_eq!(
            pixels(Rotation::Clockwise),
            ((2, 3), vec![d, a, e, b, f, c])
        );
        assert_eq!(
            pixels(Rotation::UpsideDown),
            ((3, 2), vec![f, e, d, c, b, a])
        );
        assert_eq!(
            pixels(Rotation::Anticlockwise),
            ((2, 3), vec![c, f, b, e, a, d])
        );
    }

    #[test]
    fn shrinking_keeps_the_middle_of_each_square() {
        let mut frame = Frame::new(ScreenSize::new(4, 2));
        let pixels: Vec<u32> = (0..8).map(|value| rgb(value, 0, 0)).collect();
        frame.pixels_mut().copy_from_slice(&pixels);
        let small = shrink(&frame, ScreenSize::new(2, 1));
        assert_eq!(small.size(), ScreenSize::new(2, 1));
        // Squares of 2: the middle is the bottom right pixel.
        assert_eq!(small.pixels(), [rgb(5, 0, 0), rgb(7, 0, 0)]);
        assert!(matches!(shrink(&frame, frame.size()), Cow::Borrowed(_)));
        assert!(matches!(
            shrink(&frame, ScreenSize::new(3, 2)),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn prescale_factor_floors_device_pixels_per_source_pixel() {
        assert_eq!(prescale_factor(256, 512.0, 2.0), 4);
        assert_eq!(prescale_factor(256, 700.0, 1.0), 2);
        assert_eq!(prescale_factor(256, 100.0, 1.0), 1);
        assert_eq!(prescale_factor(160, 4000.0, 2.0), MAX_FACTOR);
    }
}
