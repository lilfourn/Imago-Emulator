//! Drawing into the pictures the graphics viewers show: every core's
//! [`Graphics`](super::Graphics) is built from these, so tiles, maps,
//! sprites and palettes look alike whatever the console.
//!
//! Coordinates are pixels from the picture's top-left corner. Nothing here
//! can draw outside a picture: pixels past its edges are left out.

use crate::{Frame, ScreenSize, rgb};

/// Fills the parts of a picture that show nothing, such as gaps between
/// cells.
pub const BACKGROUND: u32 = rgb(0x20, 0x20, 0x20);
/// The visible screen's outline on a map: magenta stands out from the Game
/// Boy's greys and from most game palettes.
pub const OUTLINE: u32 = rgb(0xFF, 0x00, 0xFF);
/// The two squares of the checkerboard behind transparent sprite pixels.
pub const CHECKER: [u32; 2] = [rgb(0x40, 0x40, 0x40), rgb(0x58, 0x58, 0x58)];

/// A `width`×`height` picture filled with [`BACKGROUND`].
#[must_use]
pub fn blank(width: usize, height: usize) -> Frame {
    let mut frame = Frame::new(ScreenSize::new(width as u16, height as u16));
    frame.pixels_mut().fill(BACKGROUND);
    frame
}

/// Sets the pixel at (`x`, `y`) if it is inside the picture.
pub fn set_pixel(frame: &mut Frame, x: usize, y: usize, color: u32) {
    let size = frame.size();
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    if x < width && y < height {
        frame.pixels_mut()[y * width + x] = color;
    }
}

/// Fills the `width`×`height` rectangle from (`left`, `top`) with `color`.
pub fn fill_rect(
    frame: &mut Frame,
    (left, top): (usize, usize),
    (width, height): (usize, usize),
    color: u32,
) {
    for y in top..top + height {
        for x in left..left + width {
            set_pixel(frame, x, y, color);
        }
    }
}

/// Fills the `width`×`height` rectangle from (`left`, `top`) with a
/// checkerboard of 4-pixel squares, the usual picture of "transparent".
pub fn fill_checker(
    frame: &mut Frame,
    (left, top): (usize, usize),
    (width, height): (usize, usize),
) {
    for y in 0..height {
        for x in 0..width {
            set_pixel(frame, left + x, top + y, CHECKER[(x / 4 + y / 4) % 2]);
        }
    }
}

/// Outlines the `width`×`height` rectangle from (`left`, `top`) with
/// 1-pixel [`OUTLINE`] lines, wrapping around the picture's edges as the
/// screen wraps around the map.
pub fn outline_wrapping(
    frame: &mut Frame,
    (left, top): (usize, usize),
    (width, height): (usize, usize),
) {
    let size = frame.size();
    let (wrap_x, wrap_y) = (usize::from(size.width), usize::from(size.height));
    let (right, bottom) = (left + width - 1, top + height - 1);
    for x in left..=right {
        set_pixel(frame, x % wrap_x, top % wrap_y, OUTLINE);
        set_pixel(frame, x % wrap_x, bottom % wrap_y, OUTLINE);
    }
    for y in top..=bottom {
        set_pixel(frame, left % wrap_x, y % wrap_y, OUTLINE);
        set_pixel(frame, right % wrap_x, y % wrap_y, OUTLINE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: u32 = rgb(0xFF, 0xFF, 0xFF);

    fn pixel(frame: &Frame, x: usize, y: usize) -> u32 {
        frame.pixels()[y * usize::from(frame.size().width) + x]
    }

    #[test]
    fn a_blank_picture_is_all_background() {
        let frame = blank(24, 16);
        assert_eq!(frame.size(), ScreenSize::new(24, 16));
        assert!(frame.pixels().iter().all(|&pixel| pixel == BACKGROUND));
    }

    #[test]
    fn drawing_past_the_edges_is_left_out() {
        let mut frame = blank(8, 8);
        set_pixel(&mut frame, 8, 0, WHITE);
        set_pixel(&mut frame, 0, 8, WHITE);
        fill_rect(&mut frame, (6, 6), (4, 4), WHITE);
        let white = frame.pixels().iter().filter(|&&pixel| pixel == WHITE);
        assert_eq!(white.count(), 4, "only the corner inside the picture");
        assert_eq!(pixel(&frame, 7, 7), WHITE);
        assert_eq!(pixel(&frame, 5, 7), BACKGROUND);
    }

    #[test]
    fn a_checkerboard_has_four_pixel_squares_from_its_own_corner() {
        let mut frame = blank(16, 16);
        fill_checker(&mut frame, (2, 2), (8, 8));
        assert_eq!(pixel(&frame, 2, 2), CHECKER[0]);
        assert_eq!(pixel(&frame, 5, 5), CHECKER[0]);
        assert_eq!(pixel(&frame, 6, 2), CHECKER[1]);
        assert_eq!(pixel(&frame, 6, 6), CHECKER[0]);
        assert_eq!(pixel(&frame, 10, 2), BACKGROUND, "past its width");
    }

    #[test]
    fn an_outline_wraps_around_the_picture() {
        let mut frame = blank(16, 16);
        // From (12, 12), 8 pixels each way: its right and bottom edges are
        // at 19, which wraps to 3.
        outline_wrapping(&mut frame, (12, 12), (8, 8));
        assert_eq!(pixel(&frame, 12, 12), OUTLINE);
        assert_eq!(pixel(&frame, 3, 12), OUTLINE, "the top edge, wrapped");
        assert_eq!(pixel(&frame, 3, 3), OUTLINE, "the far corner");
        assert_eq!(pixel(&frame, 12, 0), OUTLINE, "the left edge, wrapped");
        assert_eq!(pixel(&frame, 13, 13), BACKGROUND, "inside");
        assert_eq!(pixel(&frame, 4, 4), BACKGROUND, "outside");
    }
}
