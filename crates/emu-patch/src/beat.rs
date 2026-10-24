//! UPS and BPS, byuu's formats, which end alike: the CRC-32s of the
//! source, the target and the patch before its own, little-endian.

use crate::PatchError;
use crate::reader::Reader;

/// The footer's length.
const FOOTER: usize = 12;

/// What a UPS or BPS patch says outside its body.
struct Checked<'a> {
    /// The patch between its magic and its footer.
    body: Reader<'a>,
    source_crc32: u32,
    target_crc32: u32,
}

/// Opens the UPS or BPS `patch`, which starts with `magic`, if its own
/// checksum holds.
fn open<'a>(patch: &'a [u8], magic: &[u8]) -> Result<Checked<'a>, PatchError> {
    let Some(body_end) = patch.len().checked_sub(FOOTER) else {
        return Err(PatchError::Damaged);
    };
    let (body, footer) = patch.split_at(body_end);
    let crc = |at: usize| {
        u32::from_le_bytes([footer[at], footer[at + 1], footer[at + 2], footer[at + 3]])
    };
    if crc32fast::hash(&patch[..patch.len() - 4]) != crc(8) {
        return Err(PatchError::Damaged);
    }
    let mut body = Reader::new(body);
    body.expect(magic)?;
    Ok(Checked {
        body,
        source_crc32: crc(0),
        target_crc32: crc(4),
    })
}

pub fn source_crc32(patch: &[u8]) -> Option<u32> {
    let at = patch.len().checked_sub(FOOTER)?;
    Some(u32::from_le_bytes(patch[at..at + 4].try_into().ok()?))
}

/// Fails unless `source`, `size` bytes long, is the patch's source.
fn check_source(source: &[u8], size: usize, patch: &Checked<'_>) -> Result<(), PatchError> {
    let actual = crc32fast::hash(source);
    if actual == patch.source_crc32 && source.len() == size {
        Ok(())
    } else if actual == patch.target_crc32 {
        Err(PatchError::AlreadyPatched)
    } else {
        Err(PatchError::WrongSource {
            expected: patch.source_crc32,
            actual,
        })
    }
}

/// Fails unless `target` is the patch's target.
fn check_target(target: &[u8], patch: &Checked<'_>) -> Result<(), PatchError> {
    if crc32fast::hash(target) == patch.target_crc32 {
        Ok(())
    } else {
        Err(PatchError::WrongResult)
    }
}

/// UPS: the source and target sizes, then hunks to the footer. Each
/// hunk skips some bytes, which stay as the source has them, then XORs
/// bytes with the patch's up to and including a zero. The source reads
/// as zeros past its end, and writes past the target's end are dropped.
/// (UPS also undoes itself, turning the target back into the source;
/// that way is not offered.)
pub fn apply_ups(patch: &[u8], source: &[u8], limit: usize) -> Result<Vec<u8>, PatchError> {
    let mut patch = open(patch, b"UPS1")?;
    let source_size = patch.body.beat_number()?;
    let target_size = patch.body.beat_number()?;
    check_source(source, source_size, &patch)?;
    if target_size > limit {
        return Err(PatchError::TooBig { limit });
    }
    let mut target = vec![0; target_size];
    let kept = source.len().min(target_size);
    target[..kept].copy_from_slice(&source[..kept]);
    let mut at: usize = 0;
    while !patch.body.is_empty() {
        at = at
            .checked_add(patch.body.beat_number()?)
            .ok_or(PatchError::Damaged)?;
        loop {
            let xor = patch.body.byte()?;
            if let Some(byte) = target.get_mut(at) {
                *byte ^= xor;
            }
            at = at.saturating_add(1);
            if xor == 0 {
                break;
            }
        }
    }
    check_target(&target, &patch)?;
    Ok(target)
}

/// BPS: the source and target sizes and metadata, then commands to the
/// footer that write the target front to back. Each command is a number
/// whose low two bits say what to do and whose rest, plus one, is how
/// many bytes:
///
/// - 0, source read: the source's bytes at the same offset;
/// - 1, target read: bytes from the patch;
/// - 2, source copy: the source's bytes from a cursor;
/// - 3, target copy: the target's own bytes from a cursor, which may
///   overlap what this command writes, repeating them.
///
/// Copies first move their cursor by a signed number: the lowest bit is
/// the sign. Each copied byte moves it on by one.
pub fn apply_bps(patch: &[u8], source: &[u8], limit: usize) -> Result<Vec<u8>, PatchError> {
    let mut patch = open(patch, b"BPS1")?;
    let source_size = patch.body.beat_number()?;
    let target_size = patch.body.beat_number()?;
    let metadata_size = patch.body.beat_number()?;
    patch.body.take(metadata_size)?;
    check_source(source, source_size, &patch)?;
    if target_size > limit {
        return Err(PatchError::TooBig { limit });
    }
    let mut target = Vec::with_capacity(target_size);
    let mut source_cursor: usize = 0;
    let mut target_cursor: usize = 0;
    while !patch.body.is_empty() {
        let command = patch.body.beat_number()?;
        let length = (command >> 2) + 1;
        let start = target.len();
        let end = start
            .checked_add(length)
            .filter(|end| *end <= target_size)
            .ok_or(PatchError::Damaged)?;
        match command & 3 {
            0 => target.extend_from_slice(source.get(start..end).ok_or(PatchError::Damaged)?),
            1 => target.extend_from_slice(patch.body.take(length)?),
            2 => {
                source_cursor = moved(source_cursor, patch.body.beat_number()?)?;
                let copied = source_cursor
                    .checked_add(length)
                    .and_then(|copied_end| source.get(source_cursor..copied_end))
                    .ok_or(PatchError::Damaged)?;
                target.extend_from_slice(copied);
                source_cursor += length;
            }
            _ => {
                target_cursor = moved(target_cursor, patch.body.beat_number()?)?;
                if target_cursor >= start {
                    return Err(PatchError::Damaged);
                }
                if target_cursor + length <= start {
                    target.extend_from_within(target_cursor..target_cursor + length);
                } else {
                    // Overlapping: each byte written is there to be read.
                    for at in target_cursor..target_cursor + length {
                        target.push(target[at]);
                    }
                }
                target_cursor += length;
            }
        }
    }
    if target.len() != target_size {
        return Err(PatchError::Damaged);
    }
    check_target(&target, &patch)?;
    Ok(target)
}

/// `cursor` moved by `step`, whose lowest bit is the sign.
fn moved(cursor: usize, step: usize) -> Result<usize, PatchError> {
    let distance = step >> 1;
    if step & 1 == 0 {
        cursor.checked_add(distance)
    } else {
        cursor.checked_sub(distance)
    }
    .ok_or(PatchError::Damaged)
}
