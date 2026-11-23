//! Cartridges: ROM, battery RAM, memory bank controllers and the MBC3
//! real-time clock (Pan Docs, "The Cartridge Header", "MBC1", "MBC3",
//! "MBC5"; Gekkio, "Game Boy: Complete Technical Reference", "MBC1M").

use emu_core::{LoadError, RomPatch, System};
use emu_state::payload;
use serde::{Deserialize, Serialize};

/// Clock cycles per second, for the MBC3 real-time clock.
const CYCLES_PER_SECOND: u32 = 4_194_304;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Cartridge {
    /// ROM padded to a power of two of at least 32 KiB. Restored from the
    /// ROM file, never saved in states.
    #[serde(skip)]
    rom: Box<[u8]>,
    /// Cheats' changes to what ROM reads return, at 0x0000–0x7FFF in any
    /// bank. Like the ROM, never saved in states.
    #[serde(skip)]
    rom_patches: Box<[RomPatch]>,
    /// Precomputed byte offset into `rom` for 0x0000–0x3FFF and 0x4000–0x7FFF.
    #[serde(skip)]
    rom_bank0_base: usize,
    #[serde(skip)]
    rom_bank1_base: usize,
    /// As long as the header says; see [`Cartridge::in_range`] for a
    /// save state's.
    #[serde(with = "payload::bytes")]
    ram: Box<[u8]>,
    mbc: Mbc,
    has_battery: bool,
    /// Counts writes to battery-backed RAM and clock registers.
    generation: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
enum Mbc {
    #[default]
    RomOnly,
    Mbc1 {
        ram_enabled: bool,
        /// Low five bits of the ROM bank (BANK1).
        bank_low: u8,
        /// Two more bits: ROM bank bits 5–6 or the RAM bank (BANK2).
        bank_high: u8,
        /// Mode 1 applies BANK2 to 0x0000–0x3FFF and to RAM.
        advanced_mode: bool,
        /// MBC1M, the wiring of multi-game compilation cartridges: BANK2
        /// drives ROM bank bits 4–5 instead of 5–6, and BANK1 bit 4 is not
        /// connected, so each game sees sixteen banks.
        multicart: bool,
    },
    /// 512 half-bytes of RAM built into the controller.
    Mbc2 { ram_enabled: bool, rom_bank: u8 },
    Mbc3 {
        ram_enabled: bool,
        rom_bank: u8,
        /// 0x00–0x03 selects a RAM bank, 0x08–0x0C a clock register.
        select: u8,
        rtc: Option<Rtc>,
    },
    Mbc5 {
        ram_enabled: bool,
        rom_bank: u16,
        ram_bank: u8,
    },
}

impl Cartridge {
    /// Parses the header of `rom` and loads `backup` into battery RAM.
    ///
    /// `start_time` (Unix seconds) catches the MBC3 clock up with the time
    /// that passed since the save was written.
    pub(crate) fn new(
        rom: &[u8],
        backup: Option<&[u8]>,
        start_time: i64,
    ) -> Result<Self, LoadError> {
        let kind = *rom.get(0x147).ok_or(LoadError::InvalidRom {
            system: System::Gb,
            reason: "shorter than the 0x150-byte header",
        })?;
        let ram_size = match rom[0x149] {
            0 | 1 => 0,
            2 => 0x2000,
            3 => 0x8000,
            4 => 0x2_0000,
            5 => 0x1_0000,
            _ => {
                return Err(LoadError::InvalidRom {
                    system: System::Gb,
                    reason: "unknown RAM size in header",
                });
            }
        };
        let (mbc, has_battery) = match kind {
            0x00 | 0x08 | 0x09 => (Mbc::RomOnly, kind == 0x09),
            0x01..=0x03 => (
                Mbc::Mbc1 {
                    ram_enabled: false,
                    bank_low: 1,
                    bank_high: 0,
                    advanced_mode: false,
                    multicart: is_mbc1_multicart(rom),
                },
                kind == 0x03,
            ),
            0x0F..=0x13 => (
                Mbc::Mbc3 {
                    ram_enabled: false,
                    rom_bank: 1,
                    select: 0,
                    rtc: matches!(kind, 0x0F | 0x10).then(|| Rtc::new(start_time)),
                },
                matches!(kind, 0x0F | 0x10 | 0x13),
            ),
            0x19..=0x1E => (
                Mbc::Mbc5 {
                    ram_enabled: false,
                    rom_bank: 1,
                    ram_bank: 0,
                },
                matches!(kind, 0x1B | 0x1E),
            ),
            0x05 | 0x06 => (
                Mbc::Mbc2 {
                    ram_enabled: false,
                    rom_bank: 1,
                },
                kind == 0x06,
            ),
            _ => return Err(LoadError::Unsupported("this cartridge type")),
        };

        let padded_len = rom.len().max(0x8000).next_power_of_two();
        let mut padded = vec![0xFF; padded_len];
        padded[..rom.len()].copy_from_slice(rom);

        let ram_size = if let Mbc::Mbc2 { .. } = mbc {
            0x200
        } else {
            ram_size
        };
        let mut cartridge = Self {
            rom: padded.into_boxed_slice(),
            rom_patches: Box::default(),
            rom_bank0_base: 0,
            rom_bank1_base: 0,
            ram: vec![0; ram_size].into_boxed_slice(),
            mbc,
            has_battery,
            generation: 0,
        };
        cartridge.update_rom_bases();
        if let Some(backup) = backup {
            cartridge.load_backup(backup, start_time);
        }
        Ok(cartridge)
    }

    /// Returns the bank controller to its power-on state. RAM and the
    /// clock keep running, as they are battery-powered.
    pub(crate) fn power_cycle(&mut self) {
        match &mut self.mbc {
            Mbc::RomOnly => {}
            Mbc::Mbc1 {
                ram_enabled,
                bank_low,
                bank_high,
                advanced_mode,
                multicart: _,
            } => {
                (*ram_enabled, *bank_low, *bank_high, *advanced_mode) = (false, 1, 0, false);
            }
            Mbc::Mbc2 {
                ram_enabled,
                rom_bank,
            } => (*ram_enabled, *rom_bank) = (false, 1),
            Mbc::Mbc3 {
                ram_enabled,
                rom_bank,
                select,
                ..
            } => (*ram_enabled, *rom_bank, *select) = (false, 1, 0),
            Mbc::Mbc5 {
                ram_enabled,
                rom_bank,
                ram_bank,
            } => (*ram_enabled, *rom_bank, *ram_bank) = (false, 1, 0),
        }
        self.update_rom_bases();
    }

    /// Whether this cartridge, read from a save state, has the RAM its
    /// controller reads without asking: an MBC2's 512 bytes. The others
    /// wrap their banks at whatever length the RAM has.
    pub(crate) fn in_range(&self) -> bool {
        !matches!(self.mbc, Mbc::Mbc2 { .. }) || self.ram.len() >= 0x200
    }

    /// Moves the ROM and its patches out of `other` into `self`; used after
    /// loading a save state, which contains neither.
    pub(crate) fn take_rom_from(&mut self, other: &mut Self) {
        self.rom = std::mem::take(&mut other.rom);
        self.rom_patches = std::mem::take(&mut other.rom_patches);
        self.update_rom_bases();
    }

    /// Replaces the ROM patches. Patches outside 0x0000–0x7FFF are
    /// dropped: only ROM reads pass through them.
    pub(crate) fn set_rom_patches(&mut self, patches: &[RomPatch]) {
        self.rom_patches = patches
            .iter()
            .filter(|patch| patch.address < 0x8000)
            .copied()
            .collect();
    }

    fn load_backup(&mut self, backup: &[u8], start_time: i64) {
        let ram_len = self.ram.len().min(backup.len());
        self.ram[..ram_len].copy_from_slice(&backup[..ram_len]);
        if let Mbc::Mbc3 { rtc: Some(rtc), .. } = &mut self.mbc
            && let Some(trailer) = backup.get(self.ram.len()..)
        {
            rtc.load_trailer(trailer, start_time);
        }
    }

    /// The ROM, padded to a power of two.
    pub(crate) fn rom(&self) -> &[u8] {
        &self.rom
    }

    /// Battery-backed RAM, for test ROMs that report results there.
    pub(crate) fn ram(&self) -> &[u8] {
        &self.ram
    }

    pub(crate) fn backup_generation(&self) -> Option<u64> {
        self.has_battery.then_some(self.generation)
    }

    pub(crate) fn save_backup(&self, out: &mut Vec<u8>) {
        if !self.has_battery {
            return;
        }
        out.extend_from_slice(&self.ram);
        if let Mbc::Mbc3 { rtc: Some(rtc), .. } = &self.mbc {
            rtc.write_trailer(out);
        }
    }

    /// Advances the MBC3 clock by `cycles` real-time clock cycles.
    #[inline(always)]
    pub(crate) fn tick(&mut self, cycles: u32) {
        if let Mbc::Mbc3 { rtc: Some(rtc), .. } = &mut self.mbc {
            rtc.tick(cycles);
        }
    }

    fn update_rom_bases(&mut self) {
        let banks = self.rom.len() / 0x4000;
        if banks == 0 {
            self.rom_bank0_base = 0;
            self.rom_bank1_base = 0;
            return;
        }
        let (bank0, bank1) = match &self.mbc {
            Mbc::RomOnly => (0, 1),
            Mbc::Mbc1 {
                bank_low,
                bank_high,
                advanced_mode,
                multicart,
                ..
            } => {
                let shift = mbc1_high_shift(*multicart);
                let low_mask = (1 << shift) - 1;
                let b0 = if *advanced_mode {
                    usize::from(*bank_high) << shift
                } else {
                    0
                };
                let b1 = usize::from(*bank_high) << shift | usize::from(*bank_low) & low_mask;
                (b0, b1)
            }
            Mbc::Mbc2 { rom_bank, .. } | Mbc::Mbc3 { rom_bank, .. } => (0, usize::from(*rom_bank)),
            Mbc::Mbc5 { rom_bank, .. } => (0, usize::from(*rom_bank)),
        };
        self.rom_bank0_base = (bank0 % banks) * 0x4000;
        self.rom_bank1_base = (bank1 % banks) * 0x4000;
    }

    #[inline(always)]
    pub(crate) fn read_rom(&self, address: u16) -> u8 {
        let base = if address < 0x4000 {
            self.rom_bank0_base
        } else {
            self.rom_bank1_base
        };
        let byte = self.rom[base + usize::from(address & 0x3FFF)];
        if self.rom_patches.is_empty() {
            byte
        } else {
            RomPatch::apply(&self.rom_patches, u32::from(address), byte)
        }
    }

    /// A write to 0x0000–0x7FFF: a bank controller register.
    pub(crate) fn write_rom(&mut self, address: u16, value: u8) {
        let rom_bank_mask = if self.rom.len() > 0x20_0000 {
            0xFF
        } else {
            0x7F
        };
        match &mut self.mbc {
            Mbc::RomOnly => return,
            Mbc::Mbc1 {
                ram_enabled,
                bank_low,
                bank_high,
                advanced_mode,
                ..
            } => match address {
                0x0000..=0x1FFF => {
                    *ram_enabled = value & 0x0F == 0x0A;
                    return;
                }
                0x2000..=0x3FFF => *bank_low = (value & 0x1F).max(1),
                0x4000..=0x5FFF => *bank_high = value & 0x03,
                _ => *advanced_mode = value & 1 != 0,
            },
            Mbc::Mbc2 {
                ram_enabled,
                rom_bank,
            } => {
                // Address bit 8 picks the register.
                if address < 0x4000 {
                    if address & 0x0100 == 0 {
                        *ram_enabled = value & 0x0F == 0x0A;
                        return;
                    } else {
                        *rom_bank = (value & 0x0F).max(1);
                    }
                } else {
                    return;
                }
            }
            Mbc::Mbc3 {
                ram_enabled,
                rom_bank,
                select,
                rtc,
            } => match address {
                0x0000..=0x1FFF => {
                    *ram_enabled = value & 0x0F == 0x0A;
                    return;
                }
                // ROMs over 2 MiB need the MBC30, whose bank register has all
                // 8 bits (Japanese Pokémon Crystal).
                0x2000..=0x3FFF => *rom_bank = (value & rom_bank_mask).max(1),
                0x4000..=0x5FFF => {
                    *select = value;
                    return;
                }
                _ => {
                    if let Some(rtc) = rtc {
                        rtc.write_latch(value);
                    }
                    return;
                }
            },
            Mbc::Mbc5 {
                ram_enabled,
                rom_bank,
                ram_bank,
            } => match address {
                0x0000..=0x1FFF => {
                    *ram_enabled = value & 0x0F == 0x0A;
                    return;
                }
                0x2000..=0x2FFF => *rom_bank = (*rom_bank & 0x100) | u16::from(value),
                0x3000..=0x3FFF => *rom_bank = (*rom_bank & 0xFF) | u16::from(value & 1) << 8,
                0x4000..=0x5FFF => {
                    *ram_bank = value & 0x0F;
                    return;
                }
                _ => return,
            },
        }
        self.update_rom_bases();
    }

    /// Whether the game has enabled RAM (always, without a controller).
    fn ram_enabled(&self) -> bool {
        match &self.mbc {
            Mbc::RomOnly => true,
            Mbc::Mbc1 { ram_enabled, .. }
            | Mbc::Mbc2 { ram_enabled, .. }
            | Mbc::Mbc3 { ram_enabled, .. }
            | Mbc::Mbc5 { ram_enabled, .. } => *ram_enabled,
        }
    }

    /// The RAM bank mapped at 0xA000, if RAM is enabled and mapped there
    /// rather than a clock register.
    fn selected_ram_bank(&self) -> Option<usize> {
        match &self.mbc {
            Mbc::RomOnly
            | Mbc::Mbc2 {
                ram_enabled: true, ..
            } => Some(0),
            Mbc::Mbc1 {
                ram_enabled: true,
                bank_high,
                advanced_mode,
                ..
            } => Some(if *advanced_mode {
                usize::from(*bank_high)
            } else {
                0
            }),
            Mbc::Mbc3 {
                ram_enabled: true,
                select: select @ 0..=0x07,
                ..
            } => Some(usize::from(*select)),
            Mbc::Mbc5 {
                ram_enabled: true,
                ram_bank,
                ..
            } => Some(usize::from(*ram_bank)),
            _ => None,
        }
    }

    /// The RAM byte index for `address` in `bank`, if there is RAM.
    fn ram_index_in(&self, bank: usize, address: u16) -> Option<usize> {
        if let Mbc::Mbc2 { .. } = self.mbc {
            // 512 bytes, echoed through the whole 8 KiB window.
            return Some(usize::from(address & 0x01FF));
        }
        if self.ram.is_empty() {
            return None;
        }
        Some((bank * 0x2000 + usize::from(address & 0x1FFF)) % self.ram.len())
    }

    /// The RAM byte index for `address` in the selected bank, if RAM is
    /// enabled and present.
    fn ram_index(&self, address: u16) -> Option<usize> {
        self.ram_index_in(self.selected_ram_bank()?, address)
    }

    pub(crate) fn read_ram(&self, address: u16) -> u8 {
        if let Mbc::Mbc3 {
            ram_enabled: true,
            select: register @ 0x08..=0x0C,
            rtc: Some(rtc),
            ..
        } = &self.mbc
        {
            return rtc.read(*register);
        }
        let value = self
            .ram_index(address)
            .map_or(0xFF, |index| self.ram[index]);
        if let Mbc::Mbc2 { .. } = self.mbc {
            // Only the low four bits exist.
            value | 0xF0
        } else {
            value
        }
    }

    pub(crate) fn write_ram(&mut self, address: u16, value: u8) {
        if let Mbc::Mbc3 {
            ram_enabled: true,
            select: register @ 0x08..=0x0C,
            rtc: Some(rtc),
            ..
        } = &mut self.mbc
        {
            rtc.write(*register, value);
            self.generation = self.generation.wrapping_add(1);
            return;
        }
        if let Some(index) = self.ram_index(address) {
            self.ram[index] = value;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// Writes `value` at `address` in RAM bank `bank`, whichever bank is
    /// selected, for cheats. Like the game's own writes, it needs RAM
    /// enabled.
    pub(crate) fn write_ram_bank(&mut self, bank: u8, address: u16, value: u8) {
        if !self.ram_enabled() {
            return;
        }
        if let Some(index) = self.ram_index_in(usize::from(bank), address) {
            self.ram[index] = value;
            self.generation = self.generation.wrapping_add(1);
        }
    }
}

/// The ROM bank bit BANK2 starts at: 5 on a plain MBC1, 4 on an MBC1M.
const fn mbc1_high_shift(multicart: bool) -> u32 {
    if multicart { 4 } else { 5 }
}

/// Whether an MBC1 cartridge is an MBC1M multicart. These are all 8 Mbit
/// (1 MiB) and repeat a game header, with its logo, at the start of bank
/// 0x10 where the second game begins. Comparing the logo with bank 0's own
/// avoids copying it here; a blank logo (one repeated byte) never counts,
/// as no boot ROM would accept it.
fn is_mbc1_multicart(rom: &[u8]) -> bool {
    const SECOND_GAME: usize = 0x10 * 0x4000;
    if rom.len() != 0x10_0000 {
        return false;
    }
    let logo = &rom[0x104..0x134];
    let second_logo = &rom[SECOND_GAME + 0x104..SECOND_GAME + 0x134];
    logo == second_logo && logo.iter().any(|byte| *byte != logo[0])
}

/// The MBC3 real-time clock.
///
/// Time comes from emulated cycles, never the host clock, so runs stay
/// deterministic. When a save is loaded, the clock catches up with the
/// wall-clock time that passed since the save's timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Rtc {
    #[serde(with = "payload::Below::<64>")]
    seconds: u8,
    #[serde(with = "payload::Below::<64>")]
    minutes: u8,
    #[serde(with = "payload::Below::<32>")]
    hours: u8,
    /// Nine-bit day counter.
    #[serde(with = "payload::Below::<512>")]
    days: u16,
    halted: bool,
    /// Set when the day counter overflows; cleared only by the game.
    day_carry: bool,
    /// Clock cycles into the current RTC second. Frozen while halted.
    #[serde(with = "payload::Below::<{ CYCLES_PER_SECOND as u64 }>")]
    subsecond: u32,
    /// Clock cycles into the current wall-clock second, for `unix_time`.
    #[serde(with = "payload::Below::<{ CYCLES_PER_SECOND as u64 }>")]
    wall_subsecond: u32,
    /// Seconds, minutes, hours, DL, DH as last latched.
    latched: [u8; 5],
    /// The last latch-register write was 0x00.
    latch_armed: bool,
    /// Emulated Unix time in seconds, for the save timestamp.
    unix_time: i64,
}

impl Rtc {
    fn new(start_time: i64) -> Self {
        Self {
            seconds: 0,
            minutes: 0,
            hours: 0,
            days: 0,
            halted: false,
            day_carry: false,
            subsecond: 0,
            wall_subsecond: 0,
            latched: [0; 5],
            latch_armed: false,
            unix_time: start_time,
        }
    }

    fn registers(&self) -> [u8; 5] {
        let [day_low, day_high] = self.days.to_le_bytes();
        let control = (day_high & 1) | u8::from(self.halted) << 6 | u8::from(self.day_carry) << 7;
        [self.seconds, self.minutes, self.hours, day_low, control]
    }

    fn tick(&mut self, cycles: u32) {
        self.wall_subsecond += cycles;
        if self.wall_subsecond >= CYCLES_PER_SECOND {
            self.wall_subsecond -= CYCLES_PER_SECOND;
            // A save state may hold any time.
            self.unix_time = self.unix_time.wrapping_add(1);
        }
        if self.halted {
            return;
        }
        self.subsecond += cycles;
        if self.subsecond >= CYCLES_PER_SECOND {
            self.subsecond -= CYCLES_PER_SECOND;
            self.advance_second();
        }
    }

    /// Counts one second. Counters wrap at their bit width, so values a game
    /// wrote out of range (seconds = 62) count up to the wrap instead of
    /// carrying, as on hardware.
    fn advance_second(&mut self) {
        self.seconds = (self.seconds + 1) & 0x3F;
        if self.seconds != 60 {
            return;
        }
        self.seconds = 0;
        self.minutes = (self.minutes + 1) & 0x3F;
        if self.minutes != 60 {
            return;
        }
        self.minutes = 0;
        self.hours = (self.hours + 1) & 0x1F;
        if self.hours != 24 {
            return;
        }
        self.hours = 0;
        self.days += 1;
        if self.days == 512 {
            self.days = 0;
            self.day_carry = true;
        }
    }

    fn write_latch(&mut self, value: u8) {
        if self.latch_armed && value == 1 {
            self.latched = self.registers();
        }
        self.latch_armed = value == 0;
    }

    fn read(&self, register: u8) -> u8 {
        let masks = [0x3F, 0x3F, 0x1F, 0xFF, 0xC1];
        let index = usize::from(register - 0x08);
        self.latched[index] & masks[index]
    }

    fn write(&mut self, register: u8, value: u8) {
        match register {
            0x08 => {
                self.seconds = value & 0x3F;
                self.subsecond = 0;
            }
            0x09 => self.minutes = value & 0x3F,
            0x0A => self.hours = value & 0x1F,
            0x0B => self.days = (self.days & 0x100) | u16::from(value),
            _ => {
                self.days = (self.days & 0xFF) | u16::from(value & 1) << 8;
                self.halted = value & 0x40 != 0;
                self.day_carry = value & 0x80 != 0;
            }
        }
    }

    /// Appends the de facto 48-byte trailer used by BGB, VBA-M, mGBA and
    /// SameBoy: live and latched registers as little-endian `u32`s, then a
    /// 64-bit Unix timestamp.
    fn write_trailer(&self, out: &mut Vec<u8>) {
        for value in self.registers().into_iter().chain(self.latched) {
            out.extend_from_slice(&u32::from(value).to_le_bytes());
        }
        out.extend_from_slice(&self.unix_time.to_le_bytes());
    }

    /// Reads a 48-byte trailer, or the older 44-byte one with a 32-bit
    /// timestamp, then catches up to `now`.
    fn load_trailer(&mut self, trailer: &[u8], now: i64) {
        let word = |index: usize| -> u8 {
            let bytes = &trailer[index * 4..index * 4 + 4];
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u8
        };
        let timestamp = match trailer.len() {
            48 => i64::from_le_bytes(trailer[40..48].try_into().expect("8 bytes")),
            44 => i64::from(u32::from_le_bytes(
                trailer[40..44].try_into().expect("4 bytes"),
            )),
            _ => return,
        };
        self.write(0x08, word(0));
        self.write(0x09, word(1));
        self.write(0x0A, word(2));
        self.write(0x0B, word(3));
        self.write(0x0C, word(4));
        for (index, latched) in self.latched.iter_mut().enumerate() {
            *latched = word(5 + index);
        }
        if !self.halted && now > timestamp {
            // A trailer may hold any timestamp, as far back as there is.
            for _ in 0..now.saturating_sub(timestamp).min(MAX_CATCH_UP_SECONDS) {
                self.advance_second();
            }
        }
    }
}

/// Catching up more than this (about 1.4 years, past the 511-day counter)
/// changes nothing a game can observe beyond the carry flag.
const MAX_CATCH_UP_SECONDS: i64 = 512 * 86_400 + 1;

#[cfg(test)]
mod tests {
    use super::*;

    fn rom(kind: u8, ram_size: u8, banks: usize) -> Vec<u8> {
        let mut rom = vec![0; banks * 0x4000];
        for (bank, chunk) in rom.chunks_mut(0x4000).enumerate() {
            chunk[0] = bank as u8;
        }
        rom[0x147] = kind;
        rom[0x149] = ram_size;
        rom
    }

    #[test]
    fn rom_patches_apply_in_any_bank_their_compare_byte_matches() {
        let mut cart = Cartridge::new(&rom(0x01, 0, 4), None, 0).expect("valid");
        cart.set_rom_patches(&[
            // Bank 2 only: each bank's first byte is its number.
            RomPatch {
                address: 0x4000,
                value: 0x77,
                compare: Some(2),
            },
            RomPatch {
                address: 0x0001,
                value: 0x55,
                compare: None,
            },
        ]);
        assert_eq!(cart.read_rom(0x4000), 1);
        assert_eq!(cart.read_rom(0x0001), 0x55);
        cart.write_rom(0x2000, 2);
        assert_eq!(cart.read_rom(0x4000), 0x77);
        cart.write_rom(0x2000, 3);
        assert_eq!(cart.read_rom(0x4000), 3);
    }

    #[test]
    fn battery_ram_needs_enabling_and_counts_writes() {
        let mut cart = Cartridge::new(&rom(0x03, 2, 2), None, 0).expect("valid");
        cart.write_ram(0xA000, 0x12);
        assert_eq!(cart.read_ram(0xA000), 0xFF);
        assert_eq!(cart.backup_generation(), Some(0));
        cart.write_rom(0x0000, 0x0A);
        cart.write_ram(0xA000, 0x12);
        assert_eq!(cart.read_ram(0xA000), 0x12);
        assert_eq!(cart.backup_generation(), Some(1));
    }

    #[test]
    fn bank_writes_reach_unselected_banks() {
        // MBC5 with four 8 KiB RAM banks.
        let mut cart = Cartridge::new(&rom(0x1B, 3, 2), None, 0).expect("valid");
        cart.write_ram_bank(2, 0xA010, 0x55);
        assert_eq!(cart.backup_generation(), Some(0), "RAM is not enabled");
        cart.write_rom(0x0000, 0x0A);
        cart.write_ram_bank(2, 0xA010, 0x55);
        assert_eq!(cart.read_ram(0xA010), 0x00, "bank 0 is still selected");
        cart.write_rom(0x4000, 2);
        assert_eq!(cart.read_ram(0xA010), 0x55);
    }

    /// A save whose trailer says it was written at the start of time: the
    /// seconds since overflowed, which in a debug build stopped the game
    /// loading and in a release build skipped the clock's catching up.
    #[test]
    fn a_save_from_the_start_of_time_catches_the_clock_up() {
        let mut save = vec![0; 0x2000 + 48];
        save[0x2000 + 40..].copy_from_slice(&i64::MIN.to_le_bytes());
        let mut cart = Cartridge::new(&rom(0x10, 2, 4), Some(&save), 1_000).expect("valid");
        cart.write_rom(0x0000, 0x0A);
        cart.write_rom(0x6000, 0);
        cart.write_rom(0x6000, 1);
        // As far as the clock catches up: a second past 512 days, with
        // the day counter's carry set.
        cart.write_rom(0x4000, 0x08);
        assert_eq!(cart.read_ram(0xA000), 1, "seconds");
        cart.write_rom(0x4000, 0x0C);
        assert_eq!(cart.read_ram(0xA000) & 0x81, 0x80, "the day carry");
    }

    #[test]
    fn mbc3_clock_latches_and_survives_a_save() {
        let mut cart = Cartridge::new(&rom(0x10, 2, 4), None, 1_000).expect("valid");
        cart.write_rom(0x0000, 0x0A);
        for _ in 0..(CYCLES_PER_SECOND / 4) * 61 {
            cart.tick(4);
        }
        cart.write_rom(0x6000, 0);
        cart.write_rom(0x6000, 1);
        cart.write_rom(0x4000, 0x08);
        assert_eq!(cart.read_ram(0xA000), 1);
        cart.write_rom(0x4000, 0x09);
        assert_eq!(cart.read_ram(0xA000), 1);

        let mut save = Vec::new();
        cart.save_backup(&mut save);
        assert_eq!(save.len(), 0x2000 + 48);

        // Reload 3600 seconds after the save's timestamp (1000 + 61).
        let mut reloaded =
            Cartridge::new(&rom(0x10, 2, 4), Some(&save), 1_061 + 3_600).expect("valid");
        reloaded.write_rom(0x0000, 0x0A);
        reloaded.write_rom(0x6000, 0);
        reloaded.write_rom(0x6000, 1);
        reloaded.write_rom(0x4000, 0x0A);
        assert_eq!(reloaded.read_ram(0xA000), 1);
    }
}
