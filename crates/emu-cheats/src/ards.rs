//! Action Replay DS codes (GBATEK, "DS Cart Cheat Action Replay DS";
//! EnHacklopedia, "Hacking Nintendo DS").
//!
//! An Action Replay DS cheat is a small program. It runs top to bottom
//! with an `offset` and a data register, both zero at the start of every
//! cheat, a stack of execution statuses for nested `IF`s, and one loop:
//!
//! ```text
//! 0XXXXXXX YYYYYYYY  word [X+offset] = Y        3–6: IF Y >, <, =, ≠ word [X]
//! 1XXXXXXX 0000YYYY  half [X+offset] = Y        7–A: IF Y >, <, =, ≠ (half [X] AND NOT Z)
//! 2XXXXXXX 000000YY  byte [X+offset] = Y             (ZZZZYYYY; X = 0 reads [offset])
//! BXXXXXXX 00000000  offset = word [X+offset]   C0000000 YYYYYYYY  FOR: run to NEXT Y+1 times
//! D0 ENDIF  D1 NEXT  D2 NEXT, then reset everything
//! D3 offset = Y  D4 data += Y  D5 data = Y  D6/D7/D8 store data at [Y+offset], offset += 4/2/1
//! D9/DA/DB data = word/half/byte [Y+offset]  DC offset += Y
//! EXXXXXXX YYYYYYYY  copy the Y bytes in the lines that follow to [X+offset]
//! FXXXXXXX YYYYYYYY  copy Y bytes from [offset] to [X]
//! ```
//!
//! Choices where the device and other emulators differ:
//! - firmware v1.54 behaviour: `IF`s with X = 0 read `[offset]`, and `DB`
//!   doesn't add to `offset`;
//! - an `IF` skipped inside a false block still opens a level, so its own
//!   `D0` closes it and the outer block stays skipped;
//! - every `NEXT` restores the statuses saved by `FOR`, as GBATEK says;
//! - `C4`, `C5` and `C6` (v1.54 additions, unused by Pokémon lists) and
//!   melonDS's extended `D4` operations are parse errors;
//! - unreadable memory reads as zero, and fails every `IF`;
//! - word and halfword accesses ignore the low address bits, as the ARM's
//!   do.

use crate::Memory;
use crate::codes::Item;
use crate::decode::halves;
use crate::ops::{self, Width};

/// Steps one cheat may run per frame; a runaway loop stops here.
const STEP_LIMIT: u32 = 1 << 20;

/// The most bytes one `F` code copies: all of main RAM.
const MAX_COPY: u32 = 0x40_0000;

/// A checked Action Replay DS cheat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Program {
    /// Each code as its two words, `E` data lines included.
    codes: Vec<[u32; 2]>,
}

/// Lines of data after an `E` code copying `bytes` bytes.
fn data_lines(bytes: u32) -> usize {
    bytes.div_ceil(8) as usize
}

