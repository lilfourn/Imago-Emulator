//! Background layers (GBATEK, "DS Video BG Modes / Control", "DS Video
//! Extended Palettes", "DS 3D Final 2D Output", and the GBA's "LCD VRAM BG
//! Screen Data Format" and "LCD I/O BG Rotation/Scaling").
//!
//! Each enabled background draws one line into its own buffer of layer
//! pixels (see [`super::OPAQUE`]), 0 where transparent.

use super::cache::HalfwordCache;
use super::{Engine2d, WIDTH, layer_3d_pixel, layer_color};
use crate::vram::{Engine, EngineMemory};

/// What a background shows, by BG mode and number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BgKind {
    Off,
    /// Scrolling tiles with 16-bit map entries.
    Text,
    /// The 3D engine's output (engine A's BG0).
    ThreeD,
    /// Rotation/scaling tiles with 8-bit map entries.
    Affine,
    /// Extended rotation/scaling: 16-bit map entries, 256-colour bitmap or
    /// direct-colour bitmap, by BGxCNT bits 7 and 2.
    Extended,
    /// Mode 6's 256-colour bitmap filling all 512 KiB (engine A).
    Large,
}

/// Extended bitmap sizes by BGxCNT bits 14–15.
const BITMAP_SIZES: [(u32, u32); 4] = [(128, 128), (256, 256), (512, 256), (512, 512)];

/// How a text background's tiles are stored and coloured.
struct TextTiles {
    char_base: u32,
    eight_bit: bool,
    /// Extended palette slot for 256-colour tiles, when DISPCNT enables
    /// extended BG palettes.
    ext_slot: Option<u32>,
}

impl Engine2d {
    /// Draws `line` of every enabled background into `backgrounds`. Returns
    /// which were drawn.
    pub(super) fn draw_backgrounds(
        &self,
        line: u32,
        memory: &impl EngineMemory,
        bg0_3d: Option<&[u32; WIDTH]>,
        backgrounds: &mut [[u32; WIDTH]; 4],
    ) -> [bool; 4] {
        let mut drawn = [false; 4];
        for ((index, out), was_drawn) in backgrounds.iter_mut().enumerate().zip(&mut drawn) {
            *was_drawn = match self.bg_kind(index) {
                BgKind::Off => false,
                BgKind::Text => {
                    self.text_line(index, line, memory, out);
                    true
                }
                // No 3D line (the 3D engine is off) leaves BG0 transparent.
                BgKind::ThreeD => bg0_3d.is_some_and(|pixels| {
                    self.layer_3d(pixels, out);
                    true
                }),
                BgKind::Affine => {
                    self.affine_tiles_line(index, line, memory, out);
                    true
                }
                BgKind::Extended => {
                    self.extended_line(index, line, memory, out);
                    true
                }
                BgKind::Large => {
                    self.large_bitmap_line(index, line, memory, out);
                    true
                }
            };
        }
        drawn
    }

    fn bg_kind(&self, index: usize) -> BgKind {
        let dispcnt = self.registers.dispcnt;
        if dispcnt & (0x100 << index) == 0 {
            return BgKind::Off;
        }
        // Engine B's DISPCNT has no bit 3.
        if index == 0 && dispcnt & (1 << 3) != 0 {
            return BgKind::ThreeD;
        }
        match (dispcnt & 7, index) {
            (0..=5, 0 | 1) | (0, _) | (1 | 3, 2) => BgKind::Text,
            (1, 3) | (2, 2 | 3) | (4, 2) => BgKind::Affine,
            (3..=5, 3) | (5, 2) => BgKind::Extended,
            (6, 2) if self.engine == Engine::A => BgKind::Large,
            _ => BgKind::Off,
        }
    }

    /// Mosaic block size for a background, (1, 1) when its BGxCNT bit 6 is
    /// clear.
    fn bg_mosaic(&self, control: u16) -> (u32, u32) {
        if control & (1 << 6) != 0 {
            self.registers.bg_mosaic()
        } else {
            (1, 1)
        }
    }

