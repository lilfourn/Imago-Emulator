//! The ReadByCallback decompressors, SWI 12h, 13h and 15h (GBATEK,
//! "NDS/DSi Decompression Callbacks").
//!
//! These read their compressed data through game functions listed in a
//! callback structure at R3:
//!
//! | Offset | Function | Called with | Returns |
//! |---|---|---|---|
//! | 0x00 | Open | source, destination, R2 | the header word, or a negative error |
//! | 0x04 | Close (0 for none) | source | a negative error, or anything else |
//! | 0x08 | Get_8bit | source | the next byte |
//! | 0x0C | Get_16bit | (never called) | |
//! | 0x10 | Get_32bit | source | the next word (Huffman's bitstream) |
//!
//! The source address moves past each unit read. Such a call cannot run
//! to the end in one go: it runs until it needs a game function, the
//! machine runs that function on the CPU, and the call resumes with the
//! function's R0. The SWI returns the decompressed length, or the error
//! from Open or Close.

use emu_arm::{Access, Bus};
use serde::{Deserialize, Serialize};

use super::decompress::{Decoder, Huffman, Lz77, Output, RunLength, Unit};

/// Offsets of the functions in the callback structure.
const OPEN: u32 = 0x00;
const CLOSE: u32 = 0x04;
const GET_8BIT: u32 = 0x08;
const GET_32BIT: u32 = 0x10;

/// Which of the three functions the SWI was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Format {
    /// SWI 12h, LZ77UnCompReadByCallbackWrite16bit.
    Lz77,
    /// SWI 13h, HuffUnCompReadByCallback.
    Huffman,
    /// SWI 15h, RLUnCompReadByCallbackWrite16bit.
    RunLength,
}

/// A game function to run: its address (bit 0 set for Thumb) and R0–R2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuestCall {
    pub(crate) function: u32,
    pub(crate) args: [u32; 3],
}

/// What a decompression needs once it has done all it can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Next {
    /// Run this game function, then resume with its R0.
    Call(GuestCall),
    /// Return from the SWI with this in R0.
    Return(u32),
}

/// A ReadByCallback decompression part-way through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Decompression {
    format: Format,
    /// The game's callback structure (R3).
    callbacks: u32,
    /// The source address (R0), past the data read so far.
    source: u32,
    /// The destination (R1) and Open's parameter (R2), which for Huffman
    /// is where the tree is copied to.
    destination: u32,
    parameter: u32,
    /// The decompressed length, from the header.
    length: u32,
    stage: Stage,
}

/// What a decompression is waiting for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Stage {
    /// Open's header word.
    Opening,
    /// The next unit of data, from Get_8bit or Get_32bit.
    Reading(Decoder, Output),
    /// Close's status.
    Closing,
}

impl Decompression {
    /// Starts a decompression from the SWI's R0–R3. It needs Open first.
    pub(crate) fn start<B: Bus>(format: Format, args: [u32; 4], bus: &mut B) -> (Self, GuestCall) {
        let [source, destination, parameter, callbacks] = args;
        let decompression = Self {
            format,
            callbacks,
            source,
            destination,
            parameter,
            length: 0,
            stage: Stage::Opening,
        };
        let open = decompression.call(bus, OPEN);
        (decompression, open)
    }

    /// Whether this decompression, read from a save state, holds only
    /// counts it could have reached ([`Decoder::in_range`]).
    pub(crate) fn in_range(&self) -> bool {
        match &self.stage {
            Stage::Opening | Stage::Closing => true,
            Stage::Reading(decoder, out) => decoder.in_range(out),
        }
    }

    /// Takes the R0 the last game function returned, decompresses as far
    /// as that allows, and says what comes next.
    pub(crate) fn resume<B: Bus>(&mut self, bus: &mut B, value: u32) -> Next {
        let error = (value as i32) < 0;
        match &mut self.stage {
            Stage::Opening if error => return Next::Return(value),
            Stage::Opening => {
                self.source = self.source.wrapping_add(4);
                self.length = value >> 8;
                self.stage = self.reading(value);
            }
            Stage::Reading(decoder, out) => {
                let unit = decoder.wants();
                decoder.feed(value, out, bus);
                let size = if unit == Unit::Byte { 1 } else { 4 };
                self.source = self.source.wrapping_add(size);
            }
            Stage::Closing => return Next::Return(if error { value } else { self.length }),
        }
        self.read_or_close(bus)
    }

