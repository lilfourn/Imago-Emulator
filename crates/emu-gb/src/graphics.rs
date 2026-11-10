//! Pictures of video memory for the debugger's graphics viewers: tile
//! data, the two background maps, the sprites in OAM and the palettes (Pan
//! Docs, "VRAM Tile Data", "VRAM Tile Maps", "VRAM Banks", "Object
//! Attribute Memory (OAM)", "LCD Control" and "Palettes").
//!
//! [`draw`] reads the PPU without changing it and returns plain data: a
//! [`Frame`] and lines of notes. Everything is decoded here from VRAM, OAM
//! and the registers, the same way the PPU decodes them to draw a line.

use emu_core::debug::draw::{blank, fill_checker, fill_rect, outline_wrapping, set_pixel};
use emu_core::debug::{Graphics, GraphicsRequest, GraphicsView};
use emu_core::{Frame, rgb};

use crate::model::Model;
use crate::ppu::Ppu;

/// Screen width in pixels.
const SCREEN_WIDTH: usize = 160;
/// Screen height in pixels.
const SCREEN_HEIGHT: usize = 144;

/// Bytes per tile: 8 rows of two bytes, one per bit plane.
const TILE_BYTES: usize = 16;
/// Tiles in one bank's tile data, 0x8000–0x97FF.
const TILES_PER_BANK: usize = 384;
/// Tiles per row in the tile picture.
const TILES_WIDE: usize = 16;
/// Where VRAM bank 1 starts in [`Ppu::vram`], which holds both banks.
const BANK_1: usize = 0x2000;
/// VRAM offsets of the two tile maps, at 0x9800 and 0x9C00.
const MAPS: [usize; 2] = [0x1800, 0x1C00];
/// Tiles per side of a tile map.
const MAP_TILES: usize = 32;

/// Entries in OAM.
const SPRITES: usize = 40;
/// Cells per row in the sprite picture.
const SPRITE_COLUMNS: usize = 8;
/// A sprite cell's width: one tile.
const CELL_WIDTH: usize = 8;
/// A sprite cell's height: room for an 8×16 sprite.
const CELL_HEIGHT: usize = 16;

/// Width and height of one colour swatch in the palette picture.
const SWATCH: usize = 16;

/// Draws `request`'s view of `ppu`'s video memory. The Game Boy can draw
/// every view, so this always returns a picture.
pub(crate) fn draw(ppu: &Ppu, request: &GraphicsRequest) -> Option<Graphics> {
    Some(match request.view {
        GraphicsView::Tiles => tiles(ppu, request.palette),
        GraphicsView::Map => map(ppu, request.layer),
        GraphicsView::Sprites => sprites(ppu),
        GraphicsView::Palettes => palettes(ppu),
    })
}

// ----- Views -----------------------------------------------------------

/// All tile data, 384 tiles per bank in rows of 16, coloured with palette
/// `selection` (see [`palette_choice`]). A DMG shows bank 0 (128×192); a
/// CGB shows bank 1 to its right (256×192).
fn tiles(ppu: &Ppu, selection: u8) -> Graphics {
    let banks = if is_cgb(ppu) { 2 } else { 1 };
    let rows = TILES_PER_BANK / TILES_WIDE;
    let mut frame = blank(banks * TILES_WIDE * 8, rows * 8);
    let (colors, name) = palette_choice(ppu, selection);
    for bank in 0..banks {
        for tile in 0..TILES_PER_BANK {
            let left = (bank * TILES_WIDE + tile % TILES_WIDE) * 8;
            let top = tile / TILES_WIDE * 8;
            let address = bank * BANK_1 + tile * TILE_BYTES;
            draw_tile(
                &mut frame,
                ppu.vram(),
                address,
                (left, top),
                Flips::default(),
                colors.map(Some),
            );
        }
    }
    let bank_note = if banks == 2 {
        "bank 0 left, bank 1 right"
    } else {
        "bank 0"
    };
    Graphics {
        image: frame,
        notes: vec![
            format!("Tiles 0x8000–0x97FF, {bank_note}, 16 per row, coloured with {name}"),
            "Rows 0–7: 0x8000 (objects, and BG with LCDC bit 4 set); rows 8–15: 0x8800 \
             (both); rows 16–23: 0x9000 (BG with LCDC bit 4 clear)"
                .to_string(),
        ],
    }
}

