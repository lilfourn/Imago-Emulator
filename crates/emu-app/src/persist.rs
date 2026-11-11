//! Where per-game files live on disk, and how they are written.
//!
//! Battery saves, save states, cheats and the game's own bindings sit next
//! to the ROM, as in mGBA and melonDS: `game.nds` gets `game.sav`,
//! `game.ss1`, `game.cht` and `game.bindings.toml`. A linked pair saves its
//! states next to the first player's ROM as `game.ls1`. A GBA game in a
//! DS's GBA slot keeps its own `.sav`; the session's cheats and bindings
//! are the DS game's, and its states are named after both games,
//! `game.emerald.ss1`, as the DS game alone cannot load them, nor it theirs
//! (see [`StateFiles`]). An RPG Maker game, a folder, has its files
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
//!
//! Every file is rewritten whole from memory, so two copies of the app
//! must never have one game open: each would replace the other's save. A
//! running game holds a lock for as long as it runs (see [`GameLock`]).
//!
//! Two kinds of copy of a battery save are kept beside it. Start Over sets
//! the save aside as `game.sav.bak`, then `.bak2` and so on, never
//! replacing one ([`set_aside`]). Loading a save state over a different
//! save keeps that save as `game.sav.before-load`, the newest of at most
//! five ([`keep_before_load`]): the next one moves the others up to
//! `game.sav.before-load2` and on, and the fifth goes. That copy is made
//! beside the save, which is never moved: there is a `game.sav` at every
//! moment, whatever fails next.

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use emu_core::System;

use crate::slots::LAST_FILE_SLOT;

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

    /// The folder the files are in.
    pub fn folder(&self) -> &Path {
        match self.base.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        }
    }

    /// The battery save.
    pub fn backup(&self) -> PathBuf {
        self.named("sav")
    }

    /// The battery save Start Over first sets aside ([`set_aside`]).
    pub fn set_aside(&self) -> PathBuf {
        self.named("sav.bak")
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
    /// falling back to an older plain-named save. A state in any slot
    /// counts, those only a script saves to as well.
    fn any_exist(&self) -> bool {
        let states =
            (0..=LAST_FILE_SLOT).flat_map(|slot| [self.state(slot, false), self.state(slot, true)]);
        [
            self.backup(),
            self.set_aside(),
            self.cheats(),
            self.bindings(),
        ]
        .into_iter()
        .chain(states)
        .any(|path| path.exists())
    }
}

/// A session's save states: where each slot's file is, and how to tell
/// them from another session's that sit under the same names.
///
/// A state loads only in a session of the same system and ROMs, which its
/// header names. So a DS game with a GBA cartridge in its slot names its
/// states after both, `game.emerald.ss1`: they are neither the DS game's
/// own (`game.ss1`) nor its states with another cartridge. Before they
/// were, such a session shared the DS game's names; a state it left under
/// one still loads in it (see [`StateFiles::to_read`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFiles {
    /// The first player's game's files, which name the states.
    game: GameFiles,
    /// Whether the session links several players.
    linked: bool,
    /// The name, without its extension, of the game in the first player's
    /// second slot, if there is one.
    cartridge: Option<String>,
    /// The session's system and ROMs, as its states' headers have them.
    system: System,
    rom_crc32: u32,
}

/// What is at a save state's path (see [`StateFiles::found`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// No file.
    Nothing,
    /// A state of this session's system and ROMs.
    Ours,
    /// Anything else: another session's state, such as the same game's
    /// with a different cartridge in its slot, or a file that is no state
    /// of this app's at all.
    Another,
}

impl StateFiles {
    /// The states of a session whose first player's game has the files
    /// `game`, `linked` if it has several players, with the game at
    /// `cartridge` in its second slot if it has one. Its states say they
    /// are for `system` and the ROMs with the checksum `rom_crc32`.
    pub fn new(
        game: GameFiles,
        linked: bool,
        cartridge: Option<&Path>,
        system: System,
        rom_crc32: u32,
    ) -> Self {
        let cartridge = cartridge
            .and_then(Path::file_stem)
            .map(|stem| stem.to_string_lossy().into_owned());
        Self {
            game,
            linked,
            cartridge,
            system,
            rom_crc32,
        }
    }

    /// The checksum of the session's ROMs, which its states' headers
    /// carry.
    pub fn rom_crc32(&self) -> u32 {
        self.rom_crc32
    }

    /// The session's own file for the state in `slot`, where it is saved.
    pub fn path(&self, slot: u8) -> PathBuf {
        match &self.cartridge {
            Some(cartridge) => self.game.named(&format!("{cartridge}.ss{slot}")),
            None => self.game.state(slot, self.linked),
        }
    }

    /// Where the library looks for the picture of where the first player's
    /// game was left in `slot`, beside that game's own state
    /// (`game.ss0.png`), when that is not this session's own picture: for
    /// a session with a game in its second slot. `None` for other sessions.
    pub fn library_picture(&self, slot: u8) -> Option<PathBuf> {
        self.cartridge.as_ref()?;
        Some(state_picture_path(&self.game.state(slot, self.linked)))
    }

