//! The rendering engine's per-frame registers: DISP3DCNT and
//! 0x04000330–0x040003BF (GBATEK, "DS 3D Display Control", "DS 3D Toon,
//! Edge, Fog, Alpha-Blending, Anti-Aliasing" and "DS 3D Rear-Plane").
//!
//! They are write-only bytes to the CPU; [`RenderRegisters::config`]
//! decodes them once per frame for the renderer.

use serde::{Deserialize, Serialize};

const FIRST: u32 = 0x330;
const END: u32 = 0x3C0;

/// DISP3DCNT bits the CPU can set: everything but the two status flags.
const CONTROL_MASK: u16 = 0x4FFF;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RenderRegisters {
    /// DISP3DCNT without its status bits.
    control: u16,
    /// 0x330 to 0x3BF.
    bytes: Box<[u8]>,
}

impl Default for RenderRegisters {
    fn default() -> Self {
        Self {
            control: 0,
            bytes: vec![0; (END - FIRST) as usize].into_boxed_slice(),
        }
    }
}

/// The rendering registers, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderConfig {
    pub(crate) texturing: bool,
    /// Polygon mode 2 is highlight rather than toon shading.
    pub(crate) highlight: bool,
    pub(crate) alpha_test: bool,
    pub(crate) blending: bool,
    pub(crate) antialiasing: bool,
    pub(crate) edge_marking: bool,
    /// Fog changes alpha only.
    pub(crate) fog_alpha_only: bool,
    pub(crate) fog: bool,
    pub(crate) fog_shift: u32,
    /// The rear plane comes from texture slots 2 and 3.
    pub(crate) rear_bitmap: bool,
    pub(crate) edge_colors: [u16; 8],
    pub(crate) alpha_ref: u8,
    /// CLEAR_COLOR: colour, fog bit 15, alpha 16–20, polygon ID 24–29.
    pub(crate) clear_color: u32,
    /// CLEAR_DEPTH expanded to 24 bits.
    pub(crate) clear_depth: u32,
    /// CLRIMAGE_OFFSET: x, y.
    pub(crate) clear_offset: (u32, u32),
    pub(crate) fog_color: u32,
    pub(crate) fog_offset: u32,
    pub(crate) fog_table: [u8; 32],
    pub(crate) toon: [u16; 32],
}

/// A 15-bit depth widened to 24 bits, so 0x7FFF becomes 0xFFFFFF.
pub(crate) fn expand_depth(depth: u32) -> u32 {
    let depth = depth & 0x7FFF;
    depth * 0x200 + ((depth + 1) >> 15) * 0x1FF
}

impl RenderRegisters {
    /// DISP3DCNT bits 0–11 and 14.
    pub(crate) fn control(&self) -> u16 {
        self.control
    }

    /// Writes the DISP3DCNT bits in `mask`.
    pub(crate) fn write_control(&mut self, value: u16, mask: u16) {
        let mask = mask & CONTROL_MASK;
        self.control = (self.control & !mask) | (value & mask);
    }

    /// Writes the byte at `offset` from 0x04000000, if it is one of these
    /// registers.
    pub(crate) fn write_byte(&mut self, offset: u32, value: u8) {
        if (FIRST..END).contains(&offset) {
            self.bytes[(offset - FIRST) as usize] = value;
        }
    }

    fn byte(&self, offset: u32) -> u8 {
        self.bytes[(offset - FIRST) as usize]
    }

    fn half(&self, offset: u32) -> u16 {
        u16::from_le_bytes([self.byte(offset), self.byte(offset + 1)])
    }

    fn word(&self, offset: u32) -> u32 {
        u32::from(self.half(offset)) | u32::from(self.half(offset + 2)) << 16
    }

    pub(crate) fn config(&self) -> RenderConfig {
        let control = self.control;
        let bit = |n: u32| control & (1 << n) != 0;
        let clear_offset = self.half(0x356);
        RenderConfig {
            texturing: bit(0),
            highlight: bit(1),
            alpha_test: bit(2),
            blending: bit(3),
            antialiasing: bit(4),
            edge_marking: bit(5),
            fog_alpha_only: bit(6),
            fog: bit(7),
            fog_shift: u32::from((control >> 8) & 0xF),
            rear_bitmap: bit(14),
            edge_colors: std::array::from_fn(|i| self.half(0x330 + 2 * i as u32)),
            alpha_ref: self.byte(0x340) & 0x1F,
            clear_color: self.word(0x350),
            clear_depth: expand_depth(u32::from(self.half(0x354))),
            clear_offset: (u32::from(clear_offset & 0xFF), u32::from(clear_offset >> 8)),
            fog_color: self.word(0x358),
            fog_offset: u32::from(self.half(0x35C) & 0x7FFF),
            fog_table: std::array::from_fn(|i| self.byte(0x360 + i as u32) & 0x7F),
            toon: std::array::from_fn(|i| self.half(0x380 + 2 * i as u32)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_decode_into_the_config() {
        let mut registers = RenderRegisters::default();
        for (i, byte) in 0x7FFFu32.to_le_bytes().into_iter().take(2).enumerate() {
            registers.write_byte(0x354 + i as u32, byte);
        }
        registers.write_byte(0x356, 3);
        registers.write_byte(0x357, 4);
        registers.write_byte(0x382, 0x1F);
        registers.write_control(0xFFFF, 0xFFFF);
        let config = registers.config();
        assert_eq!(config.clear_depth, 0xFF_FFFF);
        assert_eq!(config.clear_offset, (3, 4));
        assert_eq!(config.toon[1], 0x1F);
        assert_eq!(registers.control(), 0x4FFF, "status bits are not writable");
        assert!(config.rear_bitmap && config.fog && config.antialiasing);
    }

    #[test]
    fn depth_expansion_reaches_both_ends() {
        assert_eq!(expand_depth(0), 0);
        assert_eq!(expand_depth(1), 0x200);
        assert_eq!(expand_depth(0x7FFF), 0xFF_FFFF);
    }
}