/// The tile map at 0x9800 (`layer` 0) or 0x9C00 (any other layer): all
/// 32×32 tiles, 256×256 pixels, with CGB attributes from bank 1, and the
/// 160×144 screen outlined where SCX and SCY put it.
fn map(ppu: &Ppu, layer: u8) -> Graphics {
    let lcd = Lcd::read(ppu);
    let which = usize::from(layer.min(1));
    let vram = ppu.vram();
    let cgb = is_cgb(ppu);
    let mut frame = blank(MAP_TILES * 8, MAP_TILES * 8);
    for cell in 0..MAP_TILES * MAP_TILES {
        let entry = MAPS[which] + cell;
        // CGB attributes sit at the same offset in bank 1.
        let attributes = if cgb { vram[BANK_1 + entry] } else { 0 };
        let bank = if attributes & 0x08 != 0 { BANK_1 } else { 0 };
        let tile = bank + bg_tile_address(lcd.lcdc, vram[entry]);
        let colors = if cgb {
            cgb_colors(ppu.bg_palette_ram(), attributes & 7)
        } else {
            dmg_colors(ppu, 0, lcd.bgp)
        };
        let corner = (cell % MAP_TILES * 8, cell / MAP_TILES * 8);
        let flips = Flips::from_attributes(attributes);
        draw_tile(&mut frame, vram, tile, corner, flips, colors.map(Some));
    }
    let scroll = (usize::from(lcd.scx), usize::from(lcd.scy));
    outline_wrapping(&mut frame, scroll, (SCREEN_WIDTH, SCREEN_HEIGHT));
    Graphics {
        image: frame,
        notes: map_notes(&lcd, which),
    }
}

/// Every OAM entry in a grid of 8 columns and 5 rows, each in its own cell
/// over a checkerboard that shows through colour 0, with one note per
/// sprite.
fn sprites(ppu: &Ppu) -> Graphics {
    let lcd = Lcd::read(ppu);
    let tall = lcd.lcdc & 0x04 != 0;
    let rows = SPRITES / SPRITE_COLUMNS;
    let mut frame = blank(
        SPRITE_COLUMNS * (CELL_WIDTH + 1) + 1,
        rows * (CELL_HEIGHT + 1) + 1,
    );
    let shown = if lcd.lcdc & 0x02 != 0 { "on" } else { "off" };
    let height = if tall { 16 } else { 8 };
    let mut notes = vec![format!(
        "40 sprites, 8×{height} (LCDC bit 2), objects {shown} (LCDC bit 1), \
         left to right then top to bottom"
    )];
    for index in 0..SPRITES {
        let sprite = Sprite::read(ppu, index);
        let left = index % SPRITE_COLUMNS * (CELL_WIDTH + 1) + 1;
        let top = index / SPRITE_COLUMNS * (CELL_HEIGHT + 1) + 1;
        fill_checker(&mut frame, (left, top), (CELL_WIDTH, CELL_HEIGHT));
        draw_sprite(&mut frame, ppu, &lcd, sprite, (left, top));
        notes.push(sprite_note(index, sprite, is_cgb(ppu)));
    }
    Graphics {
        image: frame,
        notes,
    }
}

