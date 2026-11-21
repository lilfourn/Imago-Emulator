//! xdelta's patches: VCDIFF (RFC 3284), as xdelta3 writes it.
//!
//! A patch is a header, then windows that each make the next stretch of
//! the target. A window may name a *segment*, a stretch of the source
//! (or of the target made so far) that its copies read from. It carries
//! three sections: data, instructions and addresses. Each instruction
//! byte indexes the default code table (RFC 3284, section 5.6) for one
//! or two of ADD (bytes from the data section), RUN (one data byte,
//! repeated) and COPY (bytes from the segment, or from the window's own
//! output so far, at an address coded against a cache of recent ones).
//!
//! xdelta3 adds three things, all read here:
//!
//! - an application header, which names the files (skipped);
//! - an Adler-32 checksum of each window's output, which is checked;
//! - *secondary compression* of the sections. xdelta3 3.0.10 and later
//!   compress with LZMA when built with it, as usual; this is read.
//!   Older versions compress only when asked to, with their own DJW or
//!   FGK Huffman codes, which are not read.
//!
//! LZMA compression is one `.xz` stream for each kind of section, over
//! the whole patch: each compressed section is its size, then the next
//! LZMA2 chunks of its kind's stream, and only the first holds the
//! stream's headers. A window's sections unpack only after every earlier
//! window's, whose bytes their chunks may copy, so all are unpacked
//! before any window runs. xdelta3 never finishes the streams, so they
//! have no end marker, index or footer.
//!
//! A custom code table (VCD_CODETABLE), which xdelta3 does not write, is
//! not read either.

use std::ops::Range;

use crate::PatchError;
use crate::reader::Reader;

/// The first bytes of a patch: "VCD" with the top bits set, then the
/// version, 0.
pub const MAGIC: [u8; 4] = [0xD6, 0xC3, 0xC4, 0x00];

// The header's indicator.
const SECONDARY: u8 = 0x01;
const CODE_TABLE: u8 = 0x02;
const APP_HEADER: u8 = 0x04;

// Secondary compressors, by xdelta3's numbers.
const DJW: u8 = 1;
const LZMA: u8 = 2;
const FGK: u8 = 16;

// A window's indicator.
const SOURCE_SEGMENT: u8 = 0x01;
const TARGET_SEGMENT: u8 = 0x02;
const CHECKSUM: u8 = 0x04;

/// The start of an `.xz` stream.
const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];

pub fn apply(patch: &[u8], source: &[u8], limit: usize) -> Result<Vec<u8>, PatchError> {
    let mut reader = Reader::new(patch);
    reader.expect(&MAGIC)?;
    let indicator = reader.byte()?;
    let secondary = indicator & SECONDARY != 0;
    if secondary {
        match reader.byte()? {
            LZMA => {}
            DJW => return Err(PatchError::Unsupported("xdelta's DJW compression")),
            FGK => return Err(PatchError::Unsupported("xdelta's FGK compression")),
            _ => return Err(PatchError::Unsupported("an unknown compression")),
        }
    }
    if indicator & CODE_TABLE != 0 {
        return Err(PatchError::Unsupported("a code table of its own"));
    }
    if indicator & APP_HEADER != 0 {
        let length = reader.vcdiff_number()?;
        reader.take(length)?;
    }
    let mut windows = Vec::new();
    let mut length: usize = 0;
    while !reader.is_empty() {
        let window = Window::read(&mut reader, secondary)?;
        length = length
            .checked_add(window.length)
            .filter(|length| *length <= limit)
            .ok_or(PatchError::TooBig { limit })?;
        windows.push(window);
    }
    // A patch that makes nothing has lost its windows.
    if windows.is_empty() {
        return Err(PatchError::Damaged);
    }
    let unpacked = [0, 1, 2].map(|kind| unpack(&windows, kind, limit));
    let [data, instructions, addresses] = unpacked;
    let (data, instructions, addresses) = (data?, instructions?, addresses?);
    let mut sections = [
        data.as_slice(),
        instructions.as_slice(),
        addresses.as_slice(),
    ];
    let mut target = Vec::with_capacity(length);
    for window in &windows {
        let mut own = [&[][..]; 3];
        for (kind, section) in window.sections.iter().enumerate() {
            own[kind] = match *section {
                Section::Plain(bytes) => bytes,
                Section::Compressed { size, .. } => {
                    let (taken, rest) = sections[kind].split_at(size);
                    sections[kind] = rest;
                    taken
                }
            };
        }
        window.make(own, source, &mut target)?;
    }
    Ok(target)
}

