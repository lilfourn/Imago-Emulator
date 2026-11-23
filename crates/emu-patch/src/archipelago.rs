//! Archipelago patches: `.apemerald`, `.apred`, `.apblue` and their like.
//!
//! [Archipelago](https://archipelago.gg) randomizes where a game's items
//! are, across one player's game or many players' games, and hands each
//! player a patch for their own ROM. The patch is a zip, as
//! `worlds/Files.py` of Archipelago 0.6 writes it:
//!
//! - `archipelago.json`, the manifest: the MD5 of the ROM the patch is for
//!   (`base_checksum`), the version of the format a reader must know
//!   (`compatible_version`), and the `procedure`, a list of steps, each a
//!   name and the files it takes;
//! - the files the steps name.
//!
//! Two steps are read here, which are all that Pokémon Emerald's and
//! Pokémon Red and Blue's patches use:
//!
//! - `apply_bsdiff4`: a bsdiff 4.0 patch, three bzip2 streams (the
//!   control triples, the bytes added to the source's, and the new bytes);
//! - `apply_tokens`: a list of small edits made after it, each a kind, an
//!   offset and its data: bytes written, a range copied or filled, or one
//!   byte ANDed, ORed or XORed.
//!
//! A manifest with no procedure is a patch from before there were any:
//! one bsdiff, `delta.bsdiff4`. A game's own steps, which some games add,
//! are refused by name.
//!
//! The patched game plays on its own: what the player finds for
//! themselves is in the ROM. Items found for other players, and theirs
//! for this one, go through Archipelago's client and server, which this
//! does not do.

use std::io::{Cursor, Read};

use md5::{Digest, Md5};
use serde_json::Value;
use zip::ZipArchive;

use crate::PatchError;

/// What a zip starts with.
pub(crate) const MAGIC: [u8; 4] = *b"PK\x03\x04";

/// The newest format this reads: the `container_version` of Archipelago
/// 0.6's `worlds/Files.py`.
const VERSION: u64 = 7;

/// The manifest's name in the zip.
const MANIFEST: &str = "archipelago.json";

/// The procedure of a patch whose manifest names none.
const DELTA: &str = "delta.bsdiff4";

/// A step of a patch's procedure: its name and the files it takes.
type Step = (String, Vec<String>);

/// A patch, opened: its zip and what its manifest says.
struct Patch<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    /// The MD5 of the ROM the patch is for, in lowercase hex.
    base: Option<String>,
    /// What the patched game's file name ends with, as `.gba`.
    ending: Option<String>,
    procedure: Vec<Step>,
}

impl<'a> Patch<'a> {
    /// Opens `patch`, if it is a zip with an Archipelago patch's manifest.
    fn open(patch: &'a [u8]) -> Result<Self, PatchError> {
        let mut zip = ZipArchive::new(Cursor::new(patch)).map_err(|_| PatchError::Damaged)?;
        // A megabyte is far more than any manifest.
        let manifest = read(&mut zip, MANIFEST, 1 << 20).map_err(|_| PatchError::UnknownFormat)?;
        let manifest: Value = serde_json::from_slice(&manifest).map_err(|_| PatchError::Damaged)?;
        let needs = manifest
            .get("compatible_version")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if needs > VERSION {
            return Err(PatchError::Unsupported(
                "a newer Archipelago's patch format",
            ));
        }
        let text = |key: &str| {
            manifest
                .get(key)
                .and_then(Value::as_str)
                .map(|text| text.to_ascii_lowercase())
        };
        let procedure = match manifest.get("procedure") {
            None => vec![("apply_bsdiff4".to_owned(), vec![DELTA.to_owned()])],
            Some(Value::Array(steps)) => steps
                .iter()
                .map(step)
                .collect::<Option<Vec<Step>>>()
                .ok_or(PatchError::Damaged)?,
            // "custom": the game's own code makes the ROM.
            Some(_) => return Err(PatchError::Unsupported("a procedure of the game's own")),
        };
        Ok(Self {
            zip,
            base: text("base_checksum"),
            ending: text("result_file_ending"),
            procedure,
        })
    }
}

/// A step as the manifest writes it: `["apply_bsdiff4", ["base.bsdiff4"]]`.
fn step(step: &Value) -> Option<Step> {
    let [name, files] = step.as_array()?.as_slice() else {
        return None;
    };
    let files = files.as_array()?.iter();
    let files = files.map(|file| file.as_str().map(str::to_owned));
    Some((name.as_str()?.to_owned(), files.collect::<Option<_>>()?))
}

