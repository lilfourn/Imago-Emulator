//! Pictures of video memory for the debugger's graphics viewers: tiles
//! anywhere in VRAM, a 2D engine's text backgrounds, its sprites and its
//! palettes (GBATEK, "DS Memory Control - VRAM", "DS Video BG Modes /
//! Control", "DS Video OBJs", "DS Video Extended Palettes", and the GBA
//! chapters they build on: "LCD VRAM Character Data", "LCD VRAM BG Screen
//! Data Format", "LCD OBJ - OAM Attributes" and "LCD OBJ - VRAM Character
//! (Tile) Mapping").
//!
//! [`draw`] reads the hardware without changing it and returns plain data:
//! a [`Frame`](emu_core::Frame) and lines of notes. Everything is decoded
//! from VRAM, OAM, palette RAM and the registers, the way the 2D engines
//! decode them; what the DS shares with the GBA is decoded by
//! [`emu_core::debug::gba_video`].

use emu_core::debug::draw::blank;
use emu_core::debug::gba_video::{
    self, OBJ_PALETTE, SPRITES, Sprite, TILE_BLOCK, draw_sprite, palette_sheet, sprite_sheet,
    tile_sheet, tile_texel,
};
use emu_core::debug::{Graphics, GraphicsRequest, GraphicsView};
use emu_core::rgb;

use crate::gpu2d::Engine2d;
use crate::hardware::Hardware;
use crate::vram::{Engine, EngineMemory, EngineView, Region, Vram};

/// Where VRAM starts on the ARM9's bus.
const VRAM_ADDRESS: u32 = 0x0600_0000;
/// Bank names, by VRAMCNT number.
const BANK_NAMES: [&str; 9] = ["A", "B", "C", "D", "E", "F", "G", "H", "I"];

/// Draws `request`'s view of the video memory of `request.engine`'s 2D
/// engine (0 for A, anything else for B), or of any VRAM for the tile
/// view. The DS can draw every view, so this always returns a picture.
pub(crate) fn draw(hw: &Hardware, request: &GraphicsRequest) -> Option<Graphics> {
    let engine = EngineParts::select(hw, request.engine);
    Some(match request.view {
        GraphicsView::Tiles => tiles(&hw.vram, &engine, request),
        GraphicsView::Map => map(&engine, request.layer),
        GraphicsView::Sprites => sprites(&engine),
        GraphicsView::Palettes => palettes(&engine),
    })
}

// ----- Views -----------------------------------------------------------

/// 32 KiB of tiles from ARM9 address `request.base`, 32 per row: 1024 4bpp
/// tiles (256×256) or 512 8bpp tiles (256×128), coloured from the engine's
/// palette RAM as [`tile_palette`] says.
fn tiles(vram: &Vram, engine: &EngineParts<'_>, request: &GraphicsRequest) -> Graphics {
    // Anything outside 0x06xxxxxx is taken as an offset into VRAM.
    let base = VRAM_ADDRESS | (request.base & 0x00FF_FFE0);
    let bytes = read_arm9_vram(vram, base, TILE_BLOCK as u32);
    let eight_bit = request.eight_bit;
    let (first_color, palette_name) = tile_palette(request.palette, eight_bit);
    let (frame, count) = tile_sheet(
        |offset| bytes[offset],
        eight_bit,
        |index| engine.color(first_color + usize::from(index)),
    );
    let bpp = if eight_bit { "8bpp" } else { "4bpp" };
    Graphics {
        image: frame,
        notes: vec![
            format!(
                "Tiles at 0x{base:08X} ({}): {count} {bpp} tiles, 32 per row, coloured with \
                 engine {}'s {palette_name}",
                place(base),
                engine.letter()
            ),
            format!(
                "VRAM there: {}; unmapped bytes read 0",
                banks_mapped(vram, base, TILE_BLOCK as u32)
            ),
        ],
    }
}

/// Background `layer` (0–3; larger numbers mean 3) of the engine: a text
/// background's whole map, or a note saying what the layer is in the
/// current BG mode when that is something else.
fn map(engine: &EngineParts<'_>, layer: u8) -> Graphics {
    let layer = u32::from(layer.min(3));
    let dispcnt = engine.dispcnt();
    let control = engine.gpu.read_register(0x08 + layer * 2);
    let what = match bg_kind(engine.engine, dispcnt, layer) {
        Some(BgKind::Text) => return text_map(engine, layer, control),
        Some(BgKind::ThreeD) => "the 3D layer",
        Some(BgKind::Affine) => "an affine background",
        Some(BgKind::Extended) => "an extended background (tiles or bitmap)",
        Some(BgKind::Large) => "the large bitmap",
        None => "not used",
    };
    Graphics {
        image: blank(8, 8),
        notes: vec![format!(
            "{} BG{layer} is {what} in mode {}; only text backgrounds are drawn yet",
            engine.name(),
            dispcnt & 7
        )],
    }
}

