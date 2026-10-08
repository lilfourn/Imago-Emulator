//! Memory operations for the formats that are flat lists of codes: raw
//! patches, GameShark, Game Genie, CodeBreaker, GameShark Advance and Pro
//! Action Replay. Each code becomes one operation, so "skip the next n
//! codes" skips n operations.

use emu_core::RomPatch;

use crate::Memory;

/// Fills and slides stop after this many writes: the GBA's whole EWRAM.
const MAX_SLIDE: u32 = 0x4_0000;

/// How many bytes an operation reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Width {
    /// One byte.
    Byte = 1,
    /// Two bytes.
    Half = 2,
    /// Four bytes.
    Word = 4,
}

impl Width {
    /// The bits a value of this width has.
    pub(crate) fn mask(self) -> u32 {
        match self {
            Self::Byte => 0xFF,
            Self::Half => 0xFFFF,
            Self::Word => u32::MAX,
        }
    }

    fn signed(self, value: u32) -> i32 {
        match self {
            Self::Byte => i32::from(value as u8 as i8),
            Self::Half => i32::from(value as u16 as i16),
            Self::Word => value as i32,
        }
    }
}

/// One code's memory operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
    /// Stores `value` at `address`.
    Write {
        address: u32,
        value: u32,
        width: Width,
    },
    /// Stores `value` at `address`, then `count - 1` more times, each
    /// `step` bytes further on and `increment` larger: fills and slides.
    Slide {
        address: u32,
        value: u32,
        width: Width,
        count: u32,
        step: u32,
        increment: u32,
    },
    /// Stores `value` at each of `addresses`.
    WriteEach {
        addresses: Vec<u32>,
        value: u32,
        width: Width,
    },
    /// Stores `bytes` from `address` on.
    Patch { address: u32, bytes: Vec<u8> },
    /// Stores `value` at `address` in RAM bank `bank` of the switchable
    /// region holding it, whichever bank is mapped (Game Boy).
    WriteBank { bank: u8, address: u32, value: u8 },
    /// Stores `value` at the word read from `pointer`, plus `offset`.
    WriteIndirect {
        pointer: u32,
        offset: u32,
        value: u32,
        width: Width,
    },
    /// Combines the value in memory with `value` and stores the result.
    Modify {
        address: u32,
        value: u32,
        width: Width,
        with: Modify,
    },
    /// Skips the next `skip` operations unless the value in memory passes
    /// `test`. An unreadable address fails every test.
    If {
        address: u32,
        value: u32,
        width: Width,
        test: Test,
        skip: usize,
    },
    /// Skips the next operations; `usize::MAX` ends the cheat.
    Skip(usize),
    /// Stores `value` in the I/O registers at `address`, as the CPU would:
    /// a halfword, or a word as two halfwords, low first (GBA). `width` is
    /// never a byte.
    WriteIo {
        address: u32,
        value: u32,
        width: Width,
    },
    /// Changes what the game reads from ROM while the cheat is on. The
    /// frontend hands these to the core (see [`crate::rom_patches`]), so
    /// running the operation does nothing and conditions don't gate it.
    PatchRom(Vec<RomPatch>),
}

/// How [`Op::Modify`] combines values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modify {
    Or,
    And,
    Add,
}

/// How [`Op::If`] compares memory (left) with its value (right).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Test {
    Equal,
    NotEqual,
    /// Signed.
    Less,
    /// Signed.
    Greater,
    LessUnsigned,
    GreaterUnsigned,
    /// Memory shares a set bit with the value.
    AnyBits,
    /// Memory has none of the value's bits set.
    NoBits,
    Never,
}

impl Test {
    fn passes(self, memory: u32, value: u32, width: Width) -> bool {
        match self {
            Self::Equal => memory == value,
            Self::NotEqual => memory != value,
            Self::Less => width.signed(memory) < width.signed(value),
            Self::Greater => width.signed(memory) > width.signed(value),
            Self::LessUnsigned => memory < value,
            Self::GreaterUnsigned => memory > value,
            Self::AnyBits => memory & value != 0,
            Self::NoBits => memory & value == 0,
            Self::Never => false,
        }
    }
}

/// Patches the little-endian halfword at ROM `address` (GBA codes).
pub(crate) fn patch_rom_half(address: u32, value: u16) -> Op {
    let [low, high] = value.to_le_bytes();
    let byte = |address, value| RomPatch {
        address,
        value,
        compare: None,
    };
    Op::PatchRom(vec![
        byte(address, low),
        byte(address.wrapping_add(1), high),
    ])
}

/// Runs `ops` in order against `memory`.
pub(crate) fn run(ops: &[Op], memory: &mut (impl Memory + ?Sized)) {
    let mut next = 0;
    while let Some(op) = ops.get(next) {
        next += 1;
        match op {
            &Op::Write {
                address,
                value,
                width,
            } => write(memory, address, value, width),
            &Op::Slide {
                address,
                value,
                width,
                count,
                step,
                increment,
            } => {
                for index in 0..count.min(MAX_SLIDE) {
                    let at = address.wrapping_add(index.wrapping_mul(step));
                    let value = value.wrapping_add(index.wrapping_mul(increment));
                    write(memory, at, value, width);
                }
            }
            Op::WriteEach {
                addresses,
                value,
                width,
            } => {
                for &address in addresses {
                    write(memory, address, *value, *width);
                }
            }
            Op::Patch { address, bytes } => {
                for (index, &byte) in (0u32..).zip(bytes) {
                    memory.poke(address.wrapping_add(index), byte);
                }
            }
            &Op::WriteBank {
                bank,
                address,
                value,
            } => {
                memory.poke_bank(bank, address, value);
            }
            &Op::WriteIndirect {
                pointer,
                offset,
                value,
                width,
            } => {
                if let Some(base) = read(memory, pointer, Width::Word) {
                    write(memory, base.wrapping_add(offset), value, width);
                }
            }
            &Op::Modify {
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
            &Op::If {
                address,
                value,
                width,
                test,
                skip,
            } => {
                let passed =
                    read(memory, address, width).is_some_and(|old| test.passes(old, value, width));
                if !passed {
                    next = next.saturating_add(skip);
                }
            }
            &Op::Skip(skip) => next = next.saturating_add(skip),
            &Op::WriteIo {
                address,
                value,
                width,
            } => write_io(memory, address, value, width),
            Op::PatchRom(_) => {}
        }
    }
}

/// The little-endian value at `address`, or `None` if any byte is
/// unreadable.
pub(crate) fn read(memory: &(impl Memory + ?Sized), address: u32, width: Width) -> Option<u32> {
    (0..width as u32).try_fold(0, |value, byte| {
        let next = memory.peek(address.wrapping_add(byte))?;
        Some(value | u32::from(next) << (byte * 8))
    })
}

/// Stores `value` in the I/O registers at `address`, aligned as the CPU
/// aligns a store of `width`, a halfword at a time, low first. No code
/// writes a lone I/O byte, so a byte width writes nothing.
fn write_io(memory: &mut (impl Memory + ?Sized), address: u32, value: u32, width: Width) {
    let aligned = address & !(width as u32 - 1);
    for half in 0..width as u32 / 2 {
        let at = aligned.wrapping_add(half * 2);
        memory.poke_io_16(at, (value >> (half * 16)) as u16);
    }
}

/// Stores `value` little-endian at `address`, skipping unwritable bytes.
pub(crate) fn write(memory: &mut (impl Memory + ?Sized), address: u32, value: u32, width: Width) {
    for byte in 0..width as u32 {
        memory.poke(address.wrapping_add(byte), (value >> (byte * 8)) as u8);
    }
}
