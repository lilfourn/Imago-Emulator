//! Runs a core on its own thread at the system's frame rate.
//!
//! The UI thread sends [`Command`]s; the emulation thread publishes each
//! finished frame into a shared slot and sends an [`Event`] to wake the UI.
//! Wake-ups are coalesced, so a slow UI never queues stale frames.
//!
//! Screenshots and recordings take the core's own frames here, every one
//! of them for a recording, rather than the coalesced ones the UI shows.
//!
//! A Lua script runs here too, after each frame (see `scripting.rs`): what
//! it draws is published with the frame, and the buttons it presses go
//! into the next.
//!
//! Pacing is a sleep to the next frame deadline, which fast-forward and
//! slow motion move closer or further apart (see `speed.rs`). Sound stays
//! in sync because the audio sink adjusts its resampling ratio to the
//! buffer level (see `resample.rs`). An RPG Maker game's sound runs on its
//! player's clock instead, so it plays as it comes, from the thread that
//! reads it, while the game runs at normal speed (see `Loaded::live_sound`).
//!
//! After each frame is published, the core is serialized for rewind; a
//! worker thread turns that into a compressed delta (see
//! `rewind_thread.rs`). Faster than normal speed, only about one frame per
//! normal frame's time is recorded (see `rewind.rs`).
//!
//! Battery saves are copied here once a second while they change, and on
//! pausing, and written to disk on a thread of their own (see
//! `save_writer.rs`). Should the emulation thread panic, it still writes
//! them, and tells the UI the game has stopped. They, the save states and
//! the checkpoint are kept by [`saves::Saves`].
//!
//! Each game's checkpoint, a save state in slot [`CHECKPOINT`], keeps where
//! the player left it, with a record of the saves the game had then: the
//! UI has it saved as the player leaves, and picked up as the game starts
//! again, unless a save has changed since. An RPG Maker game's player can
//! stop on an error in the game's scripts, and its game cannot always be
//! saved, as in a battle, so its checkpoint is also saved every minute it
//! plays (see [`CHECKPOINT_INTERVAL`]).

use std::any::Any;
use std::fs;
use std::io::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use emu_cheats::Cheat;
use emu_core::debug::{Breakpoint, RunOutcome, StopReason};
use emu_core::{Buttons, Cartridge, Core, CoreInfo, DmgColors, Frame, Input, Media, System};
use emu_cores::rpg_maker;
use emu_script::{Draw, Script, ScriptError, StateRequest};

use crate::audio::AudioSink;
use crate::capture;
use crate::debugger::{self, Pending, RunState, Snapshot};
use crate::latch::Latch;
use crate::persist::{self, GameFiles, GameLock, StateFiles, file_name};
use crate::qos;
use crate::recorder::Recorder;
use crate::refresh::Lock;
use crate::rewind;
use crate::rewind_thread::RewindThread;
use crate::scripting::{self, Running};
use crate::speed::{self, Speed};
use crate::trace;
use crate::turbo::Turbo;

mod saves;

use saves::{PickUp, Saves};

/// How often dirty battery saves are written to disk.
const BACKUP_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// How often a paused game asks how a battery save being written went,
/// to tell the player without waiting for the game to play on.
const WRITTEN_CHECK_INTERVAL: Duration = Duration::from_millis(50);

/// Memory for rewind history: about a minute of Game Boy play.
const REWIND_BUDGET_BYTES: usize = 64 << 20;

/// The save-state slot that holds each game's checkpoint: where the
/// player left it, picked up when the game next starts.
pub const CHECKPOINT: u8 = 0;

/// How often a game whose core checkpoints while playing (an RPG Maker
/// game's; see [`Core::checkpoints_while_playing`]) saves its checkpoint as
/// it plays, and how soon it tries again when the game cannot be saved.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(60);
const CHECKPOINT_RETRY: Duration = Duration::from_secs(5);

/// Why a game whose core saves only when asked (an RPG Maker game's) has
/// no rewind.
const NO_REWIND: &str = "No rewind: this game saves states only when you ask";

/// A game ready to run: the core plus what is needed to save it.
pub struct Loaded {
    core: Box<dyn Core>,
    /// Each cartridge's ROM file, in the core's cartridge order: one per
    /// player, then any in a second slot. Battery saves sit next to them.
    rom_paths: Vec<PathBuf>,
    /// Each cartridge's own files, named once as the game loads and kept
    /// so for as long as it runs.
    files: Vec<GameFiles>,
    /// Identifies the ROMs in save states.
    rom_crc32: u32,
    /// Where the core's sound goes as it comes, for a core whose sound
    /// runs on a clock of its own (an RPG Maker game's player): it plays
    /// from there, not after each frame.
    live_sound: Option<rpg_maker::audio::Tap>,
    /// Where an RPG Maker or 3DS game keeps the saves it makes itself,
    /// which a checkpoint must never be picked up over once they change.
    own_saves: OwnSaves,
}

/// The saves a game makes itself, outside the app's battery saves.
#[derive(Debug, Clone, Default)]
pub enum OwnSaves {
    /// None: the game's saves are its battery saves, or it has none.
    #[default]
    None,
    /// An RPG Maker game's save files.
    RpgMaker(rpg_maker::SaveFolders),
    /// A 3DS game's save data, on the core's emulated SD card.
    ThreeDs(emu_3ds::SaveData),
}

impl OwnSaves {
    /// The save files there are now.
    fn files(&self) -> Vec<PathBuf> {
        match self {
            Self::None => Vec::new(),
            Self::RpgMaker(folders) => folders.files(),
            Self::ThreeDs(data) => data.files(),
        }
    }

    /// Sets the saves aside, so that the game starts from the very
    /// beginning: each of an RPG Maker game's save files, and each folder
    /// of a 3DS game's save data and extdata on the core's emulated SD
    /// card, is renamed beside itself to end in `.bak`, or `.bak2` and so
    /// on, never replacing an earlier one. Returns where they went. The
    /// game must have stopped.
    ///
    /// # Errors
    ///
    /// Fails if one cannot be renamed; those set aside before stay so.
    fn set_aside(&self) -> std::io::Result<Vec<PathBuf>> {
        let saves = match self {
            Self::None => Vec::new(),
            Self::RpgMaker(folders) => folders.files(),
            Self::ThreeDs(data) => data.folders(),
        };
        let mut kept = Vec::new();
        for save in saves {
            kept.extend(persist::set_aside(&save)?);
        }
        Ok(kept)
    }

    /// What to tell the player of the saves set aside to `kept`.
    fn kept_notice(&self, kept: &[PathBuf]) -> Option<String> {
        match (self, kept) {
            (_, []) => None,
            (Self::ThreeDs(_), _) => Some(
                "Your old save is kept on the 3DS's SD card, in folders renamed to end in .bak"
                    .to_owned(),
            ),
            (_, [one]) => Some(format!("Your old save is kept as {}", file_name(one))),
            (_, many) => Some(format!(
                "Your {} old saves are kept beside the game's, renamed to end in .bak",
                many.len()
            )),
        }
    }
}

impl Loaded {
    /// Static facts about the loaded core.
    pub fn info(&self) -> CoreInfo {
        self.core.info()
    }

    /// Each cartridge's ROM file: one per player, then any in a second
    /// slot.
    pub fn rom_paths(&self) -> &[PathBuf] {
        &self.rom_paths
    }