    /// The file to load the state in `slot` from: the session's own, or,
    /// if it has none there, the one under the name its states had before
    /// they were named apart, while that file is this session's.
    pub fn to_read(&self, slot: u8) -> PathBuf {
        let own = self.path(slot);
        if self.cartridge.is_none() || own.exists() {
            return own;
        }
        let former = self.game.state(slot, self.linked);
        match self.found(&former) {
            Ok(Found::Ours) => former,
            _ => own,
        }
    }

    /// What is at `path`, by the start of a save state's header alone
    /// ([`emu_state::Header::peek`]): a state of this session's system and
    /// ROMs is its own whichever build's core saved it, and whether or not
    /// the rest of the file is whole.
    ///
    /// # Errors
    ///
    /// Returns the error if there is a file but it cannot be read.
    pub fn found(&self, path: &Path) -> io::Result<Found> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Found::Nothing),
            Err(error) => return Err(error),
        };
        let mut start = [0; emu_state::Header::PEEK_LEN];
        let header = match file.read_exact(&mut start) {
            Ok(()) => emu_state::Header::peek(&start),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => None,
            Err(error) => return Err(error),
        };
        let ours = header.is_some_and(|header| {
            // The state's own version, which is not what makes it ours.
            let playing = emu_state::Header {
                system: self.system,
                state_version: header.state_version,
                rom_crc32: self.rom_crc32,
            };
            header.check(&playing).is_ok()
        });
        Ok(if ours { Found::Ours } else { Found::Another })
    }

    /// Removes the session's state in `slot`, with the record and the
    /// picture beside it: its own file, and the one [`StateFiles::to_read`]
    /// would fall back to once that is gone.
    ///
    /// # Errors
    ///
    /// Returns the error if a file is there but cannot be removed.
    pub fn remove(&self, slot: u8) -> io::Result<()> {
        // The second is another file only if the first was the session's
        // own and there is one under the former name too.
        for _ in 0..2 {
            let state = self.to_read(slot);
            remove_if_present(&state)?;
            remove_if_present(&saves_record_path(&state))?;
            remove_if_present(&state_picture_path(&state))?;
        }
        Ok(())
    }
}

/// Removes the file at `path`, if there is one.
fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
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

/// A game's files held for this copy of the app, until it is dropped: an
/// empty file in the app's data folder, locked. It is named by
/// [`backup_key`] of the game's battery save, so it is one file for a game
/// however its path is spelled, and it can be made for a game in a folder
/// that cannot be written to. The files are never removed: removing one
/// another copy has just opened would let two hold the game at once.
#[derive(Debug)]
pub struct GameLock {
    /// Locked until it is closed.
    _file: File,
}

/// How long [`GameLock::take`] keeps asking for a lock that is held before
/// it says so. A program this app starts holds a copy of every file the
/// app has open, a running game's lock among them, from the moment it is
/// made until it turns into the program it was started as. A game closed
/// and opened again inside that moment finds its own lock still held, by
/// no one who is playing it. The moment is far shorter than this.
const LOCK_LET_GO_WITHIN: Duration = Duration::from_millis(250);

/// How long [`GameLock::take`] waits between asking twice.
const LOCK_ASKED_EVERY: Duration = Duration::from_millis(5);

impl GameLock {
    /// Takes the lock on the game whose battery save is `save`, with its
    /// lock file in `folder`, which is made if it is not there. Returns
    /// `None` if another copy of the app, or another game running in this
    /// one, holds it and keeps it for [`LOCK_LET_GO_WITHIN`].
    ///
    /// # Errors
    ///
    /// Returns the error if the lock file cannot be made or locked at all,
    /// as when the folder cannot be written to: nothing says the game is
    /// open elsewhere then.
    pub fn take(folder: &Path, save: &Path) -> io::Result<Option<Self>> {
        fs::create_dir_all(folder)?;
        // The key's own spelling of the save's name, so the lock is one
        // file where names differ by case, as the key is one.
        let key = backup_key(save);
        let name = format!(
            "{}-{:08x}.lock",
            safe_file_name(&file_name(Path::new(&key))),
            crc32fast::hash(key.as_bytes())
        );
        // Opened as it is, not emptied: Windows may refuse to empty a file
        // another process holds locked.
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(folder.join(name))?;
        let asked = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { _file: file })),
                Err(fs::TryLockError::WouldBlock) if asked.elapsed() < LOCK_LET_GO_WITHIN => {
                    thread::sleep(LOCK_ASKED_EVERY);
                }
                Err(fs::TryLockError::WouldBlock) => return Ok(None),
                Err(fs::TryLockError::Error(error)) => return Err(error),
            }
        }
    }
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

