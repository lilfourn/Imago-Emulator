//! Games made from ROM hacks. A hack comes as a patch for the game it was
//! made from (see `emu_patch`). Applying it makes a new ROM, written into
//! the app's own hacks folder and named after the patch, so the hack has
//! its own saves and its own place in the library, and the original ROM
//! is left as it is.
//!
//! UPS and BPS patches name their game by checksum, so it can be found
//! among the library's games. IPS and xdelta patches don't, so the user
//! chooses it.
//!
//! An Archipelago patch is made into a game the same way: the patch
//! [Archipelago](https://archipelago.gg) hands a player for their own
//! ROM, with the game's items moved about. It names its game by MD5,
//! which is looked for among the library's games of its kind. The game
//! plays on its own; items for and from other players' games need
//! Archipelago's client, which the app is not.
//!
//! Everything here blocks; run it off the UI thread.

use std::fmt::Display;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use crate::archive;
use crate::persist::file_name;

/// The patches the app makes games from, by extension: ROM hacks', then
/// Archipelago's for the Pokémon games it has worlds for (Emerald, Red
/// and Blue, and the community's FireRed, LeafGreen and Crystal).
const PATCHES: [&str; 11] = [
    "ips",
    "ups",
    "bps",
    "xdelta",
    "vcdiff",
    "apemerald",
    "apred",
    "apblue",
    "apfirered",
    "apleafgreen",
    "apcrystal",
];

/// Where games made from patches are kept, e.g.
/// `~/Library/Application Support/ds-emulator/Hacks` on macOS.
pub fn hacks_dir() -> Option<PathBuf> {
    Some(emu_os::data_dir()?.join("ds-emulator").join("Hacks"))
}

/// Whether `path` is a ROM-hack patch, judged by its extension.
pub fn is_patch(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            PATCHES
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Whether the game at `path` was made from a patch: whether it is in the
/// hacks folder.
pub fn is_hack(path: &Path) -> bool {
    hacks_dir().is_some_and(|folder| path.starts_with(folder))
}

/// Why a patch's game was not made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MakeError {
    /// The patch at this path names no game, or none the library has, so
    /// the user must choose it.
    NoBase(PathBuf),
    /// What went wrong, to show.
    Failed(String),
}

/// Makes the game that the patch at `patch` makes from the ROM at `base`,
/// or if none is given, from the game among `known` (checksum and path)
/// whose checksum the patch names. Returns where it is.
///
/// # Errors
///
/// [`MakeError::NoBase`] if no `base` is given and none is found, or
/// [`MakeError::Failed`] if a file cannot be read or written, or the
/// patch cannot be applied to its base.
pub fn make(
    patch: &Path,
    base: Option<&Path>,
    known: &[(u32, PathBuf)],
) -> Result<PathBuf, MakeError> {
    let folder = hacks_dir().ok_or_else(|| {
        MakeError::Failed("There is no folder to keep patched games in".to_owned())
    })?;
    make_into(patch, base, known, &folder)
}

fn make_into(
    patch: &Path,
    base: Option<&Path>,
    known: &[(u32, PathBuf)],
    folder: &Path,
) -> Result<PathBuf, MakeError> {
    let patch_name = file_name(patch);
    let bytes = read_capped(patch, "it is too big to be a patch")
        .map_err(|error| MakeError::Failed(format!("Cannot read {}: {error}", patch.display())))?;
    let base = match base {
        Some(base) => base.to_owned(),
        None => source_of(&bytes, known).ok_or_else(|| MakeError::NoBase(patch.to_owned()))?,
    };
    let failed = |error: &dyn Display| {
        MakeError::Failed(format!(
            "Cannot patch {} with {patch_name}: {error}",
            file_name(&base)
        ))
    };
    if emu_3ds::is_game(&base) {
        return Err(failed(&"3DS games take no patches"));
    }
    let Some(extension) = base
        .extension()
        .filter(|_| emu_cores::system_for_path(&base).is_some())
    else {
        return Err(failed(&"it is not a .gb, .gbc, .gba or .nds game"));
    };
    let source =
        read_capped(&base, "it is too big to be a game").map_err(|error| failed(&error))?;
    let rom = emu_patch::apply(&bytes, &source, archive::MAX_ROM as usize)
        .map_err(|error| failed(&error))?;
    let name = format!("{}.{}", stem(patch), extension.to_string_lossy());
    archive::store(
        folder,
        Path::new(&name),
        crc32fast::hash(&rom),
        rom.as_slice(),
    )
    .map_err(|error| failed(&error))
}