    /// Each player's ROM file, which names the session's states and
    /// cheats.
    pub fn player_rom_paths(&self) -> &[PathBuf] {
        let players = self.core.info().players;
        &self.rom_paths[..players.min(self.rom_paths.len())]
    }

    /// Each cartridge's own files, in the order of [`Loaded::rom_paths`].
    pub fn files(&self) -> &[GameFiles] {
        &self.files
    }

    /// Where the game keeps the saves it makes itself, which Start Over
    /// sets aside (see [`erase_progress`]).
    pub fn own_saves(&self) -> OwnSaves {
        self.own_saves.clone()
    }

    /// Where the session's save states are: named after the first
    /// player's game, and after the game in its second slot too if it has
    /// one (a DS game with a GBA cartridge).
    pub fn states(&self) -> StateFiles {
        let info = self.core.info();
        // A cartridge past the players' is in the first player's slot.
        let cartridge = (info.players == 1).then(|| self.rom_paths.get(1)).flatten();
        StateFiles::new(
            self.files[0].clone(),
            info.players > 1,
            cartridge.map(PathBuf::as_path),
            info.system,
            self.rom_crc32,
        )
    }
}

/// A ROM file read from disk, with its battery save.
struct RomFile {
    system: System,
    rom: Vec<u8>,
    backup: Option<Vec<u8>>,
}

impl RomFile {
    /// The file as a core's media, its clock starting at `start_time` and
    /// a game for the original Game Boy shown with `dmg_colors`.
    fn media(&self, start_time: i64, dmg_colors: DmgColors) -> Media<'_> {
        Media {
            backup: self.backup.as_deref(),
            start_time,
            dmg_colors,
            ..Media::new(&self.rom)
        }
    }
}

/// The local date and time now, in the form [`Media::start_time`] wants:
/// seconds from 1970-01-01 00:00, with the time-zone offset already added,
/// since the emulated clocks have no time zone of their own.
fn local_clock_seconds() -> i64 {
    chrono::Local::now().naive_local().and_utc().timestamp()
}

/// The local date and time now, for naming screenshots and recordings as
/// macOS names its own: `2026-10-07 at 18.58.12`.
fn capture_stamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d at %H.%M.%S")
        .to_string()
}

/// Reads the ROM at `rom_path` and its battery save at `backup_path`.
fn read_rom(rom_path: &Path, backup_path: &Path) -> anyhow::Result<RomFile> {
    if rom_path.join("Game.ini").is_file() {
        bail!(
            "{} is an RPG Maker game for Windows: it needs an mkxp-z player app in its folder",
            rom_path.display()
        );
    }
    let system = emu_cores::system_for_path(rom_path).with_context(|| {
        format!(
            "{} is not a .gb, .gbc, .gba, .nds or .3ds file, or an RPG Maker game",
            rom_path.display()
        )
    })?;
    let rom = fs::read(rom_path).with_context(|| format!("cannot read {}", rom_path.display()))?;
    let backup = match fs::read(backup_path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", backup_path.display()));
        }
    };
    Ok(RomFile {
        system,
        rom,
        backup,
    })
}

/// A game opened to play: loaded, with what the window is to know of its
/// files, and held for this copy of the app.
pub struct Opened {
    /// The game, ready to run.
    pub loaded: Loaded,
    /// Why its saves cannot be kept, if they cannot (see [`unwritable`]).
    pub unwritable: Option<String>,
    /// Each of its games' locks, to keep for as long as it runs: another
    /// copy of the app is refused the game meanwhile.
    pub locks: Vec<GameLock>,
}

/// Opens the games at `rom_paths` to play, as [`load`] loads them, but
/// only once each is held for this copy of the app: two copies with one
/// game open would each replace the other's saves, as every file is
/// written whole from memory.
///
/// Blocking, as [`load`] is; call it off the UI thread.
///
/// # Errors
///
/// Fails if one of the games is open already, here or in another copy of
/// the app, before anything of it is read, or as [`load`] does.
pub fn open(rom_paths: &[PathBuf], dmg_colors: DmgColors) -> anyhow::Result<Opened> {
    let locks = emu_os::data_dir().map(|data| data.join("ds-emulator").join("Locks"));
    open_with(rom_paths, dmg_colors, locks.as_deref())
}

/// Opens games as [`open`] does, with their lock files in `locks`, or
/// without locks if there is no folder for them.
fn open_with(
    rom_paths: &[PathBuf],
    dmg_colors: DmgColors,
    locks: Option<&Path>,
) -> anyhow::Result<Opened> {
    let locks = match locks {
        Some(folder) => lock_games(folder, rom_paths)?,
        None => Vec::new(),
    };
    let loaded = load(rom_paths, dmg_colors)?;
    let unwritable = unwritable(loaded.files());
    Ok(Opened {
        loaded,
        unwritable,
        locks,
    })
}

/// The files a session gives the game at `path`, as the folder holds
/// things now: an RPG Maker game's are named after its folder, whether
/// `path` is the folder or the player app in it.
pub fn files_of(path: &Path) -> GameFiles {
    match rpg_maker::game_folder(path) {
        Some(game) => GameFiles::of_folder(&game),
        None => GameFiles::of(path),
    }
}

/// Takes the lock of each game at `rom_paths`, with their lock files in
/// `folder`. A game named twice is held once. If the locks cannot be used
/// at all, as when `folder` cannot be written to, the games play without:
/// that is no reason to refuse them.
fn lock_games(folder: &Path, rom_paths: &[PathBuf]) -> anyhow::Result<Vec<GameLock>> {
    let mut held = Vec::new();
    let mut keys = Vec::new();
    for rom in rom_paths {
        let save = files_of(rom).backup();
        let key = persist::backup_key(&save);
        if keys.contains(&key) {
            continue;
        }
        match GameLock::take(folder, &save) {
            Ok(Some(lock)) => held.push(lock),
            Ok(None) => bail!(
                "{} is already open. If another copy of Imago Emulator is playing it, close it there first.",
                file_name(rom)
            ),
            Err(error) => {
                eprintln!(
                    "ds-emulator: no lock on {}: {error}; playing without",
                    file_name(rom)
                );
            }
        }
        keys.push(key);
    }
    Ok(held)
}

