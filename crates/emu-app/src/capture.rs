//! Pictures of the screens as the core drew them, for screenshots and
//! recordings: no colour correction, filter or rotation, and at the
//! core's own resolution (DS frames are larger when 3D is upscaled).

use emu_core::{Frame, ScreenSize};

/// The screens laid out as one picture: each player's screens top to
/// bottom (a DS's top screen over its touchscreen), and the players of a
/// linked session left to right. Screens divide evenly between `players`.
pub fn compose(screens: &[Frame], players: usize) -> Frame {
    let players = players.clamp(1, screens.len().max(1));
    let per_player = screens.len().div_ceil(players).max(1);
    let columns: Vec<&[Frame]> = screens.chunks(per_player).collect();
    let column_width = |column: &[Frame]| column.iter().map(|s| s.size().width).max().unwrap_or(0);
    let column_height = |column: &[Frame]| column.iter().map(|s| s.size().height).sum::<u16>();
    let width = columns.iter().map(|column| column_width(column)).sum();
    let height = columns
        .iter()
        .map(|column| column_height(column))
        .max()
        .unwrap_or(0);
    let mut picture = Frame::new(ScreenSize::new(width, height));
    let picture_width = usize::from(width);
    let mut left = 0;
    for column in columns {
        let mut top = 0;
        for screen in column {
            let screen_width = usize::from(screen.size().width);
            for (row, line) in screen.rows().enumerate() {
                let start = (top + row) * picture_width + left;
                picture.pixels_mut()[start..start + screen_width].copy_from_slice(line);
            }
            top += usize::from(screen.size().height);
        }
        left += usize::from(column_width(column));
    }
    picture
}

/// `frame` as a PNG file's bytes: 8-bit RGB.
pub fn encode_png(frame: &Frame) -> Vec<u8> {
    let size = frame.size();
    let rgb: Vec<u8> = frame
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let [blue, green, red, _] = pixel.to_le_bytes();
            [red, green, blue]
        })
        .collect();
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, u32::from(size.width), u32::from(size.height));
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&rgb))
        .expect("PNG encoding into memory cannot fail for a frame's own size");
    bytes
}

/// `frame` resized to `size` by repeating or dropping pixels, for frames
/// whose size changed partway through a recording.
pub fn resize(frame: &Frame, size: ScreenSize) -> Frame {
    let own = frame.size();
    if own == size {
        return frame.clone();
    }
    let (own_width, own_height) = (usize::from(own.width), usize::from(own.height));
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let columns: Vec<usize> = (0..width).map(|x| x * own_width / width.max(1)).collect();
    let mut out = Frame::new(size);
    for (y, line) in out.rows_mut().enumerate() {
        let start = y * own_height / height.max(1) * own_width;
        let source = &frame.pixels()[start..start + own_width];
        for (pixel, column) in line.iter_mut().zip(&columns) {
            *pixel = source[*column];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::rgb;

    fn filled(width: u16, height: u16, colour: u32) -> Frame {
        let mut frame = Frame::new(ScreenSize::new(width, height));
        frame.pixels_mut().fill(colour);
        frame
    }

    #[test]
    fn a_ds_stacks_its_screens() {
        let (top, bottom) = (rgb(1, 0, 0), rgb(0, 2, 0));
        let picture = compose(&[filled(4, 3, top), filled(4, 3, bottom)], 1);
        assert_eq!(picture.size(), ScreenSize::new(4, 6));
        assert!(picture.pixels()[..12].iter().all(|pixel| *pixel == top));
        assert!(picture.pixels()[12..].iter().all(|pixel| *pixel == bottom));
    }

    #[test]
    fn linked_players_sit_side_by_side() {
        let (one, two) = (rgb(1, 0, 0), rgb(0, 2, 0));
        let picture = compose(&[filled(2, 2, one), filled(3, 1, two)], 2);
        assert_eq!(picture.size(), ScreenSize::new(5, 2));
        let rows: Vec<&[u32]> = picture.rows().collect();
        assert_eq!(rows[0], [one, one, two, two, two]);
        // The shorter screen leaves black below it.
        assert_eq!(
            rows[1],
            [one, one, rgb(0, 0, 0), rgb(0, 0, 0), rgb(0, 0, 0)]
        );
    }

    #[test]
    fn screenshots_are_rgb_pngs_of_the_frame() {
        let mut frame = Frame::new(ScreenSize::new(2, 1));
        frame
            .pixels_mut()
            .copy_from_slice(&[rgb(0x12, 0x34, 0x56), rgb(0xFF, 0, 0x80)]);
        let bytes = encode_png(&frame);
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().expect("a PNG header");
        let mut decoded = vec![0; reader.output_buffer_size().expect("small image")];
        let info = reader.next_frame(&mut decoded).expect("one image");
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(&decoded[..6], [0x12, 0x34, 0x56, 0xFF, 0, 0x80]);
    }

    #[test]
    fn resizing_repeats_or_drops_pixels() {
        let mut frame = Frame::new(ScreenSize::new(2, 1));
        frame
            .pixels_mut()
            .copy_from_slice(&[rgb(1, 0, 0), rgb(2, 0, 0)]);
        let larger = resize(&frame, ScreenSize::new(4, 2));
        let row = [rgb(1, 0, 0), rgb(1, 0, 0), rgb(2, 0, 0), rgb(2, 0, 0)];
        assert_eq!(larger.pixels(), [row, row].concat());
        let smaller = resize(&larger, ScreenSize::new(2, 1));
        assert_eq!(smaller, frame);
    }
}
