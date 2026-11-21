//! GBA CodeBreaker (Xploder) codes, `TAAAAAAA YYYY`, and their encryption
//! (GBATEK, "GBA Cheat Codes - Codebreaker/Xploder").
//!
//! Type `T` acts on the 28-bit address `AAAAAAA` with the 16-bit value
//! `YYYY`. A `9` code switches on encryption for every code after it, in
//! this cheat and the ones that follow, as it did on the device.
//!
//! Two details follow mGBA (`src/gba/cheats/codebreaker.c`, MPL-2.0,
//! © Jeffrey Pfau) where GBATEK is silent or differs:
//! - decryption also XORs the value with the last seed halfword, which
//!   GBATEK computes but never uses;
//! - a `5` code's count is in halfwords, not bytes (VBA-M agrees).

use crate::decode::Decoder;
use crate::ops::{Modify, Op, Test, Width};

/// KEYINPUT, which type `D` codes test.
const KEYINPUT: u32 = 0x0400_0130;

/// An encryption key, from a `9` code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Key {
    /// Which bit each bit of a code swaps with.
    swaps: [u8; 48],
    /// GBATEK's "seedlist".
    seeds: [u8; 14],
}

/// GBATEK's random function: three steps of an LCG, mixed.
struct Random(u32);

impl Random {
    fn step(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039);
        self.0
    }

    fn next(&mut self) -> u32 {
        let high = (self.step() << 14) & 0xC000_0000;
        let middle = (self.step() >> 1) & 0x3FFF_8000;
        let low = (self.step() >> 16) & 0x7FFF;
        high | middle | low
    }

    /// Replaces the state with its next output `times` times.
    fn spin(&mut self, times: u8) {
        for _ in 0..times {
            self.0 = self.next();
        }
    }
}

/// A code's six bytes as the device held them: the address, then the
/// value, each little-endian.
fn bytes(address: u32, value: u16) -> [u8; 6] {
    let [a0, a1, a2, a3] = address.to_le_bytes();
    let [v0, v1] = value.to_le_bytes();
    [a0, a1, a2, a3, v0, v1]
}

fn word(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn half(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

impl Key {
    /// The key a `9xyyxxxx xxxx` code sets.
    pub(crate) fn new(address: u32, value: u16) -> Self {
        let code = bytes(address, value);
        let mut swaps: [u8; 48] = std::array::from_fn(|index| index as u8);
        let mut random = Random(0x1111 ^ u32::from(code[4]));
        for _ in 0..0x50 {
            let first = (random.next() % 48) as usize;
            let second = (random.next() % 48) as usize;
            swaps.swap(first, second);
        }
        let mut seeds = [0; 14];
        seeds[..2].copy_from_slice(&code[..2]);
        // GBATEK spins byte[code+3] - 0x91 + 1 times: the low digit of 9x.
        let mut random = Random(0x4EFA_D1C3);
        random.spin(code[3] & 0x0F);
        seeds[2..6].copy_from_slice(&random.next().to_le_bytes());
        seeds[6..8].copy_from_slice(&(random.next() as u16).to_le_bytes());
        let mut random = Random(0xF254 ^ u32::from(code[5]));
        random.spin(code[5]);
        seeds[8..12].copy_from_slice(&random.next().to_le_bytes());
        seeds[12..14].copy_from_slice(&(random.next() as u16).to_le_bytes());
        Self { swaps, seeds }
    }

    /// Decrypts one code.
    pub(crate) fn decrypt(&self, address: u32, value: u16) -> (u32, u16) {
        /// Where each byte of the device's big-endian view of a code is.
        const BYTE: [usize; 6] = [3, 2, 1, 0, 5, 4];
        let mut code = bytes(address, value);
        for (first, &second) in self.swaps.iter().enumerate().rev() {
            let second = usize::from(second);
            let (byte1, bit1) = (BYTE[first / 8], first % 8);
            let (byte2, bit2) = (BYTE[second / 8], second % 8);
            let value1 = code[byte1] >> bit1 & 1;
            let value2 = code[byte2] >> bit2 & 1;
            code[byte1] = code[byte1] & !(1 << bit1) | value2 << bit1;
            code[byte2] = code[byte2] & !(1 << bit2) | value1 << bit2;
        }
        let seeds = &self.seeds;
        let code = bytes(
            word(&code) ^ word(&seeds[8..]),
            half(&code[4..]) ^ half(&seeds[12..]),
        );
        let [c0, _, _, c3, c4, c5] = code.map(u32::from);
        let (s0, s1) = (u32::from(seeds[0]), u32::from(seeds[1]));
        let address = (c3 << 24 | c3 << 16 | c0 << 8 | c5)
            ^ (u32::from(half(&code[1..])) * 0x0001_0001)
            ^ word(&seeds[2..])
            ^ (s0 * 0x0101_0101)
            ^ (s1 << 24);
        let value = (c5 | (c0 ^ c4) << 8) ^ (s0 * 0x0101) ^ u32::from(half(&seeds[6..]));
        (address, value as u16)
    }
}

/// A code that needs the lines after it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Open {
    /// `4aaaaaaa yyyy` waits for `iiiicccc ssss`.
    Slide { address: u32, value: u16 },
    /// `5aaaaaaa cccc` waits for its bytes, six per line.
    Patch {
        address: u32,
        left: usize,
        bytes: Vec<u8>,
    },
}

/// Reads CodeBreaker codes, decrypting them with `key` once a `9` code
/// sets it.
pub(crate) struct CodeBreaker<'a> {
    pub(crate) key: &'a mut Option<Key>,
    open: Option<Open>,
}

