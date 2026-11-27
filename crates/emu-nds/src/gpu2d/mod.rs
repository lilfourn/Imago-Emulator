//! The two 2D graphics engines (GBATEK, "DS Video", "DS Video BG Modes /
//! Control", "DS Video OBJs", "DS Video Extended Palettes", "DS Video
//! Display System Block Diagram", and the GBA's "LCD Video Controller"
//! chapters the DS builds on).
//!
//! Engines A and B are both an [`Engine2d`]. Engine B lacks 3D, display
//! modes 2–3, the large bitmap and DISPCNT's base offsets, and has 128 KiB
//! of BG VRAM (its VRAM view wraps there). The machine calls
//! [`Engine2d::render_line`] once per visible line, 0–191, using the
//! registers at that moment. Line 0 reloads BG2/BG3's internal reference
//! points, standing in for the frame start, and every line steps them
//! afterwards.
//!
//! A line is drawn in three steps: each background and the sprites into
//! their own line buffers ([`backgrounds`], [`objects`]); the window mask,
//! layer priority and special effects ([`compose`]); then the display mode
//! and master brightness, here. For upscaled 3D, [`scaled`] draws the
//! lines again around a larger 3D layer.
//!
//! Layer line buffers hold `u32` layer pixels: 0 is transparent; otherwise
//! [`OPAQUE`] is set and the low 15 bits are a BGR555 colour, or, with
//! [`IS_3D`] also set, the low 23 bits are a 3D pixel (18-bit colour, then
//! alpha in bits 18–22).

mod backgrounds;
mod cache;
mod color;
mod compose;
mod objects;
mod registers;
mod scaled;

use emu_state::payload;
use serde::{Deserialize, Serialize};

use crate::vram::{Engine, EngineMemory};
use compose::Layers;
use registers::Registers;

pub(crate) use scaled::{LineCopy, enlarge_line};

/// Pixels per line.
pub(crate) const WIDTH: usize = 256;

/// Layer pixel flag: something is drawn here.
const OPAQUE: u32 = 1 << 31;
/// Layer pixel flag: the low 23 bits are a 3D pixel.
const IS_3D: u32 = 1 << 30;

/// The layer pixel for a BGR555 colour (bit 15 ignored).
fn layer_color(bgr555: u16) -> u32 {
    OPAQUE | u32::from(bgr555 & 0x7FFF)
}

/// An 18-bit colour through MASTER_BRIGHT, `master_bright`, to
/// `0xFFRRGGBB`, as [`Engine2d::render_line`] outputs a line.
fn output_pixel(master_bright: u16, color: u32) -> u32 {
    let factor = u32::from(master_bright & 0x1F).min(16);
    color::to_argb(match master_bright >> 14 {
        1 => color::brighten(color, factor),
        2 => color::darken(color, factor),
        _ => color,
    })
}