    /// Character and screen base addresses of a tiled background. Engine A
    /// adds DISPCNT's 64 KiB offsets (engine B's DISPCNT lacks those bits).
    fn tile_bases(&self, control: u16) -> (u32, u32) {
        let dispcnt = self.registers.dispcnt;
        let control = u32::from(control);
        let char_base = ((control >> 2) & 0xF) * 0x4000 + ((dispcnt >> 24) & 7) * 0x1_0000;
        let screen_base = ((control >> 8) & 0x1F) * 0x800 + ((dispcnt >> 27) & 7) * 0x1_0000;
        (char_base, screen_base)
    }

    fn text_line(
        &self,
        index: usize,
        line: u32,
        memory: &impl EngineMemory,
        out: &mut [u32; WIDTH],
    ) {
        let control = self.registers.bgcnt[index];
        let (char_base, screen_base) = self.tile_bases(control);
        let eight_bit = control & (1 << 7) != 0;
        // BG0 and BG1 may use slots 2 and 3 instead of their own.
        let slot = if index < 2 && control & (1 << 13) != 0 {
            index + 2
        } else {
            index
        };
        let tiles = TextTiles {
            char_base,
            eight_bit,
            ext_slot: (eight_bit && self.registers.dispcnt & (1 << 30) != 0).then_some(slot as u32),
        };
        let (width, height) = match control >> 14 {
            0 => (256, 256),
            1 => (512, 256),
            2 => (256, 512),
            _ => (512, 512),
        };
        let (mosaic_x, mosaic_y) = self.bg_mosaic(control);
        let y = (line - line % mosaic_y + u32::from(self.registers.vofs[index])) & (height - 1);
        let scroll_x = u32::from(self.registers.hofs[index]);
        // Maps wider or taller than 256 pixels are 2 KiB blocks of 32x32
        // tiles.
        let row_base = screen_base + (y / 256) * (width / 256) * 0x800 + (y % 256 / 8) * 64;
        // The row of the tile covering map column `x`.
        let tile_row_at = |x: u32| {
            let entry = memory.bg(row_base + (x / 256) * 0x800 + (x % 256 / 8) * 2);
            self.text_tile_row(memory, &tiles, entry, y % 8)
        };

        if mosaic_x == 1 {
            // Whole tile rows at a time; the fine scroll cuts the first.
            let mut screen_x = 0;
            let mut x = scroll_x & (width - 1);
            while screen_x < WIDTH {
                let fine = (x % 8) as usize;
                let count = (8 - fine).min(WIDTH - screen_x);
                out[screen_x..screen_x + count]
                    .copy_from_slice(&tile_row_at(x)[fine..fine + count]);
                screen_x += count;
                x = (x + count as u32) & (width - 1);
            }
            return;
        }
        let mut column = u32::MAX;
        let mut tile_row = [0; 8];
        let mut sample_x = 0;
        let mut mosaic_left = 0;
        for (screen_x, pixel) in (0u32..).zip(out.iter_mut()) {
            if mosaic_left == 0 {
                sample_x = screen_x;
                mosaic_left = mosaic_x;
            }
            mosaic_left -= 1;
            let x = (sample_x + scroll_x) & (width - 1);
            if x / 8 != column {
                column = x / 8;
                tile_row = tile_row_at(x);
            }
            *pixel = tile_row[(x % 8) as usize];
        }
    }

    /// Row `fine_y` of the tile a text map entry names, flips applied.
    fn text_tile_row(
        &self,
        memory: &impl EngineMemory,
        tiles: &TextTiles,
        entry: u16,
        fine_y: u32,
    ) -> [u32; 8] {
        let tile = u32::from(entry & 0x3FF);
        let palette = u32::from(entry >> 12);
        let fine_y = if entry & (1 << 11) != 0 {
            7 - fine_y
        } else {
            fine_y
        };
        let mut row = [0; 8];
        if tiles.eight_bit {
            let address = tiles.char_base + tile * 64 + fine_y * 8;
            for (pair, pixels) in (0u32..).zip(row.chunks_exact_mut(2)) {
                let indices = memory.bg(address + pair * 2);
                for (pixel, index) in pixels.iter_mut().zip([indices & 0xFF, indices >> 8]) {
                    *pixel = match tiles.ext_slot {
                        _ if index == 0 => 0,
                        Some(slot) => layer_color(
                            memory.bg_ext_palette(slot, palette * 256 + u32::from(index)),
                        ),
                        None => self.bg_color(u32::from(index)),
                    };
                }
            }
        } else {
            let address = tiles.char_base + tile * 32 + fine_y * 4;
            let nibbles = u32::from(memory.bg(address)) | u32::from(memory.bg(address + 2)) << 16;
            for (shift, pixel) in (0..32).step_by(4).zip(row.iter_mut()) {
                let index = (nibbles >> shift) & 0xF;
                if index != 0 {
                    *pixel = self.bg_color(palette * 16 + index);
                }
            }
        }
        if entry & (1 << 10) != 0 {
            row.reverse();
        }
        row
    }

