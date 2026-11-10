//! The BIOS routines the Game Boy Advance and the Nintendo DS have in
//! common, for the cores' emulated BIOSes: BitUnPack, the Diff unfilters
//! and the LZ77, run-length and Huffman decompressors (GBATEK, "BIOS
//! Decompression Functions"). No Nintendo code is used.
//!
//! They are here because both ARM cores already depend on this crate and
//! the routines need nothing but its [`Bus`]. A routine reaches memory
//! only through the bus it is given, an access at a time and in a fixed
//! order, so a console that counts time by bus accesses (the GBA) spends
//! what it always did.
//!
//! The decoders ([`Lz77`], [`RunLength`], [`Huffman`]) take their
//! compressed data one byte or word at a time and keep all of their
//! progress in themselves and in an [`Output`], so one can stop between
//! any two units: the DS's ReadByCallback functions get each unit from a
//! function of the game's, and a save state may be made in between. These
//! types are part of a DS save state, so their fields, and the order of
//! them, are that state's layout. The functions that read their data
//! straight from memory ([`lz77`], [`run_length`]) run the same decoders
//! to the end.
//!
//! # How long a call can take
//!
//! A call runs to its end in one step of the emulation thread, where the
//! real BIOS spends CPU time with interrupts still served. So nothing
//! here loops on a number the game supplies without a bound. Each routine
//! has one, and this is where it is set:
//!
//! | Routine | What bounds it |
//! |---|---|
//! | [`bit_unpack`] | Its 16-bit length: 65,535 bytes at most, eight units from each. |
//! | [`diff_unfilter`] | The header's 24-bit length: one unit read for each written. |
//! | [`lz77`], [`run_length`] | The header's 24-bit length ([`read_normal`]): a decoder takes at most three bytes before it produces one. |
//! | [`huffman`] | A bitstream word for each byte of the 24-bit length and [`HUFFMAN_SLACK_WORDS`] more; sooner if a walk leaves the tree. |
//! | A decoder fed by the game ([`Lz77::feed`], [`RunLength::feed`], [`Huffman::feed`]) | One unit: at most 18 bytes copied, 130 written, or 32 bits followed. The game's own code runs before the next. |

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
    /// destination as the BIOS reads it, except where `waiting` says a
    /// byte still waiting to be written as half of a halfword is taken
    /// without a read.
    fn earlier<B: Bus>(&self, bus: &mut B, distance: u32, waiting: WaitingByte) -> u8 {
        if waiting == WaitingByte::Copied
            && self.width == 2
            && self.pending_bytes == 1
            && distance == 1
        {
            return self.pending as u8;
        }
        let next = self.destination.wrapping_add(self.pending_bytes);
        bus.read_8(next.wrapping_sub(distance), Access::NONSEQUENTIAL)
    }
}

