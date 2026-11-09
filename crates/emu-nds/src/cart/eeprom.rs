//! Serial EEPROM and FRAM save chips (GBATEK, "DS Cartridge Backup").
//!
//! After chip select the first byte is an instruction. Reads and writes
//! take an address of one byte (512 B parts, whose instruction bit 3 is
//! address bit 8), two bytes (8 KiB to 64 KiB, and FRAM) or three
//! (128 KiB), most significant first. Writes need the write-enable latch,
//! which they clear when the chip is released, and finish at once.
//!
//! Real EEPROMs wrap a write at the end of a 16- to 256-byte page. FRAM has
//! no pages, and the two cannot be told apart by save size, so writes here
//! run on linearly; a game written for EEPROM never crosses a page.

use emu_state::payload;
use serde::{Deserialize, Serialize};

const WRITE_ENABLE: u8 = 0x06;
const WRITE_DISABLE: u8 = 0x04;
const READ_STATUS: u8 = 0x05;
const WRITE_STATUS: u8 = 0x01;
const READ: u8 = 0x03;
const WRITE: u8 = 0x02;
/// On 512 B parts, instruction bit 3 selects the upper 256 bytes.
const HIGH_HALF: u8 = 0x08;

/// Status register bits WRSR may change: write protection (bits 2-3) and,
/// on larger parts, the status write lock (bit 7).
const PROTECT_SMALL: u8 = 0x0C;
const PROTECT_LARGE: u8 = 0x8C;

/// What the data line reads while the chip is not driving it.
const IDLE: u8 = 0xFF;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Eeprom {
    /// A power-of-two size, so addresses wrap with a mask.
    #[serde(with = "payload::bytes")]
    memory: Vec<u8>,
    address_bytes: u32,
    write_enabled: bool,
    /// Status register bits 2-3 and 7.
    protect: u8,
    /// The instruction since chip select, if one arrived.
    command: Option<u8>,
    /// Bytes received after the instruction.
    count: u32,
    address: u32,
    /// A write changed the memory since the last check.
    changed: bool,
}

impl Eeprom {
    /// A blank chip of `size` bytes: 512, 8 KiB, 32 KiB, 64 KiB or 128 KiB.
    pub(crate) fn new(size: usize) -> Self {
        let address_bytes = match size {
            0..=0x200 => 1,
            0x201..=0x1_0000 => 2,
            _ => 3,
        };
        Self {
            memory: vec![0xFF; size.next_power_of_two()],
            address_bytes,
            write_enabled: false,
            protect: 0,
            command: None,
            count: 0,
            address: 0,
            changed: false,
        }
    }

    pub(crate) fn memory(&self) -> &[u8] {
        &self.memory
    }

    /// Whether this chip, read from a save state, has memory of a size
    /// its addresses can wrap in.
    pub(crate) fn in_range(&self) -> bool {
        self.memory.len().is_power_of_two()
    }

    /// Copies a `.sav` file's bytes in.
    pub(crate) fn load(&mut self, data: &[u8]) {
        let length = data.len().min(self.memory.len());
        self.memory[..length].copy_from_slice(&data[..length]);
    }

    /// Whether a write changed the memory since the last call.
    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    fn small(&self) -> bool {
        self.address_bytes == 1
    }

    /// The instruction with the 512 B parts' address bit removed.
    fn base(&self, command: u8) -> u8 {
        if self.small() && matches!(command & !HIGH_HALF, READ | WRITE) {
            command & !HIGH_HALF
        } else {
            command
        }
    }

    /// Shifts one byte in and returns the byte shifted out.
    pub(crate) fn transfer(&mut self, input: u8) -> u8 {
        let Some(command) = self.command else {
            self.start(input);
            return IDLE;
        };
        self.count += 1;
        let base = self.base(command);
        if self.count <= self.address_bytes && matches!(base, READ | WRITE) {
            self.address = self.address << 8 | u32::from(input);
            return IDLE;
        }
        match base {
            READ_STATUS => self.status(),
            WRITE_STATUS if self.count == 1 && self.write_enabled => {
                let writable = if self.small() {
                    PROTECT_SMALL
                } else {
                    PROTECT_LARGE
                };
                self.protect = input & writable;
                IDLE
            }
            READ => {
                let value = self.memory[self.index()];
                self.address = self.address.wrapping_add(1);
                value
            }
            WRITE => {
                if self.write_enabled && !self.protected() {
                    let index = self.index();
                    self.changed |= self.memory[index] != input;
                    self.memory[index] = input;
                }
                self.address = self.address.wrapping_add(1);
                IDLE
            }
            _ => IDLE,
        }
    }