    /// Draws a rotation/scaling background over a `width`×`height` plane
    /// (powers of two): walks from the internal reference point by (PA, PC)
    /// per pixel and asks `sample` for each texel inside the plane, or
    /// wrapped into it when BGxCNT bit 13 is set.
    fn affine_line(
        &self,
        index: usize,
        line: u32,
        (width, height): (u32, u32),
        mut sample: impl FnMut(u32, u32) -> u32,
        out: &mut [u32; WIDTH],
    ) {
        let control = self.registers.bgcnt[index];
        let affine = &self.registers.affine[index - 2];
        let (pa, pc) = (i32::from(affine.pa), i32::from(affine.pc));
        let (mosaic_x, mosaic_y) = self.bg_mosaic(control);
        // Vertical mosaic repeats the first line of each block, whose
        // reference point was this many (PB, PD) steps ago.
        let back = (line % mosaic_y) as i32;
        let (mut x, mut y) = affine.reference;
        x = x.wrapping_sub(i32::from(affine.pb) * back);
        y = y.wrapping_sub(i32::from(affine.pd) * back);
        let wrap = control & (1 << 13) != 0;

        let mut texel = 0;
        let mut mosaic_left = 0;
        for pixel in out.iter_mut() {
            if mosaic_left == 0 {
                mosaic_left = mosaic_x;
                let (tx, ty) = (x >> 8, y >> 8);
                texel = if wrap {
                    sample(tx as u32 & (width - 1), ty as u32 & (height - 1))
                } else if (0..width as i32).contains(&tx) && (0..height as i32).contains(&ty) {
                    sample(tx as u32, ty as u32)
                } else {
                    0
                };
            }
            mosaic_left -= 1;
            *pixel = texel;
            x = x.wrapping_add(pa);
            y = y.wrapping_add(pc);
        }
    }

    /// Affine background: 8-bit map entries and 256-colour tiles.
    fn affine_tiles_line(
        &self,
        index: usize,
        line: u32,
        memory: &impl EngineMemory,
        out: &mut [u32; WIDTH],
    ) {
        let control = self.registers.bgcnt[index];
        let size = 128u32 << (control >> 14);
        let (char_base, screen_base) = self.tile_bases(control);
        let read = |offset| memory.bg(offset);
        let (mut map, mut chars) = (HalfwordCache::new(), HalfwordCache::new());
        let sample = |x: u32, y: u32| {
            let tile = u32::from(map.byte(screen_base + (y / 8) * (size / 8) + x / 8, read));
            let index = chars.byte(char_base + tile * 64 + (y % 8) * 8 + x % 8, read);
            if index == 0 {
                0
            } else {
                self.bg_color(u32::from(index))
            }
        };
        self.affine_line(index, line, (size, size), sample, out);
    }