/// The game among `known` (checksum and path) that the patch `patch` was
/// made from, if it names one: by CRC-32, as UPS and BPS do, or by MD5, as
/// an Archipelago patch does. The library keeps no MD5, so each game of
/// the kind the patch makes is read for its own.
fn source_of(patch: &[u8], known: &[(u32, PathBuf)]) -> Option<PathBuf> {
    if let Some(crc32) = emu_patch::source_crc32(patch) {
        let named = |(known, path): &&(u32, PathBuf)| *known == crc32 && path.is_file();
        return known.iter().find(named).map(|(_, path)| path.clone());
    }
    let md5 = emu_patch::source_md5(patch)?;
    let makes = emu_patch::target_extension(patch);
    known
        .iter()
        .map(|(_, path)| path)
        .filter(|path| same_kind(path, makes.as_deref()))
        .find(|path| {
            read_capped(path, "it is too big to be a game")
                .is_ok_and(|rom| emu_patch::md5(&rom) == md5)
        })
        .cloned()
}

/// Whether the game at `path` is of the kind a patch makes, which is of
/// the kind it is for: a cartridge game with the extension `makes`, where
/// a Game Boy game may be named `.gb` or `.gbc`. Any cartridge game, if
/// the patch does not say.
fn same_kind(path: &Path, makes: Option<&str>) -> bool {
    if emu_3ds::is_game(path) || emu_cores::system_for_path(path).is_none() {
        return false;
    }
    let game_boy = |extension: &str| ["gb", "gbc"].contains(&extension);
    let extension = path.extension().and_then(|extension| extension.to_str());
    match (extension.map(str::to_ascii_lowercase), makes) {
        (_, None) => true,
        (Some(extension), Some(makes)) => {
            extension == makes || (game_boy(&extension) && game_boy(makes))
        }
        (None, Some(_)) => false,
    }
}