impl<'a> CodeBreaker<'a> {
    pub(crate) fn new(key: &'a mut Option<Key>) -> Self {
        Self { key, open: None }
    }

    /// Feeds a line to the open multi-line code.
    fn continue_open(open: Open, address: u32, value: u16, ops: &mut Vec<Op>) -> Option<Open> {
        match open {
            Open::Slide {
                address: start,
                value: first,
            } => {
                ops.push(Op::Slide {
                    address: start,
                    value: u32::from(first),
                    width: Width::Half,
                    count: address & 0xFFFF,
                    step: u32::from(value),
                    increment: address >> 16,
                });
                None
            }
            Open::Patch {
                address: start,
                mut left,
                mut bytes,
            } => {
                let [a3, a2, a1, a0] = address.to_be_bytes();
                let [v1, v0] = value.to_be_bytes();
                for byte in [a3, a2, a1, a0, v1, v0] {
                    if left > 0 {
                        bytes.push(byte);
                        left -= 1;
                    }
                }
                if left > 0 {
                    return Some(Open::Patch {
                        address: start,
                        left,
                        bytes,
                    });
                }
                ops.push(Op::Patch {
                    address: start,
                    bytes,
                });
                None
            }
        }
    }
}

impl Decoder for CodeBreaker<'_> {
    fn code(&mut self, code: u64, ops: &mut Vec<Op>) -> Result<(), String> {
        let (mut address, mut value) = ((code >> 16) as u32, code as u16);
        if let Some(key) = &*self.key {
            (address, value) = key.decrypt(address, value);
        }
        if let Some(open) = self.open.take() {
            self.open = Self::continue_open(open, address, value, ops);
            return Ok(());
        }
        let target = address & 0x0FFF_FFFF;
        let operand = u32::from(value);
        let modify = |with| Op::Modify {
            address: target,
            value: operand,
            width: Width::Half,
            with,
        };
        let test = |test| Op::If {
            address: target,
            value: operand,
            width: Width::Half,
            test,
            skip: 1,
        };
        ops.push(match address >> 28 {
            // The game ID and the hook tell the device where to run.
            0x0 | 0x1 => return Ok(()),
            0x2 => modify(Modify::Or),
            0x3 => Op::Write {
                address: target,
                value: operand & 0xFF,
                width: Width::Byte,
            },
            0x4 => {
                self.open = Some(Open::Slide {
                    address: target,
                    value,
                });
                return Ok(());
            }
            0x5 => {
                let left = usize::from(value) * 2;
                if left > 0 {
                    self.open = Some(Open::Patch {
                        address: target,
                        left,
                        bytes: Vec::with_capacity(left),
                    });
                }
                return Ok(());
            }
            0x6 => modify(Modify::And),
            0x7 => test(Test::Equal),
            0x8 => Op::Write {
                address: target,
                value: operand,
                width: Width::Half,
            },
            0x9 => {
                *self.key = Some(Key::new(address, value));
                return Ok(());
            }
            0xA => test(Test::NotEqual),
            0xB => test(Test::Greater),
            0xC => test(Test::Less),
            0xD if target == 0x20 => Op::If {
                address: KEYINPUT,
                value: operand,
                width: Width::Half,
                test: Test::NoBits,
                skip: 1,
            },
            0xD => {
                return Err(format!(
                    "a type D code tests the joypad, D0000020; found D{target:07X}"
                ));
            }
            0xE => modify(Modify::Add),
            _ => test(Test::AnyBits),
        });
        Ok(())
    }

    fn waiting_for(&self) -> Option<&'static str> {
        match self.open {
            Some(Open::Slide { .. }) => Some("the second line of a type 4 slide code"),
            Some(Open::Patch { .. }) => Some("the rest of a type 5 code's bytes"),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;

    fn read(codes: &[u64]) -> Result<Vec<Op>, (usize, String)> {
        let items: Vec<_> = codes
            .iter()
            .enumerate()
            .map(|(line, &code)| (line + 1, crate::codes::Item::Code(code)))
            .collect();
        decode(&items, &mut CodeBreaker::new(&mut None))
    }

    #[test]
    fn plain_types() {
        assert_eq!(
            read(&[0x0000_A1B2_000A, 0x1000_0100_0007, 0x3202_5E30_0063]),
            Ok(vec![Op::Write {
                address: 0x0202_5E30,
                value: 0x63,
                width: Width::Byte
            }])
        );
        assert_eq!(
            read(&[0xB202_0000_FFFE, 0xD000_0020_0003]),
            Ok(vec![
                Op::If {
                    address: 0x0202_0000,
                    value: 0xFFFE,
                    width: Width::Half,
                    test: Test::Greater,
                    skip: 1
                },
                Op::If {
                    address: KEYINPUT,
                    value: 3,
                    width: Width::Half,
                    test: Test::NoBits,
                    skip: 1
                },
            ])
        );
        assert!(read(&[0xD000_0030_0003]).is_err());
    }

    #[test]
    fn slides_and_patches_take_their_extra_lines() {
        // Four halfwords from 0x02000000, every 4 bytes, from 0x10 by 2.
        assert_eq!(
            read(&[0x4200_0000_0010, 0x0002_0004_0004]),
            Ok(vec![Op::Slide {
                address: 0x0200_0000,
                value: 0x10,
                width: Width::Half,
                count: 4,
                step: 4,
                increment: 2
            }])
        );
        // Four halfwords: eight bytes over two lines.
        assert_eq!(
            read(&[0x5200_0000_0004, 0x1122_3344_5566, 0x7788_0000_0000]),
            Ok(vec![Op::Patch {
                address: 0x0200_0000,
                bytes: vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
            }])
        );
        let error = read(&[0x5200_0000_0004, 0x1122_3344_5566]).expect_err("short");
        assert!(error.1.contains("type 5"), "{}", error.1);
    }

    /// Vectors from an independent transcription of mGBA's CodeBreaker
    /// decryption, which this GBATEK-based one must match.
    #[test]
    fn decrypts_like_mgba() {
        let key = Key::new(0x9123_ABCD, 0x0001);
        for (input, output) in [
            ((0x0000_0000, 0x0000), (0x64EB_DA99, 0x06B4)),
            ((0x1234_5678, 0x9ABC), (0xF2A6_8C75, 0xBA0D)),
            ((0xFFFF_FFFF, 0xFFFF), (0x64EB_DA99, 0x064B)),
        ] {
            assert_eq!(key.decrypt(input.0, input.1), output, "{input:X?}");
        }
        let key = Key::new(0x9F00_BEEF, 0x7A3C);
        assert_eq!(key.decrypt(0x0BAD_F00D, 0x1234), (0x76CF_B98B, 0x540C));
    }
}
