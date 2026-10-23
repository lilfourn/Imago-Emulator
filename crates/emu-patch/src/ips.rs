//! IPS: `PATCH`, then records until `EOF`. Each record is a 3-byte
//! big-endian offset and a 2-byte size, then that many bytes to write
//! there; a size of 0 instead gives a 2-byte count and one byte to write
//! that many times. Writes past the end grow the ROM, with zeros between.
//! Lunar IPS adds a 3-byte size after `EOF` to cut the ROM to.

use crate::PatchError;
use crate::reader::Reader;

pub fn apply(patch: &[u8], source: &[u8], limit: usize) -> Result<Vec<u8>, PatchError> {
    if source.len() > limit {
        return Err(PatchError::TooBig { limit });
    }
    let mut reader = Reader::new(patch);
    reader.expect(b"PATCH")?;
    let mut target = source.to_vec();
    loop {
        let offset = reader.take(3)?;
        if offset == b"EOF" {
            break;
        }
        let offset = Reader::new(offset).big_endian(3)? as usize;
        let size = reader.big_endian(2)? as usize;
        let (size, fill) = if size == 0 {
            (reader.big_endian(2)? as usize, Some(reader.byte()?))
        } else {
            (size, None)
        };
        let end = offset + size;
        if end > limit {
            return Err(PatchError::TooBig { limit });
        }
        if target.len() < end {
            target.resize(end, 0);
        }
        match fill {
            Some(byte) => target[offset..end].fill(byte),
            None => target[offset..end].copy_from_slice(reader.take(size)?),
        }
    }
    // Anything else after `EOF` is not a size, and is left alone.
    if reader.len() == 3 {
        let size = reader.big_endian(3)? as usize;
        target.truncate(size);
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 1 << 20;

    #[test]
    fn writes_records_and_runs() {
        let patch = b"PATCH\x00\x00\x01\x00\x02AB\x00\x00\x04\x00\x00\x00\x03ZEOF";
        assert_eq!(apply(patch, b"012345678", LIMIT), Ok(b"0AB3ZZZ78".to_vec()));
    }

    #[test]
    fn grows_and_truncates_the_rom() {
        let grow = b"PATCH\x00\x00\x06\x00\x01!EOF";
        assert_eq!(apply(grow, b"0123", LIMIT), Ok(b"0123\0\0!".to_vec()));
        let cut = b"PATCH\x00\x00\x00\x00\x01!EOF\x00\x00\x02";
        assert_eq!(apply(cut, b"0123", LIMIT), Ok(b"!1".to_vec()));
    }

    #[test]
    fn refuses_cut_short_and_oversized_patches() {
        assert_eq!(
            apply(b"PATCH\x00\x00\x00", b"0", LIMIT),
            Err(PatchError::Damaged)
        );
        assert_eq!(
            apply(b"PATCH\x00\x00\x00\x00\x05ab", b"0", LIMIT),
            Err(PatchError::Damaged)
        );
        assert_eq!(
            apply(b"PATCH\x10\x00\x00\x00\x01!EOF", b"0", LIMIT),
            Err(PatchError::TooBig { limit: LIMIT })
        );
    }
}
