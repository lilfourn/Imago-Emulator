//! Cheat codes.
//!
//! A cheat list is plain text, kept next to the ROM as `game.cht`:
//!
//! ```text
//! # Comments start with "#".
//! [Infinite money]
//! 82025E30 270F
//! [Walk through walls] off
//! 02000F04:01
//! ```
//!
//! Each cheat is a `[name]` line, optionally followed by `off`, then its
//! codes, one per line. [`parse`] turns the text into [`Cheat`]s whose codes
//! are plain memory operations ([`Op`]), and [`apply`] runs the enabled ones
//! against a [`Memory`], once per frame, the way cheat devices did.
//!
//! Formats, by system:
//!
//! - every system: raw `ADDRESS:VALUE` in hex, writing 1, 2 or 4 bytes
//!   (little-endian) by the value's digit count;
//! - Game Boy: GameShark `01VVAAAA`;
//! - Game Boy Advance: CodeBreaker `XAAAAAAA YYYY`, unencrypted.
//!
//! Encrypted Action Replay and GameShark Advance codes, Game Genie ROM
//! patches and Action Replay DS codes are not supported yet; parsing them
//! returns an error naming the format.

mod code;

use emu_core::System;

/// Memory a cheat reads and writes: one player's CPU address space.
pub trait Memory {
    /// The byte at `address`, or `None` if it is not plain memory.
    fn peek(&self, address: u32) -> Option<u8>;

    /// Writes the byte at `address`. Returns whether it was plain memory.
    fn poke(&mut self, address: u32, value: u8) -> bool;
}

/// How many bytes an operation reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// One byte.
    Byte = 1,
    /// Two bytes.
    Half = 2,
    /// Four bytes.
    Word = 4,
}

/// One memory operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Stores `value`.
    Write {
        /// Where.
        address: u32,
        /// What.
        value: u32,
        /// How many bytes.
        width: Width,
    },
    /// Combines the value in memory with `value` and stores the result.
    Modify {
        /// Where.
        address: u32,
        /// The other operand.
        value: u32,
        /// How many bytes.
        width: Width,
        /// How to combine them.
        with: Modify,
    },
    /// Runs the next operation only if the value in memory passes `test`.
    If {
        /// Where.
        address: u32,
        /// The value to compare with.
        value: u32,
        /// How many bytes.
        width: Width,
        /// The comparison.
        test: Test,
    },
}

/// How [`Op::Modify`] combines values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modify {
    /// Bitwise OR.
    Or,
    /// Bitwise AND.
    And,
    /// Wrapping addition.
    Add,
}

/// How [`Op::If`] compares values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Test {
    /// Memory equals the value.
    Equal,
    /// Memory differs from the value.
    NotEqual,
}

/// A named group of codes, switched on and off together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cheat {
    /// The name from the `[name]` line.
    pub name: String,
    /// Whether [`apply`] runs it.
    pub enabled: bool,
    /// Its codes, in order.
    pub ops: Vec<Op>,
}

/// A line of a cheat list that could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct ParseError {
    /// The line number, from 1.
    pub line: usize,
    /// What is wrong with it.
    pub message: String,
}

/// Reads a cheat list for `system`. Codes before the first `[name]` line
/// form a cheat named "Unnamed".
///
/// # Errors
///
/// Returns the first line that is neither a comment, a name nor a code in a
/// supported format.
pub fn parse(system: System, text: &str) -> Result<Vec<Cheat>, ParseError> {
    let mut cheats: Vec<Cheat> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let error = |message: String| ParseError {
            line: index + 1,
            message,
        };
        if let Some(rest) = line.strip_prefix('[') {
            let (name, flags) = rest
                .split_once(']')
                .ok_or_else(|| error("a cheat name needs a closing \"]\"".to_owned()))?;
            let enabled = match flags.trim() {
                "" | "on" => true,
                "off" => false,
                other => {
                    return Err(error(format!(
                        "expected \"on\" or \"off\", found {other:?}"
                    )));
                }
            };
            cheats.push(Cheat {
                name: name.trim().to_owned(),
                enabled,
                ops: Vec::new(),
            });
            continue;
        }
        let op = code::parse(system, line).map_err(error)?;
        if cheats.is_empty() {
            cheats.push(Cheat {
                name: "Unnamed".to_owned(),
                enabled: true,
                ops: Vec::new(),
            });
        }
        if let (Some(op), Some(cheat)) = (op, cheats.last_mut()) {
            cheat.ops.push(op);
        }
    }
    Ok(cheats)
}

