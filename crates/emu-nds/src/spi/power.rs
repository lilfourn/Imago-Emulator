//! The power management chip of a DS Lite (GBATEK, "DS Power Management
//! Device").
//!
//! The first byte after chip select is an index: bits 0-6 pick a register,
//! bit 7 reads it. The next byte is the register's value, read or written.
//! Registers 5-7 mirror 4, and 8 up mirror 0-7.

use serde::{Deserialize, Serialize};

const READ: u8 = 0x80;

const CONTROL: usize = 0;
/// Register 0 bit 6 shuts the system down.
const SHUT_DOWN: u8 = 0x40;
const BACKLIGHT: usize = 4;
/// Register 4 bits 2 and 3: maximum brightness is forced while external
/// power is present, and bits 0-1 then read 3.
const FORCE_MAX: u8 = 0x0C;

/// Writable bits of registers 0-4: control, battery status (read-only),
/// microphone amplifier, microphone gain, backlight level.
const WRITABLE: [u8; 5] = [0x7F, 0x00, 0x01, 0x03, 0x07];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Power {
    registers: [u8; 5],
    /// The index byte since chip select, if one arrived.
    index: Option<u8>,
}

impl Default for Power {
    /// The state the firmware leaves: sound amplifier and both backlights
    /// on, battery good, microphone off, brightest backlight on battery.
    fn default() -> Self {
        Self {
            registers: [0x0D, 0x00, 0x00, 0x00, 0x43],
            index: None,
        }
    }
}

impl Power {
    /// Whether the game asked the system to shut down.
    pub(crate) fn powered_off(&self) -> bool {
        self.registers[CONTROL] & SHUT_DOWN != 0
    }

    /// Shifts one byte in and returns the byte shifted out.
    pub(crate) fn transfer(&mut self, input: u8) -> u8 {
        let Some(index) = self.index else {
            self.index = Some(input);
            return 0;
        };
        let register = match usize::from(index & 7) {
            low @ 0..=3 => low,
            _ => BACKLIGHT,
        };
        if index & READ != 0 {
            return self.read(register);
        }
        let writable = WRITABLE[register];
        self.registers[register] = (self.registers[register] & !writable) | (input & writable);
        0
    }

    /// Chip select goes high.
    pub(crate) fn release(&mut self) {
        self.index = None;
    }

    fn read(&self, register: usize) -> u8 {
        let value = self.registers[register];
        if register == BACKLIGHT && value & FORCE_MAX == FORCE_MAX {
            value | 3
        } else {
            value
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(power: &mut Power, index: u8, value: u8) -> u8 {
        power.transfer(index);
        let out = power.transfer(value);
        power.release();
        out
    }

    #[test]
    fn reads_and_writes_registers_with_mirrors() {
        let mut power = Power::default();
        assert_eq!(command(&mut power, READ, 0), 0x0D);
        command(&mut power, 3, 0xFF);
        assert_eq!(command(&mut power, READ | 3, 0), 0x03);
        assert_eq!(command(&mut power, READ | 0x0B, 0), 0x03);
        assert_eq!(command(&mut power, READ | 6, 0), 0x43);
        command(&mut power, 1, 0xFF);
        assert_eq!(command(&mut power, READ | 1, 0), 0);
    }
}
