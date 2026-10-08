//! ROM patches: changes to what a game reads from its cartridge ROM.

/// One byte of a game's ROM read as another, the way cheat devices such
/// as the Game Genie patched games: they sat between the console and the
/// cartridge, watched the address bus, and put their own byte on the data
/// bus when the game read a patched address. The ROM itself is unchanged.
///
/// Frontends hand a core the full set with [`crate::Core::set_rom_patches`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RomPatch {
    /// The CPU address of the byte: 0x0000–0x7FFF on a Game Boy, in
    /// whichever bank is mapped there; 0x08000000 up on a Game Boy
    /// Advance, where it covers the ROM's wait-state mirrors too.
    pub address: u32,
    /// The byte the game reads instead.
    pub value: u8,
    /// If set, the patch applies only while the ROM holds this byte at
    /// `address`. Game Boy Game Genie codes use it to pick out one ROM
    /// bank among those that share an address.
    pub compare: Option<u8>,
}

impl RomPatch {
    /// What a read of `address` returns when the ROM holds `original`
    /// there: the value of the first patch in `patches` at `address`
    /// whose compare byte, if it has one, matches `original`; otherwise
    /// `original`.
    #[must_use]
    pub fn apply(patches: &[Self], address: u32, original: u8) -> u8 {
        patches
            .iter()
            .find(|patch| {
                patch.address == address && patch.compare.is_none_or(|byte| byte == original)
            })
            .map_or(original, |patch| patch.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_matching_patch_wins_and_compares_pick_banks() {
        let patches = [
            RomPatch {
                address: 0x4000,
                value: 0x11,
                compare: Some(0xAA),
            },
            RomPatch {
                address: 0x4000,
                value: 0x22,
                compare: Some(0xBB),
            },
            RomPatch {
                address: 0x0150,
                value: 0x33,
                compare: None,
            },
            RomPatch {
                address: 0x0150,
                value: 0x44,
                compare: None,
            },
        ];
        assert_eq!(RomPatch::apply(&patches, 0x4000, 0xAA), 0x11);
        assert_eq!(RomPatch::apply(&patches, 0x4000, 0xBB), 0x22);
        assert_eq!(RomPatch::apply(&patches, 0x4000, 0xCC), 0xCC);
        assert_eq!(RomPatch::apply(&patches, 0x0150, 0x00), 0x33);
        assert_eq!(RomPatch::apply(&patches, 0x0151, 0x00), 0x00);
        assert_eq!(RomPatch::apply(&[], 0x0150, 0x99), 0x99);
    }
}
