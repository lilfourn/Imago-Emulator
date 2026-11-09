//! The DS cartridge: ROM transfers and the save chip (GBATEK, "DS
//! Cartridge I/O Ports", "DS Cartridge Protocol", "DS Cartridge Backup").
//!
//! Both CPUs see the same registers; EXMEMCNT picks which one owns them.
//! AUXSPICNT (0x040001A0) and AUXSPIDATA (0x040001A2) drive the save chip's
//! SPI bus. ROMCTRL (0x040001A4) sends the 8-byte command at
//! 0x040001A8-0x040001AF, and the reply comes a word at a time through
//! 0x04100010.
//!
//! The cartridge bus clock is the system clock divided by 5 or, with
//! ROMCTRL bit 27, by 8, and moves one byte per clock. A transfer spends 8
//! clocks on the command and the gap1 clocks of ROMCTRL bits 0-12; then
//! each word takes 4 clocks, with gap2 (bits 16-21) between 200h-byte
//! blocks. A word waits in the data port, with ROMCTRL bit 23 set and the
//! cartridge DMA requested, until something reads it; the next word starts
//! only then. This module schedules nothing: register accesses return a
//! [`RomEvent`] saying what the machine must do and when.

mod backup;
mod eeprom;
mod rom;

use serde::{Deserialize, Serialize};

use backup::Backup;
use rom::Rom;

// AUXSPICNT bits.
const SPI_BAUD: u16 = 0x0003;
const SPI_HOLD: u16 = 0x0040;
const SPI_BUSY: u16 = 0x0080;
const SPI_MODE: u16 = 0x2000;
const TRANSFER_IRQ: u16 = 0x4000;
const SLOT_ENABLE: u16 = 0x8000;
const AUXSPICNT_WRITABLE: u16 = SPI_BAUD | SPI_HOLD | SPI_MODE | TRANSFER_IRQ | SLOT_ENABLE;

// ROMCTRL bits.
const GAP1: u32 = 0x1FFF;
const SEED_APPLY: u32 = 1 << 15;
const GAP2_SHIFT: u32 = 16;
const GAP2: u32 = 0x3F << GAP2_SHIFT;
const WORD_READY: u32 = 1 << 23;
const BLOCK_SIZE_SHIFT: u32 = 24;
const SLOW_CLOCK: u32 = 1 << 27;
const RELEASE_RESET: u32 = 1 << 29;
const BUSY: u32 = 1 << 31;

/// Bus clocks to send a command, and to move one word.
const COMMAND_CLOCKS: u64 = 8;
const WORD_CLOCKS: u64 = 4;
/// The second gap comes between blocks of this many bytes.
const GAP2_BLOCK: u32 = 0x200;

/// System cycles per bit on an SPI bus at baud rate 0 (4 MHz); each step
/// of the rate doubles it.
const SPI_CYCLES_PER_BIT: u64 = 8;

/// What the machine must do after a ROM transfer register access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RomEvent {
    None,
    /// Call [`Cart::rom_step`] after this many cycles.
    StepAfter(u64),
    /// A word waits in the data port: start DMA channels in cartridge mode.
    WordReady,
    /// The transfer is over; raise IF bit 19 if `irq`.
    Finished {
        irq: bool,
    },
}

/// A ROM command in progress.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Transfer {
    command: [u8; 8],
    /// Reply bytes in all.
    length: u32,
    /// Reply bytes read so far.
    position: u32,
    /// The next word waits in the data port.
    ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Cart {
    rom: Rom,
    backup: Backup,
    /// Counts writes that changed the save.
    generation: u64,
    /// AUXSPICNT as written.
    spi_control: u16,
    spi_busy: bool,
    /// The byte the last AUXSPI transfer received.
    spi_data: u8,
    /// ROMCTRL settings as written, without the status bits.
    rom_control: u32,
    command: [u8; 8],
    transfer: Option<Transfer>,
    /// The word the data port shows.
    data: u32,
}

