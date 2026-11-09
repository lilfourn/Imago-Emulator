//! Video filters: CPU passes from one frame to another, run after colour
//! correction and rotation and before a frame becomes a GPUI image
//! ([decision 0005](../../../docs/decisions/0005-video-filters-on-the-cpu.md)).
//! They draw View > Picture's Sharp Pixels, LCD Grid and Scanlines; the
//! upscalers run on the GPU (`gpu_upscale`).
//!
//! Two kinds:
//! - **Shading** (LCD grid, scanlines) enlarges each pixel to the block of
//!   device pixels it covers on screen and darkens parts of the block, so
//!   it needs at least two device pixels per console pixel to show.
//! - **Pixel-art scalers** (Scale2x, Scale3x) round off the staircase edges
//!   of sprites and text at two or three times the size. They stand in for
//!   the Rounded Pixels upscaler on a Mac without Metal. They work on
//!   console pixels, so they skip frames already enlarged by DS 3D
//!   upscaling, whose 3D is smooth anyway.
//!
//! Scale2x and Scale3x are written here from their published rules; the
//! reference code is GPL, which the project does not reuse (PRD section 9).

use std::borrow::Cow;

use emu_core::{Frame, ScreenSize};

use crate::layout::Rotation;

/// A CPU pass over the screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Pixels as they are.
    #[default]
    None,
    /// Dark lines between pixels, as on the handhelds' LCDs.
    LcdGrid,
    /// Dark lines between rows, as on a CRT television.
    Scanlines,
    /// Twice the size, with diagonal edges filled in.
    Scale2x,
    /// Three times the size, with diagonal edges filled in.
    Scale3x,
}

/// Brightness of an LCD grid line, in 256ths, by how many device pixels
/// one console pixel covers: the thin lines of a large cell are darker so
/// the grid stays visible, without dimming the picture much.
fn grid_line_weight(cell: usize) -> u16 {
    match cell {
        0..=2 => 184,
        3 => 160,
        _ => 140,
    }
}

/// Brightness of the dark part of a scanline, in 256ths.
const SCANLINE_GAP: u16 = 140;

/// Brightness of the lit part of a scanline, in 256ths: a little above
/// full, to make up for the dark part.
const SCANLINE_BEAM: u16 = 272;

impl Filter {
    /// `frame`, already turned by `rotation`, filtered for drawing with
    /// each of its pixels `factor` device pixels wide. A frame at a higher
    /// 3D resolution has `cell` of its pixels to each console pixel.
    ///
    /// Returns the filtered frame and the whole factor still to enlarge it
    /// by; frames the filter leaves alone come back borrowed.
    pub fn apply<'a>(
        self,
        frame: &'a Frame,
        factor: usize,
        cell: usize,
        rotation: Rotation,
    ) -> (Cow<'a, Frame>, usize) {
        let factor = factor.max(1);
        let device_cell = cell.max(1) * factor;
        match self {
            Self::LcdGrid if device_cell > 1 => {
                let mask = Mask::lcd_grid(device_cell).turned(rotation);
                (Cow::Owned(shade(frame, factor, &mask)), 1)
            }
            Self::Scanlines if device_cell > 1 => {
                let mask = Mask::scanlines(device_cell).turned(rotation);
                (Cow::Owned(shade(frame, factor, &mask)), 1)
            }
            Self::Scale2x if cell <= 1 => (Cow::Owned(scale2x(frame)), (factor / 2).max(1)),
            Self::Scale3x if cell <= 1 => (Cow::Owned(scale3x(frame)), (factor / 3).max(1)),
            _ => (Cow::Borrowed(frame), factor),
        }
    }
}

/// How bright each device pixel of one console pixel's square is, in
/// 256ths: 256 leaves it as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    /// Device pixels along each side of the square.
    cell: usize,
    /// Row-major, `cell * cell` weights.
    weights: Vec<u16>,
}

impl Mask {
    /// A square of `cell` device pixels whose weight at each column and row
    /// is `weight(column, row)`.
    fn from_fn(cell: usize, weight: impl Fn(usize, usize) -> u16) -> Self {
        let cell = cell.max(1);
        let weights = (0..cell * cell)
            .map(|index| weight(index % cell, index / cell))
            .collect();
        Self { cell, weights }
    }

    /// An LCD's pixel grid: a dark line down the right of each pixel and
    /// along its bottom, darker where they cross.
    pub fn lcd_grid(cell: usize) -> Self {
        let line = grid_line_weight(cell);
        let last = cell.max(1) - 1;
        Self::from_fn(cell, |column, row| match (column == last, row == last) {
            (true, true) => line * line / 256,
            (true, false) | (false, true) => line,
            (false, false) => 256,
        })
    }