/// Whether the battery saves `a` and `b`, of a game for `system`, hold the
/// same save: the same bytes, but for a Game Boy cartridge's clock.
///
/// A Game Boy cartridge with a clock (MBC3) ends its save with the clock's
/// time, in the trailer other emulators write too: 48 bytes, or 44 in older
/// files, after save memory that is always a whole number of 512-byte
/// blocks. The time moves on by itself, so it differs between almost any
/// two moments, though the game has saved nothing.
pub fn same_save(system: System, a: &[u8], b: &[u8]) -> bool {
    /// `save` without a clock's trailer, if it ends with one.
    fn memory(save: &[u8]) -> &[u8] {
        match save.len() % 512 {
            trailer @ (44 | 48) => &save[..save.len() - trailer],
            _ => save,
        }
    }
    match system {
        System::Gb => memory(a) == memory(b),
        _ => a == b,
    }
}

/// How many battery saves from before a state was loaded are kept beside
/// the save: the newest five.
pub const BEFORE_LOAD_KEPT: u32 = 5;

/// Where the `number`th newest save from before a state was loaded over
/// the battery save at `save` is kept, counting from 1:
/// `game.sav.before-load`, then `game.sav.before-load2` up to
/// [`BEFORE_LOAD_KEPT`]. The names are these saves' own: Start Over's
/// `game.sav.bak` and its followers are another matter ([`set_aside`]).
pub fn before_load_path(save: &Path, number: u32) -> PathBuf {
    let mut path = save.as_os_str().to_owned();
    path.push(".before-load");
    if number > 1 {
        path.push(number.to_string());
    }
    PathBuf::from(path)
}

/// What became of a battery save about to be replaced by the one inside a
/// save state (see [`keep_before_load`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// Nothing needed keeping: there is no file, or it holds the save
    /// about to be written.
    Unneeded,
    /// The file holds another save, of which a copy is kept already.
    Already,
    /// The file holds another save, now kept here.
    New(PathBuf),
}

/// Makes `copy` hold what the file at `path` holds, leaving `path` as it
/// is: a hard link, which takes no room and no time, or, where the file
/// system has none (exFAT and FAT, as cards and sticks often are), a copy,
/// which is on disk before this returns. Nothing at `copy` is replaced.
///
/// A hard link is the same file under a second name, so it keeps the old
/// bytes only because `path` is next replaced whole, by a rename
/// ([`write_atomic`]), never written in place.
///
/// # Errors
///
/// Returns the error if neither can be made, or there is a file at `copy`
/// already. A copy cut short, as by a full disk, is removed.
pub fn keep_copy(path: &Path, copy: &Path) -> io::Result<()> {
    keep_copy_with(path, copy, |from, to| fs::hard_link(from, to))
}

/// Makes `copy` as [`keep_copy`] does, with `link` to make the hard link.
fn keep_copy_with(
    path: &Path,
    copy: &Path,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    match link(path, copy) {
        Ok(()) => return Ok(()),
        // Not for want of hard links: a copy would fare no better.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
            ) =>
        {
            return Err(error);
        }
        // Every system says "no hard links here" in words of its own.
        Err(_) => {}
    }
    let mut from = File::open(path)?;
    let mut to = File::create_new(copy)?;
    let copied = io::copy(&mut from, &mut to).and_then(|_| to.sync_all());
    drop(to);
    if copied.is_err() {
        fs::remove_file(copy).ok();
    }
    copied
}

/// Keeps the battery save at `path`, of a game for `system`, which is
/// about to be replaced with `bytes` from a save state, if it holds
/// another save ([`same_save`]) that is not kept already: a copy of it
/// ([`keep_copy`]) becomes `game.sav.before-load`, the saves kept before it
/// each move up a number, and the fifth of them goes
/// ([`before_load_path`]). So the newest five are kept, none of them twice.
/// A save kept already becomes the newest of them again, as it is the one
/// being replaced now: it is not the next to go.
///
/// The save itself stays where it is, and the copy is made before any
/// other is moved or dropped: whatever fails, here or in the write that
/// follows, there is a `game.sav`, and no save kept is lost for nothing.
///
/// # Errors
///
/// Returns the error if the file cannot be read or copied, or the saves
/// kept cannot be moved up; nothing must be written over `path` then.
pub fn keep_before_load(path: &Path, bytes: &[u8], system: System) -> io::Result<Kept> {
    let there = match fs::read(path) {
        Ok(there) => there,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Kept::Unneeded),
        Err(error) => return Err(error),
    };
    if same_save(system, &there, bytes) {
        return Ok(Kept::Unneeded);
    }
    let kept_at = (1..=BEFORE_LOAD_KEPT).find(|number| {
        fs::read(before_load_path(path, *number)).is_ok_and(|kept| same_save(system, &kept, &there))
    });
    match kept_at {
        Some(1) => Ok(Kept::Already),
        Some(number) => {
            // If no second copy of it can be made, the one kept stays in
            // its place: the save is kept either way.
            if let Ok(fresh) = fresh_copy_before_load(path) {
                place_newest_before_load(path, &fresh, Some(number))?;
            }
            Ok(Kept::Already)
        }
        None => {
            let fresh = fresh_copy_before_load(path)?;
            place_newest_before_load(path, &fresh, None).map(Kept::New)
        }
    }
}

