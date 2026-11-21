//! Cartridge save memory (GBATEK, "GBA Cart Backup IDs", "GBA Cart Backup
//! SRAM/FRAM", "GBA Cart Backup Flash ROM", "GBA Cart Backup EEPROM").
//!
//! The kind comes from a table of game codes when the game is listed,
//! otherwise from the save library's ID string in the ROM (`SRAM_V`,
//! `FLASH1M_V`, `EEPROM_V`, ...), which every game built with Nintendo's
//! SDK contains. SRAM and Flash sit on an 8-bit bus at
//! 0x0E000000; EEPROM is reached bit by bit through DMA at the top of the
//! ROM area.

use serde::{Deserialize, Serialize};

/// Flash chip IDs reported in ID mode: (manufacturer, device). Macronix
/// parts, as in most commercial carts.
const FLASH_64K_ID: (u8, u8) = (0xC2, 0x1C);
const FLASH_128K_ID: (u8, u8) = (0xC2, 0x09);

/// A save memory type and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    None,
    Sram,
    Flash64,
    Flash128,
    Eeprom,
}

/// Save types by game code (header 0xAC, without the region letter), for
/// games whose library strings mislead detection and for the games the
/// project is tested against. Add a game here when detection gets it wrong.
const OVERRIDES: [(&[u8; 3], Kind); 5] = [
    (b"AXV", Kind::Flash128), // Pokémon Ruby
    (b"AXP", Kind::Flash128), // Pokémon Sapphire
    (b"BPE", Kind::Flash128), // Pokémon Emerald
    (b"BPR", Kind::Flash128), // Pokémon FireRed
    (b"BPG", Kind::Flash128), // Pokémon LeafGreen
];

/// The save type listed for `rom`'s game code, if any.
fn listed(rom: &[u8]) -> Option<Kind> {
    let code = rom.get(0xAC..0xAF)?;
    OVERRIDES
        .iter()
        .find(|(game, _)| game.as_slice() == code)
        .map(|&(_, kind)| kind)
}

/// The save type `rom`'s library ID string asks for. Games with several
/// strings take the first in this order.
fn from_library_string(rom: &[u8]) -> Kind {
    // The strings start on word boundaries.
    let has = |id: &[u8]| rom.windows(id.len()).step_by(4).any(|window| window == id);
    if has(b"EEPROM_V") {
        Kind::Eeprom
    } else if has(b"SRAM_V") || has(b"SRAM_F_V") {
        Kind::Sram
    } else if has(b"FLASH1M_V") {
        Kind::Flash128
    } else if has(b"FLASH_V") || has(b"FLASH512_V") {
        Kind::Flash64
    } else {
        Kind::None
    }
}

/// Cartridge save memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Backup {
    None,
    Sram(Vec<u8>),
    Flash(Flash),
    Eeprom(Eeprom),
}

impl Backup {
    /// Blank save memory of the type `rom` uses.
    pub(crate) fn detect(rom: &[u8]) -> Self {
        match listed(rom).unwrap_or_else(|| from_library_string(rom)) {
            Kind::None => Self::None,
            Kind::Sram => Self::Sram(vec![0xFF; 0x8000]),
            Kind::Flash64 => Self::Flash(Flash::new(0x1_0000)),
            Kind::Flash128 => Self::Flash(Flash::new(0x2_0000)),
            Kind::Eeprom => Self::Eeprom(Eeprom::new()),
        }
    }

    /// Whether this save memory, read from a save state, is of a length
    /// its kind comes in, with its chip's state within it.
    pub(crate) fn in_range(&self) -> bool {
        match self {
            Self::None => true,
            Self::Sram(memory) => memory.len() == 0x8000,
            Self::Flash(flash) => {
                matches!(flash.memory.len(), 0x1_0000 | 0x2_0000)
                    && flash.bank < flash.memory.len() / 0x1_0000
            }
            Self::Eeprom(eeprom) => {
                // The longest request, a write to an 8 KiB part, and the
                // longest answer, a read's.
                matches!(eeprom.memory.len(), 0x200 | 0x2000)
                    && matches!(eeprom.address_bits, None | Some(6 | 14))
                    && eeprom.incoming.len() <= 2 + 14 + 64 + 1
                    && eeprom.outgoing.len() <= 4 + 64
            }
        }
    }

    /// Loads a `.sav` file's contents.
    pub(crate) fn load(&mut self, data: &[u8]) {
        let memory = match self {
            Self::None => return,
            Self::Sram(memory) => memory,
            Self::Flash(flash) => &mut flash.memory,
            Self::Eeprom(eeprom) => {
                // The file's size suggests the part: 8 KiB if it holds that
                // much (padded files included), otherwise 512 B. The game's
                // first request settles it (see `Eeprom::note_dma_length`).
                let size = if data.len() >= 0x2000 { 0x2000 } else { 0x200 };
                eeprom.memory = vec![0xFF; size];
                &mut eeprom.memory
            }
        };
        let length = memory.len().min(data.len());
        memory[..length].copy_from_slice(&data[..length]);
    }