/// One row of four swatches per palette, colours 0–3 left to right: a
/// DMG's BGP, OBP0 and OBP1 through the DMG shades, or a CGB's BG palettes
/// 0–7 then OBJ palettes 0–7. The notes give the raw values.
fn palettes(ppu: &Ppu) -> Graphics {
    let (summary, rows) = if is_cgb(ppu) {
        (
            "CGB palette RAM: rows 0–7 BG palettes (BCPD), rows 8–15 OBJ palettes \
             (OCPD); colours 0–3 left to right, BGR555",
            cgb_palette_rows(ppu),
        )
    } else {
        (
            "DMG palettes: rows BGP, OBP0, OBP1; colours 0–3 left to right \
             (shade 0 is the lightest)",
            dmg_palette_rows(ppu, &Lcd::read(ppu)),
        )
    };
    let pitch = SWATCH + 1;
    let mut frame = blank(4 * pitch + 1, rows.len() * pitch + 1);
    for (row, (colors, _)) in rows.iter().enumerate() {
        for (column, &color) in colors.iter().enumerate() {
            let corner = (column * pitch + 1, row * pitch + 1);
            fill_rect(&mut frame, corner, (SWATCH, SWATCH), color);
        }
    }
    let notes = std::iter::once(summary.to_string())
        .chain(rows.into_iter().map(|(_, note)| note))
        .collect();
    Graphics {
        image: frame,
        notes,
    }
}

// ----- Registers and palettes ------------------------------------------

/// The LCD registers the viewers use, read without side effects.
#[derive(Debug, Clone, Copy)]
struct Lcd {
    lcdc: u8,
    scy: u8,
    scx: u8,
    bgp: u8,
    obp0: u8,
    obp1: u8,
    wy: u8,
    wx: u8,
}

impl Lcd {
    /// Reads the registers from `ppu`.
    fn read(ppu: &Ppu) -> Self {
        Self {
            lcdc: ppu.read_register(0xFF40),
            scy: ppu.read_register(0xFF42),
            scx: ppu.read_register(0xFF43),
            bgp: ppu.read_register(0xFF47),
            obp0: ppu.read_register(0xFF48),
            obp1: ppu.read_register(0xFF49),
            wy: ppu.read_register(0xFF4A),
            wx: ppu.read_register(0xFF4B),
        }
    }
}

/// Whether `ppu` is a Game Boy Color's, with a second VRAM bank and colour
/// palettes.
fn is_cgb(ppu: &Ppu) -> bool {
    ppu.model() == Model::Cgb
}

/// The shade a DMG palette register (BGP, OBP0 or OBP1) gives each colour
/// index 0–3: two bits per index, index 0 in bits 0–1 (Pan Docs, "Palettes",
/// "FF47 — BGP").
fn dmg_shades(register: u8) -> [u8; 4] {
    [0, 1, 2, 3].map(|index| (register >> (index * 2)) & 3)
}

/// The pixels DMG palette register `register` gives colour indices 0–3;
/// `palette` is 0 for BGP, 1 for OBP0 and 2 for OBP1.
fn dmg_colors(ppu: &Ppu, palette: u8, register: u8) -> [u32; 4] {
    dmg_shades(register).map(|shade| ppu.dmg_color(palette, shade))
}

/// Colour `color` (0–3) of CGB palette `palette` (0–7) in palette RAM
/// `ram`, as a 15-bit BGR555 value: 8 bytes per palette, 2 per colour,
/// little-endian (Pan Docs, "LCD Color Palettes (CGB only)").
fn cgb_value(ram: &[u8], palette: u8, color: u8) -> u16 {
    let index = usize::from(palette) * 8 + usize::from(color) * 2;
    u16::from_le_bytes([ram[index], ram[index + 1]]) & 0x7FFF
}

/// A BGR555 colour as a pixel, widening each 5-bit channel to 8 bits as
/// `(x << 3) | (x >> 2)`, the way the PPU draws it.
fn bgr555(value: u16) -> u32 {
    let channel = |shift: u16| {
        let x = ((value >> shift) & 0x1F) as u8;
        x << 3 | x >> 2
    };
    rgb(channel(0), channel(5), channel(10))
}

/// The pixels of CGB palette `palette` (0–7) in palette RAM `ram`.
fn cgb_colors(ram: &[u8], palette: u8) -> [u32; 4] {
    [0, 1, 2, 3].map(|color| bgr555(cgb_value(ram, palette, color)))
}

