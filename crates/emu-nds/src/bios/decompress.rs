//! The BIOS's LZ77, run-length and Huffman formats (GBATEK, "BIOS
//! Decompression Functions"), decoded by [`emu_arm::bios`]'s decoders,
//! which take their compressed data one byte or word at a time.
//!
//! The ReadNormal functions feed a decoder from memory ([`read_normal`]).
//! The ReadByCallback ones feed it from the game's own functions, one call
//! per unit, so a decoder keeps all of its progress in itself and can stop
//! between any two units (see `callback.rs`).

use emu_arm::Bus;
use serde::{Deserialize, Serialize};

use emu_arm::bios::WaitingByte;
pub(crate) use emu_arm::bios::{Huffman, Lz77, Output, RunLength};

/// What the DS's LZ77 copies from distance 1 while the byte before waits
/// to be written as half of a halfword: what the destination held, since
/// that byte is not in memory yet (GBATEK's caution that the "Vram" LZ77
/// functions need Disp of at least 1).
const WAITING_BYTE: WaitingByte = WaitingByte::NotThereYet;

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
            Self::Huffman(huffman) => huffman.in_range(),
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
            Self::Lz77(lz77) => lz77.feed(value as u8, out, bus, WAITING_BYTE),
            Self::RunLength(run_length) => run_length.feed(value as u8, out, bus),
            Self::Huffman(huffman) if huffman.copying_tree() => {
                huffman.copy_tree(value as u8, bus);
            }
            Self::Huffman(huffman) => huffman.feed(value, out, bus),
        }
    }
}

/// Runs a ReadNormal function: the header and data straight from memory
/// at `source`, written a byte at a time to `destination`.
pub(crate) fn read_normal<B: Bus>(
    bus: &mut B,
    source: u32,
    destination: u32,
    mut decoder: Decoder,
) {
    emu_arm::bios::read_normal(bus, source, destination, 1, |byte, out, bus| {
        decoder.feed(u32::from(byte), out, bus);
    });
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