    /// The `.sav` file contents, or `None` with no save memory.
    pub(crate) fn data(&self) -> Option<&[u8]> {
        match self {
            Self::None => None,
            Self::Sram(memory) => Some(memory),
            Self::Flash(flash) => Some(&flash.memory),
            Self::Eeprom(eeprom) => Some(&eeprom.memory),
        }
    }

    /// Reads the byte at `offset` in the 0x0E000000 region.
    pub(crate) fn read(&self, offset: u32) -> u8 {
        match self {
            Self::Sram(memory) => memory[(offset & 0x7FFF) as usize],
            Self::Flash(flash) => flash.read(offset & 0xFFFF),
            Self::None | Self::Eeprom(_) => 0xFF,
        }
    }

    /// Writes the byte at `offset` in the 0x0E000000 region. Returns whether
    /// stored data changed.
    pub(crate) fn write(&mut self, offset: u32, value: u8) -> bool {
        match self {
            Self::Sram(memory) => {
                memory[(offset & 0x7FFF) as usize] = value;
                true
            }
            Self::Flash(flash) => flash.write(offset & 0xFFFF, value),
            Self::None | Self::Eeprom(_) => false,
        }
    }
}

/// Flash command state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum FlashState {
    #[default]
    Ready,
    /// Saw 0xAA at 0x5555.
    Unlock1,
    /// Saw 0x55 at 0x2AAA: the next write to 0x5555 is a command.
    Unlock2,
    /// The next write programs one byte.
    Program,
    /// The next write to 0x0000 selects the 64 KiB bank.
    Bank,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Flash {
    memory: Vec<u8>,
    state: FlashState,
    id_mode: bool,
    /// An erase command (0x80) was given; the next unlocked command erases.
    erase_armed: bool,
    bank: usize,
}

impl Flash {
    fn new(size: usize) -> Self {
        Self {
            memory: vec![0xFF; size],
            state: FlashState::Ready,
            id_mode: false,
            erase_armed: false,
            bank: 0,
        }
    }

    fn id(&self) -> (u8, u8) {
        if self.memory.len() > 0x1_0000 {
            FLASH_128K_ID
        } else {
            FLASH_64K_ID
        }
    }

    fn index(&self, offset: u32) -> usize {
        self.bank * 0x1_0000 + offset as usize
    }

    fn read(&self, offset: u32) -> u8 {
        match (self.id_mode, offset) {
            (true, 0) => self.id().0,
            (true, 1) => self.id().1,
            _ => self.memory[self.index(offset)],
        }
    }

    fn write(&mut self, offset: u32, value: u8) -> bool {
        match self.state {
            FlashState::Program => {
                let index = self.index(offset);
                self.memory[index] = value;
                self.state = FlashState::Ready;
                return true;
            }
            FlashState::Bank => {
                if offset == 0 && self.memory.len() > 0x1_0000 {
                    self.bank = usize::from(value & 1);
                }
                self.state = FlashState::Ready;
                return false;
            }
            FlashState::Ready if offset == 0x5555 && value == 0xAA => {
                self.state = FlashState::Unlock1;
                return false;
            }
            FlashState::Unlock1 if offset == 0x2AAA && value == 0x55 => {
                self.state = FlashState::Unlock2;
                return false;
            }
            FlashState::Unlock2 => {}
            _ => {
                self.state = FlashState::Ready;
                return false;
            }
        }
        // A command after the unlock sequence.
        self.state = FlashState::Ready;
        if self.erase_armed {
            self.erase_armed = false;
            match (offset, value) {
                (0x5555, 0x10) => {
                    self.memory.fill(0xFF);
                    return true;
                }
                (_, 0x30) => {
                    // Erase the 4 KiB sector containing `offset`.
                    let start = self.index(offset & 0xF000);
                    self.memory[start..start + 0x1000].fill(0xFF);
                    return true;
                }
                _ => {}
            }
        }
        if offset == 0x5555 {
            match value {
                0x90 => self.id_mode = true,
                0xF0 => self.id_mode = false,
                0x80 => self.erase_armed = true,
                0xA0 => self.state = FlashState::Program,
                0xB0 => self.state = FlashState::Bank,
                _ => {}
            }
        }
        false
    }
}

