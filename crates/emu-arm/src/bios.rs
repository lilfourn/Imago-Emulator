//! The BIOS routines the Game Boy Advance and the Nintendo DS have in
//! common, for the cores' emulated BIOSes (GBATEK, "BIOS Decompression
//! Functions"). No Nintendo code is used.
//!
//! A routine reaches memory only through the [`Bus`] it is given, an
//! access at a time and in a fixed order, so a console that counts time by
//! bus accesses (the GBA) spends the same time whichever core calls it.
//!
//! The decoders ([`Lz77`], [`RunLength`], [`Huffman`]) take their
//! compressed data one byte or word at a time and keep all of their
//! progress in themselves and in an [`Output`], so one can stop between
//! any two units: the DS's ReadByCallback functions get each unit from a
//! function of the game's, and a save state may be made in between. These
//! types are part of a DS save state, so their fields, and the order of
//! them, are that state's layout.

use serde::{Deserialize, Serialize};

use crate::{Access, Bus};

/// Where decompressed bytes go, written a byte, halfword or word at a time
/// as the BIOS function does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// Where the next write goes.
    destination: u32,
    /// Bytes per write: 1, 2 (the "Vram" functions) or 4 (Huffman).
    width: u32,
    /// Bytes collected toward the next write, the first in the low bits.
    pending: u32,
    pending_bytes: u32,
    /// Bytes still to produce.
    remaining: u32,
}

impl Output {
    /// An output of `length` bytes to `destination`, written `width` bytes
    /// at a time: 1, 2 or 4.
    #[must_use]
    pub fn new(destination: u32, width: u32, length: u32) -> Self {
        Self {
            destination,
            width,
            pending: 0,
            pending_bytes: 0,
            remaining: length,
        }
    }

    /// Whether this output, read from a save state, writes in units the
    /// BIOS does, with less than a unit collected.
    #[must_use]
    pub fn in_range(&self) -> bool {
        matches!(self.width, 1 | 2 | 4) && self.pending_bytes < self.width
    }

    /// Whether every byte has been produced.
    #[must_use]
    pub fn done(&self) -> bool {
        self.remaining == 0
    }

    /// Produces one byte, writing once a whole unit has been collected.
    /// Bytes past the declared length are dropped, and so is a last unit
    /// left incomplete: the halfword functions never write an odd final
    /// byte.
    fn push<B: Bus>(&mut self, bus: &mut B, byte: u8) {
        if self.remaining == 0 {
            return;
        }
        self.remaining -= 1;
        self.pending |= u32::from(byte) << (8 * self.pending_bytes);
        self.pending_bytes += 1;
        if self.pending_bytes < self.width {
            return;
        }
        match self.width {
            1 => bus.write_8(self.destination, self.pending as u8, Access::NONSEQUENTIAL),
            2 => bus.write_16(self.destination, self.pending as u16, Access::NONSEQUENTIAL),
            _ => bus.write_32(self.destination, self.pending, Access::NONSEQUENTIAL),
        }
        self.destination = self.destination.wrapping_add(self.width);
        self.pending = 0;
        self.pending_bytes = 0;
    }

    /// The byte `distance` back from the next one, read from the
    /// destination as the BIOS reads it. A byte still waiting to be written
    /// as half of a halfword is not there yet, so a distance of 1 reads
    /// whatever the destination held before: GBATEK's caution that the
    /// "Vram" LZ77 functions need Disp of at least 1.
    fn earlier<B: Bus>(&self, bus: &mut B, distance: u32) -> u8 {
        let next = self.destination.wrapping_add(self.pending_bytes);
        bus.read_8(next.wrapping_sub(distance), Access::NONSEQUENTIAL)
    }
}

/// LZ77: a flag byte, then eight blocks, each a literal byte or (flag bit
/// set) a two-byte reference to bytes already written: length − 3 in the
/// top nibble, then distance − 1 in the remaining 12 bits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lz77 {
    /// Type flags for the blocks left in this group, the next in bit 7.
    flags: u8,
    /// Blocks left in the group; at 0, a flag byte comes next.
    blocks: u8,
    /// A reference's first byte, while waiting for its second.
    reference: Option<u8>,
}

impl Lz77 {
    /// Takes the next byte of compressed data.
    pub fn feed<B: Bus>(&mut self, byte: u8, out: &mut Output, bus: &mut B) {
        if self.blocks == 0 {
            self.flags = byte;
            self.blocks = 8;
            return;
        }
        if self.flags & 0x80 == 0 {
            out.push(bus, byte);
        } else {
            let Some(high) = self.reference.take() else {
                self.reference = Some(byte);
                return;
            };
            let length = (high >> 4) + 3;
            let distance = (u32::from(high & 0xF) << 8 | u32::from(byte)) + 1;
            for _ in 0..length {
                if out.done() {
                    break;
                }
                let byte = out.earlier(bus, distance);
                out.push(bus, byte);
            }
        }
        self.flags <<= 1;
        self.blocks -= 1;
    }
}

