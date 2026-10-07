//! The GBA slot in DS mode (GBATEK, "DS Memory Control - Cartridges and
//! Main RAM", "DS Cartridge GBA Slot").
//!
//! A GBA cartridge's ROM shows once at 0x08000000-0x09FFFFFF on a 16-bit
//! bus, and its SRAM or Flash at 0x0A000000-0x0AFFFFFF on an 8-bit bus,
//! repeating every 64 KiB. The EEPROM some GBA cartridges save to is not
//! wired up (GBATEK has it reachable by DMA at timings too fast for it);
//! the games Pal Park reads all save to Flash.
//!
//! EXMEMCNT bit 7 gives the slot to one CPU; the other reads zeros (see
//! `memory.rs`). An empty slot reads all ones: the SRAM region is high-Z,
//! and so is the ROM region at the 18-cycle access time Nintendo's library
//! sets before probing it. The address-dependent values GBATEK lists for
//! faster settings are not modelled.
//!
//! Wide accesses to the 8-bit region behave as in GBA mode (GBATEK,
//! "Accessing SRAM Area by 16bit/32bit"): a read repeats the byte at the
//! address, a write stores the byte lane at the address. GBATEK's DS timing
//! table charges a 32-bit access there the same as an 8-bit one, which fits
//! a single byte transfer.

use std::ops::Range;

use emu_gba_cart::Cartridge;
use serde::{Deserialize, Serialize};

use crate::memory::Width;

/// Access times in bus cycles for EXMEMCNT's 2-bit SRAM and ROM settings.
const ACCESS_TIMES: [u64; 4] = [10, 8, 6, 18];

/// The header offset of each byte the firmware copies to 0x027FFC30;
/// `None` for the flags byte, which it leaves clear.
const MODULE_INFO: [Option<u32>; 12] = [
    Some(0xBE),
    Some(0xBF),
    Some(0xB5),
    Some(0xB6),
    Some(0xB7),
    None,
    Some(0xB0),
    Some(0xB1),
    Some(0xAC),
    Some(0xAD),
    Some(0xAE),
    Some(0xAF),
];

/// Where the ARM9 BIOS keeps its copy of the Nintendo logo that GBA
/// cartridge headers carry at 0x04-0x9F. Nintendo's library compares the
/// two to tell a GBA game from an expansion pak (NitroSDK,
/// `CTRDG_SYSROM9_NINLOGO_ADR`).
pub(crate) const BIOS_LOGO: Range<usize> = 0x20..0xBC;

/// The cartridge in the GBA slot, if any.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct GbaSlot {
    cartridge: Option<Cartridge>,
}

/// `bus_cycles` of the DS bus clock in cycles of the cartridge's clock,
/// which its real-time clock counts.
fn cartridge_cycles(bus_cycles: u64) -> u64 {
    let bus_hz = u128::from(crate::INFO.frame_rate.clock_hz);
    let cycles = u128::from(bus_cycles) * u128::from(emu_gba_cart::CLOCK_HZ) / bus_hz;
    u64::try_from(cycles).unwrap_or(u64::MAX)
}

impl GbaSlot {
    pub(crate) fn new(cartridge: Option<Cartridge>) -> Self {
        Self { cartridge }
    }

    pub(crate) fn is_occupied(&self) -> bool {
        self.cartridge.is_some()
    }

    /// Moves the ROM out of `other`, which is about to be replaced by this
    /// slot from a loaded save state.
    pub(crate) fn take_rom_from(&mut self, other: &mut Self) {
        if let (Some(cartridge), Some(old)) = (&mut self.cartridge, &mut other.cartridge) {
            cartridge.take_rom_from(old);
        }
    }

    /// The console was reset at bus cycle `now`.
    pub(crate) fn restart_cycles(&mut self, now: u64) {
        if let Some(cartridge) = &mut self.cartridge {
            cartridge.restart_cycles(cartridge_cycles(now));
        }
    }

    pub(crate) fn backup_generation(&self) -> Option<u64> {
        self.cartridge.as_ref()?.backup_generation()
    }

    /// Appends the cartridge's `.sav` contents, if it has save memory.
    pub(crate) fn save_backup(&self, out: &mut Vec<u8>) {
        if let Some(cartridge) = &self.cartridge {
            cartridge.save_backup(out);
        }
    }

    /// Reads the halfword at `address` in 0x08000000-0x09FFFFFF.
    pub(crate) fn read_rom(&mut self, address: u32) -> u16 {
        self.cartridge
            .as_mut()
            .map_or(0xFFFF, |cartridge| cartridge.read_16(address))
    }

    /// Writes the halfword at `address` in 0x08000000-0x09FFFFFF at bus
    /// cycle `now`: only the GPIO port takes it.
    pub(crate) fn write_rom(&mut self, address: u32, value: u16, now: u64) {
        if let Some(cartridge) = &mut self.cartridge {
            cartridge.write_16(address, value, cartridge_cycles(now));
        }
    }

    /// Reads the byte at `address` in 0x0A000000-0x0AFFFFFF.
    pub(crate) fn read_ram(&self, address: u32) -> u8 {
        self.cartridge
            .as_ref()
            .map_or(0xFF, |cartridge| cartridge.read_backup(address & 0xFFFF))
    }

