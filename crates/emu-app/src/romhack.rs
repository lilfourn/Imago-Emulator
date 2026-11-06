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
//! Everything here blocks; run it off the UI thread.

use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};

use crate::archive;
use crate::persist::file_name;

/// The patches the app makes games from, by extension.
const PATCHES: [&str; 5] = ["ips", "ups", "bps", "xdelta", "vcdiff"];

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
    let bytes = fs::read(patch)
        .map_err(|error| MakeError::Failed(format!("Cannot read {}: {error}", patch.display())))?;
    let base = match base {
        Some(base) => base.to_owned(),
        None => emu_patch::source_crc32(&bytes)
            .and_then(|crc32| {
                known
                    .iter()
                    .find(|(known, path)| *known == crc32 && path.is_file())
            })
            .map(|(_, path)| path.clone())
            .ok_or_else(|| MakeError::NoBase(patch.to_owned()))?,
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
    let source = fs::read(&base).map_err(|error| failed(&error))?;
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
