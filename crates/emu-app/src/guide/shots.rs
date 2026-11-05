//! Screenshots for Guide: the screens as the console drew them, with
//! nothing over them, sized for the model to read.

use emu_core::{Frame, ScreenSize};

/// The longest side a screenshot is scaled up towards, in pixels: DS
/// screens stacked come out at 512 × 768, small enough to send quickly and
/// large enough for the model to read pixel text.
const TARGET: usize = 800;
/// The most a screenshot is enlarged.
const MOST: usize = 3;
/// The longest side a screenshot may keep; larger frames, from 3D drawn
/// at many times its resolution, are shrunk.
const LARGEST: usize = 1600;

/// `frame` enlarged by a whole factor, nearest neighbour, until its
/// longest side nears [`TARGET`], or shrunk by a whole factor, averaging,
/// if it is longer than [`LARGEST`].
pub fn scale_for_model(frame: &Frame) -> Frame {
    let size = frame.size();
    let longest = usize::from(size.width.max(size.height)).max(1);
    if longest > LARGEST {
        return shrink(frame, longest.div_ceil(LARGEST));
    }
    let factor = (TARGET / longest).clamp(1, MOST);
    if factor == 1 {
        return frame.clone();
    }
    let width = usize::from(size.width);
    let height = usize::from(size.height);
    let mut scaled = Frame::new(ScreenSize::new(
        u16::try_from(width * factor).unwrap_or(u16::MAX),
        u16::try_from(height * factor).unwrap_or(u16::MAX),
    ));
    let source = frame.pixels();
    for (row, line) in scaled.rows_mut().enumerate() {
        let from = &source[(row / factor) * width..][..width];
        for (column, pixel) in line.iter_mut().enumerate() {
            *pixel = from[column / factor];
        }
    }
    scaled
}

/// `frame` shrunk `by` times, each pixel the average of the block it
/// covers.
fn shrink(frame: &Frame, by: usize) -> Frame {
    let size = frame.size();
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let (new_width, new_height) = ((width / by).max(1), (height / by).max(1));
    let mut shrunk = Frame::new(ScreenSize::new(
        u16::try_from(new_width).unwrap_or(u16::MAX),
        u16::try_from(new_height).unwrap_or(u16::MAX),
    ));
    let source = frame.pixels();
    for (row, line) in shrunk.rows_mut().enumerate() {
        for (column, pixel) in line.iter_mut().enumerate() {
            let mut sums = [0u32; 3];
            let mut count = 0;
            for y in row * by..((row + 1) * by).min(height) {
                for x in column * by..((column + 1) * by).min(width) {
                    let [blue, green, red, _] = source[y * width + x].to_le_bytes();
                    sums[0] += u32::from(blue);
                    sums[1] += u32::from(green);
                    sums[2] += u32::from(red);
                    count += 1;
                }
            }
            let average = |sum: u32| u8::try_from(sum / count.max(1)).unwrap_or(u8::MAX);
            *pixel =
                u32::from_le_bytes([average(sums[0]), average(sums[1]), average(sums[2]), 0xFF]);
        }
    }
    shrunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture;

    fn compose(frames: &[Frame], players: usize) -> Frame {
        scale_for_model(&capture::compose(frames, players))
    }

    fn frame(width: u16, height: u16) -> Frame {
        let mut frame = Frame::new(ScreenSize::new(width, height));
        for (index, pixel) in frame.pixels_mut().iter_mut().enumerate() {
            *pixel = u32::try_from(index).unwrap();
        }
        frame
    }

    #[test]
    fn handheld_screens_are_enlarged_by_a_whole_factor() {
        let ds = [frame(256, 192), frame(256, 192)];
        assert_eq!(compose(&ds, 1).size(), ScreenSize::new(512, 768));
        let three_ds = [frame(400, 240), frame(320, 240)];
        assert_eq!(compose(&three_ds, 1).size(), ScreenSize::new(400, 480));
        assert_eq!(
            compose(&[frame(240, 160)], 1).size(),
            ScreenSize::new(720, 480)
        );
        assert_eq!(
            compose(&[frame(160, 144)], 1).size(),
            ScreenSize::new(480, 432)
        );
    }

    #[test]
    fn enlarging_repeats_each_pixel() {
        let small = frame(2, 1);
        let scaled = scale_for_model(&small);
        assert_eq!(scaled.size(), ScreenSize::new(6, 3));
        assert_eq!(&scaled.pixels()[..6], &[0, 0, 0, 1, 1, 1]);
    }

    #[test]
    fn huge_frames_are_shrunk() {
        let huge = [frame(2048, 1536), frame(2048, 1536)];
        let shot = compose(&huge, 1);
        assert!(
            usize::from(shot.size().height) <= LARGEST,
            "{:?}",
            shot.size()
        );
        assert_eq!(shot.size(), ScreenSize::new(1024, 1536));
    }
}
