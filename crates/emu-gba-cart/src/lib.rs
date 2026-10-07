//! The Game Boy Advance cartridge: ROM, save memory (SRAM, Flash or
//! EEPROM) and the GPIO real-time clock (GBATEK, "GBA Cartridges").
//!
//! The same cartridge plugs into a Game Boy Advance and into a Nintendo
//! DS's GBA slot, so both cores use this crate and each maps it onto its
//! own bus. Methods take bus addresses and use only the bits the cartridge
//! decodes: the ROM offset below 32 MiB, the save memory offset below
//! 64 KiB.
//!
//! Times are counted in cycles of [`CLOCK_HZ`], the GBA system clock; a
//! host on another clock converts before calling.

mod backup;
mod cartridge;
mod rtc;

pub use cartridge::Cartridge;

/// The clock that cycle counts passed to a [`Cartridge`] run at: the GBA
/// system clock, 16.78 MHz.
pub const CLOCK_HZ: u64 = 16_777_216;
