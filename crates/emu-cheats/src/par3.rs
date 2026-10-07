//! GBA Pro Action Replay v3 (Action Replay MAX) codes, once decrypted
//! (GBATEK, "GBA Cheat Codes - Pro Action Replay V3").
//!
//! A code's first word is `ttaaaaaa`: `tt` packs a type, a size and a
//! sub-type, and `aaaaaa` packs the address `a0aaaaa` (bits 20–23 move to
//! 24–27). Writes fill, write through a pointer or add; conditionals run
//! the next one or two codes, a block up to `ELSE`/`ENDIF`, or end the
//! cheat. `00000000` introduces slides, `ELSE`, `ENDIF` and the end of the
//! list.
//!
//! Not supported: I/O writes (`C6`, `C7`), ROM patches and button codes.
//! The hook (`C4`), game ID and slowdown codes are ignored. Blocks nest;
//! one left open ends with the cheat.

use crate::decode::{Decoder, halves};
use crate::detect::{CERTAIN, CODE, UNKNOWN, fits, region};
use crate::ops::{Modify, Op, Test, Width};

const ID: u32 = 0x001D_C0DE;
const DEADFACE: u32 = 0xDEAD_FACE;

/// `a0aaaaa` from `aaaaaa`.
fn address(bits: u32) -> u32 {
    (bits & 0x000F_FFFF) | (bits & 0x00F0_0000) << 4
}

/// The width a code's size field names, or `None` for size 3.
fn width(size: u32) -> Option<Width> {
    match size {
        0 => Some(Width::Byte),
        1 => Some(Width::Half),
        2 => Some(Width::Word),
        _ => None,
    }
}

/// A conditional block waiting for its `ELSE` or `ENDIF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Block {
    /// Index of the block's [`Op::If`].
    start: usize,
    /// Index of its `ELSE`'s [`Op::Skip`], once seen.
    otherwise: Option<usize>,
}

/// Reads decrypted Pro Action Replay v3 codes.
#[derive(Debug, Default)]
pub(crate) struct ProActionReplay {
    /// A slide's address and width, waiting for its second line.
    slide: Option<(u32, Width)>,
    blocks: Vec<Block>,
}

fn set_skip(ops: &mut [Op], index: usize, skip: usize) {
    if let Some(Op::If { skip: old, .. } | Op::Skip(old)) = ops.get_mut(index) {
        *old = skip;
    }
}

impl ProActionReplay {
    fn otherwise(&mut self, ops: &mut Vec<Op>) -> Result<(), String> {
        let block = self
            .blocks
            .last_mut()
            .ok_or("ELSE without a block conditional before it")?;
        if block.otherwise.is_some() {
            return Err("a second ELSE in one block".to_owned());
        }
        let index = ops.len();
        ops.push(Op::Skip(0));
        block.otherwise = Some(index);
        set_skip(ops, block.start, index - block.start);
        Ok(())
    }

    fn end_block(&mut self, ops: &mut [Op]) -> Result<(), String> {
        let block = self
            .blocks
            .pop()
            .ok_or("ENDIF without a block conditional before it")?;
        let end = ops.len();
        match block.otherwise {
            Some(index) => set_skip(ops, index, end - index - 1),
            None => set_skip(ops, block.start, end - block.start - 1),
        }
        Ok(())
    }

    /// The `00000000 xxxxxxxx` codes.
    fn special(&mut self, right: u32, ops: &mut Vec<Op>) -> Result<(), String> {
        match right >> 24 {
            0x00 if right == 0 => ops.push(Op::Skip(usize::MAX)),
            0x08 => {}
            0x10 | 0x12 | 0x14 => {
                return Err(
                    "button codes write only while the Action Replay's button is held, \
                     and there is no such button here"
                        .to_owned(),
                );
            }
            0x18 | 0x1A | 0x1C | 0x1E => {
                return Err("ROM patches are not supported yet".to_owned());
            }
            0x40 => self.end_block(ops)?,
            0x60 => self.otherwise(ops)?,
            kind @ (0x80 | 0x82 | 0x84) => {
                let width = width((kind >> 1) & 3).unwrap_or(Width::Word);
                self.slide = Some((address(right), width));
            }
            _ => return Err(format!("unknown code 00000000 {right:08X}")),
        }
        Ok(())
    }
}