/// A window, as the patch has it.
struct Window<'a> {
    segment: Option<Segment>,
    /// How many bytes of the target it makes.
    length: usize,
    checksum: Option<u32>,
    /// Data, instructions and addresses.
    sections: [Section<'a>; 3],
}

/// Where a window's segment is: in the source, or in the target made
/// before the window.
struct Segment {
    in_source: bool,
    range: Range<usize>,
}

/// A section, which may be compressed.
#[derive(Clone, Copy)]
enum Section<'a> {
    Plain(&'a [u8]),
    /// `size` bytes unpacked, from the next `chunks` of its kind's
    /// stream.
    Compressed {
        size: usize,
        chunks: &'a [u8],
    },
}

impl<'a> Window<'a> {
    /// Reads the next window from `reader`. Its sections may be
    /// compressed if the patch has a `secondary` compressor.
    fn read(reader: &mut Reader<'a>, secondary: bool) -> Result<Self, PatchError> {
        let indicator = reader.byte()?;
        let segment = if indicator & (SOURCE_SEGMENT | TARGET_SEGMENT) != 0 {
            let length = reader.vcdiff_number()?;
            let position = reader.vcdiff_number()?;
            let end = position.checked_add(length).ok_or(PatchError::Damaged)?;
            Some(Segment {
                in_source: indicator & SOURCE_SEGMENT != 0,
                range: position..end,
            })
        } else {
            None
        };
        let delta_length = reader.vcdiff_number()?;
        let mut delta = Reader::new(reader.take(delta_length)?);
        let length = delta.vcdiff_number()?;
        let compressed = delta.byte()?;
        if compressed != 0 && !secondary {
            return Err(PatchError::Damaged);
        }
        let lengths = [
            delta.vcdiff_number()?,
            delta.vcdiff_number()?,
            delta.vcdiff_number()?,
        ];
        let checksum = if indicator & CHECKSUM != 0 {
            Some(delta.big_endian(4)?)
        } else {
            None
        };
        let mut sections = [Section::Plain(&[]); 3];
        for (kind, length) in lengths.into_iter().enumerate() {
            let bytes = delta.take(length)?;
            sections[kind] = if compressed & 1 << kind == 0 {
                Section::Plain(bytes)
            } else {
                let mut bytes = Reader::new(bytes);
                let size = bytes.vcdiff_number()?;
                Section::Compressed {
                    size,
                    chunks: bytes.take(bytes.len())?,
                }
            };
        }
        if !delta.is_empty() {
            return Err(PatchError::Damaged);
        }
        Ok(Self {
            segment,
            length,
            checksum,
            sections,
        })
    }

    /// Appends what the window makes to `target`, from its `sections`,
    /// unpacked.
    fn make(
        &self,
        sections: [&[u8]; 3],
        source: &[u8],
        target: &mut Vec<u8>,
    ) -> Result<(), PatchError> {
        let target_segment: Vec<u8>;
        let segment: &[u8] = match &self.segment {
            // Past the end of the source, it is for a bigger ROM.
            Some(segment) if segment.in_source => source
                .get(segment.range.clone())
                .ok_or(PatchError::WrongResult)?,
            Some(segment) => {
                target_segment = target
                    .get(segment.range.clone())
                    .ok_or(PatchError::Damaged)?
                    .to_vec();
                &target_segment
            }
            None => &[],
        };
        let [data, instructions, addresses] = sections;
        let start = target.len();
        let mut run = Run {
            segment,
            target,
            start,
            end: start + self.length,
            data: Reader::new(data),
            addresses: Reader::new(addresses),
            cache: AddressCache::new(),
        };
        let mut instructions = Reader::new(instructions);
        while !instructions.is_empty() {
            for op in CODE_TABLE_ENTRIES[usize::from(instructions.byte()?)] {
                if op.kind == Kind::Noop {
                    continue;
                }
                let size = match op.size {
                    0 => instructions.vcdiff_number()?,
                    size => usize::from(size),
                };
                run.execute(op, size)?;
            }
        }
        if target.len() != start + self.length {
            return Err(PatchError::Damaged);
        }
        if self
            .checksum
            .is_some_and(|checksum| checksum != adler32(&target[start..]))
        {
            return Err(PatchError::WrongResult);
        }
        Ok(())
    }
}

