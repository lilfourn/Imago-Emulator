//! Save states: the container, and the payload inside it.
//!
//! A core writes its own state bytes ([`emu_core::Core::save_state`]),
//! the payload, with the helpers in [`payload`], which also check what a
//! core reads back. This crate wraps the payload in a header that says
//! which system, core state version and ROM it belongs to, an optional
//! screenshot for slot pickers, and a checksum. Checking the header
//! against the running game is the caller's job, so a frontend can choose
//! to warn or refuse: [`Header::check`] makes the comparison and words
//! the [`Mismatch`]. [`mutation`] is for the cores' tests: it loads
//! states that are wrong in one byte.
//!
//! Layout of version 2, all integers little-endian:
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 8 | magic `EMUSTATE` |
//! | 8 | 2 | container version, 2 |
//! | 10 | 1 | [`System::tag`] |
//! | 11 | 1 | reserved, 0 |
//! | 12 | 2 | core state version ([`emu_core::CoreInfo::state_version`]) |
//! | 14 | 2 | reserved, 0 |
//! | 16 | 4 | CRC-32 of the ROM |
//! | 20 | 4 | payload length `n` |
//! | 24 | 2 | thumbnail width `w`, 0 for none |
//! | 26 | 2 | thumbnail height `h` |
//! | 28 | n | payload |
//! | 28 + n | 3wh | thumbnail, RGB, row-major |
//! | end − 4 | 4 | CRC-32 of every byte before it |
//!
//! Version 1 is the same without the thumbnail fields: a 24-byte header,
//! the payload, then the checksum. [`decode`] reads both.

pub mod mutation;
pub mod payload;

use emu_core::System;

const MAGIC: &[u8; 8] = b"EMUSTATE";
const CONTAINER_VERSION: u16 = 2;
const HEADER_LEN: usize = 28;
const V1_HEADER_LEN: usize = 24;
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

impl Header {
    /// How many bytes from the start of a container [`Header::peek`]
    /// reads.
    pub const PEEK_LEN: usize = 20;

    /// The header of the container that begins with `start`, read without
    /// the rest of the file: for telling whose state a file is. Nothing
    /// else of the container is checked, neither its version, its length
    /// nor its checksum, so loading it still takes [`decode`].
    ///
    /// `None` if `start` is shorter than [`Header::PEEK_LEN`], does not
    /// begin a save state, or names a system this build does not know.
    #[must_use]
    pub fn peek(start: &[u8]) -> Option<Self> {
        let start = start.get(..Self::PEEK_LEN)?;
        if &start[..8] != MAGIC {
            return None;
        }
        Some(Self {
            system: System::from_tag(start[10])?,
            state_version: u16::from_le_bytes([start[12], start[13]]),
            rom_crc32: u32::from_le_bytes([start[16], start[17], start[18], start[19]]),
        })
    }

    /// Checks that a state with this header goes with the game being
    /// played, for which a state saved now would have the header
    /// `playing`: the same system, the same ROM, and the state version
    /// the core reads. A core's state is not self-describing, so one of
    /// another version would be refused as malformed at best, and at
    /// worst read as a machine it never was.
    ///
    /// # Errors
    ///
    /// Returns the first [`Mismatch`] of the three, in that order.
    pub fn check(&self, playing: &Self) -> Result<(), Mismatch> {
        if self.system != playing.system {
            return Err(Mismatch::System {
                found: self.system,
                playing: playing.system,
            });
        }
        if self.rom_crc32 != playing.rom_crc32 {
            return Err(Mismatch::Rom {
                found: self.rom_crc32,
                playing: playing.rom_crc32,
            });
        }
        if self.state_version != playing.state_version {
            return Err(Mismatch::StateVersion {
                found: self.state_version,
                reads: playing.state_version,
            });
        }
        Ok(())
    }
}

