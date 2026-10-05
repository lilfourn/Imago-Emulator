//! Which Game Boy is being emulated.

use serde::{Deserialize, Serialize};

/// A Game Boy hardware model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Model {
    /// The original Game Boy (DMG-CPU), monochrome.
    Dmg,
    /// The Game Boy Color running a cartridge in CGB mode.
    Cgb,
}

impl Model {
    /// The model a cartridge asks for: CGB if bit 7 of its header's CGB
    /// flag (0x0143) is set, which covers both CGB-enhanced and CGB-only
    /// games; otherwise DMG.
    #[must_use]
    pub fn for_rom(rom: &[u8]) -> Self {
        match rom.get(0x143) {
            Some(flag) if flag & 0x80 != 0 => Self::Cgb,
            _ => Self::Dmg,
        }
    }
}
