//! A 2D engine's I/O registers at offsets 0x00–0x6F from 0x04000000
//! (engine A) or 0x04001000 (engine B) (GBATEK, "DS Video BG Modes /
//! Control", "DS Video Stuff", and the GBA's "LCD I/O" chapters).
//!
//! DISP3DCNT (0x60) belongs to the 3D engine, and DISPCAPCNT (0x64) and
//! DISP_MMEM_FIFO (0x68) to display capture, so they are not kept here.
//! Write-only registers read as zero.

use serde::{Deserialize, Serialize};

use crate::vram::Engine;

/// DISPCNT bits engine B lacks: BG0 as 3D (3), display modes 2–3 (17), the
/// VRAM display block (18–19), the bitmap OBJ 1D boundary (22) and the
/// character and screen base offsets (24–29).
const ENGINE_B_DISPCNT: u32 = !(1 << 3 | 1 << 17 | 3 << 18 | 1 << 22 | 0x3F << 24);

/// BG2 or BG3's rotation and scaling registers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct AffineBg {
    /// PA–PD, 8.8 fixed point.
    pub(super) pa: i16,
    pub(super) pb: i16,
    pub(super) pc: i16,
    pub(super) pd: i16,
    /// BGxX and BGxY as written: 28-bit signed, 20.8 fixed point.
    x: u32,
    y: u32,
    /// The internal reference point drawing starts from. Each coordinate is
    /// loaded from BGxX/BGxY when that register is written and at each
    /// frame's start, then stepped by (PB, PD) after every line.
    pub(super) reference: (i32, i32),
}

fn sign_extend_28(raw: u32) -> i32 {
    ((raw << 4) as i32) >> 4
}

impl AffineBg {
    fn reload(&mut self) {
        self.reference = (sign_extend_28(self.x), sign_extend_28(self.y));
    }

    fn step(&mut self) {
        let (x, y) = &mut self.reference;
        *x = x.wrapping_add(i32::from(self.pb));
        *y = y.wrapping_add(i32::from(self.pd));
    }

