//! The baseband chip, a Mitsumi MM3155 on the DS or the MM3218 on the DS
//! Lite (GBATEK, "DS Wifi Baseband Chip (BB)").
//!
//! The MAC reaches its 105 registers over a serial link: W_BB_WRITE holds
//! a byte to send, writing W_BB_CNT names the register and the direction
//! and starts the exchange, and W_BB_READ holds what came back. The chip
//! sets gains and carrier-sense thresholds for the radio; there is no
//! radio to tune here, so its registers only remember their values, and
//! the exchange finishes at once (W_BB_BUSY always reads 0).

use serde::{Deserialize, Serialize};

/// The registers that read back a fixed value whatever is written, and
/// that value (GBATEK's table for the MM3155; the MM3218 reads the same).
const FIXED: [(u8, u8); 21] = [
    (0x00, 0x6D), // chip ID
    (0x0D, 0),
    (0x0E, 0),
    (0x0F, 0),
    (0x10, 0),
    (0x11, 0),
    (0x12, 0),
    (0x16, 0),
    (0x17, 0),
    (0x18, 0),
    (0x19, 0),
    (0x1A, 0),
    (0x27, 0),
    (0x4D, 0),
    (0x5D, 0x01),
    (0x5E, 0),
    (0x5F, 0),
    (0x60, 0),
    (0x61, 0),
    (0x64, 0xFF),
    (0x66, 0),
];

/// Registers 0x00-0x68 exist; the rest read 0.
const REGISTERS: usize = 0x69;

/// W_BB_CNT bits 12-15.
const WRITE: u16 = 5;
const READ: u16 = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Baseband {
    registers: Vec<u8>,
    /// W_BB_READ: the byte the last read brought back.
    read: u8,
}

impl Baseband {
    /// The chip as the firmware's boot code leaves it: registers 0x00-0x68
    /// from `initial` (the firmware's wifi settings at 0x64), except those
    /// that never change.
    pub(super) fn new(initial: &[u8]) -> Self {
        let mut registers = vec![0; REGISTERS];
        for (register, value) in registers.iter_mut().zip(initial) {
            *register = *value;
        }
        for (index, value) in FIXED {
            registers[usize::from(index)] = value;
        }
        Self { registers, read: 0 }
    }

    /// A write to W_BB_CNT: an exchange with register `control & 0xFF`,
    /// sending `data` (W_BB_WRITE) or reading into W_BB_READ.
    pub(super) fn transfer(&mut self, control: u16, data: u16) {
        let index = usize::from(control & 0xFF);
        let fixed = FIXED
            .iter()
            .any(|&(register, _)| usize::from(register) == index);
        match control >> 12 {
            WRITE if index < REGISTERS && !fixed => self.registers[index] = data as u8,
            READ => self.read = self.registers.get(index).copied().unwrap_or(0),
            _ => {}
        }
    }

    /// W_BB_READ.
    pub(super) fn read(&self) -> u16 {
        u16::from(self.read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_take_writes_except_the_fixed_ones() {
        let mut baseband = Baseband::new(&[0x03, 0x9E]);
        baseband.transfer(0x6001, 0);
        assert_eq!(baseband.read(), 0x9E, "from the firmware");
        baseband.transfer(0x6000, 0);
        assert_eq!(baseband.read(), 0x6D, "the chip ID, not the firmware's 03h");
        baseband.transfer(0x5013, 0x02);
        baseband.transfer(0x5000, 0x55);
        baseband.transfer(0x6013, 0);
        assert_eq!(baseband.read(), 0x02);
        baseband.transfer(0x6000, 0);
        assert_eq!(baseband.read(), 0x6D);
        baseband.transfer(0x5070, 0x12);
        baseband.transfer(0x6070, 0);
        assert_eq!(baseband.read(), 0, "past the last register");
    }
}
