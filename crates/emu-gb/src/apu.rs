//! The audio processing unit's registers (Pan Docs, "Audio Registers").
//!
//! For now this only stores registers and reads them back with the
//! hardware's unused bits set; sound generation arrives in milestone M3.

use serde::{Deserialize, Serialize};

/// Bits that always read as 1, for 0xFF10–0xFF2F.
const READ_MASKS: [u8; 0x20] = [
    0x80, 0x3F, 0x00, 0xFF, 0xBF, // NR10–NR14
    0xFF, 0x3F, 0x00, 0xFF, 0xBF, // unused, NR21–NR24
    0x7F, 0xFF, 0x9F, 0xFF, 0xBF, // NR30–NR34
    0xFF, 0xFF, 0x00, 0x00, 0xBF, // unused, NR41–NR44
    0x00, 0x00, 0x70, // NR50–NR52
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // unused
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Apu {
    registers: Box<[u8]>,
    wave_ram: Box<[u8]>,
}

impl Apu {
    pub(crate) fn after_boot() -> Self {
        let mut registers = vec![0; 0x20];
        registers[0x16] = 0xF1; // NR52: on, channel 1 active
        registers[0x14] = 0x77; // NR50
        registers[0x15] = 0xF3; // NR51
        Self {
            registers: registers.into_boxed_slice(),
            wave_ram: vec![0; 0x10].into_boxed_slice(),
        }
    }

    pub(crate) fn read(&self, address: u16) -> u8 {
        match address {
            0xFF10..=0xFF2F => {
                let index = usize::from(address - 0xFF10);
                self.registers[index] | READ_MASKS[index]
            }
            _ => self.wave_ram[usize::from(address - 0xFF30)],
        }
    }

    pub(crate) fn write(&mut self, address: u16, value: u8) {
        match address {
            0xFF26 => self.registers[0x16] = value & 0x80,
            0xFF10..=0xFF2F => self.registers[usize::from(address - 0xFF10)] = value,
            _ => self.wave_ram[usize::from(address - 0xFF30)] = value,
        }
    }
}