    /// A CRT's scanlines: the bottom third of each row of pixels (at least
    /// one device pixel) dark, the rest a little brighter.
    pub fn scanlines(cell: usize) -> Self {
        let cell = cell.max(1);
        let gap = cell.div_ceil(3);
        Self::from_fn(cell, |_, row| {
            if row >= cell - gap {
                SCANLINE_GAP
            } else {
                SCANLINE_BEAM
            }
        })
    }

    /// The mask turned by `rotation`, for a screen drawn turned that way.
    pub fn turned(self, rotation: Rotation) -> Self {
        let side = u16::try_from(self.cell).unwrap_or(u16::MAX);
        let size = ScreenSize::new(side, side);
        let cell = self.cell;
        Self::from_fn(cell, |column, row| {
            // Cells are a few dozen pixels at most, so these fit in u16.
            let (x, y) = rotation.source_pixel(size, column as u16, row as u16);
            self.weights[usize::from(y) * cell + usize::from(x)]
        })
    }

    /// The weights along `row` of the square.
    fn row(&self, row: usize) -> &[u16] {
        let start = (row % self.cell) * self.cell;
        &self.weights[start..start + self.cell]
    }
}

/// `pixel` (0xFFRRGGBB) with each channel scaled by `weight` 256ths.
fn tint(pixel: u32, weight: u16) -> u32 {
    if weight == 256 {
        return pixel;
    }
    let weight = u32::from(weight);
    let channel = |shift: u32| ((((pixel >> shift) & 0xFF) * weight) >> 8).min(0xFF) << shift;
    0xFF00_0000 | channel(16) | channel(8) | channel(0)
}

/// `frame` enlarged `factor` times, every console pixel's square of
/// device pixels shaded by `mask`. A pixel of a frame drawn at a higher
/// 3D resolution is part of a console pixel; the mask's squares follow
/// the console pixels.
pub fn shade(frame: &Frame, factor: usize, mask: &Mask) -> Frame {
    let factor = factor.max(1);
    let size = frame.size();
    let scaled = |length: u16| u16::try_from(usize::from(length) * factor).unwrap_or(u16::MAX);
    let width = usize::from(size.width);
    let mut out = Frame::new(ScreenSize::new(scaled(size.width), scaled(size.height)));
    let out_width = width * factor;
    let pixels = out.pixels_mut();
    for row in 0..usize::from(size.height) * factor {
        let start = row * out_width;
        // Most rows of a square shade alike: copy the row above if it came
        // from the same pixels with the same weights.
        let same = row % factor != 0 && mask.row(row) == mask.row(row - 1);
        if same {
            pixels.copy_within(start - out_width..start, start);
            continue;
        }
        let first = row / factor * width;
        let source = &frame.pixels()[first..first + width];
        let weights = mask.row(row);
        let line = &mut pixels[start..start + out_width];
        let mut column = 0;
        for (block, pixel) in line.chunks_exact_mut(factor).zip(source) {
            for out in block {
                *out = tint(*pixel, weights[column]);
                column += 1;
                if column == mask.cell {
                    column = 0;
                }
            }
        }
    }
    out
}

/// The pixel at (`x` + `dx`, `y` + `dy`), or the nearest one on the frame.
fn neighbour(frame: &Frame, x: usize, y: usize, dx: isize, dy: isize) -> u32 {
    let size = frame.size();
    let clamp = |at: usize, by: isize, length: u16| {
        at.saturating_add_signed(by)
            .min(usize::from(length).saturating_sub(1))
    };
    let (x, y) = (clamp(x, dx, size.width), clamp(y, dy, size.height));
    frame.pixels()[y * usize::from(size.width) + x]
}

/// `frame` at twice the size by Scale2x (AdvMAME2x): each pixel becomes
/// four, and a corner takes the colour of the two neighbours beside it
/// when they match each other but not the other two.
pub fn scale2x(frame: &Frame) -> Frame {
    let size = frame.size();
    let width = usize::from(size.width);
    let mut out = Frame::new(ScreenSize::new(size.width * 2, size.height * 2));
    let out_width = width * 2;
    let pixels = out.pixels_mut();
    for y in 0..usize::from(size.height) {
        for x in 0..width {
            let at = |dx, dy| neighbour(frame, x, y, dx, dy);
            let (centre, up, left, right, down) =
                (at(0, 0), at(0, -1), at(-1, 0), at(1, 0), at(0, 1));
            // A corner between two matching neighbours, unless the
            // neighbours across from them match too (a straight edge).
            let corner = |a: u32, b: u32, across_a: u32, across_b: u32| {
                if a == b && a != across_b && b != across_a {
                    a
                } else {
                    centre
                }
            };
            let top = 2 * y * out_width + 2 * x;
            let bottom = top + out_width;
            pixels[top] = corner(left, up, right, down);
            pixels[top + 1] = corner(up, right, down, left);
            pixels[bottom] = corner(down, left, up, right);
            pixels[bottom + 1] = corner(right, down, left, up);
        }
    }
    out
}