/// The colours palette `selection` gives colour indices 0–3 in the tile
/// picture, and its name: on a DMG 0 is BGP, 1 OBP0 and 2 OBP1; on a CGB
/// 0–7 are the BG palettes and 8–15 the OBJ palettes. Larger numbers pick
/// the last palette.
fn palette_choice(ppu: &Ppu, selection: u8) -> ([u32; 4], String) {
    if is_cgb(ppu) {
        return match selection.min(15) {
            palette @ 0..=7 => (
                cgb_colors(ppu.bg_palette_ram(), palette),
                format!("BG palette {palette}"),
            ),
            palette => (
                cgb_colors(ppu.obj_palette_ram(), palette - 8),
                format!("OBJ palette {}", palette - 8),
            ),
        };
    }
    let lcd = Lcd::read(ppu);
    let (name, register) = match selection {
        0 => ("BGP", lcd.bgp),
        1 => ("OBP0", lcd.obp0),
        _ => ("OBP1", lcd.obp1),
    };
    let palette = selection.min(2);
    (
        dmg_colors(ppu, palette, register),
        format!("{name} (0x{register:02X})"),
    )
}

/// The DMG palette picture's rows: each register's colours and a note
/// with its value and the shade it gives each colour index.
fn dmg_palette_rows(ppu: &Ppu, lcd: &Lcd) -> Vec<([u32; 4], String)> {
    [("BGP", lcd.bgp), ("OBP0", lcd.obp0), ("OBP1", lcd.obp1)]
        .into_iter()
        .zip(0..)
        .map(|((name, register), palette)| {
            let [a, b, c, d] = dmg_shades(register);
            let note = format!("{name} 0x{register:02X}: shades {a}, {b}, {c}, {d}");
            (dmg_colors(ppu, palette, register), note)
        })
        .collect()
}

/// The CGB palette picture's rows: the 8 BG then 8 OBJ palettes' colours
/// and a note with their BGR555 values.
fn cgb_palette_rows(ppu: &Ppu) -> Vec<([u32; 4], String)> {
    let row = |kind: &str, ram: &[u8], palette: u8| {
        let values = [0, 1, 2, 3].map(|color| format!("0x{:04X}", cgb_value(ram, palette, color)));
        let note = format!("{kind} {palette}: {}", values.join(" "));
        (cgb_colors(ram, palette), note)
    };
    let background = (0..8).map(|palette| row("BG", ppu.bg_palette_ram(), palette));
    let objects = (0..8).map(|palette| row("OBJ", ppu.obj_palette_ram(), palette));
    background.chain(objects).collect()
}

// ----- Tiles, maps and sprites -----------------------------------------

/// Which ways a tile is mirrored.
#[derive(Debug, Clone, Copy, Default)]
struct Flips {
    /// Left to right.
    x: bool,
    /// Top to bottom.
    y: bool,
}

impl Flips {
    /// The flips in a CGB map attribute byte or an OAM attribute byte: bit
    /// 5 horizontal, bit 6 vertical (Pan Docs, "BG Map Attributes", "Byte 3
    /// — Attributes/Flags").
    fn from_attributes(attributes: u8) -> Self {
        Self {
            x: attributes & 0x20 != 0,
            y: attributes & 0x40 != 0,
        }
    }
}

/// Colour indices 0–3 of row `row` of the tile at VRAM offset `tile`. Each
/// row is two bytes, the low bit plane then the high; bit 7 is the leftmost
/// pixel (Pan Docs, "VRAM Tile Data").
fn tile_row(vram: &[u8], tile: usize, row: usize) -> [u8; 8] {
    let low = vram[tile + row * 2];
    let high = vram[tile + row * 2 + 1];
    std::array::from_fn(|column| {
        let bit = 7 - column;
        ((high >> bit) & 1) << 1 | ((low >> bit) & 1)
    })
}

/// VRAM offset of background tile `index`: 0x8000 + index × 16 when LCDC
/// bit 4 is set, otherwise 0x9000 + index × 16 with the index signed (Pan
/// Docs, "VRAM Tile Data").
fn bg_tile_address(lcdc: u8, index: u8) -> usize {
    if lcdc & 0x10 != 0 {
        usize::from(index) * TILE_BYTES
    } else {
        (0x1000 + i32::from(index as i8) * TILE_BYTES as i32) as usize
    }
}

