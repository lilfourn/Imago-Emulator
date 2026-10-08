use crate::System;

/// Everything a core needs to power on: the game, its save, optional
/// system files, and the time to start the real-time clock at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Media<'a> {
    /// The cartridge image.
    pub rom: &'a [u8],
    /// The battery save previously written by [`crate::Core::save_backup`],
    /// if one exists.
    pub backup: Option<&'a [u8]>,
    /// BIOS and firmware dumped by the user. A core boots with high-level
    /// emulation or direct boot for anything missing here, and never ships
    /// these files itself.
    pub system_files: &'a [SystemFile<'a>],
    /// The local wall-clock time at power-on, counted in seconds from
    /// 1970-01-01 00:00 as if that were local time too. Seeds the cartridge
    /// or console real-time clock, which has no time zone and shows this
    /// date and time as is. Tests pass a fixed value for determinism.
    pub start_time: i64,
    /// A cartridge in a second slot (the DS GBA slot), if one is inserted.
    /// Systems without a second slot reject media that carry one.
    pub second_slot: Option<Cartridge<'a>>,
    /// How a Game Boy shows games made for the original, monochrome Game
    /// Boy. Other systems ignore it.
    pub dmg_colors: DmgColors,
}

impl<'a> Media<'a> {
    /// Media with only a ROM: no save, no system files, clock at the epoch,
    /// nothing in a second slot.
    #[must_use]
    pub const fn new(rom: &'a [u8]) -> Self {
        Self {
            rom,
            backup: None,
            system_files: &[],
            start_time: 0,
            second_slot: None,
            dmg_colors: DmgColors::Grey,
        }
    }

    /// The system file of `kind`, if the user provided one.
    #[must_use]
    pub fn system_file(&self, kind: SystemFileKind) -> Option<&'a [u8]> {
        self.system_files
            .iter()
            .find(|file| file.kind == kind)
            .map(|file| file.data)
    }
}

/// How games made for the original Game Boy, before the Game Boy Color,
/// are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DmgColors {
    /// On an original Game Boy, in four shades of grey.
    #[default]
    Grey,
    /// On a Game Boy Color, which colours a game with the palettes its
    /// boot ROM picks by the game's title.
    ByTitle,
    /// On a Game Boy Color, with the palettes its boot ROM gives when a
    /// button combination is held as it starts: 0 to 11 for Right, Left,
    /// Up, Down, the same with A, then the same with B.
    Buttons(u8),
}

/// A cartridge's contents: its image and its battery save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cartridge<'a> {
    /// The cartridge image.
    pub rom: &'a [u8],
    /// The battery save previously written by [`crate::Core::save_backup`]
    /// for this cartridge, if one exists.
    pub backup: Option<&'a [u8]>,
}

/// A BIOS or firmware image dumped from the user's own hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemFile<'a> {
    /// Which file this is.
    pub kind: SystemFileKind,
    /// Its contents.
    pub data: &'a [u8],
}

/// The kinds of system file a core can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemFileKind {
    /// DMG boot ROM, 256 bytes.
    GbBootRom,
    /// CGB boot ROM, 2304 bytes.
    GbcBootRom,
    /// GBA BIOS, 16 KiB.
    GbaBios,
    /// DS ARM7 BIOS (`bios7.bin`), 16 KiB.
    NdsBios7,
    /// DS ARM9 BIOS (`bios9.bin`), 4 KiB.
    NdsBios9,
    /// DS firmware (`firmware.bin`), 256 KiB or more.
    NdsFirmware,
}

/// Why a core refused to power on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    /// The ROM is not a valid image for this system.
    #[error("not a valid {system} rom: {reason}")]
    InvalidRom {
        /// The system that rejected it.
        system: System,
        /// What was wrong.
        reason: &'static str,
    },
    /// The game needs hardware the core does not emulate yet.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
}
