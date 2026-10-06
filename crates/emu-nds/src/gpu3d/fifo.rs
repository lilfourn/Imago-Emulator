//! The geometry command FIFO and its packed-command decoder (GBATEK, "DS 3D
//! Geometry Commands").
//!
//! Every entry is a command byte with one 32-bit parameter; a command
//! without parameters takes one entry. Commands reach the FIFO two ways:
//! GXFIFO (0x04000400) takes up to four packed command bytes followed by
//! their parameters, and the ports at 0x04000440 and up take one entry per
//! write. The FIFO holds 256 entries behind a four-entry PIPE, so 260
//! entries can wait before writes must stall; GXSTAT counts only the FIFO.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Entries in the FIFO proper.
pub(crate) const FIFO_CAPACITY: usize = 256;
/// Entries in the PIPE in front of it.
const PIPE_CAPACITY: usize = 4;
/// Writes past this many waiting entries are dropped. Only reached if the
/// CPU is not stalled while the FIFO is full.
const OVERFLOW_LIMIT: usize = 0x1_0000;

/// SWAP_BUFFERS, which waits for VBlank.
pub(crate) const SWAP_BUFFERS: u8 = 0x50;

/// How many parameters `command` takes, or `None` if it is not a command.
pub(crate) fn param_count(command: u8) -> Option<usize> {
    Some(match command {
        0x11 | 0x15 | 0x41 => 0,
        0x10
        | 0x12..=0x14
        | 0x20..=0x22
        | 0x24..=0x2B
        | 0x30..=0x33
        | 0x40
        | 0x50
        | 0x60
        | 0x72 => 1,
        0x23 | 0x71 => 2,
        0x1B | 0x1C | 0x70 => 3,
        0x1A => 9,
        0x17 | 0x19 => 12,
        0x16 | 0x18 => 16,
        0x34 => 32,
        _ => return None,
    })
}

/// One 40-bit FIFO entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub(crate) command: u8,
    pub(crate) param: u32,
}

/// The FIFO and PIPE, as one queue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Fifo {
    entries: VecDeque<Entry>,
    /// Packed command bytes from GXFIFO still to unpack, next in the low
    /// byte.
    packed: u32,
    /// Parameters GXFIFO still expects for the packed command in the low
    /// byte of `packed`.
    params_left: usize,
}

impl Fifo {
    /// A write to GXFIFO: a packed command word or a parameter.
    pub(crate) fn write_packed(&mut self, value: u32) {
        if self.params_left > 0 {
            self.push(self.packed as u8, value);
            self.params_left -= 1;
            if self.params_left > 0 {
                return;
            }
            self.packed >>= 8;
        } else {
            self.packed = value;
        }
        self.unpack();
    }

    /// Queues packed commands until one needs parameters. Zero bytes
    /// (NOP) and invalid commands take nothing.
    fn unpack(&mut self) {
        while self.packed != 0 {
            let command = self.packed as u8;
            match param_count(command) {
                Some(0) => self.push(command, 0),
                Some(count) => {
                    self.params_left = count;
                    return;
                }
                None => {}
            }
            self.packed >>= 8;
        }
    }

    /// A write to a command port: one entry.
    pub(crate) fn write_port(&mut self, command: u8, param: u32) {
        if param_count(command).is_some() {
            self.push(command, param);
        }
    }

    fn push(&mut self, command: u8, param: u32) {
        if self.entries.len() < OVERFLOW_LIMIT {
            self.entries.push_back(Entry { command, param });
        }
    }

    pub(crate) fn pop(&mut self) -> Option<Entry> {
        self.entries.pop_front()
    }

    /// Entries in the FIFO proper (GXSTAT bits 16–24), not counting the
    /// PIPE, which fills first.
    pub(crate) fn len(&self) -> usize {
        self.entries
            .len()
            .saturating_sub(PIPE_CAPACITY)
            .min(FIFO_CAPACITY)
    }

    #[cfg(test)]
    /// Whether a write now would stall the CPU (or DMA).
    pub(crate) fn is_full(&self) -> bool {
        self.entries.len() >= FIFO_CAPACITY + PIPE_CAPACITY
    }

    /// Whether the PIPE or FIFO holds anything.
    pub(crate) fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }
}

/// Gathers FIFO entries into whole commands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Assembler {
    command: u8,
    params: [u32; 32],
    len: usize,
}

impl Assembler {
    /// Adds `entry`; returns the command and its parameters once all have
    /// arrived. An entry for a different command abandons a partial one.
    pub(crate) fn push(&mut self, entry: Entry) -> Option<(u8, &[u32])> {
        let count = param_count(entry.command)?;
        if count == 0 {
            self.len = 0;
            return Some((entry.command, &[]));
        }
        if self.command != entry.command {
            self.command = entry.command;
            self.len = 0;
        }
        self.params[self.len] = entry.param;
        self.len += 1;
        if self.len < count {
            return None;
        }
        self.len = 0;
        Some((self.command, &self.params[..count]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(fifo: &mut Fifo) -> Vec<(u8, u32)> {
        std::iter::from_fn(|| fifo.pop())
            .map(|entry| (entry.command, entry.param))
            .collect()
    }

    #[test]
    fn packed_commands_unpack_with_their_parameters() {
        let mut fifo = Fifo::default();
        // MTX_MODE(1 param), MTX_PUSH, MTX_IDENTITY, COLOR(1 param).
        fifo.write_packed(0x2015_1110);
        fifo.write_packed(2);
        fifo.write_packed(0x7FFF);
        assert_eq!(
            drain(&mut fifo),
            [(0x10, 2), (0x11, 0), (0x15, 0), (0x20, 0x7FFF)]
        );
        // The next word is a command word again.
        fifo.write_packed(0x0000_0041);
        assert_eq!(drain(&mut fifo), [(0x41, 0)]);
    }

    #[test]
    fn nops_and_invalid_commands_take_no_parameters() {
        let mut fifo = Fifo::default();
        fifo.write_packed(0x0015_FF00);
        assert_eq!(drain(&mut fifo), [(0x15, 0)]);
        fifo.write_port(0x3F, 1);
        assert!(!fifo.has_entries());
    }

    #[test]
    fn the_pipe_fills_before_the_fifo_counts() {
        let mut fifo = Fifo::default();
        for _ in 0..4 {
            fifo.write_port(0x11, 0);
        }
        assert_eq!(fifo.len(), 0);
        assert!(fifo.has_entries());
        for _ in 0..256 {
            fifo.write_port(0x11, 0);
        }
        assert_eq!(fifo.len(), 256);
        assert!(fifo.is_full());
    }

    #[test]
    fn assembler_waits_for_all_parameters() {
        let mut assembler = Assembler::default();
        let entry = |param| Entry {
            command: 0x1B,
            param,
        };
        assert!(assembler.push(entry(1)).is_none());
        assert!(assembler.push(entry(2)).is_none());
        let (command, params) = assembler.push(entry(3)).expect("complete");
        assert_eq!((command, params), (0x1B, &[1, 2, 3][..]));
    }
}