/// Draws the 8×8 tile at VRAM offset `tile` with its top-left corner at
/// `corner`, mirrored as `flips` says. `colors` gives each colour index's
/// pixel; `None` leaves the picture as it is there (transparent).
fn draw_tile(
    frame: &mut Frame,
    vram: &[u8],
    tile: usize,
    (left, top): (usize, usize),
    flips: Flips,
    colors: [Option<u32>; 4],
) {
    for row in 0..8 {
        let y = if flips.y { 7 - row } else { row };
        for (column, index) in tile_row(vram, tile, row).into_iter().enumerate() {
            let x = if flips.x { 7 - column } else { column };
            if let Some(color) = colors[usize::from(index)] {
                set_pixel(frame, left + x, top + y, color);
            }
        }
    }
}

/// The map picture's notes: which layers use the map and how its tiles are
/// addressed, then the window's settings.
fn map_notes(lcd: &Lcd, which: usize) -> Vec<String> {
    let address = 0x9800 + which * 0x400;
    // LCDC bit 3 picks the background's map and bit 6 the window's.
    let background = usize::from(lcd.lcdc & 0x08 != 0) == which;
    let window = usize::from(lcd.lcdc & 0x40 != 0) == which;
    let users = match (background, window) {
        (true, true) => "background and window",
        (true, false) => "background",
        (false, true) => "window",
        (false, false) => "unused",
    };
    let tiles = if lcd.lcdc & 0x10 != 0 {
        "tiles 0x8000 unsigned"
    } else {
        "tiles 0x8800 signed (0 at 0x9000)"
    };
    let window_on = if lcd.lcdc & 0x20 != 0 { "on" } else { "off" };
    let window_map = 0x9800 + usize::from(lcd.lcdc & 0x40 != 0) * 0x400;
    vec![
        format!(
            "Map 0x{address:04X} ({users}), LCDC 0x{:02X}: {tiles}, SCX {}, SCY {}; \
             the outline is the 160×144 screen",
            lcd.lcdc, lcd.scx, lcd.scy
        ),
        format!(
            "Window {window_on}: WX {} (screen x {}), WY {}, map 0x{window_map:04X}",
            lcd.wx,
            i16::from(lcd.wx) - 7,
            lcd.wy
        ),
    ]
}

/// One OAM entry (Pan Docs, "Object Attribute Memory (OAM)").
#[derive(Debug, Clone, Copy)]
struct Sprite {
    /// Byte 0: screen y + 16.
    y: u8,
    /// Byte 1: screen x + 8.
    x: u8,
    /// Byte 2: tile number in 0x8000–0x8FFF.
    tile: u8,
    /// Byte 3: priority, flips, DMG palette, CGB bank and palette.
    attributes: u8,
}

impl Sprite {
    /// OAM entry `index` (0–39).
    fn read(ppu: &Ppu, index: usize) -> Self {
        let byte = |offset: usize| ppu.read_oam(0xFE00 + (index * 4 + offset) as u16);
        Self {
            y: byte(0),
            x: byte(1),
            tile: byte(2),
            attributes: byte(3),
        }
    }
}

/// Draws `sprite` with its top-left corner at `corner`: one tile, or two
/// stacked when LCDC bit 2 makes sprites 8×16 (the top one with bit 0 of
/// the tile number clear). Flips and the palette come from its attributes;
/// colour 0 is transparent (Pan Docs, "Byte 3 — Attributes/Flags").
fn draw_sprite(frame: &mut Frame, ppu: &Ppu, lcd: &Lcd, sprite: Sprite, corner: (usize, usize)) {
    let cgb = is_cgb(ppu);
    let bank = if cgb && sprite.attributes & 0x08 != 0 {
        BANK_1
    } else {
        0
    };
    let [_, one, two, three] = if cgb {
        cgb_colors(ppu.obj_palette_ram(), sprite.attributes & 7)
    } else if sprite.attributes & 0x10 != 0 {
        dmg_colors(ppu, 2, lcd.obp1)
    } else {
        dmg_colors(ppu, 1, lcd.obp0)
    };
    let colors = [None, Some(one), Some(two), Some(three)];
    let flips = Flips::from_attributes(sprite.attributes);
    let tiles: &[u8] = if lcd.lcdc & 0x04 != 0 {
        &[sprite.tile & 0xFE, sprite.tile | 1]
    } else {
        &[sprite.tile]
    };
    for (slot, &tile) in tiles.iter().enumerate() {
        // A vertically flipped 8×16 sprite flips as a whole, so its bottom
        // tile shows on top.
        let slot = if flips.y {
            tiles.len() - 1 - slot
        } else {
            slot
        };
        let address = bank + usize::from(tile) * TILE_BYTES;
        let corner = (corner.0, corner.1 + slot * 8);
        draw_tile(frame, ppu.vram(), address, corner, flips, colors);
    }
}