/// Reads each ROM and battery save and powers on the right core: one
/// game; a DS game with a GBA game, which goes in the DS's GBA slot; or two
/// games for the same system joined by a link cable. An RPG Maker game's
/// folder, or its player app, starts the game in its hidden player, and a
/// 3DS game starts in its helper process.
///
/// Blocking file I/O, an RPG Maker or 3DS game takes a few seconds to
/// start, and the first 3DS game downloads the 3DS core; call it off the
/// UI thread.
///
/// # Errors
///
/// Fails if a file cannot be read, its system cannot be identified, linked
/// ROMs are for different systems, a 3DS game comes with another game,
/// two games would share a battery save file, or the core rejects them.
pub fn load(rom_paths: &[PathBuf], dmg_colors: DmgColors) -> anyhow::Result<Loaded> {
    if let [path] = rom_paths
        && let Some(game) = rpg_maker::game_folder(path)
    {
        return load_rpg_maker(&game);
    }
    match rom_paths {
        [path] if emu_3ds::is_game(path) => return load_3ds(path),
        _ if rom_paths.iter().any(|path| emu_3ds::is_game(path)) => {
            bail!("A 3DS game plays on its own: open it without another game")
        }
        _ => {}
    }
    let names: Vec<GameFiles> = rom_paths.iter().map(|rom| GameFiles::of(rom)).collect();
    let files = rom_paths
        .iter()
        .zip(&names)
        .map(|(rom, names)| read_rom(rom, &names.backup()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let start_time = local_clock_seconds();
    let core = match files.as_slice() {
        [] => bail!("no ROM given"),
        [one] => emu_cores::create(one.system, &one.media(start_time, dmg_colors))?,
        [first, second] if emu_cores::second_slot(first.system) == Some(second.system) => {
            let slotted = Media {
                second_slot: Some(Cartridge {
                    rom: &second.rom,
                    backup: second.backup.as_deref(),
                }),
                ..first.media(start_time, dmg_colors)
            };
            emu_cores::create(first.system, &slotted)?
        }
        [first, ..] => {
            let system = first.system;
            ensure!(
                files.iter().all(|file| file.system == system),
                "linked games must be for the same system"
            );
            let players: Vec<Media<'_>> = files
                .iter()
                .map(|file| file.media(start_time, dmg_colors))
                .collect();
            emu_cores::create_linked(system, &players)?
        }
    };
    ensure_separate_saves(&*core, rom_paths, &names)?;
    // One ROM keeps the plain CRC so older save states still match.
    let rom_crc32 = match files.as_slice() {
        [one] => crc32fast::hash(&one.rom),
        many => {
            let crcs: Vec<u8> = many
                .iter()
                .flat_map(|file| crc32fast::hash(&file.rom).to_le_bytes())
                .collect();
            crc32fast::hash(&crcs)
        }
    };
    Ok(Loaded {
        core,
        rom_paths: rom_paths.to_vec(),
        files: names,
        rom_crc32,
        live_sound: None,
        own_saves: OwnSaves::None,
    })
}

/// Starts the RPG Maker game in the folder `game` in its hidden player,
/// which is kept in the app's data folder.
fn load_rpg_maker(game: &Path) -> anyhow::Result<Loaded> {
    let players = emu_os::data_dir()
        .context("there is no folder to keep the game's player in")?
        .join("ds-emulator")
        .join("RPG Maker");
    let core = rpg_maker::Mkxp::open(game, &players)?;
    let live_sound = Some(core.sound_tap());
    let own_saves = OwnSaves::RpgMaker(core.save_folders());
    Ok(Loaded {
        core: Box::new(core),
        rom_paths: vec![game.to_owned()],
        files: vec![GameFiles::of_folder(game)],
        rom_crc32: crc32fast::hash(game.as_os_str().as_encoded_bytes()),
        live_sound,
        own_saves,
    })
}

/// Starts the 3DS game at `path` in a helper process, the app's own
/// executable started again, downloading the 3DS core into its data
/// folder first if it is not there yet. An installable title (`.cia`)
/// plays as the executable it holds, written into the games folder the
/// first time; the session is still the `.cia`'s, so its states and
/// checkpoint sit beside it and are known by its checksum.
fn load_3ds(path: &Path) -> anyhow::Result<Loaded> {
    let data = emu_3ds::data_dir().context("there is no folder to keep the 3DS core in")?;
    let helper = std::env::current_exe().context("cannot find the app to start the 3DS core")?;
    let rom_crc32 = emu_3ds::header::identity(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let game = playable_3ds(path)?;
    let core = emu_3ds::ThreeDs::open(&game, &data, &helper)?;
    Ok(Loaded {
        core: Box::new(core),
        rom_paths: vec![path.to_owned()],
        // Named once the executable is written, which may sit beside the
        // `.cia` as its namesake, so the names never change after.
        files: vec![GameFiles::of(path)],
        rom_crc32,
        live_sound: None,
        own_saves: OwnSaves::ThreeDs(emu_3ds::SaveData::of(path, &data)),
    })
}

/// Why the game whose cartridges have `files` cannot keep its saves, if it
/// cannot: a folder they are in refuses new files, as a read-only disk
/// does. Its battery saves, save states and checkpoint would each fail to
/// be written, so the player is told as the game starts.
///
/// Makes and removes a file in each folder; call it off the UI thread.
pub fn unwritable(files: &[GameFiles]) -> Option<String> {
    let mut folders: Vec<&Path> = files.iter().map(GameFiles::folder).collect();
    folders.dedup();
    folders.into_iter().find_map(|folder| {
        let error = persist::check_writable(folder).err()?;
        Some(format!(
            "{} cannot be written to ({error}), so the app cannot keep this game's saves or save states there. Move the game to a folder you can write to.",
            folder.display()
        ))
    })
}

/// Erases a stopped game's progress on disk, for Start Over: removes the
/// session's checkpoint in `states`, and sets aside the battery save at
/// `save` and the saves the game makes itself, `own`, each renamed to end
/// in `.bak`, or `.bak2` and so on, never replacing an earlier one. The
/// game then starts from the very beginning. Returns what to tell the
/// player of where the old saves are, if there were any.
///
/// # Errors
///
/// Fails if a file cannot be removed or renamed; what was done before
/// stays done.
pub fn erase_progress(
    states: &StateFiles,
    save: &Path,
    own: &OwnSaves,
) -> std::io::Result<Option<String>> {
    states.remove(CHECKPOINT)?;
    let battery = persist::set_aside(save)?;
    let kept = own.set_aside()?;
    Ok(match battery {
        Some(battery) => Some(format!("Your old save is kept as {}", file_name(&battery))),
        None => own.kept_notice(&kept),
    })
}

/// The file the 3DS core opens for the game at `path`: the game itself,
/// or for an installable title, the executable it holds, written into the
/// games folder.
fn playable_3ds(path: &Path) -> anyhow::Result<PathBuf> {
    if !emu_3ds::cia::is_cia(path) {
        return Ok(path.to_owned());
    }
    let games = crate::archive::games_dir()
        .context("there is no folder to keep the game's executable in")?;
    Ok(emu_3ds::cia::extract(path, &games)?)
}

/// Fails if two cartridges with battery saves would use one `.sav` file,
/// as one game linked to itself would: each would overwrite the other's
/// save. `files` are the cartridges' own files, in `rom_paths`' order.
fn ensure_separate_saves(
    core: &dyn Core,
    rom_paths: &[PathBuf],
    files: &[GameFiles],
) -> anyhow::Result<()> {
    let saving: Vec<(&PathBuf, PathBuf)> = (0..)
        .zip(rom_paths.iter().zip(files))
        .filter(|(cartridge, _)| core.backup_generation(*cartridge).is_some())
        .map(|(_, (rom, files))| (rom, files.backup()))
        .collect();
    let keys: Vec<String> = saving
        .iter()
        .map(|(_, save)| persist::backup_key(save))
        .collect();
    for (index, (rom, save)) in saving.iter().enumerate() {
        if let Some(other) = (0..index).find(|other| keys[*other] == keys[index]) {
            bail!(
                "{} and {} would both save to {}; rename one of them",
                file_name(saving[other].0),
                file_name(rom),
                file_name(save),
            );
        }
    }
    Ok(())
}

/// `duration` as `m:ss`, or `h:mm:ss` from an hour.
fn minutes_and_seconds(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// A request from the UI to the emulation thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Replaces a player's input from the next frame on, except that a
    /// release waits until the game has seen the press (see `latch.rs`).
    SetInput {
        /// Index into the core's players.
        player: usize,
        /// What that player holds.
        input: Input,
    },
    /// Replaces a player's turbo buttons from the next frame on: each is
    /// pressed for `frames` frames, released for as many, and so on, past
    /// the latch (see `turbo.rs`).
    SetTurbo {
        /// Index into the core's players.
        player: usize,
        /// The buttons whose turbo inputs that player holds.
        buttons: Buttons,
        /// Frames per press, and per gap.
        frames: u8,
    },
    /// Lets go of everything a player holds, turbo buttons and the
    /// microphone too, from the next frame on: at once, without waiting
    /// for the game to see a quick press or for a turbo press to finish.
    /// For when the player's keys and buttons stop reaching the game while
    /// they may still be down.
    ReleaseInput {
        /// Index into the core's players.
        player: usize,
    },
    /// Runs at this speed from the next frame on.
    SetSpeed(Speed),
    /// The interval between the refreshes of the display the window is
    /// on, if known: normal speed keeps frames in step with them.
    SetRefresh(Option<Duration>),
    /// Steps backwards through recent frames while true.
    SetRewind(bool),
    /// Pauses or resumes.
    TogglePause,
    /// Power-cycles the system.
    Reset,
    /// Writes a save state to this slot's file.
    SaveState(u8),
    /// Restores the save state in this slot's file.
    LoadState(u8),
    /// Writes the battery saves, then a save state to this slot's file,
    /// and records beside it which battery saves it goes with, for
    /// [`Command::LoadCheckpoint`].
    SaveCheckpoint(u8),
    /// Restores the save state in this slot's file, as `LoadState` does,
    /// but only while the battery saves on disk are the ones it was saved
    /// with: a newer one, or one copied in, is never rolled back. Does
    /// nothing if the slot is empty.
    LoadCheckpoint(u8),
    /// A request from the debugger window.
    Debug(debugger::Command),
    /// Draws 3D at this many times the native resolution (DS, 3DS), showing
    /// the change at once, even while paused.
    SetRenderScale(u32),
    /// Enlarges the game's own textures so, for cores that can (the 3DS).
    SetTextureUpscaling(emu_core::TextureUpscaling),
    /// Replaces a player's cheats: their ROM patches go to the core at
    /// once, and the rest run after every frame.
    SetCheats {
        /// Index into the core's players.
        player: usize,
        /// The player's cheat list; disabled cheats are skipped.
        cheats: Vec<Cheat>,
    },
    /// Saves the screens as they are now as a PNG next to the first
    /// player's ROM.
    SaveScreenshot,
    /// Starts recording every frame and its sound to a movie next to the
    /// first player's ROM, or finishes the recording under way.
    ToggleRecording,
    /// Starts a Lua script, replacing any running: its main body runs at
    /// once, and its callbacks after every frame.
    RunScript {
        /// The script's file name, for messages.
        name: String,
        /// Its source text.
        source: String,
    },
    /// Stops the script running, and clears what it drew.
    StopScript,
}

/// A notification from the emulation thread to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A new frame is in the shared slot.
    Frame,
    /// A new debugger snapshot is in the shared slot. It has its own
    /// wake-up: the debugger window must update even while the main
    /// window, which takes the frames, is hidden and draws nothing.
    Debug,
    /// Something the user should see briefly, such as "State saved".
    Notice(String),
    /// Something the user must not miss, such as a battery save that
    /// could not be written: shown for longer than a notice.
    Warning(String),
    /// A recording started (true) or finished (false).
    Recording(bool),
    /// What came of [`Command::LoadCheckpoint`] when there was one: true
    /// if it was loaded, false if it was left alone because a battery
    /// save had changed since.
    PickedUp(bool),
    /// The emulation thread stopped after an internal error, saying so
    /// for the user: no more frames come.
    Stopped(String),
}

/// One player's memory in a core, as cheats see it.
struct PlayerMemory<'a> {
    core: &'a mut dyn Core,
    player: usize,
}

impl emu_cheats::Memory for PlayerMemory<'_> {
    fn peek(&self, address: u32) -> Option<u8> {
        self.core.peek(self.player, address)
    }

    fn poke(&mut self, address: u32, value: u8) -> bool {
        self.core.poke(self.player, address, value)
    }

    fn poke_bank(&mut self, bank: u8, address: u32, value: u8) -> bool {
        self.core.poke_bank(self.player, bank, address, value)
    }

    fn poke_io_16(&mut self, address: u32, value: u16) -> bool {
        self.core.poke_io_16(self.player, address, value)
    }
}

