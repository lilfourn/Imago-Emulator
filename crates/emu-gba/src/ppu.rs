//! The picture processing unit (GBATEK, "LCD Video Controller").
//!
//! Each of the 228 lines takes 1,232 cycles: 1,006 of drawing, then HBlank.
//! Lines 160–227 are VBlank. The bus schedules the two events per line and
//! calls [`Ppu::start_hblank`] and [`Ppu::start_line`]; the PPU renders a
//! visible line when its HBlank begins, using the registers at that moment.
//!
//! Bitmap modes 3–5 are drawn here; the tiled and affine modes 0–2, objects,
//! windows and blending arrive in milestone M9 and draw the backdrop until
//! then.

use emu_core::rgb;
use serde::{Deserialize, Serialize};

pub(crate) const WIDTH: usize = 240;
pub(crate) const HEIGHT: usize = 160;
/// Cycles from the start of a line to HBlank.
pub(crate) const HDRAW_CYCLES: u64 = 1006;
/// Cycles in a whole line.
pub(crate) const LINE_CYCLES: u64 = 1232;
const LINES: u16 = 228;

/// Interrupt request bits produced by the PPU.
pub(crate) mod irq {
    pub(crate) const VBLANK: u16 = 1 << 0;
    pub(crate) const HBLANK: u16 = 1 << 1;
    pub(crate) const VCOUNT: u16 = 1 << 2;
}

/// What happened at a line event, for DMA triggers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Signals {
    /// VBlank started (line 160).
    pub(crate) vblank: bool,
    /// HBlank started on a visible line.
    pub(crate) hblank: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Ppu {
    palette: Box<[u8]>,
    vram: Box<[u8]>,
    oam: Box<[u8]>,
    /// Raw register values, 0x000–0x057.
    registers: Box<[u8]>,
    vcount: u16,
    in_hblank: bool,
    /// The frame being drawn, as `0xFFRRGGBB`.
    pixels: Box<[u32]>,
    frame_ready: bool,
}

/// Converts a BGR555 colour to `0xFFRRGGBB`.
pub(crate) fn color(bgr555: u16) -> u32 {
    let channel = |shift: u16| {
        let x = ((bgr555 >> shift) & 0x1F) as u8;
        x << 3 | x >> 2
    };
    rgb(channel(0), channel(5), channel(10))
}

impl Ppu {
    pub(crate) fn new() -> Self {
        Self {
            palette: vec![0; 0x400].into_boxed_slice(),
            vram: vec![0; 0x1_8000].into_boxed_slice(),
            oam: vec![0; 0x400].into_boxed_slice(),
            registers: vec![0; 0x58].into_boxed_slice(),
            vcount: 0,
            in_hblank: false,
            pixels: vec![rgb(0xFF, 0xFF, 0xFF); WIDTH * HEIGHT].into_boxed_slice(),
            frame_ready: false,
        }
    }