    /// Chip select goes high: the instruction ends.
    pub(crate) fn release(&mut self) {
        if let Some(command) = self.command
            && matches!(self.base(command), WRITE | WRITE_STATUS)
        {
            self.write_enabled = false;
        }
        self.command = None;
        self.count = 0;
        self.address = 0;
    }

    fn start(&mut self, command: u8) {
        match command {
            WRITE_ENABLE => self.write_enabled = true,
            WRITE_DISABLE => self.write_enabled = false,
            _ => {}
        }
        // Address bit 8 of a 512 B part comes from the instruction; the
        // address byte shifts in below it.
        let high_half = self.base(command) != command;
        self.address = u32::from(high_half);
        self.command = Some(command);
    }

    fn status(&self) -> u8 {
        // 512 B parts read their unused bits 4-7 as ones.
        let unused = if self.small() { 0xF0 } else { 0 };
        unused | self.protect | u8::from(self.write_enabled) << 1
    }

    fn index(&self) -> usize {
        self.address as usize & (self.memory.len() - 1)
    }

    /// Whether status bits 2-3 protect the current address: none, the top
    /// quarter, the top half, or everything.
    fn protected(&self) -> bool {
        let size = self.memory.len();
        let index = self.index();
        match (self.protect >> 2) & 3 {
            0 => false,
            1 => index >= size - size / 4,
            2 => index >= size / 2,
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(eeprom: &mut Eeprom, bytes: &[u8]) -> Vec<u8> {
        let out = bytes.iter().map(|&byte| eeprom.transfer(byte)).collect();
        eeprom.release();
        out
    }

    #[test]
    fn small_parts_take_address_bit_8_from_the_instruction() {
        let mut eeprom = Eeprom::new(0x200);
        assert_eq!(command(&mut eeprom, &[READ_STATUS, 0]), [IDLE, 0xF0]);
        command(&mut eeprom, &[WRITE_ENABLE]);
        assert_eq!(command(&mut eeprom, &[READ_STATUS, 0]), [IDLE, 0xF2]);
        command(&mut eeprom, &[WRITE | HIGH_HALF, 0x10, 0xAA, 0xBB]);
        command(&mut eeprom, &[WRITE_ENABLE]);
        command(&mut eeprom, &[WRITE, 0x10, 0xCC]);
        assert!(eeprom.take_changed());
        assert_eq!(eeprom.memory()[0x110..0x112], [0xAA, 0xBB]);
        assert_eq!(
            command(&mut eeprom, &[READ | HIGH_HALF, 0x10, 0, 0]),
            [IDLE, IDLE, 0xAA, 0xBB]
        );
        assert_eq!(command(&mut eeprom, &[READ, 0x10, 0]), [IDLE, IDLE, 0xCC]);
    }

    #[test]
    fn large_parts_take_two_or_three_address_bytes() {
        let mut eeprom = Eeprom::new(0x1_0000);
        command(&mut eeprom, &[WRITE_ENABLE]);
        command(&mut eeprom, &[WRITE, 0xBE, 0xEF, 0x12, 0x34]);
        assert_eq!(
            command(&mut eeprom, &[READ, 0xBE, 0xEF, 0, 0]),
            [IDLE, IDLE, IDLE, 0x12, 0x34]
        );
        assert_eq!(command(&mut eeprom, &[READ_STATUS, 0]), [IDLE, 0]);
        // Without the latch, writes do nothing.
        command(&mut eeprom, &[WRITE, 0xBE, 0xEF, 0x00]);
        assert_eq!(eeprom.memory()[0xBEEF], 0x12);

        let mut eeprom = Eeprom::new(0x2_0000);
        command(&mut eeprom, &[WRITE_ENABLE]);
        command(&mut eeprom, &[WRITE, 0x01, 0x00, 0x02, 0x77]);
        assert_eq!(eeprom.memory()[0x1_0002], 0x77);
    }

    #[test]
    fn status_bits_protect_the_upper_half() {
        let mut eeprom = Eeprom::new(0x2000);
        command(&mut eeprom, &[WRITE_ENABLE]);
        command(&mut eeprom, &[WRITE_STATUS, 0x08]);
        assert_eq!(command(&mut eeprom, &[READ_STATUS, 0]), [IDLE, 0x08]);
        command(&mut eeprom, &[WRITE_ENABLE]);
        command(&mut eeprom, &[WRITE, 0x10, 0x00, 0x01]);
        command(&mut eeprom, &[WRITE_ENABLE]);
        command(&mut eeprom, &[WRITE, 0x00, 0x00, 0x02]);
        assert_eq!((eeprom.memory()[0x1000], eeprom.memory()[0]), (0xFF, 0x02));
    }
}