/// The most recent frame, shared between the threads.
#[derive(Default)]
struct Latest {
    /// The newest frame's sequence number and screens. Once the window has
    /// taken them, the screens are its old ones, until the next frame.
    frames: Mutex<(u64, Vec<Frame>)>,
    /// What the script drew after that frame, to show over the screens.
    overlay: Mutex<Arc<[Draw]>>,
    /// What the debugger window shows, as of that frame, while it is open.
    debug: Mutex<Option<Arc<Snapshot>>>,
    unseen: AtomicBool,
    debug_unseen: AtomicBool,
    /// The newest frame the window took to draw, and when.
    taken: Mutex<Option<(u64, Instant)>>,
    /// Frames run, or stepped back through while rewinding, so far: what
    /// the speed readout counts.
    frames_shown: AtomicU64,
}

/// A running emulation thread. Dropping it stops the thread and flushes the
/// battery save.
pub struct Emulator {
    commands: Option<Sender<Command>>,
    latest: Arc<Latest>,
    thread: Option<JoinHandle<()>>,
}

impl Emulator {
    /// Starts running `loaded` on a new thread. Events go to `events`;
    /// sound goes to `audio`, if there is an output device.
    pub fn start(
        loaded: Loaded,
        events: async_channel::Sender<Event>,
        audio: Option<AudioSink>,
    ) -> Self {
        let (commands, receiver) = mpsc::channel();
        let latest = Arc::new(Latest::default());
        let players = loaded.core.info().players.max(1);
        let lock = Lock::new(loaded.core.info().frame_rate.frame_duration());
        let sound_on = Arc::new(AtomicBool::new(true));
        let audio = match (&loaded.live_sound, audio) {
            (Some(tap), Some(sink)) => {
                tap.listen(Box::new(sink.play_live(Arc::clone(&sound_on))));
                None
            }
            (_, audio) => audio,
        };
        let runner = Runner {
            saves: Saves::new(&loaded),
            loaded,
            latches: vec![Latch::default(); players],
            turbos: vec![Turbo::default(); players],
            cheats: vec![Vec::new(); players],
            debug: Debugging::default(),
            recorder: None,
            script: None,
            overlay: Arc::default(),
            mid_frame: false,
            paused: false,
            speed: Speed::NORMAL,
            rewinding: false,
            rewind: RewindThread::start(REWIND_BUDGET_BYTES),
            lock,
            recorded_at: None,
            events,
            latest: Arc::clone(&latest),
            unpublished: Vec::new(),
            audio,
            sound_on,
        };
        let thread = thread::Builder::new()
            .name("emulation".to_owned())
            .spawn(move || {
                // Each frame must be ready before the refresh that shows it.
                qos::interactive();
                let _timers = qos::Timers::fine();
                let mut runner = runner;
                let ran = panic::catch_unwind(AssertUnwindSafe(|| runner.run(&receiver)));
                if let Err(panic) = ran {
                    runner.stopped(&*panic);
                }
            })
            .expect("spawning the emulation thread");
        Self {
            commands: Some(commands),
            latest,
            thread: Some(thread),
        }
    }