    fn register16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.registers[offset], self.registers[offset + 1]])
    }

    fn dispcnt(&self) -> u16 {
        self.register16(0x00)
    }

    fn dispstat(&self) -> u16 {
        self.register16(0x04)
    }

    /// Pixels of the last completed frame, row-major.
    pub(crate) fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Whether a frame finished since the last call.
    pub(crate) fn take_frame_ready(&mut self) -> bool {
        std::mem::take(&mut self.frame_ready)
    }

    pub(crate) fn vram(&self) -> &[u8] {
        &self.vram
    }

    /// HBlank begins on the current line. Returns interrupt requests.
    pub(crate) fn start_hblank(&mut self) -> (u16, Signals) {
        self.in_hblank = true;
        let visible = usize::from(self.vcount) < HEIGHT;
        if visible {
            self.render_line();
        }
        let request = if self.dispstat() & (1 << 4) != 0 {
            irq::HBLANK
        } else {
            0
        };
        (
            request,
            Signals {
                vblank: false,
                hblank: visible,
            },
        )
    }

    /// A new line begins. Returns interrupt requests.
    pub(crate) fn start_line(&mut self) -> (u16, Signals) {
        self.in_hblank = false;
        self.vcount = (self.vcount + 1) % LINES;
        let dispstat = self.dispstat();
        let mut request = 0;
        let mut signals = Signals::default();
        if usize::from(self.vcount) == HEIGHT {
            self.frame_ready = true;
            signals.vblank = true;
            if dispstat & (1 << 3) != 0 {
                request |= irq::VBLANK;
            }
        }
        if self.vcount == dispstat >> 8 && dispstat & (1 << 5) != 0 {
            request |= irq::VCOUNT;
        }
        (request, signals)
    }

    fn in_vblank(&self) -> bool {
        (HEIGHT as u16..LINES - 1).contains(&self.vcount)
    }

    pub(crate) fn read_register(&self, offset: usize) -> u16 {
        match offset {
            0x04 => {
                let flags = u16::from(self.in_vblank())
                    | u16::from(self.in_hblank) << 1
                    | u16::from(self.vcount == self.dispstat() >> 8) << 2;
                (self.dispstat() & 0xFF38) | flags
            }
            0x06 => self.vcount,
            // Readable registers; the rest are write-only.
            0x00 | 0x02 | 0x08..=0x0E | 0x48 | 0x4A | 0x50 | 0x52 => self.register16(offset),
            _ => 0,
        }
    }

    pub(crate) fn write_register(&mut self, offset: usize, value: u16) {
        if offset == 0x06 {
            return;
        }
        let value = if offset == 0x04 {
            (self.dispstat() & 0x0007) | (value & 0xFF38)
        } else {
            value
        };
        self.registers[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn read_palette(&self, offset: u32) -> u16 {
        let index = (offset & 0x3FE) as usize;
        u16::from_le_bytes([self.palette[index], self.palette[index + 1]])
    }

    pub(crate) fn write_palette(&mut self, offset: u32, value: u16) {
        let index = (offset & 0x3FE) as usize;
        self.palette[index..index + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// VRAM is 96 KiB mirrored every 128 KiB, with 0x18000–0x1FFFF
    /// repeating the last 32 KiB.
    fn vram_index(offset: u32) -> usize {
        let offset = offset & 0x1_FFFE;
        (if offset >= 0x1_8000 {
            offset - 0x8000
        } else {
            offset
        }) as usize
    }

    pub(crate) fn read_vram(&self, offset: u32) -> u16 {
        let index = Self::vram_index(offset);
        u16::from_le_bytes([self.vram[index], self.vram[index + 1]])
    }

    pub(crate) fn write_vram(&mut self, offset: u32, value: u16) {
        let index = Self::vram_index(offset);
        self.vram[index..index + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// A byte write to VRAM: in background memory it fills both bytes of the
    /// halfword; in object memory it is ignored.
    pub(crate) fn write_vram_byte(&mut self, offset: u32, value: u8) {
        let bitmap = (3..=5).contains(&(self.dispcnt() & 7));
        let object_start = if bitmap { 0x1_4000 } else { 0x1_0000 };
        if Self::vram_index(offset) < object_start {
            self.write_vram(offset, u16::from_le_bytes([value, value]));
        }
    }

    pub(crate) fn read_oam(&self, offset: u32) -> u16 {
        let index = (offset & 0x3FE) as usize;
        u16::from_le_bytes([self.oam[index], self.oam[index + 1]])
    }

    pub(crate) fn write_oam(&mut self, offset: u32, value: u16) {
        let index = (offset & 0x3FE) as usize;
        self.oam[index..index + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn render_line(&mut self) {
        let line = usize::from(self.vcount);
        let dispcnt = self.dispcnt();
        let mut row = [0u32; WIDTH];
        let backdrop = color(self.read_palette(0));

        if dispcnt & (1 << 7) != 0 {
            // Forced blank shows white.
            row.fill(rgb(0xFF, 0xFF, 0xFF));
        } else {
            row.fill(backdrop);
            if dispcnt & (1 << 10) != 0 {
                self.render_bitmap(dispcnt, line, &mut row);
            }
        }
        self.pixels[line * WIDTH..(line + 1) * WIDTH].copy_from_slice(&row);
    }

    /// Modes 3–5: background 2 as a bitmap.
    fn render_bitmap(&self, dispcnt: u16, line: usize, row: &mut [u32; WIDTH]) {
        let frame = if dispcnt & (1 << 4) != 0 { 0xA000 } else { 0 };
        match dispcnt & 7 {
            3 => {
                for (x, pixel) in row.iter_mut().enumerate() {
                    *pixel = color(self.read_vram(((line * WIDTH + x) * 2) as u32));
                }
            }
            4 => {
                for (x, pixel) in row.iter_mut().enumerate() {
                    let index = self.vram[frame + line * WIDTH + x];
                    if index != 0 {
                        *pixel = color(self.read_palette(u32::from(index) * 2));
                    }
                }
            }
            5 if line < 128 => {
                for (x, pixel) in row.iter_mut().enumerate().take(160) {
                    *pixel = color(self.read_vram((frame + (line * 160 + x) * 2) as u32));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_widen_five_bits() {
        assert_eq!(color(0x7FFF), rgb(0xFF, 0xFF, 0xFF));
        assert_eq!(color(0x001F), rgb(0xFF, 0, 0));
        assert_eq!(color(0x7C00), rgb(0, 0, 0xFF));
    }

    #[test]
    fn vblank_starts_at_line_160_and_ends_before_227() {
        let mut ppu = Ppu::new();
        ppu.write_register(0x04, 1 << 3);
        let mut vblank_irqs = 0;
        for _ in 0..LINES {
            let (request, _) = ppu.start_line();
            if request & irq::VBLANK != 0 {
                vblank_irqs += 1;
                assert_eq!(ppu.vcount, 160);
            }
            if ppu.vcount == 226 {
                assert!(ppu.in_vblank());
            }
            if ppu.vcount == 227 {
                assert!(!ppu.in_vblank());
            }
        }
        assert_eq!(vblank_irqs, 1);
    }

    #[test]
    fn vram_mirrors_its_last_32k() {
        let mut ppu = Ppu::new();
        ppu.write_vram(0x1_0000, 0x1234);
        assert_eq!(ppu.read_vram(0x1_8000), 0x1234);
        assert_eq!(ppu.read_vram(0x3_0000), 0x1234);
    }
}