/// The file called `name` in `zip`, if it is at most `limit` bytes.
fn read(
    zip: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, PatchError> {
    let file = zip.by_name(name).map_err(|_| PatchError::Damaged)?;
    read_capped(file, limit)
}

/// All `from` holds, if that is at most `limit` bytes.
fn read_capped(from: impl Read, limit: usize) -> Result<Vec<u8>, PatchError> {
    let mut bytes = Vec::new();
    let most = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    from.take(most)
        .read_to_end(&mut bytes)
        .map_err(|_| PatchError::Damaged)?;
    if bytes.len() > limit {
        return Err(PatchError::TooBig { limit });
    }
    Ok(bytes)
}

/// The MD5 of `bytes`, in lowercase hex, as a manifest names a ROM.
pub(crate) fn md5_hex(bytes: &[u8]) -> String {
    Md5::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The MD5 of the ROM `patch` is for, in lowercase hex, if it names one.
pub(crate) fn source_md5(patch: &[u8]) -> Option<String> {
    Patch::open(patch).ok()?.base
}

/// The extension of the game `patch` makes, without its dot, if it says.
pub(crate) fn target_extension(patch: &[u8]) -> Option<String> {
    let ending = Patch::open(patch).ok()?.ending?;
    Some(ending.trim_start_matches('.').to_owned()).filter(|extension| !extension.is_empty())
}

/// Makes the game `patch` makes from `source`, a step at a time.
pub(crate) fn apply(patch: &[u8], source: &[u8], limit: usize) -> Result<Vec<u8>, PatchError> {
    let mut patch = Patch::open(patch)?;
    if let Some(expected) = &patch.base {
        let actual = md5_hex(source);
        if *expected != actual {
            return Err(PatchError::WrongRom {
                expected: expected.clone(),
                actual,
            });
        }
    }
    let mut rom = source.to_vec();
    for (name, files) in patch.procedure.clone() {
        let [file] = files.as_slice() else {
            return Err(PatchError::Damaged);
        };
        // A step's file is no bigger than the game it makes.
        let data = read(&mut patch.zip, file, limit)?;
        rom = match name.as_str() {
            "apply_bsdiff4" => bsdiff4(&data, &rom, limit)?,
            "apply_tokens" => tokens(&data, rom, limit)?,
            _ => return Err(PatchError::Unsupported("a step of the game's own")),
        };
    }
    Ok(rom)
}

/// A number as bsdiff writes one: eight bytes, the lowest first, with the
/// top bit of the last for its sign.
fn offset(bytes: &[u8]) -> Option<i64> {
    let bytes: [u8; 8] = bytes.get(..8)?.try_into().ok()?;
    let magnitude = i64::from_le_bytes(bytes) & i64::MAX;
    Some(if bytes[7] & 0x80 == 0 {
        magnitude
    } else {
        -magnitude
    })
}

/// A length as bsdiff writes one, which is never negative.
fn length(bytes: &[u8]) -> Result<usize, PatchError> {
    offset(bytes)
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(PatchError::Damaged)
}

/// The bzip2 stream `packed`, unpacked, if that is at most `limit` bytes.
fn unpack(packed: &[u8], limit: usize) -> Result<Vec<u8>, PatchError> {
    read_capped(bzip2_rs::DecoderReader::new(packed), limit)
}

/// Applies the bsdiff 4.0 patch `patch` to `source`: `BSDIFF40`, three
/// lengths (the packed control block's, the packed diff block's, and the
/// target's), then the control, diff and extra blocks, each a bzip2
/// stream. Each control triple adds so many of the diff's bytes to the
/// source's, then takes so many of the extra's as they are, then moves on
/// or back in the source.
fn bsdiff4(patch: &[u8], source: &[u8], limit: usize) -> Result<Vec<u8>, PatchError> {
    let damaged = PatchError::Damaged;
    let header = patch.get(..32).ok_or(damaged.clone())?;
    if !header.starts_with(b"BSDIFF40") {
        return Err(damaged);
    }
    let (control_length, diff_length) = (length(&header[8..])?, length(&header[16..])?);
    let target_length = length(&header[24..])?;
    if target_length > limit {
        return Err(PatchError::TooBig { limit });
    }
    let blocks = &patch[32..];
    let diff_end = control_length
        .checked_add(diff_length)
        .filter(|end| *end <= blocks.len())
        .ok_or(damaged.clone())?;
    // A triple makes at least one byte, or it would not be written.
    let control = unpack(
        &blocks[..control_length],
        target_length.saturating_add(1) * 24,
    )?;
    let diff = unpack(&blocks[control_length..diff_end], target_length)?;
    let extra = unpack(&blocks[diff_end..], target_length)?;

    let mut target = Vec::with_capacity(target_length);
    let (mut diff, mut extra) = (diff.as_slice(), extra.as_slice());
    let mut at: i64 = 0;
    for triple in control.chunks_exact(24) {
        let (add, take) = (length(triple)?, length(&triple[8..])?);
        let seek = offset(&triple[16..]).ok_or(damaged.clone())?;
        if add > diff.len() || take > extra.len() || target.len() + add + take > target_length {
            return Err(damaged);
        }
        let (added, rest) = diff.split_at(add);
        for (index, byte) in added.iter().enumerate() {
            // Past either end of the source there is nothing to add to.
            let from = i64::try_from(index)
                .ok()
                .and_then(|index| at.checked_add(index))
                .and_then(|from| usize::try_from(from).ok())
                .and_then(|from| source.get(from));
            target.push(byte.wrapping_add(from.copied().unwrap_or(0)));
        }
        diff = rest;
        let (taken, rest) = extra.split_at(take);
        target.extend_from_slice(taken);
        extra = rest;
        at = i64::try_from(add)
            .ok()
            .and_then(|add| at.checked_add(add))
            .and_then(|at| at.checked_add(seek))
            .ok_or(damaged.clone())?;
    }
    if target.len() != target_length || control.len() % 24 != 0 {
        return Err(damaged);
    }
    Ok(target)
}

/// Makes the edits in `data` to `rom`: a count, then for each its kind,
/// where it goes, its data's length and its data, numbers four bytes, the
/// lowest first. An edit may write past the end of the game, which grows
/// to hold it, but none starts there.
fn tokens(data: &[u8], mut rom: Vec<u8>, limit: usize) -> Result<Vec<u8>, PatchError> {
    let damaged = PatchError::Damaged;
    let number = |at: usize| -> Result<usize, PatchError> {
        let bytes = data.get(at..at.saturating_add(4)).ok_or(damaged.clone())?;
        let bytes: [u8; 4] = bytes.try_into().map_err(|_| damaged.clone())?;
        usize::try_from(u32::from_le_bytes(bytes)).map_err(|_| damaged.clone())
    };
    let mut at = 4;
    for _ in 0..number(0)? {
        let kind = *data.get(at).ok_or(damaged.clone())?;
        let (to, size) = (number(at + 1)?, number(at + 5)?);
        let body = data
            .get(at + 9..(at + 9).saturating_add(size))
            .ok_or(damaged.clone())?;
        at += 9 + size;
        if to > rom.len() {
            return Err(damaged);
        }
        match kind {
            // Written as they are.
            0 => write(&mut rom, to, body, limit)?,
            // A range of the game copied, or one byte repeated.
            1 | 2 => {
                let [length, value] = [0, 4].map(|at| {
                    let bytes: Option<[u8; 4]> =
                        body.get(at..at + 4).and_then(|b| b.try_into().ok());
                    bytes.map(u32::from_le_bytes)
                });
                let (Some(length), Some(value)) = (length, value) else {
                    return Err(damaged);
                };
                let length = usize::try_from(length).map_err(|_| damaged.clone())?;
                let filled = if kind == 1 {
                    let from = usize::try_from(value).map_err(|_| damaged.clone())?;
                    let range = from..from.checked_add(length).ok_or(damaged.clone())?;
                    rom.get(range).ok_or(damaged.clone())?.to_vec()
                } else {
                    vec![u8::try_from(value).map_err(|_| damaged.clone())?; length]
                };
                write(&mut rom, to, &filled, limit)?;
            }
            // One byte changed in place.
            3..=5 => {
                let (Some(byte), Some(with)) = (rom.get_mut(to), body.first()) else {
                    return Err(damaged);
                };
                match kind {
                    3 => *byte &= with,
                    4 => *byte |= with,
                    _ => *byte ^= with,
                }
            }
            _ => return Err(PatchError::Unsupported("an edit this app does not know")),
        }
    }
    Ok(rom)
}

/// Writes `bytes` into `rom` at `to`, which is within it or at its end;
/// the game grows to hold what goes past its end, up to `limit` bytes.
fn write(rom: &mut Vec<u8>, to: usize, bytes: &[u8], limit: usize) -> Result<(), PatchError> {
    let end = to.checked_add(bytes.len()).ok_or(PatchError::Damaged)?;
    if end > limit {
        return Err(PatchError::TooBig { limit });
    }
    if end > rom.len() {
        rom.resize(end, 0);
    }
    rom[to..end].copy_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One edit, as `apply_tokens` reads it.
    fn token(kind: u8, to: u32, body: &[u8]) -> Vec<u8> {
        let mut token = vec![kind];
        token.extend(to.to_le_bytes());
        token.extend(u32::try_from(body.len()).expect("short").to_le_bytes());
        token.extend(body);
        token
    }

    /// A token file of `tokens`.
    fn token_file(tokens: &[Vec<u8>]) -> Vec<u8> {
        let mut file = u32::try_from(tokens.len())
            .expect("few")
            .to_le_bytes()
            .to_vec();
        file.extend(tokens.concat());
        file
    }

    /// A copy or a fill's data: how long, and from where or with what.
    fn range(length: u32, value: u32) -> Vec<u8> {
        [length.to_le_bytes(), value.to_le_bytes()].concat()
    }

    #[test]
    fn each_kind_of_edit_is_made() {
        let rom = vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80];
        let edits = token_file(&[
            token(0, 1, &[0xAA, 0xBB]),
            token(1, 4, &range(2, 0)),
            token(2, 6, &range(1, 0xEE)),
            token(3, 0, &[0x0F]),
            token(4, 7, &[0x01]),
            token(5, 3, &[0xFF]),
            // Past the end: the game grows.
            token(0, 8, &[1, 2]),
        ]);
        assert_eq!(
            tokens(&edits, rom.clone(), 64),
            Ok(vec![0x00, 0xAA, 0xBB, 0xBF, 0x10, 0xAA, 0xEE, 0x81, 1, 2])
        );
        assert_eq!(tokens(&token_file(&[]), rom.clone(), 64), Ok(rom.clone()));
        // Cut short, a kind unknown, an edit that starts past the end, a
        // copy from past it, and a game grown past the limit.
        let damaged = Err(PatchError::Damaged);
        assert_eq!(tokens(&edits[..edits.len() - 1], rom.clone(), 64), damaged);
        assert_eq!(tokens(&[1, 0, 0], rom.clone(), 64), damaged);
        assert_eq!(
            tokens(&token_file(&[token(9, 0, &[])]), rom.clone(), 64),
            Err(PatchError::Unsupported("an edit this app does not know"))
        );
        let past = token_file(&[token(0, 9, &[1])]);
        assert_eq!(tokens(&past, rom.clone(), 64), damaged);
        let from_past = token_file(&[token(1, 0, &range(4, 6))]);
        assert_eq!(tokens(&from_past, rom.clone(), 64), damaged);
        let grown = token_file(&[token(2, 8, &range(100, 0))]);
        assert_eq!(
            tokens(&grown, rom, 64),
            Err(PatchError::TooBig { limit: 64 })
        );
    }

    #[test]
    fn bsdiff_numbers_carry_their_sign_in_the_top_bit() {
        assert_eq!(offset(&[5, 0, 0, 0, 0, 0, 0, 0]), Some(5));
        assert_eq!(offset(&[5, 0, 0, 0, 0, 0, 0, 0x80]), Some(-5));
        assert_eq!(offset(&[0, 1, 0, 0, 0, 0, 0, 0]), Some(256));
        assert_eq!(offset(&[1, 2, 3]), None);
        assert_eq!(
            length(&[5, 0, 0, 0, 0, 0, 0, 0x80]),
            Err(PatchError::Damaged)
        );
    }

    #[test]
    fn what_is_not_a_bsdiff_patch_is_damaged() {
        assert_eq!(bsdiff4(b"BSDIFF40", b"rom", 64), Err(PatchError::Damaged));
        let mut header = b"BSDIFF41".to_vec();
        header.resize(32, 0);
        assert_eq!(bsdiff4(&header, b"rom", 64), Err(PatchError::Damaged));
        // A target bigger than a game may be.
        let mut big = b"BSDIFF40".to_vec();
        big.resize(24, 0);
        big.extend(1_000_u64.to_le_bytes());
        assert_eq!(
            bsdiff4(&big, b"rom", 64),
            Err(PatchError::TooBig { limit: 64 })
        );
    }
}