/// EEPROM serial state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Eeprom {
    /// The part's contents: 512 B or 8 KiB, or more where a loaded save
    /// was bigger than the part turned out to be, which it keeps whole.
    memory: Vec<u8>,
    /// 6 for 512 B parts, 14 for 8 KiB; unknown until a game's DMA tells
    /// (see [`Eeprom::address_width`] for the guess until then).
    address_bits: Option<u32>,
    /// Bits received for the command in progress, oldest first.
    incoming: Vec<bool>,
    /// Bits waiting to be read out, next first.
    outgoing: std::collections::VecDeque<bool>,
}

impl Eeprom {
    fn new() -> Self {
        Self {
            memory: vec![0xFF; 0x200],
            address_bits: None,
            incoming: Vec::new(),
            outgoing: std::collections::VecDeque::new(),
        }
    }

    /// A DMA of `count` halfwords is about to talk to the EEPROM. A request
    /// is 9 or 73 bits long for 512 B parts and 17 or 81 for 8 KiB ones,
    /// which settles the size the first time, over any guess from a loaded
    /// save's size. Memory grows to fit but never shrinks, so the save's
    /// bytes are kept either way.
    pub(crate) fn note_dma_length(&mut self, count: u32) {
        if self.address_bits.is_some() {
            return;
        }
        let (address_bits, size) = match count {
            9 | 73 => (6, 0x200),
            17 | 81 => (14, 0x2000),
            _ => return,
        };
        self.address_bits = Some(address_bits);
        if self.memory.len() < size {
            self.memory.resize(size, 0xFF);
        }
    }

    /// The address width requests use: the one a DMA settled, or until
    /// then the one the memory's size suggests.
    fn address_width(&self) -> usize {
        let guess = if self.memory.len() >= 0x2000 { 14 } else { 6 };
        self.address_bits.unwrap_or(guess) as usize
    }

    /// Reads one bit (bit 0 of the halfword).
    pub(crate) fn read(&mut self) -> u16 {
        // With nothing queued the chip reports ready (1).
        self.outgoing.pop_front().map_or(1, u16::from)
    }

