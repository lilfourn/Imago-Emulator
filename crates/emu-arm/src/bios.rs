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
}
