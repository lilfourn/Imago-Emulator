//! Pictures of video memory for the debugger's graphics viewers: tiles, a
//! background's whole map, the sprites in OAM and palette RAM (GBATEK, "LCD
//! VRAM Overview", "LCD VRAM Character Data", "LCD VRAM BG Screen Data
//! Format", "LCD VRAM Bitmap BG Modes", "LCD OBJ - OAM Attributes", "LCD OBJ
//! - VRAM Character (Tile) Mapping" and "LCD Color Palettes").
//!
//! [`draw`] reads the PPU without changing it and returns plain data: a
//! [`Frame`] and lines of notes. Everything is decoded here from VRAM, OAM,
//! palette RAM and the registers, the way the renderer decodes them.

use emu_core::debug::{Graphics, GraphicsRequest, GraphicsView};
use emu_core::{Frame, ScreenSize, rgb};

use crate::ppu::{Ppu, color};

/// Fills the parts of a picture that show nothing.
const BACKGROUND: u32 = rgb(0x20, 0x20, 0x20);
/// The visible screen's outline on a map: magenta stands out from most
/// game palettes.
const OUTLINE: u32 = rgb(0xFF, 0x00, 0xFF);
/// The two squares of the checkerboard behind transparent sprite pixels.
const CHECKER: [u32; 2] = [rgb(0x40, 0x40, 0x40), rgb(0x58, 0x58, 0x58)];
/// Sprite cells alternate between these, so cells stand apart without
/// gaps.
const CELL_SHADES: [u32; 2] = [BACKGROUND, rgb(0x2C, 0x2C, 0x2C)];

/// Where VRAM starts on the CPU's bus.
const VRAM_ADDRESS: usize = 0x0600_0000;
/// Where palette RAM starts on the CPU's bus.
const PALETTE_ADDRESS: usize = 0x0500_0000;
/// The first OBJ colour in palette RAM, by colour number: BG colours are
/// 0–255, OBJ colours 256–511.
const OBJ_PALETTE: usize = 256;
/// Where OBJ tiles start in VRAM.
const OBJ_TILES: usize = 0x1_0000;

/// Screen width in pixels.
const SCREEN_WIDTH: usize = 240;
/// Screen height in pixels.
const SCREEN_HEIGHT: usize = 160;

/// Bytes the tile picture shows.
const TILE_BLOCK: usize = 0x8000;
/// Tiles per row in the tile picture.
const TILES_WIDE: usize = 32;

