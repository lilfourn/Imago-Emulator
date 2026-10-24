//! The debugger's memory editor: where it looks, the regions it can jump
//! to, typing a byte in hex, and how rows read as text (UI-12).
//!
//! This module holds that state and its rules; the debugger window draws
//! the rows, and edits go to the emulation thread as debugger commands.

use emu_core::System;

use crate::debugger::{BYTES_PER_ROW, MEMORY_ROWS};

/// A named block of memory to jump to, as the CPU addresses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Its usual name.
    pub name: &'static str,
    /// First address.
    pub start: u32,
}

const fn region(name: &'static str, start: u32) -> Region {
    Region { name, start }
}

const GB: [Region; 7] = [
    region("ROM", 0x0000),
    region("VRAM", 0x8000),
    region("Cart RAM", 0xA000),
    region("WRAM", 0xC000),
    region("OAM", 0xFE00),
    region("I/O", 0xFF00),
    region("HRAM", 0xFF80),
];

const GBA: [Region; 8] = [
    region("BIOS", 0x0000_0000),
    region("EWRAM", 0x0200_0000),
    region("IWRAM", 0x0300_0000),
    region("I/O", 0x0400_0000),
    region("Palette", 0x0500_0000),
    region("VRAM", 0x0600_0000),
    region("OAM", 0x0700_0000),
    region("ROM", 0x0800_0000),
];

const NDS: [Region; 8] = [
    region("Main RAM", 0x0200_0000),
    region("WRAM", 0x0300_0000),
    region("ARM7 WRAM", 0x0380_0000),
    region("I/O", 0x0400_0000),
    region("Palette", 0x0500_0000),
    region("VRAM", 0x0600_0000),
    region("OAM", 0x0700_0000),
    region("GBA slot", 0x0800_0000),
];

/// The regions the editor offers for `system`: none for an RPG Maker
/// game, which has no memory map.
pub fn regions(system: System) -> &'static [Region] {
    match system {
        System::Gb => &GB,
        System::Gba => &GBA,
        System::Nds => &NDS,
        System::RpgMaker => &[],
    }
}

/// Where the editor opens: the system's work RAM.
pub fn home(system: System) -> u32 {
    match system {
        System::Gb => 0xC000,
        System::Gba | System::Nds => 0x0200_0000,
        System::RpgMaker => 0,
    }
}

/// The highest address `system`'s CPUs can name.
pub fn last_address(system: System) -> u32 {
    match system {
        System::Gb => 0xFFFF,
        System::Gba | System::Nds => u32::MAX,
        System::RpgMaker => 0,
    }
}

/// Where the editor is looking: the first row's address, and scrolling
/// not yet worth a whole row.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Viewer {
    /// The first row's address, a multiple of [`BYTES_PER_ROW`].
    pub address: u32,
    /// Scrolled distance not yet worth a whole row, in rows.
    remainder: f32,
}

impl Viewer {
    /// Shows `address` on the first row, keeping the rows inside an
    /// address space that ends at `last`.
    pub fn go_to(&mut self, address: u32, last: u32) {
        let last_top = last_top_row(last);
        self.address = (address & !(BYTES_PER_ROW - 1)).min(last_top);
        self.remainder = 0.0;
    }

    /// Scrolls by `rows` (fractions accumulate) inside an address space
    /// that ends at `last`.
    pub fn scroll(&mut self, rows: f32, last: u32) {
        self.remainder += rows;
        let whole = self.remainder.trunc();
        self.remainder -= whole;
        let row = i64::from(self.address / BYTES_PER_ROW) + whole as i64;
        let row = row.clamp(0, i64::from(last_top_row(last) / BYTES_PER_ROW));
        self.address = u32::try_from(row).unwrap_or(0) * BYTES_PER_ROW;
    }
}

/// The highest first-row address that keeps every row inside an address
/// space ending at `last`.
fn last_top_row(last: u32) -> u32 {
    let rows = last / BYTES_PER_ROW;
    rows.saturating_sub(MEMORY_ROWS - 1) * BYTES_PER_ROW
}

/// The byte being edited, and the first hex digit typed for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// The byte's address.
    pub address: u32,
    /// The high nibble, once typed.
    pub high: Option<u8>,
}