impl Cart {
    /// A cartridge holding `rom`, with its save chip holding `save` (a
    /// `.sav` file) if given.
    pub(crate) fn new(rom: Box<[u8]>, save: Option<&[u8]>) -> Self {
        Self {
            backup: Backup::new(&rom, save),
            rom: Rom::new(rom),
            generation: 0,
            spi_control: 0,
            spi_busy: false,
            spi_data: 0,
            rom_control: 0,
            command: [0; 8],
            transfer: None,
            data: 0,
        }
    }

    /// Moves the ROM out of `other`, which is about to be replaced by this
    /// cartridge from a loaded save state. Counts the swap as a save write,
    /// since the state may hold a different save than was last flushed.
    pub(crate) fn take_rom_from(&mut self, other: &mut Self) {
        self.rom.take_data_from(&mut other.rom);
        self.generation = other.generation.wrapping_add(1);
    }

    /// Whether this card, read from a save state, has a save chip with
    /// memory its addresses can wrap in, and a ROM reply no further on
    /// than it is long.
    pub(crate) fn in_range(&self) -> bool {
        // The longest reply ROMCTRL can ask for.
        let transfer = self.transfer.is_none_or(|transfer| {
            transfer.length <= 0x4000 && transfer.position <= transfer.length
        });
        self.backup.in_range() && transfer
    }

    /// The ROM image, for direct boot to load the header and binaries.
    pub(crate) fn rom(&self) -> &[u8] {
        self.rom.data()
    }

    /// The 4-byte chip ID as the data port returns it, first byte lowest.
    /// Direct boot stores it where the BIOS would.
    pub(crate) fn chip_id(&self) -> u32 {
        self.rom.chip_id()
    }

    pub(crate) fn backup_generation(&self) -> Option<u64> {
        Some(self.generation)
    }