    /// Sends `command` to the emulation thread.
    pub fn send(&self, command: Command) {
        if let Some(commands) = &self.commands {
            // A send error means the thread already stopped; nothing to do.
            commands.send(command).ok();
        }
    }

    /// Takes the latest frames into `frames` if they are newer than `seen`,
    /// returning their sequence number. The buffers `frames` held go back
    /// to the emulation thread to copy later frames into, so frames must
    /// be taken by one caller, the one that knows which it has seen.
    pub fn take_latest(&self, seen: u64, frames: &mut Vec<Frame>) -> Option<u64> {
        self.latest.unseen.store(false, Ordering::Release);
        let mut latest = self
            .latest
            .frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (sequence, latest_frames) = &mut *latest;
        if *sequence == seen {
            return None;
        }
        std::mem::swap(frames, latest_frames);
        *self
            .latest
            .taken
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((*sequence, Instant::now()));
        Some(*sequence)
    }

    /// What the script drew after the latest frame, in screen pixels.
    pub fn overlay(&self) -> Arc<[Draw]> {
        Arc::clone(
            &self
                .latest
                .overlay
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// How many frames have run, or been stepped back through while
    /// rewinding, since the emulator started.
    pub fn frames_shown(&self) -> u64 {
        self.latest.frames_shown.load(Ordering::Relaxed)
    }

    /// What the debugger window shows, as of the latest frame or debugger
    /// command, while it is open and the core has a debugger.
    pub fn debug_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.latest.debug_unseen.store(false, Ordering::Release);
        self.latest
            .debug
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        // Closing the channel tells the thread to stop after this frame.
        self.commands = None;
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// State owned by the emulation thread.
struct Runner {
    loaded: Loaded,
    /// One per player: what they hold, each press kept long enough for
    /// the game to see it.
    latches: Vec<Latch>,
    /// One per player: the buttons they turbo, counted in frames.
    turbos: Vec<Turbo>,
    /// One list per player.
    cheats: Vec<Vec<Cheat>>,
    /// The debugger's stops, view and run state.
    debug: Debugging,
    /// The recording under way, if any.
    recorder: Option<Recorder>,
    /// The Lua script running alongside the game, if any.
    script: Option<Running>,
    /// What the script drew after the last frame.
    overlay: Arc<[Draw]>,
    /// A debugger stopped the frame under way part-way.
    mid_frame: bool,
    paused: bool,
    /// How fast frames run; rewinding always steps at normal speed.
    speed: Speed,
    rewinding: bool,
    rewind: RewindThread,
    /// Keeps frames at normal speed in step with the display.
    lock: Lock,
    /// When rewind last recorded a frame, if it has since the history was
    /// last cleared.
    recorded_at: Option<Instant>,
    /// The game's battery saves, save states and checkpoint.
    saves: Saves,
    events: async_channel::Sender<Event>,
    latest: Arc<Latest>,
    /// The next frame's screens, copied out of the core before they are
    /// swapped into `latest`, into buffers swapped out of it before.
    unpublished: Vec<Frame>,
    /// Where each frame's sound goes, unless the core's sound plays as it
    /// comes (see [`Loaded::live_sound`]).
    audio: Option<AudioSink>,
    /// Whether sound that plays as it comes should play now.
    sound_on: Arc<AtomicBool>,
}

/// The debugger's state on the emulation thread.
#[derive(Debug, Default)]
struct Debugging {
    /// What the debugger window shows, while it is open.
    view: Option<debugger::View>,
    /// The user's breakpoints and watchpoints.
    stops: emu_core::debug::Stops,
    /// What a paused emulation runs towards: a step or a frame's end.
    pending: Option<Pending>,
    /// Why the emulation last stopped, until it runs on.
    stopped: Option<emu_core::debug::Stop>,
}

/// Copies `screens` into `frames`, into the buffers already there where
/// the sizes match: cloning a frame allocates afresh, and a DS screen
/// drawn with 3D at 4× is 3 MB.
fn copy_screens(frames: &mut Vec<Frame>, screens: &[Frame]) {
    frames.truncate(screens.len());
    for (frame, screen) in frames.iter_mut().zip(screens) {
        if frame.size() == screen.size() {
            frame.pixels_mut().copy_from_slice(screen.pixels());
        } else {
            frame.clone_from(screen);
        }
    }
    let copied = frames.len();
    frames.extend_from_slice(&screens[copied..]);
}

/// What a panic on the emulation thread said.
fn panic_message(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown error")
}

impl Runner {
    fn run(&mut self, commands: &Receiver<Command>) {
        let frame_duration = self.loaded.core.info().frame_rate.frame_duration();
        let mut inputs = Vec::new();
        let mut audio = Vec::new();
        let mut deadline = Instant::now();
        let mut last_flush = Instant::now();
        let mut next_checkpoint = Instant::now() + CHECKPOINT_INTERVAL;

        // While paused: whether a battery save handed over is being waited
        // on, and only heard of, not flushed again.
        let mut hearing = false;

        'running: loop {
            let turn = Instant::now();
            loop {
                // Rewinding and debugger steps work while paused;
                // everything else waits.
                let waiting = self.paused && !self.rewinding && self.debug.pending.is_none();
                let next = if waiting {
                    if hearing {
                        self.hear_saves();
                    } else {
                        // The timed flush waits too, so flush before.
                        self.flush_backup();
                    }
                    hearing = self.saves.writing();
                    if hearing {
                        match commands.recv_timeout(WRITTEN_CHECK_INTERVAL) {
                            Ok(command) => Ok(command),
                            // Round again, to hear how the write went. One
                            // that failed waits for the next flush.
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => Err(TryRecvError::Disconnected),
                        }
                    } else {
                        commands.recv().map_err(|_| TryRecvError::Disconnected)
                    }
                } else {
                    commands.try_recv()
                };
                hearing = false;
                match next {
                    Ok(command) => {
                        self.apply(command);
                        // Before a pause waits for the next command.
                        let on = self.speed == Speed::NORMAL && !self.paused && !self.rewinding;
                        self.sound_on.store(on, Ordering::Relaxed);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'running,
                }
                if waiting && (!self.paused || self.rewinding) {
                    // Don't race to catch up on time spent paused. Only
                    // after a wait: input arriving mid-play must not move
                    // the frame clock, or each key press would delay the
                    // frames after it by however late the thread woke.
                    deadline = deadline.max(Instant::now());
                }
            }

            if self.rewinding {
                let core = &mut self.loaded.core;
                // States in the history came from this core.
                if self.rewind.pop_with(|previous| {
                    core.load_state(previous).ok();
                }) {
                    // The save went back too (see `saves.rs`).
                    self.saves.rewound();
                    self.clear_latches();
                    self.publish();
                    self.latest.frames_shown.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                if !self.mid_frame
                    && let Some(script) = &mut self.script
                {
                    script.begin_frame();
                }
                inputs.clear();
                for (player, (latch, turbo)) in self.latches.iter().zip(&self.turbos).enumerate() {
                    let input = turbo.apply(latch.input());
                    inputs.push(match &self.script {
                        Some(script) => script.input(player, input),
                        None => input,
                    });
                }
                let ran = Instant::now();
                self.lock.started(ran);
                let outcome = self.run_frame(&inputs, &mut audio);
                let run_time = ran.elapsed();
                if let Some(why) = self.loaded.core.ended() {
                    // Kept up, as no more frames come.
                    self.events.try_send(Event::Stopped(why)).ok();
                    break 'running;
                }
                self.mid_frame = !outcome.frame_done;
                if !outcome.frame_done {
                    // A debugger stopped the frame part-way.
                    self.settle(outcome);
                    self.publish();
                    continue;
                }
                self.apply_cheats();
                for latch in &mut self.latches {
                    latch.advance();
                }
                for turbo in &mut self.turbos {
                    turbo.advance();
                }
                // Only normal speed plays sound: rate control absorbs
                // ±0.5%, not a change of speed.
                if self.speed == Speed::NORMAL
                    && let Some(sink) = &mut self.audio
                {
                    sink.set_speed(self.lock.speed(Instant::now()));
                    sink.push(&audio);
                }
                self.record(&audio);
                audio.clear();
                self.settle(outcome);
                // Last, once the frame is played and recorded: a state the
                // script loads replaces it.
                self.script_frame(&inputs);
                self.publish();
                self.latest.frames_shown.fetch_add(1, Ordering::Relaxed);
                if trace::on() {
                    let frame = self.latest.frames.lock().map_or(0, |latest| latest.0);
                    let buttons = inputs.first().map_or(0, |input| input.buttons.bits());
                    let run_time = trace::micros(run_time);
                    trace::event("run", &[frame, u64::from(buttons), run_time]);
                }
                self.record_for_rewind(frame_duration);
            }

            if last_flush.elapsed() >= BACKUP_FLUSH_INTERVAL {
                self.flush_backup();
                last_flush = Instant::now();
            }

            if self.loaded.core.checkpoints_while_playing() && Instant::now() >= next_checkpoint {
                // Unsaid: the game cannot always be saved, as in a battle,
                // and the checkpoint before stays until it can be.
                let wait = match self.save_checkpoint(CHECKPOINT) {
                    Ok(()) => CHECKPOINT_INTERVAL,
                    Err(_) => CHECKPOINT_RETRY,
                };
                next_checkpoint = Instant::now() + wait;
            }

            let speed = if self.rewinding {
                Speed::NORMAL
            } else {
                self.speed
            };
            let taken = *self
                .latest
                .taken
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some((sequence, at)) = taken {
                self.lock.drawn(sequence, at);
                trace::event("margin", &[trace::micros(self.lock.margin_used())]);
            }
            let now = Instant::now();
            trace::event("busy", &[trace::micros(now - turn)]);
            let step = speed
                .frame_step(frame_duration)
                .map(|frame| self.lock.step(now, frame));
            deadline = speed::next_deadline(deadline, now, step);
            speed::sleep_until(deadline);
            trace::event("late", &[trace::micros(deadline.elapsed())]);
        }
        self.shut_down();
    }

    /// Finishes any recording and writes the battery saves, waiting for
    /// them: the last thing the thread does. Returns whether every one is
    /// on disk.
    fn shut_down(&mut self) -> bool {
        if let Some(recorder) = self.recorder.take() {
            self.finish_recording(recorder);
        }
        self.write_saves()
    }

    /// Writes the battery saves that changed and waits for them. Returns
    /// whether every one is now on disk as the game has it.
    fn write_saves(&mut self) -> bool {
        let written = self.saves.write_all(&*self.loaded.core);
        self.say_saves();
        written
    }

    /// After `panic` on this thread: saves what can still be saved, and
    /// tells the UI the game has stopped, and why.
    fn stopped(&mut self, panic: &(dyn Any + Send)) {
        let why = panic_message(panic).to_owned();
        // False too if writing them panicked.
        let saved = panic::catch_unwind(AssertUnwindSafe(|| self.shut_down())).unwrap_or(false);
        let text = if saved {
            format!(
                "The game stopped after an internal error: {why}. Its saves up to then are kept."
            )
        } else {
            format!(
                "The game stopped after an internal error: {why}. Its latest battery save may be lost."
            )
        };
        if self.events.try_send(Event::Stopped(text.clone())).is_err() {
            eprintln!("ds-emulator: {text}");
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::SetInput { player, input } => {
                if let Some(latch) = self.latches.get_mut(player) {
                    latch.set(input);
                }
            }
            Command::SetTurbo {
                player,
                buttons,
                frames,
            } => {
                if let Some(turbo) = self.turbos.get_mut(player) {
                    turbo.set(buttons, frames);
                }
            }
            Command::ReleaseInput { player } => {
                if let Some(latch) = self.latches.get_mut(player) {
                    latch.release();
                }
                if let Some(turbo) = self.turbos.get_mut(player) {
                    turbo.release();
                }
            }
            Command::SetSpeed(speed) => self.speed = speed,
            Command::SetRefresh(refresh) => {
                trace::event("refresh", &[refresh.map_or(0, trace::micros)]);
                self.lock.set_refresh(refresh);
            }
            Command::SetRewind(on) => {
                let can = self.loaded.core.saves_every_frame();
                if on && !can {
                    self.notice(NO_REWIND);
                }
                let rewinding = on && can;
                if rewinding && !self.rewinding {
                    self.saves.rewind_began();
                }
                self.rewinding = rewinding;
            }
            Command::TogglePause => {
                self.paused = !self.paused;
                self.debug.pending = None;
                if !self.paused {
                    self.debug.stopped = None;
                }
                // Show the debugger the change, even while paused.
                self.publish();
                self.notice(if self.paused { "Paused" } else { "Resumed" });
            }
            Command::Reset => {
                self.loaded.core.reset();
                self.clear_rewind();
                self.clear_latches();
                self.debug.stopped = None;
                self.notice("Reset");
            }
            Command::SaveState(slot) => {
                let saved = self.saves.save_state(&*self.loaded.core, slot);
                match saved {
                    Ok(()) => self.notice(format!("Saved state {slot}")),
                    Err(error) => self.notice(format!("Could not save state: {error:#}")),
                }
                // After, so it is what stays on screen.
                self.say_saves();
            }
            Command::Debug(command) => self.debug_command(command),
            Command::SetRenderScale(scale) => {
                self.loaded.core.set_render_scale(scale);
                self.publish();
            }
            Command::SetTextureUpscaling(upscaling) => {
                self.loaded.core.set_texture_upscaling(upscaling);
            }
            Command::SetCheats { player, cheats } => {
                if let Some(slot) = self.cheats.get_mut(player) {
                    // ROM patches live in the core, which applies them to
                    // every read; the rest run after each frame.
                    let patches = emu_cheats::rom_patches(&cheats);
                    self.loaded.core.set_rom_patches(player, &patches);
                    *slot = cheats;
                }
            }
            Command::LoadState(slot) => match self.saves.load_state(&mut *self.loaded.core, slot) {
                Ok(()) => {
                    self.jumped();
                    self.notice(format!("Loaded state {slot}"));
                }
                Err(error) => self.notice(format!("Could not load state: {error:#}")),
            },
            Command::SaveCheckpoint(slot) => {
                if let Err(error) = self.save_checkpoint(slot) {
                    self.notice(format!("Could not save where you left off: {error:#}"));
                }
            }
            Command::LoadCheckpoint(slot) => match self.load_checkpoint(slot) {
                Ok(PickUp::Empty) => {}
                Ok(picked) => {
                    self.jumped();
                    let loaded = picked == PickUp::Loaded;
                    self.events.try_send(Event::PickedUp(loaded)).ok();
                }
                Err(error) => {
                    self.notice(format!("Could not pick up where you left off: {error:#}"));
                }
            },
            Command::SaveScreenshot => self.save_screenshot(),
            Command::ToggleRecording => match self.recorder.take() {
                Some(recorder) => self.finish_recording(recorder),
                None => self.start_recording(),
            },
            Command::RunScript { name, source } => self.start_script(name, &source),
            Command::StopScript => match self.script.take() {
                Some(running) => {
                    self.overlay = Arc::default();
                    self.publish();
                    self.notice(format!("Stopped {}", running.name));
                }
                None => self.notice("No script is running"),
            },
        }
    }

    /// Compiles and starts a script, replacing any running, and shows
    /// what its main body drew at once, even while paused.
    fn start_script(&mut self, name: String, source: &str) {
        self.script = None;
        self.overlay = Arc::default();
        match Script::new(&name, source) {
            Ok(script) => {
                self.notice(format!("Running {name}"));
                let mut running = Running::new(name, script);
                let started = running.script.start(&mut *self.loaded.core);
                self.script = Some(running);
                self.obey_script(started);
            }
            Err(error) => self.notice(scripting::failure(&name, &error)),
        }
        self.publish();
    }

    /// Tells the script the frame that ran with `inputs` has finished.
    fn script_frame(&mut self, inputs: &[Input]) {
        if let Some(running) = &mut self.script {
            let result = running.script.after_frame(&mut *self.loaded.core, inputs);
            self.obey_script(result);
        }
    }

    /// Acts on what the script asked for since it was last asked, then
    /// stops it if `result` says it failed or it has nothing left to do.
    fn obey_script(&mut self, result: Result<(), ScriptError>) {
        let Some(running) = &mut self.script else {
            return;
        };
        let output = running.script.take_output();
        let log = running.take(&output);
        let name = running.name.clone();
        let finished = running.script.finished();
        self.overlay = output.draws.into();
        if let Some(line) = log {
            self.notice(line);
        }
        for request in output.states {
            self.apply(match request {
                StateRequest::Save(slot) => Command::SaveState(slot),
                StateRequest::Load(slot) => Command::LoadState(slot),
            });
        }
        if output.pause && !self.paused {
            self.paused = true;
            self.debug.pending = None;
            self.notice(format!("Paused by {name}"));
        }
        if let Err(error) = result {
            self.script = None;
            self.overlay = Arc::default();
            self.notice(scripting::failure(&name, &error));
        } else if finished {
            self.script = None;
            self.overlay = Arc::default();
            self.notice(format!("{name} finished"));
        }
    }

    /// Lays the screens out as one picture as they are now, and saves it
    /// as a PNG on a thread of its own, which says where.
    fn save_screenshot(&self) {
        let Some(rom) = self.loaded.player_rom_paths().first().cloned() else {
            return;
        };
        let players = self.loaded.core.info().players;
        let picture = capture::compose(self.loaded.core.screens(), players);
        let stamp = capture_stamp();
        let events = self.events.clone();
        let saving = thread::Builder::new()
            .name("screenshot".to_owned())
            .spawn(move || {
                let saved =
                    persist::create_capture(&rom, &stamp, "png").and_then(|(path, mut file)| {
                        file.write_all(&capture::encode_png(&picture))?;
                        Ok(path)
                    });
                let text = match saved {
                    Ok(path) => format!("Saved {}", file_name(&path)),
                    Err(error) => format!("Could not save a screenshot: {error}"),
                };
                events.try_send(Event::Notice(text)).ok();
            });
        if let Err(error) = saving {
            self.notice(format!("Could not save a screenshot: {error}"));
        }
    }

    /// Starts recording from the next frame on.
    fn start_recording(&mut self) {
        let Some(rom) = self.loaded.player_rom_paths().first() else {
            return;
        };
        let info = self.loaded.core.info();
        let started =
            persist::create_capture(rom, &capture_stamp(), "mov").and_then(|(path, file)| {
                Recorder::start(path, file, &info, self.loaded.core.screens())
            });
        match started {
            Ok(recorder) => {
                self.notice(format!("Recording to {}", file_name(recorder.path())));
                self.recorder = Some(recorder);
                self.events.try_send(Event::Recording(true)).ok();
            }
            Err(error) => self.notice(format!("Could not start recording: {error}")),
        }
    }

    /// Hands the frame just run, with the `audio` it made, to the
    /// recording, if one is under way.
    fn record(&mut self, audio: &[emu_core::StereoSample]) {
        let stopped = self
            .recorder
            .as_ref()
            .is_some_and(|recorder| !recorder.push(self.loaded.core.screens(), audio));
        if stopped && let Some(recorder) = self.recorder.take() {
            // The writer stopped after an error, which finishing reports.
            self.finish_recording(recorder);
        }
    }

    /// Finishes `recorder`'s movie and says how it went.
    fn finish_recording(&self, recorder: Recorder) {
        let name = file_name(recorder.path());
        self.events.try_send(Event::Recording(false)).ok();
        match recorder.finish() {
            Ok(recording) => self.notice(format!(
                "Saved {name}: {} of video",
                minutes_and_seconds(recording.duration)
            )),
            Err(error) => self.notice(format!("Recording {name} failed: {error}")),
        }
    }

    /// Records the frame just run for rewind, if one is due at this speed
    /// for a system whose frames last `frame_duration`, and the rewind
    /// thread has caught up.
    fn record_for_rewind(&mut self, frame_duration: Duration) {
        if !self.loaded.core.saves_every_frame() {
            return;
        }
        let now = Instant::now();
        let due = self.recorded_at.is_none_or(|at| {
            rewind::capture_due(self.speed, frame_duration, now.duration_since(at))
        });
        let core = &self.loaded.core;
        if due && self.rewind.push_with(|state| core.save_state(state)) {
            self.recorded_at = Some(now);
        }
    }

    /// Catches up with a state just loaded: forgets what led to the moment
    /// before, and shows the new one, even while paused.
    fn jumped(&mut self) {
        self.clear_rewind();
        self.clear_latches();
        self.debug.stopped = None;
        self.publish();
    }

    /// Forgets the rewind history, after the game jumps to another moment.
    fn clear_rewind(&mut self) {
        self.rewind.clear();
        self.recorded_at = None;
    }

    /// Drops releases still waiting out a press's minimum hold, and turbo
    /// presses being finished, and starts held turbo buttons again with a
    /// press: after the game jumps to another moment, a press from before
    /// would land out of place. Any frame under way is gone too.
    fn clear_latches(&mut self) {
        self.mid_frame = false;
        for latch in &mut self.latches {
            latch.clear();
        }
        for turbo in &mut self.turbos {
            turbo.clear();
        }
    }

    /// Runs each player's enabled cheats, as cheat devices did once a
    /// frame. Frames end at VBlank, where the devices ran them too, and
    /// button codes see the input the frame just ran with.
    fn apply_cheats(&mut self) {
        for (player, cheats) in self.cheats.iter().enumerate() {
            let mut memory = PlayerMemory {
                core: &mut *self.loaded.core,
                player,
            };
            emu_cheats::apply(cheats, &mut memory);
        }
    }

    /// Runs one frame, or what is left of it, checking the debugger's
    /// stops if there are any.
    fn run_frame(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<emu_core::StereoSample>,
    ) -> RunOutcome {
        let mut stops = self.debug.stops.clone();
        if let Some(pending) = self.debug.pending {
            pending.add_to(&mut stops);
        }
        if let Some(running) = &mut self.script {
            // The script's emu.on_exec stops are its own; only the
            // debugger's come back.
            let (outcome, called) =
                running
                    .script
                    .run_frame(&mut *self.loaded.core, inputs, audio, &stops);
            if called.is_err() {
                self.obey_script(called);
            }
            return outcome;
        }
        match self.loaded.core.debugger() {
            Some(debugger) if !stops.is_empty() => debugger.run_until(inputs, audio, &stops),
            _ => {
                self.loaded.core.run_frame(inputs, audio);
                RunOutcome {
                    frame_done: true,
                    stop: None,
                }
            }
        }
    }

    /// Pauses after a run that stopped, or that finished the frame a
    /// "next frame" ran towards.
    fn settle(&mut self, outcome: RunOutcome) {
        if let Some(mut stop) = outcome.stop {
            let Some(debugger) = self.loaded.core.debugger() else {
                return;
            };
            let pc = debugger.pc(stop.cpu);
            // A step over a call ends at the breakpoint after it.
            let returned = Pending::StepOver(Breakpoint {
                cpu: stop.cpu,
                address: pc,
            });
            if stop.reason == StopReason::Breakpoint && self.debug.pending == Some(returned) {
                stop.reason = StopReason::Step;
            }
            if stop.reason != StopReason::Step {
                let cpus = debugger.cpus();
                self.notice(debugger::describe_stop(&stop, &cpus, pc));
            }
            self.paused = true;
            self.debug.pending = None;
            self.debug.stopped = Some(stop);
        } else if outcome.frame_done && self.debug.pending == Some(Pending::NextFrame) {
            self.paused = true;
            self.debug.pending = None;
        }
    }

    /// Acts on a debugger request, and shows the result at once, even
    /// while paused.
    fn debug_command(&mut self, command: debugger::Command) {
        use debugger::Command;

        match command {
            Command::View(view) => self.debug.view = view,
            Command::SetStops {
                breakpoints,
                watchpoints,
            } => {
                self.debug.stops.breakpoints = breakpoints;
                self.debug.stops.watchpoints = watchpoints;
            }
            Command::Pause => {
                self.paused = true;
                self.debug.pending = None;
            }
            Command::Continue => {
                self.paused = false;
                self.debug.pending = None;
                self.debug.stopped = None;
            }
            Command::Step(cpu) => self.run_towards(Pending::Step(cpu)),
            Command::StepOver(cpu) => {
                if let Some(debugger) = self.loaded.core.debugger() {
                    let pending = Pending::step_over(debugger, cpu);
                    self.run_towards(pending);
                }
            }
            Command::NextFrame => self.run_towards(Pending::NextFrame),
            Command::Write {
                cpu,
                address,
                value,
            } => {
                if let Some(debugger) = self.loaded.core.debugger() {
                    debugger.write(cpu, address, value);
                }
            }
        }
        self.publish();
    }

    /// Pauses, then runs towards `pending`.
    fn run_towards(&mut self, pending: Pending) {
        self.paused = true;
        self.debug.pending = Some(pending);
        self.debug.stopped = None;
    }

    /// Whether the emulation runs, and if not, why, for the debugger.
    fn run_state(&self) -> RunState {
        match (self.debug.pending, self.debug.stopped) {
            (Some(_), _) => RunState::Stepping,
            (None, Some(stop)) => RunState::Stopped(stop),
            (None, None) if self.paused => RunState::Paused,
            (None, None) => RunState::Running,
        }
    }

    fn publish(&mut self) {
        // Copied before the lock is taken and swapped in under it, so the
        // window never waits for a copy, nor this thread for the window.
        copy_screens(&mut self.unpublished, self.loaded.core.screens());
        {
            let mut latest = self
                .latest
                .frames
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let (sequence, frames) = &mut *latest;
            *sequence += 1;
            self.lock.published(*sequence, Instant::now());
            std::mem::swap(frames, &mut self.unpublished);
        }
        *self
            .latest
            .overlay
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::clone(&self.overlay);
        let state = self.run_state();
        let snapshot = match (self.debug.view, self.loaded.core.debugger()) {
            (Some(view), Some(debugger)) => {
                Some(Arc::new(debugger::snapshot(debugger, &view, state)))
            }
            _ => None,
        };
        let debugging = snapshot.is_some();
        *self
            .latest
            .debug
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = snapshot;
        if !self.latest.unseen.swap(true, Ordering::AcqRel) {
            self.events.try_send(Event::Frame).ok();
        }
        if debugging && !self.latest.debug_unseen.swap(true, Ordering::AcqRel) {
            self.events.try_send(Event::Debug).ok();
        }
    }

    /// Saves a checkpoint in `slot` (see [`Saves::save_checkpoint`]).
    fn save_checkpoint(&mut self, slot: u8) -> anyhow::Result<()> {
        let saved = self.saves.save_checkpoint(&*self.loaded.core, slot);
        self.say_saves();
        saved
    }

    /// Picks up the checkpoint in `slot` (see [`Saves::load_checkpoint`]).
    fn load_checkpoint(&mut self, slot: u8) -> anyhow::Result<PickUp> {
        self.saves.load_checkpoint(&mut *self.loaded.core, slot)
    }

    /// Hands each cartridge's battery save that changed since the last
    /// flush to its writer, and retries any whose write failed.
    fn flush_backup(&mut self) {
        self.saves.flush(&*self.loaded.core);
        self.say_saves();
    }

    /// Hears how the battery saves handed to their writer went, without
    /// handing over any more.
    fn hear_saves(&mut self) {
        self.saves.hear();
        self.say_saves();
    }

    /// Tells the user what the saves have to say, and last, to stay on
    /// screen, what they warn of, such as a battery save that could not be
    /// written.
    fn say_saves(&mut self) {
        for text in self.saves.take_said() {
            self.notice(text);
        }
        for text in self.saves.take_warned() {
            self.tell(Event::Warning(text));
        }
    }

    /// Shows `text` to the user briefly (see [`Runner::tell`]).
    fn notice(&self, text: impl Into<String>) {
        self.tell(Event::Notice(text.into()));
    }

    /// Sends a notice or a warning to the UI, or, once it has stopped
    /// listening, prints its words, as for a battery save that failed as
    /// the game closed.
    fn tell(&self, event: Event) {
        let sent = self.events.try_send(event);
        if let Err(unsent) = sent
            && let Event::Notice(text) | Event::Warning(text) = unsent.into_inner()
        {
            eprintln!("ds-emulator: {text}");
        }
    }
}

#[cfg(test)]
mod tests;
