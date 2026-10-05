//! A serial flash chip of the ST M45PE family (GBATEK, "DS Firmware Serial
//! Flash Memory"). The firmware lives on one; many cartridges keep their
//! saves on another, behind the cartridge's own SPI bus ("DS Cartridge
//! Backup").
//!
//! After chip select the first byte is an instruction, then come a 3-byte
//! address (most significant byte first) and the data. Programs and erases
//! need the write-enable latch, which they clear when the chip is released.
//! They finish at once, so the status register never reports busy.

use serde::{Deserialize, Serialize};

const WRITE_ENABLE: u8 = 0x06;
const WRITE_DISABLE: u8 = 0x04;
const READ_ID: u8 = 0x9F;
const READ_STATUS: u8 = 0x05;
const READ: u8 = 0x03;
const FAST_READ: u8 = 0x0B;
const PAGE_WRITE: u8 = 0x0A;
const PAGE_PROGRAM: u8 = 0x02;
const PAGE_ERASE: u8 = 0xDB;
const SECTOR_ERASE: u8 = 0xD8;
const DEEP_POWER_DOWN: u8 = 0xB9;
const RELEASE_POWER_DOWN: u8 = 0xAB;
/// Stands in for instructions sent while asleep, which the chip ignores.
const IGNORED: u8 = 0x00;

const PAGE: u32 = 0x100;
const SECTOR: u32 = 0x1_0000;

/// What the data line reads while the chip is not driving it.
const IDLE: u8 = 0xFF;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Flash {
    /// A power-of-two size, so addresses wrap with a mask.
    memory: Vec<u8>,
    /// JEDEC manufacturer, memory type and capacity.
    id: [u8; 3],
    write_enabled: bool,
    asleep: bool,
    /// The instruction since chip select, if one arrived.
    command: Option<u8>,
    /// Bytes received after the instruction.
    count: u32,
    address: u32,
    /// A program or erase changed the memory since the last check.
    changed: bool,
}

impl Flash {
    /// A chip holding `memory`, padded with erased bytes to a power of two.
    pub(crate) fn new(mut memory: Vec<u8>, id: [u8; 3]) -> Self {
        memory.resize(memory.len().next_power_of_two(), 0xFF);
        Self {
            memory,
            id,
            write_enabled: false,
            asleep: false,
            command: None,
            count: 0,
            address: 0,
            changed: false,
        }
    }

    pub(crate) fn memory(&self) -> &[u8] {
        &self.memory
    }

    /// Whether a program or erase changed the memory since the last call.
    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Shifts one byte in and returns the byte shifted out.
    pub(crate) fn transfer(&mut self, input: u8) -> u8 {
        let Some(command) = self.command else {
            self.start(input);
            return IDLE;
        };
        self.count += 1;
        if self.count <= 3 && matches!(command, READ | FAST_READ | PAGE_WRITE | PAGE_PROGRAM) {
            self.address = self.address << 8 | u32::from(input);
            return IDLE;
        }
        match command {
            READ_ID => self
                .id
                .get(self.count as usize - 1)
                .copied()
                .unwrap_or(IDLE),
            READ_STATUS => u8::from(self.write_enabled) << 1,
            READ => self.read_next(),
            // A dummy byte follows the address.
            FAST_READ if self.count == 4 => IDLE,
            FAST_READ => self.read_next(),
            PAGE_WRITE => {
                self.write_next(input);
                IDLE
            }
            PAGE_PROGRAM => {
                // Programming can only clear bits.
                let index = self.index();
                self.write_next(self.memory[index] & input);
                IDLE
            }
            PAGE_ERASE | SECTOR_ERASE if self.count <= 3 => {
                self.address = self.address << 8 | u32::from(input);
                if self.count == 3 {
                    let size = if command == PAGE_ERASE { PAGE } else { SECTOR };
                    self.erase(size);
                }
                IDLE
            }
            _ => IDLE,
        }
    }

    /// Chip select goes high: the instruction ends.
    pub(crate) fn release(&mut self) {
        if matches!(
            self.command,
            Some(PAGE_WRITE | PAGE_PROGRAM | PAGE_ERASE | SECTOR_ERASE)
        ) {
            self.write_enabled = false;
        }
        self.command = None;
        self.count = 0;
        self.address = 0;
    }

    fn start(&mut self, command: u8) {
        if self.asleep && command != RELEASE_POWER_DOWN {
            self.command = Some(IGNORED);
            return;
        }
        match command {
            WRITE_ENABLE => self.write_enabled = true,
            WRITE_DISABLE => self.write_enabled = false,
            DEEP_POWER_DOWN => self.asleep = true,
            RELEASE_POWER_DOWN => self.asleep = false,
            _ => {}
        }
        self.command = Some(command);
    }

    fn index(&self) -> usize {
        self.address as usize & (self.memory.len() - 1)
    }

    fn read_next(&mut self) -> u8 {
        let value = self.memory[self.index()];
        self.address = self.address.wrapping_add(1);
        value
    }