    /// Writes the byte at `address` in 0x0A000000-0x0AFFFFFF.
    pub(crate) fn write_ram(&mut self, address: u32, value: u8) {
        if let Some(cartridge) = &mut self.cartridge {
            cartridge.write_backup(address & 0xFFFF, value);
        }
    }

    /// The header fields the firmware copies to 0x027FFC30-0x027FFC3B
    /// (GBATEK, "DS Firmware/BIOS RAM Usage"; NitroSDK `CTRDGModuleInfo`):
    /// module ID (0xBE), expansion IDs (0xB5-0xB7), a flags byte left
    /// clear, maker code (0xB0) and game code (0xAC). An empty slot reads
    /// 0xFFFF as its module ID, which the library takes as "no cartridge".
    pub(crate) fn module_info(&mut self) -> [u8; 12] {
        let Some(cartridge) = &mut self.cartridge else {
            return [0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        };
        let mut byte = |offset: u32| {
            let half = cartridge.read_16(0x0800_0000 + (offset & !1));
            (half >> ((offset & 1) * 8)) as u8
        };
        MODULE_INFO.map(|offset| offset.map_or(0, &mut byte))
    }

    /// What the ARM9 BIOS holds at [`BIOS_LOGO`] for the inserted
    /// cartridge to pass Nintendo's check, for the HLE BIOS, which ships no
    /// logo: the cartridge's own, with the bits a retail header never sets
    /// (0x9C bits 2 and 7, 0x9E bits 0 and 1) cleared, as in the BIOS's
    /// copy. A cartridge with those bits set fails, as on hardware.
    pub(crate) fn bios_logo(&mut self) -> Option<[u8; 0x9C]> {
        let cartridge = self.cartridge.as_mut()?;
        let mut logo = [0; 0x9C];
        for (index, pair) in (0u32..).zip(logo.chunks_exact_mut(2)) {
            let half = cartridge.read_16(0x0800_0004 + index * 2);
            pair.copy_from_slice(&half.to_le_bytes());
        }
        logo[0x9C - 4] &= !0x84;
        logo[0x9E - 4] &= !0x03;
        Some(logo)
    }
}

/// Bus cycles for an access to the slot's ROM under `timing` (EXMEMCNT
/// bits 0-4 of the CPU that owns the slot): the first access time, or the
/// second for a sequential one. A 32-bit access is two 16-bit ones; an
/// 8-bit one costs the same as 16 bits.
pub(crate) fn rom_cycles(timing: u16, width: Width, sequential: bool) -> u64 {
    let first = ACCESS_TIMES[usize::from(timing >> 2 & 3)];
    let second = if timing & 0x10 != 0 { 4 } else { 6 };
    let half = if sequential { second } else { first };
    match width {
        Width::Word => half + second,
        Width::Half | Width::Byte => half,
    }
}

/// Bus cycles for an access of any width to the slot's SRAM or Flash under
/// `timing`.
pub(crate) fn ram_cycles(timing: u16) -> u64 {
    ACCESS_TIMES[usize::from(timing & 3)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_bus_cycles_to_the_cartridge_clock() {
        assert_eq!(cartridge_cycles(33_513_982), emu_gba_cart::CLOCK_HZ);
        assert_eq!(cartridge_cycles(0), 0);
    }

    #[test]
    fn access_times_follow_exmemcnt() {
        assert_eq!(rom_cycles(0, Width::Half, false), 10);
        assert_eq!(rom_cycles(0, Width::Word, false), 16);
        assert_eq!(rom_cycles(0x1C, Width::Word, true), 8);
        assert_eq!(rom_cycles(0x0C, Width::Byte, false), 18);
        assert_eq!(ram_cycles(3), 18);
        assert_eq!(ram_cycles(2), 6);
    }

    #[test]
    fn an_empty_slot_reads_all_ones_and_reports_no_cartridge() {
        let mut slot = GbaSlot::default();
        assert_eq!(slot.read_rom(0x0800_0000), 0xFFFF);
        assert_eq!(slot.read_ram(0x0A00_0000), 0xFF);
        assert_eq!(slot.module_info()[..2], [0xFF, 0xFF]);
        assert_eq!(slot.bios_logo(), None);
        assert_eq!(slot.backup_generation(), None);
    }

    #[test]
    fn module_info_copies_the_header() {
        let mut rom = vec![0u8; 0x200];
        rom[0xAC..0xB2].copy_from_slice(b"BPEE01");
        rom[0xB5] = 0x12;
        rom[0xBE] = 0x34;
        let mut slot = GbaSlot::new(Some(Cartridge::new(rom.into(), None, 0)));
        assert_eq!(
            slot.module_info(),
            [0x34, 0, 0x12, 0, 0, 0, b'0', b'1', b'B', b'P', b'E', b'E']
        );
    }

    #[test]
    fn the_bios_logo_drops_the_debug_bits() {
        let mut rom = vec![0u8; 0x200];
        rom[0x04] = 0x24;
        rom[0x9C] = 0xA5;
        rom[0x9E] = 0xFB;
        let mut slot = GbaSlot::new(Some(Cartridge::new(rom.into(), None, 0)));
        let logo = slot.bios_logo().expect("a cartridge is in");
        assert_eq!((logo[0], logo[0x98], logo[0x9A]), (0x24, 0x21, 0xF8));
    }
}