/// Sprite sizes in pixels by shape (square, wide, tall) and size field
/// (GBATEK, "LCD OBJ - OAM Attributes").
const OBJ_SIZES: [[(usize, usize); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];
/// Entries in OAM.
const SPRITES: usize = 128;
/// Cells per row in the sprite picture.
const SPRITE_COLUMNS: usize = 16;
/// A sprite cell's width and height: room for the largest sprite.
const CELL: usize = 64;

/// Width and height of one colour swatch in the palette picture.
const SWATCH: usize = 8;
/// Gap between the BG and OBJ halves of the palette picture.
const PALETTE_GAP: usize = 8;

/// Draws `request`'s view of `ppu`'s video memory. The GBA can draw every
/// view, so this always returns a picture.
pub(crate) fn draw(ppu: &Ppu, request: &GraphicsRequest) -> Option<Graphics> {
    Some(match request.view {
        GraphicsView::Tiles => tiles(ppu, request),
        GraphicsView::Map => map(ppu, request.layer),
        GraphicsView::Sprites => sprites(ppu),
        GraphicsView::Palettes => palettes(ppu),
    })
}

// ----- Views -----------------------------------------------------------

/// 32 KiB of tiles from `request.base`, 32 per row: 1024 4bpp tiles
/// (256×256) or 512 8bpp tiles (256×128), coloured as [`tile_palette`]
/// says.
fn tiles(ppu: &Ppu, request: &GraphicsRequest) -> Graphics {
    let start = mirror(request.base as usize) & !31;
    let eight_bit = request.eight_bit;
    let tile_bytes = if eight_bit { 64 } else { 32 };
    let count = TILE_BLOCK / tile_bytes;
    let mut frame = blank(TILES_WIDE * 8, count / TILES_WIDE * 8);
    let (first_color, palette_name) = tile_palette(request.palette, eight_bit, start >= OBJ_TILES);
    let read = |offset: usize| ppu.vram()[mirror(start + offset)];
    for tile in 0..count {
        let indices = read_tile(&read, tile * tile_bytes, eight_bit);
        let corner = (tile % TILES_WIDE * 8, tile / TILES_WIDE * 8);
        draw_tile(&mut frame, &indices, corner, Flips::default(), |index| {
            palette_color(ppu, first_color + usize::from(index))
        });
    }
    let bpp = if eight_bit { "8bpp" } else { "4bpp" };
    Graphics {
        image: frame,
        notes: vec![
            format!(
                "Tiles at 0x{:08X}: {count} {bpp} tiles, 32 per row, coloured with {palette_name}",
                VRAM_ADDRESS + start
            ),
            "Addresses mirror as on hardware: VRAM repeats every 128 KiB, and its last \
             32 KiB (0x06010000) shows again at 0x06018000"
                .to_string(),
        ],
    }
}

/// Background `layer` (0–3; larger numbers mean 3) as DISPCNT's mode and
/// its BGxCNT set it up: a text map, an affine map, the bitmap, or just a
/// note when the mode has no such background.
fn map(ppu: &Ppu, layer: u8) -> Graphics {
    let layer = usize::from(layer.min(3));
    let dispcnt = ppu.raw_register(0x00);
    let mode = dispcnt & 7;
    let background = Background {
        layer,
        control: ppu.raw_register(0x08 + layer * 2),
        shown: dispcnt & (0x100 << layer) != 0,
    };
    match bg_kind(mode, layer) {
        Some(BgKind::Text) => text_map(ppu, &background),
        Some(BgKind::Affine) => affine_map(ppu, &background),
        Some(BgKind::Bitmap) => bitmap(ppu, &background, dispcnt),
        None => Graphics {
            image: blank(8, 8),
            notes: vec![format!("BG{layer} is not used in mode {mode}")],
        },
    }
}

/// Every OAM entry in a grid of 16 columns and 8 rows of 64×64 cells,
/// drawn unrotated at the cell's top-left over a checkerboard that shows
/// through transparent pixels, with one note per sprite that is not
/// disabled.
fn sprites(ppu: &Ppu) -> Graphics {
    let dispcnt = ppu.raw_register(0x00);
    let one_dimensional = dispcnt & (1 << 6) != 0;
    let bitmap_mode = (3..=5).contains(&(dispcnt & 7));
    let rows = SPRITES / SPRITE_COLUMNS;
    let mut frame = blank(SPRITE_COLUMNS * CELL, rows * CELL);
    let mapping = if one_dimensional { "1D" } else { "2D" };
    let shown = if dispcnt & (1 << 12) != 0 {
        "on"
    } else {
        "off"
    };
    let mut notes = vec![format!(
        "128 sprites, {mapping} tile mapping (DISPCNT bit 6), objects {shown} (DISPCNT bit 12); \
         affine sprites are drawn unrotated"
    )];
    for index in 0..SPRITES {
        let (column, row) = (index % SPRITE_COLUMNS, index / SPRITE_COLUMNS);
        let corner = (column * CELL, row * CELL);
        fill_rect(
            &mut frame,
            corner,
            (CELL, CELL),
            CELL_SHADES[(column + row) % 2],
        );
        let sprite = Sprite::read(ppu, index);
        if sprite.disabled() {
            continue;
        }
        // In bitmap modes the bitmap fills the first 16 KiB of OBJ VRAM,
        // so the PPU skips sprites using tiles there.
        let hidden = bitmap_mode && sprite.tile() < 512;
        if let Some(size) = sprite.size()
            && !hidden
            && sprite.mode() != 3
        {
            fill_checker(&mut frame, corner, size);
            draw_sprite(&mut frame, ppu, sprite, one_dimensional, corner);
        }
        notes.push(sprite_note(index, sprite, hidden));
    }
    Graphics {
        image: frame,
        notes,
    }
}

/// All 512 colours of palette RAM, one 8×8 swatch each: the 256 BG colours
/// on the left and the 256 OBJ colours on the right, each a 16×16 grid
/// with one 16-colour palette per row.
fn palettes(ppu: &Ppu) -> Graphics {
    let half_width = 16 * SWATCH;
    let mut frame = blank(2 * half_width + PALETTE_GAP, 16 * SWATCH);
    for index in 0..2 * OBJ_PALETTE {
        let (half, entry) = (index / OBJ_PALETTE, index % OBJ_PALETTE);
        let left = half * (half_width + PALETTE_GAP) + entry % 16 * SWATCH;
        let top = entry / 16 * SWATCH;
        fill_rect(
            &mut frame,
            (left, top),
            (SWATCH, SWATCH),
            palette_color(ppu, index),
        );
    }
    Graphics {
        image: frame,
        notes: vec![
            format!(
                "BG 0x{PALETTE_ADDRESS:08X}, OBJ 0x{:08X}",
                PALETTE_ADDRESS + OBJ_PALETTE * 2
            ),
            "Each row is one 16-colour palette, 0–15 from the top; BG colour 0 is the \
             backdrop"
                .to_string(),
        ],
    }
}

// ----- Memory and colours ----------------------------------------------

/// Where the CPU's VRAM offset `offset` lands: VRAM is 96 KiB, mirrored
/// every 128 KiB, with 0x18000–0x1FFFF repeating 0x10000–0x17FFF (GBATEK,
/// "GBA Memory Map").
fn mirror(offset: usize) -> usize {
    let offset = offset & 0x1_FFFF;
    if offset >= 0x1_8000 {
        offset - 0x8000
    } else {
        offset
    }
}

/// The VRAM byte at `offset` as the renderer reads it: 0 past the end.
fn vram_byte(ppu: &Ppu, offset: usize) -> u8 {
    ppu.vram().get(offset).copied().unwrap_or(0)
}

/// The VRAM halfword at `offset`, little-endian.
fn vram_halfword(ppu: &Ppu, offset: usize) -> u16 {
    u16::from_le_bytes([vram_byte(ppu, offset), vram_byte(ppu, offset + 1)])
}

/// A background's tile byte at `offset`: backgrounds cannot reach OBJ VRAM
/// (0x10000 and up), which reads 0 to them.
fn bg_byte(ppu: &Ppu, offset: usize) -> u8 {
    if offset < OBJ_TILES {
        vram_byte(ppu, offset)
    } else {
        0
    }
}

/// Palette RAM colour `index` (0–255 BG, 256–511 OBJ) as a pixel.
fn palette_color(ppu: &Ppu, index: usize) -> u32 {
    color(ppu.read_palette(index as u32 * 2))
}

/// The first palette RAM colour the tile picture uses, and its name: 4bpp
/// tiles use 16-colour palette `palette` (0–15 BG, 16–31 OBJ, wrapping
/// past 31); 8bpp tiles use the 256-colour BG palette, or OBJ's for tiles
/// in OBJ VRAM.
fn tile_palette(palette: u8, eight_bit: bool, in_obj_vram: bool) -> (usize, String) {
    let palette = usize::from(palette & 31);
    match (eight_bit, palette) {
        (true, _) if in_obj_vram => (OBJ_PALETTE, "the OBJ 256-colour palette".to_string()),
        (true, _) => (0, "the BG 256-colour palette".to_string()),
        (false, 0..=15) => (palette * 16, format!("BG palette {palette}")),
        (false, _) => (
            OBJ_PALETTE + (palette - 16) * 16,
            format!("OBJ palette {}", palette - 16),
        ),
    }
}

// ----- Tiles -------------------------------------------------------------

/// One 8×8 tile's palette indices, row by row.
type Tile = [[u8; 8]; 8];

/// Which ways a tile is mirrored.
#[derive(Debug, Clone, Copy, Default)]
struct Flips {
    /// Left to right.
    x: bool,
    /// Top to bottom.
    y: bool,
}

impl Flips {
    /// The flips in a text map entry: bit 10 horizontal, bit 11 vertical
    /// (GBATEK, "LCD VRAM BG Screen Data Format").
    fn from_entry(entry: u16) -> Self {
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
fn tile_texel(
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
fn read_tile(read: &impl Fn(usize) -> u8, address: usize, eight_bit: bool) -> Tile {
    std::array::from_fn(|y| std::array::from_fn(|x| tile_texel(read, address, eight_bit, x, y)))
}

/// Draws `tile` with its top-left corner at `corner`, mirrored as `flips`
/// says, painting each palette index with `paint`.
fn draw_tile(
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

// ----- Backgrounds -------------------------------------------------------

/// What a background shows in a BG mode (GBATEK, "LCD I/O Display
/// Control").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BgKind {
    /// Scrolling tiles with 16-bit map entries.
    Text,
    /// Rotation/scaling tiles with 8-bit map entries.
    Affine,
    /// BG2's bitmap in modes 3–5.
    Bitmap,
}

/// What background `layer` is in BG mode `mode`, or `None` if the mode
/// has no such background.
fn bg_kind(mode: u16, layer: usize) -> Option<BgKind> {
    match (mode, layer) {
        (0, _) | (1, 0 | 1) => Some(BgKind::Text),
        (1, 2) | (2, 2 | 3) => Some(BgKind::Affine),
        (3..=5, 2) => Some(BgKind::Bitmap),
        _ => None,
    }
}

/// One background's settings.
#[derive(Debug, Clone, Copy)]
struct Background {
    /// 0–3.
    layer: usize,
    /// Its BGxCNT.
    control: u16,
    /// DISPCNT shows it.
    shown: bool,
}

impl Background {
    /// Where its tiles start in VRAM: BGxCNT bits 2–3, in 16 KiB steps.
    fn char_base(&self) -> usize {
        usize::from((self.control >> 2) & 3) * 0x4000
    }

    /// Where its map starts in VRAM: BGxCNT bits 8–12, in 2 KiB steps.
    fn screen_base(&self) -> usize {
        usize::from((self.control >> 8) & 0x1F) * 0x800
    }

    /// 256-colour tiles (BGxCNT bit 7).
    fn eight_bit(&self) -> bool {
        self.control & (1 << 7) != 0
    }

    /// The end of every map note: priority and whether it is shown.
    fn status(&self) -> String {
        let shown = if self.shown { "on" } else { "off" };
        format!("priority {}, {shown}", self.control & 3)
    }
}

/// A text background's whole map, 256 or 512 pixels each way, with the
/// 240×160 screen outlined at its scroll offsets. Colour 0 shows the
/// backdrop.
fn text_map(ppu: &Ppu, background: &Background) -> Graphics {
    let (width, height) = match background.control >> 14 {
        0 => (256, 256),
        1 => (512, 256),
        2 => (256, 512),
        _ => (512, 512),
    };
    let eight_bit = background.eight_bit();
    let tile_bytes = if eight_bit { 64 } else { 32 };
    let backdrop = palette_color(ppu, 0);
    let read = |offset: usize| bg_byte(ppu, offset);
    let mut frame = blank(width, height);
    for cell_y in 0..height / 8 {
        for cell_x in 0..width / 8 {
            let entry = text_entry(ppu, background.screen_base(), width, cell_x, cell_y);
            let address = background.char_base() + usize::from(entry & 0x3FF) * tile_bytes;
            let tile = read_tile(&read, address, eight_bit);
            let palette = usize::from(entry >> 12) * 16;
            let corner = (cell_x * 8, cell_y * 8);
            draw_tile(
                &mut frame,
                &tile,
                corner,
                Flips::from_entry(entry),
                |index| match index {
                    0 => backdrop,
                    _ if eight_bit => palette_color(ppu, usize::from(index)),
                    _ => palette_color(ppu, palette + usize::from(index)),
                },
            );
        }
    }
    // BGxHOFS and BGxVOFS are write-only; the PPU keeps what was written.
    let layer = background.layer;
    let scroll_x = usize::from(ppu.raw_register(0x10 + layer * 4) & 0x1FF);
    let scroll_y = usize::from(ppu.raw_register(0x12 + layer * 4) & 0x1FF);
    outline_wrapping(
        &mut frame,
        (scroll_x, scroll_y),
        (SCREEN_WIDTH, SCREEN_HEIGHT),
    );
    let bpp = if eight_bit { "8bpp" } else { "4bpp" };
    Graphics {
        image: frame,
        notes: vec![format!(
            "BG{layer}: text, {width}×{height}, {bpp}, map 0x{:08X}, tiles 0x{:08X}, {}; \
             scroll ({scroll_x}, {scroll_y}), the outline is the 240×160 screen",
            VRAM_ADDRESS + background.screen_base(),
            VRAM_ADDRESS + background.char_base(),
            background.status()
        )],
    }
}

/// The map entry for tile (`x`, `y`) of a text background `width` pixels
/// wide whose map starts at `screen_base`. Maps wider or taller than 256
/// pixels are 2 KiB screen blocks of 32×32 entries, left to right then top
/// to bottom (GBATEK, "LCD VRAM BG Screen Data Format").
fn text_entry(ppu: &Ppu, screen_base: usize, width: usize, x: usize, y: usize) -> u16 {
    let block = x / 32 + (y / 32) * (width / 256);
    vram_halfword(
        ppu,
        screen_base + block * 0x800 + (y % 32) * 64 + (x % 32) * 2,
    )
}

/// An affine background's whole map, 128 to 1024 pixels square, drawn
/// unrotated: one byte per map entry, 8bpp tiles, no flips. Colour 0 shows
/// the backdrop.
fn affine_map(ppu: &Ppu, background: &Background) -> Graphics {
    let size = 128 << (background.control >> 14);
    let tiles_wide = size / 8;
    let backdrop = palette_color(ppu, 0);
    let read = |offset: usize| bg_byte(ppu, offset);
    let mut frame = blank(size, size);
    for cell in 0..tiles_wide * tiles_wide {
        let tile = usize::from(bg_byte(ppu, background.screen_base() + cell));
        let indices = read_tile(&read, background.char_base() + tile * 64, true);
        let corner = (cell % tiles_wide * 8, cell / tiles_wide * 8);
        draw_tile(
            &mut frame,
            &indices,
            corner,
            Flips::default(),
            |index| match index {
                0 => backdrop,
                _ => palette_color(ppu, usize::from(index)),
            },
        );
    }
    let wrap = if background.control & (1 << 13) != 0 {
        "wraps"
    } else {
        "no wrap"
    };
    Graphics {
        image: frame,
        notes: vec![format!(
            "BG{}: affine, {size}×{size}, 8bpp, map 0x{:08X}, tiles 0x{:08X}, {wrap}, {}; \
             drawn unrotated, without the screen outline",
            background.layer,
            VRAM_ADDRESS + background.screen_base(),
            VRAM_ADDRESS + background.char_base(),
            background.status()
        )],
    }
}

/// BG2's bitmap in modes 3–5, unscaled: mode 3's 240×160 direct colours,
/// mode 4's 240×160 BG palette indices, or mode 5's 160×128 direct colours,
/// from the frame DISPCNT bit 4 shows in modes 4 and 5 (GBATEK, "LCD VRAM
/// Bitmap BG Modes").
fn bitmap(ppu: &Ppu, background: &Background, dispcnt: u16) -> Graphics {
    let mode = dispcnt & 7;
    let page = usize::from(mode != 3 && dispcnt & (1 << 4) != 0);
    let start = page * 0xA000;
    let (width, height) = if mode == 5 {
        (160, 128)
    } else {
        (SCREEN_WIDTH, SCREEN_HEIGHT)
    };
    let mut frame = blank(width, height);
    for y in 0..height {
        for x in 0..width {
            let offset = y * width + x;
            let pixel = if mode == 4 {
                palette_color(ppu, usize::from(vram_byte(ppu, start + offset)))
            } else {
                color(vram_halfword(ppu, start + offset * 2) & 0x7FFF)
            };
            set_pixel(&mut frame, x, y, pixel);
        }
    }
    let format = if mode == 4 {
        "8bpp through the BG palette"
    } else {
        "15-bit colour"
    };
    let frame_note = if mode == 3 {
        String::new()
    } else {
        format!(", frame {page} (DISPCNT bit 4)")
    };
    Graphics {
        image: frame,
        notes: vec![format!(
            "BG2: bitmap mode {mode}, {width}×{height}, {format}, at 0x{:08X}{frame_note}, {}; \
             drawn unscaled",
            VRAM_ADDRESS + start,
            background.status()
        )],
    }
}

// ----- Sprites -----------------------------------------------------------

/// One OAM entry's three attribute halfwords (GBATEK, "LCD OBJ - OAM
/// Attributes").
#[derive(Debug, Clone, Copy)]
struct Sprite {
    attr0: u16,
    attr1: u16,
    attr2: u16,
}

impl Sprite {
    /// OAM entry `index` (0–127). The fourth halfword of each entry is an
    /// affine parameter, which drawing unrotated does not need.
    fn read(ppu: &Ppu, index: usize) -> Self {
        let base = index as u32 * 8;
        Self {
            attr0: ppu.read_oam(base),
            attr1: ppu.read_oam(base + 2),
            attr2: ppu.read_oam(base + 4),
        }
    }

    /// Rotation/scaling is on (attribute 0 bit 8).
    fn affine(self) -> bool {
        self.attr0 & (1 << 8) != 0
    }

    /// Not drawn at all: not affine, with attribute 0 bit 9 set.
    fn disabled(self) -> bool {
        !self.affine() && self.attr0 & (1 << 9) != 0
    }

    /// An affine sprite's drawing area is twice its size (attribute 0 bit
    /// 9).
    fn double_size(self) -> bool {
        self.affine() && self.attr0 & (1 << 9) != 0
    }

    /// Attribute 0 bits 10–11: 0 normal, 1 semi-transparent, 2 OBJ window,
    /// 3 prohibited.
    fn mode(self) -> usize {
        usize::from((self.attr0 >> 10) & 3)
    }

    /// Mosaic applies (attribute 0 bit 12).
    fn mosaic(self) -> bool {
        self.attr0 & (1 << 12) != 0
    }

    /// 256-colour tiles (attribute 0 bit 13).
    fn eight_bit(self) -> bool {
        self.attr0 & (1 << 13) != 0
    }

    /// Attribute 0 bits 14–15: 0 square, 1 wide, 2 tall, 3 prohibited.
    fn shape(self) -> usize {
        usize::from(self.attr0 >> 14)
    }

    /// Width and height in pixels, from the shape and the size field
    /// (attribute 1 bits 14–15); `None` for the prohibited shape.
    fn size(self) -> Option<(usize, usize)> {
        let sizes = OBJ_SIZES.get(self.shape())?;
        Some(sizes[usize::from(self.attr1 >> 14)])
    }

    /// Screen x: attribute 1 bits 0–8, signed.
    fn x(self) -> i32 {
        let x = i32::from(self.attr1 & 0x1FF);
        if x >= 256 { x - 512 } else { x }
    }

    /// Screen y: attribute 0 bits 0–7, where values from 160 on are above
    /// the screen, as the renderer treats them.
    fn y(self) -> i32 {
        let y = i32::from(self.attr0 & 0xFF);
        if y >= SCREEN_HEIGHT as i32 {
            y - 256
        } else {
            y
        }
    }

    /// Mirrored left to right (attribute 1 bit 12, without rotation).
    fn h_flip(self) -> bool {
        !self.affine() && self.attr1 & (1 << 12) != 0
    }

    /// Mirrored top to bottom (attribute 1 bit 13, without rotation).
    fn v_flip(self) -> bool {
        !self.affine() && self.attr1 & (1 << 13) != 0
    }

    /// Which of the 32 affine parameter groups it uses (attribute 1 bits
    /// 9–13).
    fn affine_index(self) -> u16 {
        (self.attr1 >> 9) & 0x1F
    }

    /// Its first tile number (attribute 2 bits 0–9), in 32-byte units.
    fn tile(self) -> usize {
        usize::from(self.attr2 & 0x3FF)
    }

    /// Priority against backgrounds (attribute 2 bits 10–11).
    fn priority(self) -> u16 {
        (self.attr2 >> 10) & 3
    }

    /// 16-colour OBJ palette (attribute 2 bits 12–15).
    fn palette(self) -> usize {
        usize::from(self.attr2 >> 12)
    }
}

/// Draws `sprite` unrotated with its top-left corner at `corner`, flipped
/// as its attributes say; transparent texels leave the picture as it is.
fn draw_sprite(
    frame: &mut Frame,
    ppu: &Ppu,
    sprite: Sprite,
    one_dimensional: bool,
    (left, top): (usize, usize),
) {
    let Some((width, height)) = sprite.size() else {
        return;
    };
    for y in 0..height {
        let texel_y = if sprite.v_flip() { height - 1 - y } else { y };
        for x in 0..width {
            let texel_x = if sprite.h_flip() { width - 1 - x } else { x };
            if let Some(pixel) = sprite_texel(ppu, sprite, one_dimensional, texel_x, texel_y) {
                set_pixel(frame, left + x, top + y, pixel);
            }
        }
    }
}

/// The colour of texel (`x`, `y`) of `sprite`, `None` where transparent.
/// With 2D mapping OBJ VRAM is a sheet 32 tiles wide, so a sprite's next
/// row of tiles is 32 tiles on; with 1D mapping its tiles follow each
/// other. 8bpp tiles take two tile numbers each (GBATEK, "LCD OBJ - VRAM
/// Character (Tile) Mapping").
fn sprite_texel(
    ppu: &Ppu,
    sprite: Sprite,
    one_dimensional: bool,
    x: usize,
    y: usize,
) -> Option<u32> {
    let (width, _) = sprite.size()?;
    let eight_bit = sprite.eight_bit();
    let step = if eight_bit { 2 } else { 1 };
    let row_stride = if one_dimensional {
        width / 8 * step
    } else {
        32
    };
    let tile = (sprite.tile() + y / 8 * row_stride + x / 8 * step) & 0x3FF;
    let read = |offset: usize| vram_byte(ppu, offset);
    let index = usize::from(tile_texel(
        &read,
        OBJ_TILES + tile * 32,
        eight_bit,
        x % 8,
        y % 8,
    ));
    match index {
        0 => None,
        _ if eight_bit => Some(palette_color(ppu, OBJ_PALETTE + index)),
        _ => Some(palette_color(
            ppu,
            OBJ_PALETTE + sprite.palette() * 16 + index,
        )),
    }
}

/// One line about OAM entry `index`: position, size and shape, tile,
/// colours, priority, mode, rotation or flips, and mosaic. `hidden` says
/// the PPU skips it because its tiles are in bitmap VRAM.
fn sprite_note(index: usize, sprite: Sprite, hidden: bool) -> String {
    let Some((width, height)) = sprite.size() else {
        return format!("{index:3}: prohibited shape 3, not drawn");
    };
    let shape = ["square", "wide", "tall"][sprite.shape()];
    let colors = if sprite.eight_bit() {
        "8bpp".to_string()
    } else {
        format!("palette {}", sprite.palette())
    };
    let mode = ["normal", "semi-transparent", "window", "prohibited mode 3"][sprite.mode()];
    let mut note = format!(
        "{index:3}: x {}, y {}, {width}×{height} {shape}, tile 0x{:03X}, {colors}, priority {}, {mode}",
        sprite.x(),
        sprite.y(),
        sprite.tile(),
        sprite.priority()
    );
    if sprite.affine() {
        note += &format!(", affine {}", sprite.affine_index());
    }
    let flags = [
        (sprite.double_size(), "double size"),
        (sprite.h_flip(), "h-flip"),
        (sprite.v_flip(), "v-flip"),
        (sprite.mosaic(), "mosaic"),
        (hidden, "hidden (tile below 512 in a bitmap mode)"),
    ];
    for (_, flag) in flags.iter().filter(|(set, _)| *set) {
        note += ", ";
        note += flag;
    }
    note
}

// ----- Drawing -----------------------------------------------------------

/// A `width`×`height` picture filled with [`BACKGROUND`].
fn blank(width: usize, height: usize) -> Frame {
    let mut frame = Frame::new(ScreenSize::new(width as u16, height as u16));
    frame.pixels_mut().fill(BACKGROUND);
    frame
}

/// Sets the pixel at (`x`, `y`) if it is inside the picture.
fn set_pixel(frame: &mut Frame, x: usize, y: usize, color: u32) {
    let size = frame.size();
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    if x < width && y < height {
        frame.pixels_mut()[y * width + x] = color;
    }
}

/// Fills the `width`×`height` rectangle from (`left`, `top`) with `color`.
fn fill_rect(
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
fn fill_checker(frame: &mut Frame, (left, top): (usize, usize), (width, height): (usize, usize)) {
    for y in 0..height {
        for x in 0..width {
            set_pixel(frame, left + x, top + y, CHECKER[(x / 4 + y / 4) % 2]);
        }
    }
}

/// Outlines the `width`×`height` rectangle from (`left`, `top`) with
/// 1-pixel [`OUTLINE`] lines, wrapping around the picture's edges as the
/// screen wraps around the map.
fn outline_wrapping(
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

    const RED: u16 = 0x001F;
    const GREEN: u16 = 0x03E0;
    const BLUE: u16 = 0x7C00;

    fn request(view: GraphicsView) -> GraphicsRequest {
        GraphicsRequest {
            view,
            ..GraphicsRequest::default()
        }
    }

    fn draw_request(ppu: &Ppu, request: GraphicsRequest) -> Graphics {
        draw(ppu, &request).expect("the GBA draws every view")
    }

    fn pixel(graphics: &Graphics, x: usize, y: usize) -> u32 {
        let width = usize::from(graphics.image.size().width);
        graphics.image.pixels()[y * width + x]
    }

    /// Hides every sprite, as games do: all-zero OAM entries are visible
    /// 8×8 sprites.
    fn hide_sprites(ppu: &mut Ppu) {
        for index in 0..128 {
            ppu.write_oam(index * 8, 1 << 9);
        }
    }

    #[test]
    fn four_bit_tiles_use_the_chosen_palette_and_base() {
        let mut ppu = Ppu::new();
        ppu.write_palette(0x200 + 2 * 32 + 2, RED); // OBJ palette 2, colour 1
        ppu.write_palette(0x200 + 2 * 32 + 4, GREEN); // colour 2
        ppu.write_vram(0x20, 0x0021); // tile 1: pixels 0 and 1 are 1 and 2
        ppu.write_vram(0x1_0000, 0x0001);
        let tiles = |base| {
            draw_request(
                &ppu,
                GraphicsRequest {
                    base,
                    palette: 18,
                    ..request(GraphicsView::Tiles)
                },
            )
        };

        let graphics = tiles(0x0600_0000);
        assert_eq!(graphics.image.size(), ScreenSize::new(256, 256));
        assert_eq!(pixel(&graphics, 8, 0), color(RED));
        assert_eq!(pixel(&graphics, 9, 0), color(GREEN));
        assert_eq!(pixel(&graphics, 10, 0), color(0));
        assert!(graphics.notes[0].contains("OBJ palette 2"));
        assert_eq!(pixel(&tiles(0x0601_0000), 0, 0), color(RED));
        let mirrored = tiles(0x0601_8000);
        assert_eq!(pixel(&mirrored, 0, 0), color(RED));
        assert!(mirrored.notes[0].starts_with("Tiles at 0x06010000"));
    }

    #[test]
    fn eight_bit_tiles_take_the_bg_or_obj_palette_by_base() {
        let mut ppu = Ppu::new();
        ppu.write_vram(0, 0x0005);
        ppu.write_vram(0x1_0000, 0x0005);
        ppu.write_palette(5 * 2, RED);
        ppu.write_palette(0x200 + 5 * 2, GREEN);
        let tiles = |base| {
            draw_request(
                &ppu,
                GraphicsRequest {
                    base,
                    eight_bit: true,
                    ..request(GraphicsView::Tiles)
                },
            )
        };
        let background = tiles(0x0600_0000);
        assert_eq!(background.image.size(), ScreenSize::new(256, 128));
        assert_eq!(pixel(&background, 0, 0), color(RED));
        let objects = tiles(0x0601_0000);
        assert_eq!(pixel(&objects, 0, 0), color(GREEN));
        assert!(objects.notes[0].contains("the OBJ 256-colour palette"));
    }

    #[test]
    fn text_maps_flip_tiles_and_outline_the_screen() {
        let mut ppu = Ppu::new();
        ppu.write_palette(0, BLUE); // backdrop
        ppu.write_palette(3 * 32 + 2, RED); // BG palette 3, colour 1
        ppu.write_vram(0x4000 + 32, 0x0001); // tile 1: top-left pixel
        ppu.write_vram(0xF800 + 2, 1 | 1 << 10 | 1 << 11 | 3 << 12); // cell (1, 0)
        ppu.write_register(0x0A, 31 << 8 | 1 << 2); // BG1: map block 31, tiles 0x4000
        ppu.write_register(0x14, 100); // BG1HOFS
        ppu.write_register(0x16, 200); // BG1VOFS
        ppu.write_register(0x00, 1 << 9); // mode 0, BG1 on

        let graphics = draw_request(
            &ppu,
            GraphicsRequest {
                layer: 1,
                ..request(GraphicsView::Map)
            },
        );
        assert_eq!(graphics.image.size(), ScreenSize::new(256, 256));
        assert_eq!(pixel(&graphics, 8 + 7, 7), color(RED), "flipped both ways");
        assert_eq!(
            pixel(&graphics, 8, 0),
            color(BLUE),
            "colour 0 is the backdrop"
        );
        // The screen's corners: (100, 200) to (339, 359), wrapped.
        assert_eq!(pixel(&graphics, 100, 200), OUTLINE);
        assert_eq!(pixel(&graphics, 83, 103), OUTLINE);
        assert_eq!(pixel(&graphics, 84, 104), color(BLUE));
        assert_eq!(
            graphics.notes[0],
            "BG1: text, 256×256, 4bpp, map 0x0600F800, tiles 0x06004000, priority 0, on; \
             scroll (100, 200), the outline is the 240×160 screen"
        );
    }

    #[test]
    fn bitmaps_draw_their_frame_and_missing_layers_say_so() {
        let mut ppu = Ppu::new();
        ppu.write_register(0x00, 3 | 1 << 10);
        ppu.write_vram(2, RED);
        let map = |ppu: &Ppu, layer| {
            draw_request(
                ppu,
                GraphicsRequest {
                    layer,
                    ..request(GraphicsView::Map)
                },
            )
        };
        let mode_3 = map(&ppu, 2);
        assert_eq!(mode_3.image.size(), ScreenSize::new(240, 160));
        assert_eq!(pixel(&mode_3, 1, 0), color(RED));

        ppu.write_register(0x00, 4 | 1 << 4); // mode 4, frame 1
        ppu.write_vram(0xA000, 0x0300); // pixel 1 is colour 3
        ppu.write_palette(3 * 2, GREEN);
        let mode_4 = map(&ppu, 2);
        assert_eq!(pixel(&mode_4, 1, 0), color(GREEN));
        assert!(mode_4.notes[0].contains("at 0x0600A000, frame 1"));

        ppu.write_register(0x00, 2);
        let missing = map(&ppu, 0);
        assert_eq!(missing.image.size(), ScreenSize::new(8, 8));
        assert_eq!(missing.notes, ["BG0 is not used in mode 2"]);
    }

    #[test]
    fn sprites_draw_in_their_cells_with_a_note_each() {
        let mut ppu = Ppu::new();
        hide_sprites(&mut ppu);
        ppu.write_register(0x00, 1 << 6 | 1 << 12); // 1D mapping, objects on
        ppu.write_palette(0x200 + 4 * 32 + 2, RED); // OBJ palette 4, colour 1
        ppu.write_vram(0x1_0000 + 2 * 32, 0x0001); // tile 2: top-left pixel
        // Sprite 17: 16×8 at (-16, 20), tile 2, priority 1, palette 4,
        // flipped left to right.
        ppu.write_oam(17 * 8, 20 | 1 << 14);
        ppu.write_oam(17 * 8 + 2, 0x1F0 | 1 << 12);
        ppu.write_oam(17 * 8 + 4, 2 | 1 << 10 | 4 << 12);
        // Sprite 3: 16×16, affine group 5, double size, 8bpp.
        ppu.write_oam(3 * 8, 50 | 1 << 8 | 1 << 9 | 1 << 13);
        ppu.write_oam(3 * 8 + 2, 5 << 9 | 1 << 14);
        ppu.write_oam(3 * 8 + 4, 0x10);

        let graphics = draw_request(&ppu, request(GraphicsView::Sprites));
        assert_eq!(graphics.image.size(), ScreenSize::new(1024, 512));
        // Sprite 17's cell is column 1, row 1.
        assert_eq!(pixel(&graphics, 64 + 15, 64), color(RED), "flipped");
        assert_eq!(pixel(&graphics, 64, 64), CHECKER[0]);
        assert_eq!(
            pixel(&graphics, 64 + 16, 64),
            CELL_SHADES[0],
            "past its width"
        );
        assert_eq!(
            pixel(&graphics, 0, 0),
            CELL_SHADES[0],
            "sprite 0 is disabled"
        );
        assert_eq!(
            graphics.notes[1..],
            [
                "  3: x 0, y 50, 16×16 square, tile 0x010, 8bpp, priority 0, normal, \
                 affine 5, double size",
                " 17: x -16, y 20, 16×8 wide, tile 0x002, palette 4, priority 1, normal, h-flip",
            ]
        );
    }

    #[test]
    fn palettes_show_bg_colours_left_and_obj_colours_right() {
        let mut ppu = Ppu::new();
        ppu.write_palette(17 * 2, RED); // BG palette 1, colour 1
        ppu.write_palette(0x3FE, GREEN); // OBJ palette 15, colour 15
        let graphics = draw_request(&ppu, request(GraphicsView::Palettes));
        assert_eq!(graphics.image.size(), ScreenSize::new(264, 128));
        assert_eq!(pixel(&graphics, 8, 8), color(RED));
        assert_eq!(pixel(&graphics, 15, 15), color(RED));
        assert_eq!(pixel(&graphics, 136 + 15 * 8, 15 * 8), color(GREEN));
        assert_eq!(pixel(&graphics, 130, 0), BACKGROUND, "the gap");
        assert_eq!(graphics.notes[0], "BG 0x05000000, OBJ 0x05000200");
    }
}
