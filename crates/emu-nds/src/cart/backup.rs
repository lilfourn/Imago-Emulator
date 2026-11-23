//! The cartridge's save chip on the AUXSPI bus (GBATEK, "DS Cartridge
//! Backup", "DS Cart Infrared Cartridge SPI Commands").
//!
//! Nothing in the header says which chip a game has. A table of game codes
//! decides for the games in it; otherwise a `.sav` file's size; otherwise
//! the game's first instructions to the chip (see [`Detector`]). See
//! [`chip_kind`] for how the first two combine.
//!
//! Games with an infrared port (game codes starting with `I`, such as
//! Pokémon HeartGold and Black) put an infrared controller in front of the
//! chip. Each transfer starts with a byte for the controller: 00h passes the
//! rest through to the save chip, 08h asks for its version (AAh), and the
//! infrared commands find nothing to receive.

use emu_state::payload;
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

/// The type for a game whose first write does not show how long its
/// addresses are: a 64 KiB EEPROM, the most common type.
const DEFAULT: Kind = Kind::Eeprom(64 * KIB);

/// The size for a game whose instructions show flash: the most common
/// size, which also holds a 256 KiB part's addresses.
const FLASH_SIZE: usize = 512 * KIB;

/// Save types by game code (header 0x0C, without the region letter). Add a
/// game here when detection picks the wrong chip for it.
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

/// Save chip instructions that tell the types apart.
const WRITE_STATUS: u8 = 0x01;
const WRITE: u8 = 0x02;
const READ: u8 = 0x03;
const WRITE_DISABLE: u8 = 0x04;
const READ_STATUS: u8 = 0x05;
const WRITE_ENABLE: u8 = 0x06;
/// Writes the upper 256 bytes of a 512 B EEPROM, or a page of flash.
const WRITE_HIGH_OR_PAGE: u8 = 0x0A;
const READ_ID: u8 = 0x9F;
const SECTOR_ERASE: u8 = 0xD8;
const PAGE_ERASE: u8 = 0xDB;

/// What the data line reads while no chip drives it, and what an erased
/// byte reads.
const IDLE: u8 = 0xFF;

/// The most bytes of one instruction an unknown chip keeps to replay:
/// three of address and as many of data as the largest chip a write runs
/// on through holds, 128 KiB (an EEPROM or FRAM here has no pages: see
/// `eeprom.rs`). Flash takes a page of 256 at a time.
const WRITTEN_MOST: usize = 3 + 128 * 1024;

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

/// The smallest chip that holds a save of `length` bytes. For a save
/// longer than the largest chip there is, that is a flash chip that never
/// was, of the next power of two: the game finds no save it knows in it,
/// but the file, which is not one of this game's, is written back whole.
fn from_save_size(length: usize) -> Option<Kind> {
    if length == 0 {
        return None;
    }
    let larger = Kind::Flash(length.next_power_of_two());
    Some(
        SIZES
            .into_iter()
            .find(|&kind| size(kind) >= length)
            .unwrap_or(larger),
    )
}

fn listed(rom: &[u8]) -> Option<Kind> {
    let code = game_code(rom)?;
    GAMES
        .iter()
        .find(|(game, _)| game.as_slice() == &code[..3])
        .map(|&(_, kind)| kind)
}

/// The chip for `rom` given a save of `length` bytes (0 for none), or
/// `None` if neither the table nor the save says.
///
/// A game in the table gets its listed chip whenever the save fits in it.
/// A short or stray `.sav` (say 512 bytes for a 512 KiB flash game) would
/// otherwise turn the chip into a tiny EEPROM that the game cannot use;
/// instead the save fills the start of the chip and the rest reads as
/// erased. melonDS also lets its game list win, but it truncates a save
/// that is too big. Here a save too big for the listed chip gets the
/// smallest chip that holds all of it ([`from_save_size`]), whatever its
/// length, so no byte of the file is dropped when it is written back.
/// Other games go by the save's size.
fn chip_kind(rom: &[u8], length: usize) -> Option<Kind> {
    match listed(rom) {
        Some(kind) if length <= size(kind) => Some(kind),
        listed => from_save_size(length).or(listed),
    }
}

