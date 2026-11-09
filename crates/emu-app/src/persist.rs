//! Where per-game files live on disk, and how they are written.
//!
//! Battery saves, save states, cheats and the game's own bindings sit next
//! to the ROM, as in mGBA and melonDS: `game.nds` gets `game.sav`,
//! `game.ss1`, `game.cht` and `game.bindings.toml`. A linked pair saves its
//! states next to the first player's ROM as `game.ls1`. A GBA game in a
//! DS's GBA slot keeps its own `.sav`; the session's states, cheats and
//! bindings are the DS game's. An RPG Maker game, a folder, has its files
//! beside the folder, under its whole name: `Game 1.2` gets `Game 1.2.ss1`.
//! Screenshots and recordings go there too,
//! named after the game and the time, as macOS names its own:
//! `game 2026-10-07 at 18.58.12.png`.
//!
//! Two ROMs with one name for different systems, such as `game.nds` and
//! `game.gba` in one folder, would share all those files. So a ROM with
//! such a namesake beside it names its files with its extension too:
//! `game.nds.sav`, `game.nds.ss1`, `game.nds.cht`, and `game.gba.sav` for
//! the other. Once it has any file named so, it keeps to those names, even
//! after the namesake goes, so its progress is never left behind. A ROM
//! with neither keeps the plain names, which other emulators use too, so a
//! `game.sav` copied from one loads. A plain `game.sav` left from before
//! the namesake arrived could be either game's, so neither reads it (see
//! [`stranded_save`]).
//!
//! The names are decided once, as a game starts (see [`GameFiles`]), and
//! kept for as long as it runs.

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::slots::STATE_SLOTS;

/// Where one game's own files go: its battery save, cheats and bindings,
/// and the session's save states if it is the first player's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameFiles {
    /// Each file's path, but for the suffix after its last dot: `game`,
    /// or `game.nds` when the files keep the ROM's extension.
    base: PathBuf,
}

impl GameFiles {
    /// The files of the game in `rom`, named by the rules above as the
    /// folder holds things now.
    pub fn of(rom: &Path) -> Self {
        let full = Self {
            base: rom.to_owned(),
        };
        let base = if rom.extension().is_some() && (has_namesake(rom) || full.any_exist()) {
            full.base
        } else {
            rom.with_extension("")
        };
        Self { base }
    }

    /// The files of the game in `folder`, which are named after all of it:
    /// a dot in a folder's name starts no extension.
    pub fn of_folder(folder: &Path) -> Self {
        Self {
            base: folder.to_owned(),
        }
    }

    /// The battery save.
    pub fn backup(&self) -> PathBuf {
        self.named("sav")
    }

    /// The cheat list.
    pub fn cheats(&self) -> PathBuf {
        self.named("cht")
    }

    /// The game's own bindings, laid over the user's.
    pub fn bindings(&self) -> PathBuf {
        self.named("bindings.toml")
    }

    /// The save state in `slot` of a session whose first player's game
    /// this is: `game.ss1`, or `game.ls1` if it is `linked` to others (a
    /// cartridge in a second slot is not a player).
    pub fn state(&self, slot: u8, linked: bool) -> PathBuf {
        let kind = if linked { "ls" } else { "ss" };
        self.named(&format!("{kind}{slot}"))
    }

    /// The file named `base.suffix`.
    fn named(&self, suffix: &str) -> PathBuf {
        let mut path = self.base.as_os_str().to_owned();
        path.push(".");
        path.push(suffix);
        PathBuf::from(path)
    }

    /// Whether any of these files is there, or a battery save set aside
    /// by Start Over: a game started over keeps its names, rather than
    /// falling back to an older plain-named save.
    fn any_exist(&self) -> bool {
        let states =
            (0..=STATE_SLOTS).flat_map(|slot| [self.state(slot, false), self.state(slot, true)]);
        [
            self.backup(),
            self.named("sav.bak"),
            self.cheats(),
            self.bindings(),
        ]
        .into_iter()
        .chain(states)
        .any(|path| path.exists())
    }
}

