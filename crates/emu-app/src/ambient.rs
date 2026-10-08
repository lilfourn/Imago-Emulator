//! The backdrop behind the screens: a soft glow of the game's own colours
//! filling the margins the screens' proportions leave in the window.
//!
//! The screens are sampled, as the window arranges them, into a picture a
//! few dozen pixels across, which is blurred and dimmed. Drawn stretched
//! over the whole window, GPUI's linear filtering smooths it the rest of
//! the way; the screens themselves hide the middle of it.

use emu_core::{Frame, ScreenSize, rgb};

use crate::color::Correction;
use crate::layout::{Placement, Rect};

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

/// Samples averaged across and down each picture pixel, so small moving
/// sprites do not make it flicker.
const SAMPLES: u16 = 4;

/// Reach of each box blur pass, in picture pixels.
const BLUR_RADIUS: usize = 3;

/// Box blur passes each way; three come close to a Gaussian blur.
const BLUR_PASSES: usize = 3;

/// Brightness kept, in 256ths, so the glow stays behind the screens.
const BRIGHTNESS: u32 = 112;

/// Red, green and blue.
type Color = [u32; 3];

/// The backdrop for `frames` drawn at `placements`: the area the
/// placements span, shrunk, blurred and dimmed, its colours corrected with
/// `correction` if given. Gaps between screens take the nearest screen's
/// colours. `None` if nothing is placed.
pub fn picture(
    frames: &[Frame],
    placements: &[Placement],
    correction: Option<&Correction>,
) -> Option<Frame> {
    let span = span(placements)?;
    let size = picture_size(span);
    let mut colors = sample(frames, placements, span, size, correction);
    for _ in 0..BLUR_PASSES {
        colors = blur(&colors, size, (1, 0));
        colors = blur(&colors, size, (0, 1));
    }
    let mut picture = Frame::new(size);
    for (pixel, color) in picture.pixels_mut().iter_mut().zip(&colors) {
        *pixel = dim(*color);
    }
    Some(picture)
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

/// Each picture pixel's average colour over the part of `span` it covers.
fn sample(
    frames: &[Frame],
    placements: &[Placement],
    span: Rect,
    size: ScreenSize,
    correction: Option<&Correction>,
) -> Vec<Color> {
    let step = (
        span.width / f32::from(size.width * SAMPLES),
        span.height / f32::from(size.height * SAMPLES),
    );
    let mut colors = Vec::with_capacity(size.pixel_count());
    for row in 0..size.height {
        for column in 0..size.width {
            let mut sum = [0; 3];
            let mut count = 0;
            for (dx, dy) in (0..SAMPLES).flat_map(|dy| (0..SAMPLES).map(move |dx| (dx, dy))) {
                let point = (
                    span.x + (f32::from(column * SAMPLES + dx) + 0.5) * step.0,
                    span.y + (f32::from(row * SAMPLES + dy) + 0.5) * step.1,
                );
                let Some(pixel) = pixel_near(frames, placements, point) else {
                    continue;
                };
                let pixel = correction.map_or(pixel, |correction| correction.pixel(pixel));
                for (channel, shift) in sum.iter_mut().zip([16, 8, 0]) {
                    *channel += (pixel >> shift) & 0xFF;
                }
                count += 1;
            }
            colors.push(sum.map(|channel| channel / count.max(1)));
        }
    }
    colors
}

/// The pixel shown at window `point`, or, between screens, at the nearest
/// point of the nearest screen.
fn pixel_near(frames: &[Frame], placements: &[Placement], point: (f32, f32)) -> Option<u32> {
    let placement = placements
        .iter()
        .min_by(|a, b| distance(a.rect, point).total_cmp(&distance(b.rect, point)))?;
    let rect = placement.rect;
    // Just inside the far edges, which belong to the next pixel over.
    let inside = (
        point.0.clamp(rect.x, rect.x + rect.width * 0.999),
        point.1.clamp(rect.y, rect.y + rect.height * 0.999),
    );
    let frame = frames.get(placement.screen)?;
    let at = placement.pixel_at(frame.size(), inside)?;
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

/// `colors`, a `size` picture, with each pixel averaged with its
/// [`BLUR_RADIUS`] neighbours each way along `axis`, `(1, 0)` across or
/// `(0, 1)` down. Past the edges the edge pixel repeats.
fn blur(colors: &[Color], size: ScreenSize, axis: (usize, usize)) -> Vec<Color> {
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let taps = 2 * BLUR_RADIUS + 1;
    let mut out = Vec::with_capacity(colors.len());
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0; 3];
            for offset in 0..taps {
                // Steps from -BLUR_RADIUS to +BLUR_RADIUS, held at the edges.
                let along = |position: usize, length: usize, on: usize| {
                    (position + offset * on)
                        .saturating_sub(BLUR_RADIUS * on)
                        .min(length - 1)
                };
                let index = along(y, height, axis.1) * width + along(x, width, axis.0);
                for (channel, value) in sum.iter_mut().zip(colors[index]) {
                    *channel += value;
                }
            }
            // `taps` is a small constant, so the cast cannot truncate.
            out.push(sum.map(|channel| channel / taps as u32));
        }
    }
    out
}

/// `color` at [`BRIGHTNESS`], as an opaque pixel.
fn dim(color: Color) -> u32 {
    // Each channel is at most 255 and stays so once dimmed.
    let [red, green, blue] = color.map(|channel| (channel * BRIGHTNESS / 256) as u8);
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

    fn channels(pixel: u32) -> [u32; 3] {
        [16, 8, 0].map(|shift| (pixel >> shift) & 0xFF)
    }

    #[test]
    fn backdrops_are_named_in_the_settings_file() {
        for backdrop in [Backdrop::Solid, Backdrop::Ambient] {
            assert_eq!(Backdrop::from_name(backdrop.name()), Some(backdrop));
        }
        assert_eq!(Backdrop::from_name("boxart"), None);
    }

    #[test]
    fn nothing_placed_has_no_picture() {
        assert_eq!(picture(&[], &[], None), None);
    }

    #[test]
    fn a_stacked_ds_gives_a_tall_picture_of_its_colours_dimmed() {
        let frames = [filled(DS, rgb(200, 100, 0)), filled(DS, rgb(200, 100, 0))];
        let placements = [
            placement(0, 100.0, 0.0, 256.0, 192.0),
            placement(1, 100.0, 192.0, 256.0, 192.0),
        ];
        let picture = picture(&frames, &placements, None).expect("screens are placed");
        assert_eq!(picture.size(), ScreenSize::new(32, 48));
        let dimmed = rgb(87, 43, 0);
        assert!(picture.pixels().iter().all(|&pixel| pixel == dimmed));
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
        let pixels = picture.pixels();
        let [top_red, _, top_blue] = channels(pixels[0]);
        let [bottom_red, _, bottom_blue] = channels(pixels[pixels.len() - 1]);
        assert!(top_red > 100 && top_blue == 0, "{top_red} {top_blue}");
        assert!(
            bottom_blue > 100 && bottom_red == 0,
            "{bottom_red} {bottom_blue}"
        );
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
        let pixels = picture.pixels();
        assert_eq!(channels(pixels[0])[2], 0);
        assert_eq!(channels(pixels[pixels.len() - 1])[0], 0);
    }
}