/// One line about OAM entry `index`: its raw X and Y and where they put it
/// on screen (X − 8, Y − 16), its tile, and its attribute flags spelled out.
fn sprite_note(index: usize, sprite: Sprite, cgb: bool) -> String {
    let attributes = sprite.attributes;
    let mut note = format!(
        "{index:2}: X {} (screen {}), Y {} (screen {}), tile 0x{:02X}",
        sprite.x,
        i16::from(sprite.x) - 8,
        sprite.y,
        i16::from(sprite.y) - 16,
        sprite.tile
    );
    if cgb {
        note += &format!(
            ", palette {}, bank {}",
            attributes & 7,
            (attributes >> 3) & 1
        );
    } else if attributes & 0x10 != 0 {
        note += ", OBP1";
    } else {
        note += ", OBP0";
    }
    for (bit, flag) in [(0x20, "x-flip"), (0x40, "y-flip"), (0x80, "behind BG")] {
        if attributes & bit != 0 {
            note += ", ";
            note += flag;
        }
    }
    note
}

#[cfg(test)]
mod tests {
    use emu_core::ScreenSize;
    use emu_core::debug::draw::{BACKGROUND, CHECKER, OUTLINE};

    use super::*;
    use crate::ppu::DMG_SHADES;

    const RED: u16 = 0x001F;
    const GREEN: u16 = 0x03E0;
    const BLUE: u16 = 0x7C00;
    const WHITE: u16 = 0x7FFF;

    /// Writes a PPU register, discarding any interrupt it raises.
    fn write_register(ppu: &mut Ppu, address: u16, value: u8) {
        ppu.write_register(address, value, &mut 0);
    }

    /// Writes BGR555 `colors` into CGB palette RAM from byte `start`,
    /// through BCPS/BCPD (`select` 0xFF68) or OCPS/OCPD (0xFF6A).
    fn write_cgb_palette(ppu: &mut Ppu, select: u16, start: u8, colors: &[u16]) {
        write_register(ppu, select, 0x80 | start);
        for byte in colors.iter().flat_map(|color| color.to_le_bytes()) {
            write_register(ppu, select + 1, byte);
        }
    }

    /// Writes row `row` of the tile at CPU address `tile` so that pixel
    /// `column` has colour index `color`, the rest 0.
    fn write_tile_pixel(ppu: &mut Ppu, tile: u16, row: u16, column: u8, color: u8) {
        let bit = 0x80 >> column;
        let low = if color & 1 != 0 { bit } else { 0 };
        let high = if color & 2 != 0 { bit } else { 0 };
        ppu.write_vram(tile + row * 2, low);
        ppu.write_vram(tile + row * 2 + 1, high);
    }

    fn request(view: GraphicsView) -> GraphicsRequest {
        GraphicsRequest {
            view,
            ..GraphicsRequest::default()
        }
    }

    fn pixel(graphics: &Graphics, x: usize, y: usize) -> u32 {
        let width = usize::from(graphics.image.size().width);
        graphics.image.pixels()[y * width + x]
    }