/// Runs every enabled cheat's operations against `memory`.
pub fn apply(cheats: &[Cheat], memory: &mut impl Memory) {
    for cheat in cheats.iter().filter(|cheat| cheat.enabled) {
        let mut skip = false;
        for op in &cheat.ops {
            if std::mem::take(&mut skip) {
                continue;
            }
            match *op {
                Op::Write {
                    address,
                    value,
                    width,
                } => write(memory, address, value, width),
                Op::Modify {
                    address,
                    value,
                    width,
                    with,
                } => {
                    if let Some(old) = read(memory, address, width) {
                        let new = match with {
                            Modify::Or => old | value,
                            Modify::And => old & value,
                            Modify::Add => old.wrapping_add(value),
                        };
                        write(memory, address, new, width);
                    }
                }
                Op::If {
                    address,
                    value,
                    width,
                    test,
                } => {
                    let passed = read(memory, address, width).is_some_and(|old| match test {
                        Test::Equal => old == value,
                        Test::NotEqual => old != value,
                    });
                    skip = !passed;
                }
            }
        }
    }
}

fn read(memory: &impl Memory, address: u32, width: Width) -> Option<u32> {
    (0..width as u32).try_fold(0, |value, byte| {
        let next = memory.peek(address.wrapping_add(byte))?;
        Some(value | u32::from(next) << (byte * 8))
    })
}

fn write(memory: &mut impl Memory, address: u32, value: u32, width: Width) {
    for byte in 0..width as u32 {
        memory.poke(address.wrapping_add(byte), (value >> (byte * 8)) as u8);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Sparse RAM at 0x02000000-0x0203FFFF; the rest is not memory.
    #[derive(Default)]
    struct Ram(HashMap<u32, u8>);

    impl Memory for Ram {
        fn peek(&self, address: u32) -> Option<u8> {
            (0x0200_0000..0x0204_0000)
                .contains(&address)
                .then(|| self.0.get(&address).copied().unwrap_or(0))
        }

        fn poke(&mut self, address: u32, value: u8) -> bool {
            let ram = self.peek(address).is_some();
            if ram {
                self.0.insert(address, value);
            }
            ram
        }
    }

    #[test]
    fn reads_names_flags_and_codes() {
        let text = "# money\n[Money]\n82025E30 270F\n\n[Moon jump] off\n02000F04:01\n";
        let cheats = parse(System::Gba, text).expect("valid list");
        assert_eq!(cheats.len(), 2);
        assert_eq!(
            (cheats[0].name.as_str(), cheats[0].enabled),
            ("Money", true)
        );
        assert_eq!(
            cheats[0].ops,
            [Op::Write {
                address: 0x0202_5E30,
                value: 0x270F,
                width: Width::Half
            }]
        );
        assert!(!cheats[1].enabled);
    }

    #[test]
    fn errors_name_the_line() {
        let error = parse(System::Gba, "[Money]\nnot a code\n").expect_err("bad code");
        assert_eq!(error.line, 2);
    }

    #[test]
    fn applies_enabled_cheats_and_conditions() {
        let text = "[On]\n02000000:1234\n[Off] off\n02000010:FF\n\
                    [Guarded]\n72000000 0000\n32000020 0055\n72000000 1234\n32000021 0066\n";
        let cheats = parse(System::Gba, text).expect("valid list");
        let mut ram = Ram::default();
        apply(&cheats, &mut ram);
        assert_eq!(read(&ram, 0x0200_0000, Width::Half), Some(0x1234));
        assert_eq!(ram.peek(0x0200_0010), Some(0));
        // The first condition fails, so its write is skipped; the second passes.
        assert_eq!(ram.peek(0x0200_0020), Some(0));
        assert_eq!(ram.peek(0x0200_0021), Some(0x66));
    }

    #[test]
    fn modifies_in_place() {
        let cheats = parse(System::Gba, "22000000 00F0\nE2000002 0002\n").expect("valid");
        let mut ram = Ram::default();
        write(&mut ram, 0x0200_0000, 0x000F, Width::Half);
        write(&mut ram, 0x0200_0002, 0xFFFF, Width::Half);
        apply(&cheats, &mut ram);
        assert_eq!(read(&ram, 0x0200_0000, Width::Half), Some(0x00FF));
        assert_eq!(read(&ram, 0x0200_0002, Width::Half), Some(0x0001));
    }
}
