//! The cartridge's save chip on the AUXSPI bus (GBATEK, "DS Cartridge
//! Backup", "DS Cart Infrared Cartridge SPI Commands").
//!
//! Nothing in the header says which chip a game has. A `.sav` file's size
//! decides when one is given; otherwise a table of game codes; otherwise
//! [`DEFAULT`], a 64 KiB EEPROM, the most common type.
//!
//! Games with an infrared port (game codes starting with `I`, such as
//! Pokémon HeartGold and Black) put an infrared controller in front of the
//! chip. Each transfer starts with a byte for the controller: 00h passes the
//! rest through to the save chip, 08h asks for its version (AAh), and the
//! infrared commands find nothing to receive.

use serde::{Deserialize, Serialize};

use super::eeprom::Eeprom;
use crate::spi::flash::Flash;

/// A save chip type: EEPROM (which covers FRAM) or flash, and its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Eeprom(usize),
    Flash(usize),
}

const KIB: usize = 1024;

/// Save sizes in ascending order and the chip each one means.
const SIZES: [Kind; 9] = [
    Kind::Eeprom(512),
    Kind::Eeprom(8 * KIB),
    Kind::Eeprom(32 * KIB),
    Kind::Eeprom(64 * KIB),
    Kind::Eeprom(128 * KIB),
    Kind::Flash(256 * KIB),
    Kind::Flash(512 * KIB),
    Kind::Flash(1024 * KIB),
    Kind::Flash(8192 * KIB),
];

/// The type for games neither a save nor the table identifies.
const DEFAULT: Kind = Kind::Eeprom(64 * KIB);

/// Save types by game code (header 0x0C, without the region letter). Add a
/// game here when the default is wrong for it.
const GAMES: [(&[u8; 3], Kind); 14] = [
    (b"ADA", Kind::Flash(512 * KIB)),  // Pokémon Diamond
    (b"APA", Kind::Flash(512 * KIB)),  // Pokémon Pearl
    (b"CPU", Kind::Flash(512 * KIB)),  // Pokémon Platinum
    (b"IPK", Kind::Flash(512 * KIB)),  // Pokémon HeartGold
    (b"IPG", Kind::Flash(512 * KIB)),  // Pokémon SoulSilver
    (b"IRB", Kind::Flash(512 * KIB)),  // Pokémon Black
    (b"IRA", Kind::Flash(512 * KIB)),  // Pokémon White
    (b"IRE", Kind::Flash(512 * KIB)),  // Pokémon Black 2
    (b"IRD", Kind::Flash(512 * KIB)),  // Pokémon White 2
    (b"AMC", Kind::Flash(256 * KIB)),  // Mario Kart DS
    (b"AZE", Kind::Flash(512 * KIB)),  // The Legend of Zelda: Phantom Hourglass
    (b"BKI", Kind::Flash(1024 * KIB)), // The Legend of Zelda: Spirit Tracks
    (b"ASM", Kind::Eeprom(8 * KIB)),   // Super Mario 64 DS
    (b"ARF", Kind::Eeprom(64 * KIB)),  // Rune Factory
];

/// DeSmuME appends this line and a footer after the raw save.
const DESMUME_FOOTER: &[u8] =
    b"|<--Snip above here to create a raw sav by excluding this DeSmuME savedata footer:";

/// Infrared controller commands.
const IR_SAVE: u8 = 0x00;
const IR_VERSION: u8 = 0x08;
const IR_CHIP_VERSION: u8 = 0xAA;

/// The game code at header 0x0C, if the ROM is long enough.
pub(crate) fn game_code(rom: &[u8]) -> Option<&[u8]> {
    rom.get(0x0C..0x10)
}

/// Whether the game has an infrared port, by its game code.
pub(crate) fn has_infrared(rom: &[u8]) -> bool {
    game_code(rom).is_some_and(|code| code[0] == b'I')
}

/// The raw save in a `.sav` file, without DeSmuME's footer.
fn raw_save(file: &[u8]) -> &[u8] {
    file.windows(DESMUME_FOOTER.len())
        .position(|window| window == DESMUME_FOOTER)
        .map_or(file, |end| &file[..end])
}