/// Every OAM entry of the engine in a grid of 16 columns and 8 rows of
/// 64×64 cells, drawn unrotated at the cell's top-left over a checkerboard
/// that shows through transparent pixels, with one note per sprite that is
/// not disabled.
fn sprites(engine: &EngineParts<'_>) -> Graphics {
    let dispcnt = engine.dispcnt();
    let mut frame = sprite_sheet();
    let mut notes = vec![sprites_summary(engine, dispcnt)];
    for index in 0..SPRITES {
        let sprite = Sprite::read(|offset| engine.gpu.read_oam(offset), index);
        if sprite.disabled() {
            continue;
        }
        let layout = layout(dispcnt, sprite);
        // Alpha-OAM 0 hides a bitmap sprite.
        let hidden = is_bitmap(sprite) && sprite.palette() == 0;
        if let Some(layout) = &layout
            && !hidden
        {
            let size = (layout.width, layout.height);
            draw_sprite(&mut frame, index, size, sprite.flips(), |x, y| {
                sprite_texel(engine, layout, x, y)
            });
        }
        notes.push(sprite_note(
            index,
            sprite,
            dispcnt,
            layout.is_none(),
            hidden,
        ));
    }
    Graphics {
        image: frame,
        notes,
    }
}

/// The engine's 512 standard palette colours, one 8×8 swatch each: the
/// 256 BG colours on the left and the 256 OBJ colours on the right, each a
/// 16×16 grid with one 16-colour palette per row.
fn palettes(engine: &EngineParts<'_>) -> Graphics {
    let bg = engine.palette_address();
    Graphics {
        image: palette_sheet(|index| engine.color(index)),
        notes: vec![
            format!(
                "{}: BG 0x{bg:08X}, OBJ 0x{:08X}",
                engine.name(),
                bg + OBJ_PALETTE as u32 * 2
            ),
            "Each row is one 16-colour palette, 0–15 from the top; BG colour 0 is the \
             backdrop. Extended palettes (in VRAM) are not shown"
                .to_string(),
        ],
    }
}

// ----- Engines, memory and colours ---------------------------------------

/// One 2D engine and what it reads: its registers, palette RAM and OAM,
/// and its view of VRAM.
struct EngineParts<'a> {
    engine: Engine,
    gpu: &'a Engine2d,
    memory: EngineView<'a>,
}

impl<'a> EngineParts<'a> {
    /// Engine A for `number` 0, engine B for anything else.
    fn select(hw: &'a Hardware, number: u8) -> Self {
        let (engine, gpu) = if number == 0 {
            (Engine::A, &hw.engine_a)
        } else {
            (Engine::B, &hw.engine_b)
        };
        Self {
            engine,
            gpu,
            memory: hw.vram.engine(engine),
        }
    }

    /// "Engine A" or "Engine B".
    fn name(&self) -> &'static str {
        match self.engine {
            Engine::A => "Engine A",
            Engine::B => "Engine B",
        }
    }

    /// 'A' or 'B'.
    fn letter(&self) -> char {
        match self.engine {
            Engine::A => 'A',
            Engine::B => 'B',
        }
    }

    /// DISPCNT, both halves.
    fn dispcnt(&self) -> u32 {
        u32::from(self.gpu.read_register(0x00)) | u32::from(self.gpu.read_register(0x02)) << 16
    }

    /// Where the ARM9 sees this engine's palette RAM; its OAM is at the
    /// same offset from 0x07000000.
    fn palette_address(&self) -> u32 {
        match self.engine {
            Engine::A => 0x0500_0000,
            Engine::B => 0x0500_0400,
        }
    }

    /// Where the ARM9 sees this engine's BG VRAM.
    fn bg_address(&self) -> u32 {
        match self.engine {
            Engine::A => 0x0600_0000,
            Engine::B => 0x0620_0000,
        }
    }

    /// Standard palette colour `index` (0–255 BG, 256–511 OBJ) as a pixel.
    fn color(&self, index: usize) -> u32 {
        bgr555(self.gpu.read_palette(index as u32 * 2))
    }

    /// The byte at `offset` in the engine's BG VRAM.
    fn bg_byte(&self, offset: u32) -> u8 {
        (self.memory.bg(offset) >> ((offset & 1) * 8)) as u8
    }

    /// The byte at `offset` in the engine's OBJ VRAM.
    fn obj_byte(&self, offset: u32) -> u8 {
        (self.memory.obj(offset) >> ((offset & 1) * 8)) as u8
    }
}