/// How many of the `count` bytes after a read or write instruction were
/// its address, by DeSmuME's rule: a game's first access usually moves a
/// single byte, and a longer one a multiple of four bytes.
fn address_bytes(count: u32) -> Option<u32> {
    match count {
        0 | 1 => None,
        2..=4 => Some(count - 1),
        _ => Some(count % 4).filter(|&bytes| bytes != 0),
    }
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
    /// Until the game shows which type it expects.
    Unknown(Detector),
}

impl Chip {
    /// A chip of `kind` holding `data`, a raw save, from its start; the
    /// rest reads as erased.
    fn new(kind: Kind, data: &[u8]) -> Self {
        match kind {
            Kind::Eeprom(size) => {
                let mut eeprom = Eeprom::new(size);
                eeprom.load(data);
                Self::Eeprom(eeprom)
            }
            Kind::Flash(size) => {
                let mut memory = vec![IDLE; size];
                let length = data.len().min(size);
                memory[..length].copy_from_slice(&data[..length]);
                Self::Flash(Flash::new(memory, flash_id(size)))
            }
        }
    }

    /// Shifts one byte in and returns the byte shifted out.
    fn transfer(&mut self, input: u8) -> u8 {
        match self {
            Self::Eeprom(eeprom) => eeprom.transfer(input),
            Self::Flash(flash) => flash.transfer(input),
            Self::Unknown(detector) => detector.transfer(input),
        }
    }

    /// Chip select goes high. An unknown chip becomes the type the
    /// instruction showed, if it showed one.
    fn release(&mut self) {
        match self {
            Self::Eeprom(eeprom) => eeprom.release(),
            Self::Flash(flash) => flash.release(),
            Self::Unknown(detector) => {
                if let Some(chip) = detector.release() {
                    *self = chip;
                }
            }
        }
    }
}

/// A blank chip of a type not known yet, standing in until the game's
/// instructions show which type it expects, as in DeSmuME. Reads return
/// erased bytes, as a blank chip of any type would; the instruction that
/// shows the type is replayed into a chip of it.
///
/// - Reading the ID (9Fh) or erasing (D8h, DBh) shows flash. The ID read
///   answers as the flash picked here would.
/// - The first read whose byte count shows its address length (see
///   [`address_bytes`]) shows a 512 B EEPROM with one byte, a 64 KiB one
///   (which serves 8 KiB parts too) with two, and with three a 128 KiB
///   EEPROM or flash, which the first write tells apart.
/// - Writing (02h) shows EEPROM, and 0Ah the upper half of a 512 B EEPROM
///   or a flash page. Without a read first, the write's own byte count
///   gives the address length.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Detector {
    write_enabled: bool,
    /// Address bytes the first read that showed them had.
    address_bytes: Option<u32>,
    /// The instruction since chip select, if one arrived.
    command: Option<u8>,
    /// Bytes received after the instruction.
    count: u32,
    /// The first [`WRITTEN_MOST`] of those bytes, for an instruction that
    /// changes the chip.
    #[serde(with = "payload::bytes")]
    written: Vec<u8>,
}

impl Detector {
    /// Shifts one byte in and returns the byte shifted out.
    fn transfer(&mut self, input: u8) -> u8 {
        let Some(command) = self.command else {
            match input {
                WRITE_ENABLE => self.write_enabled = true,
                WRITE_DISABLE => self.write_enabled = false,
                _ => {}
            }
            self.command = Some(input);
            return IDLE;
        };
        // A game may hold chip select and clock on for as long as it
        // likes, and a save state hold any count.
        self.count = self.count.saturating_add(1);
        match command {
            READ_STATUS => u8::from(self.write_enabled) << 1,
            READ_ID => flash_id(FLASH_SIZE)
                .get((self.count as usize).wrapping_sub(1))
                .copied()
                .unwrap_or(IDLE),
            WRITE | WRITE_HIGH_OR_PAGE | SECTOR_ERASE | PAGE_ERASE => {
                if self.written.len() < WRITTEN_MOST {
                    self.written.push(input);
                }
                IDLE
            }
            _ => IDLE,
        }
    }

    /// Chip select goes high: the instruction ends. Returns a chip of the
    /// type it showed, with it replayed, if it showed one.
    fn release(&mut self) -> Option<Chip> {
        if self.command == Some(READ) && self.address_bytes.is_none() {
            self.address_bytes = address_bytes(self.count);
        }
        let chip = self.kind().map(|kind| self.replay(kind));
        if matches!(
            self.command,
            Some(WRITE_STATUS | WRITE | WRITE_HIGH_OR_PAGE | SECTOR_ERASE | PAGE_ERASE)
        ) {
            self.write_enabled = false;
        }
        self.command = None;
        self.count = 0;
        self.written.clear();
        chip
    }