/// A copy of the battery save at `save` ([`keep_copy`]) under a name of
/// its own, `game.sav.before-load.new`, until there is room for it among
/// the saves kept. One left there by a run that was cut short goes first.
fn fresh_copy_before_load(save: &Path) -> io::Result<PathBuf> {
    let mut fresh = before_load_path(save, 1).into_os_string();
    fresh.push(".new");
    let fresh = PathBuf::from(fresh);
    remove_if_present(&fresh)?;
    keep_copy(save, &fresh)?;
    Ok(fresh)
}

/// Makes `fresh`, a copy of the battery save at `save`, the newest of the
/// saves kept from before a state load, the others each moving up a
/// number. `instead_of` is the place of an older copy of the same save,
/// if one is kept, which goes first, so the save is kept once. Returns
/// where the copy now is.
///
/// # Errors
///
/// Returns the error if a copy cannot be moved or removed. `fresh` is
/// removed then, and the older copy may be gone too: the save is still at
/// `save`, over which nothing must be written.
fn place_newest_before_load(
    save: &Path,
    fresh: &Path,
    instead_of: Option<u32>,
) -> io::Result<PathBuf> {
    let newest = before_load_path(save, 1);
    let placed = instead_of
        .map_or(Ok(()), |number| {
            remove_if_present(&before_load_path(save, number))
        })
        .and_then(|()| make_room_before_load(save))
        .and_then(|()| rename(fresh, &newest));
    if let Err(error) = placed {
        fs::remove_file(fresh).ok();
        return Err(error);
    }
    Ok(newest)
}

/// Frees the newest place among the saves kept from before a state was
/// loaded over the battery save at `save`: each moves up a number, into
/// the first free place, or, with all five taken, the oldest goes.
fn make_room_before_load(save: &Path) -> io::Result<()> {
    let mut free = BEFORE_LOAD_KEPT;
    for number in 1..=BEFORE_LOAD_KEPT {
        if !before_load_path(save, number).try_exists()? {
            free = number;
            break;
        }
    }
    remove_if_present(&before_load_path(save, free))?;
    for number in (1..free).rev() {
        rename(
            &before_load_path(save, number),
            &before_load_path(save, number + 1),
        )?;
    }
    Ok(())
}

/// Checks that files can be made in `folder`, as a game's saves and save
/// states are, by making an empty one there and removing it: asking the
/// system is not enough, as a disk mounted read-only, or a share, says no
/// only when written to. No file already there is touched.
///
/// # Errors
///
/// Returns why no file could be made.
pub fn check_writable(folder: &Path) -> io::Result<()> {
    static MADE: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        ".ds-emulator-{}-{}.tmp",
        std::process::id(),
        MADE.fetch_add(1, Ordering::Relaxed)
    );
    let path = folder.join(name);
    File::create_new(&path)?;
    // It could be made; one that cannot be removed is no reason to warn.
    fs::remove_file(&path).ok();
    Ok(())
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