impl Decoder for ProActionReplay {
    fn code(&mut self, code: u64, ops: &mut Vec<Op>) -> Result<(), String> {
        let (left, right) = halves(code);
        if let Some((address, width)) = self.slide.take() {
            // yyyyyyyy ssccssss: value, then value step, count, address step.
            ops.push(Op::Slide {
                address,
                value: left & width.mask(),
                width,
                count: (right >> 16) & 0xFF,
                step: (right & 0xFFFF) * width as u32,
                increment: right >> 24,
            });
            return Ok(());
        }
        if right == ID || left == DEADFACE {
            return Ok(());
        }
        if left == 0 {
            return self.special(right, ops);
        }
        let kind = left >> 24;
        if kind == 0xC4 {
            // The hook tells the device where to run.
            return Ok(());
        }
        let unknown = || format!("unknown code {left:08X} {right:08X}");
        if kind & 1 != 0 {
            return Err(if kind == 0xC7 {
                "I/O writes (C6, C7) are not supported yet".to_owned()
            } else {
                unknown()
            });
        }
        let address = address(left);
        let condition = (left >> 27) & 7;
        let size = (left >> 25) & 3;
        let action = left >> 30;
        if condition != 0 {
            let test = [
                Test::Never,
                Test::Equal,
                Test::NotEqual,
                Test::Less,
                Test::Greater,
                Test::LessUnsigned,
                Test::GreaterUnsigned,
                Test::AnyBits,
            ][condition as usize];
            let (test, width) = width(size).map_or((Test::Never, Width::Word), |w| (test, w));
            let skip = match action {
                0 => 1,
                1 => 2,
                2 => {
                    self.blocks.push(Block {
                        start: ops.len(),
                        otherwise: None,
                    });
                    0
                }
                _ => usize::MAX,
            };
            ops.push(Op::If {
                address,
                value: right & width.mask(),
                width,
                test,
                skip,
            });
            return Ok(());
        }
        let Some(width) = width(size) else {
            return Err(if kind == 0xC6 {
                "I/O writes (C6, C7) are not supported yet".to_owned()
            } else {
                unknown()
            });
        };
        let value = right & width.mask();
        ops.push(match (action, width) {
            (0, Width::Byte) => Op::Slide {
                address,
                value,
                width,
                count: (right >> 8) + 1,
                step: 1,
                increment: 0,
            },
            (0, Width::Half) => Op::Slide {
                address,
                value,
                width,
                count: (right >> 16) + 1,
                step: 2,
                increment: 0,
            },
            (0, Width::Word) => Op::Write {
                address,
                value,
                width,
            },
            (1, _) => Op::WriteIndirect {
                pointer: address,
                offset: match width {
                    Width::Byte => right >> 8,
                    Width::Half => (right >> 16) * 2,
                    Width::Word => 0,
                },
                value,
                width,
            },
            (2, _) => Op::Modify {
                address,
                value,
                width,
                with: Modify::Add,
            },
            _ => return Err(unknown()),
        });
        Ok(())
    }

    fn waiting_for(&self) -> Option<&'static str> {
        self.slide
            .is_some()
            .then_some("the second line of a slide code")
    }

    fn finish(&mut self, ops: &mut Vec<Op>) {
        while self.end_block(ops).is_ok() {}
    }
}