/// Why a save state does not go with the game being played. Each says so
/// in words that follow "could not load the state:".
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Mismatch {
    /// The state is another system's.
    #[error("it is a {found} state, not a {playing} one")]
    System {
        /// The system that wrote the state.
        found: System,
        /// The system being played.
        playing: System,
    },
    /// The state was saved while another ROM was running.
    #[error("it was saved with another ROM (CRC-32 {found:08x}, not {playing:08x})")]
    Rom {
        /// CRC-32 of the ROM the state was saved with.
        found: u32,
        /// CRC-32 of the ROM being played.
        playing: u32,
    },
    /// The state was saved by a build whose core lays its state out
    /// another way.
    #[error("it is state version {found}, and this build reads version {reads}")]
    StateVersion {
        /// The state version in the header.
        found: u16,
        /// The state version the running core reads and writes.
        reads: u16,
    },
}

/// A small picture of the screen when the state was saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thumbnail<'a> {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// `width * height` pixels as R, G, B bytes, row-major.
    pub rgb: &'a [u8],
}

/// A decoded container, borrowing from the bytes it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded<'a> {
    /// What the state belongs to.
    pub header: Header,
    /// The core's state bytes.
    pub payload: &'a [u8],
    /// The screenshot, if one was saved.
    pub thumbnail: Option<Thumbnail<'a>>,
}