/// What an LZ77 reference of distance 1 copies when the byte before it
/// has not been written yet: the "Vram" functions write halfwords, so the
/// first byte of each waits for the second.
///
/// GBATEK cautions that those functions need a Disp of at least 1, which
/// is a distance of at least 2 here (the distance is Disp + 1), and does
/// not say what a console does with less. The two emulated BIOSes were
/// written differently, and neither has been checked against a console,
/// so each keeps what it did. Data packed for these functions as GBATEK
/// asks has no such reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingByte {
    /// The waiting byte itself, as if it were written already, without a
    /// read of memory: the GBA's LZ77UnCompVram.
    Copied,
    /// Whatever the destination held before, read from memory, since the
    /// waiting byte is not there yet: the DS's
    /// LZ77UnCompReadByCallbackWrite16bit.
    NotThereYet,
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
    /// Takes the next byte of compressed data. `waiting` is the console's
    /// answer for a reference of distance 1 into a halfword not yet
    /// written.
    pub fn feed<B: Bus>(&mut self, byte: u8, out: &mut Output, bus: &mut B, waiting: WaitingByte) {
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
                let byte = out.earlier(bus, distance, waiting);
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
            let (child, is_symbol) = huffman_child(self.node, node, (bits >> bit) & 1);
            if !is_symbol {
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

/// One step of a walk through a Huffman tree: from the node at `address`,
/// whose byte is `node`, bit `one` (0 or 1) of the bitstream leads to the
/// child at the address returned, which is a symbol if the flag returned
/// is set and another node if not.
fn huffman_child(address: u32, node: u32, one: u32) -> (u32, bool) {
    let child = (address & !1).wrapping_add((node & 0x3F) * 2 + 2 + one);
    (child, node & (0x80 >> one) != 0)
}

/// Bitstream words [`huffman`] follows beyond one for each byte it has to
/// write: room for the longest codes a tree can hold (256 bits) in data
/// of a few bytes.
pub const HUFFMAN_SLACK_WORDS: u32 = 64;

/// HuffUnComp, as the GBA has it: the data at `source` (header, tree and
/// bitstream, read where they lie) unpacked to `destination` in whole
/// words, all in one call. GBATEK's Huffman format has symbols of 4 or 8
/// bits. Any size that fills the 32-bit words it writes exactly is
/// decoded (1 and 2 bits too); data that names another size, which never
/// fills a word, is left alone.
///
/// A node's children lie after it, so a walk only moves on through the
/// tree. One that leaves the tree (the size byte says where it ends) can
/// reach no symbol the data holds, and ends the call.
///
/// The bitstream followed is at most a word for each byte to write, and
/// [`HUFFMAN_SLACK_WORDS`] more. That is four times what the bytes take
/// unpacked, and a Huffman code is at worst as long as fixed-width
/// symbols; without it, a stream of a tree's longest codes for the
/// longest length would keep one call going for minutes.
///
/// # How this differs from [`Huffman`]
///
/// The two decode the same format, but they are two BIOS functions, and
/// the emulated ones were written apart. They share the step through the
/// tree and are otherwise kept as they were, since no difference between
/// them has been checked against a console:
///
/// - This one reads a node's byte when the walk arrives at it, and the
///   root's again after each symbol, before it writes a finished word;
///   [`Huffman`] reads a node when the next bit needs it. The reads are
///   of the same bytes, in another order around the bitstream's words and
///   the writes, and this one ends on a read of the root that the other
///   never makes.
/// - This one decodes only sizes that fill a word, and ORs a symbol's
///   whole byte into the word; [`Huffman`] takes any size below 16 and
///   keeps only the symbol's own bits.
/// - This one ends when a walk leaves the tree or its bitstream outruns
///   the budget above; [`Huffman`] follows a walk wherever it leads, a
///   word for each call the game's own function answers.
pub fn huffman<B: Bus>(bus: &mut B, mut source: u32, mut destination: u32) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let symbol_bits = header & 0xF;
    if !matches!(symbol_bits, 1 | 2 | 4 | 8) {
        return;
    }
    let mut remaining = header >> 8;
    let tree_size = u32::from(bus.read_8(source.wrapping_add(4), Access::NONSEQUENTIAL));
    let root = source.wrapping_add(5);
    // The nodes' bytes, from the root to the bitstream.
    let tree_length = tree_size * 2 + 1;
    source = root.wrapping_add(tree_length);
    let mut node = root;
    let mut node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
    let mut out = 0u32;
    let mut out_bits = 0;
    let mut words = remaining + HUFFMAN_SLACK_WORDS;
    while remaining > 0 && words > 0 {
        words -= 1;
        let bits = bus.read_32(source, Access::NONSEQUENTIAL);
        source = source.wrapping_add(4);
        for bit in (0..32).rev() {
            let (next, is_symbol) = huffman_child(node, node_value, (bits >> bit) & 1);
            if next.wrapping_sub(root) >= tree_length {
                return;
            }
            node = next;
            node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
            if is_symbol {
                out |= node_value << out_bits;
                out_bits += symbol_bits;
                node = root;
                node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
                if out_bits >= 32 {
                    bus.write_32(destination, out, Access::NONSEQUENTIAL);
                    destination = destination.wrapping_add(4);
                    out = 0;
                    out_bits = 0;
                    remaining = remaining.saturating_sub(4);
                    if remaining == 0 {
                        return;
                    }
                }
            }
        }
    }
}

/// Runs a function that reads its data straight from memory: the 4-byte
/// header at `source`, then a byte at a time to `feed` (which holds the
/// decoder) until the header's length has been written to `destination`,
/// `width` bytes at a time.
///
/// It ends because the length is 24 bits and no decoder takes more than
/// a few bytes without producing one: LZ77 takes a flag byte for every
/// eight blocks and at most two bytes a block, run-length a flag byte and
/// at most one byte a run.
pub fn read_normal<B: Bus>(
    bus: &mut B,
    mut source: u32,
    destination: u32,
    width: u32,
    mut feed: impl FnMut(u8, &mut Output, &mut B),
) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    source = source.wrapping_add(4);
    let mut out = Output::new(destination, width, header >> 8);
    while !out.done() {
        let byte = bus.read_8(source, Access::NONSEQUENTIAL);
        source = source.wrapping_add(1);
        feed(byte, &mut out, bus);
    }
}