/// The layer pixel for a 3D pixel: transparent where its alpha is 0.
fn layer_3d_pixel(pixel: u32) -> u32 {
    if (pixel >> 18) & 0x1F != 0 {
        OPAQUE | IS_3D | (pixel & 0x7F_FFFF)
    } else {
        0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Engine2d {
    engine: Engine,
    registers: Registers,
    /// 256 BG colours, then 256 OBJ colours, BGR555.
    #[serde(with = "payload::ExactList::<0x200>")]
    palette: Box<[u16]>,
    /// 128 sprites of four halfwords: attributes 0–2, then one affine
    /// parameter (every fourth sprite's group of four makes PA–PD).
    #[serde(with = "payload::ExactList::<0x200>")]
    oam: Box<[u16]>,
    /// The last graphics line before master brightness, for display
    /// capture. Rebuilt every line, so not saved.
    #[serde(skip, default = "blank_line")]
    capture: [u16; WIDTH],
    /// The last line's layers, for drawing it again around a larger 3D
    /// layer ([`Engine2d::copy_line`]). Rebuilt every line, so not saved.
    #[serde(skip)]
    layers: Option<Box<Layers>>,
    /// Whether the last line showed 3D: the graphics display mode, with
    /// the 3D layer drawn as BG0.
    #[serde(skip)]
    showed_3d: bool,
}

fn blank_line() -> [u16; WIDTH] {
    [0; WIDTH]
}

impl Engine2d {
    /// Whether this engine's registers, read from a save state, hold
    /// only the bits their writes keep.
    pub(crate) fn in_range(&self) -> bool {
        self.registers.in_range()
    }

    pub(crate) fn new(engine: Engine) -> Self {
        Self {
            engine,
            registers: Registers::new(engine),
            palette: vec![0; 0x200].into_boxed_slice(),
            oam: vec![0; 0x200].into_boxed_slice(),
            capture: blank_line(),
            layers: None,
            showed_3d: false,
        }
    }

    /// The register halfword at `offset` (0x00–0x6F, halfword-aligned)
    /// from this engine's I/O base. Write-only and unused registers read 0.
    pub(crate) fn read_register(&self, offset: u32) -> u16 {
        self.registers.read(offset)
    }

    /// Writes the register halfword at `offset` (0x00–0x6F,
    /// halfword-aligned). DISP3DCNT, DISPCAPCNT and DISP_MMEM_FIFO are
    /// ignored; they belong elsewhere.
    pub(crate) fn write_register(&mut self, offset: u32, value: u16) {
        self.registers.write(offset, value);
    }

    /// Writes the bytes `mask` covers (0x00FF, 0xFF00 or both) of the
    /// register halfword at `offset`; the other byte stays as the
    /// register holds it.
    pub(crate) fn write_register_bytes(&mut self, offset: u32, value: u16, mask: u16) {
        self.registers.write_bytes(offset, value, mask);
    }

    /// The palette halfword at `offset` within this engine's 1 KiB.
    pub(crate) fn read_palette(&self, offset: u32) -> u16 {
        self.palette[(offset as usize & 0x3FF) / 2]
    }

    pub(crate) fn write_palette(&mut self, offset: u32, value: u16) {
        self.palette[(offset as usize & 0x3FF) / 2] = value;
    }

    /// The OAM halfword at `offset` within this engine's 1 KiB.
    pub(crate) fn read_oam(&self, offset: u32) -> u16 {
        self.oam[(offset as usize & 0x3FF) / 2]
    }

    pub(crate) fn write_oam(&mut self, offset: u32, value: u16) {
        self.oam[(offset as usize & 0x3FF) / 2] = value;
    }

    /// The last line's graphics output (BGs, 3D and sprites after special
    /// effects, before display mode and master brightness), as BGR555 with
    /// bit 15 set: every pixel of the graphics screen is drawn, if only as
    /// backdrop. This is display capture's source A "graphics screen".
    pub(crate) fn capture_line(&self) -> &[u16; WIDTH] {
        &self.capture
    }

    /// Draws visible line `line` (0–191) into `out` as `0xFFRRGGBB`.
    ///
    /// `memory` is this engine's VRAM. `lcdc` reads the halfword at an
    /// offset in the LCDC area (0x06800000 on the ARM9), where VRAM display
    /// mode finds bank A–D; engine B never calls it. `bg0_3d` is the 3D
    /// engine's line for engine A, or `None` when there is none, which
    /// leaves a 3D BG0 transparent.
    pub(crate) fn render_line(
        &mut self,
        line: u16,
        memory: &impl EngineMemory,
        lcdc: impl Fn(u32) -> u16,
        bg0_3d: Option<&[u32; WIDTH]>,
        out: &mut [u32; WIDTH],
    ) {
        let line = u32::from(line);
        if line == 0 {
            self.registers.reload_affine();
        }
        // Drawn in every display mode: capture can record it while VRAM
        // display shows something else.
        let mut graphics = [0; WIDTH];
        let mut layers = self
            .layers
            .take()
            .unwrap_or_else(|| Box::new(Layers::new()));
        let drew_3d = self.graphics_line(line, memory, bg0_3d, &mut layers, &mut graphics);
        self.layers = Some(layers);
        for (captured, &color) in self.capture.iter_mut().zip(&graphics) {
            *captured = 0x8000 | color::narrow(color);
        }
        let dispcnt = self.registers.dispcnt;
        let display_mode = (dispcnt >> 16) & 3;
        self.showed_3d = display_mode == 1 && drew_3d;
        match display_mode {
            0 => out.fill(color::to_argb(color::WHITE)),
            1 => self.output(&graphics, out),
            2 => self.output(&vram_display_line(line, (dispcnt >> 18) & 3, lcdc), out),
            // Main memory display needs the DISP_MMEM_FIFO stream, which
            // display capture owns; until it is fed here, it shows black.
            _ => self.output(&[0; WIDTH], out),
        }
        self.registers.step_affine();
    }

    /// Goes past `line` without drawing it, for a frame nobody looks at:
    /// all that [`Engine2d::render_line`] leaves behind for the lines
    /// after it, the affine backgrounds' place.
    pub(crate) fn skip_line(&mut self, line: u16) {
        if line == 0 {
            self.registers.reload_affine();
        }
        self.registers.step_affine();
    }

    /// Composes `line`'s BG, 3D and OBJ layers, drawn into `layers`, into
    /// 18-bit colours. Returns whether the 3D layer was drawn as BG0.
    fn graphics_line(
        &self,
        line: u32,
        memory: &impl EngineMemory,
        bg0_3d: Option<&[u32; WIDTH]>,
        layers: &mut Layers,
        out: &mut [u32; WIDTH],
    ) -> bool {
        layers.clear();
        let dispcnt = self.registers.dispcnt;
        if dispcnt & (1 << 7) != 0 {
            // Forced blank.
            out.fill(color::WHITE);
            return false;
        }
        layers.drawn = self.draw_backgrounds(line, memory, bg0_3d, &mut layers.backgrounds);
        let mut object_window = [false; WIDTH];
        if dispcnt & (1 << 12) != 0 {
            self.draw_objects(line, memory, &mut layers.objects, &mut object_window);
        }
        self.window_line(line, &object_window, &mut layers.window);
        self.compose(layers, out);
        layers.drawn[0] && self.shows_3d_as_bg0()
    }

    /// Applies MASTER_BRIGHT to 18-bit colours and converts them to
    /// `0xFFRRGGBB`.
    fn output(&self, line: &[u32; WIDTH], out: &mut [u32; WIDTH]) {
        let master_bright = self.registers.master_bright;
        let factor = u32::from(master_bright & 0x1F).min(16);
        match master_bright >> 14 {
            1 => convert(line, out, |color| color::brighten(color, factor)),
            2 => convert(line, out, |color| color::darken(color, factor)),
            _ => convert(line, out, |color| color),
        }
    }

    /// The layer pixel for BG palette colour `index`.
    fn bg_color(&self, index: u32) -> u32 {
        layer_color(self.palette[index as usize])
    }

    /// The layer pixel for OBJ palette colour `index`.
    fn obj_color(&self, index: u32) -> u32 {
        layer_color(self.palette[0x100 + index as usize])
    }
}

fn convert(line: &[u32; WIDTH], out: &mut [u32; WIDTH], adjust: impl Fn(u32) -> u32) {
    for (pixel, &color) in out.iter_mut().zip(line) {
        *pixel = color::to_argb(adjust(color));
    }
}

/// VRAM display mode: line `line` of bank `block` (A–D, 128 KiB each in the
/// LCDC area) as a 256×192 BGR555 bitmap, bit 15 unused.
fn vram_display_line(line: u32, block: u32, lcdc: impl Fn(u32) -> u16) -> [u32; WIDTH] {
    let start = block * 0x2_0000 + line * WIDTH as u32 * 2;
    let mut pixels = [0; WIDTH];
    for (offset, pixel) in (start..).step_by(2).zip(pixels.iter_mut()) {
        *pixel = color::widen(lcdc(offset));
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) use crate::vram::tests::Random;
    use crate::vram::{Region, Vram};

    /// A layer pixel: transparent half the time, else a BGR555 or (when
    /// `three_d`) a 3D pixel with a random alpha.
    pub(super) fn layer_pixel(random: &mut Random, three_d: bool) -> u32 {
        let value = random.next();
        match value & 3 {
            0 | 1 => 0,
            2 if three_d => OPAQUE | IS_3D | (value >> 2) & 0x7F_FFFF,
            _ => OPAQUE | (value >> 2) & 0x7FFF,
        }
    }

    const RED: u16 = 0x001F;
    const GREEN: u16 = 0x03E0;
    const BLUE: u16 = 0x7C00;
    const WHITE: u16 = 0x7FFF;
    /// DISPCNT display mode 1: graphics.
    const GRAPHICS: u32 = 1 << 16;

    /// `0xFFRRGGBB` for one of the tests' BGR555 colours, or black, shown
    /// without effects.
    fn argb(bgr555: u16) -> u32 {
        match bgr555 {
            0 => 0xFF00_0000,
            RED => 0xFFFF_0000,
            GREEN => 0xFF00_FF00,
            BLUE => 0xFF00_00FF,
            WHITE => 0xFFFF_FFFF,
            _ => panic!("no pixel listed for {bgr555:#06X}"),
        }
    }

    /// VRAM with bank A as engine A's BG memory, B as its OBJ memory, C in
    /// the LCDC area, E as its BG extended palettes and F as its OBJ
    /// extended palette.
    fn vram() -> Vram {
        let mut vram = Vram::default();
        vram.set_control(0, 0x81);
        vram.set_control(1, 0x82);
        vram.set_control(2, 0x80);
        vram.set_control(4, 0x84);
        vram.set_control(5, 0x85);
        vram
    }

    fn render_with_3d(
        engine: &mut Engine2d,
        vram: &Vram,
        line: u16,
        bg0_3d: Option<&[u32; WIDTH]>,
    ) -> [u32; WIDTH] {
        let mut out = [0; WIDTH];
        let memory = vram.engine(engine.engine);
        let lcdc = |offset| vram.read_16(Region::Lcdc, offset);
        engine.render_line(line, &memory, lcdc, bg0_3d, &mut out);
        out
    }

    fn render(engine: &mut Engine2d, vram: &Vram, line: u16) -> [u32; WIDTH] {
        render_with_3d(engine, vram, line, None)
    }

    fn set_dispcnt(engine: &mut Engine2d, dispcnt: u32) {
        engine.write_register(0x00, dispcnt as u16);
        engine.write_register(0x02, (dispcnt >> 16) as u16);
    }

    /// Engine A in mode 0 with BG0 showing tile 1 (every pixel colour 1 =
    /// red) in map cell (0, 0) of screen block 31, on a blue backdrop.
    fn scene() -> (Engine2d, Vram) {
        let mut engine = Engine2d::new(Engine::A);
        let mut vram = vram();
        engine.write_palette(0, BLUE);
        engine.write_palette(2, RED);
        for offset in (32..64).step_by(2) {
            vram.write_16(Region::BgA, offset, 0x1111);
        }
        vram.write_16(Region::BgA, 0xF800, 1);
        engine.write_register(0x08, 31 << 8);
        set_dispcnt(&mut engine, GRAPHICS | 1 << 8);
        (engine, vram)
    }

    /// Sets the first sprites' attributes 0–2 to `sprites` and hides the
    /// rest (all-zero OAM entries are visible 8x8 sprites at (0, 0)).
    fn place_sprites(engine: &mut Engine2d, sprites: &[[u16; 3]]) {
        for entry in 0..128 {
            engine.write_oam(entry * 8, 1 << 9);
        }
        for (offset, value) in (0..)
            .step_by(8)
            .zip(sprites)
            .flat_map(|(base, attributes)| (base..).step_by(2).zip(*attributes))
        {
            engine.write_oam(offset, value);
        }
    }

    /// Fills OBJ VRAM bytes `start..start + len` with `byte`.
    fn fill_obj(vram: &mut Vram, start: u32, len: u32, byte: u8) {
        for offset in (start..start + len).step_by(2) {
            vram.write_16(Region::ObjA, offset, u16::from_le_bytes([byte, byte]));
        }
    }

    #[test]
    fn display_off_shows_white() {
        let (mut engine, vram) = scene();
        set_dispcnt(&mut engine, 1 << 8);
        assert!(
            render(&mut engine, &vram, 0)
                .iter()
                .all(|&pixel| pixel == 0xFFFF_FFFF)
        );
    }

    #[test]
    fn text_background_draws_its_tile_over_the_backdrop() {
        let (mut engine, vram) = scene();
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[0], argb(RED));
        assert_eq!(row[7], argb(RED));
        assert_eq!(row[8], argb(BLUE));
        assert_eq!(render(&mut engine, &vram, 8)[0], argb(BLUE));
    }

    #[test]
    fn text_background_scrolls() {
        let (mut engine, vram) = scene();
        engine.write_register(0x10, 4); // BG0HOFS
        engine.write_register(0x12, 4); // BG0VOFS
        let row = render(&mut engine, &vram, 3);
        assert_eq!(row[3], argb(RED));
        assert_eq!(row[4], argb(BLUE));
        assert_eq!(render(&mut engine, &vram, 4)[0], argb(BLUE));
    }

    #[test]
    fn text_background_mosaic_repeats_each_block_start() {
        let (mut engine, vram) = scene();
        engine.write_register(0x08, 31 << 8 | 1 << 6);
        engine.write_register(0x4C, 0x33); // 4x4 blocks
        engine.write_register(0x10, 2); // BG0HOFS: block 1 samples map x 6
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[7], argb(RED), "unmosaicked, map x 9 is blue");
        assert_eq!(row[8], argb(BLUE));
        engine.write_register(0x12, 6); // BG0VOFS: line 3 repeats line 0
        assert_eq!(render(&mut engine, &vram, 3)[0], argb(RED));
        assert_eq!(render(&mut engine, &vram, 4)[0], argb(BLUE));
    }

    #[test]
    fn text_background_256_colour_tiles_use_the_selected_extended_palette() {
        let (mut engine, mut vram) = scene();
        // Tile 1 in 256 colours: 64 bytes of colour 5.
        for offset in (64..128).step_by(2) {
            vram.write_16(Region::BgA, offset, 0x0505);
        }
        vram.write_16(Region::BgA, 0xF800, 1 | 2 << 12); // palette 2
        engine.write_register(0x08, 31 << 8 | 1 << 7 | 1 << 13); // slot 2
        engine.write_palette(5 * 2, RED);
        vram.write_16(Region::BgExtPaletteA, 2 * 0x2000 + (2 * 256 + 5) * 2, GREEN);
        assert_eq!(
            render(&mut engine, &vram, 0)[0],
            argb(RED),
            "standard palette"
        );
        set_dispcnt(&mut engine, GRAPHICS | 1 << 8 | 1 << 30);
        assert_eq!(render(&mut engine, &vram, 0)[0], argb(GREEN), "extended");
    }

    #[test]
    fn direct_colour_bitmap_shows_only_pixels_with_bit_15() {
        let mut engine = Engine2d::new(Engine::A);
        let mut vram = vram();
        engine.write_palette(0, BLUE);
        // BG3 in mode 3: a 256x256 direct-colour bitmap, unscaled.
        engine.write_register(0x0E, 1 << 7 | 1 << 2 | 1 << 14);
        engine.write_register(0x30, 0x100); // PA
        engine.write_register(0x36, 0x100); // PD
        set_dispcnt(&mut engine, GRAPHICS | 3 | 1 << 11);
        vram.write_16(Region::BgA, 0, 0x8000 | RED);
        vram.write_16(Region::BgA, 2, GREEN);
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[0], argb(RED));
        assert_eq!(row[1], argb(BLUE));
    }

    #[test]
    fn affine_bitmap_scales_and_steps_its_reference_point_per_line() {
        let mut engine = Engine2d::new(Engine::A);
        let mut vram = vram();
        engine.write_palette(2, RED);
        engine.write_palette(4, GREEN);
        // BG2 in mode 5: a 128x128 256-colour bitmap.
        engine.write_register(0x0C, 1 << 7);
        engine.write_register(0x20, 0x80); // PA: half a texel per pixel
        engine.write_register(0x26, 0x200); // PD: two rows per line
        set_dispcnt(&mut engine, GRAPHICS | 5 | 1 << 10);
        vram.write_16(Region::BgA, 0, 0x0201); // row 0: red, green
        vram.write_16(Region::BgA, 2 * 128, 0x0002); // row 2: green
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[..3], [argb(RED), argb(RED), argb(GREEN)]);
        assert_eq!(render(&mut engine, &vram, 1)[0], argb(GREEN));
        // Line 0 reloads the reference point.
        assert_eq!(render(&mut engine, &vram, 0)[0], argb(RED));
        // A line gone past without drawing moves it as a drawn one does.
        engine.skip_line(0);
        assert_eq!(render(&mut engine, &vram, 1)[0], argb(GREEN));
        engine.skip_line(0);
        engine.skip_line(1);
        assert_eq!(render(&mut engine, &vram, 2)[0], argb(0));
    }

    #[test]
    fn extended_tiles_use_16_bit_map_entries() {
        let mut engine = Engine2d::new(Engine::A);
        let mut vram = vram();
        engine.write_palette(3 * 2, RED);
        // BG3 in mode 3: 128x128 tiles, map at 0x800, characters at 0x4000.
        engine.write_register(0x0E, 1 << 2 | 1 << 8);
        engine.write_register(0x30, 0x100);
        engine.write_register(0x36, 0x100);
        set_dispcnt(&mut engine, GRAPHICS | 3 | 1 << 11);
        vram.write_16(Region::BgA, 0x4000 + 2 * 64, 0x0003); // tile 2, pixel 0
        vram.write_16(Region::BgA, 0x800, 2 | 1 << 10); // tile 2, h-flipped
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[7], argb(RED));
        assert_eq!(row[0], argb(0));
    }

    #[test]
    fn large_bitmap_reaches_past_128k() {
        let mut engine = Engine2d::new(Engine::A);
        let mut vram = vram();
        vram.set_control(1, 0x81 | 1 << 3); // bank B: engine A BG at 128K
        engine.write_palette(2, RED);
        engine.write_register(0x0C, 0); // 512x1024
        engine.write_register(0x20, 0x100);
        engine.write_register(0x2E, 0x0001); // BG2Y = 256.0
        set_dispcnt(&mut engine, GRAPHICS | 6 | 1 << 10);
        vram.write_16(Region::BgA, 256 * 512, 0x0001);
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[0], argb(RED));
        assert_eq!(row[1], argb(0));
    }

    #[test]
    fn sprites_win_priority_ties_and_lower_oam_indices_win_among_sprites() {
        let (mut engine, mut vram) = scene();
        engine.write_palette(0x202, GREEN);
        engine.write_palette(0x204, WHITE);
        fill_obj(&mut vram, 0, 32, 0x11); // tile 0: colour 1
        fill_obj(&mut vram, 32, 32, 0x22); // tile 1: colour 2
        // Sprite 0: green at x 4; sprite 1: white at x 0.
        place_sprites(&mut engine, &[[0, 4, 0], [0, 0, 1]]);
        set_dispcnt(&mut engine, GRAPHICS | 1 << 8 | 1 << 12 | 1 << 4);
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[0], argb(WHITE), "sprite before an equal-priority BG");
        assert_eq!(row[4], argb(GREEN), "lower OAM index in front");
        assert_eq!(row[11], argb(GREEN));
        assert_eq!(row[12], argb(BLUE));

        // Priority beats OAM index, and BGs beat lower-priority sprites.
        engine.write_oam(4, 1 << 10); // sprite 0: priority 1
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[4], argb(WHITE));
        assert_eq!(row[8], argb(GREEN), "over the backdrop");
        engine.write_oam(12, 1 | 1 << 10); // sprite 1: priority 1 too
        assert_eq!(render(&mut engine, &vram, 0)[0], argb(RED));
    }

    #[test]
    fn one_dimensional_tile_boundary_scales_tile_numbers() {
        let (mut engine, mut vram) = scene();
        engine.write_palette(0x202, GREEN);
        engine.write_palette(0x204, WHITE);
        fill_obj(&mut vram, 32, 32, 0x22); // tile 1 at a 32-byte boundary
        fill_obj(&mut vram, 64, 32, 0x11); // tile 1 at a 64-byte boundary
        place_sprites(&mut engine, &[[0, 0, 1]]);
        set_dispcnt(&mut engine, GRAPHICS | 1 << 12 | 1 << 4);
        assert_eq!(render(&mut engine, &vram, 0)[0], argb(WHITE));
        set_dispcnt(&mut engine, GRAPHICS | 1 << 12 | 1 << 4 | 1 << 20);
        assert_eq!(render(&mut engine, &vram, 0)[0], argb(GREEN));
    }

    #[test]
    fn bitmap_sprites_draw_direct_colour_with_1d_mapping() {
        let (mut engine, mut vram) = scene();
        // Tile 2 at a 128-byte boundary: byte 256.
        vram.write_16(Region::ObjA, 256, 0x8000 | GREEN);
        vram.write_16(Region::ObjA, 258, WHITE); // no alpha bit
        place_sprites(&mut engine, &[[3 << 10, 0, 2 | 15 << 12]]);
        set_dispcnt(&mut engine, GRAPHICS | 1 << 8 | 1 << 12 | 1 << 6);
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[0], argb(GREEN));
        assert_eq!(row[1], argb(RED));
        engine.write_oam(4, 2); // Alpha-OAM 0 hides it
        assert_eq!(render(&mut engine, &vram, 0)[0], argb(RED));
    }

    #[test]
    fn bitmap_sprites_blend_with_their_alpha() {
        let (mut engine, mut vram) = scene();
        vram.write_16(Region::ObjA, 0, 0x8000 | BLUE);
        place_sprites(&mut engine, &[[3 << 10, 0, 7 << 12]]); // alpha 7: 8/16
        engine.write_register(0x50, 1 << 8); // BG0 2nd target
        set_dispcnt(&mut engine, GRAPHICS | 1 << 8 | 1 << 12 | 1 << 6);
        // Each channel 63 * 8 / 16 = 31 of 63.
        assert_eq!(render(&mut engine, &vram, 0)[0], 0xFF7D_007D);
    }

    #[test]
    fn window_hides_a_background_outside_it() {
        let (mut engine, vram) = scene();
        engine.write_register(0x40, 2); // WIN0H: x 0..2
        engine.write_register(0x44, 192); // WIN0V: lines 0..192
        engine.write_register(0x48, 0x01); // inside: BG0
        engine.write_register(0x4A, 0x00); // outside: nothing
        set_dispcnt(&mut engine, GRAPHICS | 1 << 8 | 1 << 13);
        let row = render(&mut engine, &vram, 0);
        assert_eq!(row[1], argb(RED));
        assert_eq!(row[2], argb(BLUE));
    }

    #[test]
    fn alpha_blending_mixes_first_and_second_targets() {
        let (mut engine, vram) = scene();
        engine.write_register(0x50, 1 | 1 << 6 | 1 << 13); // BG0 over backdrop
        engine.write_register(0x52, 8 | 8 << 8);
        let row = render(&mut engine, &vram, 0);
        // Each channel 63 * 8 / 16 = 31 of 63.
        assert_eq!(row[0], 0xFF7D_007D);
        assert_eq!(row[8], argb(BLUE), "the backdrop is not a 1st target");
        engine.write_register(0x48, 0x01); // WIN0 inside: BG0, no effects
        engine.write_register(0x40, 0x0001);
        engine.write_register(0x44, 192);
        set_dispcnt(&mut engine, GRAPHICS | 1 << 8 | 1 << 13);
        assert_eq!(render(&mut engine, &vram, 0)[0], argb(RED));
    }

    #[test]
    fn master_brightness_fades_after_capture() {
        let (mut engine, vram) = scene();
        engine.write_register(0x6C, 1 << 14 | 16);
        assert_eq!(render(&mut engine, &vram, 0)[0], 0xFFFF_FFFF);
        assert_eq!(engine.capture_line()[0], 0x8000 | RED);
        assert_eq!(engine.capture_line()[8], 0x8000 | BLUE);
        engine.write_register(0x6C, 2 << 14 | 31); // factors above 16 act as 16
        assert_eq!(render(&mut engine, &vram, 0)[0], 0xFF00_0000);
        engine.write_register(0x6C, 2 << 14 | 8);
        // 63 - 63 * 8 / 16 = 32 of 63.
        assert_eq!(render(&mut engine, &vram, 0)[0], 0xFF82_0000);
    }

    #[test]
    fn three_d_pixels_blend_with_the_layer_below_using_their_alpha() {
        let mut engine = Engine2d::new(Engine::A);
        let vram = vram();
        engine.write_palette(0, BLUE);
        engine.write_register(0x50, 1 << 13); // backdrop is a 2nd target
        set_dispcnt(&mut engine, GRAPHICS | 1 << 3 | 1 << 8);
        let mut line_3d = [0; WIDTH];
        line_3d[0] = 0x3F | 15 << 18; // red, half alpha
        line_3d[1] = 0x3F | 31 << 18; // red, opaque
        line_3d[2] = 0x3F; // alpha 0: nothing drawn
        let row = render_with_3d(&mut engine, &vram, 0, Some(&line_3d));
        // 63 * 16 / 32 = 31 of 63 of each.
        assert_eq!(row[0], 0xFF7D_007D);
        assert_eq!(row[1], argb(RED));
        assert_eq!(row[2], argb(BLUE));
        // Without a 2nd target the 3D pixel shows as is.
        engine.write_register(0x50, 0);
        let row = render_with_3d(&mut engine, &vram, 0, Some(&line_3d));
        assert_eq!(row[0], argb(RED));
    }

    /// `engine`'s last line, drawn as `native`, drawn again `scale` times
    /// larger around the larger 3D layer's `rows`.
    fn draw_larger(
        engine: &Engine2d,
        native: &[u32; WIDTH],
        scale: usize,
        rows: &[Vec<u32>],
    ) -> Vec<u32> {
        let mut copy = LineCopy::default();
        engine.copy_line(native, &mut copy);
        let mut out = vec![0; WIDTH * scale * scale];
        copy.draw(scale, |row| rows.get(row).map(Vec::as_slice), &mut out);
        out
    }

    #[test]
    fn a_line_drawn_larger_composes_the_larger_3d_layer() {
        let mut engine = Engine2d::new(Engine::A);
        let vram = vram();
        engine.write_palette(0, BLUE);
        engine.write_register(0x50, 1 << 13); // backdrop is a 2nd target
        engine.write_register(0x6C, 1 << 14 | 4); // brighten by 4/16
        set_dispcnt(&mut engine, GRAPHICS | 1 << 3 | 1 << 8);
        let red = 0x3F | 31 << 18;
        let mut line_3d = [0; WIDTH];
        line_3d[0] = red;
        let native = render_with_3d(&mut engine, &vram, 0, Some(&line_3d));
        // Pixel 0's square at 2x: red, half-alpha red; nothing, red.
        let mut rows = vec![vec![0; 512]; 2];
        rows[0][0] = red;
        rows[0][1] = 0x3F | 15 << 18;
        rows[1][1] = red;
        let out = draw_larger(&engine, &native, 2, &rows);
        assert_eq!(out[0], native[0]);
        // Red and blue 31 of 63 each, brightened by (63 - 31) * 4 / 16 to 39
        // and green by 63 * 4 / 16 to 15.
        assert_eq!(out[1], 0xFF9E_3C9E);
        // Blue alone: red and green 15, blue 63.
        assert_eq!(out[512], 0xFF3C_3CFF);
        assert_eq!(out[513], native[0]);
        assert_eq!(&out[2..4], &[native[1]; 2], "no 3D there, as at 1x");
    }

    #[test]
    fn a_copied_line_says_whether_it_changed() {
        let mut engine = Engine2d::new(Engine::A);
        let vram = vram();
        engine.write_palette(0, BLUE);
        set_dispcnt(&mut engine, GRAPHICS | 1 << 3 | 1 << 8);
        let mut line_3d = [0x3F | 31 << 18; WIDTH];
        let mut copy = LineCopy::default();
        let mut copied = |engine: &mut Engine2d, line_3d: &[u32; WIDTH]| {
            let native = render_with_3d(engine, &vram, 0, Some(line_3d));
            engine.copy_line(&native, &mut copy)
        };
        assert!(copied(&mut engine, &line_3d), "the first copy");
        assert!(!copied(&mut engine, &line_3d), "the same line again");
        // The 3D layer's own pixels are one of the line's layers.
        line_3d[9] = 0;
        assert!(copied(&mut engine, &line_3d), "another pixel");
        assert!(!copied(&mut engine, &line_3d));
        // Registers the larger line is composed with, which leave this
        // line's own pixels alone: BG0HOFS over a layer of one colour, and
        // an effect with no layer to work on.
        line_3d[9] = line_3d[0];
        assert!(copied(&mut engine, &line_3d));
        engine.write_register(0x10, 1);
        assert!(copied(&mut engine, &line_3d), "scrolled");
        assert!(!copied(&mut engine, &line_3d));
        engine.write_register(0x54, 3);
        assert!(copied(&mut engine, &line_3d), "BLDY");
        assert!(!copied(&mut engine, &line_3d));
        // Without 3D only the line's pixels count.
        set_dispcnt(&mut engine, GRAPHICS);
        assert!(copied(&mut engine, &line_3d), "no 3D layer");
        engine.write_register(0x54, 5);
        assert!(!copied(&mut engine, &line_3d), "nothing it is drawn from");
    }

    #[test]
    fn a_line_drawn_larger_keeps_windows_and_scrolling() {
        let mut engine = Engine2d::new(Engine::A);
        let vram = vram();
        engine.write_palette(0, BLUE);
        // Window 0 over x 0..8 shows everything but BG0.
        set_dispcnt(&mut engine, GRAPHICS | 1 << 3 | 1 << 8 | 1 << 13);
        engine.write_register(0x40, 0x0008);
        engine.write_register(0x44, 0x00C0);
        engine.write_register(0x48, 0x003E);
        engine.write_register(0x4A, 0x003F);
        engine.write_register(0x10, 1); // BG0HOFS: one pixel left
        let native = render_with_3d(&mut engine, &vram, 0, Some(&[0x3F | 31 << 18; WIDTH]));
        let mut green = vec![0x3F << 6 | 31 << 18; 512];
        green[2] = 0x3F << 12 | 31 << 18;
        let out = draw_larger(&engine, &native, 2, &[green.clone(), green]);
        assert_eq!(out[15], argb(BLUE), "inside the window");
        assert_eq!(out[16], argb(GREEN), "outside, the larger 3D");
        assert_eq!(out[16 + 512], argb(GREEN));
        // Scrolled by two cells: the end of the row is past the 3D frame.
        assert_eq!(out[509], argb(GREEN));
        assert_eq!(out[510], argb(BLUE));
    }

    #[test]
    fn lines_without_3d_come_out_enlarged() {
        let (mut engine, vram) = scene();
        let native = render(&mut engine, &vram, 0);
        let rows = vec![vec![0x3F | 31 << 18; 512]; 2];
        let enlarged: Vec<u32> = native.iter().flat_map(|&pixel| [pixel; 2]).collect();
        assert_eq!(
            draw_larger(&engine, &native, 2, &rows),
            [&enlarged[..], &enlarged[..]].concat()
        );
        // 3D, but the frame is forced blank.
        set_dispcnt(&mut engine, GRAPHICS | 1 << 3 | 1 << 8 | 1 << 7);
        let native = render_with_3d(&mut engine, &vram, 0, Some(&[0x3F | 31 << 18; WIDTH]));
        assert!(
            draw_larger(&engine, &native, 2, &rows)
                .iter()
                .all(|&pixel| pixel == native[0])
        );
    }

    #[test]
    fn vram_display_shows_the_selected_lcdc_bank() {
        let (mut engine, mut vram) = scene();
        set_dispcnt(&mut engine, 2 << 16 | 2 << 18); // bank C
        vram.write_16(Region::Lcdc, 0x4_0000 + (256 + 3) * 2, GREEN);
        let row = render(&mut engine, &vram, 1);
        assert_eq!(row[3], argb(GREEN));
        assert_eq!(row[4], argb(0));
    }

    #[test]
    fn main_memory_display_is_black_until_fed() {
        let (mut engine, vram) = scene();
        set_dispcnt(&mut engine, 3 << 16);
        assert!(
            render(&mut engine, &vram, 0)
                .iter()
                .all(|&pixel| pixel == 0xFF00_0000)
        );
    }

    #[test]
    fn engine_b_draws_bg0_as_text_and_ignores_3d() {
        let mut engine = Engine2d::new(Engine::B);
        let mut vram = Vram::default();
        vram.set_control(2, 0x84); // bank C: engine B BG
        engine.write_palette(0, BLUE);
        engine.write_palette(2, RED);
        for offset in (32..64).step_by(2) {
            vram.write_16(Region::BgB, offset, 0x1111);
        }
        vram.write_16(Region::BgB, 0xF800, 1);
        engine.write_register(0x08, 31 << 8);
        set_dispcnt(&mut engine, GRAPHICS | 1 << 3 | 1 << 8);
        let line_3d = [0x3F | 31 << 18; WIDTH];
        let row = render_with_3d(&mut engine, &vram, 0, Some(&line_3d));
        assert_eq!(row[0], argb(RED));
        assert_eq!(row[8], argb(BLUE));
    }

    #[test]
    fn palette_and_oam_are_halfword_addressed_within_1k() {
        let mut engine = Engine2d::new(Engine::A);
        engine.write_palette(0x402, 0x1234);
        assert_eq!(engine.read_palette(2), 0x1234);
        engine.write_oam(0x3FE, 0xBEEF);
        assert_eq!(engine.read_oam(0x7FE), 0xBEEF);
    }
}
