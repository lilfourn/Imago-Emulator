//! Games downloaded as `.zip` files. The ROMs inside are unpacked into the
//! app's own games folder, where they and their saves stay after the
//! download is deleted.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use zip::ZipArchive;

use crate::persist;

/// The most a ROM unpacks to: a DS cartridge holds at most 512 MiB, so a
/// bigger file is not a game.
const MAX_ROM: u64 = 512 << 20;

/// Where unpacked games are kept, e.g.
/// `~/Library/Application Support/ds-emulator/Games` on macOS.
pub fn games_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("ds-emulator").join("Games"))
}

/// Whether `path` is a zip file, judged by its extension.
pub fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
}

/// Unpacks the games in the zip file at `zip` into the games folder and
/// returns where each is now: at least one.
///
/// Blocking file I/O; call it off the UI thread.
///
/// # Errors
///
/// Fails if the zip cannot be read, holds no game, or a game cannot be
/// written.
pub fn unpack(zip: &Path) -> Result<Vec<PathBuf>, String> {
    let folder = games_dir().ok_or("There is no folder to unpack games into")?;
    unpack_into(zip, &folder)
}

fn unpack_into(zip: &Path, folder: &Path) -> Result<Vec<PathBuf>, String> {
    let failed =
        |error: &dyn std::fmt::Display| format!("Cannot unpack {}: {error}", zip.display());
    let file = File::open(zip).map_err(|error| failed(&error))?;
    let mut archive = ZipArchive::new(file).map_err(|error| failed(&error))?;
    let mut games = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| failed(&error))?;
        // macOS adds a copy of each file's metadata under `__MACOSX`, with
        // the file's own name and extension.
        let name = entry
            .enclosed_name()
            .filter(|inner| entry.is_file() && !inner.starts_with("__MACOSX"))
            .filter(|inner| emu_cores::system_for_path(inner).is_some())
            .and_then(|inner| inner.file_name().map(PathBuf::from));
        let Some(name) = name else {
            continue;
        };
        fs::create_dir_all(folder).map_err(|error| failed(&error))?;
        let crc32 = entry.crc32();
        games.push(extract(&mut entry, crc32, folder, &name).map_err(|error| failed(&error))?);
    }
    if games.is_empty() {
        return Err(format!(
            "{} holds no .gb, .gbc, .gba or .nds game",
            zip.display()
        ));
    }
    Ok(games)
}

/// Writes the ROM `entry`, whose CRC-32 is `crc32`, into `folder` as
/// `name`, and returns its path. The same game unpacked before is used as
/// it is; a different one of that name is left alone, as it may have
/// saves beside it, and this one is numbered: `Game (2).gba`.
fn extract(entry: &mut impl Read, crc32: u32, folder: &Path, name: &Path) -> io::Result<PathBuf> {
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let extension = name.extension().unwrap_or_default().to_string_lossy();
    for number in 1.. {
        let path = if number == 1 {
            folder.join(name)
        } else {
            folder.join(format!("{stem} ({number}).{extension}"))
        };
        match fs::read(&path) {
            Ok(existing) if crc32fast::hash(&existing) == crc32 => return Ok(path),
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut rom = Vec::new();
        entry.take(MAX_ROM + 1).read_to_end(&mut rom)?;
        if rom.len() as u64 > MAX_ROM {
            return Err(io::Error::other(format!(
                "{} is too big to be a game",
                name.display()
            )));
        }
        persist::write_atomic(&path, &rom)?;
        return Ok(path);
    }
    unreachable!("a free name is always found")
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;

    /// A zip file in `dir` named `name` holding `files`, compressed as
    /// downloads are.
    fn zip_file(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (file, bytes) in files {
            writer.start_file(*file, deflated).expect("start");
            writer.write_all(bytes).expect("write");
        }
        let bytes = writer.finish().expect("finish").into_inner();
        let path = dir.join(name);
        fs::write(&path, bytes).expect("zip written");
        path
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-app-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn recognises_zips_by_extension() {
        assert!(is_zip(Path::new("Tetris (World).ZIP")));
        assert!(!is_zip(Path::new("tetris.gb")));
        assert!(!is_zip(Path::new("zip")));
    }

    #[test]
    fn unpacks_only_the_games() {
        let dir = temp_dir("unpack-games");
        let games = dir.join("Games");
        let zip = zip_file(
            &dir,
            "Tetris.zip",
            &[
                ("Tetris/Tetris (World).gb", b"tetris"),
                ("__MACOSX/Tetris/._Tetris (World).gb", b"metadata"),
                ("Tetris/readme.txt", b"hello"),
            ],
        );
        let unpacked = unpack_into(&zip, &games).expect("unpacks");
        assert_eq!(unpacked, [games.join("Tetris (World).gb")]);
        assert_eq!(fs::read(&unpacked[0]).expect("unpacked"), b"tetris");
        assert_eq!(fs::read_dir(&games).expect("games").count(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpacking_again_reuses_the_game_and_keeps_a_different_one() {
        let dir = temp_dir("unpack-again");
        let games = dir.join("Games");
        let first = zip_file(&dir, "a.zip", &[("Game.gba", b"first")]);
        let second = zip_file(&dir, "b.zip", &[("Game.gba", b"second")]);
        let path = games.join("Game.gba");
        assert_eq!(
            unpack_into(&first, &games).expect("first"),
            [path.as_path()]
        );
        assert_eq!(
            unpack_into(&first, &games).expect("again"),
            [path.as_path()]
        );
        let renamed = games.join("Game (2).gba");
        assert_eq!(
            unpack_into(&second, &games).expect("second"),
            [renamed.as_path()]
        );
        assert_eq!(fs::read(&path).expect("kept"), b"first");
        assert_eq!(fs::read(&renamed).expect("numbered"), b"second");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_zip_without_a_game_or_not_a_zip_is_refused() {
        let dir = temp_dir("unpack-refused");
        let games = dir.join("Games");
        let empty = zip_file(&dir, "notes.zip", &[("notes.txt", b"notes")]);
        assert!(
            unpack_into(&empty, &games)
                .unwrap_err()
                .contains("holds no")
        );
        let fake = dir.join("fake.zip");
        fs::write(&fake, b"not a zip").expect("written");
        assert!(
            unpack_into(&fake, &games)
                .unwrap_err()
                .starts_with("Cannot unpack")
        );
        assert!(!games.exists());
        fs::remove_dir_all(&dir).ok();
    }
}