/// A BGR555 colour as a pixel, as the 2D engines show it: each 5-bit
/// channel widens to the LCD's 6 bits (`x << 1`, plus 1 if `x` is not 0),
/// then to 8 (GBATEK, "DS Video Stuff"). Bit 15 is ignored.
fn bgr555(value: u16) -> u32 {
    let channel = |shift: u16| {
        let x = (value >> shift) & 0x1F;
        let six = x << 1 | u16::from(x != 0);
        (six << 2 | six >> 4) as u8
    };
    rgb(channel(0), channel(5), channel(10))
}

/// `length` bytes from ARM9 address `start` on, read as the ARM9 reads
/// VRAM: through whichever banks VRAMCNT maps there, ORed where several
/// overlap, 0 where none is (GBATEK, "DS Memory Control - VRAM").
fn read_arm9_vram(vram: &Vram, start: u32, length: u32) -> Vec<u8> {
    (0..length)
        .map(|offset| {
            let address = start.wrapping_add(offset);
            if address >> 24 != VRAM_ADDRESS >> 24 {
                return 0;
            }
            Vram::arm9_region(address).map_or(0, |(region, offset)| vram.read_8(region, offset))
        })
        .collect()
}

/// What ARM9 address `address` in VRAM reaches, such as "engine A BG
/// +0x08000".
fn place(address: u32) -> String {
    match Vram::arm9_region(address) {
        Some((region, offset)) => format!("{} +0x{offset:05X}", region_name(region)),
        None => "past the LCDC area".to_string(),
    }
}

/// A short name for a VRAM region.
fn region_name(region: Region) -> &'static str {
    match region {
        Region::Lcdc => "LCDC",
        Region::BgA => "engine A BG",
        Region::ObjA => "engine A OBJ",
        Region::BgB => "engine B BG",
        Region::ObjB => "engine B OBJ",
        Region::Arm7 => "ARM7",
        Region::Texture => "3D textures",
        Region::TexturePalette => "3D texture palettes",
        Region::BgExtPaletteA => "engine A BG extended palettes",
        Region::BgExtPaletteB => "engine B BG extended palettes",
        Region::ObjExtPaletteA => "engine A OBJ extended palette",
        Region::ObjExtPaletteB => "engine B OBJ extended palette",
    }
}

/// The banks mapped anywhere in the `length` bytes from ARM9 address
/// `start`, such as "banks A, E", or "no banks".
fn banks_mapped(vram: &Vram, start: u32, length: u32) -> String {
    // One address in each 8 KiB page the range touches.
    let probes = (0..length).step_by(0x2000).chain([length - 1]);
    let mask = probes
        .filter_map(|offset| Vram::arm9_region(start.wrapping_add(offset)))
        .fold(0, |mask, (region, offset)| {
            mask | vram.banks_at(region, offset)
        });
    let names: Vec<&str> = (0..BANK_NAMES.len())
        .filter(|&bank| mask & (1 << bank) != 0)
        .map(|bank| BANK_NAMES[bank])
        .collect();
    match names.len() {
        0 => "no banks".to_string(),
        1 => format!("bank {}", names[0]),
        _ => format!("banks {}", names.join(", ")),
    }
}

/// The first palette colour the tile picture uses, and its name: 4bpp
/// tiles use 16-colour palette `palette` (0–15 BG, 16–31 OBJ, wrapping
/// past 31); 8bpp tiles use the 256-colour BG palette for `palette` 0–15
/// and OBJ's for 16–31.
fn tile_palette(palette: u8, eight_bit: bool) -> (usize, String) {
    let palette = usize::from(palette & 31);
    match (eight_bit, palette) {
        (true, 0..=15) => (0, "BG 256-colour palette".to_string()),
        (true, _) => (OBJ_PALETTE, "OBJ 256-colour palette".to_string()),
        (false, 0..=15) => (palette * 16, format!("BG palette {palette}")),
        (false, _) => (
            OBJ_PALETTE + (palette - 16) * 16,
            format!("OBJ palette {}", palette - 16),
        ),
    }
}