    /// The decoder and output for data with this header.
    fn reading(&self, header: u32) -> Stage {
        let halfwords = Output::new(self.destination, 2, self.length);
        match self.format {
            Format::Lz77 => Stage::Reading(Decoder::Lz77(Lz77::default()), halfwords),
            Format::RunLength => {
                Stage::Reading(Decoder::RunLength(RunLength::default()), halfwords)
            }
            // Huffman writes whole words until it has covered the length.
            Format::Huffman => Stage::Reading(
                Decoder::Huffman(Huffman::new(header & 0xF, self.parameter)),
                Output::new(self.destination, 4, self.length.next_multiple_of(4)),
            ),
        }
    }

    /// The next read, or Close once the output is complete.
    fn read_or_close<B: Bus>(&mut self, bus: &mut B) -> Next {
        if let Stage::Reading(decoder, out) = &self.stage
            && !out.done()
        {
            let function = match decoder.wants() {
                Unit::Byte => GET_8BIT,
                Unit::Word => GET_32BIT,
            };
            return Next::Call(self.call(bus, function));
        }
        let close = self.call(bus, CLOSE);
        if close.function == 0 {
            return Next::Return(self.length);
        }
        self.stage = Stage::Closing;
        Next::Call(close)
    }

    /// A call to the function at `offset` in the callback structure. Each
    /// gets the source address in R0; only Open is promised the
    /// destination and parameter in R1 and R2, but they are the same for
    /// every call.
    fn call<B: Bus>(&self, bus: &mut B, offset: u32) -> GuestCall {
        let function = bus.read_32(self.callbacks.wrapping_add(offset), Access::NONSEQUENTIAL);
        GuestCall {
            function,
            args: [self.source, self.destination, self.parameter],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bios::tests::Ram;

    /// Where the test's callback structure is, and its pretend functions.
    const CALLBACKS: u32 = 0x300;
    const OPEN_FUNCTION: u32 = 0x1000;
    const CLOSE_FUNCTION: u32 = 0x1004;
    /// A Thumb function: bit 0 set.
    const GET_8BIT_FUNCTION: u32 = 0x1009;
    const GET_32BIT_FUNCTION: u32 = 0x1010;

    /// RAM with a callback structure, with or without Close.
    fn ram_with_callbacks(close: bool) -> Ram {
        let mut ram = Ram::new();
        let close = if close { CLOSE_FUNCTION } else { 0 };
        for (index, function) in [
            OPEN_FUNCTION,
            close,
            GET_8BIT_FUNCTION,
            0,
            GET_32BIT_FUNCTION,
        ]
        .into_iter()
        .enumerate()
        {
            ram.write_32(
                CALLBACKS + index as u32 * 4,
                function,
                Access::NONSEQUENTIAL,
            );
        }
        ram
    }

    /// Runs a decompression to the end, answering its calls as a game's
    /// callbacks that read memory would (GBATEK's examples: LDR R0,[R0]
    /// and LDRB R0,[R0]), with Close returning `close_status`. Returns
    /// the SWI's result and every call made.
    fn run(
        ram: &mut Ram,
        format: Format,
        args: [u32; 3],
        close_status: u32,
    ) -> (u32, Vec<GuestCall>) {
        let [source, destination, parameter] = args;
        let (mut decompression, mut call) =
            Decompression::start(format, [source, destination, parameter, CALLBACKS], ram);
        let mut calls = Vec::new();
        loop {
            calls.push(call);
            let address = call.args[0];
            let value = match call.function {
                OPEN_FUNCTION | GET_32BIT_FUNCTION => ram.read_32(address, Access::NONSEQUENTIAL),
                GET_8BIT_FUNCTION => u32::from(ram.read_8(address, Access::NONSEQUENTIAL)),
                CLOSE_FUNCTION => close_status,
                other => panic!("called {other:#X}"),
            };
            match decompression.resume(ram, value) {
                Next::Call(next) => call = next,
                Next::Return(result) => return (result, calls),
            }
        }
    }

    #[test]
    fn lz77_reads_through_the_callbacks_and_writes_halfwords() {
        let mut ram = ram_with_callbacks(true);
        ram.0[0x200..0x208].fill(0xEE);
        // Header: type 0x10, 7 bytes. "A", "B", then 5 from distance 2.
        let packed = [0x10, 7, 0, 0, 0b0010_0000, b'A', b'B', 0x20, 0x01];
        ram.0[0x100..0x109].copy_from_slice(&packed);
        let (result, calls) = run(&mut ram, Format::Lz77, [0x100, 0x200, 0x55], 0);
        assert_eq!(result, 7, "the length");
        // The odd last byte is never written.
        assert_eq!(&ram.0[0x200..0x208], b"ABABAB\xEE\xEE");
        let functions: Vec<u32> = calls.iter().map(|call| call.function).collect();
        let mut expected = vec![OPEN_FUNCTION];
        expected.extend([GET_8BIT_FUNCTION; 5]);
        expected.push(CLOSE_FUNCTION);
        assert_eq!(functions, expected);
        assert_eq!(calls[0].args, [0x100, 0x200, 0x55], "Open's arguments");
        let sources: Vec<u32> = calls.iter().map(|call| call.args[0]).collect();
        assert_eq!(sources, [0x100, 0x104, 0x105, 0x106, 0x107, 0x108, 0x109]);
    }

    #[test]
    fn run_length_reads_through_the_callbacks() {
        let mut ram = ram_with_callbacks(false);
        // Header: type 0x30, 6 bytes. "ok", then "?" four times.
        let packed = [0x30, 6, 0, 0, 0x01, b'o', b'k', 0x81, b'?'];
        ram.0[0x100..0x109].copy_from_slice(&packed);
        let (result, calls) = run(&mut ram, Format::RunLength, [0x100, 0x200, 0], 0);
        assert_eq!(result, 6);
        assert_eq!(&ram.0[0x200..0x206], b"ok????");
        // Open and five bytes; no Close in the structure.
        assert_eq!(calls.len(), 6);
    }

    #[test]
    fn huffman_copies_the_tree_and_reads_words() {
        let mut ram = ram_with_callbacks(true);
        ram.0[0x200..0x20C].fill(0xEE);
        // Header: type 0x20, 8-bit symbols, 5 bytes. The tree from the
        // decoder's test, padded to 8 bytes so the bitstream is aligned.
        let tree = [0x03, 0x80, 0x01, 0xC0, 0x02, 0x03, 0x00, 0x00];
        ram.0[0x100..0x104].copy_from_slice(&[0x28, 5, 0, 0]);
        ram.0[0x104..0x10C].copy_from_slice(&tree);
        // 1, 2, 3, 1, 2, 3, 1, 2: "0 10 11 0 10 11 0 10".
        let bits: u32 = 0b0101_1010_1101_0110_1011_0101_1010_1101;
        ram.0[0x10C..0x110].copy_from_slice(&bits.to_le_bytes());
        let (result, calls) = run(&mut ram, Format::Huffman, [0x100, 0x200, 0x400], 0);
        assert_eq!(result, 5);
        assert_eq!(&ram.0[0x400..0x408], &tree, "the tree is in the buffer");
        // Five bytes take two whole words.
        assert_eq!(
            &ram.0[0x200..0x20C],
            &[1, 2, 3, 1, 2, 3, 1, 2, 0xEE, 0xEE, 0xEE, 0xEE]
        );
        let word_reads: Vec<u32> = calls
            .iter()
            .filter(|call| call.function == GET_32BIT_FUNCTION)
            .map(|call| call.args[0])
            .collect();
        assert_eq!(word_reads, [0x10C], "one word read, after the tree");
    }

    #[test]
    fn open_and_close_errors_are_returned() {
        let mut ram = ram_with_callbacks(true);
        // Open returns the "header" it reads: here, a negative error.
        ram.write_32(0x100, 0xFFFF_FFF0, Access::NONSEQUENTIAL);
        let (result, calls) = run(&mut ram, Format::Lz77, [0x100, 0x200, 0], 0);
        assert_eq!((result, calls.len()), (0xFFFF_FFF0, 1), "Open's error");
        ram.0[0x100..0x106].copy_from_slice(&[0x10, 1, 0, 0, 0x00, b'x']);
        let (result, _) = run(&mut ram, Format::Lz77, [0x100, 0x200, 0], 0x8000_0001);
        assert_eq!(result, 0x8000_0001, "Close's error");
    }
}
