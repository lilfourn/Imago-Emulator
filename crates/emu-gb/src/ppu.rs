//! The picture processing unit (Pan Docs, "Rendering", "LCD Control",
//! "LCD Status Registers", "Scrolling", "OAM", "Palettes", "VRAM Banks").
//!
//! Timing follows the hardware's modes dot by dot (456 dots per line, 154
//! lines per frame). Pixels are drawn a whole line at a time when mode 3
//! starts, using the registers as they are at that moment. That is enough
//! for nearly every game; mid-line register effects would need a pixel FIFO.
//!
//! In CGB mode the PPU has a second VRAM bank holding per-tile attributes
//! (palette, bank, flips, priority), 8 background and 8 object palettes of
//! four 15-bit colours, and draws objects in OAM order.

use emu_core::rgb;
use serde::{Deserialize, Serialize};

use crate::interrupts;
use crate::model::Model;

pub(crate) const WIDTH: usize = 160;
pub(crate) const HEIGHT: usize = 144;
const DOTS_PER_LINE: u16 = 456;
const LINES_PER_FRAME: u8 = 154;
const OAM_SCAN_DOTS: u16 = 80;

/// The four DMG shades, lightest first. Neutral greys match the reference
/// screenshots of the common test-ROM suites.
pub(crate) const DMG_SHADES: [u32; 4] = [
    rgb(0xFF, 0xFF, 0xFF),
    rgb(0xAA, 0xAA, 0xAA),
    rgb(0x55, 0x55, 0x55),
    rgb(0x00, 0x00, 0x00),
];
const WHITE: u32 = rgb(0xFF, 0xFF, 0xFF);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Mode {
    HBlank = 0,
    VBlank = 1,
    OamScan = 2,
    Drawing = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Ppu {
    model: Model,
    /// Two 8 KiB banks; bank 1 exists only on CGB.
    vram: Box<[u8]>,
    vram_bank: u8,
    oam: Box<[u8]>,
    lcdc: u8,
    /// STAT bits 3–6: which conditions raise the STAT interrupt.
    stat_select: u8,
    scy: u8,
    scx: u8,
    ly: u8,
    lyc: u8,
    bgp: u8,
    obp0: u8,
    obp1: u8,
    wy: u8,
    wx: u8,
    /// CGB palette RAM: 8 palettes x 4 colours x 2 bytes, little-endian
    /// RGB555.
    bg_palettes: Box<[u8]>,
    obj_palettes: Box<[u8]>,
    /// BCPS and OCPS: palette RAM index plus auto-increment (bit 7).
    bg_palette_select: u8,
    obj_palette_select: u8,
    /// OPRI bit 0: objects prioritised by X coordinate (DMG style) rather
    /// than OAM order.
    priority_by_x: bool,
    mode: Mode,
    /// Dots elapsed in the current line.
    dot: u16,
    /// Length of mode 3 on this line.
    drawing_dots: u16,
    /// LY matched WY at some point this frame, so the window may show.
    window_in_frame: bool,
    /// The window's own line counter; advances only on lines it is drawn.
    window_line: u8,
    /// The OR of all enabled STAT conditions; the interrupt fires on its
    /// rising edge.
    stat_line: bool,
    /// The frame being drawn, as `0xFFRRGGBB`.
    pixels: Box<[u32]>,
    frame_ready: bool,
    /// Mode 0 began on a visible line since the last call; drives HBlank
    /// DMA.
    hblank_started: bool,
}

impl Ppu {
    /// The state the boot ROM leaves: LCD on, BGP = 0xFC.
    pub(crate) fn after_boot(model: Model) -> Self {
        Self {
            model,
            vram: vec![0; 0x4000].into_boxed_slice(),
            vram_bank: 0,
            oam: vec![0; 0xA0].into_boxed_slice(),
            lcdc: 0x91,
            stat_select: 0,
            scy: 0,
            scx: 0,
            ly: 0,
            lyc: 0,
            bgp: 0xFC,
            obp0: 0xFF,
            obp1: 0xFF,
            wy: 0,
            wx: 0,
            bg_palettes: vec![0xFF; 64].into_boxed_slice(),
            obj_palettes: vec![0xFF; 64].into_boxed_slice(),
            bg_palette_select: 0,
            obj_palette_select: 0,
            priority_by_x: model == Model::Dmg,
            mode: Mode::OamScan,
            dot: 0,
            drawing_dots: 172,
            window_in_frame: false,
            window_line: 0,
            stat_line: false,
            pixels: vec![WHITE; WIDTH * HEIGHT].into_boxed_slice(),
            frame_ready: false,
            hblank_started: false,
        }
    }

    fn cgb(&self) -> bool {
        self.model == Model::Cgb
    }

    fn lcd_enabled(&self) -> bool {
        self.lcdc & 0x80 != 0
    }

    /// Pixels of the last completed frame, row-major, as `0xFFRRGGBB`.
    pub(crate) fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Whether a frame finished since the last call.
    pub(crate) fn take_frame_ready(&mut self) -> bool {
        std::mem::take(&mut self.frame_ready)
    }

    /// Whether HBlank began on a visible line since the last call.
    pub(crate) fn take_hblank_started(&mut self) -> bool {
        std::mem::take(&mut self.hblank_started)
    }

    /// Whether the LCD is on; with it off no frames complete.
    pub(crate) fn is_on(&self) -> bool {
        self.lcd_enabled()
    }

    pub(crate) fn vram(&self) -> &[u8] {
        &self.vram
    }

    /// The hardware model, for the debugger's graphics viewers.
    pub(crate) fn model(&self) -> Model {
        self.model
    }

    /// CGB background palette RAM: 8 palettes of 4 little-endian BGR555
    /// colours, as BCPD reads it.
    pub(crate) fn bg_palette_ram(&self) -> &[u8] {
        &self.bg_palettes
    }

    /// CGB object palette RAM, laid out as [`Ppu::bg_palette_ram`].
    pub(crate) fn obj_palette_ram(&self) -> &[u8] {
        &self.obj_palettes
    }

    /// Advances `dots` dots: four per M-cycle, or two in CGB double speed.
    pub(crate) fn tick(&mut self, dots: u8, interrupt_flag: &mut u8) {
        if !self.lcd_enabled() {
            return;
        }
        for _ in 0..dots {
            self.advance_dot(interrupt_flag);
        }
        self.update_stat_line(interrupt_flag);
    }

    fn advance_dot(&mut self, interrupt_flag: &mut u8) {
        self.dot += 1;
        if self.ly < HEIGHT as u8 {
            if self.dot == OAM_SCAN_DOTS {
                self.mode = Mode::Drawing;
                self.drawing_dots = self.drawing_length();
                self.render_line();
            } else if self.dot == OAM_SCAN_DOTS + self.drawing_dots {
                self.mode = Mode::HBlank;
                self.hblank_started = true;
            }
        }
        if self.dot < DOTS_PER_LINE {
            return;
        }
        self.dot = 0;
        self.ly += 1;
        if self.ly == HEIGHT as u8 {
            self.mode = Mode::VBlank;
            *interrupt_flag |= interrupts::VBLANK;
            self.frame_ready = true;
        } else if self.ly == LINES_PER_FRAME {
            self.ly = 0;
            self.window_line = 0;
            self.window_in_frame = false;
            self.mode = Mode::OamScan;
        } else if self.ly < HEIGHT as u8 {
            self.mode = Mode::OamScan;
        }
        if self.ly == self.wy {
            self.window_in_frame = true;
        }
    }

    fn update_stat_line(&mut self, interrupt_flag: &mut u8) {
        let mode_selected = match self.mode {
            Mode::HBlank => self.stat_select & 0x08 != 0,
            // On DMG the mode 2 condition also fires as VBlank begins.
            Mode::VBlank if !self.cgb() && self.ly == HEIGHT as u8 && self.dot == 0 => {
                self.stat_select & 0x30 != 0
            }
            Mode::VBlank => self.stat_select & 0x10 != 0,
            Mode::OamScan => self.stat_select & 0x20 != 0,
            Mode::Drawing => false,
        };
        let line = mode_selected || (self.stat_select & 0x40 != 0 && self.ly == self.lyc);
        if line && !self.stat_line {
            *interrupt_flag |= interrupts::STAT;
        }
        self.stat_line = line;
    }

    /// Mode 3 length: 172 dots, plus fine scroll, plus the window and each
    /// object on the line (Pan Docs, "Mode 3 length").
    fn drawing_length(&self) -> u16 {
        let mut dots = 172 + u16::from(self.scx & 7);
        if self.window_visible_on_line() {
            dots += 6;
        }
        if self.lcdc & 0x02 != 0 {
            for sprite in self.line_sprites() {
                let offset = (u16::from(sprite.x) + u16::from(self.scx)) % 8;
                dots += 6 + 5u16.saturating_sub(offset);
            }
        }
        dots
    }

    fn window_visible_on_line(&self) -> bool {
        // On DMG, LCDC bit 0 off also hides the window; on CGB it only
        // removes background priority.
        let enabled = self.lcdc & 0x20 != 0 && (self.cgb() || self.lcdc & 0x01 != 0);
        enabled && self.window_in_frame && self.wx <= 166
    }

    /// Up to ten objects overlapping the current line, in OAM order.
    fn line_sprites(&self) -> Vec<Sprite> {
        let height = if self.lcdc & 0x04 != 0 { 16 } else { 8 };
        let line = i16::from(self.ly) + 16;
        self.oam
            .chunks_exact(4)
            .enumerate()
            .map(|(index, entry)| Sprite {
                y: entry[0],
                x: entry[1],
                tile: entry[2],
                attributes: entry[3],
                index,
            })
            .filter(|sprite| {
                let top = i16::from(sprite.y);
                line >= top && line < top + height
            })
            .take(10)
            .collect()
    }

    /// Colour index 0–3 of pixel (`column`, `row`) in the tile at VRAM
    /// offset `tile_address`.
    fn tile_pixel(&self, tile_address: usize, row: usize, column: usize) -> u8 {
        let low = self.vram[tile_address + row * 2];
        let high = self.vram[tile_address + row * 2 + 1];
        let bit = 7 - column;
        ((high >> bit) & 1) << 1 | ((low >> bit) & 1)
    }

    /// VRAM offset of background/window tile `index`, honouring LCDC bit 4.
    fn bg_tile_address(&self, index: u8) -> usize {
        if self.lcdc & 0x10 != 0 {
            usize::from(index) * 16
        } else {
            (0x1000 + i32::from(index as i8) * 16) as usize
        }
    }

    /// The background or window pixel at (`x`, `y`) of the tile map at VRAM
    /// offset `map`.
    fn map_pixel(&self, map: usize, x: usize, y: usize) -> BgPixel {
        let entry = map + (y / 8) * 32 + x / 8;
        let index = self.vram[entry];
        // Attributes live at the same offset in bank 1.
        let attributes = if self.cgb() {
            self.vram[0x2000 + entry]
        } else {
            0
        };
        let bank = if attributes & 0x08 != 0 { 0x2000 } else { 0 };
        let row = if attributes & 0x40 != 0 {
            7 - y % 8
        } else {
            y % 8
        };
        let column = if attributes & 0x20 != 0 {
            7 - x % 8
        } else {
            x % 8
        };
        BgPixel {
            color: self.tile_pixel(bank + self.bg_tile_address(index), row, column),
            palette: attributes & 7,
            priority: attributes & 0x80 != 0,
        }
    }

    fn render_line(&mut self) {
        let ly = usize::from(self.ly);
        let mut background = [BgPixel::default(); WIDTH];

        // On DMG, LCDC bit 0 off blanks the background and window to colour 0.
        if self.cgb() || self.lcdc & 0x01 != 0 {
            let map = if self.lcdc & 0x08 != 0 {
                0x1800 + 0x400
            } else {
                0x1800
            };
            let y = usize::from(self.ly.wrapping_add(self.scy));
            for (x, pixel) in background.iter_mut().enumerate() {
                *pixel = self.map_pixel(map, (x + usize::from(self.scx)) & 0xFF, y);
            }

            if self.window_visible_on_line() {
                let map = if self.lcdc & 0x40 != 0 {
                    0x1800 + 0x400
                } else {
                    0x1800
                };
                let start = usize::from(self.wx).saturating_sub(7);
                let skip = 7usize.saturating_sub(usize::from(self.wx));
                let y = usize::from(self.window_line);
                for (x, pixel) in background.iter_mut().enumerate().skip(start) {
                    *pixel = self.map_pixel(map, x - start + skip, y);
                }
                self.window_line += 1;
            }
        }

        let mut row = [WHITE; WIDTH];
        for (out, pixel) in row.iter_mut().zip(&background) {
            *out = if self.cgb() {
                cgb_color(&self.bg_palettes, pixel.palette, pixel.color)
            } else {
                DMG_SHADES[usize::from(palette_shade(self.bgp, pixel.color))]
            };
        }
        if self.lcdc & 0x02 != 0 {
            self.draw_sprites(&background, &mut row);
        }
        self.pixels[ly * WIDTH..(ly + 1) * WIDTH].copy_from_slice(&row);
    }

    fn draw_sprites(&self, background: &[BgPixel; WIDTH], row: &mut [u32; WIDTH]) {
        let tall = self.lcdc & 0x04 != 0;
        let height = if tall { 16 } else { 8 };
        let mut sprites = self.line_sprites();
        if self.priority_by_x {
            // DMG priority: smaller X wins, then lower OAM index.
            sprites.sort_by_key(|sprite| (sprite.x, sprite.index));
        }
        // CGB with LCDC bit 0 clear: objects always draw over the background.
        let background_can_win = !self.cgb() || self.lcdc & 0x01 != 0;

        let mut claimed = [false; WIDTH];
        for sprite in sprites {
            let mut line = usize::from(self.ly) + 16 - usize::from(sprite.y);
            if sprite.attributes & 0x40 != 0 {
                line = height - 1 - line;
            }
            let tile = if tall {
                sprite.tile & 0xFE
            } else {
                sprite.tile
            };
            let bank = if self.cgb() && sprite.attributes & 0x08 != 0 {
                0x2000
            } else {
                0
            };
            let tile_address = bank + usize::from(tile) * 16;
            for column in 0..8 {
                let Some(x) = (usize::from(sprite.x) + column).checked_sub(8) else {
                    continue;
                };
                if x >= WIDTH || claimed[x] {
                    continue;
                }
                let pixel = if sprite.attributes & 0x20 != 0 {
                    7 - column
                } else {
                    column
                };
                let color = self.tile_pixel(tile_address, line, pixel);
                if color == 0 {
                    continue;
                }
                // An opaque pixel hides lower-priority objects even when the
                // background is drawn over it.
                claimed[x] = true;
                let behind = background_can_win
                    && background[x].color != 0
                    && (sprite.attributes & 0x80 != 0 || background[x].priority);
                if behind {
                    continue;
                }
                row[x] = if self.cgb() {
                    cgb_color(&self.obj_palettes, sprite.attributes & 7, color)
                } else {
                    let palette = if sprite.attributes & 0x10 != 0 {
                        self.obp1
                    } else {
                        self.obp0
                    };
                    DMG_SHADES[usize::from(palette_shade(palette, color))]
                };
            }
        }
    }

    fn vram_index(&self, address: u16) -> usize {
        usize::from(self.vram_bank) * 0x2000 + usize::from(address & 0x1FFF)
    }

    pub(crate) fn read_vram(&self, address: u16) -> u8 {
        self.vram[self.vram_index(address)]
    }

    pub(crate) fn write_vram(&mut self, address: u16, value: u8) {
        let index = self.vram_index(address);
        self.vram[index] = value;
    }

    pub(crate) fn read_oam(&self, address: u16) -> u8 {
        self.oam[usize::from(address - 0xFE00)]
    }

    pub(crate) fn write_oam(&mut self, address: u16, value: u8) {
        self.oam[usize::from(address - 0xFE00)] = value;
    }

    /// OAM DMA writes OAM byte `index` directly.
    pub(crate) fn write_oam_dma(&mut self, index: u8, value: u8) {
        self.oam[usize::from(index)] = value;
    }

    pub(crate) fn read_register(&self, address: u16) -> u8 {
        let cgb = self.cgb();
        match address {
            0xFF40 => self.lcdc,
            0xFF41 => {
                let mode = if self.lcd_enabled() {
                    self.mode as u8
                } else {
                    0
                };
                let coincidence = u8::from(self.ly == self.lyc) << 2;
                0x80 | self.stat_select | coincidence | mode
            }
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            0xFF44 => self.ly,
            0xFF45 => self.lyc,
            0xFF47 => self.bgp,
            0xFF48 => self.obp0,
            0xFF49 => self.obp1,
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            0xFF4F if cgb => 0xFE | self.vram_bank,
            0xFF68 if cgb => self.bg_palette_select | 0x40,
            0xFF69 if cgb => self.bg_palettes[usize::from(self.bg_palette_select & 0x3F)],
            0xFF6A if cgb => self.obj_palette_select | 0x40,
            0xFF6B if cgb => self.obj_palettes[usize::from(self.obj_palette_select & 0x3F)],
            0xFF6C if cgb => 0xFE | u8::from(self.priority_by_x),
            _ => 0xFF,
        }
    }

    pub(crate) fn write_register(&mut self, address: u16, value: u8, interrupt_flag: &mut u8) {
        let cgb = self.cgb();
        match address {
            0xFF40 => {
                let was_on = self.lcd_enabled();
                self.lcdc = value;
                if was_on && !self.lcd_enabled() {
                    self.ly = 0;
                    self.dot = 0;
                    self.mode = Mode::HBlank;
                    self.stat_line = false;
                    self.pixels.fill(WHITE);
                    self.frame_ready = true;
                } else if !was_on && self.lcd_enabled() {
                    // The first line after switching on skips OAM scan.
                    self.ly = 0;
                    self.dot = 0;
                    self.mode = Mode::HBlank;
                    self.window_line = 0;
                    self.window_in_frame = self.wy == 0;
                }
            }
            0xFF41 => self.stat_select = value & 0x78,
            0xFF42 => self.scy = value,
            0xFF43 => self.scx = value,
            0xFF45 => self.lyc = value,
            0xFF47 => self.bgp = value,
            0xFF48 => self.obp0 = value,
            0xFF49 => self.obp1 = value,
            0xFF4A => self.wy = value,
            0xFF4B => self.wx = value,
            0xFF4F if cgb => self.vram_bank = value & 1,
            0xFF68 if cgb => self.bg_palette_select = value & 0xBF,
            0xFF69 if cgb => {
                write_palette(&mut self.bg_palettes, &mut self.bg_palette_select, value);
            }
            0xFF6A if cgb => self.obj_palette_select = value & 0xBF,
            0xFF6B if cgb => {
                write_palette(&mut self.obj_palettes, &mut self.obj_palette_select, value);
            }
            0xFF6C if cgb => self.priority_by_x = value & 1 != 0,
            _ => {}
        }
        if self.lcd_enabled() {
            self.update_stat_line(interrupt_flag);
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct BgPixel {
    /// Colour index 0–3 before the palette.
    color: u8,
    /// CGB palette number.
    palette: u8,
    /// CGB attribute bit 7: the background wins over objects.
    priority: bool,
}

#[derive(Debug, Clone, Copy)]
struct Sprite {
    y: u8,
    x: u8,
    tile: u8,
    attributes: u8,
    index: usize,
}

/// Maps colour index `color` through a DMG palette register.
fn palette_shade(palette: u8, color: u8) -> u8 {
    (palette >> (color * 2)) & 3
}

/// Colour `color` of CGB palette `palette`, widening each 5-bit channel to
/// 8 bits as `(x << 3) | (x >> 2)`.
fn cgb_color(palettes: &[u8], palette: u8, color: u8) -> u32 {
    let index = usize::from(palette) * 8 + usize::from(color) * 2;
    let value = u16::from_le_bytes([palettes[index], palettes[index + 1]]);
    let channel = |shift: u16| {
        let x = ((value >> shift) & 0x1F) as u8;
        x << 3 | x >> 2
    };
    rgb(channel(0), channel(5), channel(10))
}

/// Writes palette RAM at the index in `select`, auto-incrementing if bit 7
/// is set.
fn write_palette(palettes: &mut [u8], select: &mut u8, value: u8) {
    palettes[usize::from(*select & 0x3F)] = value;
    if *select & 0x80 != 0 {
        *select = 0x80 | ((*select + 1) & 0x3F);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgb_colors_widen_five_bit_channels() {
        let palettes = [0xFF, 0x7F, 0x1F, 0x00];
        assert_eq!(cgb_color(&palettes, 0, 0), rgb(0xFF, 0xFF, 0xFF));
        assert_eq!(cgb_color(&palettes, 0, 1), rgb(0xFF, 0x00, 0x00));
    }

    #[test]
    fn palette_writes_auto_increment_and_wrap() {
        let mut palettes = [0u8; 64];
        let mut select = 0x80 | 63;
        write_palette(&mut palettes, &mut select, 0x12);
        write_palette(&mut palettes, &mut select, 0x34);
        assert_eq!((palettes[63], palettes[0], select), (0x12, 0x34, 0x81));
    }
}