    /// Writes the halfword at `offset` (0x0–0xE) of the PA–BGxY block.
    fn write(&mut self, offset: u32, value: u16) {
        let low = |raw: u32| (raw & 0xFFFF_0000) | u32::from(value);
        let high = |raw: u32| (raw & 0xFFFF) | u32::from(value) << 16;
        match offset {
            0x0 => self.pa = value as i16,
            0x2 => self.pb = value as i16,
            0x4 => self.pc = value as i16,
            0x6 => self.pd = value as i16,
            0x8 | 0xA => {
                self.x = if offset == 0x8 {
                    low(self.x)
                } else {
                    high(self.x)
                };
                self.reference.0 = sign_extend_28(self.x);
            }
            0xC | 0xE => {
                self.y = if offset == 0xC {
                    low(self.y)
                } else {
                    high(self.y)
                };
                self.reference.1 = sign_extend_28(self.y);
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Registers {
    /// The DISPCNT bits this engine has.
    dispcnt_mask: u32,
    pub(super) dispcnt: u32,
    pub(super) bgcnt: [u16; 4],
    pub(super) hofs: [u16; 4],
    pub(super) vofs: [u16; 4],
    /// BG2 and BG3.
    pub(super) affine: [AffineBg; 2],
    /// WIN0H and WIN1H: X1 in the high byte, X2 in the low.
    pub(super) window_h: [u16; 2],
    /// WIN0V and WIN1V: Y1 in the high byte, Y2 in the low.
    pub(super) window_v: [u16; 2],
    pub(super) winin: u16,
    pub(super) winout: u16,
    pub(super) mosaic: u16,
    pub(super) bldcnt: u16,
    pub(super) bldalpha: u16,
    pub(super) bldy: u16,
    pub(super) master_bright: u16,
}

impl Registers {
    /// Whether these, read from a save state, hold only the bits their
    /// writes keep where the drawing counts on it: the scroll offsets'
    /// nine.
    pub(super) fn in_range(&self) -> bool {
        self.hofs
            .iter()
            .chain(&self.vofs)
            .all(|&offset| offset < 0x200)
    }

    pub(super) fn new(engine: Engine) -> Self {
        Self {
            dispcnt_mask: match engine {
                Engine::A => u32::MAX,
                Engine::B => ENGINE_B_DISPCNT,
            },
            dispcnt: 0,
            bgcnt: [0; 4],
            hofs: [0; 4],
            vofs: [0; 4],
            affine: Default::default(),
            window_h: [0; 2],
            window_v: [0; 2],
            winin: 0,
            winout: 0,
            mosaic: 0,
            bldcnt: 0,
            bldalpha: 0,
            bldy: 0,
            master_bright: 0,
        }
    }

    /// The halfword at `offset` (halfword-aligned).
    pub(super) fn read(&self, offset: u32) -> u16 {
        match offset {
            0x00 => self.dispcnt as u16,
            0x02 => (self.dispcnt >> 16) as u16,
            0x08..=0x0E => self.bgcnt[(offset as usize - 0x08) / 2],
            0x48 => self.winin,
            0x4A => self.winout,
            0x50 => self.bldcnt,
            0x52 => self.bldalpha,
            0x6C => self.master_bright,
            _ => 0,
        }
    }

    /// The halfword at `offset` as the register holds it, whether or not
    /// the CPU can read it back: what a byte written beside it leaves.
    fn held(&self, offset: u32) -> u16 {
        let affine = |bg: &AffineBg, offset: u32| match offset {
            0x0 => bg.pa as u16,
            0x2 => bg.pb as u16,
            0x4 => bg.pc as u16,
            0x6 => bg.pd as u16,
            0x8 => bg.x as u16,
            0xA => (bg.x >> 16) as u16,
            0xC => bg.y as u16,
            _ => (bg.y >> 16) as u16,
        };
        match offset {
            0x10..=0x1E => {
                let index = (offset as usize - 0x10) / 4;
                if offset & 2 == 0 {
                    self.hofs[index]
                } else {
                    self.vofs[index]
                }
            }
            0x20..=0x2E => affine(&self.affine[0], offset - 0x20),
            0x30..=0x3E => affine(&self.affine[1], offset - 0x30),
            0x40 | 0x42 => self.window_h[(offset as usize - 0x40) / 2],
            0x44 | 0x46 => self.window_v[(offset as usize - 0x44) / 2],
            0x4C => self.mosaic,
            0x54 => self.bldy,
            _ => self.read(offset),
        }
    }

    /// Writes the bytes `mask` covers (0x00FF, 0xFF00 or both) of the
    /// halfword at `offset` (halfword-aligned).
    pub(super) fn write_bytes(&mut self, offset: u32, value: u16, mask: u16) {
        let merged = if mask == 0xFFFF {
            value
        } else {
            (self.held(offset) & !mask) | (value & mask)
        };
        self.write(offset, merged);
    }

    /// Writes the halfword at `offset` (halfword-aligned).
    pub(super) fn write(&mut self, offset: u32, value: u16) {
        let wide = u32::from(value);
        match offset {
            0x00 => self.dispcnt = (self.dispcnt & 0xFFFF_0000 | wide) & self.dispcnt_mask,
            0x02 => self.dispcnt = (self.dispcnt & 0xFFFF | wide << 16) & self.dispcnt_mask,
            0x08..=0x0E => self.bgcnt[(offset as usize - 0x08) / 2] = value,
            0x10..=0x1E => {
                let index = (offset as usize - 0x10) / 4;
                let scroll = if offset & 2 == 0 {
                    &mut self.hofs
                } else {
                    &mut self.vofs
                };
                scroll[index] = value & 0x1FF;
            }
            0x20..=0x2E => self.affine[0].write(offset - 0x20, value),
            0x30..=0x3E => self.affine[1].write(offset - 0x30, value),
            0x40 | 0x42 => self.window_h[(offset as usize - 0x40) / 2] = value,
            0x44 | 0x46 => self.window_v[(offset as usize - 0x44) / 2] = value,
            0x48 => self.winin = value & 0x3F3F,
            0x4A => self.winout = value & 0x3F3F,
            0x4C => self.mosaic = value,
            0x50 => self.bldcnt = value & 0x3FFF,
            0x52 => self.bldalpha = value & 0x1F1F,
            0x54 => self.bldy = value & 0x1F,
            0x6C => self.master_bright = value & 0xC01F,
            _ => {}
        }
    }

    /// Reloads both internal reference points, as at a frame's start.
    pub(super) fn reload_affine(&mut self) {
        self.affine.iter_mut().for_each(AffineBg::reload);
    }

    /// Steps both internal reference points by (PB, PD), after a line.
    pub(super) fn step_affine(&mut self) {
        self.affine.iter_mut().for_each(AffineBg::step);
    }

    /// Background mosaic block size (width, height) in pixels.
    pub(super) fn bg_mosaic(&self) -> (u32, u32) {
        let size = |shift: u16| u32::from((self.mosaic >> shift) & 0xF) + 1;
        (size(0), size(4))
    }

    /// Sprite mosaic block size (width, height) in pixels.
    pub(super) fn obj_mosaic(&self) -> (u32, u32) {
        let size = |shift: u16| u32::from((self.mosaic >> shift) & 0xF) + 1;
        (size(8), size(12))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_only_registers_read_as_zero() {
        let mut registers = Registers::new(Engine::A);
        registers.write(0x10, 0x123);
        registers.write(0x4C, 0x1111);
        registers.write(0x54, 0x10);
        assert_eq!(registers.read(0x10), 0);
        assert_eq!(registers.read(0x4C), 0);
        assert_eq!(registers.read(0x54), 0);
        assert_eq!(registers.hofs[0], 0x123);
    }

    #[test]
    fn dispcnt_halves_combine() {
        let mut registers = Registers::new(Engine::A);
        registers.write(0x00, 0x1234);
        registers.write(0x02, 0xC001);
        assert_eq!(registers.dispcnt, 0xC001_1234);
        assert_eq!(registers.read(0x02), 0xC001);
    }

    #[test]
    fn engine_b_drops_engine_a_only_dispcnt_bits() {
        let mut registers = Registers::new(Engine::B);
        registers.write(0x00, 0xFFFF);
        registers.write(0x02, 0xFFFF);
        assert_eq!(registers.dispcnt & (1 << 3), 0, "no 3D");
        assert_eq!((registers.dispcnt >> 16) & 3, 1, "display modes 0–1 only");
        assert_eq!(registers.dispcnt & (0x3F << 24), 0, "no base offsets");
        assert_ne!(registers.dispcnt & (1 << 31), 0);
    }

    #[test]
    fn reference_point_reloads_on_write_and_steps_per_line() {
        let mut registers = Registers::new(Engine::A);
        registers.write(0x22, 0x0010); // BG2PB
        registers.write(0x26, 0x0100); // BG2PD
        registers.write(0x28, 0x0000); // BG2X low
        registers.write(0x2A, 0x0800); // BG2X high: bit 27, negative
        registers.write(0x2C, 0x0200); // BG2Y = 2.0
        assert_eq!(registers.affine[0].reference, (-0x800_0000, 0x200));
        registers.step_affine();
        registers.step_affine();
        assert_eq!(registers.affine[0].reference, (-0x800_0000 + 0x20, 0x400));
        registers.reload_affine();
        assert_eq!(registers.affine[0].reference, (-0x800_0000, 0x200));
    }

    #[test]
    fn mosaic_sizes_are_stored_minus_one() {
        let mut registers = Registers::new(Engine::A);
        registers.write(0x4C, 0x3210);
        assert_eq!(registers.bg_mosaic(), (1, 2));
        assert_eq!(registers.obj_mosaic(), (3, 4));
    }
}