impl Cursor {
    /// The cursor on `address`, with nothing typed.
    pub fn at(address: u32) -> Self {
        Self {
            address,
            high: None,
        }
    }

    /// Takes a typed hex digit. The first is held; the second completes a
    /// byte, returned as the write to make, and moves on to the next
    /// address.
    pub fn type_digit(&mut self, digit: u8) -> Option<(u32, u8)> {
        match self.high.take() {
            None => {
                self.high = Some(digit & 0xF);
                None
            }
            Some(high) => {
                let write = (self.address, high << 4 | (digit & 0xF));
                *self = Self::at(self.address.wrapping_add(1));
                Some(write)
            }
        }
    }

    /// Moves by `bytes` (negative for back), dropping a half-typed byte.
    pub fn moved(self, bytes: i32) -> Self {
        Self::at(self.address.wrapping_add_signed(bytes))
    }
}

/// The value of a hex digit key, if it is one.
pub fn hex_digit(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let digit = chars.next()?.to_digit(16)?;
    chars.next().is_none().then_some(digit as u8)
}

/// Reads an address typed in hex, with or without a `0x` or `$` prefix.
pub fn parse_address(text: &str) -> Option<u32> {
    let text = text.trim();
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .or_else(|| text.strip_prefix('$'))
        .unwrap_or(text);
    u32::from_str_radix(digits, 16).ok()
}

/// An address in hex: four digits below 0x10000, as on a 16-bit bus,
/// otherwise eight.
pub fn format_address(address: u32) -> String {
    if address <= 0xFFFF {
        format!("{address:04X}")
    } else {
        format!("{address:08X}")
    }
}

/// A byte in hex, or `--` where it cannot be read.
pub fn format_byte(byte: Option<u8>) -> String {
    byte.map_or_else(|| "--".to_owned(), |byte| format!("{byte:02X}"))
}

/// Bytes as ASCII, with `.` for anything unprintable or unreadable.
pub fn ascii(bytes: &[Option<u8>]) -> String {
    bytes
        .iter()
        .map(|byte| match byte {
            Some(byte @ 0x20..=0x7E) => char::from(*byte),
            _ => '.',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_two_digits_writes_a_byte_and_moves_on() {
        let mut cursor = Cursor::at(0xC000);
        assert_eq!(cursor.type_digit(0xA), None);
        assert_eq!(cursor.high, Some(0xA));
        assert_eq!(cursor.type_digit(0x5), Some((0xC000, 0xA5)));
        assert_eq!(cursor, Cursor::at(0xC001));
        assert_eq!(cursor.moved(-16), Cursor::at(0xBFF1));
    }

    #[test]
    fn hex_keys_and_addresses_parse() {
        assert_eq!(hex_digit("f"), Some(15));
        assert_eq!(hex_digit("7"), Some(7));
        assert_eq!(hex_digit("g"), None);
        assert_eq!(hex_digit("f1"), None);
        assert_eq!(parse_address("0x02000000"), Some(0x0200_0000));
        assert_eq!(parse_address(" $ff40 "), Some(0xFF40));
        assert_eq!(parse_address("C000"), Some(0xC000));
        assert_eq!(parse_address("nope"), None);
    }

    #[test]
    fn rows_stay_inside_the_address_space() {
        let mut viewer = Viewer::default();
        viewer.scroll(-3.0, 0xFFFF);
        assert_eq!(viewer.address, 0);
        viewer.go_to(0xFFF7, 0xFFFF);
        // 16 rows of 16 bytes end at 0xFFFF.
        assert_eq!(viewer.address, 0xFF00);
        viewer.scroll(2.5, u32::MAX);
        viewer.scroll(0.5, u32::MAX);
        assert_eq!(viewer.address, 0xFF30);
        viewer.go_to(0x0200_0013, u32::MAX);
        assert_eq!(viewer.address, 0x0200_0010);
    }

    #[test]
    fn rows_read_as_text() {
        assert_eq!(format_address(0xC000), "C000");
        assert_eq!(format_address(0x0200_0010), "02000010");
        assert_eq!(format_byte(None), "--");
        assert_eq!(format_byte(Some(0x0A)), "0A");
        assert_eq!(ascii(&[Some(0x48), Some(0x69), None, Some(0)]), "Hi..");
        assert_eq!(regions(System::Gba)[1].name, "EWRAM");
    }
}
