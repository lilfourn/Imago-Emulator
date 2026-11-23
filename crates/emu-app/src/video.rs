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

/// Pixels along each side of the squares [`turn`] works in.
const TURN_TILE: u16 = 32;

/// `frame` turned by `rotation`, for screens drawn on their side.
///
/// Turned a quarter, a row is a column of the frame, which in a large frame
/// reads one pixel from each of hundreds of rows far apart in memory.
/// Working a square at a time keeps the rows it reads and writes in the
/// cache: a DS screen drawn with 3D at 4× turns twice as fast or more.
pub fn turn(frame: &Frame, rotation: Rotation) -> Frame {
    let size = frame.size();
    let width = usize::from(size.width);
    let source = frame.pixels();
    let mut turned = Frame::new(rotation.turn_size(size));
    let turned_size = turned.size();
    let turned_width = usize::from(turned_size.width);
    let pixels = turned.pixels_mut();
    // Upside down, a row is a row of the frame backwards: one tile will do.
    let tile = if rotation.swaps_axes() {
        TURN_TILE
    } else {
        u16::MAX
    };
    for top in (0..turned_size.height).step_by(usize::from(tile)) {
        let rows = top..turned_size.height.min(top.saturating_add(tile));
        for left in (0..turned_size.width).step_by(usize::from(tile)) {
            let columns = left..turned_size.width.min(left.saturating_add(tile));
            for row in rows.clone() {
                let start = usize::from(row) * turned_width;
                let line = &mut pixels[start..][usize::from(left)..usize::from(columns.end)];
                for (pixel, column) in line.iter_mut().zip(left..) {
                    let (x, y) = rotation.source_pixel(size, column, row);
                    *pixel = source[usize::from(y) * width + usize::from(x)];
                }
            }
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
    if factor <= 1 {
        // Filtered and upscaled-3D frames come already large: one copy.
        // Collecting each pixel's bytes instead grew the buffer as it went,
        // several times over for a 3DS screen at 4× (6 MB).
        return pixel_bytes(frame.pixels());
    }
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

/// `pixels` as BGRA bytes, each `0xAARRGGBB` pixel's little-endian bytes,
/// into a buffer made once at its full size: a loop the compiler turns
/// into wide copies.
pub fn pixel_bytes(pixels: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0; pixels.len() * 4];
    for (out, pixel) in bytes.chunks_exact_mut(4).zip(pixels) {
        out.copy_from_slice(&pixel.to_le_bytes());
    }
    bytes
}

/// A GPUI image of `frame` enlarged `factor` times.
pub fn render_image(frame: &Frame, factor: usize) -> Arc<RenderImage> {
    let size = frame.size();
    let scaled = |length: u16| {
        u32::try_from(usize::from(length) * factor).expect("factor is at most MAX_FACTOR")
    };
    bgra_image(
        scaled(size.width),
        scaled(size.height),
        upscale_bgra(frame, factor),
    )
}

/// A GPUI image `width` × `height` pixels of `bgra`, which must be exactly
/// `width * height * 4` bytes, as the upscaler and [`upscale_bgra`] give.
pub fn bgra_image(width: u32, height: u32, bgra: Vec<u8>) -> Arc<RenderImage> {
    // GPUI's images hold BGRA, whatever the buffer type calls it.
    let buffer = image::RgbaImage::from_raw(width, height, bgra)
        .expect("the bytes are exactly width * height * 4");
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

    /// [`turn`] as first written, a row of the turned frame at a time:
    /// the oracle for the tiled one.
    fn turn_row_by_row(frame: &Frame, rotation: Rotation) -> Frame {
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

    /// A frame of `size` whose every pixel differs.
    fn numbered(size: ScreenSize) -> Frame {
        let mut frame = Frame::new(size);
        for (index, pixel) in (0u32..).zip(frame.pixels_mut()) {
            *pixel = index;
        }
        frame
    }

    const ROTATIONS: [Rotation; 4] = [
        Rotation::Upright,
        Rotation::Clockwise,
        Rotation::UpsideDown,
        Rotation::Anticlockwise,
    ];

    #[test]
    fn turning_in_tiles_matches_turning_row_by_row() {
        // Whole tiles, part tiles, frames smaller than a tile, and lines.
        let sizes = [
            (256, 192),
            (1024, 768),
            (240, 160),
            (160, 144),
            (33, 65),
            (7, 3),
            (1, 40),
            (40, 1),
        ];
        for (width, height) in sizes {
            let frame = numbered(ScreenSize::new(width, height));
            for rotation in ROTATIONS {
                assert!(
                    turn(&frame, rotation) == turn_row_by_row(&frame, rotation),
                    "{width}x{height} {rotation:?}"
                );
            }
        }
    }

    /// Times [`turn`] against [`turn_row_by_row`]: `cargo test --release
    /// -p emu-app -- --ignored --nocapture turn_timing`.
    #[test]
    #[ignore = "a timing, not a test"]
    fn turn_timing() {
        use crate::bench::best_of;

        for (name, size) in [
            ("DS", ScreenSize::new(256, 192)),
            ("DS, 3D at 4x", ScreenSize::new(1024, 768)),
        ] {
            let frame = numbered(size);
            for rotation in [Rotation::Clockwise, Rotation::UpsideDown] {
                let old = best_of(100, || turn_row_by_row(&frame, rotation));
                let new = best_of(100, || turn(&frame, rotation));
                println!("turn {name} {rotation:?}: {old:?} -> {new:?}");
            }
        }
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
        assert_eq!(prescale_factor(160, 4000.0, 2.0), 6);
    }
}