    /// Appends the `.sav` contents: the raw save chip memory.
    pub(crate) fn save_backup(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.backup.data());
    }

    /// Reads AUXSPICNT (0x040001A0).
    pub(crate) fn spi_control(&self) -> u16 {
        self.spi_control | if self.spi_busy { SPI_BUSY } else { 0 }
    }

    /// Writes AUXSPICNT (0x040001A0). Leaving SPI mode, or disabling the
    /// slot, releases the save chip.
    pub(crate) fn write_spi_control(&mut self, value: u16) {
        self.spi_control = value & AUXSPICNT_WRITABLE;
        if value & SPI_MODE == 0 || value & SLOT_ENABLE == 0 {
            self.backup.release();
            self.count_backup_change();
        }
    }

    /// Reads AUXSPIDATA (0x040001A2): the byte the last transfer received.
    pub(crate) fn spi_data(&self) -> u8 {
        self.spi_data
    }

    /// Writes AUXSPIDATA (0x040001A2), which in SPI mode exchanges a byte
    /// with the save chip. Returns the cycles AUXSPICNT stays busy, after
    /// which the machine calls [`Cart::finish_spi`]; this bus raises no
    /// interrupt. Without the hold bit, the chip is released afterwards.
    pub(crate) fn write_spi_data(&mut self, value: u8) -> Option<u64> {
        if self.spi_control & SPI_MODE == 0 {
            return None;
        }
        self.spi_data = self.backup.transfer(value);
        if self.spi_control & SPI_HOLD == 0 {
            self.backup.release();
        }
        self.count_backup_change();
        self.spi_busy = true;
        Some((8 * SPI_CYCLES_PER_BIT) << (self.spi_control & SPI_BAUD))
    }

    /// Counts a save write if the save chip changed. Checked after a
    /// release too: the write that shows an unknown chip's type lands then.
    fn count_backup_change(&mut self) {
        if self.backup.take_changed() {
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// The AUXSPI transfer ends.
    pub(crate) fn finish_spi(&mut self) {
        self.spi_busy = false;
    }

    /// Reads ROMCTRL (0x040001A4).
    pub(crate) fn rom_control(&self) -> u32 {
        let ready = self.transfer.is_some_and(|transfer| transfer.ready);
        self.rom_control
            | if ready { WORD_READY } else { 0 }
            | if self.transfer.is_some() { BUSY } else { 0 }
    }

    /// Writes ROMCTRL (0x040001A4). Bit 31 sends the command unless a
    /// transfer is already running.
    pub(crate) fn write_rom_control(&mut self, value: u32) -> RomEvent {
        // The reset line, once released, stays released.
        let released = self.rom_control & RELEASE_RESET;
        self.rom_control = (value & !(SEED_APPLY | WORD_READY | BUSY)) | released;
        if value & BUSY == 0 || self.transfer.is_some() {
            return RomEvent::None;
        }
        let length = match (value >> BLOCK_SIZE_SHIFT) & 7 {
            0 => 0,
            7 => 4,
            size => 0x100 << size,
        };
        self.transfer = Some(Transfer {
            command: self.command,
            length,
            position: 0,
            ready: false,
        });
        let first_word = if length == 0 { 0 } else { WORD_CLOCKS };
        let gap1 = u64::from(self.rom_control & GAP1);
        RomEvent::StepAfter(self.cycles(COMMAND_CLOCKS + gap1 + first_word))
    }

    /// Writes command byte `index` (0x040001A8 + `index`); byte 0 goes out
    /// first.
    pub(crate) fn write_command(&mut self, index: usize, value: u8) {
        self.command[index & 7] = value;
    }

    /// The time [`RomEvent::StepAfter`] asked for has come: the next word
    /// arrives, or a transfer without data ends.
    pub(crate) fn rom_step(&mut self) -> RomEvent {
        let Some(transfer) = &mut self.transfer else {
            return RomEvent::None;
        };
        if transfer.position < transfer.length {
            transfer.ready = true;
            return RomEvent::WordReady;
        }
        self.transfer = None;
        RomEvent::Finished {
            irq: self.spi_control & TRANSFER_IRQ != 0,
        }
    }

    /// Reads the data port (0x04100010). Taking a ready word starts the
    /// next one, or ends the transfer after the last; with no word ready,
    /// the port repeats the last one.
    pub(crate) fn read_data(&mut self) -> (u32, RomEvent) {
        let Some(transfer) = &mut self.transfer else {
            return (self.data, RomEvent::None);
        };
        if !transfer.ready {
            return (self.data, RomEvent::None);
        }
        let bytes = [0, 1, 2, 3].map(|i| self.rom.reply(&transfer.command, transfer.position + i));
        self.data = u32::from_le_bytes(bytes);
        transfer.ready = false;
        transfer.position += 4;
        if transfer.position >= transfer.length {
            self.transfer = None;
            let irq = self.spi_control & TRANSFER_IRQ != 0;
            return (self.data, RomEvent::Finished { irq });
        }
        let gap2 = if transfer.position % GAP2_BLOCK == 0 {
            u64::from((self.rom_control & GAP2) >> GAP2_SHIFT)
        } else {
            0
        };
        (
            self.data,
            RomEvent::StepAfter(self.cycles(WORD_CLOCKS + gap2)),
        )
    }

    /// System cycles for `clocks` cartridge bus clocks.
    fn cycles(&self, clocks: u64) -> u64 {
        let divider = if self.rom_control & SLOW_CLOCK != 0 {
            8
        } else {
            5
        };
        clocks * divider
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom() -> Box<[u8]> {
        let mut rom: Vec<u8> = (0..0x2_0000u32).map(|i| (i * 7 + (i >> 9)) as u8).collect();
        rom[0x0C..0x10].copy_from_slice(b"ADAE");
        // An NDS-only cartridge of the smallest capacity.
        rom[0x12] = 0;
        rom[0x14] = 0;
        rom.into_boxed_slice()
    }

    fn send(cart: &mut Cart, command: [u8; 8], control: u32) -> RomEvent {
        for (index, &byte) in command.iter().enumerate() {
            cart.write_command(index, byte);
        }
        cart.write_rom_control(control)
    }

    /// Runs a transfer to the end, reading every word as DMA would.
    fn receive(cart: &mut Cart, command: [u8; 8], control: u32) -> (Vec<u8>, bool) {
        let mut event = send(cart, command, control);
        let mut bytes = Vec::new();
        loop {
            match event {
                RomEvent::StepAfter(_) => event = cart.rom_step(),
                RomEvent::WordReady => {
                    let (word, next) = cart.read_data();
                    bytes.extend_from_slice(&word.to_le_bytes());
                    event = next;
                }
                RomEvent::Finished { irq } => return (bytes, irq),
                RomEvent::None => panic!("the transfer stalled"),
            }
        }
    }

    const START: u32 = BUSY | RELEASE_RESET;
    const BLOCK_200: u32 = 1 << BLOCK_SIZE_SHIFT;
    const BLOCK_4: u32 = 7 << BLOCK_SIZE_SHIFT;

    #[test]
    fn reads_the_header() {
        let rom = rom();
        let mut cart = Cart::new(rom.clone(), None);
        let (bytes, irq) = receive(&mut cart, [0; 8], START | BLOCK_200);
        assert_eq!(bytes, rom[..0x200]);
        assert!(!irq);
        assert_eq!(cart.rom_control() & (BUSY | WORD_READY), 0);
    }

    #[test]
    fn reads_data_blocks_with_the_secure_area_redirected() {
        let rom = rom();
        let mut cart = Cart::new(rom.clone(), None);
        cart.write_spi_control(SLOT_ENABLE | TRANSFER_IRQ);
        let read = |address: u32| {
            let [a, b, c, d] = address.to_be_bytes();
            [0xB7, a, b, c, d, 0, 0, 0]
        };
        let (bytes, irq) = receive(&mut cart, read(0x1_0000), START | BLOCK_200);
        assert_eq!(bytes, rom[0x1_0000..0x1_0200]);
        assert!(irq);
        let (bytes, _) = receive(&mut cart, read(0x4000), START | BLOCK_200);
        assert_eq!(bytes, rom[0x8000..0x8200]);
    }

    #[test]
    fn reports_the_chip_id() {
        let mut cart = Cart::new(rom(), None);
        let (bytes, _) = receive(&mut cart, [0xB8, 0, 0, 0, 0, 0, 0, 0], START | BLOCK_4);
        assert_eq!(
            u32::from_le_bytes(bytes.try_into().unwrap()),
            cart.chip_id()
        );
        assert_eq!(cart.chip_id(), 0x0000_00C2);
    }

    #[test]
    fn times_words_by_clock_rate_and_gaps() {
        let mut cart = Cart::new(rom(), None);
        // gap1 = 0x10 and gap2 = 0x18 at 6.7 MHz, 1000h bytes.
        let control = START | 4 << BLOCK_SIZE_SHIFT | 0x18 << GAP2_SHIFT | 0x10;
        assert_eq!(
            send(&mut cart, [0xB7, 0, 0, 0x80, 0, 0, 0, 0], control),
            RomEvent::StepAfter((8 + 0x10 + 4) * 5)
        );
        assert_eq!(cart.read_data().1, RomEvent::None, "no word is ready yet");
        assert_eq!(cart.rom_step(), RomEvent::WordReady);
        assert_ne!(cart.rom_control() & WORD_READY, 0);
        assert_eq!(cart.read_data().1, RomEvent::StepAfter(4 * 5));
        for _ in 1..0x7F {
            cart.rom_step();
            cart.read_data();
        }
        cart.rom_step();
        // The word ending the first 200h-byte block is followed by gap2.
        assert_eq!(cart.read_data().1, RomEvent::StepAfter((4 + 0x18) * 5));

        // A command without data ends after the command and gap1, at 4.2 MHz.
        let mut cart = Cart::new(rom(), None);
        assert_eq!(
            send(&mut cart, [0x9F; 8], START | SLOW_CLOCK | 0x20),
            RomEvent::StepAfter((8 + 0x20) * 8)
        );
        assert_eq!(cart.rom_step(), RomEvent::Finished { irq: false });
    }

    #[test]
    fn save_states_keep_a_transfer_but_not_the_rom() {
        let rom = rom();
        let mut cart = Cart::new(rom.clone(), None);
        let read = [0xB7, 0, 0, 0x90, 0, 0, 0, 0];
        send(&mut cart, read, START | BLOCK_200);
        cart.rom_step();
        let state = postcard::to_stdvec(&cart).expect("cart state serializes");
        assert!(
            state.len() < 0x10_0000,
            "the state holds the save, not the ROM"
        );
        let mut loaded: Cart = postcard::from_bytes(&state).expect("cart state deserializes");
        loaded.take_rom_from(&mut cart);
        let (word, _) = loaded.read_data();
        assert_eq!(word.to_le_bytes(), rom[0x9000..0x9004]);
    }

    #[test]
    fn saves_through_auxspi_and_counts_changes() {
        fn exchange(cart: &mut Cart, bytes: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            for (i, &byte) in bytes.iter().enumerate() {
                // Hold the chip until the last byte.
                let hold = if i + 1 < bytes.len() { SPI_HOLD } else { 0 };
                cart.write_spi_control(SLOT_ENABLE | SPI_MODE | hold);
                assert_eq!(cart.write_spi_data(byte), Some(64));
                assert_ne!(cart.spi_control() & SPI_BUSY, 0);
                cart.finish_spi();
                out.push(cart.spi_data());
            }
            out
        }
        let mut cart = Cart::new(rom(), None);
        exchange(&mut cart, &[0x06]);
        exchange(&mut cart, &[0x0A, 0x00, 0x00, 0x10, 0xAB]);
        assert_eq!(cart.backup_generation(), Some(1));
        assert_eq!(exchange(&mut cart, &[0x03, 0x00, 0x00, 0x10, 0])[4], 0xAB);
        let mut save = Vec::new();
        cart.save_backup(&mut save);
        assert_eq!((save.len(), save[0x10]), (0x8_0000, 0xAB));

        // Outside SPI mode the data register does nothing.
        cart.write_spi_control(SLOT_ENABLE);
        assert_eq!(cart.write_spi_data(0x06), None);

        let mut loaded = Cart::new(rom(), Some(&save));
        loaded.take_rom_from(&mut cart);
        assert_eq!(loaded.backup_generation(), Some(2));
        assert_eq!(loaded.rom().len(), 0x2_0000);
    }

    #[test]
    fn a_save_chip_found_by_its_first_write_counts_the_write() {
        let mut rom = rom();
        rom[0x0C..0x10].copy_from_slice(b"ZZZE");
        let mut cart = Cart::new(rom, None);
        let mut exchange = |bytes: &[u8]| {
            for (i, &byte) in bytes.iter().enumerate() {
                let hold = if i + 1 < bytes.len() { SPI_HOLD } else { 0 };
                cart.write_spi_control(SLOT_ENABLE | SPI_MODE | hold);
                cart.write_spi_data(byte);
            }
        };
        exchange(&[0x06]);
        // A flash page write; the chip select ends it with the last byte.
        exchange(&[0x0A, 0x00, 0x01, 0x00, 0x5A]);
        assert_eq!(cart.backup_generation(), Some(1));
        let mut save = Vec::new();
        cart.save_backup(&mut save);
        assert_eq!((save.len(), save[0x100]), (0x8_0000, 0x5A));
    }
}