/// `frame` at three times the size by Scale3x (AdvMAME3x), the
/// three-by-three counterpart of [`scale2x`]: corners fill as there, and
/// an edge's middle takes a neighbour's colour where a corner beside it
/// filled but the pixel across that corner differs.
pub fn scale3x(frame: &Frame) -> Frame {
    let size = frame.size();
    let width = usize::from(size.width);
    let mut out = Frame::new(ScreenSize::new(size.width * 3, size.height * 3));
    let out_width = width * 3;
    let pixels = out.pixels_mut();
    for y in 0..usize::from(size.height) {
        for x in 0..width {
            let at = |dx, dy| neighbour(frame, x, y, dx, dy);
            // a b c
            // d e f
            // g h i
            let (a, b, c) = (at(-1, -1), at(0, -1), at(1, -1));
            let (d, e, f) = (at(-1, 0), at(0, 0), at(1, 0));
            let (g, h, i) = (at(-1, 1), at(0, 1), at(1, 1));
            // Which corners fill, as in Scale2x.
            let top_left = d == b && b != f && d != h;
            let top_right = b == f && b != d && f != h;
            let bottom_left = d == h && d != b && h != f;
            let bottom_right = h == f && h != d && f != b;
            let pick = |fill: bool, colour: u32| if fill { colour } else { e };
            let block = [
                pick(top_left, d),
                pick((top_left && e != c) || (top_right && e != a), b),
                pick(top_right, f),
                pick((top_left && e != g) || (bottom_left && e != a), d),
                e,
                pick((top_right && e != i) || (bottom_right && e != c), f),
                pick(bottom_left, d),
                pick((bottom_left && e != i) || (bottom_right && e != g), h),
                pick(bottom_right, f),
            ];
            let start = 3 * y * out_width + 3 * x;
            for (row, colours) in block.chunks_exact(3).enumerate() {
                let at = start + row * out_width;
                pixels[at..at + 3].copy_from_slice(colours);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::rgb;

    const W: u32 = rgb(0xFF, 0xFF, 0xFF);
    const K: u32 = rgb(0, 0, 0);

    /// White shaded by `weight` 256ths.
    fn grey(weight: u16) -> u32 {
        let level = u8::try_from((255 * u32::from(weight)) >> 8).expect("at most 255");
        rgb(level, level, level)
    }

    fn frame(width: u16, height: u16, pixels: &[u32]) -> Frame {
        let mut frame = Frame::new(ScreenSize::new(width, height));
        frame.pixels_mut().copy_from_slice(pixels);
        frame
    }

    #[test]
    fn no_filter_borrows_the_frame() {
        let source = frame(2, 1, &[W, K]);
        let (out, factor) = Filter::None.apply(&source, 4, 1, Rotation::Upright);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(factor, 4);
    }

    #[test]
    fn lcd_grid_darkens_the_right_and_bottom_of_each_pixel() {
        let source = frame(2, 1, &[W, K]);
        let (out, factor) = Filter::LcdGrid.apply(&source, 3, 1, Rotation::Upright);
        assert_eq!((out.size(), factor), (ScreenSize::new(6, 3), 1));
        let line = grid_line_weight(3);
        let pixels = out.pixels();
        assert_eq!(&pixels[..3], [W, W, grey(line)]);
        assert_eq!(pixels[2 * 6 + 2], grey(line * line / 256), "the corner");
        assert!(pixels[3..6].iter().all(|pixel| *pixel == K), "black stays");
    }

    #[test]
    fn shading_needs_two_device_pixels_per_console_pixel() {
        let source = frame(2, 1, &[W, K]);
        let (out, factor) = Filter::Scanlines.apply(&source, 1, 1, Rotation::Upright);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(factor, 1);
        // A frame drawn at 2x 3D resolution has two pixels to each console
        // pixel already, so it shades even unenlarged.
        let (out, _) = Filter::Scanlines.apply(&source, 1, 2, Rotation::Upright);
        assert!(matches!(out, Cow::Owned(_)));
    }

    #[test]
    fn scanlines_follow_console_rows_and_turn_with_the_screen() {
        let source = frame(1, 2, &[W, W]);
        let (out, _) = Filter::Scanlines.apply(&source, 3, 1, Rotation::Upright);
        let column: Vec<u32> = out.rows().map(|row| row[0]).collect();
        let dark = grey(SCANLINE_GAP);
        assert_eq!(column, [W, W, dark, W, W, dark]);
        // Turned a quarter, rows run down the window: the lines do too.
        let (out, _) = Filter::Scanlines.apply(&source, 3, 1, Rotation::Clockwise);
        let top: Vec<u32> = out.rows().next().expect("a row").to_vec();
        assert_eq!(top, [dark, W, W]);
    }

    #[test]
    fn masks_follow_console_pixels_on_upscaled_frames() {
        // A 2x 3D frame drawn 2x: each console pixel is four device pixels
        // wide, and only its last column is a grid line.
        let source = frame(2, 2, &[W; 4]);
        let (out, _) = Filter::LcdGrid.apply(&source, 2, 2, Rotation::Upright);
        let line = grey(grid_line_weight(4));
        assert_eq!(out.rows().next().expect("a row"), [W, W, W, line]);
    }

    /// A white field with black below a staircase diagonal:
    ///
    /// ```text
    /// W W W W
    /// W W W K
    /// W W K K
    /// W K K K
    /// ```
    fn staircase() -> Frame {
        frame(4, 4, &[W, W, W, W, W, W, W, K, W, W, K, K, W, K, K, K])
    }

    /// The pixel at (`x`, `y`) of `frame`.
    fn at(frame: &Frame, x: usize, y: usize) -> u32 {
        frame.pixels()[y * usize::from(frame.size().width) + x]
    }

    #[test]
    fn scale2x_rounds_a_diagonal_and_keeps_flat_areas() {
        let out = scale2x(&staircase());
        assert_eq!(out.size(), ScreenSize::new(8, 8));
        // The white pixel at (2, 1) has black right of it and below it:
        // its bottom right corner turns black. The black pixel at (3, 1)
        // has white above it and left of it: its top left turns white.
        assert_eq!(at(&out, 5, 3), K);
        assert_eq!([at(&out, 4, 2), at(&out, 5, 2), at(&out, 4, 3)], [W; 3]);
        assert_eq!(at(&out, 6, 2), W);
        assert_eq!([at(&out, 7, 2), at(&out, 6, 3), at(&out, 7, 3)], [K; 3]);
        // Away from the edge nothing changes.
        assert!((0..4).all(|y| (0..4).all(|x| at(&out, x, y) == W)));
        assert!((6..8).all(|y| (6..8).all(|x| at(&out, x, y) == K)));
        let flat = scale2x(&frame(2, 2, &[W; 4]));
        assert!(flat.pixels().iter().all(|pixel| *pixel == W));
    }

    #[test]
    fn scale2x_leaves_a_lone_pixel_square() {
        let mut pixels = [K; 9];
        pixels[4] = W;
        let out = scale2x(&frame(3, 3, &pixels));
        let whites: Vec<usize> = (0..36).filter(|i| out.pixels()[*i] == W).collect();
        assert_eq!(whites, [14, 15, 20, 21]);
    }

    #[test]
    fn scale3x_rounds_a_diagonal() {
        let out = scale3x(&staircase());
        assert_eq!(out.size(), ScreenSize::new(12, 12));
        // As with Scale2x, the white pixel at (2, 1) loses its bottom right
        // corner, and the black one at (3, 1) gains a white top left; that
        // corner's white also spreads to the middle of its top edge.
        assert_eq!(at(&out, 8, 5), K);
        assert_eq!([at(&out, 7, 5), at(&out, 8, 4), at(&out, 6, 3)], [W; 3]);
        assert_eq!(
            [at(&out, 9, 3), at(&out, 10, 3), at(&out, 11, 3)],
            [W, W, K]
        );
        assert_eq!([at(&out, 9, 4), at(&out, 10, 4)], [K, K]);
        let mut lone = [K; 9];
        lone[4] = W;
        let out = scale3x(&frame(3, 3, &lone));
        let whites = out.pixels().iter().filter(|pixel| **pixel == W).count();
        assert_eq!(whites, 9, "a lone pixel stays a square");
    }

    #[test]
    fn scalers_report_what_is_left_to_enlarge_and_skip_upscaled_frames() {
        let source = frame(2, 2, &[K, W, W, K]);
        let (out, factor) = Filter::Scale2x.apply(&source, 5, 1, Rotation::Upright);
        assert_eq!((out.size(), factor), (ScreenSize::new(4, 4), 2));
        let (out, factor) = Filter::Scale3x.apply(&source, 2, 1, Rotation::Upright);
        assert_eq!((out.size(), factor), (ScreenSize::new(6, 6), 1));
        let (out, factor) = Filter::Scale2x.apply(&source, 4, 2, Rotation::Upright);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(factor, 4);
    }
}