/// Run-length: a flag byte, then either N literal bytes (bit 7 clear,
/// N = bits 0–6 + 1) or one byte to repeat N times (bit 7 set, N = bits
/// 0–6 + 3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLength {
    /// Bytes left in the current run; at 0, a flag byte comes next.
    left: u8,
    /// Whether the run repeats its one data byte.
    repeat: bool,
}

impl RunLength {
    /// Takes the next byte of compressed data.
    pub fn feed<B: Bus>(&mut self, byte: u8, out: &mut Output, bus: &mut B) {
        if self.left == 0 {
            self.repeat = byte & 0x80 != 0;
            self.left = (byte & 0x7F) + if self.repeat { 3 } else { 1 };
            return;
        }
        if self.repeat {
            for _ in 0..self.left {
                out.push(bus, byte);
            }
            self.left = 0;
        } else {
            out.push(bus, byte);
            self.left -= 1;
        }
    }
}

/// Huffman: a tree of nodes, then a bitstream in words, read from bit 31
/// down, that walks it from the root to a symbol at a time. The tree comes
/// first as a size byte (bytes / 2 − 1, counting itself) and the nodes,
/// which the BIOS copies to the game's buffer to read them out of order.
///
/// A node holds the offset to its pair of children in bits 0–5: child 0
/// is at (node address & !1) + offset × 2 + 2, and child 1 just after.
/// Bit 7 (child 0) and bit 6 (child 1) mark a child as a symbol rather
/// than another node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Huffman {
    /// Bits per symbol, normally 4 or 8.
    symbol_bits: u32,
    /// The tree's copy: its size byte, then the root node.
    tree: u32,
    /// Tree bytes copied so far, and the tree's length once its size byte
    /// is in.
    copied: u32,
    tree_length: u32,
    /// The node the next bit picks a child of.
    node: u32,
    /// Symbols collected toward the next byte, the first in the low bits.
    symbols: u32,
    symbols_bits: u32,
}

impl Huffman {
    /// A decoder with `symbol_bits`-bit symbols (the header's bits 0–3)
    /// that copies the tree to `tree`.
    #[must_use]
    pub fn new(symbol_bits: u32, tree: u32) -> Self {
        Self {
            symbol_bits,
            tree,
            copied: 0,
            tree_length: 0,
            node: tree.wrapping_add(1),
            symbols: 0,
            symbols_bits: 0,
        }
    }

    /// Whether this decoder, read from a save state, holds only counts it
    /// could have reached, since the code shifts by them: symbols of up to
    /// 15 bits (a header's low nibble), less than a byte of them
    /// collected, and a tree of 512 bytes at most.
    #[must_use]
    pub fn in_range(&self) -> bool {
        self.symbol_bits < 16
            && self.symbols_bits < 8
            && self.tree_length <= 512
            && self.copied <= self.tree_length.max(1)
    }

    /// Whether the tree is still being copied, a byte at a time, before
    /// the bitstream's words.
    #[must_use]
    pub fn copying_tree(&self) -> bool {
        self.copied == 0 || self.copied < self.tree_length
    }

    /// Takes the next byte of the tree: its size byte first.
    pub fn copy_tree<B: Bus>(&mut self, byte: u8, bus: &mut B) {
        if self.copied == 0 {
            self.tree_length = (u32::from(byte) + 1) * 2;
        }
        bus.write_8(
            self.tree.wrapping_add(self.copied),
            byte,
            Access::NONSEQUENTIAL,
        );
        self.copied += 1;
    }

    /// Follows one word of the bitstream, stopping once the output is
    /// complete.
    pub fn feed<B: Bus>(&mut self, bits: u32, out: &mut Output, bus: &mut B) {
        for bit in (0..32).rev() {
            if out.done() {
                return;
            }
            let node = u32::from(bus.read_8(self.node, Access::NONSEQUENTIAL));
            let one = (bits >> bit) & 1;
            let child = (self.node & !1).wrapping_add((node & 0x3F) * 2 + 2 + one);
            if node & (0x80 >> one) == 0 {
                self.node = child;
                continue;
            }
            let symbol = u32::from(bus.read_8(child, Access::NONSEQUENTIAL));
            self.symbols |= (symbol & ((1 << self.symbol_bits) - 1)) << self.symbols_bits;
            self.symbols_bits += self.symbol_bits;
            while self.symbols_bits >= 8 {
                out.push(bus, self.symbols as u8);
                self.symbols >>= 8;
                self.symbols_bits -= 8;
            }
            self.node = self.tree.wrapping_add(1);
        }
    }
}
