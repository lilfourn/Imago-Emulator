//! The Game Pak: ROM, save memory, and the GPIO port some games wire to a
//! real-time clock (GBATEK, "GBA Cartridges").
//!
//! The bus hands every access in 0x08000000-0x0FFFFFFF here. ROM fills the
//! 32 MiB window three times over (one per wait-state set); EEPROM answers
//! at the top of it; SRAM and Flash sit in their own 8-bit region.

use serde::{Deserialize, Serialize};

use crate::backup::Backup;
use crate::rtc::{self, Rtc};

/// ROM addresses (offsets from 0x08000000) of the GPIO registers.
const GPIO: std::ops::RangeInclusive<u32> = 0xC4..=0xC9;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Cartridge {
    /// Restored from the ROM file, never saved in states.
    #[serde(skip)]
    rom: Box<[u8]>,
    backup: Backup,
    rtc: Option<Rtc>,
    /// Counts writes that changed save memory.
    generation: u64,
}

impl Cartridge {
    /// A cartridge holding `rom`, with save memory and clock detected from
    /// the ROM. `backup` is a previous `.sav`; `start_time` sets the clock.
    pub(crate) fn new(rom: Box<[u8]>, backup: Option<&[u8]>, start_time: i64) -> Self {
        let mut save = Backup::detect(&rom);
        if let Some(data) = backup {
            save.load(data);
        }
        Self {
            rtc: rtc::present(&rom).then(|| Rtc::new(start_time)),
            rom,
            backup: save,
            generation: 0,
        }
    }

    /// Moves the ROM out of `other`, which is about to be replaced by this
    /// cartridge from a loaded save state. Counts the swap as a save write,
    /// since the state may hold different save memory than was last flushed.
    pub(crate) fn take_rom_from(&mut self, other: &mut Self) {
        self.rom = std::mem::take(&mut other.rom);
        self.generation = other.generation + 1;
    }

    /// The console was reset at system cycle `now` with this cartridge in.
    pub(crate) fn restart_cycles(&mut self, now: u64) {
        if let Some(rtc) = &mut self.rtc {
            rtc.restart_cycles(now);
        }
    }

    pub(crate) fn backup_generation(&self) -> Option<u64> {
        self.backup.data().map(|_| self.generation)
    }

    /// Appends the `.sav` contents: the raw save memory.
    pub(crate) fn save_backup(&self, out: &mut Vec<u8>) {
        if let Some(data) = self.backup.data() {
            out.extend_from_slice(data);
        }
    }

    /// Whether `address` (0x08000000-0x0DFFFFFF) talks to the EEPROM: all
    /// of 0x0D000000 up, or with a 32 MiB ROM only its last 256 bytes.
    pub(crate) fn eeprom_at(&self, address: u32) -> bool {
        matches!(self.backup, Backup::Eeprom(_))
            && address >> 24 == 0x0D
            && (self.rom.len() <= 0x100_0000 || address & 0x00FF_FF00 == 0x00FF_FF00)
    }

    /// A DMA of `count` units is about to write the EEPROM.
    pub(crate) fn note_eeprom_dma(&mut self, count: u32) {
        if let Backup::Eeprom(eeprom) = &mut self.backup {
            eeprom.note_dma_length(count);
        }
    }

    /// Reads the halfword at `address` in 0x08000000-0x0DFFFFFF.
    pub(crate) fn read_16(&mut self, address: u32) -> u16 {
        if self.eeprom_at(address)
            && let Backup::Eeprom(eeprom) = &mut self.backup
        {
            return eeprom.read();
        }
        let offset = address & 0x01FF_FFFE;
        if GPIO.contains(&offset)
            && let Some(value) = self.rtc.as_ref().and_then(|rtc| rtc.read(offset))
        {
            return value;
        }
        let index = offset as usize;
        match self.rom.get(index..index + 2) {
            Some(bytes) => u16::from_le_bytes([bytes[0], bytes[1]]),
            // Past the end, the cartridge bus returns the address.
            None => (address >> 1) as u16,
        }
    }

    /// Writes the halfword at `address` in 0x08000000-0x0DFFFFFF at system
    /// cycle `now`. ROM ignores it; EEPROM and the GPIO port take it.
    pub(crate) fn write_16(&mut self, address: u32, value: u16, now: u64) {
        if self.eeprom_at(address)
            && let Backup::Eeprom(eeprom) = &mut self.backup
        {
            if eeprom.write(value) {
                self.generation += 1;
            }
            return;
        }
        let offset = address & 0x01FF_FFFE;
        if GPIO.contains(&offset)
            && let Some(rtc) = &mut self.rtc
        {
            rtc.write(offset, value, now);
        }
    }

    /// Reads the byte at `address` in the SRAM/Flash region,
    /// 0x0E000000-0x0FFFFFFF.
    pub(crate) fn read_backup(&self, address: u32) -> u8 {
        self.backup.read(address & 0x00FF_FFFF)
    }

    /// Writes the byte at `address` in the SRAM/Flash region.
    pub(crate) fn write_backup(&mut self, address: u32, value: u8) {
        if self.backup.write(address & 0x00FF_FFFF, value) {
            self.generation += 1;
        }
    }
}

impl Default for Cartridge {
    fn default() -> Self {
        Self {
            rom: Box::default(),
            backup: Backup::None,
            rtc: None,
            generation: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with(id: &[u8]) -> Box<[u8]> {
        let mut rom = vec![0; 0x400];
        rom[0x200..0x200 + id.len()].copy_from_slice(id);
        rom.into_boxed_slice()
    }

    #[test]
    fn counts_save_writes_and_loads_saves() {
        let mut cartridge = Cartridge::new(rom_with(b"SRAM_V113"), Some(&[0x12]), 0);
        assert_eq!(cartridge.read_backup(0x0E00_0000), 0x12);
        assert_eq!(cartridge.backup_generation(), Some(0));
        cartridge.write_backup(0x0F00_8001, 0x34); // a mirror of 0x0E000001
        assert_eq!(cartridge.backup_generation(), Some(1));
        let mut save = Vec::new();
        cartridge.save_backup(&mut save);
        assert_eq!((save.len(), save[0], save[1]), (0x8000, 0x12, 0x34));
    }

    #[test]
    fn eeprom_sits_at_the_top_of_large_roms_only() {
        let small = Cartridge::new(rom_with(b"EEPROM_V124"), None, 0);
        assert!(small.eeprom_at(0x0D00_0000));
        assert!(!small.eeprom_at(0x0C00_0000));
        let mut big = vec![0; 0x200_0000];
        big[0x200..0x208].copy_from_slice(b"EEPROM_V");
        let big = Cartridge::new(big.into_boxed_slice(), None, 0);
        assert!(!big.eeprom_at(0x0D00_0000));
        assert!(big.eeprom_at(0x0DFF_FF00));
    }

    #[test]
    fn no_save_memory_means_no_backup() {
        let cartridge = Cartridge::new(rom_with(b""), None, 0);
        assert_eq!(cartridge.backup_generation(), None);
        assert_eq!(cartridge.read_backup(0x0E00_0000), 0xFF);
    }
}