/// Whether a ROM with `rom`'s name but another ROM extension sits beside
/// it, as `game.gba` does beside `game.nds`.
fn has_namesake(rom: &Path) -> bool {
    let Some(own) = rom.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    emu_cores::ROM_EXTENSIONS
        .iter()
        .map(|(extension, _)| *extension)
        .filter(|extension| !extension.eq_ignore_ascii_case(own))
        // Both cases, for file systems that tell them apart.
        .flat_map(|extension| [extension.to_owned(), extension.to_ascii_uppercase()])
        .any(|extension| rom.with_extension(extension).is_file())
}

/// The plain-named `game.sav` beside `rom`, if `files`, the ROM's, keep
/// their full names (`game.nds.sav`), have no battery save yet, and the
/// plain one exists: a save from before a namesake arrived, which could
/// be either game's, so neither reads it.
pub fn stranded_save(rom: &Path, files: &GameFiles) -> Option<PathBuf> {
    let own = files.backup();
    let plain = rom.with_extension("sav");
    (own != plain && !own.exists() && plain.is_file()).then_some(plain)
}

/// A key equal for any two battery save paths that are one file: `save`
/// with its folder resolved (so `./a.sav` and `/games/a.sav` match),
/// compared without case, as macOS file systems usually do.
pub fn backup_key(save: &Path) -> String {
    let folder = match save.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder,
        _ => Path::new("."),
    };
    let resolved = match (fs::canonicalize(folder), save.file_name()) {
        (Ok(folder), Some(name)) => folder.join(name),
        _ => save.to_owned(),
    };
    resolved.to_string_lossy().to_lowercase()
}

/// The record beside the save state at `state` of the battery saves it
/// was saved with: `game.ss0.saves`.
pub fn saves_record_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".saves");
    PathBuf::from(path)
}

/// The picture beside the save state at `state` of the screen as it was
/// saved, which the library shows where the game was left off:
/// `game.ss0.png`.
pub fn state_picture_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".png");
    PathBuf::from(path)
}

