//! The backdrop behind the screens: a soft glow of the game's own colours
//! filling the margins the screens' proportions leave in the window.
//!
//! The screens are averaged, as the window arranges them, into a picture a
//! few dozen pixels across, which is blurred, toned and faded in over a
//! fraction of a second. Drawn stretched over the whole window, GPUI's
//! linear filtering smooths it the rest of the way; the screens themselves
//! hide the middle of it.

use std::time::{Duration, Instant};

use emu_core::{Frame, ScreenSize, rgb};

use crate::color::Correction;
use crate::layout::{Placement, Rect};
use crate::theme::STAGE as BACKGROUND;

/// What fills the window around the screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backdrop {
    /// A plain dark colour.
    Solid,
    /// The game's colours, blurred and dimmed.
    #[default]
    Ambient,
}

/// Each backdrop and its name in the settings file.
const NAMES: [(Backdrop, &str); 2] = [(Backdrop::Solid, "solid"), (Backdrop::Ambient, "ambient")];

impl Backdrop {
    /// The backdrop's name in the settings file.
    pub fn name(self) -> &'static str {
        NAMES
            .iter()
            .find(|(backdrop, _)| *backdrop == self)
            .map_or("ambient", |(_, name)| name)
    }

    /// The backdrop with this name in the settings file.
    pub fn from_name(name: &str) -> Option<Self> {
        NAMES
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(backdrop, _)| *backdrop)
    }

    /// The backdrop as the settings window names it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Ambient => "Ambient",
        }
    }
}

/// Pixels along the picture's longer side.
const SIZE: f32 = 48.0;

/// Most pixels averaged from each screen, a native DS screen's worth, so
/// frames drawn at a higher 3D resolution cost no more than native ones.
const MOST_SAMPLES: usize = 256 * 192;

/// Reach of each box blur pass, in picture pixels.
const BLUR_RADIUS: usize = 3;

/// Box blur passes each way; three come close to a Gaussian blur.
const BLUR_PASSES: usize = 3;

/// Brightness kept, so the glow stays behind the screens.
const BRIGHTNESS: f32 = 0.45;

/// How much more saturated than the screens the glow is; averaging greys
/// colours out, and this gives some back.
const SATURATION: f32 = 1.3;

/// Brightness taken away at the window's corners, falling off smoothly
/// from the middle.
const VIGNETTE: f32 = 0.5;

/// How long the glow takes to move most of the way (63%) to new colours,
/// so a flash on screen swells rather than flickers.
const FADE: Duration = Duration::from_millis(150);

/// Longest time one step of a fade covers, so after a pause, with nothing
/// drawn for a while, the next colours still fade in.
const LONGEST_STEP: Duration = Duration::from_millis(50);

/// Difference in any channel, out of 255, under which a fade is done.
const SETTLED: f32 = 0.5;

/// Pixels repeated around a picture's edges by [`Picture::frame`], to draw
/// past the window's edges. GPUI packs images edge to edge in its texture
/// atlas, and its linear filtering blends an image's outermost half pixel
/// with whatever lies beside it there, which here would be a screen.
pub const BORDER: u16 = 2;

/// Red, green and blue, from 0 to 255.
type Color = [f32; 3];

/// Reusable working buffers for [`picture_into`], so updating the backdrop
/// every frame allocates nothing on the heap.
#[derive(Debug, Default)]
pub struct Scratch {
    sums: Vec<[u32; 3]>,
    counts: Vec<u32>,
    across: Vec<usize>,
    down: Vec<usize>,
    runs: Vec<(usize, usize)>,
    blur: Vec<Color>,
}

/// A backdrop picture, in more precision than a frame holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    size: ScreenSize,
    colors: Vec<Color>,
}

impl Picture {
    /// The picture's size, in pixels.
    pub fn size(&self) -> ScreenSize {
        self.size
    }

    /// Its pixels' red, green and blue, 0 to 255, row by row: what Guide
    /// takes its colours from.
    pub fn colors(&self) -> &[[f32; 3]] {
        &self.colors
    }

    /// Writes the picture as opaque pixels into `out`, with its edge pixels
    /// repeated `border` times on every side, reusing `out`'s buffer when
    /// sizes match.
    pub fn frame_into(&self, border: u16, out: &mut Frame) {
        let (width, height) = (usize::from(self.size.width), usize::from(self.size.height));
        let border_width = usize::from(border);
        let target = ScreenSize::new(self.size.width + 2 * border, self.size.height + 2 * border);
        if out.size() != target {
            *out = Frame::new(target);
        }
        for (row, line) in out.rows_mut().enumerate() {
            let y = row.saturating_sub(border_width).min(height - 1);
            let src = &self.colors[y * width..(y + 1) * width];
            for (column, pixel) in line.iter_mut().enumerate() {
                let x = column.saturating_sub(border_width).min(width - 1);
                *pixel = opaque(src[x]);
            }
        }
    }