// ----- Backgrounds -------------------------------------------------------

/// What a background shows, by BG mode and number (GBATEK, "DS Video BG
/// Modes / Control").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BgKind {
    /// Scrolling tiles with 16-bit map entries.
    Text,
    /// The 3D engine's output (engine A's BG0 with DISPCNT bit 3).
    ThreeD,
    /// Rotation/scaling tiles with 8-bit map entries.
    Affine,
    /// Extended rotation/scaling: 16-bit map entries or a bitmap.
    Extended,
    /// Mode 6's 256-colour bitmap filling all 512 KiB (engine A).
    Large,
}

/// What background `layer` is under `dispcnt`, whether or not it is shown,
/// or `None` if the BG mode has no such background.
fn bg_kind(engine: Engine, dispcnt: u32, layer: u32) -> Option<BgKind> {
    // Engine B's DISPCNT has no bit 3.
    if layer == 0 && dispcnt & (1 << 3) != 0 {
        return Some(BgKind::ThreeD);
    }
    match (dispcnt & 7, layer) {
        (0..=5, 0 | 1) | (0, _) | (1 | 3, 2) => Some(BgKind::Text),
        (1, 3) | (2, 2 | 3) | (4, 2) => Some(BgKind::Affine),
        (3..=5, 3) | (5, 2) => Some(BgKind::Extended),
        (6, 2) if engine == Engine::A => Some(BgKind::Large),
        _ => None,
    }
}

/// A text background's whole map, 256 or 512 pixels each way. Colour 0
/// shows the backdrop. 256-colour tiles use the extended BG palettes when
/// DISPCNT bit 30 enables them, as the engine does.
fn text_map(engine: &EngineParts<'_>, layer: u32, control: u16) -> Graphics {
    let dispcnt = engine.dispcnt();
    let control_bits = u32::from(control);
    // Engine A adds DISPCNT's 64 KiB steps; engine B's DISPCNT lacks them.
    let char_base = ((control_bits >> 2) & 0xF) * 0x4000 + ((dispcnt >> 24) & 7) * 0x1_0000;
    let screen_base = ((control_bits >> 8) & 0x1F) * 0x800 + ((dispcnt >> 27) & 7) * 0x1_0000;
    let eight_bit = control & (1 << 7) != 0;
    // BG0 and BG1 may use extended palette slots 2 and 3 instead of their own.
    let slot = if layer < 2 && control & (1 << 13) != 0 {
        layer + 2
    } else {
        layer
    };
    let ext_slot = (eight_bit && dispcnt & (1 << 30) != 0).then_some(slot);
    let backdrop = engine.color(0);
    let frame = gba_video::text_map(
        control,
        |offset| engine.memory.bg(screen_base + offset as u32),
        |offset| engine.bg_byte(char_base + offset as u32),
        |palette, index| {
            let index = usize::from(index);
            match ext_slot {
                _ if index == 0 => backdrop,
                Some(slot) => {
                    let color = (palette * 256 + index) as u32;
                    bgr555(engine.memory.bg_ext_palette(slot, color))
                }
                None if eight_bit => engine.color(index),
                None => engine.color(palette * 16 + index),
            }
        },
    );
    let size = frame.size();
    let (width, height) = (size.width, size.height);
    let bpp = match (eight_bit, ext_slot) {
        (false, _) => "4bpp".to_string(),
        (true, None) => "8bpp".to_string(),
        (true, Some(slot)) => format!("8bpp, extended palette slot {slot}"),
    };
    let shown = if dispcnt & (0x100 << layer) != 0 {
        "on"
    } else {
        "off"
    };
    Graphics {
        image: frame,
        notes: vec![format!(
            "{} BG{layer}: text, {width}×{height}, {bpp}, map 0x{:08X}, tiles 0x{:08X}, \
             priority {}, {shown}; the screen is not outlined",
            engine.name(),
            engine.bg_address() + screen_base,
            engine.bg_address() + char_base,
            control & 3
        )],
    }
}

// ----- Sprites -----------------------------------------------------------

/// A direct-colour bitmap sprite: mode 3, which the GBA prohibits (GBATEK,
/// "DS Video OBJs").
fn is_bitmap(sprite: Sprite) -> bool {
    sprite.mode() == 3
}

