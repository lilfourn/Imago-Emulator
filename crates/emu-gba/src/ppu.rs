//! The picture processing unit (GBATEK, "LCD Video Controller").
//!
//! Each of the 228 lines takes 1,232 cycles: 1,007 of drawing, then HBlank.
//! Lines 160–227 are VBlank. The bus schedules the two events per line and
//! calls [`Ppu::start_hblank`] and [`Ppu::start_line`]; the PPU renders a
//! visible line when its HBlank begins, using the registers at that moment.
//! The interrupts trail the flags: HBlank's and VBlank's by a cycle,
//! VCount's by two (NanoBoyAdvance's timings).
//!
//! Drawing itself is in [`render`].

mod render;

use emu_core::rgb;
use emu_state::payload;
use serde::{Deserialize, Serialize};

pub(crate) const WIDTH: usize = 240;
pub(crate) const HEIGHT: usize = 160;
/// Cycles from the start of a line to HBlank.
pub(crate) const HDRAW_CYCLES: u64 = 1007;
/// Cycles in a whole line.
pub(crate) const LINE_CYCLES: u64 = 1232;
const LINES: u16 = 228;
/// The lines whose HBlank starts DMA 3 in video capture mode; the next
/// line's start stops it (GBATEK, "DMA3 Video Capture Mode").
const CAPTURE_LINES: std::ops::Range<u16> = 2..162;

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
    /// HBlank started on a line video capture copies (2–161).
    pub(crate) capture: bool,
    /// The line after the last that video capture copies began (162).
    pub(crate) capture_end: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Ppu {
    #[serde(with = "payload::Exact::<0x400>")]
    palette: Box<[u8]>,
    #[serde(with = "payload::Exact::<0x1_8000>")]
    vram: Box<[u8]>,
    #[serde(with = "payload::Exact::<0x400>")]
    oam: Box<[u8]>,
    /// Raw register values, 0x000–0x057.
    #[serde(with = "payload::Exact::<0x58>")]
    registers: Box<[u8]>,
    #[serde(with = "payload::Below::<{ LINES as u64 }>")]
    vcount: u16,
    in_hblank: bool,
    /// BG2 and BG3's internal affine reference points (X, Y), 20.8 fixed
    /// point. Loaded from BGxX/BGxY when written and at each frame's start,
    /// then stepped by PB/PD after every line.
    affine_reference: [(i32, i32); 2],
    /// The frame being drawn, as `0xFFRRGGBB`.
    #[serde(with = "payload::ExactList::<{ WIDTH * HEIGHT }>")]
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
            affine_reference: [(0, 0); 2],
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

    /// The register halfword at `offset` (0x00–0x56) as last written,
    /// write-only registers such as the scroll offsets included, without
    /// side effects. The renderer reads them this way; so do the debugger's
    /// graphics viewers.
    pub(crate) fn raw_register(&self, offset: usize) -> u16 {
        self.register16(offset)
    }

    /// HBlank begins on the current line. Returns interrupt requests.
    pub(crate) fn start_hblank(&mut self) -> (u16, Signals) {
        self.in_hblank = true;
        let visible = usize::from(self.vcount) < HEIGHT;
        if visible {
            self.render_line();
            self.step_affine_reference();
        }
        let request = if self.dispstat() & (1 << 4) != 0 {
            irq::HBLANK
        } else {
            0
        };
        (
            request,
            Signals {
                hblank: visible,
                capture: CAPTURE_LINES.contains(&self.vcount),
                ..Signals::default()
            },
        )
    }

    /// A new line begins. Returns interrupt requests.
    pub(crate) fn start_line(&mut self) -> (u16, Signals) {
        self.in_hblank = false;
        self.vcount = (self.vcount + 1) % LINES;
        if self.vcount == 0 {
            self.reload_affine_reference(0);
            self.reload_affine_reference(1);
        }
        let dispstat = self.dispstat();
        let mut request = 0;
        let mut signals = Signals {
            capture_end: self.vcount == CAPTURE_LINES.end,
            ..Signals::default()
        };
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

    /// Reads the register at `offset` (0x00–0x5E), with unused bits as 0.
    /// `None` for write-only and unused registers, which read as open bus.
    pub(crate) fn read_register(&self, offset: usize) -> Option<u16> {
        let readable = match offset {
            0x04 => {
                let flags = u16::from(self.in_vblank())
                    | u16::from(self.in_hblank) << 1
                    | u16::from(self.vcount == self.dispstat() >> 8) << 2;
                return Some((self.dispstat() & 0xFF38) | flags);
            }
            0x06 => return Some(self.vcount),
            0x00 | 0x02 | 0x0C | 0x0E => 0xFFFF,
            // BG0 and BG1 have no display area overflow bit.
            0x08 | 0x0A => 0xDFFF,
            0x48 | 0x4A => 0x3F3F, // WININ, WINOUT
            0x50 => 0x3FFF,        // BLDCNT
            0x52 => 0x1F1F,        // BLDALPHA
            _ => return None,
        };
        Some(self.register16(offset) & readable)
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
        match offset {
            0x28..=0x2E => self.reload_affine_reference(0),
            0x38..=0x3E => self.reload_affine_reference(1),
            _ => {}
        }
    }

    /// Copies BGxX/BGxY (28-bit signed) into the internal reference point.
    fn reload_affine_reference(&mut self, index: usize) {
        let base = 0x28 + index * 0x10;
        let read = |offset: usize| {
            let raw =
                u32::from(self.register16(offset)) | u32::from(self.register16(offset + 2)) << 16;
            ((raw << 4) as i32) >> 4
        };
        self.affine_reference[index] = (read(base), read(base + 4));
    }

    /// After each line the reference point moves by (PB, PD).
    fn step_affine_reference(&mut self) {
        for index in 0..2 {
            let base = 0x20 + index * 0x10;
            let pb = i32::from(self.register16(base + 2) as i16);
            let pd = i32::from(self.register16(base + 6) as i16);
            let (x, y) = &mut self.affine_reference[index];
            *x = x.wrapping_add(pb);
            *y = y.wrapping_add(pd);
        }
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
        let mut row = [0u32; WIDTH];
        if self.dispcnt() & (1 << 7) != 0 {
            // Forced blank shows white.
            row.fill(rgb(0xFF, 0xFF, 0xFF));
        } else {
            self.draw_line(line, &mut row);
        }
        self.pixels[line * WIDTH..(line + 1) * WIDTH].copy_from_slice(&row);
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
}
