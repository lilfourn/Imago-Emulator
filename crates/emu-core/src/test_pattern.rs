//! Placeholder video for cores whose PPU is not written yet, so the
//! frontend's video, input and save-state paths can be exercised end to end.

use crate::{Frame, TouchPoint, rgb};

const BARS: [u32; 8] = [
    rgb(0xFF, 0xFF, 0xFF),
    rgb(0xFF, 0xFF, 0x00),
    rgb(0x00, 0xFF, 0xFF),
    rgb(0x00, 0xFF, 0x00),
    rgb(0xFF, 0x00, 0xFF),
    rgb(0xFF, 0x00, 0x00),
    rgb(0x00, 0x00, 0xFF),
    rgb(0x10, 0x10, 0x10),
];

/// Fills `frame` with eight vertical colour bars scrolled left by `offset`
/// pixels.
pub fn color_bars(frame: &mut Frame, offset: u64) {
    let width = u64::from(frame.size().width);
    let bar_width = (width / 8).max(1);
    for row in frame.rows_mut() {
        for (x, pixel) in (0..).zip(row.iter_mut()) {
            let bar = (x + offset) % width / bar_width;
            *pixel = BARS[usize::try_from(bar).unwrap_or(0).min(BARS.len() - 1)];
        }
    }
}

/// Inverts every pixel's colour, keeping it opaque.
pub fn invert(frame: &mut Frame) {
    for pixel in frame.pixels_mut() {
        *pixel ^= 0x00FF_FFFF;
    }
}

/// Draws a one-pixel black crosshair through `point`.
pub fn crosshair(frame: &mut Frame, point: TouchPoint) {
    let size = frame.size();
    let (x, y) = (usize::from(point.x), usize::from(point.y));
    if point.x >= size.width || point.y >= size.height {
        return;
    }
    let black = rgb(0, 0, 0);
    for (row_index, row) in frame.rows_mut().enumerate() {
        if row_index == y {
            row.fill(black);
        } else {
            row[x] = black;
        }
    }
}