    /// The picture as opaque pixels, with its edge pixels repeated
    /// `border` times on every side.
    pub fn frame(&self, border: u16) -> Frame {
        let mut frame = Frame::new(ScreenSize::new(
            self.size.width + 2 * border,
            self.size.height + 2 * border,
        ));
        self.frame_into(border, &mut frame);
        frame
    }
}

/// Writes the backdrop for `frames` drawn at `placements` into `out`, reusing
/// `scratch` and `out`'s allocations. Returns `false` if nothing is placed.
pub fn picture_into(
    frames: &[Frame],
    placements: &[Placement],
    correction: Option<&Correction>,
    scratch: &mut Scratch,
    out: &mut Picture,
) -> bool {
    let Some(span) = span(placements) else {
        return false;
    };
    let size = picture_size(span);
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    sample_into(
        frames,
        placements,
        span,
        size,
        correction,
        scratch,
        &mut out.colors,
    );
    scratch.blur.resize(out.colors.len(), [0.0; 3]);
    for _ in 0..BLUR_PASSES {
        blur_horizontal(&out.colors, &mut scratch.blur, width, height);
        blur_vertical(&scratch.blur, &mut out.colors, width, height);
    }
    tone(&mut out.colors, size);
    out.size = size;
    true
}

/// The backdrop for `frames` drawn at `placements`: the area the
/// placements span, shrunk, blurred and toned, its colours corrected with
/// `correction` if given. Gaps between screens take the nearest screen's
/// colours. `None` if nothing is placed.
///
/// Every screen pixel counts toward the picture pixel it is drawn in, so
/// the picture is the same at any window size, and fine detail averages
/// out rather than flickering as it moves.
pub fn picture(
    frames: &[Frame],
    placements: &[Placement],
    correction: Option<&Correction>,
) -> Option<Picture> {
    let mut scratch = Scratch::default();
    let mut out = Picture {
        size: ScreenSize::new(0, 0),
        colors: Vec::new(),
    };
    picture_into(frames, placements, correction, &mut scratch, &mut out).then_some(out)
}

/// Where to shadow the screens at `placements`: the area they span if they
/// fill it, so screens that touch cast one shadow, or else each screen.
pub fn shadows(placements: &[Placement]) -> Vec<Rect> {
    let rects = || placements.iter().map(|placement| placement.rect);
    let area = |rect: Rect| rect.width * rect.height;
    match span(placements) {
        Some(span) if (rects().map(area).sum::<f32>() - area(span)).abs() <= area(span) * 0.001 => {
            vec![span]
        }
        _ => rects().collect(),
    }
}

/// A backdrop moving smoothly from one picture to the next.
#[derive(Debug, Clone)]
pub struct Fade {
    shown: Picture,
    target: Picture,
    at: Instant,
}

impl Fade {
    /// A fade showing `target` from the start, at `now`.
    pub fn new(target: Picture, now: Instant) -> Self {
        Self {
            shown: target.clone(),
            target,
            at: now,
        }
    }

    /// Heads for `target` from wherever the fade has got to, or jumps
    /// there if it is a different size.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn aim(&mut self, target: Picture) {
        if target.size != self.shown.size {
            self.shown = target.clone();
        }
        self.target = target;
    }

    /// Updates the target picture in place with `build`, jumping straight to
    /// it if its size changes. Returns `false` if `build` produced no picture.
    pub fn aim_with(&mut self, build: impl FnOnce(&mut Picture) -> bool) -> bool {
        if !build(&mut self.target) {
            return false;
        }
        if self.target.size != self.shown.size {
            self.shown.size = self.target.size;
            self.shown.colors.clear();
            self.shown.colors.extend_from_slice(&self.target.colors);
        }
        true
    }

    /// Moves on to `now`. Whether there is still further to go.
    pub fn step(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).min(LONGEST_STEP);
        self.at = now;
        let amount = 1.0 - (-elapsed.as_secs_f32() / FADE.as_secs_f32()).exp();
        let mut moving = false;
        for (shown, target) in self.shown.colors.iter_mut().zip(&self.target.colors) {
            for (shown, target) in shown.iter_mut().zip(target) {
                *shown += (target - *shown) * amount;
                if (target - *shown).abs() <= SETTLED {
                    *shown = *target;
                } else {
                    moving = true;
                }
            }
        }
        moving
    }

    /// Whether the fade has arrived where it was heading.
    pub fn settled(&self) -> bool {
        self.shown == self.target
    }

    /// The picture the fade has got to.
    pub fn shown(&self) -> &Picture {
        &self.shown
    }
}

