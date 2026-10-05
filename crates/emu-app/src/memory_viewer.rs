//! The memory viewer: a live hex dump of one region of a player's RAM,
//! drawn over the game (UI-12).
//!
//! This module holds the viewer's position and turns bytes into text rows;
//! the window draws them.

use emu_core::System;

/// Rows shown at once.
pub const ROWS: u32 = 16;
/// Bytes on each row.
pub const BYTES_PER_ROW: u32 = 16;

/// A named block of plain RAM, as the CPU addresses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Its usual name.
    pub name: &'static str,
    /// First address.
    pub start: u32,
    /// Size in bytes.
    pub length: u32,
}

const GB: [Region; 3] = [
    Region {
        name: "WRAM",
        start: 0xC000,
        length: 0x2000,
    },
    Region {
        name: "HRAM",
        start: 0xFF80,
        length: 0x7F,
    },
    Region {
        name: "Cartridge RAM",
        start: 0xA000,
        length: 0x2000,
    },
];

const GBA: [Region; 2] = [
    Region {
        name: "EWRAM",
        start: 0x0200_0000,
        length: 0x4_0000,
    },
    Region {
        name: "IWRAM",
        start: 0x0300_0000,
        length: 0x8000,
    },
];

const NDS: [Region; 1] = [Region {
    name: "Main RAM",
    start: 0x0200_0000,
    length: 0x40_0000,
}];

/// The regions the viewer offers for `system`.
pub fn regions(system: System) -> &'static [Region] {
    match system {
        System::Gb => &GB,
        System::Gba => &GBA,
        System::Nds => &NDS,
    }
}

/// Where the viewer is looking.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Viewer {
    /// Index into the system's [`regions`].
    pub region: usize,
    /// Offset of the first row into the region; a multiple of
    /// [`BYTES_PER_ROW`].
    pub offset: u32,
    /// Scrolled distance not yet worth a whole row, in rows.
    remainder: f32,
}

impl Viewer {
    /// Shows `region` from its start.
    pub fn select(&mut self, region: usize) {
        *self = Self {
            region,
            ..Self::default()
        };
    }

    /// Scrolls by `rows` (fractions accumulate), staying inside a region of
    /// `length` bytes.
    pub fn scroll(&mut self, rows: f32, length: u32) {
        self.remainder += rows;
        let whole = self.remainder.trunc();
        self.remainder -= whole;
        let last_row = length.saturating_sub(1) / BYTES_PER_ROW;
        let last_top = last_row.saturating_sub(ROWS - 1);
        let row = i64::from(self.offset / BYTES_PER_ROW) + whole as i64;
        let row = row.clamp(0, i64::from(last_top));
        self.offset = u32::try_from(row).unwrap_or(0) * BYTES_PER_ROW;
    }

    /// The first address shown and how many bytes, in `region`.
    pub fn span(&self, region: &Region) -> (u32, u32) {
        let length = (ROWS * BYTES_PER_ROW).min(region.length.saturating_sub(self.offset));
        (region.start + self.offset, length)
    }
}

/// One row of a hex dump: the address (four digits below 0x10000, as on a
/// 16-bit bus), each byte in hex (`--` where it cannot be read), then the
/// bytes as ASCII.
pub fn hex_row(address: u32, bytes: &[Option<u8>]) -> String {
    let hex: Vec<String> = bytes
        .iter()
        .map(|byte| byte.map_or_else(|| "--".to_owned(), |byte| format!("{byte:02X}")))
        .collect();
    let ascii: String = bytes
        .iter()
        .map(|byte| match byte {
            Some(byte @ 0x20..=0x7E) => char::from(*byte),
            _ => '.',
        })
        .collect();
    let digits = if address <= 0xFFFF { 4 } else { 8 };
    let width = BYTES_PER_ROW as usize * 3 - 1;
    format!("{address:0digits$X}  {:<width$}  {ascii}", hex.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_show_hex_and_ascii() {
        let bytes = [Some(0x48), Some(0x69), None, Some(0x00)];
        assert_eq!(
            hex_row(0x0200_0010, &bytes),
            format!("02000010  48 69 -- 00{}  Hi..", " ".repeat(36))
        );
    }

    #[test]
    fn game_boy_addresses_have_four_digits() {
        assert!(hex_row(0xC000, &[Some(0)]).starts_with("C000  00 "));
    }

    #[test]
    fn scrolling_stays_inside_the_region() {
        let hram = GB[1];
        let mut viewer = Viewer::default();
        viewer.scroll(-3.0, hram.length);
        assert_eq!(viewer.offset, 0);
        viewer.scroll(100.0, hram.length);
        // HRAM's 127 bytes fit in 8 rows, all on screen at once.
        assert_eq!(viewer.offset, 0);
        let ewram = GBA[0];
        viewer.scroll(2.5, ewram.length);
        viewer.scroll(0.5, ewram.length);
        assert_eq!(viewer.offset, 3 * BYTES_PER_ROW);
        assert_eq!(viewer.span(&ewram), (0x0200_0030, 256));
        assert_eq!(viewer.span(&hram).1, 127 - 0x30);
    }
}
