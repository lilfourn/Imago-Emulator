//! Save-state container format.
//!
//! A core writes its own state bytes ([`emu_core::Core::save_state`]); this
//! crate wraps them in a header that says which system, core state version
//! and ROM they belong to, plus a checksum. Checking the header against the
//! running game is the caller's job, so a frontend can choose to warn or
//! refuse.
//!
//! Layout, all integers little-endian:
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 8 | magic `EMUSTATE` |
//! | 8 | 2 | container version, currently 1 |
//! | 10 | 1 | [`System::tag`] |
//! | 11 | 1 | reserved, 0 |
//! | 12 | 2 | core state version ([`emu_core::CoreInfo::state_version`]) |
//! | 14 | 2 | reserved, 0 |
//! | 16 | 4 | CRC-32 of the ROM |
//! | 20 | 4 | payload length `n` |
//! | 24 | n | payload |
//! | 24 + n | 4 | CRC-32 of bytes `0..24 + n` |

use emu_core::System;

const MAGIC: &[u8; 8] = b"EMUSTATE";
const CONTAINER_VERSION: u16 = 1;
const HEADER_LEN: usize = 24;
const CHECKSUM_LEN: usize = 4;

/// What a save state belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The system that wrote it.
    pub system: System,
    /// The core's state format version.
    pub state_version: u16,
    /// CRC-32 of the ROM that was running.
    pub rom_crc32: u32,
}

/// Wraps `payload` in a container.
///
/// # Panics
///
/// Panics if `payload` is 4 GiB or larger, which no supported system
/// approaches.
#[must_use]
pub fn encode(header: &Header, payload: &[u8]) -> Vec<u8> {
    let payload_len = u32::try_from(payload.len()).expect("save state payload under 4 GiB");
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + CHECKSUM_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
    out.push(header.system.tag());
    out.push(0);
    out.extend_from_slice(&header.state_version.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&header.rom_crc32.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(payload);
    let checksum = crc32fast::hash(&out);
    out.extend_from_slice(&checksum.to_le_bytes());
    out
}

/// Splits a container into its header and the core's payload.
///
/// # Errors
///
/// Returns [`DecodeError`] if the bytes are not a well-formed container.
pub fn decode(bytes: &[u8]) -> Result<(Header, &[u8]), DecodeError> {
    let (header, rest) = bytes
        .split_first_chunk::<HEADER_LEN>()
        .ok_or(DecodeError::Truncated)?;
    if &header[0..8] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = u16::from_le_bytes([header[8], header[9]]);
    if version != CONTAINER_VERSION {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    let system = System::from_tag(header[10]).ok_or(DecodeError::UnknownSystem(header[10]))?;
    let state_version = u16::from_le_bytes([header[12], header[13]]);
    let rom_crc32 = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
    let payload_len = u32::from_le_bytes([header[20], header[21], header[22], header[23]]);
    let payload_len = usize::try_from(payload_len).map_err(|_| DecodeError::Truncated)?;

    if rest.len() != payload_len + CHECKSUM_LEN {
        return Err(DecodeError::Truncated);
    }
    let (payload, checksum) = rest.split_at(payload_len);
    let checksum = u32::from_le_bytes([checksum[0], checksum[1], checksum[2], checksum[3]]);
    if crc32fast::hash(&bytes[..HEADER_LEN + payload_len]) != checksum {
        return Err(DecodeError::ChecksumMismatch);
    }

    let header = Header {
        system,
        state_version,
        rom_crc32,
    };
    Ok((header, payload))
}

/// Why bytes could not be decoded as a save state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// Shorter or longer than the header says.
    #[error("save state is truncated or has trailing bytes")]
    Truncated,
    /// Not a save state file.
    #[error("not a save state (bad magic)")]
    BadMagic,
    /// Written by a newer, incompatible build.
    #[error("save state container version {0} is not supported")]
    UnsupportedVersion(u16),
    /// The system tag is not one this build knows.
    #[error("save state is for unknown system tag {0}")]
    UnknownSystem(u8),
    /// The bytes were corrupted.
    #[error("save state checksum does not match")]
    ChecksumMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: Header = Header {
        system: System::Gba,
        state_version: 3,
        rom_crc32: 0xDEAD_BEEF,
    };

    #[test]
    fn round_trips_header_and_payload() {
        let bytes = encode(&HEADER, b"machine state");
        assert_eq!(decode(&bytes), Ok((HEADER, &b"machine state"[..])));
    }

    #[test]
    fn round_trips_empty_payload() {
        let bytes = encode(&HEADER, &[]);
        assert_eq!(decode(&bytes), Ok((HEADER, &[][..])));
    }

    #[test]
    fn rejects_other_files() {
        assert_eq!(
            decode(b"not a state file at all, really"),
            Err(DecodeError::BadMagic)
        );
        assert_eq!(decode(b"short"), Err(DecodeError::Truncated));
    }

    #[test]
    fn rejects_truncation_and_corruption() {
        let bytes = encode(&HEADER, b"machine state");
        assert_eq!(
            decode(&bytes[..bytes.len() - 1]),
            Err(DecodeError::Truncated)
        );

        let mut corrupted = bytes;
        corrupted[HEADER_LEN] ^= 1;
        assert_eq!(decode(&corrupted), Err(DecodeError::ChecksumMismatch));
    }
}