/// The smallest rectangle around every placement, if it has an area.
fn span(placements: &[Placement]) -> Option<Rect> {
    let rects = placements.iter().map(|placement| placement.rect);
    let left = rects.clone().map(|rect| rect.x).reduce(f32::min)?;
    let top = rects.clone().map(|rect| rect.y).reduce(f32::min)?;
    let right = rects
        .clone()
        .map(|rect| rect.x + rect.width)
        .reduce(f32::max)?;
    let bottom = rects.map(|rect| rect.y + rect.height).reduce(f32::max)?;
    (right > left && bottom > top).then_some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// A picture size with `span`'s proportions, [`SIZE`] along its longer
/// side.
fn picture_size(span: Rect) -> ScreenSize {
    let scale = SIZE / span.width.max(span.height);
    // At most SIZE, and at least one, so the casts cannot truncate.
    let length = |length: f32| ((length * scale).round() as u16).max(1);
    ScreenSize::new(length(span.width), length(span.height))
}

/// Each picture pixel's average colour over the screen pixels drawn in
/// the part of `span` it covers. Picture pixels no screen covers take the
/// colour of the nearest screen pixel.
#[cfg(test)]
fn sample(
    frames: &[Frame],
    placements: &[Placement],
    span: Rect,
    size: ScreenSize,
    correction: Option<&Correction>,
) -> Vec<Color> {
    let mut scratch = Scratch::default();
    let mut colors = Vec::new();
    sample_into(
        frames,
        placements,
        span,
        size,
        correction,
        &mut scratch,
        &mut colors,
    );
    colors
}

/// [`sample`] writing into `colors` using reusable buffers in `scratch`.
fn sample_into(
    frames: &[Frame],
    placements: &[Placement],
    span: Rect,
    size: ScreenSize,
    correction: Option<&Correction>,
    scratch: &mut Scratch,
    colors: &mut Vec<Color>,
) {
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let scale = (
        f32::from(size.width) / span.width,
        f32::from(size.height) / span.height,
    );
    let pixel_count = size.pixel_count();
    // Whole numbers, so they add up exactly in any order.
    scratch.sums.resize(pixel_count, [0; 3]);
    scratch.sums.fill([0; 3]);
    scratch.counts.resize(pixel_count, 0);
    scratch.counts.fill(0);
    for placement in placements {
        let Some(frame) = frames.get(placement.screen) else {
            continue;
        };
        let source = frame.size();
        let (source_width, source_height) = (usize::from(source.width), usize::from(source.height));
        // Every pixel, or for a large frame an even grid of them. Only the
        // frame's size decides which, never the window's.
        let stride = (source_width * source_height / MOST_SAMPLES).isqrt().max(1);
        let first = stride / 2;
        // The picture's column and row that frame pixel (x, y) is drawn in.
        let place = |x: usize, y: usize| {
            let centre = (x as f32 + 0.5, y as f32 + 0.5);
            let (left, top) = placement.window_point(source, centre);
            // `as` saturates, so points a rounding error outside the
            // span land in its edge pixels.
            let column = (((left - span.x) * scale.0) as usize).min(width - 1);
            let row = (((top - span.y) * scale.1) as usize).min(height - 1);
            (column, row)
        };
        // A frame's columns decide the picture's column, or, turned a
        // quarter, its row, and the frame's rows the other. So a pixel's
        // index in the picture is one offset for its column plus one for
        // its row, each worked out once rather than for every pixel.
        let turned = placement.rotation.swaps_axes();
        scratch.across.clear();
        scratch
            .across
            .extend(
                (first..source_width)
                    .step_by(stride)
                    .map(|x| match place(x, first) {
                        (_, row) if turned => row * width,
                        (column, _) => column,
                    }),
            );
        scratch.down.clear();
        scratch
            .down
            .extend(
                (first..source_height)
                    .step_by(stride)
                    .map(|y| match place(first, y) {
                        (column, _) if turned => column,
                        (_, row) => row * width,
                    }),
            );
        // Samples side by side in a row mostly land in one picture pixel:
        // each run of them is added up on its own, then added in at once.
        scratch.runs.clear();
        scratch.runs.extend(
            scratch
                .across
                .chunk_by(|one, next| one == next)
                .map(|run| (run[0], run.len())),
        );
        let sums = &mut scratch.sums;
        let counts = &mut scratch.counts;
        let runs = &scratch.runs;
        if stride == 1 {
            if let Some(correction) = correction {
                for (line, &offset) in frame.rows().zip(&scratch.down) {
                    let mut pos = 0;
                    for &(along, length) in runs {
                        let mut sr = 0_u32;
                        let mut sg = 0_u32;
                        let mut sb = 0_u32;
                        for &pixel in &line[pos..pos + length] {
                            let pixel = correction.pixel(pixel);
                            sr += (pixel >> 16) & 0xFF;
                            sg += (pixel >> 8) & 0xFF;
                            sb += pixel & 0xFF;
                        }
                        pos += length;
                        let index = offset + along;
                        sums[index][0] += sr;
                        sums[index][1] += sg;
                        sums[index][2] += sb;
                        counts[index] += length as u32;
                    }
                }
            } else {
                for (line, &offset) in frame.rows().zip(&scratch.down) {
                    let mut pos = 0;
                    for &(along, length) in runs {
                        let mut sr = 0_u32;
                        let mut sg = 0_u32;
                        let mut sb = 0_u32;
                        for &pixel in &line[pos..pos + length] {
                            sr += (pixel >> 16) & 0xFF;
                            sg += (pixel >> 8) & 0xFF;
                            sb += pixel & 0xFF;
                        }
                        pos += length;
                        let index = offset + along;
                        sums[index][0] += sr;
                        sums[index][1] += sg;
                        sums[index][2] += sb;
                        counts[index] += length as u32;
                    }
                }
            }
        } else if let Some(correction) = correction {
            for (line, &offset) in frame.rows().skip(first).step_by(stride).zip(&scratch.down) {
                let mut sampled = line.iter().skip(first).step_by(stride);
                for &(along, length) in runs {
                    let mut sr = 0_u32;
                    let mut sg = 0_u32;
                    let mut sb = 0_u32;
                    for &pixel in sampled.by_ref().take(length) {
                        let pixel = correction.pixel(pixel);
                        sr += (pixel >> 16) & 0xFF;
                        sg += (pixel >> 8) & 0xFF;
                        sb += pixel & 0xFF;
                    }
                    let index = offset + along;
                    sums[index][0] += sr;
                    sums[index][1] += sg;
                    sums[index][2] += sb;
                    counts[index] += length as u32;
                }
            }
        } else {
            for (line, &offset) in frame.rows().skip(first).step_by(stride).zip(&scratch.down) {
                let mut sampled = line.iter().skip(first).step_by(stride);
                for &(along, length) in runs {
                    let mut sr = 0_u32;
                    let mut sg = 0_u32;
                    let mut sb = 0_u32;
                    for &pixel in sampled.by_ref().take(length) {
                        sr += (pixel >> 16) & 0xFF;
                        sg += (pixel >> 8) & 0xFF;
                        sb += pixel & 0xFF;
                    }
                    let index = offset + along;
                    sums[index][0] += sr;
                    sums[index][1] += sg;
                    sums[index][2] += sb;
                    counts[index] += length as u32;
                }
            }
        }
    }
    colors.clear();
    colors.reserve(pixel_count);
    for (index, (&sum, &count)) in scratch.sums.iter().zip(&scratch.counts).enumerate() {
        if count > 0 {
            let inv = count as f32;
            colors.push([
                sum[0] as f32 / inv,
                sum[1] as f32 / inv,
                sum[2] as f32 / inv,
            ]);
            continue;
        }
        let centre = (
            span.x + ((index % width) as f32 + 0.5) / scale.0,
            span.y + ((index / width) as f32 + 0.5) / scale.1,
        );
        let pixel = pixel_near(frames, placements, centre).unwrap_or(0);
        let pixel = correction.map_or(pixel, |correction| correction.pixel(pixel));
        colors.push(channels(pixel));
    }
}

/// The pixel shown at window `point`, or, between screens, at the nearest
/// point of the nearest screen.
fn pixel_near(frames: &[Frame], placements: &[Placement], point: (f32, f32)) -> Option<u32> {
    let placement = placements
        .iter()
        .min_by(|a, b| distance(a.rect, point).total_cmp(&distance(b.rect, point)))?;
    let frame = frames.get(placement.screen)?;
    let at = placement.pixel_nearest(frame.size(), point)?;
    let index = usize::from(at.y) * usize::from(frame.size().width) + usize::from(at.x);
    frame.pixels().get(index).copied()
}

/// How far `point` lies outside `rect`, squared; zero inside it.
fn distance(rect: Rect, point: (f32, f32)) -> f32 {
    let dx = (rect.x - point.0)
        .max(point.0 - rect.x - rect.width)
        .max(0.0);
    let dy = (rect.y - point.1)
        .max(point.1 - rect.y - rect.height)
        .max(0.0);
    dx * dx + dy * dy
}

/// Horizontal box blur of `src` into `dst`, averaging each pixel with its
/// [`BLUR_RADIUS`] neighbours on each side, repeating edge pixels past ends.
fn blur_horizontal(src: &[Color], dst: &mut [Color], width: usize, height: usize) {
    let taps = 2 * BLUR_RADIUS + 1;
    let divisor = taps as f32;
    let left_end = BLUR_RADIUS.min(width);
    let inner_end = width.saturating_sub(BLUR_RADIUS);
    let right_start = inner_end.max(left_end);
    for y in 0..height {
        let row = &src[y * width..(y + 1) * width];
        let out = &mut dst[y * width..(y + 1) * width];
        for x in 0..left_end {
            let mut sum = [0.0_f32; 3];
            for offset in 0..taps {
                let c = row[(x + offset).saturating_sub(BLUR_RADIUS).min(width - 1)];
                sum[0] += c[0];
                sum[1] += c[1];
                sum[2] += c[2];
            }
            out[x] = [sum[0] / divisor, sum[1] / divisor, sum[2] / divisor];
        }
        for x in left_end..inner_end {
            let mut sum = [0.0_f32; 3];
            for c in &row[x - BLUR_RADIUS..=x + BLUR_RADIUS] {
                sum[0] += c[0];
                sum[1] += c[1];
                sum[2] += c[2];
            }
            out[x] = [sum[0] / divisor, sum[1] / divisor, sum[2] / divisor];
        }
        for x in right_start..width {
            let mut sum = [0.0_f32; 3];
            for offset in 0..taps {
                let c = row[(x + offset).saturating_sub(BLUR_RADIUS).min(width - 1)];
                sum[0] += c[0];
                sum[1] += c[1];
                sum[2] += c[2];
            }
            out[x] = [sum[0] / divisor, sum[1] / divisor, sum[2] / divisor];
        }
    }
}

/// Vertical box blur of `src` into `dst`, averaging each pixel with its
/// [`BLUR_RADIUS`] neighbours above and below, repeating edge rows past ends.
fn blur_vertical(src: &[Color], dst: &mut [Color], width: usize, height: usize) {
    let taps = 2 * BLUR_RADIUS + 1;
    let divisor = taps as f32;
    for y in 0..height {
        let rows: [&[Color]; 2 * BLUR_RADIUS + 1] = std::array::from_fn(|offset| {
            let iy = (y + offset).saturating_sub(BLUR_RADIUS).min(height - 1);
            &src[iy * width..(iy + 1) * width]
        });
        let out = &mut dst[y * width..(y + 1) * width];
        for (x, pixel) in out.iter_mut().enumerate() {
            let mut sum = [0.0_f32; 3];
            for row in &rows {
                let c = row[x];
                sum[0] += c[0];
                sum[1] += c[1];
                sum[2] += c[2];
            }
            *pixel = [sum[0] / divisor, sum[1] / divisor, sum[2] / divisor];
        }
    }
}

/// Tones `colors`, a `size` picture, to sit behind the screens: richer,
/// dimmer, darker toward the corners, and lit over the app's background
/// so a black screen leaves it as it is.
fn tone(colors: &mut [Color], size: ScreenSize) {
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let (width_f, height_f) = (f32::from(size.width), f32::from(size.height));
    let background = channels(BACKGROUND);
    for y in 0..height {
        let v = (y as f32 + 0.5) / height_f * 2.0 - 1.0;
        let vv = v * v;
        let row = &mut colors[y * width..(y + 1) * width];
        for (x, color) in row.iter_mut().enumerate() {
            // From -1 to 1 across and down, from the middle.
            let u = (x as f32 + 0.5) / width_f * 2.0 - 1.0;
            let light = BRIGHTNESS * (1.0 - VIGNETTE * (u * u + vv) / 2.0);
            let [red, green, blue] = *color;
            let luma = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
            for (channel, back) in color.iter_mut().zip(background) {
                let lit = ((luma + (*channel - luma) * SATURATION).clamp(0.0, 255.0)) * light;
                // Screen blending: light added to the background, never past
                // white.
                *channel = 255.0 - (255.0 - back) * (255.0 - lit) / 255.0;
            }
        }
    }
}

/// `pixel`'s red, green and blue.
fn channels(pixel: u32) -> Color {
    levels(pixel).map(|level| level as f32)
}

/// `pixel`'s red, green and blue, from 0 to 255.
fn levels(pixel: u32) -> [u32; 3] {
    [16, 8, 0].map(|shift| (pixel >> shift) & 0xFF)
}

/// `color`, rounded, as an opaque pixel.
fn opaque(color: Color) -> u32 {
    // Clamped to 0..=255 first, so the casts cannot truncate.
    let [red, green, blue] = color.map(|channel| channel.round().clamp(0.0, 255.0) as u8);
    rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Rotation;

    const DS: ScreenSize = ScreenSize::new(256, 192);

    fn filled(size: ScreenSize, color: u32) -> Frame {
        let mut frame = Frame::new(size);
        frame.pixels_mut().fill(color);
        frame
    }

    fn placement(screen: usize, x: f32, y: f32, width: f32, height: f32) -> Placement {
        Placement {
            screen,
            rect: Rect {
                x,
                y,
                width,
                height,
            },
            rotation: Rotation::Upright,
        }
    }

    /// A stacked DS drawn `scale` times its size, `left` from the window's
    /// edge.
    fn stacked(left: f32, scale: f32) -> [Placement; 2] {
        let (width, height) = (256.0 * scale, 192.0 * scale);
        [
            placement(0, left, 0.0, width, height),
            placement(1, left, height, width, height),
        ]
    }

    /// One-pixel stripes, `first` in even columns and the other colour in
    /// odd ones.
    fn stripes(first: u32, second: u32) -> Frame {
        let mut frame = Frame::new(DS);
        for row in frame.rows_mut() {
            for (column, pixel) in row.iter_mut().enumerate() {
                *pixel = if column % 2 == 0 { first } else { second };
            }
        }
        frame
    }

    fn pixel(picture: &Picture, x: usize, y: usize) -> [u32; 3] {
        let frame = picture.frame(0);
        let pixel = frame.pixels()[y * usize::from(picture.size().width) + x];
        channels(pixel).map(|channel| channel as u32)
    }

    #[test]
    fn nothing_placed_has_no_picture() {
        assert_eq!(picture(&[], &[], None), None);
    }

    #[test]
    fn a_stacked_ds_gives_a_tall_picture_of_its_colours_dimmed() {
        let frames = [filled(DS, rgb(200, 100, 0)), filled(DS, rgb(200, 100, 0))];
        let picture = picture(&frames, &stacked(100.0, 1.0), None).expect("screens are placed");
        assert_eq!(picture.size(), ScreenSize::new(32, 48));
        let [red, green, blue] = pixel(&picture, 16, 24);
        assert!(red > green && green > blue, "{red} {green} {blue}");
        assert!(red < 140, "dimmed: {red}");
        // The corners are darker than the middle.
        assert!(pixel(&picture, 0, 0)[0] < red);
    }

    #[test]
    fn a_black_screen_leaves_the_background_as_it_is() {
        let frames = [filled(DS, 0), filled(DS, 0)];
        let picture = picture(&frames, &stacked(0.0, 1.0), None).expect("screens are placed");
        let background = channels(BACKGROUND).map(|channel| channel as u32);
        assert_eq!(pixel(&picture, 0, 0), background);
        assert_eq!(pixel(&picture, 16, 24), background);
    }

    #[test]
    fn each_screen_colours_its_own_end() {
        let frames = [filled(DS, rgb(255, 0, 0)), filled(DS, rgb(0, 0, 255))];
        // A gap between the screens takes the nearer one's colour.
        let placements = [
            placement(0, 0.0, 0.0, 256.0, 192.0),
            placement(1, 0.0, 282.0, 256.0, 192.0),
        ];
        let picture = picture(&frames, &placements, None).expect("screens are placed");
        let bottom = usize::from(picture.size().height) - 1;
        let [top_red, _, top_blue] = pixel(&picture, 0, 0);
        let [bottom_red, _, bottom_blue] = pixel(&picture, 0, bottom);
        assert!(top_red > top_blue + 30, "{top_red} {top_blue}");
        assert!(bottom_blue > bottom_red + 30, "{bottom_red} {bottom_blue}");
    }

    #[test]
    fn turned_screens_are_sampled_as_drawn() {
        // Red on the left of the upright screen, which turning clockwise
        // puts at the top.
        let size = ScreenSize::new(240, 160);
        let mut frame = filled(size, rgb(0, 0, 255));
        for row in frame.rows_mut() {
            row[..120].fill(rgb(255, 0, 0));
        }
        let placements = [Placement {
            rotation: Rotation::Clockwise,
            ..placement(0, 0.0, 0.0, 160.0, 240.0)
        }];
        let picture = picture(&[frame], &placements, None).expect("a screen is placed");
        let bottom = usize::from(picture.size().height) - 1;
        let [top_red, _, top_blue] = pixel(&picture, 0, 0);
        let [bottom_red, _, bottom_blue] = pixel(&picture, 0, bottom);
        assert!(top_red > top_blue, "{top_red} {top_blue}");
        assert!(bottom_blue > bottom_red, "{bottom_red} {bottom_blue}");
    }

    #[test]
    fn fine_detail_averages_out_rather_than_flickering() {
        // Stripes one pixel wide, moved along by one pixel, look the same.
        let white = rgb(255, 255, 255);
        let one = stripes(white, 0);
        let other = stripes(0, white);
        for scale in [1.0, 2.0, 2.734, 3.5] {
            let placements = stacked(0.0, scale);
            let before = picture(&[one.clone(), one.clone()], &placements, None);
            let after = picture(&[other.clone(), other.clone()], &placements, None);
            assert_eq!(before, after, "at {scale}x");
        }
    }

    #[test]
    fn the_picture_is_the_same_at_any_window_size() {
        let mut frame = Frame::new(DS);
        for (index, pixel) in frame.pixels_mut().iter_mut().enumerate() {
            *pixel = rgb((index * 7) as u8, (index * 13) as u8, (index / 256) as u8);
        }
        let frames = [frame.clone(), frame];
        let at = |left, scale| picture(&frames, &stacked(left, scale), None);
        let native = at(0.0, 1.0);
        for (left, scale) in [(37.5, 1.8229), (210.0, 2.0833), (5.25, 3.177)] {
            assert_eq!(at(left, scale), native, "at {scale}x");
        }
    }

    #[test]
    fn large_frames_are_sampled_the_same_at_any_window_size() {
        // A DS screen drawn at twice the resolution, in one-pixel stripes.
        let mut frame = Frame::new(ScreenSize::new(512, 384));
        for row in frame.rows_mut() {
            for (column, pixel) in row.iter_mut().enumerate() {
                *pixel = if column % 2 == 0 {
                    rgb(255, 255, 255)
                } else {
                    0
                };
            }
        }
        let frames = [frame.clone(), frame];
        let at = |scale| picture(&frames, &stacked(0.0, scale), None);
        let native = at(1.0);
        for scale in [2.5, 2.5026, 1.2, 4.0] {
            assert_eq!(at(scale), native, "at {scale}x");
        }
    }

    #[test]
    fn frames_carry_a_border_of_their_edge_pixels() {
        let frames = [filled(DS, rgb(255, 0, 0)), filled(DS, rgb(0, 0, 255))];
        let picture = picture(&frames, &stacked(0.0, 1.0), None).expect("screens are placed");
        let framed = picture.frame(BORDER);
        let size = picture.size();
        let border = usize::from(BORDER);
        assert_eq!(
            framed.size(),
            ScreenSize::new(size.width + 2 * BORDER, size.height + 2 * BORDER)
        );
        let width = usize::from(framed.size().width);
        let inner = picture.frame(0);
        assert_eq!(framed.pixels()[0], inner.pixels()[0]);
        assert_eq!(framed.pixels()[border * width + border], inner.pixels()[0]);
        assert_eq!(
            framed.pixels().last(),
            inner.pixels().last(),
            "the far corner repeats too"
        );
    }

    #[test]
    fn touching_screens_cast_one_shadow() {
        assert_eq!(
            shadows(&stacked(10.0, 2.0)),
            vec![Rect {
                x: 10.0,
                y: 0.0,
                width: 512.0,
                height: 768.0
            }]
        );
        let apart = [
            placement(0, 0.0, 0.0, 256.0, 192.0),
            placement(1, 0.0, 282.0, 256.0, 192.0),
        ];
        assert_eq!(shadows(&apart).len(), 2);
    }

    #[test]
    fn a_fade_eases_toward_new_colours_and_settles() {
        let dark = picture(&[filled(DS, 0)], &stacked(0.0, 1.0)[..1], None).expect("placed");
        let light = picture(
            &[filled(DS, rgb(255, 255, 255))],
            &stacked(0.0, 1.0)[..1],
            None,
        )
        .expect("placed");
        let start = Instant::now();
        let mut fade = Fade::new(dark.clone(), start);
        fade.aim(light.clone());
        assert!(fade.step(start + LONGEST_STEP / 2));
        let middle = pixel(fade.shown(), 16, 12)[0];
        assert!(
            middle > pixel(&dark, 16, 12)[0] && middle < pixel(&light, 16, 12)[0],
            "{middle}"
        );
        // However long it has been, one step covers only so much.
        assert!(fade.step(start + FADE * 20));
        let steps = (1..100).map(|step| start + FADE * 20 + LONGEST_STEP * step);
        assert!(steps.map(|now| fade.step(now)).any(|moving| !moving));
        assert_eq!(fade.shown(), &light);
    }

    /// [`sample`] as it was first written, placing every pixel on its own
    /// through [`Placement::window_point`]: the oracle for the faster one.
    fn sample_each_pixel(
        frames: &[Frame],
        placements: &[Placement],
        span: Rect,
        size: ScreenSize,
        correction: Option<&Correction>,
    ) -> Vec<Color> {
        let (width, height) = (usize::from(size.width), usize::from(size.height));
        let scale = (
            f32::from(size.width) / span.width,
            f32::from(size.height) / span.height,
        );
        let mut sums = vec![[0.0; 3]; size.pixel_count()];
        let mut counts = vec![0_u32; size.pixel_count()];
        for placement in placements {
            let Some(frame) = frames.get(placement.screen) else {
                continue;
            };
            let source = frame.size();
            let (source_width, source_height) =
                (usize::from(source.width), usize::from(source.height));
            let stride = (source_width * source_height / MOST_SAMPLES).isqrt().max(1);
            for y in (stride / 2..source_height).step_by(stride) {
                for x in (stride / 2..source_width).step_by(stride) {
                    let centre = (x as f32 + 0.5, y as f32 + 0.5);
                    let (left, top) = placement.window_point(source, centre);
                    let column = (((left - span.x) * scale.0) as usize).min(width - 1);
                    let row = (((top - span.y) * scale.1) as usize).min(height - 1);
                    let pixel = frame.pixels()[y * source_width + x];
                    let pixel = correction.map_or(pixel, |correction| correction.pixel(pixel));
                    let index = row * width + column;
                    for (sum, channel) in sums[index].iter_mut().zip(channels(pixel)) {
                        *sum += channel;
                    }
                    counts[index] += 1;
                }
            }
        }
        let mut colors = Vec::with_capacity(sums.len());
        for (index, (sum, count)) in sums.into_iter().zip(counts).enumerate() {
            if count > 0 {
                colors.push(sum.map(|channel| channel / count as f32));
                continue;
            }
            let centre = (
                span.x + ((index % width) as f32 + 0.5) / scale.0,
                span.y + ((index / width) as f32 + 0.5) / scale.1,
            );
            let pixel = pixel_near(frames, placements, centre).unwrap_or(0);
            let pixel = correction.map_or(pixel, |correction| correction.pixel(pixel));
            colors.push(channels(pixel));
        }
        colors
    }

    /// A frame of `size` in made-up colours, different for each `seed`,
    /// with the 15-bit colours GBC and GBA cores draw.
    fn noise(size: ScreenSize, seed: u32) -> Frame {
        let mut frame = Frame::new(size);
        let mut state = seed.wrapping_mul(0x9E37_79B9) | 1;
        let widen = |level: u32| (level << 3 | level >> 2) as u8;
        for pixel in frame.pixels_mut() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *pixel = rgb(
                widen(state & 31),
                widen(state >> 5 & 31),
                widen(state >> 10 & 31),
            );
        }
        frame
    }

    /// `screens` placed one under another at `scale`, `left` and `top`
    /// from the window's corner, `gap` apart, each turned by `rotation`.
    fn column_of(
        screens: &[ScreenSize],
        rotation: Rotation,
        (left, top, scale, gap): (f32, f32, f32, f32),
    ) -> Vec<Placement> {
        let mut y = top;
        let mut placements = Vec::new();
        for (screen, &size) in screens.iter().enumerate() {
            let turned = rotation.turn_size(size);
            let (width, height) = (
                f32::from(turned.width) * scale,
                f32::from(turned.height) * scale,
            );
            placements.push(Placement {
                rotation,
                ..placement(screen, left, y, width, height)
            });
            y += height + gap;
        }
        placements
    }

    #[test]
    fn sampling_places_pixels_as_the_window_draws_them() {
        let systems: [&[ScreenSize]; 6] = [
            &[DS, DS],
            &[ScreenSize::new(1024, 768), ScreenSize::new(1024, 768)],
            &[ScreenSize::new(512, 384), DS],
            &[ScreenSize::new(240, 160)],
            &[ScreenSize::new(160, 144)],
            &[ScreenSize::new(400, 240), ScreenSize::new(320, 240)],
        ];
        let rotations = [
            Rotation::Upright,
            Rotation::Clockwise,
            Rotation::UpsideDown,
            Rotation::Anticlockwise,
        ];
        let places = [
            (0.0, 0.0, 1.0, 0.0),
            (37.5, 12.25, 1.8229, 0.0),
            (210.0, 0.0, 2.0833, 90.0),
            (5.25, 3.5, 3.177, 13.7),
            (0.0, 0.0, 0.31, 0.0),
        ];
        let gbc = Correction::for_system(emu_core::System::Gb);
        let gba = Correction::for_system(emu_core::System::Gba);
        let mut seed = 0;
        for screens in systems {
            for rotation in rotations {
                for place in places {
                    for correction in [None, gbc.as_ref(), gba.as_ref()] {
                        seed += 1;
                        let frames: Vec<Frame> = (0..)
                            .zip(screens)
                            .map(|(index, &size)| noise(size, seed * 4 + index))
                            .collect();
                        let placements = column_of(screens, rotation, place);
                        let span = span(&placements).expect("screens are placed");
                        let size = picture_size(span);
                        assert_eq!(
                            sample(&frames, &placements, span, size, correction),
                            sample_each_pixel(&frames, &placements, span, size, correction),
                            "{screens:?} {rotation:?} at {place:?}"
                        );
                    }
                }
            }
        }
    }

    /// Times [`sample`] against [`sample_each_pixel`], and the whole
    /// [`picture`]: `cargo test --release -p emu-app -- --ignored
    /// --nocapture ambient_timing`.
    #[test]
    #[ignore = "a timing, not a test"]
    fn ambient_timing() {
        let cases: [(&str, &[ScreenSize], Option<Correction>); 3] = [
            ("DS", &[DS, DS], None),
            (
                "DS, 3D at 4x",
                &[ScreenSize::new(1024, 768), ScreenSize::new(1024, 768)],
                None,
            ),
            (
                "GBA, corrected",
                &[ScreenSize::new(240, 160)],
                Correction::for_system(emu_core::System::Gba),
            ),
        ];
        for (name, screens, correction) in cases {
            let frames: Vec<Frame> = (0..).zip(screens).map(|(i, &s)| noise(s, i)).collect();
            let placements = column_of(screens, Rotation::Upright, (300.0, 0.0, 2.0, 0.0));
            let span = span(&placements).expect("screens are placed");
            let size = picture_size(span);
            let correction = correction.as_ref();
            let old = crate::bench::best_of(200, || {
                sample_each_pixel(&frames, &placements, span, size, correction)
            });
            let new =
                crate::bench::best_of(200, || sample(&frames, &placements, span, size, correction));
            let whole = crate::bench::best_of(200, || picture(&frames, &placements, correction));
            println!("ambient {name}: sample {old:?} -> {new:?}; whole picture now {whole:?}");
        }
    }

    #[test]
    fn a_fade_jumps_to_a_picture_of_another_shape() {
        let wide = picture(&[filled(DS, 0)], &stacked(0.0, 1.0)[..1], None).expect("placed");
        let tall =
            picture(&[filled(DS, 0), filled(DS, 0)], &stacked(0.0, 1.0), None).expect("placed");
        let mut fade = Fade::new(wide, Instant::now());
        fade.aim(tall.clone());
        assert_eq!(fade.shown(), &tall);
    }
}