impl Program {
    /// Checks a cheat's codes.
    ///
    /// # Errors
    ///
    /// Returns the line and reason for a raw patch, an unsupported or
    /// unknown code type, or an `E` code missing data lines.
    pub(crate) fn new(items: &[(usize, Item)]) -> Result<Self, (usize, String)> {
        let mut codes = Vec::with_capacity(items.len());
        let mut data = 0;
        for &(line, ref item) in items {
            let Item::Code(code) = *item else {
                return Err((
                    line,
                    "raw ADDRESS:VALUE patches can't be mixed with Action Replay DS codes; \
                     give them a cheat of their own"
                        .to_owned(),
                ));
            };
            let (left, right) = halves(code);
            codes.push([left, right]);
            if data > 0 {
                data -= 1;
                continue;
            }
            match left >> 24 {
                0xC4..=0xC6 => {
                    return Err((
                        line,
                        format!("code type {:02X} is not supported yet", left >> 24),
                    ));
                }
                0xD4 if left & 0x00FF_FFFF != 0 => {
                    return Err((
                        line,
                        "D4 with an operation in its first word (a melonDS extension) \
                         is not supported"
                            .to_owned(),
                    ));
                }
                0xC1..=0xCF | 0xDD..=0xDF => {
                    return Err((line, format!("unknown code type {:02X}", left >> 24)));
                }
                0xC0 | 0xD0..=0xDC => {}
                _ if left >> 28 == 0xE => {
                    data = data_lines(right);
                    let left_in_cheat = items.len() - codes.len();
                    if data > left_in_cheat {
                        return Err((
                            line,
                            format!(
                                "an E code copying {right} bytes needs {data} data lines \
                                 after it; the cheat has {left_in_cheat}"
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(Self { codes })
    }

    /// Runs the cheat once against `memory`.
    pub(crate) fn run(&self, memory: &mut (impl Memory + ?Sized)) {
        let mut machine = Machine::new();
        let mut next = 0;
        for _ in 0..STEP_LIMIT {
            let Some(&[left, right]) = self.codes.get(next) else {
                break;
            };
            next = machine.step(left, right, next + 1, &self.codes, memory);
        }
    }
}

/// The execution status and the saved ones of the `IF`s around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Conditions {
    executing: bool,
    /// One bit per open `IF`, the innermost lowest: the status before it.
    saved: u32,
    /// Open `IF`s, up to the 32 the device keeps.
    depth: u8,
}

impl Conditions {
    const fn new() -> Self {
        Self {
            executing: true,
            saved: 0,
            depth: 0,
        }
    }

    /// Opens an `IF`: codes run on only if they ran before and `passed`.
    fn open(&mut self, passed: bool) {
        self.saved = self.saved << 1 | u32::from(self.executing);
        self.depth = (self.depth + 1).min(32);
        self.executing &= passed;
    }

    /// `D0`: back to the status before the innermost `IF`, or to running
    /// codes if none is open.
    fn close(&mut self) {
        if self.depth == 0 {
            self.executing = true;
            return;
        }
        self.executing = self.saved & 1 != 0;
        self.saved >>= 1;
        self.depth -= 1;
    }
}

/// A `FOR` loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Loop {
    /// The code after the `C0`.
    start: usize,
    /// Repeats still to run.
    left: u32,
    /// The statuses when `C0` ran, which every `NEXT` restores.
    conditions: Conditions,
}

/// The device's registers while a cheat runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Machine {
    offset: u32,
    data: u32,
    conditions: Conditions,
    repeat: Option<Loop>,
}

fn read(memory: &(impl Memory + ?Sized), address: u32, width: Width) -> Option<u32> {
    ops::read(memory, address & !(width as u32 - 1), width)
}

fn write(memory: &mut (impl Memory + ?Sized), address: u32, value: u32, width: Width) {
    ops::write(
        memory,
        address & !(width as u32 - 1),
        value & width.mask(),
        width,
    );
}

impl Machine {
    const fn new() -> Self {
        Self {
            offset: 0,
            data: 0,
            conditions: Conditions::new(),
            repeat: None,
        }
    }

    /// `NEXT`: restores the `FOR`'s statuses and returns where to repeat
    /// from, or ends the loop.
    fn next_iteration(&mut self) -> Option<usize> {
        let repeat = self.repeat.as_mut()?;
        self.conditions = repeat.conditions;
        if repeat.left == 0 {
            self.repeat = None;
            return None;
        }
        repeat.left -= 1;
        Some(repeat.start)
    }

    /// Runs one code, `next` being the index after it. Returns the index of
    /// the code to run next.
    fn step(
        &mut self,
        left: u32,
        right: u32,
        next: usize,
        codes: &[[u32; 2]],
        memory: &mut (impl Memory + ?Sized),
    ) -> usize {
        // D0, D1 and D2 run whatever the execution status.
        match left >> 24 {
            0xD0 => {
                self.conditions.close();
                return next;
            }
            0xD1 => return self.next_iteration().unwrap_or(next),
            0xD2 => {
                if let Some(start) = self.next_iteration() {
                    return start;
                }
                *self = Self::new();
                return next;
            }
            _ => {}
        }
        let kind = left >> 28;
        if !self.conditions.executing {
            match kind {
                0x3..=0xA => self.conditions.open(false),
                0xE => return next + data_lines(right),
                _ => {}
            }
            return next;
        }
        let x = left & 0x0FFF_FFFF;
        let at = x.wrapping_add(self.offset);
        match kind {
            0x0 => write(memory, at, right, Width::Word),
            0x1 => write(memory, at, right, Width::Half),
            0x2 => write(memory, at, right, Width::Byte),
            0x3..=0x6 => {
                let address = if x == 0 { self.offset } else { x };
                let passed = read(memory, address, Width::Word).is_some_and(|word| match kind {
                    0x3 => right > word,
                    0x4 => right < word,
                    0x5 => right == word,
                    _ => right != word,
                });
                self.conditions.open(passed);
            }
            0x7..=0xA => {
                let address = if x == 0 { self.offset } else { x };
                let (mask, value) = (right >> 16, right & 0xFFFF);
                let passed = read(memory, address, Width::Half).is_some_and(|half| {
                    let half = half & !mask;
                    match kind {
                        0x7 => value > half,
                        0x8 => value < half,
                        0x9 => value == half,
                        _ => value != half,
                    }
                });
                self.conditions.open(passed);
            }
            0xB => self.offset = read(memory, at, Width::Word).unwrap_or(0),
            // Only C0 gets past `Program::new`.
            0xC => {
                self.repeat = Some(Loop {
                    start: next,
                    left: right,
                    conditions: self.conditions,
                });
            }
            0xD => self.data_code(left >> 24, right, memory),
            0xE => {
                let lines = data_lines(right);
                let bytes = codes
                    .get(next..next + lines)
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|&[low, high]| {
                        let [a, b, c, d] = low.to_le_bytes();
                        let [e, f, g, h] = high.to_le_bytes();
                        [a, b, c, d, e, f, g, h]
                    });
                for (index, byte) in (0..right).zip(bytes) {
                    memory.poke(at.wrapping_add(index), byte);
                }
                return next + lines;
            }
            _ => {
                for index in 0..right.min(MAX_COPY) {
                    if let Some(byte) = memory.peek(self.offset.wrapping_add(index)) {
                        memory.poke(x.wrapping_add(index), byte);
                    }
                }
            }
        }
        next
    }

    /// D3–DC, which take their address or value from the second word.
    fn data_code(&mut self, kind: u32, right: u32, memory: &mut (impl Memory + ?Sized)) {
        let at = right.wrapping_add(self.offset);
        match kind {
            0xD3 => self.offset = right,
            0xD4 => self.data = self.data.wrapping_add(right),
            0xD5 => self.data = right,
            0xD6 => {
                write(memory, at, self.data, Width::Word);
                self.offset = self.offset.wrapping_add(4);
            }
            0xD7 => {
                write(memory, at, self.data, Width::Half);
                self.offset = self.offset.wrapping_add(2);
            }
            0xD8 => {
                write(memory, at, self.data, Width::Byte);
                self.offset = self.offset.wrapping_add(1);
            }
            0xD9 => self.data = read(memory, at, Width::Word).unwrap_or(0),
            0xDA => self.data = read(memory, at, Width::Half).unwrap_or(0),
            0xDB => self.data = read(memory, at, Width::Byte).unwrap_or(0),
            0xDC => self.offset = self.offset.wrapping_add(right),
            _ => {}
        }
    }
}