    #[test]
    fn cgb_tiles_show_both_banks_in_the_chosen_palette() {
        let mut ppu = Ppu::after_boot(Model::Cgb);
        // OBJ palette 1 (selection 9): white, red, green, blue.
        write_cgb_palette(&mut ppu, 0xFF6A, 8, &[WHITE, RED, GREEN, BLUE]);
        write_tile_pixel(&mut ppu, 0x8010, 0, 0, 1); // bank 0, tile 1
        ppu.write_vram(0x8011, 0x40); // pixel 1 colour 2
        write_register(&mut ppu, 0xFF4F, 1);
        write_tile_pixel(&mut ppu, 0x8000, 7, 7, 3); // bank 1, tile 0

        let graphics = draw(
            &ppu,
            &GraphicsRequest {
                palette: 9,
                ..request(GraphicsView::Tiles)
            },
        )
        .expect("the Game Boy draws tiles");
        assert_eq!(graphics.image.size(), ScreenSize::new(256, 192));
        assert_eq!(pixel(&graphics, 8, 0), bgr555(RED));
        assert_eq!(pixel(&graphics, 9, 0), bgr555(GREEN));
        assert_eq!(pixel(&graphics, 10, 0), bgr555(WHITE));
        assert_eq!(pixel(&graphics, 128 + 7, 7), bgr555(BLUE), "bank 1");
        assert!(graphics.notes[0].contains("OBJ palette 1"));
    }

    #[test]
    fn dmg_tiles_use_the_chosen_register_through_the_dmg_shades() {
        let mut ppu = Ppu::after_boot(Model::Dmg);
        write_register(&mut ppu, 0xFF48, 0x1B); // OBP0: shade = 3 - index
        write_tile_pixel(&mut ppu, 0x8000, 0, 0, 3);
        let draw_with = |palette| {
            let request = GraphicsRequest {
                palette,
                ..request(GraphicsView::Tiles)
            };
            draw(&ppu, &request).expect("the Game Boy draws tiles")
        };
        let obp0 = draw_with(1);
        assert_eq!(obp0.image.size(), ScreenSize::new(128, 192));
        assert_eq!(pixel(&obp0, 0, 0), DMG_SHADES[0]);
        assert_eq!(pixel(&obp0, 1, 0), DMG_SHADES[3]);
        assert!(obp0.notes[0].contains("OBP0 (0x1B)"));
        // BGP after boot is 0xFC: index 0 is shade 0, the rest shade 3.
        let bgp = draw_with(0);
        assert_eq!(pixel(&bgp, 0, 0), DMG_SHADES[3]);
        assert_eq!(pixel(&bgp, 1, 0), DMG_SHADES[0]);
        assert!(draw_with(7).notes[0].contains("OBP1"), "clamped");
    }

    #[test]
    fn cgb_map_applies_attribute_flips_and_outlines_the_screen() {
        let mut ppu = Ppu::after_boot(Model::Cgb);
        // BG palette 2, colour 1 is red; the others stay white.
        write_cgb_palette(&mut ppu, 0xFF68, 2 * 8 + 2, &[RED]);
        write_tile_pixel(&mut ppu, 0x8010, 0, 0, 1); // tile 1: top-left pixel
        ppu.write_vram(0x9822, 1); // map cell (2, 1)
        write_register(&mut ppu, 0xFF4F, 1);
        ppu.write_vram(0x9822, 0x60 | 2); // both flips, palette 2
        write_register(&mut ppu, 0xFF43, 250); // SCX

        let graphics = draw(&ppu, &request(GraphicsView::Map)).expect("maps draw");
        assert_eq!(graphics.image.size(), ScreenSize::new(256, 256));
        assert_eq!(pixel(&graphics, 2 * 8 + 7, 8 + 7), bgr555(RED), "flipped");
        assert_eq!(pixel(&graphics, 2 * 8, 8), bgr555(WHITE));
        // The screen's outline wraps from x 250 round to x 153.
        assert_eq!(pixel(&graphics, 250, 0), OUTLINE);
        assert_eq!(pixel(&graphics, 255, 143), OUTLINE);
        assert_eq!(pixel(&graphics, 153, 70), OUTLINE);
        assert_eq!(pixel(&graphics, 154, 70), bgr555(WHITE));
        assert!(graphics.notes[0].starts_with("Map 0x9800 (background and window)"));
        assert!(graphics.notes[0].contains("SCX 250, SCY 0"));
    }