/// Renames a file or folder, on Windows waiting out a program that has
/// it open for a moment, and replaces a file so that a crash mid-write
/// never leaves half of one: `emu_os`'s, which Guide and the 3DS games'
/// crates use too.
pub use emu_os::file::{rename, write_atomic};

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
    fn a_state_only_a_script_saved_keeps_the_names_too() {
        let dir = temp_dir("persist-script-slots");
        // Scripts may use slots 1 to 9; the app's own keys reach 1 to 4.
        for (rom, state) in [
            ("Nine.gb", "Nine.gb.ss9"),
            ("Five.gb", "Five.gb.ss5"),
            ("Linked.gb", "Linked.gb.ls7"),
        ] {
            let rom = dir.join(rom);
            fs::write(&rom, b"rom").expect("write");
            assert_eq!(GameFiles::of(&rom).backup(), rom.with_extension("sav"));
            fs::write(dir.join(state), b"state").expect("write");
            let mut full = rom.clone().into_os_string();
            full.push(".sav");
            assert_eq!(GameFiles::of(&rom).backup(), PathBuf::from(full), "{state}");
        }
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

    /// A save state's bytes for `system` and the ROMs `rom_crc32`.
    fn state_of(system: System, rom_crc32: u32) -> Vec<u8> {
        let header = emu_state::Header {
            system,
            state_version: 1,
            rom_crc32,
        };
        emu_state::encode(&header, b"payload", None)
    }

    #[test]
    fn a_game_with_a_cartridge_in_its_slot_names_its_states_after_both() {
        let dir = temp_dir("state-files");
        let game = GameFiles::of(&dir.join("platinum.nds"));
        let alone = StateFiles::new(game.clone(), false, None, System::Nds, 0xA);
        let cartridge = dir.join("gba/emerald.gba");
        let slotted = StateFiles::new(game.clone(), false, Some(&cartridge), System::Nds, 0xB);
        assert_eq!(alone.path(1), dir.join("platinum.ss1"));
        assert_eq!(slotted.path(1), dir.join("platinum.emerald.ss1"));
        assert_eq!(slotted.path(0), dir.join("platinum.emerald.ss0"));
        // The library looks for the game's picture beside its own state.
        assert_eq!(
            slotted.library_picture(0),
            Some(dir.join("platinum.ss0.png"))
        );
        assert_eq!(alone.library_picture(0), None, "its own picture is there");
        // Another cartridge is another session again.
        let other = StateFiles::new(
            game.clone(),
            false,
            Some(&dir.join("ruby.gba")),
            System::Nds,
            0xC,
        );
        assert_eq!(other.path(1), dir.join("platinum.ruby.ss1"));
        // Linked consoles keep the names they had.
        let linked = StateFiles::new(game, true, None, System::Nds, 0xD);
        assert_eq!(linked.path(2), dir.join("platinum.ls2"));
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_state_file_is_ours_only_with_our_system_and_roms() {
        let dir = temp_dir("state-found");
        let game = GameFiles::of(&dir.join("platinum.nds"));
        let states = StateFiles::new(game, false, None, System::Nds, 0xA);
        let path = states.path(1);
        assert_eq!(states.found(&path).expect("none"), Found::Nothing);
        fs::write(&path, state_of(System::Nds, 0xA)).expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Ours);
        fs::write(&path, state_of(System::Nds, 0xB)).expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Another);
        fs::write(&path, state_of(System::Gba, 0xA)).expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Another);
        // Another emulator's file of the same name, or a cut-off one.
        fs::write(&path, b"mGBA save state, not ours at all").expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Another);
        fs::write(&path, b"EMUSTATE").expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Another);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_state_file_is_ours_whatever_its_version_and_however_it_ends() {
        let dir = temp_dir("state-found-start");
        let game = GameFiles::of(&dir.join("platinum.nds"));
        let states = StateFiles::new(game, false, None, System::Nds, 0xA);
        let path = states.path(1);
        // Another build's state of the same game is still this session's.
        let header = emu_state::Header {
            system: System::Nds,
            state_version: 9,
            rom_crc32: 0xA,
        };
        let mut state = emu_state::encode(&header, b"payload", None);
        fs::write(&path, &state).expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Ours);
        // So is one in a container this build cannot load, one cut short
        // after its header, and one whose bytes have gone bad: only the
        // header's start is looked at.
        state[8] = 7;
        fs::write(&path, &state).expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Ours);
        fs::write(&path, &state[..20]).expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Ours);
        fs::write(&path, &state[..19]).expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Another);
        // A system this build does not know is not this session's.
        let mut unknown = state_of(System::Nds, 0xA);
        unknown[10] = 0xEE;
        fs::write(&path, &unknown).expect("write");
        assert_eq!(states.found(&path).expect("read"), Found::Another);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn states_left_under_the_shared_names_still_load_in_the_session_that_made_them() {
        let dir = temp_dir("state-former");
        let game = GameFiles::of(&dir.join("platinum.nds"));
        let alone = StateFiles::new(game.clone(), false, None, System::Nds, 0xA);
        let cartridge = dir.join("emerald.gba");
        let slotted = StateFiles::new(game, false, Some(&cartridge), System::Nds, 0xB);
        // Before the names were apart: slot 1 saved with the cartridge in,
        // slot 2 without.
        fs::write(dir.join("platinum.ss1"), state_of(System::Nds, 0xB)).expect("write");
        fs::write(dir.join("platinum.ss2"), state_of(System::Nds, 0xA)).expect("write");
        assert_eq!(slotted.to_read(1), dir.join("platinum.ss1"));
        assert_eq!(slotted.to_read(2), dir.join("platinum.emerald.ss2"));
        assert_eq!(alone.to_read(1), dir.join("platinum.ss1"));
        assert_eq!(alone.to_read(2), dir.join("platinum.ss2"));
        // Once it has saved under its own name, that is the one it reads.
        fs::write(slotted.path(1), state_of(System::Nds, 0xB)).expect("write");
        assert_eq!(slotted.to_read(1), dir.join("platinum.emerald.ss1"));

        // Removing a slot takes both, with what sits beside them, and
        // leaves the other session's.
        fs::write(dir.join("platinum.ss1.saves"), b"record").expect("write");
        fs::write(dir.join("platinum.emerald.ss1.png"), b"picture").expect("write");
        slotted.remove(1).expect("removed");
        slotted.remove(2).expect("nothing of its own there");
        let mut left: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .map(|entry| file_name(&entry.expect("entry").path()))
            .collect();
        left.sort();
        assert_eq!(left, ["platinum.ss2"]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The files in `dir` whose names start with `game.sav.`, with what
    /// they hold: the copies kept of the battery save `game.sav`.
    fn copies_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| file_name(path).starts_with("game.sav."))
            .map(|path| (file_name(&path), fs::read(&path).expect("read")))
            .collect();
        files.sort();
        files
    }

    /// `files` as [`copies_in`] gives them.
    fn copies(files: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
        let files = files.iter();
        files
            .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
            .collect()
    }

    #[test]
    fn a_save_about_to_be_replaced_by_a_state_s_is_kept_only_if_it_differs() {
        let dir = temp_dir("before-load");
        let save = dir.join("game.sav");
        let keep = |bytes| keep_before_load(&save, bytes, System::Gba);
        assert_eq!(keep(b"new").expect("no file"), Kept::Unneeded);
        write_atomic(&save, b"new").expect("write");
        assert_eq!(keep(b"new").expect("the same"), Kept::Unneeded);
        assert_eq!(fs::read(&save).expect("left"), b"new");
        assert!(copies_in(&dir).is_empty());

        write_atomic(&save, b"old").expect("write");
        assert_eq!(
            keep(b"new").expect("differs"),
            Kept::New(dir.join("game.sav.before-load"))
        );
        assert_eq!(copies_in(&dir), copies(&[("game.sav.before-load", b"old")]));
        // The save itself has not moved: it is replaced only by the write
        // that follows, should that work.
        assert_eq!(fs::read(&save).expect("still there"), b"old");
        // A save of which a copy is kept already is not kept twice.
        write_atomic(&save, b"old").expect("write");
        assert_eq!(keep(b"new").expect("kept already"), Kept::Already);
        assert_eq!(fs::read(&save).expect("left"), b"old");
        assert_eq!(copies_in(&dir), copies(&[("game.sav.before-load", b"old")]));
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_copy_is_a_hard_link_or_where_there_are_none_a_file_of_its_own() {
        let dir = temp_dir("keep-copy");
        let save = dir.join("game.sav");
        write_atomic(&save, b"twenty hours").expect("write");
        let linked = dir.join("linked");
        keep_copy(&save, &linked).expect("kept");
        assert_eq!(fs::read(&linked).expect("the copy"), b"twenty hours");
        assert_eq!(fs::read(&save).expect("the save"), b"twenty hours");
        // Replaced whole, as the app writes saves, the save leaves the
        // copy as it was.
        write_atomic(&save, b"two hours").expect("write");
        assert_eq!(fs::read(&linked).expect("the copy"), b"twenty hours");

        // On a file system without hard links it is copied.
        let no_links = |_: &Path, _: &Path| Err(io::Error::from(io::ErrorKind::Unsupported));
        let copied = dir.join("copied");
        keep_copy_with(&save, &copied, no_links).expect("kept");
        assert_eq!(fs::read(&copied).expect("the copy"), b"two hours");
        // It shares nothing with the save, even written in place.
        fs::write(&save, b"overwritten").expect("write");
        assert_eq!(fs::read(&copied).expect("the copy"), b"two hours");

        // Neither replaces a file that is there, nor leaves one for a save
        // that is not.
        for kept in [&linked, &copied] {
            let taken = keep_copy(&save, kept).expect_err("there already");
            assert_eq!(taken.kind(), io::ErrorKind::AlreadyExists);
            let taken = keep_copy_with(&save, kept, no_links).expect_err("there already");
            assert_eq!(taken.kind(), io::ErrorKind::AlreadyExists);
        }
        assert_eq!(fs::read(&linked).expect("the copy"), b"twenty hours");
        assert_eq!(fs::read(&copied).expect("the copy"), b"two hours");
        let gone = dir.join("gone.sav");
        assert!(keep_copy(&gone, &dir.join("of-gone")).is_err());
        assert!(keep_copy_with(&gone, &dir.join("of-gone"), no_links).is_err());
        assert!(!dir.join("of-gone").exists());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn the_newest_five_saves_from_before_a_load_are_kept_and_start_over_s_never_touched() {
        let dir = temp_dir("before-load-five");
        let save = dir.join("game.sav");
        // What Start Over set aside, twice.
        fs::write(dir.join("game.sav.bak"), b"start over").expect("write");
        fs::write(dir.join("game.sav.bak2"), b"start over 2").expect("write");
        let start_over: [(&str, &[u8]); 2] = [
            ("game.sav.bak", b"start over"),
            ("game.sav.bak2", b"start over 2"),
        ];
        for number in 1..=5u8 {
            write_atomic(&save, &[number]).expect("write");
            keep_before_load(&save, b"state", System::Nds).expect("kept");
        }
        let mut five = copies(&start_over);
        five.extend(copies(&[
            ("game.sav.before-load", &[5]),
            ("game.sav.before-load2", &[4]),
            ("game.sav.before-load3", &[3]),
            ("game.sav.before-load4", &[2]),
            ("game.sav.before-load5", &[1]),
        ]));
        assert_eq!(copies_in(&dir), five);

        // A sixth moves them all up, and the oldest goes.
        write_atomic(&save, &[6]).expect("write");
        assert_eq!(
            keep_before_load(&save, b"state", System::Nds).expect("kept"),
            Kept::New(dir.join("game.sav.before-load"))
        );
        let mut six = copies(&start_over);
        six.extend(copies(&[
            ("game.sav.before-load", &[6]),
            ("game.sav.before-load2", &[5]),
            ("game.sav.before-load3", &[4]),
            ("game.sav.before-load4", &[3]),
            ("game.sav.before-load5", &[2]),
        ]));
        assert_eq!(copies_in(&dir), six);
        // One of the five is not kept again, wherever it is among them:
        // it becomes the newest, as the save being replaced now.
        write_atomic(&save, &[3]).expect("write");
        assert_eq!(
            keep_before_load(&save, b"state", System::Nds).expect("kept already"),
            Kept::Already
        );
        let mut again = copies(&start_over);
        again.extend(copies(&[
            ("game.sav.before-load", &[3]),
            ("game.sav.before-load2", &[6]),
            ("game.sav.before-load3", &[5]),
            ("game.sav.before-load4", &[4]),
            ("game.sav.before-load5", &[2]),
        ]));
        assert_eq!(copies_in(&dir), again);
        // The newest of them, kept again, stays where it is.
        assert_eq!(
            keep_before_load(&save, b"state", System::Nds).expect("kept already"),
            Kept::Already
        );
        assert_eq!(copies_in(&dir), again);
        // A gap, as one deleted by hand leaves, is closed by the next.
        fs::remove_file(dir.join("game.sav.before-load2")).expect("removed");
        write_atomic(&save, &[7]).expect("write");
        keep_before_load(&save, b"state", System::Nds).expect("kept");
        let mut closed = copies(&start_over);
        closed.extend(copies(&[
            ("game.sav.before-load", &[7]),
            ("game.sav.before-load2", &[3]),
            ("game.sav.before-load3", &[5]),
            ("game.sav.before-load4", &[4]),
            ("game.sav.before-load5", &[2]),
        ]));
        assert_eq!(copies_in(&dir), closed);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_save_kept_again_is_not_the_next_of_the_five_to_go() {
        let dir = temp_dir("before-load-again");
        let save = dir.join("game.sav");
        // Kept in turn: 1 is the oldest of the five.
        for number in 1..=5u8 {
            write_atomic(&save, &[number]).expect("write");
            keep_before_load(&save, b"state", System::Nds).expect("kept");
        }
        // The game holds 1 again, and a state is loaded over it.
        write_atomic(&save, &[1]).expect("write");
        assert_eq!(
            keep_before_load(&save, b"state", System::Nds).expect("kept already"),
            Kept::Already
        );
        // Then a save not kept yet is: the oldest goes, which is not 1.
        write_atomic(&save, &[6]).expect("write");
        keep_before_load(&save, b"state", System::Nds).expect("kept");
        assert_eq!(
            copies_in(&dir),
            copies(&[
                ("game.sav.before-load", &[6]),
                ("game.sav.before-load2", &[1]),
                ("game.sav.before-load3", &[5]),
                ("game.sav.before-load4", &[4]),
                ("game.sav.before-load5", &[3]),
            ])
        );
        // The game's own file was never moved.
        assert_eq!(fs::read(&save).expect("there"), [6]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_game_boy_clock_s_time_alone_keeps_nothing_before_a_load() {
        let dir = temp_dir("before-load-clock");
        let save = dir.join("game.sav");
        let with_clock = |memory: u8, clock: u8| {
            let mut bytes = vec![memory; 8192];
            bytes.extend([clock; 48]);
            bytes
        };
        write_atomic(&save, &with_clock(1, 10)).expect("write");
        let state = with_clock(1, 99);
        assert_eq!(
            keep_before_load(&save, &state, System::Gb).expect("the same save"),
            Kept::Unneeded
        );
        // Nor is a save kept twice for the time it was kept at.
        let other = with_clock(2, 50);
        keep_before_load(&save, &other, System::Gb).expect("kept");
        write_atomic(&save, &with_clock(1, 77)).expect("write");
        assert_eq!(
            keep_before_load(&save, &other, System::Gb).expect("kept already"),
            Kept::Already
        );
        assert_eq!(copies_in(&dir).len(), 1);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_game_boy_clock_s_time_is_no_part_of_what_a_save_holds() {
        // 8 KiB of save memory, then the clock's 48-byte trailer.
        let save = |memory: u8, clock: u8| {
            let mut save = vec![memory; 8192];
            save.extend([clock; 48]);
            save
        };
        assert!(same_save(System::Gb, &save(1, 10), &save(1, 99)));
        assert!(!same_save(System::Gb, &save(1, 10), &save(2, 10)));
        // An older file's 44-byte trailer, or a file with none.
        assert!(same_save(
            System::Gb,
            &save(1, 10)[..8192 + 44],
            &save(1, 99)
        ));
        assert!(same_save(System::Gb, &save(1, 10)[..8192], &save(1, 99)));
        assert!(!same_save(System::Gb, &save(2, 10)[..8192], &save(1, 99)));
        // A cartridge with a clock and no save memory holds nothing else.
        assert!(same_save(System::Gb, &[1; 48], &[2; 48]));
        // Memory of another size is another save.
        assert!(!same_save(System::Gb, &[1; 8192], &[1; 32768]));
        // Other systems' saves end with no clock: every byte counts.
        assert!(!same_save(System::Gba, &save(1, 10), &save(1, 99)));
        assert!(same_save(System::Nds, &save(1, 10), &save(1, 10)));
    }

    #[test]
    fn a_game_is_locked_by_one_holder_at_a_time_however_its_path_is_spelled() {
        let dir = temp_dir("game-lock");
        let locks = dir.join("data/Locks");
        fs::create_dir_all(dir.join("games/sub")).expect("create");
        let save = dir.join("games/Pokemon.sav");
        let held = GameLock::take(&locks, &save).expect("usable");
        assert!(held.is_some());
        // Another copy of the app, which opens the lock file itself.
        assert!(GameLock::take(&locks, &save).expect("usable").is_none());
        let respelled = dir.join("games/sub/../pokemon.sav");
        assert!(
            GameLock::take(&locks, &respelled)
                .expect("usable")
                .is_none()
        );
        // Another game is no matter, nor one of the same name elsewhere.
        assert!(
            GameLock::take(&locks, &dir.join("games/Other.sav"))
                .expect("usable")
                .is_some()
        );
        assert!(
            GameLock::take(&locks, &dir.join("games/sub/Pokemon.sav"))
                .expect("usable")
                .is_some()
        );
        // Once the first lets go, the game can be held again.
        drop(held);
        assert!(GameLock::take(&locks, &save).expect("usable").is_some());
        // A game in a folder that is gone can still be held.
        assert!(
            GameLock::take(&locks, &dir.join("gone/game.sav"))
                .expect("usable")
                .is_some()
        );
        // A lock folder that cannot be made is an error, not a held game.
        fs::write(dir.join("file"), b"in the way").expect("write");
        assert!(GameLock::take(&dir.join("file/Locks"), &save).is_err());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_lock_let_go_a_moment_after_it_is_asked_for_is_taken() {
        let dir = temp_dir("game-lock-moment");
        let locks = dir.join("data/Locks");
        let save = dir.join("games/Pokemon.sav");
        // As a program the app has just started holds its copy of the
        // lock for a moment after the game is closed.
        let held = GameLock::take(&locks, &save)
            .expect("usable")
            .expect("free");
        let holder = thread::spawn(move || {
            thread::sleep(LOCK_LET_GO_WITHIN / 5);
            drop(held);
        });
        assert!(GameLock::take(&locks, &save).expect("usable").is_some());
        holder.join().expect("let go");

        // One that is kept is still refused, after the wait and no more.
        let held = GameLock::take(&locks, &save).expect("usable");
        assert!(held.is_some());
        let asked = Instant::now();
        assert!(GameLock::take(&locks, &save).expect("usable").is_none());
        assert!(asked.elapsed() >= LOCK_LET_GO_WITHIN);
        assert!(asked.elapsed() < LOCK_LET_GO_WITHIN * 20);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The case itself: before the lock waited, a few openings in every
    /// hundred were refused here.
    #[cfg(unix)]
    #[test]
    fn a_game_closed_while_programs_start_opens_again_at_once() {
        use std::process::Command;
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let dir = temp_dir("game-lock-programs");
        let locks = dir.join("data/Locks");
        let save = dir.join("games/Pokemon.sav");
        let done = Arc::new(AtomicBool::new(false));
        let starters: Vec<_> = (0..2)
            .map(|_| {
                let done = Arc::clone(&done);
                thread::spawn(move || {
                    while !done.load(Ordering::Relaxed) {
                        Command::new("true").status().expect("started");
                    }
                })
            })
            .collect();
        // Each is let go as the next line asks for it again.
        let refused = (0..500)
            .filter(|_| GameLock::take(&locks, &save).expect("usable").is_none())
            .count();
        done.store(true, Ordering::Relaxed);
        for starter in starters {
            starter.join().expect("stopped");
        }
        assert_eq!(refused, 0, "of 500 openings");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_folder_is_checked_by_making_a_file_and_leaves_nothing_behind() {
        let dir = temp_dir("check-writable");
        fs::write(dir.join("game.sav"), b"save").expect("write");
        check_writable(&dir).expect("writable");
        check_writable(&dir).expect("writable again");
        let left: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .map(|entry| file_name(&entry.expect("entry").path()))
            .collect();
        assert_eq!(left, ["game.sav"]);
        // A folder that is gone, as an unplugged drive's is.
        assert!(check_writable(&dir.join("gone")).is_err());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_is_found_out() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = temp_dir("check-read-only");
        let locked = dir.join("locked");
        fs::create_dir(&locked).expect("create");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).expect("read-only");
        // An administrator may write anywhere, as in a container's tests.
        let privileged = File::create(locked.join("probe")).is_ok();
        if !privileged {
            let refused = check_writable(&locked).expect_err("read-only");
            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(GameFiles::of(&locked.join("game.gba")).folder(), locked);
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("writable");
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
