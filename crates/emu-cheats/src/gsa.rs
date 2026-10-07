//! GBA GameShark Advance and Action Replay v1/v2 codes, once decrypted
//! (GBATEK, "GBA Cheat Codes - Gameshark/Action Replay V1/V2").
//!
//! ```text
//! 0aaaaaaa 000000xx  byte [a] = xx
//! 1aaaaaaa 0000xxxx  halfword [a] = xxxx
//! 2aaaaaaa xxxxxxxx  word [a] = xxxxxxxx
//! 3000cccc xxxxxxxx  word = xxxxxxxx at cccc-1 addresses, two per line after
//! Daaaaaaa 0000xxxx  if halfword [a] = xxxx, run the next code
//! E0zzxxxx 0aaaaaaa  if halfword [a] = xxxx, run the next zz codes
//! Faaaaaaa 00000x0y  hook (ignored)
//! xxxxxxxx 001DC0DE  game ID (ignored)
//! DEADFACE 0000xxyy  new encryption seeds (see `tea.rs`)
//! ```
//!
//! ROM patches (`6`) and button codes (`8`) are not supported: one needs a
//! ROM-read hook, the other the device's own button.

use crate::decode::{Decoder, halves};
use crate::detect::{CERTAIN, CODE, UNKNOWN, fits, region};
use crate::ops::{Op, Test, Width};

const ID: u32 = 0x001D_C0DE;
const DEADFACE: u32 = 0xDEAD_FACE;
const SLOWDOWN: u32 = 0x80F0_0000;

/// A type 3 code collecting its addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct List {
    value: u32,
    left: usize,
    addresses: Vec<u32>,
}

/// Reads decrypted GameShark Advance codes.
#[derive(Debug, Default)]
pub(crate) struct GameSharkAdvance {
    list: Option<List>,
}

impl Decoder for GameSharkAdvance {
    fn code(&mut self, code: u64, ops: &mut Vec<Op>) -> Result<(), String> {
        let (left, right) = halves(code);
        if let Some(mut list) = self.list.take() {
            for address in [left, right] {
                if list.left > 0 {
                    list.addresses.push(address);
                    list.left -= 1;
                }
            }
            if list.left > 0 {
                self.list = Some(list);
            } else {
                ops.push(Op::WriteEach {
                    addresses: list.addresses,
                    value: list.value,
                    width: Width::Word,
                });
            }
            return Ok(());
        }
        if right == ID || left == DEADFACE || left == SLOWDOWN {
            return Ok(());
        }
        let address = left & 0x0FFF_FFFF;
        let write = |width: Width| Op::Write {
            address,
            value: right & width.mask(),
            width,
        };
        ops.push(match left >> 28 {
            0x0 => write(Width::Byte),
            0x1 => write(Width::Half),
            0x2 => write(Width::Word),
            0x3 if left & 0x0FFF_0000 == 0 => {
                let count = (left & 0xFFFF).saturating_sub(1) as usize;
                if count > 0 {
                    self.list = Some(List {
                        value: right,
                        left: count,
                        addresses: Vec::with_capacity(count),
                    });
                }
                return Ok(());
            }
            0x6 => return Err("ROM patches (type 6) are not supported yet".to_owned()),
            0x8 => {
                return Err(
                    "type 8 codes write only while the GameShark's button is held, \
                     and there is no such button here"
                        .to_owned(),
                );
            }
            0xD if right >> 16 == 0 => Op::If {
                address,
                value: right,
                width: Width::Half,
                test: Test::Equal,
                skip: 1,
            },
            0xE if left & 0x0F00_0000 == 0 => Op::If {
                address: right & 0x0FFF_FFFF,
                value: left & 0xFFFF,
                width: Width::Half,
                test: Test::Equal,
                skip: ((left >> 16) & 0xFF) as usize,
            },
            // The hook tells the device where to run.
            0xF => return Ok(()),
            _ => return Err(format!("unknown code {left:08X} {right:08X}")),
        });
        Ok(())
    }

    fn waiting_for(&self) -> Option<&'static str> {
        self.list
            .is_some()
            .then_some("the rest of a type 3 code's addresses")
    }
}

/// How much decrypted `codes` look like GameShark Advance codes: positive
/// for plausible ones, very high for an ID or seed code.
pub(crate) fn plausibility(codes: &[u64]) -> i32 {
    let mut score = 0;
    let mut list_lines = 0;
    for &code in codes {
        if list_lines > 0 {
            list_lines -= 1;
            continue;
        }
        let (left, right) = halves(code);
        let address = left & 0x0FFF_FFFF;
        score += if right == ID || (left == DEADFACE && right >> 16 == 0) {
            CERTAIN
        } else {
            match left >> 28 {
                0x0 => CODE + region(address) + fits(right, 0xFF),
                0x1 => CODE + region(address) + fits(right, 0xFFFF),
                0x2 => CODE + region(address),
                0x3 if left & 0x0FFF_0000 == 0 => {
                    list_lines = (left & 0xFFFF) as usize / 2;
                    CODE
                }
                0x6 if left & 0x0F00_0000 == 0 => CODE + fits(right, 0xF000_FFFF),
                0x8 if left == SLOWDOWN || matches!((left >> 20) & 0xF, 1 | 2) => CODE,
                0xD => CODE + region(address) + fits(right, 0xFFFF),
                0xE if left & 0x0F00_0000 == 0 => CODE + region(right & 0x0FFF_FFFF),
                0xF => CODE + fits(right, 0x0F0F),
                _ => UNKNOWN,
            }
        };
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codes::Item;
    use crate::decode::decode;

    fn read(codes: &[u64]) -> Result<Vec<Op>, (usize, String)> {
        let items: Vec<_> = codes
            .iter()
            .enumerate()
            .map(|(line, &code)| (line + 1, Item::Code(code)))
            .collect();
        decode(&items, &mut GameSharkAdvance::default())
    }

    #[test]
    fn writes_conditions_and_lists() {
        assert_eq!(
            read(&[
                0x4241_5850_001D_C0DE,
                0xF800_0100_0000_0001,
                0x1202_5E30_0000_270F,
            ]),
            Ok(vec![Op::Write {
                address: 0x0202_5E30,
                value: 0x270F,
                width: Width::Half
            }])
        );
        assert_eq!(
            read(&[
                0xE002_0001_0300_1000,
                0x3000_0004_0000_0063,
                0x0200_0000_0200_0004,
                0x0200_0008_0000_0000
            ]),
            Ok(vec![
                Op::If {
                    address: 0x0300_1000,
                    value: 1,
                    width: Width::Half,
                    test: Test::Equal,
                    skip: 2
                },
                Op::WriteEach {
                    addresses: vec![0x0200_0000, 0x0200_0004, 0x0200_0008],
                    value: 0x63,
                    width: Width::Word
                },
            ])
        );
    }

    #[test]
    fn unsupported_types_say_why() {
        assert!(
            read(&[0x6000_1234_0000_5678])
                .expect_err("patch")
                .1
                .contains("ROM")
        );
        assert!(
            read(&[0x8212_3456_0000_0001])
                .expect_err("button")
                .1
                .contains("button")
        );
        assert!(read(&[0x4000_0000_0000_0000]).is_err());
    }

    #[test]
    fn plausible_codes_score_above_garbage() {
        assert!(plausibility(&[0x1202_5E30_0000_270F]) > 0);
        assert!(plausibility(&[0x4000_0000_0000_0000]) < 0);
        assert!(plausibility(&[0x0F12_3456_1234_5678]) < 0);
        assert!(plausibility(&[0x1234_5678_001D_C0DE]) >= CERTAIN);
    }
}