    #[test]
    fn sprites_show_each_entry_in_its_cell_with_its_attributes() {
        let mut ppu = Ppu::after_boot(Model::Dmg);
        write_register(&mut ppu, 0xFF49, 0xE4); // OBP1: shade = index
        write_tile_pixel(&mut ppu, 0x8020, 0, 0, 3); // tile 2
        // Sprite 9 at screen (30, 20): tile 2, OBP1, x-flip.
        for (offset, value) in [36, 38, 2, 0x30].into_iter().enumerate() {
            ppu.write_oam(0xFE00 + 9 * 4 + offset as u16, value);
        }

        let graphics = draw(&ppu, &request(GraphicsView::Sprites)).expect("sprites draw");
        assert_eq!(graphics.image.size(), ScreenSize::new(73, 86));
        // Sprite 9 is in column 1, row 1: its cell starts at (10, 18).
        assert_eq!(pixel(&graphics, 10 + 7, 18), DMG_SHADES[3], "x-flipped");
        assert_eq!(
            pixel(&graphics, 10, 18),
            CHECKER[0],
            "colour 0 shows through"
        );
        assert_eq!(pixel(&graphics, 9, 18), BACKGROUND, "the gap");
        assert_eq!(graphics.notes.len(), 41);
        assert_eq!(
            graphics.notes[10],
            " 9: X 38 (screen 30), Y 36 (screen 20), tile 0x02, OBP1, x-flip"
        );
    }

    #[test]
    fn cgb_sprites_take_their_bank_and_palette() {
        let mut ppu = Ppu::after_boot(Model::Cgb);
        write_cgb_palette(&mut ppu, 0xFF6A, 3 * 8 + 2, &[GREEN]);
        write_register(&mut ppu, 0xFF4F, 1);
        write_tile_pixel(&mut ppu, 0x8000, 0, 0, 1); // bank 1, tile 0
        ppu.write_oam(0xFE03, 0x08 | 3 | 0x80); // sprite 0: bank 1, palette 3, behind BG

        let graphics = draw(&ppu, &request(GraphicsView::Sprites)).expect("sprites draw");
        assert_eq!(pixel(&graphics, 1, 1), bgr555(GREEN));
        assert_eq!(
            graphics.notes[1],
            " 0: X 0 (screen -8), Y 0 (screen -16), tile 0x00, palette 3, bank 1, behind BG"
        );
    }

    #[test]
    fn palettes_show_every_colour_with_its_value() {
        let mut ppu = Ppu::after_boot(Model::Cgb);
        write_cgb_palette(&mut ppu, 0xFF6A, 7 * 8 + 6, &[RED]); // OBJ 7, colour 3
        let graphics = draw(&ppu, &request(GraphicsView::Palettes)).expect("palettes draw");
        assert_eq!(graphics.image.size(), ScreenSize::new(69, 273));
        assert_eq!(pixel(&graphics, 3 * 17 + 1, 15 * 17 + 1), bgr555(RED));
        assert_eq!(pixel(&graphics, 3 * 17 + 16, 15 * 17 + 16), bgr555(RED));
        assert_eq!(pixel(&graphics, 3 * 17, 15 * 17 + 1), BACKGROUND);
        assert_eq!(graphics.notes[16], "OBJ 7: 0x7FFF 0x7FFF 0x7FFF 0x001F");

        let dmg = draw(
            &Ppu::after_boot(Model::Dmg),
            &request(GraphicsView::Palettes),
        )
        .expect("palettes draw");
        assert_eq!(dmg.image.size(), ScreenSize::new(69, 52));
        assert_eq!(pixel(&dmg, 1, 1), DMG_SHADES[0]);
        assert_eq!(pixel(&dmg, 18, 1), DMG_SHADES[3]);
        assert_eq!(dmg.notes[1], "BGP 0xFC: shades 0, 3, 3, 3");
    }
}