/// Every compressed section of `kind` in `windows`, unpacked and joined.
fn unpack(windows: &[Window<'_>], kind: usize, limit: usize) -> Result<Vec<u8>, PatchError> {
    let mut stream = Vec::new();
    let mut size: usize = 0;
    let mut headed = false;
    for window in windows {
        let Section::Compressed {
            size: section_size,
            chunks,
        } = window.sections[kind]
        else {
            continue;
        };
        size = size
            .checked_add(section_size)
            .filter(|size| *size <= limit)
            .ok_or(PatchError::TooBig { limit })?;
        let mut chunks = Reader::new(chunks);
        if !headed && !chunks.is_empty() {
            // The stream header: magic, flags and their CRC-32. Then the
            // block header, whose first byte gives its length in 4-byte
            // units, less one.
            chunks.expect(&XZ_MAGIC)?;
            chunks.take(6)?;
            let block_header = (usize::from(chunks.byte()?) + 1) * 4;
            chunks.take(block_header - 1)?;
            headed = true;
        }
        let chunks = chunks.take(chunks.len())?;
        if lzma2_length(chunks, section_size)? != chunks.len() {
            return Err(PatchError::Damaged);
        }
        stream.extend_from_slice(chunks);
    }
    if stream.is_empty() {
        return Ok(Vec::new());
    }
    // The end marker xdelta3 leaves out.
    stream.push(0x00);
    let mut unpacked = Vec::with_capacity(size);
    lzma_rs::lzma2_decompress(&mut stream.as_slice(), &mut unpacked)
        .map_err(|_| PatchError::Damaged)?;
    if unpacked.len() != size {
        return Err(PatchError::Damaged);
    }
    Ok(unpacked)
}

/// How many bytes of `chunks`, LZMA2 chunks, hold `size` bytes unpacked,
/// as their headers say. Checked before unpacking, so that nothing
/// unpacks to more than its sections' sizes.
fn lzma2_length(chunks: &[u8], size: usize) -> Result<usize, PatchError> {
    let mut reader = Reader::new(chunks);
    let mut unpacked = 0;
    while unpacked < size {
        let control = reader.byte()?;
        let (chunk_unpacked, packed) = match control {
            // Stored: the size, less one.
            0x01 | 0x02 => {
                let length = reader.big_endian(2)? as usize + 1;
                (length, length)
            }
            // Compressed: the unpacked size's top bits are in the
            // control byte. From 0xC0, a properties byte follows.
            0x80..=0xFF => {
                let high = u32::from(control & 0x1F) << 16;
                let unpacked = (high | reader.big_endian(2)?) as usize + 1;
                let packed = reader.big_endian(2)? as usize + 1;
                if control >= 0xC0 {
                    reader.byte()?;
                }
                (unpacked, packed)
            }
            // The end, before the section's size; or reserved.
            _ => return Err(PatchError::Damaged),
        };
        reader.take(packed)?;
        unpacked += chunk_unpacked;
    }
    if unpacked != size {
        return Err(PatchError::Damaged);
    }
    Ok(chunks.len() - reader.len())
}

/// One window's instructions being carried out.
struct Run<'a, 'b> {
    segment: &'a [u8],
    target: &'b mut Vec<u8>,
    /// Where the window's output starts in `target`.
    start: usize,
    /// Where it must end.
    end: usize,
    data: Reader<'a>,
    addresses: Reader<'a>,
    cache: AddressCache,
}

impl Run<'_, '_> {
    fn execute(&mut self, op: Op, size: usize) -> Result<(), PatchError> {
        if size > self.end - self.target.len() {
            return Err(PatchError::Damaged);
        }
        match op.kind {
            Kind::Noop => {}
            Kind::Add => self.target.extend_from_slice(self.data.take(size)?),
            Kind::Run => {
                let byte = self.data.byte()?;
                self.target.resize(self.target.len() + size, byte);
            }
            Kind::Copy => {
                // Addresses run through the segment, then this window's
                // output so far.
                let made = self.target.len() - self.start;
                let here = self.segment.len() + made;
                let address = self.cache.address(&mut self.addresses, op.mode, here)?;
                self.copy(address, size);
            }
        }
        Ok(())
    }

    /// Copies `size` bytes from `address`, which is before here. A copy
    /// that reaches past here repeats what it has just written.
    fn copy(&mut self, address: usize, size: usize) {
        let segment = self.segment.len();
        if address + size <= segment {
            self.target
                .extend_from_slice(&self.segment[address..address + size]);
        } else if address >= segment && address + size <= self.target.len() - self.start + segment {
            let from = self.start + address - segment;
            self.target.extend_from_within(from..from + size);
        } else {
            for at in address..address + size {
                let byte = match self.segment.get(at) {
                    Some(byte) => *byte,
                    None => self.target[self.start + at - segment],
                };
                self.target.push(byte);
            }
        }
    }
}

/// The recent addresses COPYs are coded against (RFC 3284, section 5.1):
/// the last four, and 768 more by their value.
struct AddressCache {
    near: [usize; 4],
    next: usize,
    same: [usize; 3 * 256],
}

impl AddressCache {
    fn new() -> Self {
        Self {
            near: [0; 4],
            next: 0,
            same: [0; 3 * 256],
        }
    }

    /// Reads the address of a COPY in `mode` at `here` from `addresses`,
    /// and remembers it.
    fn address(
        &mut self,
        addresses: &mut Reader<'_>,
        mode: u8,
        here: usize,
    ) -> Result<usize, PatchError> {
        let address = match mode {
            // Self: as it is.
            0 => Some(addresses.vcdiff_number()?),
            // Here: back from here.
            1 => here.checked_sub(addresses.vcdiff_number()?),
            // Near: on from a recent one.
            2..=5 => self.near[usize::from(mode - 2)].checked_add(addresses.vcdiff_number()?),
            // Same: one remembered by value, picked by one byte.
            6..=8 => Some(self.same[usize::from(mode - 6) * 256 + usize::from(addresses.byte()?)]),
            _ => None,
        }
        .filter(|address| *address < here)
        .ok_or(PatchError::Damaged)?;
        self.near[self.next] = address;
        self.next = (self.next + 1) % self.near.len();
        self.same[address % self.same.len()] = address;
        Ok(address)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Noop,
    Add,
    Run,
    Copy,
}

/// Half of a code table entry. A size of 0 is read from the
/// instructions.
#[derive(Debug, Clone, Copy)]
struct Op {
    kind: Kind,
    size: u8,
    mode: u8,
}

const NOOP: Op = Op {
    kind: Kind::Noop,
    size: 0,
    mode: 0,
};

/// RFC 3284's default code table, section 5.6.
static CODE_TABLE_ENTRIES: [[Op; 2]; 256] = default_code_table();

const fn default_code_table() -> [[Op; 2]; 256] {
    const fn op(kind: Kind, size: u8, mode: u8) -> Op {
        Op { kind, size, mode }
    }
    let mut table = [[NOOP; 2]; 256];
    table[0][0] = op(Kind::Run, 0, 0);
    let mut index = 1;
    let mut size = 0;
    while size <= 17 {
        table[index][0] = op(Kind::Add, size, 0);
        index += 1;
        size += 1;
    }
    let mut mode = 0;
    while mode <= 8 {
        table[index][0] = op(Kind::Copy, 0, mode);
        index += 1;
        let mut size = 4;
        while size <= 18 {
            table[index][0] = op(Kind::Copy, size, mode);
            index += 1;
            size += 1;
        }
        mode += 1;
    }
    let mut mode = 0;
    while mode <= 8 {
        // Modes 0 to 5 pair ADDs with COPYs of 4 to 6 bytes; 6 to 8,
        // with COPYs of 4.
        let longest_copy = if mode <= 5 { 6 } else { 4 };
        let mut add = 1;
        while add <= 4 {
            let mut copy = 4;
            while copy <= longest_copy {
                table[index] = [op(Kind::Add, add, 0), op(Kind::Copy, copy, mode)];
                index += 1;
                copy += 1;
            }
            add += 1;
        }
        mode += 1;
    }
    let mut mode = 0;
    while mode <= 8 {
        table[index] = [op(Kind::Copy, 4, mode), op(Kind::Add, 1, 0)];
        index += 1;
        mode += 1;
    }
    assert!(index == 256);
    table
}

/// Adler-32 (RFC 1950), as xdelta3 checks each window.
fn adler32(bytes: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1_u32, 0_u32);
    // The most bytes before `b` could overflow.
    for chunk in bytes.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    b << 16 | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_code_table_matches_the_rfc() {
        // Spot checks against RFC 3284's table: index, then its halves as
        // (kind, size, mode).
        let check = |index: usize, first: (Kind, u8, u8), second: Option<(Kind, u8, u8)>| {
            let [one, two] = CODE_TABLE_ENTRIES[index];
            assert_eq!((one.kind, one.size, one.mode), first, "entry {index}");
            let two = (two.kind != Kind::Noop).then_some((two.kind, two.size, two.mode));
            assert_eq!(two, second, "entry {index}");
        };
        check(0, (Kind::Run, 0, 0), None);
        check(1, (Kind::Add, 0, 0), None);
        check(18, (Kind::Add, 17, 0), None);
        check(19, (Kind::Copy, 0, 0), None);
        check(20, (Kind::Copy, 4, 0), None);
        check(34, (Kind::Copy, 18, 0), None);
        check(35, (Kind::Copy, 0, 1), None);
        check(162, (Kind::Copy, 18, 8), None);
        check(163, (Kind::Add, 1, 0), Some((Kind::Copy, 4, 0)));
        check(234, (Kind::Add, 4, 0), Some((Kind::Copy, 6, 5)));
        check(235, (Kind::Add, 1, 0), Some((Kind::Copy, 4, 6)));
        check(246, (Kind::Add, 4, 0), Some((Kind::Copy, 4, 8)));
        check(247, (Kind::Copy, 4, 0), Some((Kind::Add, 1, 0)));
        check(255, (Kind::Copy, 4, 8), Some((Kind::Add, 1, 0)));
    }

    #[test]
    fn adler32_matches_known_values() {
        assert_eq!(adler32(b""), 1);
        // RFC 1950's own example is not given; Wikipedia's is.
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        // Long enough that the sums wrap many times over: zlib's value.
        assert_eq!(adler32(&[0xFF; 100_000]), 0x149A_302C);
    }

    #[test]
    fn copies_from_the_segment_and_repeat_their_own_output() {
        // One window over the source "abc": COPY 3 from address 0 (the
        // segment), then a COPY of 6 from address 3, which is its own
        // output, so it repeats "abc".
        let source = b"abc";
        let mut window = vec![SOURCE_SEGMENT, 3, 0];
        let instructions = [19, 3, 19, 6];
        let addresses = [0, 3];
        let mut delta = vec![9, 0, 0, instructions.len() as u8, addresses.len() as u8];
        delta.extend_from_slice(&instructions);
        delta.extend_from_slice(&addresses);
        window.push(delta.len() as u8);
        window.extend_from_slice(&delta);
        let mut patch = MAGIC.to_vec();
        patch.push(0);
        patch.extend_from_slice(&window);
        assert_eq!(apply(&patch, source, 1 << 20), Ok(b"abcabcabc".to_vec()));
    }
}
