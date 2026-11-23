//! The cartridge's ROM chip and its replies to the 8-byte commands (GBATEK,
//! "DS Cartridge Protocol").
//!
//! The machine boots games directly, so the chip starts in main data mode.
//! KEY2 encryption scrambles commands and data on the wire alike and is
//! transparent here; KEY1 is never needed. The unencrypted boot commands
//! are answered too.

use serde::{Deserialize, Serialize};

use super::backup;

/// Get data from an address (main data mode).
const READ_DATA: u8 = 0xB7;
/// Get the chip ID (main data mode).
const CHIP_ID: u8 = 0xB8;
/// Get the header (unencrypted mode).
const HEADER: u8 = 0x00;
/// Get the chip ID (unencrypted mode).
const RAW_CHIP_ID: u8 = 0x90;

/// Macronix, the maker of most DS ROM chips.
const MANUFACTURER: u8 = 0xC2;
const MIB: usize = 0x10_0000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Rom {
    /// Restored from the ROM file, never saved in states.
    #[serde(skip)]
    data: Box<[u8]>,
    chip_id: u32,
    /// The chip size minus one: addresses past the end mirror.
    mask: u32,
}

impl Rom {
    pub(crate) fn new(data: Box<[u8]>) -> Self {
        let size = chip_size(&data);
        Self {
            chip_id: chip_id(&data, size),
            mask: u32::try_from(size - 1).unwrap_or(u32::MAX),
            data,
        }
    }

    pub(crate) fn data(&self) -> &[u8] {
        &self.data
    }

    /// Moves the ROM image out of `other`.
    pub(crate) fn take_data_from(&mut self, other: &mut Self) {
        self.data = std::mem::take(&mut other.data);
    }

    /// The 4-byte chip ID as the data port returns it, first byte lowest.
    pub(crate) fn chip_id(&self) -> u32 {
        self.chip_id
    }

    /// Byte `offset` of the reply to `command`.
    pub(crate) fn reply(&self, command: &[u8; 8], offset: u32) -> u8 {
        let address = u32::from_be_bytes([command[1], command[2], command[3], command[4]]);
        match command[0] {
            READ_DATA => {
                // The stream wraps within the 4 KiB block it started in.
                let block = address & !0xFFF;
                self.main_data(block | (address.wrapping_add(offset) & 0xFFF))
            }
            CHIP_ID | RAW_CHIP_ID => self.chip_id.to_le_bytes()[(offset & 3) as usize],
            // The header repeats every 4 KiB.
            HEADER => self.byte(address.wrapping_add(offset) & 0xFFF),
            // 9Fh reads the floating bus; other commands get nothing either.
            _ => 0xFF,
        }
    }

    /// A byte as command B7h reads it: addresses mirror past the end of the
    /// chip, and the header and secure area below 8000h are out of reach,
    /// redirected to 8000h + (address & 1FFh).
    fn main_data(&self, address: u32) -> u8 {
        let address = address & self.mask;
        if address < 0x8000 {
            self.byte(0x8000 + (address & 0x1FF))
        } else {
            self.byte(address)
        }
    }

    /// A byte of the image; a trimmed image reads erased bytes past its end.
    fn byte(&self, address: u32) -> u8 {
        self.data.get(address as usize).copied().unwrap_or(0xFF)
    }
}

/// The chip's size: the header's device capacity (0x14, 128 KiB shifted
/// left) or the image, whichever is larger, rounded up to a power of two.
fn chip_size(rom: &[u8]) -> usize {
    let capacity = rom
        .get(0x14)
        .filter(|&&shift| shift < 16)
        .map_or(0, |&shift| 0x2_0000 << shift);
    rom.len().max(capacity).max(MIB).next_power_of_two()
}

/// GBATEK's chip ID: manufacturer, size (megabytes - 1 up to 128 MiB, then
/// 100h - megabytes / 256), the infrared flag, and the protocol flags: bit
/// 7 for the newer protocol of carts from 128 MiB up, bit 6 for DSi carts
/// (header 0x12 bit 1).
fn chip_id(rom: &[u8], size: usize) -> u32 {
    let megabytes = size / MIB;
    let size_code = if megabytes <= 128 {
        megabytes - 1
    } else {
        0x100 - megabytes / 256
    };
    let infrared = u8::from(backup::has_infrared(rom));
    let newer_protocol = if megabytes >= 128 { 0x80 } else { 0 };
    let dsi = if rom.get(0x12).is_some_and(|unit| unit & 2 != 0) {
        0x40
    } else {
        0
    };
    u32::from_le_bytes([
        MANUFACTURER,
        size_code as u8,
        infrared,
        newer_protocol | dsi,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(bytes: &[u8]) -> [u8; 8] {
        let mut command = [0; 8];
        command[..bytes.len()].copy_from_slice(bytes);
        command
    }

    #[test]
    fn chip_ids_follow_the_size_and_flags() {
        let mut image = vec![0; 0x200];
        image[0x14] = 0x0B; // 256 MiB
        image[0x12] = 0x02; // DSi enhanced
        image[0x0C..0x10].copy_from_slice(b"IRAO");
        let rom = Rom::new(image.into_boxed_slice());
        assert_eq!(rom.chip_id().to_le_bytes(), [0xC2, 0xFF, 0x01, 0xC0]);
        // A trimmed 1 MiB + 1 image rounds up to 2 MiB.
        let rom = Rom::new(vec![0; 0x10_0001].into_boxed_slice());
        assert_eq!(rom.chip_id().to_le_bytes(), [0xC2, 0x01, 0x00, 0x00]);
        let mut image = vec![0; 0x200];
        image[0x14] = 0x0A; // 128 MiB
        let rom = Rom::new(image.into_boxed_slice());
        assert_eq!(rom.chip_id().to_le_bytes(), [0xC2, 0x7F, 0x00, 0x80]);
    }

    #[test]
    fn data_reads_wrap_in_4k_blocks_and_skip_the_secure_area() {
        let image: Vec<u8> = (0..0x10_0000u32).map(|i| (i ^ i >> 8) as u8).collect();
        let rom = Rom::new(image.clone().into_boxed_slice());
        let read = command(&[READ_DATA, 0x00, 0x01, 0x2F, 0xFE]);
        assert_eq!(rom.reply(&read, 0), image[0x1_2FFE]);
        assert_eq!(rom.reply(&read, 2), image[0x1_2000]);
        let secure = command(&[READ_DATA, 0x00, 0x00, 0x40, 0x10]);
        assert_eq!(rom.reply(&secure, 0), image[0x8010]);
        // Past the end of the chip, addresses mirror (here onto the
        // redirected area).
        let mirror = command(&[READ_DATA, 0x00, 0x10, 0x00, 0x05]);
        assert_eq!(rom.reply(&mirror, 0), image[0x8005]);
    }

    #[test]
    fn the_header_command_repeats_the_first_4k() {
        let image: Vec<u8> = (0..0x2000u32).map(|i| i as u8 ^ 0x5A).collect();
        let rom = Rom::new(image.clone().into_boxed_slice());
        let header = command(&[HEADER]);
        assert_eq!(rom.reply(&header, 0x15C), image[0x15C]);
        assert_eq!(rom.reply(&header, 0x1001), image[1]);
        assert_eq!(rom.reply(&command(&[0x9F]), 0), 0xFF);
        assert_eq!(rom.reply(&command(&[RAW_CHIP_ID]), 4), 0xC2, "Macronix");
    }
}
