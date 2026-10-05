//! The picture processing unit (Pan Docs, "Rendering", "LCD Control",
//! "LCD Status Registers", "Scrolling", "OAM").
//!
//! Timing follows the hardware's modes dot by dot (456 dots per line, 154
//! lines per frame). Pixels are drawn a whole line at a time when mode 3
//! starts, using the registers as they are at that moment. That is enough
//! for nearly every game; mid-line register effects would need a pixel FIFO.

use serde::{Deserialize, Serialize};

use crate::interrupts;

pub(crate) const WIDTH: usize = 160;
pub(crate) const HEIGHT: usize = 144;
const DOTS_PER_LINE: u16 = 456;
const LINES_PER_FRAME: u8 = 154;
const OAM_SCAN_DOTS: u16 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Mode {
    HBlank = 0,
    VBlank = 1,
    OamScan = 2,
    Drawing = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Ppu {
    vram: Box<[u8]>,
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
    /// Shade (0 = lightest, 3 = darkest) of each pixel of the frame being
    /// drawn.
    shades: Box<[u8]>,
    frame_ready: bool,
}

impl Ppu {
    /// The state the DMG boot ROM leaves: LCD on, BGP = 0xFC.
    pub(crate) fn after_boot() -> Self {
        Self {
            vram: vec![0; 0x2000].into_boxed_slice(),
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
            mode: Mode::OamScan,
            dot: 0,
            drawing_dots: 172,
            window_in_frame: false,
            window_line: 0,
            stat_line: false,
            shades: vec![0; WIDTH * HEIGHT].into_boxed_slice(),
            frame_ready: false,
        }
    }

    fn lcd_enabled(&self) -> bool {
        self.lcdc & 0x80 != 0
    }

    /// Shades of the last completed frame, row-major.
    pub(crate) fn shades(&self) -> &[u8] {
        &self.shades
    }

    /// Whether a frame finished since the last call.
    pub(crate) fn take_frame_ready(&mut self) -> bool {
        std::mem::take(&mut self.frame_ready)
    }

    /// Whether the LCD is on; with it off no frames complete.
    pub(crate) fn is_on(&self) -> bool {
        self.lcd_enabled()
    }

    pub(crate) fn vram(&self) -> &[u8] {
        &self.vram
    }

    /// Advances one M-cycle (four dots).
    pub(crate) fn tick(&mut self, interrupt_flag: &mut u8) {
        if !self.lcd_enabled() {
            return;
        }
        for _ in 0..4 {
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
            Mode::VBlank if self.ly == HEIGHT as u8 && self.dot == 0 => {
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
        self.lcdc & 0x21 == 0x21 && self.window_in_frame && self.wx <= 166
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

    fn render_line(&mut self) {
        let ly = usize::from(self.ly);
        let mut bg_colors = [0u8; WIDTH];
        let mut row = [0u8; WIDTH];

        if self.lcdc & 0x01 != 0 {
            let map = if self.lcdc & 0x08 != 0 {
                0x1C00
            } else {
                0x1800
            };
            let y = usize::from(self.ly.wrapping_add(self.scy));
            for (x, color) in bg_colors.iter_mut().enumerate() {
                let bx = (x + usize::from(self.scx)) & 0xFF;
                let index = self.vram[map + (y / 8) * 32 + bx / 8];
                *color = self.tile_pixel(self.bg_tile_address(index), y % 8, bx % 8);
            }

            if self.window_visible_on_line() {
                let map = if self.lcdc & 0x40 != 0 {
                    0x1C00
                } else {
                    0x1800
                };
                let start = usize::from(self.wx).saturating_sub(7);
                let y = usize::from(self.window_line);
                let skip = 7usize.saturating_sub(usize::from(self.wx));
                for (x, color) in bg_colors.iter_mut().enumerate().skip(start) {
                    let wx = x - start + skip;
                    let index = self.vram[map + (y / 8) * 32 + wx / 8];
                    *color = self.tile_pixel(self.bg_tile_address(index), y % 8, wx % 8);
                }
                self.window_line += 1;
            }
        }
        for (shade, color) in row.iter_mut().zip(bg_colors) {
            *shade = palette_shade(self.bgp, color);
        }

        if self.lcdc & 0x02 != 0 {
            self.draw_sprites(&bg_colors, &mut row);
        }
        self.shades[ly * WIDTH..(ly + 1) * WIDTH].copy_from_slice(&row);
    }

    fn draw_sprites(&self, bg_colors: &[u8; WIDTH], row: &mut [u8; WIDTH]) {
        let tall = self.lcdc & 0x04 != 0;
        let height = if tall { 16 } else { 8 };
        let mut sprites = self.line_sprites();
        // DMG priority: smaller X wins, then lower OAM index.
        sprites.sort_by_key(|sprite| (sprite.x, sprite.index));

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
            let tile_address = usize::from(tile) * 16;
            let palette = if sprite.attributes & 0x10 != 0 {
                self.obp1
            } else {
                self.obp0
            };
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
                let behind_background = sprite.attributes & 0x80 != 0 && bg_colors[x] != 0;
                if !behind_background {
                    row[x] = palette_shade(palette, color);
                }
            }
        }
    }

    pub(crate) fn read_vram(&self, address: u16) -> u8 {
        self.vram[usize::from(address & 0x1FFF)]
    }

    pub(crate) fn write_vram(&mut self, address: u16, value: u8) {
        self.vram[usize::from(address & 0x1FFF)] = value;
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
            _ => 0xFF,
        }
    }

    pub(crate) fn write_register(&mut self, address: u16, value: u8, interrupt_flag: &mut u8) {
        match address {
            0xFF40 => {
                let was_on = self.lcd_enabled();
                self.lcdc = value;
                if was_on && !self.lcd_enabled() {
                    self.ly = 0;
                    self.dot = 0;
                    self.mode = Mode::HBlank;
                    self.stat_line = false;
                    self.shades.fill(0);
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
            0xFF44 => {}
            0xFF45 => self.lyc = value,
            0xFF47 => self.bgp = value,
            0xFF48 => self.obp0 = value,
            0xFF49 => self.obp1 = value,
            0xFF4A => self.wy = value,
            0xFF4B => self.wx = value,
            _ => {}
        }
        if self.lcd_enabled() {
            self.update_stat_line(interrupt_flag);
        }
    }
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
