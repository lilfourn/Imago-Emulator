//! ROM-hack patches.
//!
//! A ROM hack is shared as a patch: the bytes that turn the ROM it was
//! made from, the *source*, into the hacked game, the *target*, so the
//! game itself is never passed around. [`apply`] makes the target from a
//! patch and its source; [`Format::of`] tells the formats apart by the
//! bytes they start with.
//!
//! | Format | Starts with | Checks | Spec |
//! |---|---|---|---|
//! | IPS | `PATCH` | none | Lunar IPS's truncation extension |
//! | UPS | `UPS1` | CRC-32 of source, target and patch | byuu's UPS |
//! | BPS | `BPS1` | CRC-32 of source, target and patch | `bps_spec.md` of beat and Flips |
//! | xdelta | `D6 C3 C4 00` | Adler-32 of each window | RFC 3284, with xdelta3's additions |
//!
//! UPS and BPS name their source by checksum ([`source_crc32`]), so a
//! frontend can find it among the user's games, and both check the
//! result. An xdelta patch checks its result only. An IPS patch checks
//! nothing: applied to the wrong ROM, it makes a broken game.
//!
//! Knows formats, not files: patches, sources and targets are bytes.

mod beat;
mod ips;
mod reader;
mod vcdiff;

/// The formats a patch can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// International Patching System, the oldest: bytes to write at
    /// offsets up to 16 MiB.
    Ips,
    /// Universal Patching System: the target as the source XORed with
    /// the patch.
    Ups,
    /// Binary Patching System, UPS's successor: the target copied from
    /// the source, the patch and itself.
    Bps,
    /// VCDIFF, as xdelta3 writes it; usual for DS hacks, whose ROMs are
    /// too big for IPS.
    Vcdiff,
}

impl Format {
    /// The format of `patch`, by the bytes it starts with.
    #[must_use]
    pub fn of(patch: &[u8]) -> Option<Self> {
        if patch.starts_with(b"PATCH") {
            Some(Self::Ips)
        } else if patch.starts_with(b"UPS1") {
            Some(Self::Ups)
        } else if patch.starts_with(b"BPS1") {
            Some(Self::Bps)
        } else if patch.starts_with(&vcdiff::MAGIC) {
            Some(Self::Vcdiff)
        } else {
            None
        }
    }

    /// The format's usual name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ips => "IPS",
            Self::Ups => "UPS",
            Self::Bps => "BPS",
            Self::Vcdiff => "xdelta",
        }
    }
}

/// Why a patch cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    /// The bytes are not a patch in any of the formats.
    #[error("it is not an IPS, UPS, BPS or xdelta patch")]
    UnknownFormat,
    /// The patch is cut short, fails its own checksum, or says something
    /// impossible.
    #[error("the patch is damaged")]
    Damaged,
    /// The patch names another ROM as its source.
    #[error("the patch is for a different ROM: one with checksum {expected:08X}, not {actual:08X}")]
    WrongSource {
        /// The CRC-32 of the ROM the patch was made from.
        expected: u32,
        /// The CRC-32 of the ROM it was given.
        actual: u32,
    },
    /// The ROM given is the patch's target: the hacked game already.
    #[error("the game is already patched")]
    AlreadyPatched,
    /// The result is not what the patch made when it was created, so the
    /// ROM given is not the one it was made from.
    #[error("the patched game came out wrong, so the patch was made from a different ROM")]
    WrongResult,
    /// The result would be bigger than the limit [`apply`] was given.
    #[error("the patched game would be bigger than {limit} bytes")]
    TooBig {
        /// The limit.
        limit: usize,
    },
    /// The patch uses a part of its format this crate does not read.
    #[error("the patch uses {0}, which is not supported")]
    Unsupported(&'static str),
}

/// Makes the target of `patch` from `source`, the ROM it was made from,
/// and checks it as far as the format allows (see the table above).
///
/// # Errors
///
/// Fails if `patch` is not a patch, is damaged or is for another ROM, or
/// if the target would be bigger than `limit` bytes.
pub fn apply(patch: &[u8], source: &[u8], limit: usize) -> Result<Vec<u8>, PatchError> {
    match Format::of(patch).ok_or(PatchError::UnknownFormat)? {
        Format::Ips => ips::apply(patch, source, limit),
        Format::Ups => beat::apply_ups(patch, source, limit),
        Format::Bps => beat::apply_bps(patch, source, limit),
        Format::Vcdiff => vcdiff::apply(patch, source, limit),
    }
}

/// The CRC-32 of the ROM `patch` was made from, for the formats that
/// record it: UPS and BPS.
#[must_use]
pub fn source_crc32(patch: &[u8]) -> Option<u32> {
    match Format::of(patch)? {
        Format::Ups | Format::Bps => beat::source_crc32(patch),
        Format::Ips | Format::Vcdiff => None,
    }
}
