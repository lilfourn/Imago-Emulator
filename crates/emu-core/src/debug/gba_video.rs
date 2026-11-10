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

use super::draw::{BACKGROUND, blank, fill_checker, fill_rect, set_pixel};
use crate::{Frame, rgb};

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

/// Sprite sizes in pixels by shape (square, wide, tall) and size field
/// (GBATEK, "LCD OBJ - OAM Attributes").
const OBJ_SIZES: [[(usize, usize); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];
/// Entries in OAM (in each engine's, on the DS).
pub const SPRITES: usize = 128;
/// Cells per row in the sprite picture.
const SPRITE_COLUMNS: usize = 16;
/// A sprite cell's width and height: room for the largest sprite.
const CELL: usize = 64;
/// Sprite cells alternate between these, so cells stand apart without
/// gaps.
pub const CELL_SHADES: [u32; 2] = [BACKGROUND, rgb(0x2C, 0x2C, 0x2C)];

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

/// One OAM entry's three attribute halfwords (GBATEK, "LCD OBJ - OAM
/// Attributes"; the DS's are the same but for what mode 3, the tile number
/// and the palette field mean, "DS Video OBJs").
#[derive(Debug, Clone, Copy)]
pub struct Sprite {
    attr0: u16,
    attr1: u16,
    attr2: u16,
}

impl Sprite {
    /// OAM entry `index` (0–127), its halfwords read through `read_oam`
    /// by their offset in OAM. The fourth halfword of each entry is an
    /// affine parameter, which drawing unrotated does not need.
    pub fn read(read_oam: impl Fn(u32) -> u16, index: usize) -> Self {
        let base = index as u32 * 8;
        Self {
            attr0: read_oam(base),
            attr1: read_oam(base + 2),
            attr2: read_oam(base + 4),
        }
    }

    /// Rotation/scaling is on (attribute 0 bit 8).
    #[must_use]
    pub fn affine(self) -> bool {
        self.attr0 & (1 << 8) != 0
    }

    /// Not drawn at all: not affine, with attribute 0 bit 9 set.
    #[must_use]
    pub fn disabled(self) -> bool {
        !self.affine() && self.attr0 & (1 << 9) != 0
    }

    /// An affine sprite's drawing area is twice its size (attribute 0 bit
    /// 9).
    #[must_use]
    pub fn double_size(self) -> bool {
        self.affine() && self.attr0 & (1 << 9) != 0
    }

    /// Attribute 0 bits 10–11: 0 normal, 1 semi-transparent, 2 OBJ window;
    /// 3 is prohibited on the GBA and a bitmap sprite on the DS.
    #[must_use]
    pub fn mode(self) -> usize {
        usize::from((self.attr0 >> 10) & 3)
    }

    /// Mosaic applies (attribute 0 bit 12).
    #[must_use]
    pub fn mosaic(self) -> bool {
        self.attr0 & (1 << 12) != 0
    }

    /// 256-colour tiles (attribute 0 bit 13).
    #[must_use]
    pub fn eight_bit(self) -> bool {
        self.attr0 & (1 << 13) != 0
    }

    /// Attribute 0 bits 14–15: 0 square, 1 wide, 2 tall, 3 prohibited.
    #[must_use]
    pub fn shape(self) -> usize {
        usize::from(self.attr0 >> 14)
    }

    /// Width and height in pixels, from the shape and the size field
    /// (attribute 1 bits 14–15); `None` for the prohibited shape.
    #[must_use]
    pub fn size(self) -> Option<(usize, usize)> {
        let sizes = OBJ_SIZES.get(self.shape())?;
        Some(sizes[usize::from(self.attr1 >> 14)])
    }

    /// Screen x: attribute 1 bits 0–8, signed.
    #[must_use]
    pub fn x(self) -> i32 {
        let x = i32::from(self.attr1 & 0x1FF);
        if x >= 256 { x - 512 } else { x }
    }

    /// Attribute 0 bits 0–7, the screen y as stored. The consoles take it
    /// differently: the GBA's renderer puts values from 160 on above the
    /// screen, the DS's counts rows within 256 lines.
    #[must_use]
    pub fn y(self) -> u16 {
        self.attr0 & 0xFF
    }

    /// Mirrored left to right (attribute 1 bit 12, without rotation).
    #[must_use]
    pub fn h_flip(self) -> bool {
        !self.affine() && self.attr1 & (1 << 12) != 0
    }

    /// Mirrored top to bottom (attribute 1 bit 13, without rotation).
    #[must_use]
    pub fn v_flip(self) -> bool {
        !self.affine() && self.attr1 & (1 << 13) != 0
    }

    /// Both flips.
    #[must_use]
    pub fn flips(self) -> Flips {
        Flips {
            x: self.h_flip(),
            y: self.v_flip(),
        }
    }

    /// Which of the 32 affine parameter groups it uses (attribute 1 bits
    /// 9–13).
    #[must_use]
    pub fn affine_index(self) -> u16 {
        (self.attr1 >> 9) & 0x1F
    }

    /// Its tile number (attribute 2 bits 0–9): in 32-byte units on the
    /// GBA, in units DISPCNT sets on the DS.
    #[must_use]
    pub fn tile(self) -> u16 {
        self.attr2 & 0x3FF
    }

    /// Priority against backgrounds (attribute 2 bits 10–11).
    #[must_use]
    pub fn priority(self) -> u16 {
        (self.attr2 >> 10) & 3
    }

    /// Attribute 2 bits 12–15: the 16-colour OBJ palette; on the DS also
    /// the extended OBJ palette, or a bitmap sprite's alpha.
    #[must_use]
    pub fn palette(self) -> u16 {
        self.attr2 >> 12
    }

    /// One line about OAM entry `index`: position, size and shape, tile,
    /// colours, priority, mode, rotation or flips, and mosaic, then each
    /// of `more` that is set. The caller gives what the consoles tell
    /// differently: `y`, the screen row as its renderer takes
    /// [`y`](Self::y); `colors`, what colours the texels; and `mode_3`,
    /// its name for mode 3.
    #[must_use]
    pub fn note(
        self,
        index: usize,
        y: i32,
        colors: &str,
        mode_3: &str,
        more: &[(bool, &str)],
    ) -> String {
        let Some((width, height)) = self.size() else {
            return format!("{index:3}: prohibited shape 3, not drawn");
        };
        let shape = ["square", "wide", "tall"][self.shape()];
        let mode = ["normal", "semi-transparent", "window", mode_3][self.mode()];
        let mut note = format!(
            "{index:3}: x {}, y {y}, {width}×{height} {shape}, tile 0x{:03X}, {colors}, priority {}, {mode}",
            self.x(),
            self.tile(),
            self.priority()
        );
        if self.affine() {
            note += &format!(", affine {}", self.affine_index());
        }
        let flags = [
            (self.double_size(), "double size"),
            (self.h_flip(), "h-flip"),
            (self.v_flip(), "v-flip"),
            (self.mosaic(), "mosaic"),
        ];
        for (_, flag) in flags.iter().chain(more).filter(|(set, _)| *set) {
            note += ", ";
            note += flag;
        }
        note
    }
}

/// The sprite picture before any sprite is drawn: a grid of 16 columns
/// and 8 rows of 64×64 cells, one per OAM entry, shaded alternately.
#[must_use]
pub fn sprite_sheet() -> Frame {
    let rows = SPRITES / SPRITE_COLUMNS;
    let mut frame = blank(SPRITE_COLUMNS * CELL, rows * CELL);
    for index in 0..SPRITES {
        let (column, row) = (index % SPRITE_COLUMNS, index / SPRITE_COLUMNS);
        let shade = CELL_SHADES[(column + row) % 2];
        fill_rect(&mut frame, (column * CELL, row * CELL), (CELL, CELL), shade);
    }
    frame
}

/// Draws OAM entry `index`, `size` pixels, unrotated at its cell's
/// top-left in a [`sprite_sheet`]: a checkerboard its size, which shows
/// through transparent texels, then the texels, mirrored as `flips` says.
/// `texel` gives the colour of a texel of the sprite, `None` where it is
/// transparent.
pub fn draw_sprite(
    frame: &mut Frame,
    index: usize,
    (width, height): (usize, usize),
    flips: Flips,
    texel: impl Fn(usize, usize) -> Option<u32>,
) {
    let (left, top) = (index % SPRITE_COLUMNS * CELL, index / SPRITE_COLUMNS * CELL);
    fill_checker(frame, (left, top), (width, height));
    for y in 0..height {
        let texel_y = if flips.y { height - 1 - y } else { y };
        for x in 0..width {
            let texel_x = if flips.x { width - 1 - x } else { x };
            if let Some(pixel) = texel(texel_x, texel_y) {
                set_pixel(frame, left + x, top + y, pixel);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::draw::CHECKER;
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

    /// A sprite from its three attribute halfwords.
    fn sprite(attributes: [u16; 3]) -> Sprite {
        // Read as entry 5, so the offsets are checked too.
        Sprite::read(|offset| attributes[(offset as usize - 5 * 8) / 2], 5)
    }

    #[test]
    fn oam_attributes_decode_as_the_documentation_says() {
        // 32×16 at (-16, 200): wide, size 2; 8bpp, mosaic, mode 1; tile
        // 0x155, priority 2, palette 9; flipped both ways.
        let plain = sprite([
            200 | 1 << 10 | 1 << 12 | 1 << 13 | 1 << 14,
            0x1F0 | 1 << 12 | 1 << 13 | 2 << 14,
            0x155 | 2 << 10 | 9 << 12,
        ]);
        assert_eq!(plain.size(), Some((32, 16)));
        assert_eq!((plain.x(), plain.y()), (-16, 200));
        assert_eq!((plain.shape(), plain.mode()), (1, 1));
        assert!(plain.eight_bit() && plain.mosaic());
        assert!(!plain.affine() && !plain.disabled() && !plain.double_size());
        assert_eq!(plain.flips(), Flips { x: true, y: true });
        assert_eq!(
            (plain.tile(), plain.priority(), plain.palette()),
            (0x155, 2, 9)
        );

        // Bit 9 disables a plain sprite and doubles an affine one, whose
        // attribute 1 bits 9–13 are its parameter group, not flips.
        assert!(sprite([1 << 9, 0, 0]).disabled());
        let affine = sprite([1 << 8 | 1 << 9, 21 << 9, 0]);
        assert!(affine.affine() && affine.double_size() && !affine.disabled());
        assert_eq!(affine.affine_index(), 21);
        assert_eq!(affine.flips(), Flips::default());

        assert_eq!(sprite([3 << 14, 0, 0]).size(), None, "prohibited");
    }

    #[test]
    fn a_sprites_note_lists_what_it_is_and_what_the_console_adds() {
        let entry = sprite([20 | 1 << 14, 0x1F0 | 1 << 12, 2 | 1 << 10 | 4 << 12]);
        assert_eq!(
            entry.note(17, 20, "palette 4", "prohibited mode 3", &[]),
            " 17: x -16, y 20, 16×8 wide, tile 0x002, palette 4, priority 1, normal, h-flip"
        );
        let third = sprite([3 << 10 | 1 << 8, 5 << 9, 0]);
        assert_eq!(
            third.note(
                3,
                -1,
                "alpha 0",
                "bitmap",
                &[(false, "left out"), (true, "hidden")]
            ),
            "  3: x 0, y -1, 8×8 square, tile 0x000, alpha 0, priority 0, bitmap, affine 5, hidden"
        );
        assert_eq!(
            sprite([3 << 14, 0, 0]).note(0, 0, "", "", &[]),
            "  0: prohibited shape 3, not drawn"
        );
    }

    #[test]
    fn sprites_draw_in_their_own_cells_over_a_checkerboard() {
        let mut frame = sprite_sheet();
        assert_eq!(frame.size(), ScreenSize::new(1024, 512));
        assert_eq!(pixel(&frame, 0, 0), CELL_SHADES[0]);
        assert_eq!(pixel(&frame, 64, 0), CELL_SHADES[1]);
        assert_eq!(pixel(&frame, 64, 64), CELL_SHADES[0]);

        // Entry 17 is column 1, row 1. Only texel (0, 0) is opaque.
        let texel = |x: usize, y: usize| (x == 0 && y == 0).then_some(7);
        let flips = Flips { x: true, y: false };
        draw_sprite(&mut frame, 17, (16, 8), flips, texel);
        assert_eq!(pixel(&frame, 64 + 15, 64), 7, "mirrored");
        assert_eq!(pixel(&frame, 64, 64), CHECKER[0], "transparent");
        assert_eq!(pixel(&frame, 64 + 16, 64), CELL_SHADES[0], "past its width");
        assert_eq!(pixel(&frame, 64, 64 + 8), CELL_SHADES[0], "past its height");
    }
}