/// How much decrypted `codes` look like Pro Action Replay v3 codes:
/// positive for plausible ones, very high for an ID or seed code.
pub(crate) fn plausibility(codes: &[u64]) -> i32 {
    let mut score = 0;
    let mut slide_line = false;
    for &code in codes {
        if std::mem::take(&mut slide_line) {
            continue;
        }
        let (left, right) = halves(code);
        score += if right == ID || (left == DEADFACE && right >> 16 == 0) {
            CERTAIN
        } else if left == 0 {
            match right >> 24 {
                0x00 if right == 0 => CODE,
                0x08 => CODE + fits(right, 0x00FF_FF00),
                0x10 | 0x12 | 0x14 => CODE + region(address(right)),
                0x18 | 0x1A | 0x1C | 0x1E => CODE,
                0x40 | 0x60 => CODE + fits(right, 0xFF00_0000),
                0x80 | 0x82 | 0x84 => {
                    slide_line = true;
                    CODE + region(address(right))
                }
                _ => UNKNOWN,
            }
        } else {
            let kind = left >> 24;
            let condition = (left >> 27) & 7;
            let size = (left >> 25) & 3;
            let action = left >> 30;
            let mask = width(size).map_or(0, Width::mask);
            match (kind, condition, action) {
                (0xC4, ..) => CODE + fits(right, 0xFFFF),
                (0xC6 | 0xC7, ..) => CODE,
                _ if kind & 1 != 0 => UNKNOWN,
                (_, 1.., _) if size == 3 => CODE,
                (_, 1.., _) => CODE + region(address(left)) + fits(right, mask),
                (_, 0, 0 | 1) if size < 3 => CODE + region(address(left)),
                (_, 0, 2) if size < 3 => CODE + region(address(left)) + fits(right, mask),
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
        decode(&items, &mut ProActionReplay::default())
    }

    #[test]
    fn addresses_unpack_and_writes_fill() {
        assert_eq!(
            read(&[0x0422_5E30_0000_270F, 0x0022_0000_0000_0300]),
            Ok(vec![
                Op::Write {
                    address: 0x0202_5E30,
                    value: 0x270F,
                    width: Width::Word
                },
                Op::Slide {
                    address: 0x0202_0000,
                    value: 0,
                    width: Width::Byte,
                    count: 4,
                    step: 1,
                    increment: 0
                },
            ])
        );
        assert_eq!(
            read(&[0x4232_0000_0002_0063, 0x8222_0010_0000_0001]),
            Ok(vec![
                Op::WriteIndirect {
                    pointer: 0x0302_0000,
                    offset: 4,
                    value: 0x63,
                    width: Width::Half
                },
                Op::Modify {
                    address: 0x0202_0010,
                    value: 1,
                    width: Width::Half,
                    with: Modify::Add
                },
            ])
        );
    }

    #[test]
    fn conditionals_and_blocks() {
        // IF halfword [0x02020000] < 5 (signed) THEN block; ELSE; ENDIF.
        let ops = read(&[
            0x9A22_0000_0000_0005,
            0x0422_0004_0000_0001,
            0x0000_0000_6000_0000,
            0x0422_0004_0000_0002,
            0x0422_0008_0000_0003,
            0x0000_0000_4000_0000,
            0x0422_000C_0000_0004,
        ])
        .expect("valid");
        assert_eq!(
            ops[0],
            Op::If {
                address: 0x0202_0000,
                value: 5,
                width: Width::Half,
                test: Test::Less,
                skip: 2
            }
        );
        assert_eq!(ops[2], Op::Skip(2));
        assert_eq!(ops.len(), 6);
        // Size 3 is "always false"; sub-type 3 ends the cheat.
        let ops = read(&[0xCE22_0000_0000_0000]).expect("valid");
        assert!(matches!(
            ops[0],
            Op::If {
                test: Test::Never,
                skip: usize::MAX,
                ..
            }
        ));
    }

    #[test]
    fn slides_take_a_second_line() {
        assert_eq!(
            read(&[0x0000_0000_8222_0000, 0x0000_0010_0103_0002]),
            Ok(vec![Op::Slide {
                address: 0x0202_0000,
                value: 0x10,
                width: Width::Half,
                count: 3,
                step: 4,
                increment: 1
            }])
        );
        assert!(read(&[0x0000_0000_8222_0000]).is_err());
    }

    #[test]
    fn unsupported_codes_say_why() {
        for (code, needle) in [
            (0xC600_0100_0000_0001, "I/O"),
            (0xC700_0100_0000_0001, "I/O"),
            (0x0000_0000_1822_0000, "ROM"),
            (0x0000_0000_1222_0000, "button"),
            (0x0000_0000_4000_0000, "ENDIF"),
        ] {
            let error = read(&[code]).expect_err("unsupported");
            assert!(error.1.contains(needle), "{code:X}: {}", error.1);
        }
    }
}