/// The LZ77UnComp functions that read from memory: writes the data at
/// `source` to `destination` a byte at a time or, with `halfwords`, two
/// (the "Vram" function, which never writes an odd last byte). `waiting`
/// is the console's answer for a reference of distance 1 into a halfword
/// not yet written.
pub fn lz77<B: Bus>(
    bus: &mut B,
    source: u32,
    destination: u32,
    halfwords: bool,
    waiting: WaitingByte,
) {
    let mut decoder = Lz77::default();
    let width = if halfwords { 2 } else { 1 };
    read_normal(bus, source, destination, width, |byte, out, bus| {
        decoder.feed(byte, out, bus, waiting);
    });
}

/// The RLUnComp functions that read from memory: writes the data at
/// `source` to `destination` a byte at a time or, with `halfwords`, two
/// (the "Vram" function, which never writes an odd last byte).
pub fn run_length<B: Bus>(bus: &mut B, source: u32, destination: u32, halfwords: bool) {
    let mut decoder = RunLength::default();
    let width = if halfwords { 2 } else { 1 };
    read_normal(bus, source, destination, width, |byte, out, bus| {
        decoder.feed(byte, out, bus);
    });
}

/// BitUnPack: widens the packed pixels at `source` to `destination`,
/// adding an offset to non-zero (or all) values, as the structure at
/// `info` says: a 16-bit length in bytes, the two widths in bits and the
/// offset. Widths the BIOS does not know are left alone.
///
/// It reads at most the 65,535 bytes its length can name, and each makes
/// eight units at most.
pub fn bit_unpack<B: Bus>(bus: &mut B, mut source: u32, mut destination: u32, info: u32) {
    let length = u32::from(bus.read_16(info, Access::NONSEQUENTIAL));
    let source_width = u32::from(bus.read_8(info.wrapping_add(2), Access::NONSEQUENTIAL));
    let destination_width = u32::from(bus.read_8(info.wrapping_add(3), Access::NONSEQUENTIAL));
    let offset_field = bus.read_32(info.wrapping_add(4), Access::NONSEQUENTIAL);
    let offset = offset_field & 0x7FFF_FFFF;
    let offset_zero = offset_field >> 31 != 0;
    if !matches!(source_width, 1 | 2 | 4 | 8)
        || !matches!(destination_width, 1 | 2 | 4 | 8 | 16 | 32)
    {
        return;
    }
    let mut out = 0u32;
    let mut out_bits = 0;
    for _ in 0..length {
        let byte = u32::from(bus.read_8(source, Access::NONSEQUENTIAL));
        source = source.wrapping_add(1);
        for shift in (0..8).step_by(source_width as usize) {
            let mut value = (byte >> shift) & ((1 << source_width) - 1);
            if value != 0 || offset_zero {
                value = value.wrapping_add(offset);
            }
            out |= value.checked_shl(out_bits).unwrap_or(0);
            out_bits += destination_width;
            if out_bits >= 32 {
                bus.write_32(destination, out, Access::NONSEQUENTIAL);
                destination = destination.wrapping_add(4);
                out = 0;
                out_bits = 0;
            }
        }
    }
}