/// How a sprite's texels are stored.
#[derive(Debug, Clone, Copy)]
enum Texels {
    /// 16-colour tiles using standard OBJ palette `palette` (0–15).
    Tiles16 { palette: usize },
    /// 256-colour tiles; `ext_base` is the first colour of the sprite's
    /// extended palette when DISPCNT bit 31 enables extended OBJ palettes.
    Tiles256 { ext_base: Option<u32> },
    /// Direct colour with bit 15 as alpha (clear is transparent).
    Bitmap,
}

/// Where a sprite's texels are in OBJ VRAM.
#[derive(Debug, Clone, Copy)]
struct Layout {
    texels: Texels,
    /// Byte offset of the top-left texel.
    base: u32,
    /// Bytes from one row of 8×8 tiles to the next, or from one bitmap row
    /// to the next.
    stride: u32,
    /// Address mask: 2D tile mapping wraps within 32 KiB.
    mask: u32,
    /// The sprite's width in pixels.
    width: usize,
    /// The sprite's height in pixels.
    height: usize,
}

/// Where `sprite`'s texels are, from its attributes and DISPCNT's mapping
/// bits (4, 5–6, 20–22, 31); `None` for the prohibited shape and the
/// reserved bitmap mapping (GBATEK, "DS Video OBJs").
fn layout(dispcnt: u32, sprite: Sprite) -> Option<Layout> {
    let (width, height) = sprite.size()?;
    let tile = u32::from(sprite.tile());
    let (texels, base, stride, mask) = if is_bitmap(sprite) {
        let (base, stride) = match (dispcnt >> 5) & 3 {
            // 2D: one 128- or 256-dot-wide bitmap. The tile number's low 4
            // or 5 bits pick an 8-dot column, the rest an 8-dot row.
            dimension @ (0 | 1) => {
                let mask = if dimension == 0 { 0xF } else { 0x1F };
                (
                    (tile & mask) * 0x10 + (tile & !mask) * 0x80,
                    (mask + 1) * 16,
                )
            }
            // 1D: the tile number counts 128- or 256-byte units.
            2 => (tile * (128 << ((dispcnt >> 22) & 1)), width as u32 * 2),
            _ => return None,
        };
        (Texels::Bitmap, base, stride, u32::MAX)
    } else {
        let eight_bit = sprite.eight_bit();
        let texels = if eight_bit {
            Texels::Tiles256 {
                ext_base: (dispcnt & (1 << 31) != 0).then_some(u32::from(sprite.palette()) * 256),
            }
        } else {
            Texels::Tiles16 {
                palette: usize::from(sprite.palette()),
            }
        };
        if dispcnt & (1 << 4) != 0 {
            // 1D: a sprite's tiles follow each other, from the tile number
            // in units of the boundary (DISPCNT bits 20–21: 32–256 bytes).
            let tile_bytes = if eight_bit { 64 } else { 32 };
            let base = tile * (32 << ((dispcnt >> 20) & 3));
            (texels, base, width as u32 / 8 * tile_bytes, u32::MAX)
        } else {
            // 2D: a sheet 32 tiles wide in the first 32 KiB.
            (texels, tile * 32, 32 * 32, 0x7FFF)
        }
    };
    Some(Layout {
        texels,
        base,
        stride,
        mask,
        width,
        height,
    })
}

/// The colour of texel (`x`, `y`) of a sprite laid out as `layout`, `None`
/// where transparent.
fn sprite_texel(engine: &EngineParts<'_>, layout: &Layout, x: usize, y: usize) -> Option<u32> {
    // The same texel for arithmetic on OBJ VRAM offsets, which are 32 bits.
    let (wide_x, wide_y) = (x as u32, y as u32);
    match layout.texels {
        Texels::Bitmap => {
            let color = engine
                .memory
                .obj(layout.base + wide_y * layout.stride + wide_x * 2);
            (color & 0x8000 != 0).then(|| bgr555(color))
        }
        Texels::Tiles16 { palette } => {
            let tile = layout.base + wide_y / 8 * layout.stride + wide_x / 8 * 32;
            let read = |offset: usize| engine.obj_byte(offset as u32 & layout.mask);
            let index = usize::from(tile_texel(&read, tile as usize, false, x % 8, y % 8));
            (index != 0).then(|| engine.color(OBJ_PALETTE + palette * 16 + index))
        }
        Texels::Tiles256 { ext_base } => {
            let tile = layout.base + wide_y / 8 * layout.stride + wide_x / 8 * 64;
            let read = |offset: usize| engine.obj_byte(offset as u32 & layout.mask);
            let index = tile_texel(&read, tile as usize, true, x % 8, y % 8);
            match ext_base {
                _ if index == 0 => None,
                Some(base) => Some(bgr555(
                    engine.memory.obj_ext_palette(base + u32::from(index)),
                )),
                None => Some(engine.color(OBJ_PALETTE + usize::from(index))),
            }
        }
    }
}

