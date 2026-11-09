//! The BIOS's LZ77, run-length and Huffman formats (GBATEK, "BIOS
//! Decompression Functions"), as decoders that take their compressed data
//! one byte or word at a time.
//!
//! The ReadNormal functions feed a decoder from memory ([`read_normal`]).
//! The ReadByCallback ones feed it from the game's own functions, one call
//! per unit, so a decoder keeps all of its progress in itself and can stop
//! between any two units (see `callback.rs`).

use emu_arm::{Access, Bus};
use serde::{Deserialize, Serialize};

/// Where decompressed bytes go, written a byte, halfword or word at a time
/// as the BIOS function does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Output {
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
    pub(crate) fn new(destination: u32, width: u32, length: u32) -> Self {
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
    fn in_range(&self) -> bool {
        matches!(self.width, 1 | 2 | 4) && self.pending_bytes < self.width
    }

    /// Whether every byte has been produced.
    pub(crate) fn done(&self) -> bool {
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

/// What a decoder takes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unit {
    Byte,
    Word,
}

/// A decoder for one of the three formats, part-way through its data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Decoder {
    Lz77(Lz77),
    RunLength(RunLength),
    Huffman(Huffman),
}

impl Decoder {
    /// Whether this decoder and its `out`, read from a save state, hold
    /// only counts they could have reached: the code shifts by them.
    pub(crate) fn in_range(&self, out: &Output) -> bool {
        let decoder = match self {
            Self::Lz77(_) | Self::RunLength(_) => true,
            // Symbols of up to 15 bits (a header's low nibble), less than
            // a byte of them collected, and a tree of 512 bytes at most.
            Self::Huffman(huffman) => {
                huffman.symbol_bits < 16
                    && huffman.symbols_bits < 8
                    && huffman.tree_length <= 512
                    && huffman.copied <= huffman.tree_length.max(1)
            }
        };
        decoder && out.in_range()
    }

    /// The unit it takes next.
    pub(crate) fn wants(&self) -> Unit {
        match self {
            Self::Huffman(huffman) if !huffman.copying_tree() => Unit::Word,
            _ => Unit::Byte,
        }
    }

    /// Takes the next unit of compressed data: a byte in the low 8 bits, or
    /// a word, as [`Decoder::wants`] said.
    pub(crate) fn feed<B: Bus>(&mut self, value: u32, out: &mut Output, bus: &mut B) {
        match self {
            Self::Lz77(lz77) => lz77.feed(value as u8, out, bus),
            Self::RunLength(run_length) => run_length.feed(value as u8, out, bus),
            Self::Huffman(huffman) if huffman.copying_tree() => {
                huffman.copy_tree(value as u8, bus);
            }
            Self::Huffman(huffman) => huffman.feed(value, out, bus),
        }
    }
}

/// LZ77: a flag byte, then eight blocks, each a literal byte or (flag bit
/// set) a two-byte reference to bytes already written: length − 3 in the
/// top nibble, then distance − 1 in the remaining 12 bits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lz77 {
    /// Type flags for the blocks left in this group, the next in bit 7.
    flags: u8,
    /// Blocks left in the group; at 0, a flag byte comes next.
    blocks: u8,
    /// A reference's first byte, while waiting for its second.
    reference: Option<u8>,
}