/// What the battery saves at `saves` hold now, a line each: the file's
/// name and the CRC-32 of its bytes, or `none` if there is no file. Two
/// readings are equal only while every file is unchanged.
///
/// # Errors
///
/// Returns the error if a file exists but cannot be read.
pub fn saves_fingerprint(saves: &[PathBuf]) -> io::Result<String> {
    let mut lines = String::new();
    for save in saves {
        let name = save
            .file_name()
            .map_or_else(|| save.to_string_lossy(), |name| name.to_string_lossy());
        let crc = match fs::read(save) {
            Ok(bytes) => format!("{:08X}", crc32fast::hash(&bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => "none".to_owned(),
            Err(error) => return Err(error),
        };
        lines.push_str(&format!("{name} {crc}\n"));
    }
    Ok(lines)
}

/// Renames the file at `path` to `path.bak`, or to `path.bak2`, `.bak3`
/// and so on if that is taken, so no earlier backup is ever replaced.
/// Returns where it went, or `None` if there was no file.
///
/// # Errors
///
/// Returns the error if the file cannot be renamed.
pub fn set_aside(path: &Path) -> io::Result<Option<PathBuf>> {
    for number in 1..=u32::MAX {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".bak");
        if number > 1 {
            backup.push(number.to_string());
        }
        let backup = PathBuf::from(backup);
        if backup.try_exists()? {
            continue;
        }
        return match rename(path, &backup) {
            Ok(()) => Ok(Some(backup)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        };
    }
    Err(io::Error::other("every backup name is taken"))
}

/// Sets the file at `path` aside, as [`set_aside`] does, unless it holds
/// exactly `bytes` or is not there: for a file about to be replaced with
/// `bytes` whose old contents nothing else keeps. Returns where it went, if
/// it was set aside.
///
/// # Errors
///
/// Returns the error if the file cannot be read or renamed.
pub fn set_aside_unless_same(path: &Path, bytes: &[u8]) -> io::Result<Option<PathBuf>> {
    match fs::read(path) {
        Ok(there) if there == bytes => Ok(None),
        Ok(_) => set_aside(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
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

/// `path`'s file name, or the whole path if it has none, as notices name
/// files.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// `name`, a file's name, made one this system can hold.
///
/// On Windows, each of `<>:"/\|?*` and the control characters becomes
/// `_`; dots and spaces at the end go, as Windows drops them, which would
/// make `Game.` and `Game` one file; and a device's name, such as `CON`,
/// `nul.txt` or `COM1.gb`, gets `_` after its first part (`CON_`,
/// `nul_.txt`), as Windows would open the device instead. A name that
/// leaves nothing becomes `_`.
///
/// Elsewhere `name` is kept as it is: macOS and Linux take any name
/// without a `/`, and the names given here are already a path's last part.
pub fn safe_file_name(name: &str) -> Cow<'_, str> {
    if cfg!(windows) {
        Cow::Owned(windows_file_name(name))
    } else {
        Cow::Borrowed(name)
    }
}

/// `name` made a file name Windows can hold, as [`safe_file_name`] says.
/// Built on every system, so its tests run everywhere.
fn windows_file_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| {
            if u32::from(c) < 0x20 || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut safe = replaced.trim_end_matches(['.', ' ']).to_owned();
    if safe.is_empty() {
        return "_".to_owned();
    }
    // `NUL.tar.gz` is the device too, and so is `NUL .txt`.
    let first = safe
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    if is_windows_device(first) {
        safe.insert(first.len(), '_');
    }
    safe
}

/// Whether `stem` names one of Windows's devices, in any case: `CON`,
/// `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, or `COM` or `LPT` with a
/// digit or a superscript one, two or three.
fn is_windows_device(stem: &str) -> bool {
    let stem = stem.to_ascii_uppercase();
    let port = ["COM", "LPT"]
        .iter()
        .find_map(|port| stem.strip_prefix(port));
    ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
        || port.is_some_and(|number| {
            matches!(number, "\u{b9}" | "\u{b2}" | "\u{b3}")
                || (number.len() == 1 && number.bytes().all(|digit| digit.is_ascii_digit()))
        })
}

/// Renames the file or folder at `from` to `to`, replacing a file there,
/// as [`fs::rename`] does.
///
/// On Windows that can fail for a moment while another program, such as a
/// virus scanner or the search indexer, has either file open, so it is
/// tried again for about a second. A read-only file at `to` is made
/// writable first, as macOS and Linux replace one anyway.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::rename(from, to)
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)
    }
}

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    /// How many times a rename is tried.
    const TRIES: u32 = 10;
    /// How long to wait after a rename fails, at first and at most: in all,
    /// about a second.
    const FIRST_WAIT: Duration = Duration::from_millis(10);
    const LONGEST_WAIT: Duration = Duration::from_millis(200);

    /// Windows's `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION` and
    /// `ERROR_LOCK_VIOLATION`: another program has the file open.
    const IN_USE: [i32; 3] = [5, 32, 33];

    pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
        let mut wait = FIRST_WAIT;
        for _ in 1..TRIES {
            match fs::rename(from, to) {
                Err(error)
                    if error
                        .raw_os_error()
                        .is_some_and(|code| IN_USE.contains(&code)) =>
                {
                    if error.kind() == io::ErrorKind::PermissionDenied {
                        make_writable(to);
                    }
                    thread::sleep(wait);
                    wait = (wait * 2).min(LONGEST_WAIT);
                }
                done => return done,
            }
        }
        fs::rename(from, to)
    }

    /// Clears the read-only flag of the file at `path`, if it has one.
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears the file's read-only flag, nothing more"
    )]
    fn make_writable(path: &Path) {
        if let Ok(metadata) = fs::metadata(path)
            && metadata.is_file()
            && metadata.permissions().readonly()
        {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions).ok();
        }
    }
}