/// All of the file at `path`, a patch or the game it patches, unless it is
/// bigger than a cartridge holds ([`archive::MAX_ROM`]), which neither is:
/// then it fails with `too_big`, and nothing of it is read into memory.
/// Archives hold patches to the same size.
fn read_capped(path: &Path, too_big: &str) -> io::Result<Vec<u8>> {
    if fs::metadata(path)?.len() > archive::MAX_ROM {
        return Err(io::Error::other(too_big));
    }
    // Held to it as it is read too: what is not a plain file has no size
    // to ask for.
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(archive::MAX_ROM + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > archive::MAX_ROM {
        return Err(io::Error::other(too_big));
    }
    Ok(bytes)
}

/// What to open from the files the user opened together, each path's
/// own: the files in an archive, or the path itself. That is the first
/// ROM and the first patch from each, and at most one patch, which then
/// patches the ROM with it, if there is one. Returns the ROMs and the
/// patch.
///
/// # Errors
///
/// Says what to do instead if there are two patches, or a patch and two
/// ROMs.
pub fn pair(files: Vec<Vec<PathBuf>>) -> Result<(Vec<PathBuf>, Option<PathBuf>), String> {
    let mut roms = Vec::new();
    let mut patches = Vec::new();
    for files in files {
        roms.extend(files.iter().find(|file| !is_patch(file)).cloned());
        patches.extend(files.into_iter().find(|file| is_patch(file)));
    }
    match patches.len() {
        0 => Ok((roms, None)),
        1 if roms.len() <= 1 => Ok((roms, patches.pop())),
        1 => Err("Open a patch with just the game it patches".to_owned()),
        _ => Err("Open one patch at a time".to_owned()),
    }
}

/// The patch's name without its extension, nor a game's extension before
/// that: `Radical Red` for `Radical Red.gba.bps`, but `Hack v1.2` for
/// `Hack v1.2.ips`.
fn stem(patch: &Path) -> String {
    let stem = Path::new(patch.file_stem().unwrap_or_default());
    let stem = if emu_cores::system_for_path(stem).is_some() {
        stem.file_stem().unwrap_or_default()
    } else {
        stem.as_os_str()
    };
    stem.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-app-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// An IPS patch that writes `bytes` at the start of the ROM.
    fn ips(bytes: &[u8]) -> Vec<u8> {
        let mut patch = b"PATCH\0\0\0\0".to_vec();
        patch.push(bytes.len() as u8);
        patch.extend_from_slice(bytes);
        patch.extend_from_slice(b"EOF");
        patch
    }

    /// A UPS patch that writes `bytes` at the start of `source`, which
    /// they must all differ from: UPS names its game by checksum.
    fn ups(source: &[u8], bytes: &[u8]) -> Vec<u8> {
        let mut target = source.to_vec();
        target[..bytes.len()].copy_from_slice(bytes);
        let mut patch = b"UPS1".to_vec();
        patch.extend_from_slice(&[0x80 | source.len() as u8, 0x80 | target.len() as u8, 0x80]);
        patch.extend(
            source
                .iter()
                .zip(&target)
                .take(bytes.len())
                .map(|(a, b)| a ^ b),
        );
        patch.push(0);
        patch.extend_from_slice(&crc32fast::hash(source).to_le_bytes());
        patch.extend_from_slice(&crc32fast::hash(&target).to_le_bytes());
        let crc = crc32fast::hash(&patch);
        patch.extend_from_slice(&crc.to_le_bytes());
        patch
    }

    #[test]
    fn recognises_patches_by_extension() {
        for name in ["a.ips", "a.UPS", "a.bps", "a.xdelta", "a.vcdiff"] {
            assert!(is_patch(Path::new(name)), "{name}");
        }
        // Archipelago's, for the Pokémon games.
        for name in ["AP_1_P1.apemerald", "a.apred", "a.APBLUE", "a.apcrystal"] {
            assert!(is_patch(Path::new(name)), "{name}");
        }
        // Not its worlds, nor a Mac app.
        assert!(!is_patch(Path::new("emerald.apworld")));
        assert!(!is_patch(Path::new("Game.app")));
        assert!(!is_patch(Path::new("a.gba")));
        assert!(!is_patch(Path::new("bps")));
    }

    #[test]
    fn pairs_a_patch_with_the_rom_opened_with_it() {
        let paths = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(PathBuf::from).collect() };
        // A ROM; two linked; a zip of two ROMs, which plays the first.
        assert_eq!(pair(vec![paths(&["a.gba"])]), Ok((paths(&["a.gba"]), None)));
        assert_eq!(
            pair(vec![paths(&["a.gb"]), paths(&["b.gb", "c.gb"])]),
            Ok((paths(&["a.gb", "b.gb"]), None))
        );
        // A patch alone, with its game, or from a zip that holds both.
        assert_eq!(
            pair(vec![paths(&["hack.bps"])]),
            Ok((vec![], Some(PathBuf::from("hack.bps"))))
        );
        assert_eq!(
            pair(vec![paths(&["a.gba"]), paths(&["hack.bps"])]),
            Ok((paths(&["a.gba"]), Some(PathBuf::from("hack.bps"))))
        );
        assert_eq!(
            pair(vec![paths(&["a.nds", "hack.xdelta"])]),
            Ok((paths(&["a.nds"]), Some(PathBuf::from("hack.xdelta"))))
        );
        assert!(pair(vec![paths(&["a.bps"]), paths(&["b.ips"])]).is_err());
        assert!(
            pair(vec![
                paths(&["a.bps"]),
                paths(&["a.gba"]),
                paths(&["b.gba"])
            ])
            .is_err()
        );
    }

    #[test]
    fn names_the_game_after_the_patch() {
        assert_eq!(stem(Path::new("/a/Radical Red.gba.bps")), "Radical Red");
        assert_eq!(stem(Path::new("/a/Hack v1.2.ips")), "Hack v1.2");
    }

    #[test]
    fn makes_the_game_from_the_rom_given() {
        let dir = temp_dir("romhack-given");
        let (hacks, base, patch) = (
            dir.join("Hacks"),
            dir.join("Game.gb"),
            dir.join("Hack v1.2.ips"),
        );
        fs::write(&base, b"original").expect("base");
        fs::write(&patch, ips(b"HACK")).expect("patch");
        let made = make_into(&patch, Some(&base), &[], &hacks).expect("made");
        assert_eq!(made, hacks.join("Hack v1.2.gb"));
        assert_eq!(fs::read(&made).expect("made"), b"HACKinal");
        assert_eq!(fs::read(&base).expect("base"), b"original", "left as it is");
        // Again: the same game.
        assert_eq!(make_into(&patch, Some(&base), &[], &hacks), Ok(made));
        fs::remove_dir_all(&dir).ok();
    }

    /// The made-up ROM `emu-patch`'s fixtures are for: its tests' `source`.
    fn fixture_rom() -> Vec<u8> {
        let mut state: u32 = 1;
        let mut rom: Vec<u8> = (0..96 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect();
        rom[0x9000..0xA000].fill(0xFF);
        rom
    }

    #[test]
    fn an_archipelago_patch_finds_its_game_by_md5() {
        let dir = temp_dir("romhack-archipelago");
        let patch = dir.join("AP_52413_P1_Player.apemerald");
        let fixture = include_bytes!("../../emu-patch/tests/fixtures/archipelago.apemerald");
        fs::write(&patch, fixture).expect("patch");
        let hacks = dir.join("Hacks");
        // Another game of its kind, its game under another kind's name,
        // and its game.
        let (other, misnamed, emerald) = (
            dir.join("Other.gba"),
            dir.join("Emerald.nds"),
            dir.join("Emerald.gba"),
        );
        fs::write(&other, b"another game").expect("other");
        fs::write(&misnamed, fixture_rom()).expect("misnamed");
        let known = |paths: &[&PathBuf]| -> Vec<(u32, PathBuf)> {
            paths.iter().map(|path| (0, (*path).clone())).collect()
        };
        assert_eq!(
            make_into(&patch, None, &known(&[&other, &misnamed, &emerald]), &hacks),
            Err(MakeError::NoBase(patch.clone())),
            "its game is not in the library"
        );
        fs::write(&emerald, fixture_rom()).expect("emerald");
        let made =
            make_into(&patch, None, &known(&[&other, &misnamed, &emerald]), &hacks).expect("made");
        assert_eq!(made, hacks.join("AP_52413_P1_Player.gba"));
        // What Archipelago's own code makes of it (emu-patch's tests).
        assert_eq!(
            crc32fast::hash(&fs::read(&made).expect("made")),
            0xA3B4_5298
        );
        // Given another game, it says which it is for.
        let wrong = make_into(&patch, Some(&other), &[], &hacks).expect_err("another game");
        let MakeError::Failed(why) = wrong else {
            panic!("{wrong:?}");
        };
        assert!(
            why.starts_with(
                "Cannot patch Other.gba with AP_52413_P1_Player.apemerald: the patch is for a different ROM: one with MD5 dc59b2e2918eca7d7187c16cc8201c68"
            ),
            "{why}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_the_game_a_checksum_names() {
        let dir = temp_dir("romhack-found");
        let (hacks, base, patch) = (
            dir.join("Hacks"),
            dir.join("Game.gba"),
            dir.join("Hack.ups"),
        );
        fs::write(&base, b"original").expect("base");
        fs::write(&patch, ups(b"original", b"HACK")).expect("patch");
        let known = [
            (crc32fast::hash(b"other"), dir.join("Other.gba")),
            (crc32fast::hash(b"original"), base.clone()),
        ];
        let made = make_into(&patch, None, &known, &hacks).expect("made");
        assert_eq!(fs::read(made).expect("made"), b"HACKinal");
        // Without the game, or for IPS, which names none: ask.
        assert_eq!(
            make_into(&patch, None, &known[..1], &hacks),
            Err(MakeError::NoBase(patch.clone()))
        );
        let ips_patch = dir.join("Hack.ips");
        fs::write(&ips_patch, ips(b"HACK")).expect("patch");
        assert_eq!(
            make_into(&ips_patch, None, &known, &hacks),
            Err(MakeError::NoBase(ips_patch.clone()))
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_patch_or_a_game_too_big_to_be_one_is_not_read() {
        let dir = temp_dir("romhack-too-big");
        let (hacks, base, patch) = (
            dir.join("Hacks"),
            dir.join("Game.gba"),
            dir.join("Hack.ips"),
        );
        // As long as it says, with nothing written: a byte more than a
        // cartridge holds.
        let too_big = |path: &Path| {
            fs::File::create(path)
                .and_then(|file| file.set_len(archive::MAX_ROM + 1))
                .expect("made")
        };
        fs::write(&base, b"original").expect("base");
        too_big(&patch);
        assert_eq!(
            make_into(&patch, Some(&base), &[], &hacks),
            Err(MakeError::Failed(format!(
                "Cannot read {}: it is too big to be a patch",
                patch.display()
            )))
        );
        fs::write(&patch, ips(b"HACK")).expect("patch");
        too_big(&base);
        assert_eq!(
            make_into(&patch, Some(&base), &[], &hacks),
            Err(MakeError::Failed(
                "Cannot patch Game.gba with Hack.ips: it is too big to be a game".to_owned()
            ))
        );
        assert!(!hacks.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn says_why_a_patch_does_not_apply() {
        let dir = temp_dir("romhack-refused");
        let (hacks, base, patch) = (
            dir.join("Hacks"),
            dir.join("Game.gba"),
            dir.join("Hack.ups"),
        );
        fs::write(&base, b"another!").expect("base");
        fs::write(&patch, ups(b"original", b"HACK")).expect("patch");
        let Err(MakeError::Failed(error)) = make_into(&patch, Some(&base), &[], &hacks) else {
            panic!("applied to the wrong game");
        };
        assert!(
            error.starts_with(
                "Cannot patch Game.gba with Hack.ups: the patch is for a different ROM"
            ),
            "{error}"
        );
        let notes = dir.join("notes.txt");
        fs::write(&notes, b"notes").expect("notes");
        let Err(MakeError::Failed(error)) = make_into(&patch, Some(&notes), &[], &hacks) else {
            panic!("applied to notes");
        };
        assert!(
            error.ends_with("it is not a .gb, .gbc, .gba or .nds game"),
            "{error}"
        );
        let cartridge = dir.join("Game.3ds");
        fs::write(&cartridge, b"original").expect("cartridge");
        let Err(MakeError::Failed(error)) = make_into(&patch, Some(&cartridge), &[], &hacks) else {
            panic!("applied to a 3DS game");
        };
        assert!(error.ends_with("3DS games take no patches"), "{error}");
        assert!(!hacks.exists());
        fs::remove_dir_all(&dir).ok();
    }
}