impl Lz77 {
    fn feed<B: Bus>(&mut self, byte: u8, out: &mut Output, bus: &mut B) {
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
pub(crate) struct RunLength {
    /// Bytes left in the current run; at 0, a flag byte comes next.
    left: u8,
    /// Whether the run repeats its one data byte.
    repeat: bool,
}

impl RunLength {
    fn feed<B: Bus>(&mut self, byte: u8, out: &mut Output, bus: &mut B) {
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
pub(crate) struct Huffman {
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
    pub(crate) fn new(symbol_bits: u32, tree: u32) -> Self {
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

    fn copying_tree(&self) -> bool {
        self.copied == 0 || self.copied < self.tree_length
    }

    fn copy_tree<B: Bus>(&mut self, byte: u8, bus: &mut B) {
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
    fn feed<B: Bus>(&mut self, bits: u32, out: &mut Output, bus: &mut B) {
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

/// Runs a ReadNormal function: the header and data straight from memory
/// at `source`, written a byte at a time to `destination`.
pub(crate) fn read_normal<B: Bus>(
    bus: &mut B,
    mut source: u32,
    destination: u32,
    mut decoder: Decoder,
) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    source = source.wrapping_add(4);
    let mut out = Output::new(destination, 1, header >> 8);
    while !out.done() {
        let byte = bus.read_8(source, Access::NONSEQUENTIAL);
        source = source.wrapping_add(1);
        decoder.feed(u32::from(byte), &mut out, bus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bios::tests::Ram;

    /// Feeds `data` a byte at a time to an LZ77 decoder writing `length`
    /// bytes in halfwords at 0x200.
    fn lz77_halfwords(ram: &mut Ram, data: &[u8], length: u32) {
        let mut decoder = Decoder::Lz77(Lz77::default());
        let mut out = Output::new(0x200, 2, length);
        for byte in data {
            decoder.feed(u32::from(*byte), &mut out, ram);
        }
        assert!(out.done());
    }

    #[test]
    fn halfword_lz77_drops_an_odd_last_byte() {
        let mut ram = Ram::new();
        ram.0[0x200..0x208].fill(0xEE);
        // Literals "A", "B", then 5 bytes from distance 2: "ABABABA".
        lz77_halfwords(&mut ram, &[0b0010_0000, b'A', b'B', 0x20, 0x01], 7);
        assert_eq!(&ram.0[0x200..0x208], b"ABABAB\xEE\xEE");
    }

    #[test]
    fn halfword_lz77_reads_back_past_a_pending_byte() {
        // "XYZ", then 3 bytes from distance 2, starting while "Z" waits
        // for its partner: they read "Y" (already written), "Z", "Y".
        let mut ram = Ram::new();
        lz77_halfwords(&mut ram, &[0b0001_0000, b'X', b'Y', b'Z', 0x00, 0x01], 6);
        assert_eq!(&ram.0[0x200..0x206], b"XYZYZY");
        // From distance 1 a pending byte is not in memory yet, so the
        // reference reads what the destination held before (GBATEK: the
        // "Vram" functions need Disp ≥ 1). "Q", then 3 bytes from
        // distance 1: old 0xE0 (Q is pending), the 0xE0 just written,
        // then old 0xE2.
        let mut ram = Ram::new();
        ram.0[0x200..0x204].copy_from_slice(&[0xE0, 0xE1, 0xE2, 0xE3]);
        lz77_halfwords(&mut ram, &[0b0100_0000, b'Q', 0x00, 0x00], 4);
        assert_eq!(&ram.0[0x200..0x204], &[b'Q', 0xE0, 0xE0, 0xE2]);
    }

    #[test]
    fn halfword_run_length_drops_an_odd_last_byte() {
        let mut ram = Ram::new();
        ram.0[0x200..0x208].fill(0xEE);
        let mut decoder = Decoder::RunLength(RunLength::default());
        let mut out = Output::new(0x200, 2, 5);
        // Two literals "hi", then "!" three times.
        for byte in [0x01, b'h', b'i', 0x80, b'!'] {
            decoder.feed(u32::from(byte), &mut out, &mut ram);
        }
        assert!(out.done());
        assert_eq!(&ram.0[0x200..0x206], b"hi!!\xEE\xEE");
    }

    #[test]
    fn read_normal_expands_runs_a_byte_at_a_time() {
        let mut ram = Ram::new();
        // Header: type 0x30, 9 bytes. "ab", then "z" seven times.
        let packed = [0x30, 9, 0, 0, 0x01, b'a', b'b', 0x84, b'z'];
        ram.0[0x100..0x109].copy_from_slice(&packed);
        read_normal(
            &mut ram,
            0x100,
            0x201,
            Decoder::RunLength(RunLength::default()),
        );
        assert_eq!(&ram.0[0x201..0x20A], b"abzzzzzzz");
    }

    #[test]
    fn huffman_decodes_four_and_eight_bit_symbols() {
        // Codes 1 = "0", 2 = "10", 3 = "11". Copied to 0x100: the size byte
        // (6 bytes / 2 − 1); the root at 0x101, with symbol 1 as child 0
        // at 0x102 and a node as child 1 at 0x103; that node's children,
        // both symbols, at 0x104 and 0x105.
        let tree = [0x02, 0x80, 0x01, 0xC0, 0x02, 0x03];
        for (symbol_bits, expected) in [(8, [1u8, 2, 3, 1]), (4, [0x21, 0x13, 0x32, 0x21])] {
            let mut ram = Ram::new();
            let mut decoder = Decoder::Huffman(Huffman::new(symbol_bits, 0x100));
            let mut out = Output::new(0x200, 4, 4);
            for byte in tree {
                assert_eq!(decoder.wants(), Unit::Byte);
                decoder.feed(u32::from(byte), &mut out, &mut ram);
            }
            assert_eq!(&ram.0[0x100..0x106], &tree, "the tree is copied");
            assert_eq!(decoder.wants(), Unit::Word);
            // 1, 2, 3, 1, 2, 3, ...: "0 10 11 0 10 11 ...". Four-bit
            // symbols pack two to a byte, the first in the low nibble.
            decoder.feed(
                0b0101_1010_1101_0110_1011_0101_1010_1101,
                &mut out,
                &mut ram,
            );
            assert!(out.done());
            assert_eq!(&ram.0[0x200..0x204], &expected, "{symbol_bits}-bit symbols");
        }
    }
}