fn size(kind: Kind) -> usize {
    match kind {
        Kind::Eeprom(size) | Kind::Flash(size) => size,
    }
}

/// The smallest chip that holds a save of `length` bytes.
fn from_save_size(length: usize) -> Option<Kind> {
    if length == 0 {
        return None;
    }
    SIZES.into_iter().find(|&kind| size(kind) >= length)
}

fn listed(rom: &[u8]) -> Option<Kind> {
    let code = game_code(rom)?;
    GAMES
        .iter()
        .find(|(game, _)| game.as_slice() == &code[..3])
        .map(|&(_, kind)| kind)
}

/// The JEDEC ID of an ST flash part of `size` bytes.
fn flash_id(size: usize) -> [u8; 3] {
    match size {
        0x4_0000 => [0x20, 0x40, 0x12],
        0x8_0000 => [0x20, 0x40, 0x13],
        0x10_0000 => [0x20, 0x40, 0x14],
        // Art Academy's 8 MiB Macronix part.
        _ => [0xC2, 0x20, 0x17],
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Chip {
    Eeprom(Eeprom),
    Flash(Flash),
}

/// The infrared controller's view of one transfer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Infrared {
    /// The first byte since chip select.
    command: Option<u8>,
    /// Bytes since that one.
    count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Backup {
    chip: Chip,
    infrared: Option<Infrared>,
}

impl Backup {
    /// The save chip for `rom`, holding `save` (a `.sav` file) if given.
    pub(crate) fn new(rom: &[u8], save: Option<&[u8]>) -> Self {
        let save = save.map(raw_save);
        let kind = save
            .and_then(|data| from_save_size(data.len()))
            .or_else(|| listed(rom))
            .unwrap_or(DEFAULT);
        let data = save.unwrap_or_default();
        let chip = match kind {
            Kind::Eeprom(size) => {
                let mut eeprom = Eeprom::new(size);
                eeprom.load(data);
                Chip::Eeprom(eeprom)
            }
            Kind::Flash(size) => {
                let mut memory = vec![0xFF; size];
                let length = data.len().min(size);
                memory[..length].copy_from_slice(&data[..length]);
                Chip::Flash(Flash::new(memory, flash_id(size)))
            }
        };
        Self {
            chip,
            infrared: has_infrared(rom).then(Infrared::default),
        }
    }

    /// The `.sav` file contents: the raw chip memory.
    pub(crate) fn data(&self) -> &[u8] {
        match &self.chip {
            Chip::Eeprom(eeprom) => eeprom.memory(),
            Chip::Flash(flash) => flash.memory(),
        }
    }

    /// Whether a write changed the save since the last call.
    pub(crate) fn take_changed(&mut self) -> bool {
        match &mut self.chip {
            Chip::Eeprom(eeprom) => eeprom.take_changed(),
            Chip::Flash(flash) => flash.take_changed(),
        }
    }

    /// Shifts one byte in and returns the byte shifted out.
    pub(crate) fn transfer(&mut self, input: u8) -> u8 {
        let Some(infrared) = &mut self.infrared else {
            return self.chip_transfer(input);
        };
        let Some(command) = infrared.command else {
            infrared.command = Some(input);
            return 0;
        };
        infrared.count += 1;
        let count = infrared.count;
        match command {
            IR_SAVE => self.chip_transfer(input),
            IR_VERSION if count == 1 => IR_CHIP_VERSION,
            // Infrared receive reports an empty packet; the rest is padding.
            _ => 0,
        }
    }

    /// Chip select goes high.
    pub(crate) fn release(&mut self) {
        if let Some(infrared) = &mut self.infrared {
            *infrared = Infrared::default();
        }
        match &mut self.chip {
            Chip::Eeprom(eeprom) => eeprom.release(),
            Chip::Flash(flash) => flash.release(),
        }
    }

    fn chip_transfer(&mut self, input: u8) -> u8 {
        match &mut self.chip {
            Chip::Eeprom(eeprom) => eeprom.transfer(input),
            Chip::Flash(flash) => flash.transfer(input),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with_code(code: &[u8; 4]) -> Vec<u8> {
        let mut rom = vec![0; 0x200];
        rom[0x0C..0x10].copy_from_slice(code);
        rom
    }

    fn command(backup: &mut Backup, bytes: &[u8]) -> Vec<u8> {
        let out = bytes.iter().map(|&byte| backup.transfer(byte)).collect();
        backup.release();
        out
    }

    #[test]
    fn picks_the_chip_from_the_save_size_then_the_game_code() {
        let pokemon = rom_with_code(b"ADAE");
        let backup = Backup::new(&pokemon, None);
        assert!(matches!(backup.chip, Chip::Flash(_)));
        assert_eq!(backup.data().len(), 512 * KIB);
        assert!(backup.infrared.is_none());

        let backup = Backup::new(&pokemon, Some(&[0x12; 0x200]));
        assert!(matches!(backup.chip, Chip::Eeprom(_)));
        assert_eq!(backup.data(), [0x12; 0x200]);

        let unknown = rom_with_code(b"ZZZE");
        assert_eq!(Backup::new(&unknown, None).data().len(), 64 * KIB);
        assert_eq!(from_save_size(300 * KIB), Some(Kind::Flash(512 * KIB)));
    }

    #[test]
    fn drops_the_desmume_footer() {
        let mut file = vec![0x34; 8 * KIB];
        file.extend_from_slice(DESMUME_FOOTER);
        file.extend_from_slice(&[0; 24]);
        file.extend_from_slice(b"|-DESMUME SAVE-|");
        let backup = Backup::new(&rom_with_code(b"ZZZE"), Some(&file));
        assert_eq!(backup.data(), vec![0x34; 8 * KIB]);
    }

    #[test]
    fn flash_round_trips_without_infrared() {
        let mut backup = Backup::new(&rom_with_code(b"CPUE"), None);
        command(&mut backup, &[0x06]);
        command(&mut backup, &[0x0A, 0x07, 0xFF, 0x00, 1, 2, 3]);
        assert!(backup.take_changed());
        assert_eq!(
            command(&mut backup, &[0x03, 0x07, 0xFF, 0x00, 0, 0, 0])[4..],
            [1, 2, 3]
        );
        command(&mut backup, &[0x06]);
        command(&mut backup, &[0xDB, 0x07, 0xFF, 0x00]);
        assert_eq!(command(&mut backup, &[0x03, 0x07, 0xFF, 0x00, 0])[4], 0xFF);
        assert_eq!(
            command(&mut backup, &[0x9F, 0, 0, 0])[1..],
            [0x20, 0x40, 0x13]
        );
    }

    #[test]
    fn flash_round_trips_behind_the_infrared_controller() {
        let mut backup = Backup::new(&rom_with_code(b"IRAO"), None);
        assert!(backup.infrared.is_some());
        assert_eq!(command(&mut backup, &[IR_VERSION, 0]), [0, IR_CHIP_VERSION]);
        command(&mut backup, &[IR_SAVE, 0x06]);
        command(&mut backup, &[IR_SAVE, 0x02, 0x00, 0x01, 0x00, 0x5A, 0xA5]);
        assert_eq!(
            command(&mut backup, &[IR_SAVE, 0x03, 0x00, 0x01, 0x00, 0, 0])[5..],
            [0x5A, 0xA5]
        );
        // Without the prefix, the flash never sees the instruction.
        command(&mut backup, &[0x06]);
        command(&mut backup, &[0x02, 0x00, 0x01, 0x00, 0x00]);
        assert_eq!(
            command(&mut backup, &[IR_SAVE, 0x03, 0x00, 0x01, 0x00, 0])[5],
            0x5A
        );
        command(&mut backup, &[IR_SAVE, 0x06]);
        command(&mut backup, &[IR_SAVE, 0xD8, 0x00, 0x00, 0x00]);
        assert_eq!(
            command(&mut backup, &[IR_SAVE, 0x03, 0x00, 0x01, 0x00, 0])[5],
            0xFF
        );
    }
}