/// Replaces `path` with `bytes` so that a crash or power cut mid-write
/// never leaves a half-written file: the bytes go to a sibling temporary
/// file first, reach the disk, and only then is it renamed over `path`
/// (with [`rename`], which on Windows waits out a virus scanner).
///
/// On macOS the folder is not synced after the rename: syncing there
/// flushes the whole drive's cache, which is slow, and without it a power
/// cut can at worst undo the rename, leaving the old file whole. Windows
/// can't open a folder to sync it, and NTFS journals the rename itself.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    rename(&temporary, path)?;
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let folder = match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
        // Best effort: not every file system can sync a folder.
        File::open(folder).and_then(|folder| folder.sync_all()).ok();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_files_sit_next_to_the_rom() {
        let files = GameFiles::of(Path::new("/games/platinum.nds"));
        assert_eq!(files.backup(), Path::new("/games/platinum.sav"));
        assert_eq!(files.cheats(), Path::new("/games/platinum.cht"));
        assert_eq!(files.bindings(), Path::new("/games/platinum.bindings.toml"));
        assert_eq!(files.state(1, false), Path::new("/games/platinum.ss1"));
        assert_eq!(files.state(2, true), Path::new("/games/platinum.ls2"));
    }

    #[test]
    fn a_folder_names_its_files_with_all_its_name() {
        let files = GameFiles::of_folder(Path::new("/games/Pokemon TGOM 4.2.2"));
        assert_eq!(
            files.state(1, false),
            Path::new("/games/Pokemon TGOM 4.2.2.ss1")
        );
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
        let key = backup_key(&dir.join("Pokemon.sav"));
        assert_eq!(key, backup_key(&dir.join("sub/../pokemon.sav")));
        assert_ne!(key, backup_key(&dir.join("sub/pokemon.sav")));
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-app-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn namesakes_for_other_systems_keep_files_of_their_own() {
        let dir = temp_dir("persist-namesakes");
        let gba = dir.join("Game.gba");
        let nds = dir.join("Game.nds");
        fs::write(&gba, b"rom").expect("write");
        fs::write(dir.join("Game.txt"), b"notes").expect("write");
        // Alone, as other emulators name it.
        assert_eq!(GameFiles::of(&gba).backup(), dir.join("Game.sav"));
        assert_eq!(GameFiles::of(&gba).cheats(), dir.join("Game.cht"));

        fs::write(&nds, b"rom").expect("write");
        let files = GameFiles::of(&nds);
        assert_eq!(GameFiles::of(&gba).backup(), dir.join("Game.gba.sav"));
        assert_eq!(files.backup(), dir.join("Game.nds.sav"));
        assert_eq!(files.cheats(), dir.join("Game.nds.cht"));
        assert_eq!(files.bindings(), dir.join("Game.nds.bindings.toml"));
        assert_eq!(files.state(1, false), dir.join("Game.nds.ss1"));

        // A plain save from before is neither's.
        assert_eq!(stranded_save(&nds, &files), None);
        fs::write(dir.join("Game.sav"), b"save").expect("write");
        assert_eq!(stranded_save(&nds, &files), Some(dir.join("Game.sav")));
        fs::write(dir.join("Game.nds.sav"), b"save").expect("write");
        assert_eq!(stranded_save(&nds, &files), None);
        let gba_files = GameFiles::of(&gba);
        assert_eq!(stranded_save(&gba, &gba_files), Some(dir.join("Game.sav")));
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn files_named_in_full_stay_so_once_the_namesake_goes() {
        let dir = temp_dir("persist-namesake-gone");
        let gba = dir.join("Game.gba");
        let nds = dir.join("Game.nds");
        fs::write(&gba, b"rom").expect("write");
        fs::write(dir.join("Game.sav"), b"old").expect("write");
        assert_eq!(GameFiles::of(&gba).backup(), dir.join("Game.sav"));
        // The namesake comes, and the game saves under its full name...
        fs::write(&nds, b"rom").expect("write");
        let files = GameFiles::of(&gba);
        fs::write(files.backup(), b"new").expect("write");
        // ...which it keeps once the namesake goes, not the older plain one.
        fs::remove_file(&nds).expect("remove");
        assert_eq!(GameFiles::of(&gba), files);
        assert_eq!(GameFiles::of(&gba).backup(), dir.join("Game.gba.sav"));
        // Starting over sets the save aside, and the names still stay.
        set_aside(&files.backup()).expect("set aside");
        assert_eq!(GameFiles::of(&gba), files);

        // A save state alone is enough to keep the names.
        let other = dir.join("Other.gb");
        fs::write(&other, b"rom").expect("write");
        fs::write(dir.join("Other.gb.ss0"), b"state").expect("write");
        assert_eq!(GameFiles::of(&other).backup(), dir.join("Other.gb.sav"));
        assert_eq!(
            GameFiles::of(&other).state(0, false),
            dir.join("Other.gb.ss0")
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn setting_aside_never_replaces_an_earlier_backup() {
        let dir = temp_dir("set-aside");
        let save = dir.join("game.sav");
        assert_eq!(set_aside(&save).expect("nothing to do"), None);
        for (round, expected) in [(1, "game.sav.bak"), (2, "game.sav.bak2")] {
            fs::write(&save, [round]).expect("write");
            let kept = set_aside(&save).expect("renamed");
            assert_eq!(kept, Some(dir.join(expected)));
            assert!(!save.exists());
        }
        assert_eq!(fs::read(dir.join("game.sav.bak")).expect("first"), [1]);
        assert_eq!(fs::read(dir.join("game.sav.bak2")).expect("second"), [2]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_file_about_to_be_replaced_is_set_aside_only_if_it_differs() {
        let dir = temp_dir("set-aside-unless");
        let save = dir.join("game.sav");
        assert_eq!(set_aside_unless_same(&save, b"new").expect("none"), None);
        fs::write(&save, b"new").expect("write");
        assert_eq!(set_aside_unless_same(&save, b"new").expect("same"), None);
        assert_eq!(fs::read(&save).expect("left"), b"new");
        fs::write(&save, b"old").expect("write");
        assert_eq!(
            set_aside_unless_same(&save, b"new").expect("differs"),
            Some(dir.join("game.sav.bak"))
        );
        assert!(!save.exists());
        assert_eq!(fs::read(dir.join("game.sav.bak")).expect("kept"), b"old");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn fingerprints_change_with_the_battery_saves() {
        let dir = temp_dir("fingerprint");
        let saves = [dir.join("a.sav"), dir.join("b.sav")];
        fs::write(&saves[0], b"one").expect("write");
        let before = saves_fingerprint(&saves).expect("readable");
        assert_eq!(
            before,
            format!("a.sav {:08X}\nb.sav none\n", crc32fast::hash(b"one"))
        );
        assert_eq!(saves_fingerprint(&saves).expect("readable"), before);
        fs::write(&saves[0], b"two").expect("write");
        assert_ne!(saves_fingerprint(&saves).expect("readable"), before);
        assert_eq!(
            saves_record_path(&dir.join("a.ss0")),
            dir.join("a.ss0.saves")
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn names_windows_cannot_hold_are_made_ones_it_can() {
        let cases = [
            (
                "Pokemon - Platinum (Random 123456).nds",
                "Pokemon - Platinum (Random 123456).nds",
            ),
            ("Pokemon: Platinum?.nds", "Pokemon_ Platinum_.nds"),
            ("a<b>c\"d|e*f\\g/h.gba", "a_b_c_d_e_f_g_h.gba"),
            ("tab\there\u{1}.gb", "tab_here_.gb"),
            ("Game. . ", "Game"),
            ("...", "_"),
            ("", "_"),
            ("CON", "CON_"),
            ("con.gb", "con_.gb"),
            ("Nul.tar.gz", "Nul_.tar.gz"),
            ("NUL .txt", "NUL_ .txt"),
            ("COM1.gba", "COM1_.gba"),
            ("lpt9", "lpt9_"),
            ("COM\u{b9}.nds", "COM\u{b9}_.nds"),
            ("CONOUT$.sav", "CONOUT$_.sav"),
            // Only those exact names are devices.
            ("CONSOLE.nds", "CONSOLE.nds"),
            ("COM10.gb", "COM10.gb"),
            ("Aux Battle.gba", "Aux Battle.gba"),
            ("Pokémon Émeraude.gba", "Pokémon Émeraude.gba"),
        ];
        for (name, safe) in cases {
            assert_eq!(windows_file_name(name), safe, "{name:?}");
            // What is safe stays so.
            assert_eq!(windows_file_name(safe), safe, "{safe:?}");
        }
        // Only Windows changes names.
        let kept = safe_file_name("CON.gb: a game?");
        if cfg!(windows) {
            assert_eq!(kept, "CON_.gb_ a game_");
        } else {
            assert_eq!(kept, "CON.gb: a game?");
        }
    }

    #[test]
    fn a_rename_replaces_the_file_there() {
        let dir = temp_dir("rename");
        let (from, to) = (dir.join("game.sav.tmp"), dir.join("game.sav"));
        fs::write(&from, b"new").expect("write");
        fs::write(&to, b"old").expect("write");
        // Read-only, as macOS and Linux replace it anyway.
        let mut permissions = fs::metadata(&to).expect("metadata").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&to, permissions).expect("read-only");
        rename(&from, &to).expect("renamed");
        assert_eq!(fs::read(&to).expect("read"), b"new");
        assert!(!from.exists());
        assert!(rename(&from, &to).is_err(), "nothing left to rename");
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
