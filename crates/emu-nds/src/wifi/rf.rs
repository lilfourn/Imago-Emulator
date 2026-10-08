//! The radio chip: an RF9008 on the DS, or the radio half of the DS
//! Lite's Mitsumi MM3218 (GBATEK, "DS Wifi RF Chip").
//!
//! The firmware's wifi settings say which (byte 0x40: 2 or 3), and the two
//! take different serial words through W_RF_DATA1 and W_RF_DATA2:
//! - type 2: 32 registers of 18 bits; DATA2 holds the index in bits 2-6,
//!   a read flag in bit 7 and data bits 16-17 in bits 0-1, and DATA1 the
//!   low 16 data bits;
//! - type 3: 64 registers of 8 bits; DATA2 holds a command (5 writes, 6
//!   reads) and DATA1 the index above the data byte.
//!
//! Writing DATA2 runs the exchange, which finishes at once. There is no
//! radio to tune, so the registers only remember their values.

use serde::{Deserialize, Serialize};

/// The DS Lite's chip, with 8-bit registers.
const TYPE_3: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Rf {
    /// The chip type from the firmware (2 or 3).
    kind: u8,
    registers: Vec<u32>,
}

impl Rf {
    /// A chip of type `kind`, its first registers holding `initial` as the
    /// firmware's boot code leaves them. Type 3 takes one byte per
    /// register; type 2 takes three bytes per write, the index in the top
    /// bits of the 24-bit word.
    pub(super) fn new(kind: u8, initial: &[u8]) -> Self {
        let mut rf = Self {
            kind,
            registers: vec![0; 0x40],
        };
        if kind == TYPE_3 {
            for (register, value) in rf.registers.iter_mut().zip(initial) {
                *register = u32::from(*value);
            }
        } else {
            for word in initial.chunks_exact(3) {
                let word = u32::from(word[0]) | u32::from(word[1]) << 8 | u32::from(word[2]) << 16;
                rf.registers[(word >> 18) as usize & 0x1F] = word & 0x3_FFFF;
            }
        }
        rf
    }

    /// Runs the exchange a write to W_RF_DATA2 starts, and returns the new
    /// (DATA1, DATA2): reads bring data back into them.
    pub(super) fn transfer(&mut self, data1: u16, data2: u16) -> (u16, u16) {
        if self.kind == TYPE_3 {
            let index = usize::from(data1 >> 8) & 0x3F;
            match data2 & 0xF {
                5 => self.registers[index] = u32::from(data1 & 0xFF),
                6 => return ((data1 & 0xFF00) | self.registers[index] as u16, data2),
                _ => {}
            }
            return (data1, data2);
        }
        let index = usize::from(data2 >> 2) & 0x1F;
        if data2 & 0x80 != 0 {
            let value = self.registers[index];
            (value as u16, (data2 & !3) | (value >> 16) as u16 & 3)
        } else {
            self.registers[index] = u32::from(data1) | u32::from(data2 & 3) << 16;
            (data1, data2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_3_registers_are_bytes_named_in_data1() {
        let mut rf = Rf::new(3, &[0x31, 0x4C]);
        assert_eq!(rf.transfer(0x0100, 6), (0x014C, 6), "from the firmware");
        rf.transfer(0x2A5A, 5);
        assert_eq!(rf.transfer(0x2A00, 6).0, 0x2A5A);
        assert_eq!(rf.transfer(0x6A00, 6).0, 0x6A5A, "index 6Ah mirrors 2Ah");
    }

    #[test]
    fn type_2_registers_hold_18_bits() {
        // RF[5] = 01728h, written as the 24-bit word 141728h.
        let mut rf = Rf::new(2, &[0x28, 0x17, 0x14]);
        assert_eq!(rf.transfer(0, 0x80 | 5 << 2), (0x1728, 0x80 | 5 << 2));
        rf.transfer(0xFFFA, 8 << 2 | 3);
        assert_eq!(rf.transfer(0, 0x80 | 8 << 2), (0xFFFA, 0x80 | 8 << 2 | 3));
    }
}
