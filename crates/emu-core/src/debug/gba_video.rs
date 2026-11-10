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

use super::draw::{blank, fill_rect, set_pixel};
use crate::Frame;

/// Bytes the tile picture shows.
pub const TILE_BLOCK: usize = 0x8000;
/// Tiles per row in the tile picture.
const TILES_WIDE: usize = 32;

/// The first OBJ colour in palette RAM, by colour number: BG colours are
/// 0–255, OBJ colours 256–511 (each of the DS's engines has its own 512).
pub const OBJ_PALETTE: usize = 256;
/// Width and height of one colour swatch in the palette picture.
const SWATCH: usize = 8;
/// Gap between the BG and OBJ halves of the palette picture.
const PALETTE_GAP: usize = 8;

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

/// The tile picture: the [`TILE_BLOCK`] bytes `read` gives, as tiles 32 to
/// a row, 1024 4bpp tiles (256×256) or 512 8bpp tiles (256×128), each
/// palette index painted by `paint`. Returns the picture and how many
/// tiles it shows.
pub fn tile_sheet(
    read: impl Fn(usize) -> u8,
    eight_bit: bool,
    paint: impl Fn(u8) -> u32,
) -> (Frame, usize) {
    let tile_bytes = if eight_bit { 64 } else { 32 };
    let count = TILE_BLOCK / tile_bytes;
    let mut frame = blank(TILES_WIDE * 8, count / TILES_WIDE * 8);
    for tile in 0..count {
        let indices = read_tile(&read, tile * tile_bytes, eight_bit);
        let corner = (tile % TILES_WIDE * 8, tile / TILES_WIDE * 8);
        draw_tile(&mut frame, &indices, corner, Flips::default(), &paint);
    }
    (frame, count)
}

/// The palette picture: 512 colours, one 8×8 swatch each, the 256 BG
/// colours on the left and the 256 OBJ colours on the right, each a 16×16
/// grid with one 16-colour palette per row. `color` gives the pixel of a
/// colour number (0–255 BG, 256–511 OBJ).
pub fn palette_sheet(color: impl Fn(usize) -> u32) -> Frame {
    let half_width = 16 * SWATCH;
    let mut frame = blank(2 * half_width + PALETTE_GAP, 16 * SWATCH);
    for index in 0..2 * OBJ_PALETTE {
        let (half, entry) = (index / OBJ_PALETTE, index % OBJ_PALETTE);
        let left = half * (half_width + PALETTE_GAP) + entry % 16 * SWATCH;
        let top = entry / 16 * SWATCH;
        fill_rect(&mut frame, (left, top), (SWATCH, SWATCH), color(index));
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::super::draw::BACKGROUND;
    use super::*;
    use crate::ScreenSize;

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

    #[test]
    fn the_tile_picture_is_32_tiles_wide_at_either_depth() {
        // Tile 33 is the second tile of the second row.
        let mut memory = vec![0u8; TILE_BLOCK];
        memory[33 * 32] = 0x02;
        memory[33 * 64] |= 0x40;
        let read = |offset: usize| memory[offset];
        let paint = |index: u8| u32::from(index);

        let (four_bit, count) = tile_sheet(read, false, paint);
        assert_eq!(count, 1024);
        assert_eq!(four_bit.size(), ScreenSize::new(256, 256));
        assert_eq!(pixel(&four_bit, 8, 8), 2);

        let (eight_bit, count) = tile_sheet(read, true, paint);
        assert_eq!(count, 512);
        assert_eq!(eight_bit.size(), ScreenSize::new(256, 128));
        assert_eq!(pixel(&eight_bit, 8, 8), 0x40);
    }

    #[test]
    fn the_palette_picture_has_bg_colours_left_and_obj_colours_right() {
        let frame = palette_sheet(|index| index as u32);
        assert_eq!(frame.size(), ScreenSize::new(264, 128));
        assert_eq!(pixel(&frame, 0, 0), 0);
        assert_eq!(pixel(&frame, 8, 8), 17, "BG palette 1, colour 1");
        assert_eq!(pixel(&frame, 127, 127), 255);
        assert_eq!(pixel(&frame, 130, 0), BACKGROUND, "the gap");
        assert_eq!(pixel(&frame, 136, 0), 256, "the first OBJ colour");
        assert_eq!(pixel(&frame, 263, 127), 511);
    }
}