/// Wraps `payload` and an optional `thumbnail` in a container.
///
/// # Panics
///
/// Panics if `payload` is 4 GiB or larger, which no supported system
/// approaches, or if the thumbnail's RGB data does not match its size.
#[must_use]
pub fn encode(header: &Header, payload: &[u8], thumbnail: Option<Thumbnail<'_>>) -> Vec<u8> {
    let payload_len = u32::try_from(payload.len()).expect("save state payload under 4 GiB");
    let (width, height, rgb) = thumbnail.map_or((0, 0, &[][..]), |t| (t.width, t.height, t.rgb));
    assert_eq!(
        rgb.len(),
        usize::from(width) * usize::from(height) * 3,
        "thumbnail size"
    );
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + rgb.len() + CHECKSUM_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
    out.push(header.system.tag());
    out.push(0);
    out.extend_from_slice(&header.state_version.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&header.rom_crc32.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(rgb);
    let checksum = crc32fast::hash(&out);
    out.extend_from_slice(&checksum.to_le_bytes());
    out
}

/// Splits a container into its header, the core's payload and the
/// thumbnail.
///
/// # Errors
///
/// Returns [`DecodeError`] if the bytes are not a well-formed container.
pub fn decode(bytes: &[u8]) -> Result<Decoded<'_>, DecodeError> {
    let fixed = bytes.get(..V1_HEADER_LEN).ok_or(DecodeError::Truncated)?;
    if &fixed[0..8] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let le16 = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    let le32 = |offset: usize| {
        u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])
    };
    let version = le16(8);
    let header_len = match version {
        1 => V1_HEADER_LEN,
        2 => HEADER_LEN,
        _ => return Err(DecodeError::UnsupportedVersion(version)),
    };
    if bytes.len() < header_len + CHECKSUM_LEN {
        return Err(DecodeError::Truncated);
    }
    let system = System::from_tag(bytes[10]).ok_or(DecodeError::UnknownSystem(bytes[10]))?;
    let header = Header {
        system,
        state_version: le16(12),
        rom_crc32: le32(16),
    };
    let payload_len = usize::try_from(le32(20)).map_err(|_| DecodeError::Truncated)?;
    let (width, height) = if version >= 2 {
        (le16(24), le16(26))
    } else {
        (0, 0)
    };
    let thumbnail_len = usize::from(width) * usize::from(height) * 3;

    let body_end = header_len + payload_len + thumbnail_len;
    if bytes.len() != body_end + CHECKSUM_LEN {
        return Err(DecodeError::Truncated);
    }
    if crc32fast::hash(&bytes[..body_end]) != le32(body_end) {
        return Err(DecodeError::ChecksumMismatch);
    }
    let payload = &bytes[header_len..header_len + payload_len];
    let thumbnail = (thumbnail_len > 0).then(|| Thumbnail {
        width,
        height,
        rgb: &bytes[header_len + payload_len..body_end],
    });
    Ok(Decoded {
        header,
        payload,
        thumbnail,
    })
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
    fn round_trips_header_payload_and_thumbnail() {
        let rgb = [1, 2, 3, 4, 5, 6];
        let thumbnail = Thumbnail {
            width: 2,
            height: 1,
            rgb: &rgb,
        };
        let bytes = encode(&HEADER, b"machine state", Some(thumbnail));
        let decoded = decode(&bytes).expect("valid container");
        assert_eq!(decoded.header, HEADER);
        assert_eq!(decoded.payload, b"machine state");
        assert_eq!(decoded.thumbnail, Some(thumbnail));
    }

    #[test]
    fn a_header_is_checked_against_the_game_being_played() {
        assert_eq!(HEADER.check(&HEADER), Ok(()));
        let other_system = Header {
            system: System::Nds,
            ..HEADER
        };
        let other_rom = Header {
            rom_crc32: 0x1234_5678,
            ..HEADER
        };
        let other_version = Header {
            state_version: 4,
            ..HEADER
        };
        let mismatch = |state: &Header| state.check(&HEADER).map_err(|why| why.to_string());
        assert_eq!(
            mismatch(&other_system),
            Err("it is a nds state, not a gba one".to_owned())
        );
        assert_eq!(
            mismatch(&other_rom),
            Err("it was saved with another ROM (CRC-32 12345678, not deadbeef)".to_owned())
        );
        assert_eq!(
            mismatch(&other_version),
            Err("it is state version 4, and this build reads version 3".to_owned())
        );
        // The first of them is the one reported: the system, then the ROM.
        let all = Header {
            system: System::Gb,
            state_version: 9,
            rom_crc32: 1,
        };
        assert!(matches!(all.check(&HEADER), Err(Mismatch::System { .. })));
        let rom_and_version = Header {
            system: System::Gba,
            ..all
        };
        assert!(matches!(
            rom_and_version.check(&HEADER),
            Err(Mismatch::Rom { .. })
        ));
    }

    #[test]
    fn a_header_is_peeked_from_the_start_of_a_container() {
        for system in System::ALL {
            let header = Header { system, ..HEADER };
            let bytes = encode(&header, b"machine state", None);
            assert_eq!(Header::peek(&bytes), Some(header));
            // The start is enough, whatever follows it.
            assert_eq!(Header::peek(&bytes[..Header::PEEK_LEN]), Some(header));
            assert_eq!(Header::peek(&bytes[..Header::PEEK_LEN - 1]), None);
        }
        // The same fields `decode` reads, of a version 1 container too.
        let mut old = Vec::new();
        old.extend_from_slice(MAGIC);
        old.extend_from_slice(&1u16.to_le_bytes());
        old.extend_from_slice(&[System::Gb.tag(), 0, 7, 0, 0, 0]);
        old.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        assert_eq!(
            Header::peek(&old),
            Some(Header {
                system: System::Gb,
                state_version: 7,
                rom_crc32: 0x1234_5678,
            })
        );
        let mut bytes = encode(&HEADER, b"machine state", None);
        bytes[0] ^= 1;
        assert_eq!(Header::peek(&bytes), None, "not a save state");
        let mut bytes = encode(&HEADER, b"machine state", None);
        bytes[10] = 0xEE;
        assert_eq!(Header::peek(&bytes), None, "an unknown system");
    }

    #[test]
    fn round_trips_without_a_thumbnail() {
        let bytes = encode(&HEADER, &[], None);
        let decoded = decode(&bytes).expect("valid container");
        assert_eq!((decoded.payload, decoded.thumbnail), (&[][..], None));
    }

    #[test]
    fn reads_version_1_containers() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&[System::Gb.tag(), 0, 7, 0, 0, 0]);
        bytes.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(b"hi");
        let checksum = crc32fast::hash(&bytes);
        bytes.extend_from_slice(&checksum.to_le_bytes());

        let decoded = decode(&bytes).expect("valid v1 container");
        assert_eq!(decoded.header.state_version, 7);
        assert_eq!(decoded.payload, b"hi");
        assert_eq!(decoded.thumbnail, None);
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
        let bytes = encode(&HEADER, b"machine state", None);
        assert_eq!(
            decode(&bytes[..bytes.len() - 1]),
            Err(DecodeError::Truncated)
        );

        let mut corrupted = bytes;
        corrupted[HEADER_LEN] ^= 1;
        assert_eq!(decode(&corrupted), Err(DecodeError::ChecksumMismatch));
    }
}
