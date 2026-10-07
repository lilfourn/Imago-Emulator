//! Game Boy GameShark codes, `TTVVAAAA` (Pan Docs, "Game Genie/Shark
//! Cheats"; EnHacklopedia, "Hacking Gameboy").
//!
//! `VV` is the byte to write and `AAAA` the address, low byte first. `TT`
//! is the type: `01` writes to whichever bank is mapped; `8b` and `9b`
//! write to bank `b` of the switchable RAM holding the address (cartridge
//! RAM at 0xA000–0xBFFF, or Game Boy Color WRAM at 0xD000–0xDFFF), the way
//! the device switched banks before writing.

use crate::decode::Decoder;
use crate::ops::{Op, Width};

/// Reads GameShark codes.
pub(crate) struct GameShark;

impl Decoder for GameShark {
    fn code(&mut self, code: u64, ops: &mut Vec<Op>) -> Result<(), String> {
        let code = code as u32;
        let kind = (code >> 24) as u8;
        let value = (code >> 16) as u8;
        let address = (code & 0xFF) << 8 | (code >> 8) & 0xFF;
        ops.push(match kind {
            0x01 => Op::Write {
                address,
                value: u32::from(value),
                width: Width::Byte,
            },
            0x80..=0x9F => Op::WriteBank {
                bank: kind & 0x0F,
                address,
                value,
            },
            _ => {
                return Err(format!(
                    "code type {kind:02X} is not supported (01, 8x and 9x are)"
                ));
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(code: u64) -> Result<Vec<Op>, String> {
        let mut ops = Vec::new();
        GameShark.code(code, &mut ops).map(|()| ops)
    }

    #[test]
    fn addresses_are_low_byte_first() {
        assert_eq!(
            decode(0x0101_38CD),
            Ok(vec![Op::Write {
                address: 0xCD38,
                value: 0x01,
                width: Width::Byte
            }])
        );
    }

    #[test]
    fn bank_types_name_the_bank() {
        assert_eq!(
            decode(0x9163_10D0),
            Ok(vec![Op::WriteBank {
                bank: 1,
                address: 0xD010,
                value: 0x63
            }])
        );
        assert_eq!(
            decode(0x8302_00A0),
            Ok(vec![Op::WriteBank {
                bank: 3,
                address: 0xA000,
                value: 0x02
            }])
        );
        assert!(decode(0x0201_38CD).expect_err("type 02").contains("02"));
    }
}