/// The Diff8bitUnFilter and Diff16bitUnFilter functions: each unit of the
/// data at `source`, after its 4-byte header, is a difference, and what is
/// written to `destination` is the running sum. With `sixteen` the units
/// are halfwords (the length's odd last byte is left out); otherwise they
/// are bytes, written one at a time or, with `halfwords`, two at a time as
/// the "Vram" function does, which never writes an odd last byte.
///
/// It reads a unit for each one of the header's 24-bit length.
pub fn diff_unfilter<B: Bus>(
    bus: &mut B,
    mut source: u32,
    mut destination: u32,
    sixteen: bool,
    halfwords: bool,
) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source = source.wrapping_add(4);
    if sixteen {
        let mut sum = 0u16;
        while remaining >= 2 {
            sum = sum.wrapping_add(bus.read_16(source, Access::NONSEQUENTIAL));
            bus.write_16(destination, sum, Access::NONSEQUENTIAL);
            source = source.wrapping_add(2);
            destination = destination.wrapping_add(2);
            remaining -= 2;
        }
        return;
    }
    let mut out = Output::new(destination, if halfwords { 2 } else { 1 }, remaining);
    let mut sum = 0u8;
    while !out.done() {
        sum = sum.wrapping_add(bus.read_8(source, Access::NONSEQUENTIAL));
        out.push(bus, sum);
        source = source.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 KiB of flat memory that repeats through the whole bus, and
    /// every access made to it, in order.
    struct Ram {
        bytes: Vec<u8>,
        accesses: Vec<Made>,
    }

    /// One bus access: a read or a write of `bytes` bytes at `address`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Made {
        Read { address: u32, bytes: u32 },
        Write { address: u32, bytes: u32 },
    }

    impl Ram {
        fn new() -> Self {
            Self {
                bytes: vec![0; 0x1_0000],
                accesses: Vec::new(),
            }
        }

        /// RAM with `data` at `address`.
        fn with(address: usize, data: &[u8]) -> Self {
            let mut ram = Self::new();
            ram.bytes[address..address + data.len()].copy_from_slice(data);
            ram
        }

        fn read(&mut self, address: u32, bytes: u32) -> u32 {
            self.accesses.push(Made::Read { address, bytes });
            let start = (address & 0xFFFF & !(bytes - 1)) as usize;
            let mut value = [0; 4];
            value[..bytes as usize].copy_from_slice(&self.bytes[start..start + bytes as usize]);
            u32::from_le_bytes(value)
        }

        fn write(&mut self, address: u32, bytes: u32, value: u32) {
            self.accesses.push(Made::Write { address, bytes });
            let start = (address & 0xFFFF & !(bytes - 1)) as usize;
            self.bytes[start..start + bytes as usize]
                .copy_from_slice(&value.to_le_bytes()[..bytes as usize]);
        }
    }

    impl Bus for Ram {
        fn read_32(&mut self, address: u32, _: Access) -> u32 {
            self.read(address, 4)
        }
        fn read_16(&mut self, address: u32, _: Access) -> u16 {
            self.read(address, 2) as u16
        }
        fn read_8(&mut self, address: u32, _: Access) -> u8 {
            self.read(address, 1) as u8
        }
        fn write_32(&mut self, address: u32, value: u32, _: Access) {
            self.write(address, 4, value);
        }
        fn write_16(&mut self, address: u32, value: u16, _: Access) {
            self.write(address, 2, u32::from(value));
        }
        fn write_8(&mut self, address: u32, value: u8, _: Access) {
            self.write(address, 1, u32::from(value));
        }
        fn idle(&mut self) {}
    }

    #[test]
    fn bit_unpack_widens_pixels_and_adds_its_offset_to_the_ones_that_are_not_zero() {
        // Two bytes of 4-bit pixels to 8 bits, offset 0x10: 0x30 is the
        // pixels 0 and 3, 0x21 the pixels 1 and 2.
        let mut ram = Ram::with(0x100, &[0x30, 0x21]);
        ram.bytes[0x300..0x308].copy_from_slice(&[2, 0, 4, 8, 0x10, 0, 0, 0]);
        bit_unpack(&mut ram, 0x100, 0x200, 0x300);
        assert_eq!(&ram.bytes[0x200..0x205], &[0, 0x13, 0x11, 0x12, 0]);

        // With the offset's top bit set, zero pixels get it too.
        let mut ram = Ram::with(0x100, &[0x30, 0x21]);
        ram.bytes[0x300..0x308].copy_from_slice(&[2, 0, 4, 8, 0x10, 0, 0, 0x80]);
        bit_unpack(&mut ram, 0x100, 0x200, 0x300);
        assert_eq!(&ram.bytes[0x200..0x204], &[0x10, 0x13, 0x11, 0x12]);
    }

    #[test]
    fn bit_unpack_leaves_widths_the_bios_does_not_know_alone() {
        for widths in [[3, 8], [4, 12], [0, 8], [8, 0]] {
            let mut ram = Ram::with(0x100, &[0xFF; 4]);
            ram.bytes[0x300..0x304].copy_from_slice(&[4, 0, widths[0], widths[1]]);
            bit_unpack(&mut ram, 0x100, 0x200, 0x300);
            assert!(
                ram.accesses
                    .iter()
                    .all(|made| matches!(made, Made::Read { .. })),
                "{widths:?}"
            );
            // Only the structure was read: nothing of the data.
            assert_eq!(ram.accesses.len(), 4, "{widths:?}");
        }
    }

    #[test]
    fn diff_unfilter_writes_running_sums_of_bytes_one_or_two_at_a_time() {
        // Header: type 0x81, 5 bytes. Sums: 1, 3, 6, 10, 15.
        let data = [0x81, 5, 0, 0, 1, 2, 3, 4, 5];
        let mut bytes = Ram::with(0x100, &data);
        bytes.bytes[0x200..0x208].fill(0xEE);
        diff_unfilter(&mut bytes, 0x100, 0x200, false, false);
        assert_eq!(&bytes.bytes[0x200..0x206], &[1, 3, 6, 10, 15, 0xEE]);
        let writes = |ram: &Ram| -> Vec<Made> {
            let writes = ram.accesses.iter().copied();
            writes
                .filter(|made| matches!(made, Made::Write { .. }))
                .collect()
        };
        assert_eq!(writes(&bytes).len(), 5);

        // Two at a time, the odd fifth byte is read but never written.
        let mut halfwords = Ram::with(0x100, &data);
        halfwords.bytes[0x200..0x208].fill(0xEE);
        diff_unfilter(&mut halfwords, 0x100, 0x200, false, true);
        assert_eq!(&halfwords.bytes[0x200..0x206], &[1, 3, 6, 10, 0xEE, 0xEE]);
        let expected = [0x200, 0x202].map(|address| Made::Write { address, bytes: 2 });
        assert_eq!(writes(&halfwords), expected);
        assert_eq!(halfwords.accesses.len(), 1 + 5 + 2, "header, bytes, writes");
    }

    #[test]
    fn diff_unfilter_sums_halfwords_and_leaves_out_an_odd_last_byte() {
        // Header: type 0x82, 5 bytes: two halfwords and a byte left over.
        let mut ram = Ram::with(0x100, &[0x82, 5, 0, 0, 0xFF, 0xFF, 2, 0, 9]);
        ram.bytes[0x200..0x206].fill(0xEE);
        diff_unfilter(&mut ram, 0x100, 0x200, true, true);
        // 0xFFFF, then 0xFFFF + 2 wrapped.
        assert_eq!(&ram.bytes[0x200..0x206], &[0xFF, 0xFF, 1, 0, 0xEE, 0xEE]);
        assert_eq!(ram.accesses.len(), 1 + 2 + 2);
    }

    #[test]
    fn lz77_copies_literals_and_references_a_byte_or_two_at_a_time() {
        // Header: type 0x10, 7 bytes. "A", "B", then 5 from distance 2.
        let data = [0x10, 7, 0, 0, 0b0010_0000, b'A', b'B', 0x20, 0x01];
        for waiting in [WaitingByte::Copied, WaitingByte::NotThereYet] {
            let mut bytes = Ram::with(0x100, &data);
            bytes.bytes[0x200..0x208].fill(0xEE);
            lz77(&mut bytes, 0x100, 0x200, false, waiting);
            assert_eq!(&bytes.bytes[0x200..0x208], b"ABABABA\xEE");

            // Two at a time, the odd seventh byte is never written.
            let mut halfwords = Ram::with(0x100, &data);
            halfwords.bytes[0x200..0x208].fill(0xEE);
            lz77(&mut halfwords, 0x100, 0x200, true, waiting);
            assert_eq!(&halfwords.bytes[0x200..0x208], b"ABABAB\xEE\xEE");
        }
    }

    /// "Q", then three bytes from distance 1, two bytes at a time over
    /// E0 E1 E2 E3: the first and third are asked for while a byte waits.
    fn lz77_from_distance_1(waiting: WaitingByte) -> Ram {
        let data = [0x10, 4, 0, 0, 0b0100_0000, b'Q', 0x00, 0x00];
        let mut ram = Ram::with(0x100, &data);
        ram.bytes[0x200..0x204].copy_from_slice(&[0xE0, 0xE1, 0xE2, 0xE3]);
        lz77(&mut ram, 0x100, 0x200, true, waiting);
        ram
    }

    #[test]
    fn a_gbas_lz77_copies_a_waiting_byte_from_distance_1_without_reading_memory() {
        let ram = lz77_from_distance_1(WaitingByte::Copied);
        assert_eq!(&ram.bytes[0x200..0x204], b"QQQQ");
        // The header and four bytes of data, then one read of memory, for
        // the second of the three, which no byte was waiting before.
        let reads = [0x100, 0x104, 0x105, 0x106, 0x107, 0x201];
        let read = |made: &Made| match *made {
            Made::Read { address, .. } => Some(address),
            Made::Write { .. } => None,
        };
        assert_eq!(
            ram.accesses.iter().filter_map(read).collect::<Vec<_>>(),
            reads
        );
    }

    #[test]
    fn a_dss_lz77_reads_what_memory_held_from_distance_1_while_a_byte_waits() {
        let ram = lz77_from_distance_1(WaitingByte::NotThereYet);
        // Old E0 ("Q" is waiting), the E0 just written, then old E2.
        assert_eq!(&ram.bytes[0x200..0x204], &[b'Q', 0xE0, 0xE0, 0xE2]);
        let back = |address| Made::Read { address, bytes: 1 };
        for address in [0x200, 0x201, 0x202] {
            assert!(ram.accesses.contains(&back(address)), "{address:#X}");
        }
    }

    #[test]
    fn a_byte_at_a_time_nothing_waits_so_the_consoles_agree_at_distance_1() {
        // "A", then five from distance 1.
        let data = [0x10, 6, 0, 0, 0b0100_0000, b'A', 0x20, 0x00];
        let unpack = |waiting| {
            let mut ram = Ram::with(0x100, &data);
            lz77(&mut ram, 0x100, 0x200, false, waiting);
            (ram.bytes, ram.accesses)
        };
        let copied = unpack(WaitingByte::Copied);
        assert_eq!(&copied.0[0x200..0x206], b"AAAAAA");
        assert!(copied == unpack(WaitingByte::NotThereYet));
    }

    #[test]
    fn run_length_copies_literals_and_runs_a_byte_or_two_at_a_time() {
        // Header: type 0x30, 9 bytes. "ab", then "z" seven times.
        let data = [0x30, 9, 0, 0, 0x01, b'a', b'b', 0x84, b'z'];
        let mut bytes = Ram::with(0x100, &data);
        run_length(&mut bytes, 0x100, 0x201, false);
        assert_eq!(&bytes.bytes[0x200..0x20B], b"\0abzzzzzzz\0");

        let mut halfwords = Ram::with(0x100, &data);
        halfwords.bytes[0x200..0x20A].fill(0xEE);
        run_length(&mut halfwords, 0x100, 0x200, true);
        assert_eq!(&halfwords.bytes[0x200..0x20A], b"abzzzzzz\xEE\xEE");
    }

    #[test]
    fn a_function_reads_no_more_than_its_data_takes() {
        // Nothing to write: only the header is read.
        let mut empty = Ram::with(0x100, &[0x30, 0, 0, 0, 0xFF]);
        run_length(&mut empty, 0x100, 0x200, false);
        assert_eq!(
            empty.accesses,
            [Made::Read {
                address: 0x100,
                bytes: 4
            }]
        );

        // A run longer than the length stops at the length, and a literal
        // stretch cut short by it reads no further.
        let mut run = Ram::with(0x100, &[0x30, 2, 0, 0, 0xFF, b'r']);
        run_length(&mut run, 0x100, 0x200, false);
        assert_eq!(&run.bytes[0x200..0x203], b"rr\0");
        let mut literals = Ram::with(0x100, &[0x30, 2, 0, 0, 0x7F, b'x', b'y', b'z']);
        run_length(&mut literals, 0x100, 0x200, false);
        assert_eq!(&literals.bytes[0x200..0x203], b"xy\0");
        let last_read = literals.accesses.iter().rev().find_map(|made| match *made {
            Made::Read { address, .. } => Some(address),
            Made::Write { .. } => None,
        });
        assert_eq!(last_read, Some(0x106), "the byte after is not read");
    }

    /// A Huffman stream at 0x100 for `length` bytes of `symbol_bits`-bit
    /// symbols with the codes 1 = "0", 2 = "10" and 3 = "11": the header,
    /// the size byte (8 bytes / 2 − 1), the root at 0x105 with symbol 1
    /// as child 0 and a node as child 1, that node's two symbols, padding
    /// to a word, then the bitstream 1, 2, 3, 1, 2, 3, ...
    fn huffman_stream(symbol_bits: u8, length: u8) -> Ram {
        let mut ram = Ram::with(0x100, &[0x20 | symbol_bits, length, 0, 0]);
        ram.bytes[0x104..0x10C].copy_from_slice(&[0x03, 0x80, 0x01, 0xC0, 0x02, 0x03, 0, 0]);
        let bits: u32 = 0b0101_1010_1101_0110_1011_0101_1010_1101;
        ram.bytes[0x10C..0x110].copy_from_slice(&bits.to_le_bytes());
        ram
    }

    #[test]
    fn huffman_unpacks_whole_words_of_symbols_of_a_size_that_fills_one() {
        for (symbol_bits, expected) in [
            (8, [1, 2, 3, 1]),
            (4, [0x21, 0x13, 0x32, 0x21]),
            (2, [0x79, 0x9E, 0xE7, 0x79]),
        ] {
            let mut ram = huffman_stream(symbol_bits, 4);
            huffman(&mut ram, 0x100, 0x200);
            assert_eq!(&ram.bytes[0x200..0x204], &expected, "{symbol_bits} bits");
            assert_eq!(&ram.bytes[0x204..0x208], &[0; 4], "one word for four bytes");
        }
        // Sizes that never fill a word exactly are left alone, after the
        // header alone has been read.
        for symbol_bits in [0, 3, 5, 6, 7, 9, 15] {
            let mut ram = huffman_stream(symbol_bits, 0xFF);
            huffman(&mut ram, 0x100, 0x200);
            assert_eq!(
                ram.accesses,
                [Made::Read {
                    address: 0x100,
                    bytes: 4
                }]
            );
        }
    }

    #[test]
    fn huffman_reads_each_node_as_the_walk_arrives_and_the_root_again_before_it_writes() {
        // One word of 8-bit symbols: 1 ("0"), 2 ("10"), 3 ("11"), 1.
        let mut ram = huffman_stream(8, 4);
        huffman(&mut ram, 0x100, 0x200);
        let read = |address| Made::Read { address, bytes: 1 };
        let expected = [
            Made::Read {
                address: 0x100,
                bytes: 4,
            },
            read(0x104), // the size byte
            read(0x105), // the root
            Made::Read {
                address: 0x10C,
                bytes: 4,
            }, // the bitstream's first word
            read(0x106),
            read(0x105), // 1, and back to the root
            read(0x107),
            read(0x108),
            read(0x105), // 2
            read(0x107),
            read(0x109),
            read(0x105), // 3
            read(0x106),
            read(0x105), // 1, the root once more,
            Made::Write {
                address: 0x200,
                bytes: 4,
            }, // and only then the word
        ];
        assert_eq!(ram.accesses, expected);
    }

    #[test]
    fn huffman_ends_when_a_walk_leaves_the_tree_or_the_bitstream_outruns_its_budget() {
        // No node marks a child as a symbol: the walk runs off the tree's
        // seven bytes after two steps.
        let mut ram = huffman_stream(8, 0xFF);
        ram.bytes[0x105..0x10C].fill(0);
        huffman(&mut ram, 0x100, 0x200);
        assert!(ram.accesses.len() < 10, "{}", ram.accesses.len());
        assert!(
            ram.accesses
                .iter()
                .all(|made| matches!(made, Made::Read { .. }))
        );

        // The longest code a tree can hold: a full tree (size byte 0xFF)
        // that is a chain of 255 nodes, each the 1 child of the one
        // before, the last with two symbols. Every symbol takes 255 bits,
        // so 256 bytes would take 2,040 words of bitstream; the call
        // follows a word for each byte and the slack, and ends.
        let mut ram = Ram::with(0x100, &[0x28, 0x00, 0x01, 0x00]);
        ram.bytes[0x104] = 0xFF;
        ram.bytes[0x104 + 509] = 0xC0;
        ram.bytes[0x104 + 511] = 0x5A;
        ram.bytes[0x304..0x1304].fill(0xFF);
        huffman(&mut ram, 0x100, 0x2000);
        let words_read = ram
            .accesses
            .iter()
            .filter(|made| matches!(made, Made::Read { bytes: 4, .. }))
            .count();
        // The header, then the bitstream.
        assert_eq!(words_read as u32, 1 + 0x100 + HUFFMAN_SLACK_WORDS);
        // 320 words hold 40 symbols of 255 bits: ten words written.
        let written = |ram: &Ram, word: usize| ram.bytes[0x2000 + word * 4..][..4].to_vec();
        assert_eq!(written(&ram, 9), [0x5A; 4]);
        assert_eq!(written(&ram, 10), [0; 4]);
    }

    #[test]
    fn the_resumable_huffman_reads_a_node_when_a_bit_needs_it_and_keeps_only_a_symbols_bits() {
        // The same tree, copied to 0x400 a byte at a time, its symbol 3
        // given high bits that 4-bit symbols must not carry.
        let mut ram = Ram::new();
        let mut decoder = Huffman::new(4, 0x400);
        let mut out = Output::new(0x200, 4, 4);
        for byte in [0x02, 0x80, 0x01, 0xC0, 0x02, 0xF3] {
            assert!(decoder.copying_tree());
            decoder.copy_tree(byte, &mut ram);
        }
        assert!(!decoder.copying_tree() && decoder.in_range());
        ram.accesses.clear();
        decoder.feed(
            0b0101_1010_1101_0110_1011_0101_1010_1101,
            &mut out,
            &mut ram,
        );
        assert!(out.done());
        assert_eq!(&ram.bytes[0x200..0x204], &[0x21, 0x13, 0x32, 0x21]);
        // The first symbol: the root is read for the first bit, then the
        // symbol; nothing is read ahead.
        let read = |address| Made::Read { address, bytes: 1 };
        assert_eq!(ram.accesses[..2], [read(0x401), read(0x402)]);
        assert_eq!(
            ram.accesses.last(),
            Some(&Made::Write {
                address: 0x200,
                bytes: 4
            })
        );

        // The function that reads where the data lies keeps the whole
        // byte: the 0xF0 of each symbol 3 lands on the symbol after it,
        // a 1 that becomes 0xF.
        let mut ram = huffman_stream(4, 4);
        ram.bytes[0x109] = 0xF3;
        huffman(&mut ram, 0x100, 0x200);
        assert_eq!(&ram.bytes[0x200..0x204], &[0x21, 0xF3, 0x32, 0x2F]);
    }
}