/// The sprite picture's first note: the engine's mapping settings.
fn sprites_summary(engine: &EngineParts<'_>, dispcnt: u32) -> String {
    let tiles = if dispcnt & (1 << 4) != 0 {
        format!("1D in {}-byte steps", 32 << ((dispcnt >> 20) & 3))
    } else {
        "2D".to_string()
    };
    let bitmaps = match (dispcnt >> 5) & 3 {
        0 => "2D 128 wide".to_string(),
        1 => "2D 256 wide".to_string(),
        2 => format!("1D in {}-byte steps", 128 << ((dispcnt >> 22) & 1)),
        _ => "reserved".to_string(),
    };
    let shown = if dispcnt & (1 << 12) != 0 {
        "on"
    } else {
        "off"
    };
    let extended = if dispcnt & (1 << 31) != 0 {
        "on"
    } else {
        "off"
    };
    format!(
        "{}: 128 sprites; tiles {tiles} (DISPCNT bits 4, 20–21), bitmaps {bitmaps} (bits 5–6, \
         22), extended palettes {extended} (bit 31), objects {shown} (bit 12); affine sprites \
         are drawn unrotated",
        engine.name()
    )
}

/// One line about OAM entry `index`: position, size and shape, tile,
/// colours, priority, mode, rotation or flips, and mosaic. `unmapped` says
/// it uses the reserved bitmap mapping; `hidden` that Alpha-OAM 0 hides
/// it.
fn sprite_note(index: usize, sprite: Sprite, dispcnt: u32, unmapped: bool, hidden: bool) -> String {
    let colors = match (is_bitmap(sprite), sprite.eight_bit()) {
        (true, _) => format!("alpha {}", sprite.palette()),
        (false, true) if dispcnt & (1 << 31) != 0 => {
            format!("8bpp, extended palette {}", sprite.palette())
        }
        (false, true) => "8bpp".to_string(),
        (false, false) => format!("palette {}", sprite.palette()),
    };
    // Rows count within 256 lines, so a sprite crossing the bottom edge
    // shows at the top as well: y is told as stored.
    sprite.note(
        index,
        i32::from(sprite.y()),
        &colors,
        "bitmap",
        &[
            (unmapped, "reserved bitmap mapping, not drawn"),
            (hidden, "hidden (alpha 0)"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use emu_core::ScreenSize;
    use emu_core::debug::draw::CHECKER;
    use emu_core::debug::gba_video::CELL_SHADES;

    use super::*;

    const RED: u16 = 0x001F;
    const GREEN: u16 = 0x03E0;
    const BLUE: u16 = 0x7C00;

    /// VRAMCNT_A; bank n's is n bytes on.
    const VRAMCNT: u32 = 0x0400_0240;

    fn hardware() -> Hardware {
        Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into())
    }

    fn request(view: GraphicsView) -> GraphicsRequest {
        GraphicsRequest {
            view,
            ..GraphicsRequest::default()
        }
    }

    fn draw_request(hw: &Hardware, request: GraphicsRequest) -> Graphics {
        draw(hw, &request).expect("the DS draws every view")
    }

    fn tiles_at(hw: &Hardware, base: u32) -> Graphics {
        draw_request(
            hw,
            GraphicsRequest {
                base,
                ..request(GraphicsView::Tiles)
            },
        )
    }

    fn pixel(graphics: &Graphics, x: usize, y: usize) -> u32 {
        let width = usize::from(graphics.image.size().width);
        graphics.image.pixels()[y * width + x]
    }

    /// Hides every sprite of engine A, as games do: all-zero OAM entries
    /// are visible 8×8 sprites.
    fn hide_sprites(hw: &mut Hardware) {
        for index in 0..128 {
            hw.arm9_write_16(0x0700_0000 + index * 8, 1 << 9);
        }
    }

    #[test]
    fn tiles_read_the_chosen_base_as_the_arm9_does() {
        let mut hw = hardware();
        hw.arm9_write_8(VRAMCNT, 0x80); // bank A in the LCDC area
        hw.arm9_write_16(0x0680_0020, 0x0021); // tile 1: pixels 0 and 1 are 1 and 2
        hw.arm9_write_16(0x0500_0002, RED); // engine A BG colour 1
        let lcdc = tiles_at(&hw, 0x0680_0000);
        assert_eq!(lcdc.image.size(), ScreenSize::new(256, 256));
        assert_eq!(pixel(&lcdc, 8, 0), bgr555(RED));
        assert_eq!(pixel(&lcdc, 9, 0), bgr555(0));
        assert!(lcdc.notes[0].starts_with("Tiles at 0x06800000 (LCDC +0x00000)"));
        assert_eq!(lcdc.notes[1], "VRAM there: bank A; unmapped bytes read 0");
        let unmapped = tiles_at(&hw, 0x0600_0000);
        assert_eq!(pixel(&unmapped, 8, 0), bgr555(0));
        assert!(unmapped.notes[1].starts_with("VRAM there: no banks"));

        hw.arm9_write_8(VRAMCNT, 0x81); // bank A as engine A BG
        let background = tiles_at(&hw, 0x0600_0000);
        assert_eq!(pixel(&background, 8, 0), bgr555(RED));
        assert!(background.notes[0].contains("(engine A BG +0x00000)"));
        assert_eq!(pixel(&tiles_at(&hw, 0x0680_0000), 8, 0), bgr555(0));
    }

    #[test]
    fn tiles_take_the_chosen_engines_palette() {
        let mut hw = hardware();
        hw.arm9_write_8(VRAMCNT + 2, 0x84); // bank C as engine B BG
        hw.arm9_write_16(0x0620_0000, 0x0005); // 8bpp tile 0: pixel 0 is 5
        hw.arm9_write_16(0x0500_0000 + 5 * 2, RED); // engine A BG colour 5
        hw.arm9_write_16(0x0500_0400 + 5 * 2, GREEN); // engine B BG colour 5
        hw.arm9_write_16(0x0500_0600 + 5 * 2, BLUE); // engine B OBJ colour 5
        let tiles = |engine, palette| {
            draw_request(
                &hw,
                GraphicsRequest {
                    engine,
                    base: 0x0620_0000,
                    eight_bit: true,
                    palette,
                    ..request(GraphicsView::Tiles)
                },
            )
        };
        let engine_b = tiles(1, 0);
        assert_eq!(engine_b.image.size(), ScreenSize::new(256, 128));
        assert_eq!(pixel(&engine_b, 0, 0), bgr555(GREEN));
        assert!(engine_b.notes[0].contains("engine B's BG 256-colour palette"));
        assert_eq!(
            engine_b.notes[1],
            "VRAM there: bank C; unmapped bytes read 0"
        );
        assert_eq!(pixel(&tiles(0, 0), 0, 0), bgr555(RED));
        assert_eq!(pixel(&tiles(1, 16), 0, 0), bgr555(BLUE));
    }

    #[test]
    fn palettes_show_the_chosen_engines_colours() {
        let mut hw = hardware();
        hw.arm9_write_16(0x0500_0600 + 2, RED); // engine B OBJ colour 1
        let palettes = |engine| {
            draw_request(
                &hw,
                GraphicsRequest {
                    engine,
                    ..request(GraphicsView::Palettes)
                },
            )
        };
        let engine_b = palettes(1);
        assert_eq!(engine_b.image.size(), ScreenSize::new(264, 128));
        assert_eq!(pixel(&engine_b, 136 + 8, 0), bgr555(RED));
        assert_eq!(engine_b.notes[0], "Engine B: BG 0x05000400, OBJ 0x05000600");
        assert_eq!(pixel(&palettes(0), 136 + 8, 0), bgr555(0));
    }

    #[test]
    fn sprites_draw_from_the_engines_obj_vram_with_a_note_each() {
        let mut hw = hardware();
        hw.arm9_write_8(VRAMCNT + 1, 0x82); // bank B as engine A OBJ
        hide_sprites(&mut hw);
        hw.arm9_write_16(0x0400_0000, 1 << 4 | 1 << 12); // 1D tiles, objects on
        hw.arm9_write_16(0x0640_0020, 0x0001); // tile 1: top-left pixel
        hw.arm9_write_16(0x0500_0200 + (2 * 16 + 1) * 2, GREEN); // OBJ palette 2, colour 1
        // Sprite 1: 8×8 at (5, 10), tile 1, palette 2, flipped top to bottom.
        hw.arm9_write_16(0x0700_0008, 10);
        hw.arm9_write_16(0x0700_000A, 5 | 1 << 13);
        hw.arm9_write_16(0x0700_000C, 1 | 2 << 12);

        let graphics = draw_request(&hw, request(GraphicsView::Sprites));
        assert_eq!(graphics.image.size(), ScreenSize::new(1024, 512));
        assert_eq!(pixel(&graphics, 64, 7), bgr555(GREEN), "flipped");
        assert_eq!(pixel(&graphics, 64, 0), CHECKER[0]);
        assert_eq!(
            pixel(&graphics, 64 + 8, 0),
            CELL_SHADES[1],
            "past its width"
        );
        assert_eq!(
            graphics.notes[1..],
            ["  1: x 5, y 10, 8×8 square, tile 0x001, palette 2, priority 0, normal, v-flip"]
        );
        assert!(graphics.notes[0].starts_with("Engine A: 128 sprites; tiles 1D in 32-byte steps"));
    }

    #[test]
    fn bitmap_sprites_draw_direct_colour_unless_alpha_hides_them() {
        let mut hw = hardware();
        hw.arm9_write_8(VRAMCNT + 1, 0x82); // bank B as engine A OBJ
        hide_sprites(&mut hw);
        hw.arm9_write_16(0x0400_0000, 2 << 5 | 1 << 12); // 1D bitmaps in 128 bytes
        hw.arm9_write_16(0x0640_0100, 0x8000 | RED); // bitmap tile 2, pixel 0
        hw.arm9_write_16(0x0640_0102, BLUE); // pixel 1, alpha bit clear
        hw.arm9_write_16(0x0700_0000, 3 << 10); // sprite 0: bitmap
        hw.arm9_write_16(0x0700_0002, 0);
        hw.arm9_write_16(0x0700_0004, 2 | 15 << 12); // tile 2, alpha 15
        hw.arm9_write_16(0x0700_0008, 3 << 10); // sprite 1: the same, alpha 0
        hw.arm9_write_16(0x0700_000A, 0);
        hw.arm9_write_16(0x0700_000C, 2);

        let graphics = draw_request(&hw, request(GraphicsView::Sprites));
        assert_eq!(pixel(&graphics, 0, 0), bgr555(RED));
        assert_eq!(pixel(&graphics, 1, 0), CHECKER[0], "transparent");
        assert_eq!(pixel(&graphics, 64, 0), CELL_SHADES[1], "hidden");
        assert_eq!(
            graphics.notes[1],
            "  0: x 0, y 0, 8×8 square, tile 0x002, alpha 15, priority 0, bitmap"
        );
        assert!(graphics.notes[2].ends_with("alpha 0, priority 0, bitmap, hidden (alpha 0)"));
    }

    #[test]
    fn text_maps_draw_the_engines_layer_and_other_kinds_say_so() {
        let mut hw = hardware();
        hw.arm9_write_8(VRAMCNT + 2, 0x84); // bank C as engine B BG
        hw.arm9_write_16(0x0500_0400, BLUE); // engine B backdrop
        hw.arm9_write_16(0x0500_0402, RED); // engine B BG colour 1
        hw.arm9_write_16(0x0620_0020, 0x0001); // tile 1: top-left pixel
        hw.arm9_write_16(0x0620_F800, 1 | 1 << 10); // cell (0, 0): tile 1, h-flip
        hw.arm9_write_16(0x0400_1008, 31 << 8); // engine B BG0: map block 31
        hw.arm9_write_16(0x0400_1000, 1 << 8); // mode 0, BG0 on
        let map = |hw: &Hardware, engine, layer| {
            draw_request(
                hw,
                GraphicsRequest {
                    engine,
                    layer,
                    ..request(GraphicsView::Map)
                },
            )
        };

        let graphics = map(&hw, 1, 0);
        assert_eq!(graphics.image.size(), ScreenSize::new(256, 256));
        assert_eq!(pixel(&graphics, 7, 0), bgr555(RED), "flipped");
        assert_eq!(pixel(&graphics, 0, 0), bgr555(BLUE), "the backdrop");
        assert!(graphics.notes[0].starts_with(
            "Engine B BG0: text, 256×256, 4bpp, map 0x0620F800, tiles 0x06200000, priority 0, on"
        ));

        hw.arm9_write_16(0x0400_0000, 2); // engine A: mode 2
        let affine = map(&hw, 0, 2);
        assert_eq!(affine.image.size(), ScreenSize::new(8, 8));
        assert_eq!(
            affine.notes,
            ["Engine A BG2 is an affine background in mode 2; only text backgrounds are drawn yet"]
        );
    }
}
