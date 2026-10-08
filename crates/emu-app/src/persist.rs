//! Where per-game files live on disk, and how they are written.
//!
//! Battery saves, save states, cheats and the game's own bindings sit next
//! to the ROM, as in mGBA and melonDS: `game.nds` gets `game.sav`,
//! `game.ss1`, `game.cht` and `game.bindings.toml`. A linked pair saves its
//! states next to the first player's ROM as `game.ls1`. A GBA game in a
//! DS's GBA slot keeps its own `.sav`; the session's states, cheats and
//! bindings are the DS game's. Screenshots and recordings go there too,
//! named after the game and the time, as macOS names its own:
//! `game 2026-10-07 at 18.58.12.png`.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// The battery save file for `rom`.
pub fn backup_path(rom: &Path) -> PathBuf {
    rom.with_extension("sav")
}

/// A key equal for any two ROMs whose battery saves land in one file: the
/// save path with its folder resolved (so `./a.nds` and `/games/a.gba`
/// match), compared without case, as macOS file systems usually do.
pub fn backup_key(rom: &Path) -> String {
    let save = backup_path(rom);
    let folder = match save.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder,
        _ => Path::new("."),
    };
    let resolved = match (fs::canonicalize(folder), save.file_name()) {
        (Ok(folder), Some(name)) => folder.join(name),
        _ => save,
    };
    resolved.to_string_lossy().to_lowercase()
}

/// The cheat list for `rom`.
pub fn cheats_path(rom: &Path) -> PathBuf {
    rom.with_extension("cht")
}

/// The game's own bindings for `rom`, laid over the user's.
pub fn bindings_path(rom: &Path) -> PathBuf {
    rom.with_extension("bindings.toml")
}

/// The save state file in `slot` for a session of `roms`, one per player
/// (a cartridge in a second slot is not a player).
///
/// # Panics
///
/// Panics if `roms` is empty.
pub fn state_path(roms: &[PathBuf], slot: u8) -> PathBuf {
    let extension = if roms.len() > 1 { "ls" } else { "ss" };
    roms[0].with_extension(format!("{extension}{slot}"))
}

/// Creates a new, empty file next to `rom` for a screenshot or recording
/// taken at `stamp` (such as `2026-10-07 at 18.58.12`), and returns its
/// path and the file: `game <stamp>.<extension>`, or `game <stamp>
/// 2.<extension>` and so on if that name is taken, even by a capture being
/// written at the same moment. Existing files are never replaced.
///
/// # Errors
///
/// Returns the error if the file cannot be created.
pub fn create_capture(rom: &Path, stamp: &str, extension: &str) -> io::Result<(PathBuf, File)> {
    let game = rom
        .file_stem()
        .map_or_else(|| "capture".into(), |stem| stem.to_string_lossy());
    let mut number = 1u32;
    loop {
        let name = match number {
            1 => format!("{game} {stamp}.{extension}"),
            _ => format!("{game} {stamp} {number}.{extension}"),
        };
        let path = rom.with_file_name(name);
        match File::create_new(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => number += 1,
            Err(error) => return Err(error),
        }
    }
}

/// Replaces `path` with `bytes` so that a crash mid-write never leaves a
/// half-written file: the bytes go to a sibling temporary file first, which
/// is then renamed over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_files_sit_next_to_the_rom() {
        let rom = Path::new("/games/platinum.nds");
        assert_eq!(backup_path(rom), Path::new("/games/platinum.sav"));
        assert_eq!(
            bindings_path(rom),
            Path::new("/games/platinum.bindings.toml")
        );
        assert_eq!(
            state_path(&[rom.to_owned()], 1),
            Path::new("/games/platinum.ss1")
        );
        let linked = [
            PathBuf::from("/games/red.gb"),
            PathBuf::from("/games/blue.gb"),
        ];
        assert_eq!(state_path(&linked, 2), Path::new("/games/red.ls2"));
    }

    #[test]
    fn captures_are_named_after_the_game_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("emu-app-captures-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let rom = dir.join("platinum.nds");
        let stamp = "2026-10-07 at 18.58.12";
        let created = |extension| {
            let (path, _) = create_capture(&rom, stamp, extension).expect("create");
            path
        };
        let first = created("png");
        assert_eq!(first, dir.join("platinum 2026-10-07 at 18.58.12.png"));
        // The first is still empty, being written, and keeps its name.
        assert_eq!(
            created("png"),
            dir.join("platinum 2026-10-07 at 18.58.12 2.png")
        );
        assert_eq!(
            created("mov"),
            dir.join("platinum 2026-10-07 at 18.58.12.mov")
        );
        assert!(create_capture(&dir.join("missing/rom.nds"), stamp, "png").is_err());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn backup_keys_see_through_folder_spellings_and_case() {
        let dir = std::env::temp_dir().join(format!("emu-app-keys-{}", std::process::id()));
        fs::create_dir_all(dir.join("sub")).expect("create temp dir");
        let key = backup_key(&dir.join("Pokemon.nds"));
        assert_eq!(key, backup_key(&dir.join("sub/../pokemon.gba")));
        assert_ne!(key, backup_key(&dir.join("sub/pokemon.gba")));
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn write_atomic_replaces_contents_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("emu-app-persist-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("game.sav");

        write_atomic(&path, b"first").expect("first write");
        write_atomic(&path, b"second").expect("second write");

        assert_eq!(fs::read(&path).expect("read back"), b"second");
        assert!(!dir.join("game.sav.tmp").exists());
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