    /// Stores `value` and moves on within the page, which a write never
    /// leaves.
    fn write_next(&mut self, value: u8) {
        if self.write_enabled {
            let index = self.index();
            self.changed |= self.memory[index] != value;
            self.memory[index] = value;
        }
        self.address = (self.address & !(PAGE - 1)) | (self.address.wrapping_add(1) & (PAGE - 1));
    }

    fn erase(&mut self, size: u32) {
        if !self.write_enabled {
            return;
        }
        let start = self.index() & !(size as usize - 1);
        let end = (start + size as usize).min(self.memory.len());
        let block = &mut self.memory[start..end];
        self.changed |= block.iter().any(|&byte| byte != 0xFF);
        block.fill(0xFF);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Runs one instruction: selects, shifts `bytes` through and releases.
    /// Returns the bytes shifted out.
    pub(crate) fn command(flash: &mut Flash, bytes: &[u8]) -> Vec<u8> {
        let out = bytes.iter().map(|&byte| flash.transfer(byte)).collect();
        flash.release();
        out
    }

    fn read(flash: &mut Flash, address: u32, length: usize) -> Vec<u8> {
        let mut bytes = vec![
            READ,
            (address >> 16) as u8,
            (address >> 8) as u8,
            address as u8,
        ];
        bytes.resize(4 + length, 0);
        command(flash, &bytes)[4..].to_vec()
    }

    #[test]
    fn reports_its_id_and_status() {
        let mut flash = Flash::new(vec![0xFF; 0x4_0000], [0x20, 0x40, 0x12]);
        assert_eq!(
            command(&mut flash, &[READ_ID, 0, 0, 0]),
            [IDLE, 0x20, 0x40, 0x12]
        );
        command(&mut flash, &[WRITE_ENABLE]);
        assert_eq!(command(&mut flash, &[READ_STATUS, 0, 0]), [IDLE, 2, 2]);
    }

    #[test]
    fn writes_need_the_enable_latch() {
        let mut flash = Flash::new(vec![0xFF; 0x4_0000], [0x20, 0x40, 0x12]);
        command(&mut flash, &[PAGE_WRITE, 0, 0x01, 0x00, 0x12]);
        assert_eq!(read(&mut flash, 0x100, 1), [0xFF]);
        assert!(!flash.take_changed());
        command(&mut flash, &[WRITE_ENABLE]);
        command(&mut flash, &[PAGE_WRITE, 0, 0x01, 0x00, 0x12, 0x34]);
        assert_eq!(read(&mut flash, 0x100, 2), [0x12, 0x34]);
        assert!(flash.take_changed());
        // The latch cleared when the write ended.
        command(&mut flash, &[PAGE_WRITE, 0, 0x01, 0x00, 0x00]);
        assert_eq!(read(&mut flash, 0x100, 1), [0x12]);
    }

    #[test]
    fn program_clears_bits_and_erase_sets_them() {
        let mut flash = Flash::new(vec![0xFF; 0x4_0000], [0x20, 0x40, 0x12]);
        command(&mut flash, &[WRITE_ENABLE]);
        command(&mut flash, &[PAGE_PROGRAM, 0x01, 0x02, 0xFF, 0xF0, 0x3C]);
        // The second byte wrapped to the start of the page.
        assert_eq!(read(&mut flash, 0x1_02FF, 1), [0xF0]);
        assert_eq!(read(&mut flash, 0x1_0200, 1), [0x3C]);
        command(&mut flash, &[WRITE_ENABLE]);
        command(&mut flash, &[PAGE_PROGRAM, 0x01, 0x02, 0xFF, 0x0F]);
        assert_eq!(read(&mut flash, 0x1_02FF, 1), [0x00]);
        command(&mut flash, &[WRITE_ENABLE]);
        command(&mut flash, &[PAGE_ERASE, 0x01, 0x02, 0x80]);
        assert_eq!(read(&mut flash, 0x1_0200, 0x100), [0xFF; 0x100]);
        command(&mut flash, &[WRITE_ENABLE]);
        command(&mut flash, &[PAGE_WRITE, 0x01, 0x80, 0x00, 0x55]);
        command(&mut flash, &[WRITE_ENABLE]);
        command(&mut flash, &[SECTOR_ERASE, 0x01, 0x00, 0x00]);
        assert_eq!(read(&mut flash, 0x1_8000, 1), [0xFF]);
    }

    #[test]
    fn fast_read_skips_a_dummy_byte_and_sleep_ignores_commands() {
        let mut memory = vec![0xFF; 0x4_0000];
        memory[0x3_FFFF] = 0xAB;
        memory[0] = 0xCD;
        let mut flash = Flash::new(memory, [0x20, 0x40, 0x12]);
        assert_eq!(
            command(&mut flash, &[FAST_READ, 0x03, 0xFF, 0xFF, 0, 0, 0]),
            [IDLE, IDLE, IDLE, IDLE, IDLE, 0xAB, 0xCD]
        );
        command(&mut flash, &[DEEP_POWER_DOWN]);
        assert_eq!(command(&mut flash, &[READ_ID, 0]), [IDLE, IDLE]);
        command(&mut flash, &[RELEASE_POWER_DOWN]);
        assert_eq!(command(&mut flash, &[READ_ID, 0]), [IDLE, 0x20]);
    }
}
