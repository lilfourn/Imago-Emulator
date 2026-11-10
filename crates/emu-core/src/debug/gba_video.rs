//! The video memory formats the Game Boy Advance and the Nintendo DS's 2D
//! engines share, decoded for the graphics viewers: the DS kept the GBA's
//! tiles, text map entries and OAM attributes and added to them (GBATEK,
//! "LCD VRAM Character Data", "LCD VRAM BG Screen Data Format", "LCD OBJ -
//! OAM Attributes"; "DS Video BG Modes / Control" and "DS Video OBJs" say
//! what the DS changed).
//!
//! Only what is the same format on both consoles is here. Each core reads
//! its own memory and passes it in as closures, and keeps what differs:
//! where tiles and maps are, which palette colours them, and how a
//! sprite's tile number finds its texels.

use super::draw::set_pixel;
use crate::Frame;

/// One 8×8 tile's palette indices, row by row.
pub type Tile = [[u8; 8]; 8];

/// Which ways a tile or sprite is mirrored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flips {
    /// Left to right.
    pub x: bool,
    /// Top to bottom.
    pub y: bool,
}

impl Flips {
    /// The flips in a text map entry: bit 10 horizontal, bit 11 vertical
    /// (GBATEK, "LCD VRAM BG Screen Data Format").
    #[must_use]
    pub fn from_entry(entry: u16) -> Self {
        Self {
            x: entry & (1 << 10) != 0,
            y: entry & (1 << 11) != 0,
        }
    }
}

/// Palette index of texel (`x`, `y`) (each 0–7) of the tile at `address`.
/// 4bpp tiles are 32 bytes, two pixels per byte with the left one in the
/// low nibble; 8bpp tiles are 64 bytes, one per pixel (GBATEK, "LCD VRAM
/// Character Data"). `read` gives the byte at an offset.
pub fn tile_texel(
    read: &impl Fn(usize) -> u8,
    address: usize,
    eight_bit: bool,
    x: usize,
    y: usize,
) -> u8 {
    if eight_bit {
        read(address + y * 8 + x)
    } else {
        let byte = read(address + y * 4 + x / 2);
        if x.is_multiple_of(2) {
            byte & 0xF
        } else {
            byte >> 4
        }
    }
}

/// The whole tile at `address`, read through `read`.
pub fn read_tile(read: &impl Fn(usize) -> u8, address: usize, eight_bit: bool) -> Tile {
    std::array::from_fn(|y| std::array::from_fn(|x| tile_texel(read, address, eight_bit, x, y)))
}

/// Draws `tile` with its top-left corner at `corner`, mirrored as `flips`
/// says, painting each palette index with `paint`.
pub fn draw_tile(
    frame: &mut Frame,
    tile: &Tile,
    (left, top): (usize, usize),
    flips: Flips,
    paint: impl Fn(u8) -> u32,
) {
    for (row, indices) in tile.iter().enumerate() {
        let y = if flips.y { 7 - row } else { row };
        for (column, &index) in indices.iter().enumerate() {
            let x = if flips.x { 7 - column } else { column };
            set_pixel(frame, left + x, top + y, paint(index));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::draw::{BACKGROUND, blank};
    use super::*;

    fn pixel(frame: &Frame, x: usize, y: usize) -> u32 {
        frame.pixels()[y * usize::from(frame.size().width) + x]
    }

    #[test]
    fn four_bit_tiles_keep_the_left_pixel_in_the_low_nibble() {
        // Tile 1 (32 bytes on): row 2 starts with pixels 3 and 0xA.
        let mut memory = [0u8; 64];
        memory[32 + 2 * 4] = 0xA3;
        let read = |offset: usize| memory[offset];
        assert_eq!(tile_texel(&read, 32, false, 0, 2), 0x3);
        assert_eq!(tile_texel(&read, 32, false, 1, 2), 0xA);
        let tile = read_tile(&read, 32, false);
        assert_eq!(tile[2][..3], [0x3, 0xA, 0]);
    }

    #[test]
    fn eight_bit_tiles_are_a_byte_a_pixel() {
        let mut memory = [0u8; 128];
        memory[64 + 7 * 8 + 5] = 0xC8;
        let read = |offset: usize| memory[offset];
        assert_eq!(tile_texel(&read, 64, true, 5, 7), 0xC8);
        assert_eq!(read_tile(&read, 64, true)[7][5], 0xC8);
    }

    #[test]
    fn tiles_draw_mirrored_as_their_map_entry_says() {
        let mut tile: Tile = [[0; 8]; 8];
        tile[0][1] = 9;
        let paint = |index: u8| u32::from(index);
        let drawn = |entry: u16| {
            let mut frame = blank(16, 16);
            draw_tile(&mut frame, &tile, (8, 8), Flips::from_entry(entry), paint);
            frame
        };
        assert_eq!(pixel(&drawn(0), 9, 8), 9);
        assert_eq!(pixel(&drawn(1 << 10), 14, 8), 9, "left to right");
        assert_eq!(pixel(&drawn(1 << 11), 9, 15), 9, "top to bottom");
        assert_eq!(pixel(&drawn(3 << 10), 14, 15), 9, "both");
        assert_eq!(pixel(&drawn(0), 7, 7), BACKGROUND, "outside the tile");
    }
}