    /// The type the instruction since chip select shows, if it shows one.
    fn kind(&self) -> Option<Kind> {
        let address_length = self.address_bytes.or_else(|| address_bytes(self.count));
        match self.command? {
            READ_ID | SECTOR_ERASE | PAGE_ERASE => Some(Kind::Flash(FLASH_SIZE)),
            READ => match self.address_bytes? {
                1 => Some(Kind::Eeprom(512)),
                2 => Some(DEFAULT),
                // 128 KiB EEPROM or flash: the first write tells.
                _ => None,
            },
            // Nothing was written.
            WRITE | WRITE_HIGH_OR_PAGE if self.count < 2 => None,
            WRITE => Some(match address_length {
                Some(1) => Kind::Eeprom(512),
                Some(3) => Kind::Eeprom(128 * KIB),
                _ => DEFAULT,
            }),
            WRITE_HIGH_OR_PAGE if address_length == Some(1) => Some(Kind::Eeprom(512)),
            WRITE_HIGH_OR_PAGE => Some(Kind::Flash(FLASH_SIZE)),
            _ => None,
        }
    }

    /// A blank chip of `kind` that has seen the write-enable latch's state
    /// and the instruction since chip select.
    fn replay(&self, kind: Kind) -> Chip {
        let mut chip = Chip::new(kind, &[]);
        if self.write_enabled {
            chip.transfer(WRITE_ENABLE);
            chip.release();
        }
        if let Some(command) = self.command {
            chip.transfer(command);
            for &byte in &self.written {
                chip.transfer(byte);
            }
            chip.release();
        }
        chip
    }
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
        let data = save.map(raw_save).unwrap_or_default();
        let chip = chip_kind(rom, data.len()).map_or_else(
            || Chip::Unknown(Detector::default()),
            |kind| Chip::new(kind, data),
        );
        Self {
            chip,
            infrared: has_infrared(rom).then(Infrared::default),
        }
    }

    /// Whether this chip, read from a save state, has memory of a size
    /// its addresses can wrap in.
    pub(crate) fn in_range(&self) -> bool {
        match &self.chip {
            Chip::Eeprom(eeprom) => eeprom.in_range(),
            Chip::Flash(flash) => flash.in_range(),
            Chip::Unknown(_) => true,
        }
    }

    /// The `.sav` file contents: the raw chip memory, or nothing while the
    /// type is unknown.
    pub(crate) fn data(&self) -> &[u8] {
        match &self.chip {
            Chip::Eeprom(eeprom) => eeprom.memory(),
            Chip::Flash(flash) => flash.memory(),
            Chip::Unknown(_) => &[],
        }
    }

    /// Whether a write changed the save since the last call. A write that
    /// shows an unknown chip's type counts when chip select goes high.
    pub(crate) fn take_changed(&mut self) -> bool {
        match &mut self.chip {
            Chip::Eeprom(eeprom) => eeprom.take_changed(),
            Chip::Flash(flash) => flash.take_changed(),
            Chip::Unknown(_) => false,
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
        infrared.count = infrared.count.saturating_add(1);
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
        self.chip.release();
    }

    fn chip_transfer(&mut self, input: u8) -> u8 {
        self.chip.transfer(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a chip shifts out while it takes an instruction: nothing drives
    /// the line, which reads high.
    const IDLE: u8 = 0xFF;

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
    fn picks_the_chip_from_the_game_code_then_the_save_size() {
        let pokemon = rom_with_code(b"ADAE");
        let backup = Backup::new(&pokemon, None);
        assert!(matches!(backup.chip, Chip::Flash(_)));
        assert_eq!(backup.data().len(), 512 * KIB);
        assert!(backup.infrared.is_none());

        let unknown = rom_with_code(b"ZZZE");
        let backup = Backup::new(&unknown, None);
        assert!(matches!(backup.chip, Chip::Unknown(_)));
        assert!(backup.data().is_empty(), "no save until the type shows");
        let backup = Backup::new(&unknown, Some(&[0x12; 0x200]));
        assert!(matches!(backup.chip, Chip::Eeprom(_)));
        assert_eq!(backup.data(), [0x12; 0x200]);
        assert_eq!(from_save_size(300 * KIB), Some(Kind::Flash(512 * KIB)));
    }

    #[test]
    fn an_unknown_games_first_read_shows_a_512_byte_eeprom() {
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        // The status register and its latch work before the type shows.
        assert_eq!(command(&mut backup, &[0x05, 0]), [IDLE, 0]);
        command(&mut backup, &[0x06]);
        assert_eq!(command(&mut backup, &[0x05, 0]), [IDLE, 2]);
        // One address byte, then one data byte.
        assert_eq!(command(&mut backup, &[0x03, 0x10, 0]), [IDLE; 3]);
        assert!(matches!(backup.chip, Chip::Eeprom(_)));
        assert_eq!(backup.data().len(), 512);
        // The latch carried over, and 0Ah writes the upper half.
        command(&mut backup, &[0x0A, 0x10, 0xAB]);
        assert!(backup.take_changed());
        assert_eq!(backup.data()[0x110], 0xAB);
    }

    #[test]
    fn an_unknown_games_first_read_shows_a_64_kib_eeprom() {
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        command(&mut backup, &[0x03, 0x00, 0x00, 0]);
        assert!(matches!(backup.chip, Chip::Eeprom(_)));
        assert_eq!(backup.data().len(), 64 * KIB);
    }

    #[test]
    fn an_unknown_flash_game_shows_by_its_first_page_write() {
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        // Three address bytes: 128 KiB EEPROM or flash, so still unknown.
        command(&mut backup, &[0x03, 0x00, 0x01, 0x00, 0]);
        assert!(matches!(backup.chip, Chip::Unknown(_)));
        command(&mut backup, &[0x06]);
        command(&mut backup, &[0x0A, 0x00, 0x01, 0x00, 1, 2, 3, 4]);
        assert!(matches!(backup.chip, Chip::Flash(_)));
        assert!(backup.take_changed());
        assert_eq!(backup.data().len(), 512 * KIB);
        assert_eq!(backup.data()[0x100..0x104], [1, 2, 3, 4]);
    }

    #[test]
    fn a_three_byte_address_eeprom_write_shows_128_kib() {
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        command(&mut backup, &[0x03, 0x00, 0x01, 0x00, 0]);
        command(&mut backup, &[0x06]);
        command(&mut backup, &[0x02, 0x01, 0x00, 0x02, 0x77]);
        assert!(matches!(backup.chip, Chip::Eeprom(_)));
        assert_eq!(backup.data().len(), 128 * KIB);
        assert_eq!(backup.data()[0x1_0002], 0x77);
    }

    #[test]
    fn flash_only_instructions_show_flash() {
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        assert_eq!(
            command(&mut backup, &[0x9F, 0, 0, 0]),
            [IDLE, 0x20, 0x40, 0x13]
        );
        assert!(matches!(backup.chip, Chip::Flash(_)));

        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        command(&mut backup, &[0x06]);
        command(&mut backup, &[0xD8, 0x00, 0x00, 0x00]);
        assert!(matches!(backup.chip, Chip::Flash(_)));
        assert_eq!(
            command(&mut backup, &[0x05, 0]),
            [IDLE, 0],
            "erase used the latch"
        );
    }

    #[test]
    fn a_write_before_any_read_goes_by_its_own_length() {
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        command(&mut backup, &[0x06]);
        // One address byte and 16 data bytes.
        let mut write = vec![0x02, 0x20];
        write.extend_from_slice(&[0x5A; 16]);
        command(&mut backup, &write);
        assert_eq!(backup.data().len(), 512);
        assert_eq!(backup.data()[0x20..0x30], [0x5A; 16]);
    }

    /// Bytes clocked into an unknown chip were all kept for as long as
    /// chip select was held, megabytes of them, in every save state and
    /// rewind snapshot. No chip a write runs on through holds more than
    /// 128 KiB.
    #[test]
    fn an_unknown_chip_keeps_no_more_of_an_instruction_than_a_chip_holds() {
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        command(&mut backup, &[0x06]);
        backup.transfer(0x0A);
        for byte in 0..300_000u32 {
            backup.transfer(byte as u8);
        }
        // A state saved while select is still held.
        let state = postcard::to_stdvec(&backup).expect("backup state serializes");
        assert!(state.len() < 132 * KIB, "{} bytes", state.len());

        // A write longer than a page, as a chip without pages takes, is
        // replayed whole into the chip it shows: none of it is dropped.
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        command(&mut backup, &[0x03, 0x00, 0x01, 0x00, 0]);
        command(&mut backup, &[0x06]);
        let long: Vec<u8> = (0..1000u32).map(|byte| (byte % 251) as u8 + 1).collect();
        let mut write = vec![0x02, 0x00, 0x00, 0x10];
        write.extend_from_slice(&long);
        command(&mut backup, &write);
        assert_eq!(backup.data().len(), 128 * KIB);
        assert!(backup.data()[0x10..0x10 + long.len()] == long[..]);

        // A whole page, the longest write a chip takes, is kept whole:
        // three address bytes and 256 of data.
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        command(&mut backup, &[0x06]);
        let mut write = vec![0x0A, 0x00, 0x02, 0x00];
        write.extend((0..=255u8).rev());
        command(&mut backup, &write);
        assert!(matches!(backup.chip, Chip::Flash(_)));
        let page: Vec<u8> = (0..=255u8).rev().collect();
        assert!(backup.data()[0x200..0x300] == page[..]);
    }

    #[test]
    fn detection_survives_a_save_state() {
        let mut backup = Backup::new(&rom_with_code(b"ZZZE"), None);
        command(&mut backup, &[0x03, 0x00, 0x01, 0x00, 0]);
        command(&mut backup, &[0x06]);
        let state = postcard::to_stdvec(&backup).expect("backup state serializes");
        let mut loaded: Backup = postcard::from_bytes(&state).expect("backup state deserializes");
        // The three-byte addresses and the latch carried over.
        command(&mut loaded, &[0x02, 0x00, 0x00, 0x05, 0x42]);
        assert_eq!(loaded.data().len(), 128 * KIB);
        assert_eq!(loaded.data()[5], 0x42);
    }

    #[test]
    fn detection_works_behind_the_infrared_controller() {
        let mut backup = Backup::new(&rom_with_code(b"IZZE"), None);
        command(&mut backup, &[IR_SAVE, 0x06]);
        command(&mut backup, &[IR_SAVE, 0x0A, 0x00, 0x00, 0x10, 0x99]);
        assert!(matches!(backup.chip, Chip::Flash(_)));
        assert_eq!(backup.data()[0x10], 0x99);
    }

    #[test]
    fn a_short_save_keeps_a_listed_games_chip() {
        let backup = Backup::new(&rom_with_code(b"ADAE"), Some(&[0x12; 0x200]));
        assert!(matches!(backup.chip, Chip::Flash(_)));
        assert_eq!(backup.data().len(), 512 * KIB);
        assert_eq!(backup.data()[..0x200], [0x12; 0x200]);
        assert!(backup.data()[0x200..].iter().all(|&byte| byte == 0xFF));
    }

    #[test]
    fn a_save_too_big_for_a_listed_games_chip_is_kept_whole() {
        let file = vec![0x34; 128 * KIB];
        let backup = Backup::new(&rom_with_code(b"ASME"), Some(&file));
        assert!(matches!(backup.chip, Chip::Eeprom(_)));
        assert_eq!(backup.data(), file, "not cut to the listed 8 KiB");
    }

    /// A save longer than the largest chip there is, 8 MiB, matched no
    /// size: a game in the table had it cut to its listed chip, and any
    /// other got a blank chip of a type not known yet, whose first write
    /// replaced the file.
    #[test]
    fn a_save_longer_than_any_chip_is_kept_whole() {
        let mut file = vec![0x34; 9 * 1024 * KIB];
        file[0] = 0x12;
        *file.last_mut().expect("not empty") = 0x56;
        // A game not in the table, and one listed with an 8 KiB EEPROM.
        for code in [b"ZZZE", b"ASME"] {
            let backup = Backup::new(&rom_with_code(code), Some(&file));
            assert!(
                matches!(backup.chip, Chip::Flash(_)),
                "a chip that holds it"
            );
            let data = backup.data();
            assert_eq!(data.len(), 16 * 1024 * KIB);
            assert!(data[..file.len()] == file[..], "every byte of the file");
        }
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
        assert_eq!(command(&mut backup, &[IR_VERSION, 0]), [0, 0xAA]);
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
