//! Memory for tests: sparse RAM at 0x02000000–0x0203FFFF, a KEYINPUT
//! register at 0x04000130, and Game Boy-style RAM banks.

use std::collections::HashMap;

use crate::Memory;

/// The test memory.
pub(crate) struct TestMemory {
    ram: HashMap<u32, u8>,
    /// KEYINPUT: a clear bit is a held button.
    pub(crate) keys: u16,
    /// Bytes written with [`Memory::poke_bank`], by bank and address.
    pub(crate) banks: HashMap<(u8, u32), u8>,
}

impl Default for TestMemory {
    fn default() -> Self {
        Self {
            ram: HashMap::new(),
            keys: 0x03FF,
            banks: HashMap::new(),
        }
    }
}

const RAM: std::ops::Range<u32> = 0x0200_0000..0x0204_0000;

impl TestMemory {
    /// The little-endian word at `address`.
    pub(crate) fn word(&self, address: u32) -> u32 {
        u32::from_le_bytes([0, 1, 2, 3].map(|byte| self.peek(address + byte).unwrap_or(0)))
    }

    /// The little-endian halfword at `address`.
    pub(crate) fn half(&self, address: u32) -> u16 {
        u16::from_le_bytes([0, 1].map(|byte| self.peek(address + byte).unwrap_or(0)))
    }

    /// Stores a little-endian word at `address`.
    pub(crate) fn set_word(&mut self, address: u32, value: u32) {
        for (offset, byte) in (0..).zip(value.to_le_bytes()) {
            self.poke(address + offset, byte);
        }
    }
}

impl Memory for TestMemory {
    fn peek(&self, address: u32) -> Option<u8> {
        match address {
            _ if RAM.contains(&address) => Some(self.ram.get(&address).copied().unwrap_or(0)),
            0x0400_0130 => Some(self.keys as u8),
            0x0400_0131 => Some((self.keys >> 8) as u8),
            _ => None,
        }
    }

    fn poke(&mut self, address: u32, value: u8) -> bool {
        let ram = RAM.contains(&address);
        if ram {
            self.ram.insert(address, value);
        }
        ram
    }

    fn poke_bank(&mut self, bank: u8, address: u32, value: u8) -> bool {
        self.banks.insert((bank, address), value);
        true
    }
}