    /// Writes one bit (bit 0 of the halfword). Returns whether stored data
    /// changed.
    pub(crate) fn write(&mut self, value: u16) -> bool {
        self.incoming.push(value & 1 != 0);
        let address_bits = self.address_width();
        let write_length = 2 + address_bits + 64 + 1;
        let bits = &self.incoming;
        let number = |range: std::ops::Range<usize>| {
            bits[range]
                .iter()
                .fold(0usize, |value, &bit| value << 1 | usize::from(bit))
        };
        match (bits.first(), bits.get(1)) {
            // Read request: 1 1, address, 0.
            (Some(true), Some(true)) if bits.len() == 2 + address_bits + 1 => {
                let block = number(2..2 + address_bits) % (self.memory.len() / 8);
                self.outgoing = std::iter::repeat_n(false, 4)
                    .chain(
                        (0..64)
                            .map(|bit| self.memory[block * 8 + bit / 8] & (0x80 >> (bit % 8)) != 0),
                    )
                    .collect();
                self.incoming.clear();
                false
            }
            // Write request: 1 0, address, 64 data bits, 0.
            (Some(true), Some(false)) if bits.len() == write_length => {
                let block = number(2..2 + address_bits) % (self.memory.len() / 8);
                for byte in 0..8 {
                    let start = 2 + address_bits + byte * 8;
                    self.memory[block * 8 + byte] = number(start..start + 8) as u8;
                }
                self.incoming.clear();
                self.outgoing.clear();
                true
            }
            (Some(false), _) => {
                self.incoming.clear();
                false
            }
            // As long as a write request and still unmatched: garbled (by
            // the address width changing partway, say), so start afresh.
            _ if bits.len() >= write_length => {
                self.incoming.clear();
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_save_types_from_library_strings() {
        let mut rom = vec![0u8; 0x400];
        rom[0x100..0x109].copy_from_slice(b"FLASH1M_V");
        assert!(
            matches!(Backup::detect(&rom), Backup::Flash(ref flash) if flash.memory.len() == 0x2_0000)
        );
        rom[0x100..0x109].copy_from_slice(b"SRAM_V103");
        assert!(matches!(Backup::detect(&rom), Backup::Sram(_)));
        assert!(matches!(Backup::detect(&[0; 0x100]), Backup::None));
    }

    #[test]
    fn listed_game_codes_win_over_library_strings() {
        let mut rom = vec![0u8; 0x400];
        rom[0x100..0x106].copy_from_slice(b"SRAM_V");
        rom[0xAC..0xB0].copy_from_slice(b"BPEE");
        assert!(
            matches!(Backup::detect(&rom), Backup::Flash(ref flash) if flash.memory.len() == 0x2_0000)
        );
    }

    fn command(backup: &mut Backup, value: u8) {
        backup.write(0x5555, 0xAA);
        backup.write(0x2AAA, 0x55);
        backup.write(0x5555, value);
    }

    #[test]
    fn flash_reports_its_id_and_programs_bytes() {
        let mut backup = Backup::Flash(Flash::new(0x2_0000));
        command(&mut backup, 0x90);
        assert_eq!((backup.read(0), backup.read(1)), FLASH_128K_ID);
        command(&mut backup, 0xF0);
        command(&mut backup, 0xA0);
        assert!(backup.write(0x1234, 0x42));
        assert_eq!(backup.read(0x1234), 0x42);
        // Without the unlock sequence, writes do nothing.
        assert!(!backup.write(0x1234, 0x00));
        assert_eq!(backup.read(0x1234), 0x42);
    }

    #[test]
    fn flash_erases_sectors_and_switches_banks() {
        let mut backup = Backup::Flash(Flash::new(0x2_0000));
        command(&mut backup, 0xA0);
        backup.write(0x0010, 0x11);
        command(&mut backup, 0xB0);
        backup.write(0, 1);
        assert_eq!(backup.read(0x0010), 0xFF);
        command(&mut backup, 0xB0);
        backup.write(0, 0);
        command(&mut backup, 0x80);
        backup.write(0x5555, 0xAA);
        backup.write(0x2AAA, 0x55);
        backup.write(0x0000, 0x30);
        assert_eq!(backup.read(0x0010), 0xFF);
    }

    /// A `.sav` of `length` bytes, no two neighbouring blocks alike.
    fn save_file(length: usize) -> Vec<u8> {
        (0..length).map(|index| (index % 251) as u8).collect()
    }

    /// A blank EEPROM with `file` loaded.
    fn eeprom_from(file: &[u8]) -> Eeprom {
        let mut backup = Backup::Eeprom(Eeprom::new());
        backup.load(file);
        match backup {
            Backup::Eeprom(eeprom) => eeprom,
            _ => unreachable!(),
        }
    }

    /// Sends the low `width` bits of `value`, most significant first.
    fn send(eeprom: &mut Eeprom, value: usize, width: usize) {
        for bit in (0..width).rev() {
            eeprom.write(((value >> bit) & 1) as u16);
        }
    }

    /// Reads block `block` with a request of `address_bits`-bit address.
    fn read_block(eeprom: &mut Eeprom, address_bits: usize, block: usize) -> Vec<u8> {
        send(eeprom, 0b11, 2);
        send(eeprom, block, address_bits);
        send(eeprom, 0, 1);
        let bits: Vec<u8> = (0..68).map(|_| eeprom.read() as u8).collect();
        bits[4..]
            .chunks(8)
            .map(|byte| byte.iter().fold(0, |value, &bit| value << 1 | bit))
            .collect()
    }

    #[test]
    fn eeprom_keeps_a_padded_8_kib_save() {
        let file = save_file(0x1_0000);
        let mut eeprom = eeprom_from(&file);
        eeprom.note_dma_length(17);
        assert_eq!(read_block(&mut eeprom, 14, 0x3FF), file[0x1FF8..0x2000]);
        assert_eq!(eeprom.memory, file[..0x2000]);
    }

    #[test]
    fn eeprom_requests_settle_512_b_addressing_over_an_8_kib_save() {
        let file = save_file(0x2000);
        let mut eeprom = eeprom_from(&file);
        eeprom.note_dma_length(9);
        assert_eq!(read_block(&mut eeprom, 6, 5), file[40..48]);
        // The rest of the file is kept, to be saved again as it was.
        assert_eq!(eeprom.memory, file);
    }

    #[test]
    fn eeprom_requests_settle_8_kib_addressing_over_a_512_b_save() {
        let file = save_file(0x200);
        let mut eeprom = eeprom_from(&file);
        eeprom.note_dma_length(81);
        assert_eq!(read_block(&mut eeprom, 14, 63), file[0x1F8..0x200]);
        assert_eq!(read_block(&mut eeprom, 14, 64), [0xFF; 8]);
        assert_eq!(eeprom.memory.len(), 0x2000);
    }

    #[test]
    fn eeprom_drops_requests_that_outgrow_the_longest() {
        let file = save_file(0x2000);
        let mut eeprom = eeprom_from(&file);
        // Half a 14-bit read request, then the game settles on 6 bits:
        // the bits so far can no longer end a request.
        send(&mut eeprom, 0b11, 2);
        send(&mut eeprom, 0, 10);
        eeprom.note_dma_length(9);
        for _ in 0..100 {
            eeprom.write(0);
        }
        assert_eq!(read_block(&mut eeprom, 6, 5), file[40..48]);
    }
}