    /// Extended background (BG2/BG3): tiles with 16-bit map entries when
    /// BGxCNT bit 7 is clear, otherwise a bitmap, direct-colour if bit 2 is
    /// set. Bitmaps start at the screen base in 16 KiB steps, without
    /// DISPCNT's offset.
    fn extended_line(
        &self,
        index: usize,
        line: u32,
        memory: &impl EngineMemory,
        out: &mut [u32; WIDTH],
    ) {
        let control = self.registers.bgcnt[index];
        if control & (1 << 7) == 0 {
            self.extended_tiles_line(index, line, memory, out);
            return;
        }
        let size = BITMAP_SIZES[usize::from(control >> 14)];
        let width = size.0;
        let base = u32::from((control >> 8) & 0x1F) * 0x4000;
        let read = |offset| memory.bg(offset);
        let mut cache = HalfwordCache::new();
        if control & (1 << 2) == 0 {
            let sample = |x: u32, y: u32| match cache.byte(base + y * width + x, read) {
                0 => 0,
                index => self.bg_color(u32::from(index)),
            };
            self.affine_line(index, line, size, sample, out);
        } else {
            let sample = |x: u32, y: u32| {
                let color = cache.halfword(base + (y * width + x) * 2, read);
                // Bit 15 is the pixel's alpha: clear is transparent.
                if color & 0x8000 != 0 {
                    layer_color(color)
                } else {
                    0
                }
            };
            self.affine_line(index, line, size, sample, out);
        }
    }

    /// Extended background with 16-bit map entries: text-style flips and
    /// palette numbers on 256-colour tiles. With extended BG palettes,
    /// BG2 and BG3 use slots 2 and 3.
    fn extended_tiles_line(
        &self,
        index: usize,
        line: u32,
        memory: &impl EngineMemory,
        out: &mut [u32; WIDTH],
    ) {
        let control = self.registers.bgcnt[index];
        let size = 128u32 << (control >> 14);
        let (char_base, screen_base) = self.tile_bases(control);
        let ext_slot = (self.registers.dispcnt & (1 << 30) != 0).then_some(index as u32);
        let read = |offset| memory.bg(offset);
        let (mut map, mut chars) = (HalfwordCache::new(), HalfwordCache::new());
        let sample = |x: u32, y: u32| {
            let entry = map.halfword(screen_base + ((y / 8) * (size / 8) + x / 8) * 2, read);
            let column = if entry & (1 << 10) != 0 {
                7 - x % 8
            } else {
                x % 8
            };
            let row = if entry & (1 << 11) != 0 {
                7 - y % 8
            } else {
                y % 8
            };
            let tile = u32::from(entry & 0x3FF);
            let index = chars.byte(char_base + tile * 64 + row * 8 + column, read);
            match ext_slot {
                _ if index == 0 => 0,
                Some(slot) => layer_color(
                    memory.bg_ext_palette(slot, u32::from(entry >> 12) * 256 + u32::from(index)),
                ),
                None => self.bg_color(u32::from(index)),
            }
        };
        self.affine_line(index, line, (size, size), sample, out);
    }

    /// Mode 6's large 256-colour bitmap from the start of BG VRAM: 512×1024,
    /// or 1024×512 when BGxCNT bit 14 is set (sizes 2–3 are reserved and
    /// read as 0–1).
    fn large_bitmap_line(
        &self,
        index: usize,
        line: u32,
        memory: &impl EngineMemory,
        out: &mut [u32; WIDTH],
    ) {
        let control = self.registers.bgcnt[index];
        let size = if control & (1 << 14) == 0 {
            (512, 1024)
        } else {
            (1024, 512)
        };
        let width = size.0;
        let read = |offset| memory.bg(offset);
        let mut cache = HalfwordCache::new();
        let sample = |x: u32, y: u32| match cache.byte(y * width + x, read) {
            0 => 0,
            index => self.bg_color(u32::from(index)),
        };
        self.affine_line(index, line, size, sample, out);
    }

    /// The 3D engine's line as BG0. BG0HOFS scrolls it within a 512-pixel
    /// span: the 256 rendered pixels, then 256 transparent ones. Alpha 0
    /// is transparent; mosaic does not apply.
    fn layer_3d(&self, pixels: &[u32; WIDTH], out: &mut [u32; WIDTH]) {
        let scroll = usize::from(self.registers.hofs[0]);
        for (screen_x, pixel) in out.iter_mut().enumerate() {
            *pixel = pixels
                .get((screen_x + scroll) & 0x1FF)
                .map_or(0, |&value| layer_3d_pixel(value));
        }
    }

    /// Whether BG0 is the 3D layer, when it is enabled.
    pub(super) fn shows_3d_as_bg0(&self) -> bool {
        self.bg_kind(0) == BgKind::ThreeD
    }
}
