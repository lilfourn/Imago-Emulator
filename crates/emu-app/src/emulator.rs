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
//! them, and tells the UI the game has stopped.

use std::any::Any;
use std::fs;
use std::io::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
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
use crate::persist::{self, GameFiles};
use crate::recorder::Recorder;
use crate::refresh::Lock;
use crate::rewind;
use crate::rewind_thread::RewindThread;
use crate::save_writer::SaveWriter;
use crate::scripting::{self, Running};
use crate::speed::{self, Speed};
use crate::trace;
use crate::turbo::Turbo;
use crate::video;

/// How often dirty battery saves are written to disk.
const BACKUP_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Memory for rewind history: about a minute of Game Boy play.
const REWIND_BUDGET_BYTES: usize = 64 << 20;

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

    /// The session's save state file in `slot`.
    fn state_path(&self, slot: u8) -> PathBuf {
        self.files[0].state(slot, self.core.info().players > 1)
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
            "{} is not a .gb, .gbc, .gba or .nds file, or an RPG Maker game",
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

/// Reads each ROM and battery save and powers on the right core: one
/// game; a DS game with a GBA game, which goes in the DS's GBA slot; or two
/// games for the same system joined by a link cable. An RPG Maker game's
/// folder, or its player app, starts the game in its hidden player.
///
/// Blocking file I/O, and an RPG Maker game takes a few seconds to start;
/// call it off the UI thread.
///
/// # Errors
///
/// Fails if a file cannot be read, its system cannot be identified, linked
/// ROMs are for different systems, two games would share a battery save
/// file, or the core rejects them.
pub fn load(rom_paths: &[PathBuf], dmg_colors: DmgColors) -> anyhow::Result<Loaded> {
    if let [path] = rom_paths
        && let Some(game) = rpg_maker::game_folder(path)
    {
        return load_rpg_maker(&game);
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
    })
}

/// Starts the RPG Maker game in the folder `game` in its hidden player,
/// which is kept in the app's data folder.
fn load_rpg_maker(game: &Path) -> anyhow::Result<Loaded> {
    let players = dirs::data_dir()
        .context("there is no folder to keep the game's player in")?
        .join("ds-emulator")
        .join("RPG Maker");
    let core = rpg_maker::Mkxp::open(game, &players)?;
    let live_sound = Some(core.sound_tap());
    Ok(Loaded {
        core: Box::new(core),
        rom_paths: vec![game.to_owned()],
        files: vec![GameFiles::of_folder(game)],
        rom_crc32: crc32fast::hash(game.as_os_str().as_encoded_bytes()),
        live_sound,
    })
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

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
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
    /// Draws 3D at this many times the native resolution (DS), showing
    /// the change at once, even while paused.
    SetRenderScale(u32),
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
            flushed_generations: generations(&*loaded.core, loaded.rom_paths.len()),
            backup_paths: loaded.files.iter().map(GameFiles::backup).collect(),
            writer: SaveWriter::start(),
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
            audio,
            sound_on,
        };
        let thread = thread::Builder::new()
            .name("emulation".to_owned())
            .spawn(move || {
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

    /// Copies the latest frame into `frames` if it is newer than `seen`,
    /// returning its sequence number.
    pub fn take_latest(&self, seen: u64, frames: &mut Vec<Frame>) -> Option<u64> {
        self.latest.unseen.store(false, Ordering::Release);
        let latest = self
            .latest
            .frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (sequence, latest_frames) = &*latest;
        if *sequence == seen {
            return None;
        }
        frames.clone_from(latest_frames);
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
    /// Per cartridge: the backup generation last handed to `writer`, or
    /// `None` if the cartridge has no battery or its last write failed.
    flushed_generations: Vec<Option<u64>>,
    /// Per cartridge: its battery save file.
    backup_paths: Vec<PathBuf>,
    /// Writes the battery saves to disk.
    writer: SaveWriter,
    events: async_channel::Sender<Event>,
    latest: Arc<Latest>,
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

/// What came of picking up from a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickUp {
    /// There is none.
    Empty,
    /// It was loaded.
    Loaded,
    /// A battery save changed after it was saved, so it was left alone.
    SavesChanged,
}

/// Each of the first `cartridges` cartridges' backup generation in
/// `core`, taken to be on disk already: 0 for one without a battery.
fn generations(core: &dyn Core, cartridges: usize) -> Vec<Option<u64>> {
    (0..cartridges)
        .map(|cartridge| Some(core.backup_generation(cartridge).unwrap_or(0)))
        .collect()
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

        'running: loop {
            let turn = Instant::now();
            loop {
                // Rewinding and debugger steps work while paused;
                // everything else waits.
                let waiting = self.paused && !self.rewinding && self.debug.pending.is_none();
                let next = if waiting {
                    // The timed flush waits too, so flush before.
                    self.flush_backup();
                    commands.recv().map_err(|_| TryRecvError::Disconnected)
                } else {
                    commands.try_recv()
                };
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
        self.flush_backup();
        self.writer.wait();
        self.take_written();
        // A failed write leaves its cartridge unflushed (see `take_written`).
        (0..self.flushed_generations.len()).all(|cartridge| {
            match self.loaded.core.backup_generation(cartridge) {
                Some(generation) => self.flushed_generations[cartridge] == Some(generation),
                None => true,
            }
        })
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
            Command::SetRefresh(refresh) => self.lock.set_refresh(refresh),
            Command::SetRewind(on) => {
                let can = self.loaded.core.saves_every_frame();
                if on && !can {
                    self.notice(NO_REWIND);
                }
                self.rewinding = on && can;
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
            Command::SaveState(slot) => match self.save_state(slot) {
                Ok(()) => self.notice(format!("Saved state {slot}")),
                Err(error) => self.notice(format!("Could not save state: {error:#}")),
            },
            Command::Debug(command) => self.debug_command(command),
            Command::SetRenderScale(scale) => {
                self.loaded.core.set_render_scale(scale);
                self.publish();
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
            Command::LoadState(slot) => match self.load_state(slot) {
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
        {
            let mut latest = self
                .latest
                .frames
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let (sequence, frames) = &mut *latest;
            *sequence += 1;
            self.lock.published(*sequence, Instant::now());
            let screens = self.loaded.core.screens();
            if frames.len() == screens.len() {
                frames.clone_from_slice(screens);
            } else {
                *frames = screens.to_vec();
            }
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

    fn save_state(&self, slot: u8) -> anyhow::Result<()> {
        let info = self.loaded.core.info();
        let mut payload = Vec::new();
        self.loaded
            .core
            .try_save_state(&mut payload)
            .map_err(anyhow::Error::msg)?;
        let header = emu_state::Header {
            system: info.system,
            state_version: info.state_version,
            rom_crc32: self.loaded.rom_crc32,
        };
        // At the screen's own size, whatever the 3D resolution.
        let screen = video::shrink(&self.loaded.core.screens()[0], info.screens[0]);
        let size = screen.size();
        let rgb: Vec<u8> = screen
            .pixels()
            .iter()
            .flat_map(|pixel| {
                let [b, g, r, _] = pixel.to_le_bytes();
                [r, g, b]
            })
            .collect();
        let thumbnail = emu_state::Thumbnail {
            width: size.width,
            height: size.height,
            rgb: &rgb,
        };
        let path = self.loaded.state_path(slot);
        persist::write_atomic(
            &path,
            &emu_state::encode(&header, &payload, Some(thumbnail)),
        )
        .with_context(|| format!("cannot write {}", path.display()))
    }

    fn load_state(&mut self, slot: u8) -> anyhow::Result<()> {
        let path = self.loaded.state_path(slot);
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let emu_state::Decoded {
            header, payload, ..
        } = emu_state::decode(&bytes)?;
        let info = self.loaded.core.info();
        ensure!(
            header.system == info.system,
            "it is a {} state",
            header.system
        );
        ensure!(
            header.rom_crc32 == self.loaded.rom_crc32,
            "it was saved from a different ROM"
        );
        ensure!(
            header.state_version == info.state_version,
            "it is state version {}, this build reads {}",
            header.state_version,
            info.state_version,
        );
        Ok(self.loaded.core.load_state(payload)?)
    }

    /// Writes the battery saves and waits for them, then saves a state to
    /// `slot` and records beside it the battery saves on disk, which
    /// `load_checkpoint` checks. Nothing is saved unless every battery
    /// save was written, and the record comes last: should the state not
    /// be written, the one before keeps the record that matches it.
    fn save_checkpoint(&mut self, slot: u8) -> anyhow::Result<()> {
        // A game that saves only when asked saves itself.
        if !self.loaded.core.saves_every_frame() {
            return Ok(());
        }
        // A record of older saves on disk would let a later start load
        // this state over them once they could be written again.
        ensure!(self.write_saves(), "the battery save could not be written");
        self.save_state(slot)?;
        let state = self.loaded.state_path(slot);
        let record = persist::saves_record_path(&state);
        let saves = persist::saves_fingerprint(&self.backup_paths)
            .context("cannot read the battery saves")?;
        persist::write_atomic(&record, saves.as_bytes())
            .with_context(|| format!("cannot write {}", record.display()))
    }

    /// Loads the state in `slot` if the battery saves on disk are those
    /// its record says it was saved with. A state saved before records
    /// were kept is loaded only if its battery saves are the same as the
    /// game's now, and otherwise undone.
    fn load_checkpoint(&mut self, slot: u8) -> anyhow::Result<PickUp> {
        let state = self.loaded.state_path(slot);
        if !state.exists() || !self.loaded.core.saves_every_frame() {
            return Ok(PickUp::Empty);
        }
        let record = persist::saves_record_path(&state);
        match fs::read_to_string(&record) {
            Ok(recorded) => {
                let saves = persist::saves_fingerprint(&self.backup_paths)
                    .context("cannot read the battery saves")?;
                if recorded != saves {
                    return Ok(PickUp::SavesChanged);
                }
                self.load_state(slot)?;
                Ok(PickUp::Loaded)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let started = self.backups();
                let mut undo = Vec::new();
                self.loaded.core.save_state(&mut undo);
                self.load_state(slot)?;
                if self.backups() == started {
                    return Ok(PickUp::Loaded);
                }
                let undone = self.loaded.core.load_state(&undo);
                // The battery saves are back as on disk: nothing to write.
                self.flushed_generations = generations(&*self.loaded.core, self.backup_paths.len());
                undone.context("cannot undo loading it")?;
                Ok(PickUp::SavesChanged)
            }
            Err(error) => Err(error).with_context(|| format!("cannot read {}", record.display())),
        }
    }

    /// Each cartridge's battery save as it is now.
    fn backups(&self) -> Vec<Vec<u8>> {
        (0..self.backup_paths.len())
            .map(|cartridge| {
                let mut backup = Vec::new();
                self.loaded.core.save_backup(cartridge, &mut backup);
                backup
            })
            .collect()
    }

    /// Hands each cartridge's battery save that changed since the last
    /// flush to the writer, and retries any whose write failed.
    fn flush_backup(&mut self) {
        self.take_written();
        for cartridge in 0..self.flushed_generations.len() {
            let Some(generation) = self.loaded.core.backup_generation(cartridge) else {
                continue;
            };
            if self.flushed_generations[cartridge] == Some(generation) {
                continue;
            }
            let mut backup = Vec::new();
            self.loaded.core.save_backup(cartridge, &mut backup);
            let path = self.backup_paths[cartridge].clone();
            self.writer.write(cartridge, generation, path, backup);
            self.flushed_generations[cartridge] = Some(generation);
        }
    }

    /// Notes the battery save writes finished since last asked: a failed
    /// one is said, and written again at the next flush unless a newer
    /// one is on its way.
    fn take_written(&mut self) {
        for written in self.writer.finished() {
            let Err(error) = written.result else {
                continue;
            };
            self.notice(format!(
                "Could not write {}: {error}",
                written.path.display()
            ));
            let flushed = &mut self.flushed_generations[written.cartridge];
            if *flushed == Some(written.generation) {
                *flushed = None;
            }
        }
    }

    /// Shows `text` to the user, or, once the UI has stopped listening,
    /// prints it, as for a battery save that failed as the game closed.
    fn notice(&self, text: impl Into<String>) {
        let sent = self.events.try_send(Event::Notice(text.into()));
        if let Err(unsent) = sent
            && let Event::Notice(text) = unsent.into_inner()
        {
            eprintln!("ds-emulator: {text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use emu_core::{
        Button, Buttons, CoreInfo, FrameRate, RomPatch, ScreenSize, StateError, StereoSample,
        System, TouchPoint,
    };

    use emu_core::Debugger;
    use emu_core::debug::{CpuInfo, Instruction, Register, Stops};

    use super::*;
    use crate::latch::{BUTTON_FRAMES, TOUCH_FRAMES};

    const SCREENS: [ScreenSize; 1] = [ScreenSize::new(1, 1)];

    /// A core whose single pixel is its frame counter.
    struct Counter {
        frames: [Frame; 1],
    }

    impl Counter {
        fn count(&self) -> u32 {
            self.frames[0].pixels()[0]
        }

        fn set(&mut self, count: u32) {
            self.frames[0].pixels_mut()[0] = count;
        }
    }

    impl Core for Counter {
        fn info(&self) -> CoreInfo {
            CoreInfo {
                system: System::Gb,
                screens: &SCREENS,
                touch_screen: None,
                players: 1,
                // 1000 frames per second keeps the test fast.
                frame_rate: FrameRate::new(1000, 1),
                audio_sample_rate: 48_000,
                state_version: 1,
            }
        }

        fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {
            self.set(self.count() + 1);
        }

        fn screens(&self) -> &[Frame] {
            &self.frames
        }

        fn reset(&mut self) {
            self.set(0);
        }

        fn save_state(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.count().to_le_bytes());
        }

        fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
            let bytes = state.try_into().map_err(|_| StateError::Truncated)?;
            self.set(u32::from_le_bytes(bytes));
            Ok(())
        }

        /// The counter is the only memory, at addresses 0-3.
        fn peek(&self, _: usize, address: u32) -> Option<u8> {
            self.count().to_le_bytes().get(address as usize).copied()
        }

        fn poke(&mut self, _: usize, address: u32, value: u8) -> bool {
            let mut bytes = self.count().to_le_bytes();
            let Some(byte) = bytes.get_mut(address as usize) else {
                return false;
            };
            *byte = value;
            self.set(u32::from_le_bytes(bytes));
            true
        }

        fn debugger(&mut self) -> Option<&mut dyn Debugger> {
            Some(self)
        }
    }

    /// Gives a test core a debugger with one CPU, whose memory is the
    /// core's `peek` and `poke` and which runs frames whole, so tests can
    /// watch memory through the debugger's snapshot.
    macro_rules! peek_debugger {
        ($core:ty) => {
            impl Debugger for $core {
                fn cpus(&self) -> Vec<CpuInfo> {
                    vec![CpuInfo {
                        name: "TEST",
                        player: 0,
                    }]
                }
                fn registers(&self, _: usize) -> Vec<Register> {
                    Vec::new()
                }
                fn status(&self, _: usize) -> String {
                    String::new()
                }
                fn pc(&self, _: usize) -> u32 {
                    0
                }
                fn read(&self, _: usize, address: u32) -> Option<u8> {
                    self.peek(0, address)
                }
                fn write(&mut self, _: usize, address: u32, value: u8) -> bool {
                    self.poke(0, address, value)
                }
                fn disassemble(&self, _: usize, address: u32) -> Instruction {
                    Instruction {
                        address,
                        bytes: vec![0],
                        text: String::new(),
                        call: false,
                    }
                }
                fn run_until(
                    &mut self,
                    inputs: &[Input],
                    audio: &mut Vec<StereoSample>,
                    _: &Stops,
                ) -> RunOutcome {
                    self.run_frame(inputs, audio);
                    RunOutcome {
                        frame_done: true,
                        stop: None,
                    }
                }
            }
        };
    }

    peek_debugger!(Counter);
    peek_debugger!(Keypad);

    /// Opens the debugger's view, with its memory from `address`.
    fn watch_memory(emulator: &Emulator, address: u32) {
        let view = debugger::View {
            memory: address,
            ..debugger::View::default()
        };
        emulator.send(Command::Debug(debugger::Command::View(Some(view))));
    }

    /// Waits for the debugger's snapshot to pass `test`.
    fn wait_for(emulator: &Emulator, test: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(snapshot) = emulator.debug_snapshot().filter(|shown| test(shown)) {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "the debugger never showed it");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn start(rom_path: PathBuf) -> (Emulator, async_channel::Receiver<Event>) {
        let core = Box::new(Counter {
            frames: [Frame::new(SCREENS[0])],
        });
        let loaded = Loaded {
            core,
            files: vec![GameFiles::of(&rom_path)],
            rom_paths: vec![rom_path],
            rom_crc32: 0,
            live_sound: None,
        };
        let (events, receiver) = async_channel::unbounded();
        (Emulator::start(loaded, events, None), receiver)
    }

    fn latest_count(emulator: &Emulator) -> u32 {
        let mut frames = Vec::new();
        emulator.take_latest(u64::MAX, &mut frames);
        frames[0].pixels()[0]
    }

    /// Every frame's inputs, one per player, as a core saw them.
    type Seen = Arc<Mutex<Vec<Vec<Input>>>>;

    /// A two-player core that records every frame's inputs.
    struct Recorder {
        seen: Seen,
        frames: [Frame; 1],
    }

    impl Core for Recorder {
        fn info(&self) -> CoreInfo {
            CoreInfo {
                system: System::Gba,
                screens: &SCREENS,
                touch_screen: None,
                players: 2,
                frame_rate: FrameRate::new(1000, 1),
                audio_sample_rate: 48_000,
                state_version: 1,
            }
        }

        fn run_frame(&mut self, inputs: &[Input], _: &mut Vec<StereoSample>) {
            self.seen.lock().expect("unpoisoned").push(inputs.to_vec());
        }

        fn screens(&self) -> &[Frame] {
            &self.frames
        }

        fn reset(&mut self) {}

        fn save_state(&self, _: &mut Vec<u8>) {}

        fn load_state(&mut self, _: &[u8]) -> Result<(), StateError> {
            Ok(())
        }
    }

    fn start_recorder() -> (Emulator, Seen) {
        let seen = Seen::default();
        let loaded = Loaded {
            core: Box::new(Recorder {
                seen: Arc::clone(&seen),
                frames: [Frame::new(SCREENS[0])],
            }),
            files: vec![GameFiles::of(&PathBuf::from("/nonexistent/recorder.gba"))],
            rom_paths: vec![PathBuf::from("/nonexistent/recorder.gba")],
            rom_crc32: 0,
            live_sound: None,
        };
        let (events, _) = async_channel::unbounded();
        (Emulator::start(loaded, events, None), seen)
    }

    /// Waits for the emulator to run `count` more frames.
    fn run_frames(seen: &Seen, count: usize) {
        let target = seen.lock().expect("unpoisoned").len() + count;
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.lock().expect("unpoisoned").len() < target {
            assert!(Instant::now() < deadline, "frames stopped running");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// The number of recorded frames in which `player`'s input passed
    /// `test`.
    fn frames_where(seen: &Seen, player: usize, test: impl Fn(&Input) -> bool) -> usize {
        let seen = seen.lock().expect("unpoisoned");
        seen.iter().filter(|inputs| test(&inputs[player])).count()
    }

    const TAP: Input = Input {
        buttons: Buttons::NONE,
        touch: Some(TouchPoint { x: 10, y: 20 }),
        microphone: 0,
    };

    #[test]
    fn a_press_and_release_between_two_frames_still_reaches_the_game() {
        let (emulator, seen) = start_recorder();
        let a = Input {
            buttons: [Button::A].into_iter().collect(),
            ..Input::default()
        };
        // Nothing runs while paused, so every press and release below
        // arrives between the same two frames, as a trackpad's tap can.
        emulator.send(Command::TogglePause);
        let release = Input::default();
        for (player, input) in [(0, TAP), (0, release), (1, a), (1, release)] {
            emulator.send(Command::SetInput { player, input });
        }
        emulator.send(Command::TogglePause);
        run_frames(&seen, 10);
        let touched = frames_where(&seen, 0, |input| input.touch.is_some());
        let pressed = frames_where(&seen, 1, |input| input.buttons.contains(Button::A));
        assert_eq!(
            (touched, pressed),
            (usize::from(TOUCH_FRAMES), usize::from(BUTTON_FRAMES))
        );
        // Each player saw only their own.
        assert_eq!(frames_where(&seen, 0, |input| !input.buttons.is_empty()), 0);
        assert_eq!(frames_where(&seen, 1, |input| input.touch.is_some()), 0);
    }

    #[test]
    fn turbo_alternates_frame_by_frame_past_the_latch() {
        let (emulator, seen) = start_recorder();
        let a: Buttons = [Button::A].into_iter().collect();
        emulator.send(Command::SetTurbo {
            player: 1,
            buttons: a,
            frames: 1,
        });
        run_frames(&seen, 10);
        let held_until = seen.lock().expect("unpoisoned").len();
        emulator.send(Command::SetTurbo {
            player: 1,
            buttons: Buttons::NONE,
            frames: 1,
        });
        // A press under way finishes; then nothing.
        run_frames(&seen, 3);
        let released_from = seen.lock().expect("unpoisoned").len();
        run_frames(&seen, 5);
        let seen_now = seen.lock().expect("unpoisoned").clone();
        let pressed: Vec<bool> = seen_now[..held_until]
            .iter()
            .map(|inputs| inputs[1].buttons.contains(Button::A))
            .collect();
        // Frames may run before the command arrives.
        let start = pressed.iter().position(|a| *a).expect("turbo pressed A");
        let turbo = &pressed[start..];
        assert!(
            turbo.windows(2).all(|pair| pair[0] != pair[1]),
            "a one-frame press is not held for the latch's minimum: {turbo:?}"
        );
        let after = &seen_now[released_from..];
        assert!(after.iter().all(|inputs| inputs[1].buttons.is_empty()));
        assert_eq!(frames_where(&seen, 0, |input| !input.buttons.is_empty()), 0);
    }

    #[test]
    fn reset_drops_a_release_still_waiting() {
        let (emulator, seen) = start_recorder();
        emulator.send(Command::TogglePause);
        emulator.send(Command::SetInput {
            player: 0,
            input: TAP,
        });
        emulator.send(Command::SetInput {
            player: 0,
            input: Input::default(),
        });
        emulator.send(Command::Reset);
        emulator.send(Command::TogglePause);
        run_frames(&seen, 10);
        assert_eq!(frames_where(&seen, 0, |input| input.touch.is_some()), 0);
    }

    #[test]
    fn release_input_lets_go_of_one_player_at_once() {
        let (emulator, seen) = start_recorder();
        let held = |buttons: &[Button]| Input {
            buttons: buttons.iter().copied().collect(),
            ..Input::default()
        };
        emulator.send(Command::TogglePause);
        // Player 1 taps A, holds Left and a turbo B; player 0 holds A.
        for input in [held(&[Button::A]), held(&[]), held(&[Button::Left])] {
            emulator.send(Command::SetInput { player: 1, input });
        }
        emulator.send(Command::SetTurbo {
            player: 1,
            buttons: [Button::B].into_iter().collect(),
            frames: 3,
        });
        emulator.send(Command::SetInput {
            player: 0,
            input: held(&[Button::A]),
        });
        emulator.send(Command::ReleaseInput { player: 1 });
        emulator.send(Command::TogglePause);
        run_frames(&seen, 10);
        assert_eq!(
            frames_where(&seen, 1, |input| *input != Input::default()),
            0
        );
        assert!(frames_where(&seen, 0, |input| input.buttons.contains(Button::A)) >= 10);
    }

    #[test]
    fn counts_the_frames_shown() {
        let (emulator, events) = start(PathBuf::from("/nonexistent/counter.gb"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while emulator.frames_shown() < 10 {
            assert!(Instant::now() < deadline, "frames stopped running");
            thread::sleep(Duration::from_millis(1));
        }
        emulator.send(Command::TogglePause);
        wait_for_notice(&events, "Paused");
        // Paused: the count stands still, and matches the game's own,
        // which counts up from opaque black.
        let shown = emulator.frames_shown();
        assert_eq!(u64::from(latest_count(&emulator) & 0x00FF_FFFF), shown);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(emulator.frames_shown(), shown);
    }

    #[test]
    fn rewind_steps_back_through_frames() {
        let (emulator, _events) = start(PathBuf::from("/nonexistent/counter.gb"));
        thread::sleep(Duration::from_millis(200));
        emulator.send(Command::SetRewind(true));
        thread::sleep(Duration::from_millis(20));
        let rewinding = latest_count(&emulator);
        thread::sleep(Duration::from_millis(50));
        let rewound = latest_count(&emulator);
        assert!(rewound < rewinding, "{rewound} should be below {rewinding}");

        emulator.send(Command::SetRewind(false));
        thread::sleep(Duration::from_millis(50));
        assert!(latest_count(&emulator) > rewound);
    }

    #[test]
    fn cheats_run_after_every_frame() {
        let (emulator, _events) = start(PathBuf::from("/nonexistent/counter.gb"));
        let cheats = emu_cheats::parse(System::Gb, "[Hold]\n0000:00\n").expect("valid");
        emulator.send(Command::SetCheats { player: 0, cheats });
        watch_memory(&emulator, 0);
        thread::sleep(Duration::from_millis(50));
        // Each frame adds one; the cheat then zeroes the low byte.
        let snapshot = emulator.debug_snapshot().expect("watching");
        assert_eq!(snapshot.memory[0], Some(0));
    }

    /// A GBA-like core with KEYINPUT at 0x04000130 and one byte of RAM at
    /// 0x02000000, which every frame clears.
    struct Keypad {
        keys: u16,
        ram: u8,
        frames: [Frame; 1],
    }

    impl Core for Keypad {
        fn info(&self) -> CoreInfo {
            CoreInfo {
                system: System::Gba,
                screens: &SCREENS,
                touch_screen: None,
                players: 1,
                frame_rate: FrameRate::new(1000, 1),
                audio_sample_rate: 48_000,
                state_version: 1,
            }
        }

        fn run_frame(&mut self, inputs: &[Input], _: &mut Vec<StereoSample>) {
            let input = inputs.first().copied().unwrap_or_default();
            self.keys = !input.buttons.bits() & 0x03FF;
            self.ram = 0;
        }

        fn screens(&self) -> &[Frame] {
            &self.frames
        }

        fn reset(&mut self) {}

        fn save_state(&self, _: &mut Vec<u8>) {}

        fn load_state(&mut self, _: &[u8]) -> Result<(), StateError> {
            Ok(())
        }

        fn peek(&self, _: usize, address: u32) -> Option<u8> {
            match address {
                0x0200_0000 => Some(self.ram),
                0x0400_0130 => Some(self.keys as u8),
                0x0400_0131 => Some((self.keys >> 8) as u8),
                _ => None,
            }
        }

        fn poke(&mut self, _: usize, address: u32, value: u8) -> bool {
            let ram = address == 0x0200_0000;
            if ram {
                self.ram = value;
            }
            ram
        }

        fn debugger(&mut self) -> Option<&mut dyn Debugger> {
            Some(self)
        }
    }

    #[test]
    fn button_cheats_see_the_frame_s_own_input() {
        let loaded = Loaded {
            core: Box::new(Keypad {
                keys: 0x03FF,
                ram: 0,
                frames: [Frame::new(SCREENS[0])],
            }),
            files: vec![GameFiles::of(&PathBuf::from("/nonexistent/keypad.gba"))],
            rom_paths: vec![PathBuf::from("/nonexistent/keypad.gba")],
            rom_crc32: 0,
            live_sound: None,
        };
        let (events, _events) = async_channel::unbounded();
        let emulator = Emulator::start(loaded, events, None);
        // CodeBreaker: if A is held, write 1.
        let cheats =
            emu_cheats::parse(System::Gba, "D0000020 0001\n32000000 0001\n").expect("valid");
        emulator.send(Command::SetCheats { player: 0, cheats });
        watch_memory(&emulator, 0x0200_0000);
        emulator.send(Command::SetInput {
            player: 0,
            input: Input {
                buttons: [Button::A].into_iter().collect(),
                ..Input::default()
            },
        });
        thread::sleep(Duration::from_millis(50));
        // The frame cleared the byte; the cheat, seeing A, set it again.
        let snapshot = emulator.debug_snapshot().expect("watching");
        assert_eq!(snapshot.memory[0], Some(1));
    }

    /// A Game Boy running `program` at 0x0100 on the emulation thread.
    fn start_game_boy(program: &[u8]) -> (Emulator, async_channel::Receiver<Event>) {
        let mut rom = vec![0; 0x8000];
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        let core = emu_cores::create(System::Gb, &Media::new(&rom)).expect("valid rom");
        let loaded = Loaded {
            core,
            files: vec![GameFiles::of(&PathBuf::from("/nonexistent/debugged.gb"))],
            rom_paths: vec![PathBuf::from("/nonexistent/debugged.gb")],
            rom_crc32: 0,
            live_sound: None,
        };
        let (events, receiver) = async_channel::unbounded();
        (Emulator::start(loaded, events, None), receiver)
    }

    fn debug(emulator: &Emulator, command: debugger::Command) {
        emulator.send(Command::Debug(command));
    }

    #[test]
    fn breakpoints_pause_mid_frame_and_steps_run_one_instruction() {
        // LD A, 0x42; LD (0xC000), A; INC A; NOP; JR -4 (back to INC A)
        let program = [0x3E, 0x42, 0xEA, 0x00, 0xC0, 0x3C, 0x00, 0x18, 0xFC];
        let (emulator, _events) = start_game_boy(&program);
        watch_memory(&emulator, 0xC000);
        let at_nop = Breakpoint {
            cpu: 0,
            address: 0x0106,
        };
        debug(
            &emulator,
            debugger::Command::SetStops {
                breakpoints: vec![at_nop],
                watchpoints: Vec::new(),
            },
        );
        let stopped = wait_for(&emulator, |shown| {
            matches!(shown.state, RunState::Stopped(_))
        });
        assert_eq!((stopped.pc, stopped.memory[0]), (0x0106, Some(0x42)));
        assert!(stopped.code.iter().any(|line| line.address == 0x0106));
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            emulator.debug_snapshot().expect("open").pc,
            0x0106,
            "still there"
        );

        debug(&emulator, debugger::Command::Step(0));
        let stepped = wait_for(&emulator, |shown| shown.pc == 0x0107);
        let step = emu_core::debug::Stop {
            cpu: 0,
            reason: StopReason::Step,
        };
        assert_eq!(stepped.state, RunState::Stopped(step));

        debug(
            &emulator,
            debugger::Command::Write {
                cpu: 0,
                address: 0xC000,
                value: 0x99,
            },
        );
        wait_for(&emulator, |shown| shown.memory[0] == Some(0x99));

        // Round the loop to the breakpoint again.
        debug(&emulator, debugger::Command::Continue);
        let again = wait_for(&emulator, |shown| shown.pc == 0x0106);
        assert!(
            matches!(again.state, RunState::Stopped(stop) if stop.reason == StopReason::Breakpoint)
        );
    }

    #[test]
    fn stepping_over_a_call_stops_after_it_returns() {
        // CALL 0x0150; JR -5 (back to the CALL); and at 0x0150, INC A; RET.
        let mut program = vec![0; 0x52];
        program[..5].copy_from_slice(&[0xCD, 0x50, 0x01, 0x18, 0xFB]);
        program[0x50..].copy_from_slice(&[0x3C, 0xC9]);
        let (emulator, _events) = start_game_boy(&program);
        watch_memory(&emulator, 0xC000);
        debug(
            &emulator,
            debugger::Command::SetStops {
                breakpoints: vec![Breakpoint {
                    cpu: 0,
                    address: 0x0100,
                }],
                watchpoints: Vec::new(),
            },
        );
        wait_for(&emulator, |shown| {
            matches!(shown.state, RunState::Stopped(_))
        });
        debug(&emulator, debugger::Command::StepOver(0));
        let returned = wait_for(&emulator, |shown| shown.pc == 0x0103);
        let step = emu_core::debug::Stop {
            cpu: 0,
            reason: StopReason::Step,
        };
        assert_eq!(returned.state, RunState::Stopped(step));
    }

    #[test]
    fn next_frame_runs_one_frame_then_pauses() {
        let (emulator, _events) = start(PathBuf::from("/nonexistent/counter.gb"));
        watch_memory(&emulator, 0);
        debug(&emulator, debugger::Command::Pause);
        let paused = wait_for(&emulator, |shown| shown.state == RunState::Paused);
        let before = paused.memory[0].expect("readable");
        debug(&emulator, debugger::Command::NextFrame);
        let after = wait_for(&emulator, |shown| {
            shown.state == RunState::Paused && shown.memory[0] != Some(before)
        });
        assert_eq!(after.memory[0], Some(before.wrapping_add(1)));
        thread::sleep(Duration::from_millis(20));
        let later = emulator.debug_snapshot().expect("open");
        assert_eq!(later.memory[0], after.memory[0], "still paused");
    }

    /// Every `set_rom_patches` call a core received.
    type PatchLog = Arc<Mutex<Vec<(usize, Vec<RomPatch>)>>>;

    /// A Game Boy-like core that records the ROM patches it is given.
    struct Patchable {
        log: PatchLog,
        frames: [Frame; 1],
    }

    impl Core for Patchable {
        fn info(&self) -> CoreInfo {
            CoreInfo {
                system: System::Gb,
                screens: &SCREENS,
                touch_screen: None,
                players: 1,
                frame_rate: FrameRate::new(1000, 1),
                audio_sample_rate: 48_000,
                state_version: 1,
            }
        }

        fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {}

        fn screens(&self) -> &[Frame] {
            &self.frames
        }

        fn reset(&mut self) {}

        fn save_state(&self, _: &mut Vec<u8>) {}

        fn load_state(&mut self, _: &[u8]) -> Result<(), StateError> {
            Ok(())
        }

        fn set_rom_patches(&mut self, player: usize, patches: &[RomPatch]) {
            let mut log = self.log.lock().expect("unpoisoned");
            log.push((player, patches.to_vec()));
        }
    }

    #[test]
    fn cheat_lists_hand_their_rom_patches_to_the_core() {
        let log = PatchLog::default();
        let loaded = Loaded {
            core: Box::new(Patchable {
                log: Arc::clone(&log),
                frames: [Frame::new(SCREENS[0])],
            }),
            files: vec![GameFiles::of(&PathBuf::from("/nonexistent/patchable.gb"))],
            rom_paths: vec![PathBuf::from("/nonexistent/patchable.gb")],
            rom_crc32: 0,
            live_sound: None,
        };
        let (events, _events) = async_channel::unbounded();
        let emulator = Emulator::start(loaded, events, None);
        for text in ["[Jump]\n3E1-23F\n", "[Jump] off\n3E1-23F\n"] {
            let cheats = emu_cheats::parse(System::Gb, text).expect("valid");
            emulator.send(Command::SetCheats { player: 0, cheats });
        }
        // Dropping the handle applies the queued commands, then stops.
        drop(emulator);
        let jump = RomPatch {
            address: 0x0123,
            value: 0x3E,
            compare: None,
        };
        assert_eq!(
            *log.lock().expect("unpoisoned"),
            [(0, vec![jump]), (0, Vec::new())]
        );
    }

    /// Waits for a notice starting with `start`, returning it.
    fn wait_for_notice(events: &async_channel::Receiver<Event>, start: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match events.try_recv() {
                Ok(Event::Notice(text)) if text.starts_with(start) => return text,
                Ok(_) => {}
                Err(_) => {
                    assert!(Instant::now() < deadline, "no notice {start:?}");
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// The files in `dir` with `extension`.
    fn files_with(dir: &Path, extension: &str) -> Vec<PathBuf> {
        let entries = fs::read_dir(dir).expect("read dir");
        let paths = entries.map(|entry| entry.expect("entry").path());
        paths
            .filter(|path| path.extension().is_some_and(|found| found == extension))
            .collect()
    }

    #[test]
    fn screenshots_land_next_to_the_rom() {
        let dir = std::env::temp_dir().join(format!("emu-app-shots-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let (emulator, events) = start(dir.join("counter.gb"));
        emulator.send(Command::SaveScreenshot);
        let saved = wait_for_notice(&events, "Saved counter ");
        let pngs = files_with(&dir, "png");
        assert_eq!(pngs.len(), 1, "{saved}");
        let bytes = fs::read(&pngs[0]).expect("the screenshot");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        drop(emulator);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn recordings_hold_every_frame_run_and_none_while_paused() {
        let dir = std::env::temp_dir().join(format!("emu-app-movie-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let (emulator, events) = start(dir.join("counter.gb"));
        emulator.send(Command::TogglePause);
        wait_for_notice(&events, "Paused");
        let first = latest_count(&emulator);
        emulator.send(Command::ToggleRecording);
        wait_for_notice(&events, "Recording to counter ");
        // As fast as it goes, which the writer must keep up with.
        emulator.send(Command::SetSpeed(Speed::Unlimited));
        emulator.send(Command::TogglePause);
        thread::sleep(Duration::from_millis(50));
        emulator.send(Command::TogglePause);
        wait_for_notice(&events, "Paused");
        let last = latest_count(&emulator);
        thread::sleep(Duration::from_millis(20));
        emulator.send(Command::ToggleRecording);
        let saved = wait_for_notice(&events, "Saved counter ");
        assert!(saved.ends_with(" of video"), "{saved}");

        let movies = files_with(&dir, "mov");
        assert_eq!(movies.len(), 1);
        let bytes = fs::read(&movies[0]).expect("the movie");
        let jpegs = bytes
            .windows(4)
            .filter(|window| *window == [0xFF, 0xD8, 0xFF, 0xE0])
            .count();
        assert!(last > first + 10, "frames ran: {first} to {last}");
        assert_eq!(jpegs as u32, last - first, "one picture per frame run");
        drop(emulator);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn durations_read_as_minutes_and_seconds() {
        assert_eq!(minutes_and_seconds(Duration::from_millis(83_900)), "1:23");
        assert_eq!(minutes_and_seconds(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn save_states_round_trip_through_slot_files() {
        let dir = std::env::temp_dir().join(format!("emu-app-states-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let (emulator, events) = start(dir.join("counter.gb"));
        thread::sleep(Duration::from_millis(50));
        emulator.send(Command::TogglePause);
        thread::sleep(Duration::from_millis(20));
        let saved = latest_count(&emulator);
        emulator.send(Command::SaveState(2));
        emulator.send(Command::TogglePause);
        thread::sleep(Duration::from_millis(50));
        emulator.send(Command::LoadState(2));
        emulator.send(Command::TogglePause);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(latest_count(&emulator), saved);

        let notices: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(notices.contains(&Event::Notice("Saved state 2".to_owned())));
        assert!(notices.contains(&Event::Notice("Loaded state 2".to_owned())));
        drop(emulator);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    fn run_script(emulator: &Emulator, source: &str) {
        emulator.send(Command::RunScript {
            name: "hud.lua".to_owned(),
            source: source.to_owned(),
        });
    }

    /// Waits for the script's drawing to pass `test`.
    fn wait_for_overlay(emulator: &Emulator, test: impl Fn(&[Draw]) -> bool) -> Arc<[Draw]> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let overlay = emulator.overlay();
            if test(&overlay) {
                return overlay;
            }
            assert!(Instant::now() < deadline, "the script never drew it");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn scripts_read_write_and_draw_after_every_frame() {
        let (emulator, events) = start(PathBuf::from("/nonexistent/counter.gb"));
        run_script(
            &emulator,
            r#"
            emu.on_frame(function()
                gui.text(0, 0, "count " .. memory.read_u8(0))
                memory.write_u8(1, 1)
            end)
            "#,
        );
        wait_for_notice(&events, "Running hud.lua");
        let overlay = wait_for_overlay(&emulator, |drawn| !drawn.is_empty());
        assert!(
            matches!(&overlay[0], Draw::Text { text, .. } if text.starts_with("count ")),
            "{overlay:?}"
        );
        // Byte 1 of the counter, which the script keeps at 1.
        thread::sleep(Duration::from_millis(20));
        assert_eq!(latest_count(&emulator) >> 8 & 0xFF, 1);

        emulator.send(Command::StopScript);
        wait_for_notice(&events, "Stopped hud.lua");
        assert!(emulator.overlay().is_empty(), "stopping clears its drawing");
        emulator.send(Command::StopScript);
        wait_for_notice(&events, "No script is running");
    }

    #[test]
    fn a_script_error_is_a_notice_and_stops_the_script() {
        let (emulator, events) = start(PathBuf::from("/nonexistent/counter.gb"));
        run_script(
            &emulator,
            r#"
            emu.on_frame(function()
                gui.box(0, 0, 0, 0)
                if emu.frame() == 3 then error("boom") end
            end)
            "#,
        );
        wait_for_notice(&events, "hud.lua:4: boom · script stopped");
        assert!(emulator.overlay().is_empty());
        run_script(&emulator, "x = = 1");
        wait_for_notice(&events, "hud.lua:1: unexpected symbol");
        // The game runs on.
        let before = latest_count(&emulator);
        thread::sleep(Duration::from_millis(20));
        assert!(latest_count(&emulator) > before);
    }

    #[test]
    fn scripts_press_buttons_on_the_next_frame() {
        let (emulator, seen) = start_recorder();
        run_script(
            &emulator,
            "emu.on_frame(function() joypad.set({a = true}, 2) end)",
        );
        run_frames(&seen, 10);
        let a = |input: &Input| input.buttons.contains(Button::A);
        assert!(frames_where(&seen, 1, a) >= 5, "player 2 pressed A");
        assert_eq!(frames_where(&seen, 0, a), 0, "player 1 did not");
    }

    #[test]
    fn scripts_save_states_and_pause() {
        let dir = std::env::temp_dir().join(format!("emu-app-script-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let (emulator, events) = start(dir.join("counter.gb"));
        run_script(
            &emulator,
            r#"
            emu.save_state(3)
            emu.on_frame(function()
                if emu.frame() == 5 then emu.pause() end
            end)
            "#,
        );
        wait_for_notice(&events, "Saved state 3");
        wait_for_notice(&events, "Paused by hud.lua");
        assert!(
            GameFiles::of(&dir.join("counter.gb"))
                .state(3, false)
                .exists()
        );
        let paused = latest_count(&emulator);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(latest_count(&emulator), paused);
        drop(emulator);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// A DS ROM whose two programs loop forever.
    fn nds_rom() -> Vec<u8> {
        let mut rom = vec![0; 0x400];
        let mut put = |offset: usize, value: u32| {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        for (header, program, address) in [(0x20, 0x200, 0x0200_4000), (0x30, 0x300, 0x0238_0000)] {
            put(header, program);
            put(header + 4, address);
            put(header + 8, address);
            put(header + 12, 4);
            put(program as usize, 0xEAFF_FFFE); // B .
        }
        rom
    }

    /// A GBA ROM with 128 KiB of Flash.
    fn gba_rom() -> Vec<u8> {
        let mut rom = vec![0; 0x400];
        rom[0xB2] = 0x96;
        rom[0x200..0x209].copy_from_slice(b"FLASH1M_V");
        rom
    }

    /// Writes `files` into a fresh directory named after `test`.
    fn rom_dir(test: &str, files: &[(&str, &[u8])]) -> (PathBuf, Vec<PathBuf>) {
        let dir = std::env::temp_dir().join(format!("emu-app-{test}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let paths = files
            .iter()
            .map(|(name, bytes)| {
                let path = dir.join(name);
                fs::write(&path, bytes).expect("write rom");
                path
            })
            .collect();
        (dir, paths)
    }

    #[test]
    fn a_gba_game_after_a_ds_game_goes_in_the_gba_slot() {
        let (dir, paths) = rom_dir(
            "slot",
            &[("platinum.nds", &nds_rom()), ("emerald.gba", &gba_rom())],
        );
        let loaded = load(&paths, DmgColors::Grey).expect("loads");
        assert_eq!(
            (loaded.info().system, loaded.info().players),
            (System::Nds, 1)
        );
        assert_eq!(loaded.rom_paths().len(), 2);
        assert_eq!(loaded.player_rom_paths(), &paths[..1]);
        assert!(loaded.core.backup_generation(1).is_some(), "the GBA save");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn games_for_one_system_still_link() {
        let (dir, paths) = rom_dir(
            "link",
            &[("ruby.gba", &gba_rom()), ("sapphire.gba", &gba_rom())],
        );
        let loaded = load(&paths, DmgColors::Grey).expect("loads");
        assert_eq!(
            (loaded.info().system, loaded.info().players),
            (System::Gba, 2)
        );
        assert_eq!(loaded.player_rom_paths(), &paths[..]);
        let (dir_mixed, mixed) = rom_dir(
            "mixed",
            &[("ruby.gba", &gba_rom()), ("platinum.nds", &nds_rom())],
        );
        let error = load(&mixed, DmgColors::Grey)
            .err()
            .expect("a GBA game cannot take a DS game");
        assert!(format!("{error:#}").contains("same system"), "{error:#}");
        fs::remove_dir_all(&dir).expect("clean up");
        fs::remove_dir_all(&dir_mixed).expect("clean up");
    }

    #[test]
    fn two_ds_games_link_over_local_wireless() {
        let (dir, paths) = rom_dir(
            "wireless",
            &[("white.nds", &nds_rom()), ("black.nds", &nds_rom())],
        );
        let loaded = load(&paths, DmgColors::Grey).expect("loads");
        assert_eq!(
            (loaded.info().system, loaded.info().players),
            (System::Nds, 2)
        );
        assert_eq!(loaded.info().screens.len(), 4);
        assert_eq!(loaded.player_rom_paths(), &paths[..]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn same_named_games_for_two_systems_keep_saves_of_their_own() {
        let (dir, paths) = rom_dir(
            "namesake-roms",
            &[("pokemon.nds", &nds_rom()), ("pokemon.gba", &gba_rom())],
        );
        // The GBA game goes in the DS game's slot, each with its own save.
        let loaded = load(&paths, DmgColors::Grey).expect("loads");
        assert!(loaded.core.backup_generation(1).is_some(), "the GBA save");
        let saves: Vec<PathBuf> = loaded.files().iter().map(GameFiles::backup).collect();
        assert_eq!(
            saves,
            [dir.join("pokemon.nds.sav"), dir.join("pokemon.gba.sav")]
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn games_that_would_share_a_save_file_are_refused() {
        let (dir, paths) = rom_dir("shared-save", &[("ruby.gba", &gba_rom())]);
        let twice = [paths[0].clone(), paths[0].clone()];
        let error = load(&twice, DmgColors::Grey).err().expect("refused");
        assert_eq!(
            format!("{error:#}"),
            "ruby.gba and ruby.gba would both save to ruby.sav; rename one of them"
        );
        assert!(!dir.join("ruby.sav").exists());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// A core with a one-byte battery save at address 0 and a byte of
    /// other progress at address 1, both kept in its save states. Each
    /// frame adds `tick` to the save, and panics once it reaches
    /// `crash_at`.
    struct Cart {
        save: u8,
        progress: u8,
        generation: u64,
        tick: u8,
        crash_at: Option<u8>,
        frames: [Frame; 1],
    }

    impl Core for Cart {
        fn info(&self) -> CoreInfo {
            CoreInfo {
                system: System::Gba,
                screens: &SCREENS,
                touch_screen: None,
                players: 1,
                frame_rate: FrameRate::new(1000, 1),
                audio_sample_rate: 48_000,
                state_version: 1,
            }
        }

        fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {
            if self.tick > 0 {
                self.save = self.save.wrapping_add(self.tick);
                self.generation += 1;
            }
            if self.crash_at == Some(self.save) {
                panic!("the cartridge caught fire");
            }
        }

        fn screens(&self) -> &[Frame] {
            &self.frames
        }

        fn reset(&mut self) {}

        fn save_state(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&[self.save, self.progress]);
        }

        fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
            let [save, progress]: [u8; 2] = state.try_into().map_err(|_| StateError::Truncated)?;
            self.save = save;
            self.progress = progress;
            // As the real cores do, so the loaded save gets written.
            self.generation += 1;
            Ok(())
        }

        fn backup_generation(&self, cartridge: usize) -> Option<u64> {
            (cartridge == 0).then_some(self.generation)
        }

        fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
            if cartridge == 0 {
                out.push(self.save);
            }
        }

        fn peek(&self, _: usize, address: u32) -> Option<u8> {
            match address {
                0 => Some(self.save),
                1 => Some(self.progress),
                _ => None,
            }
        }

        fn poke(&mut self, _: usize, address: u32, value: u8) -> bool {
            match address {
                0 => {
                    self.save = value;
                    self.generation += 1;
                }
                1 => self.progress = value,
                _ => return false,
            }
            true
        }

        fn debugger(&mut self) -> Option<&mut dyn Debugger> {
            Some(self)
        }
    }

    peek_debugger!(Cart);

    /// Starts a [`Cart`] for the ROM `dir/game.gba`, with `save` as its
    /// battery save, as if read from disk.
    fn start_cart(
        dir: &Path,
        save: u8,
        tick: u8,
        crash_at: Option<u8>,
    ) -> (Emulator, async_channel::Receiver<Event>) {
        let loaded = Loaded {
            core: Box::new(Cart {
                save,
                progress: 0,
                generation: 0,
                tick,
                crash_at,
                frames: [Frame::new(SCREENS[0])],
            }),
            files: vec![GameFiles::of(&dir.join("game.gba"))],
            rom_paths: vec![dir.join("game.gba")],
            rom_crc32: 0,
            live_sound: None,
        };
        let (events, receiver) = async_channel::unbounded();
        (Emulator::start(loaded, events, None), receiver)
    }

    /// Writes `value` at `address` in a [`Cart`], as its game would.
    fn poke(emulator: &Emulator, address: u32, value: u8) {
        debug(
            emulator,
            debugger::Command::Write {
                cpu: 0,
                address,
                value,
            },
        );
    }

    /// A [`Cart`]'s battery save and other progress, as the debugger
    /// shows them after the commands sent so far.
    fn cart_bytes(emulator: &Emulator) -> (u8, u8) {
        watch_memory(emulator, 0);
        let shown = wait_for(emulator, |shown| shown.memory[0].is_some());
        (
            shown.memory[0].expect("the save"),
            shown.memory[1].expect("the progress"),
        )
    }

    /// Waits for `expected`.
    fn wait_for_event(events: &async_channel::Receiver<Event>, expected: &Event) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match events.try_recv() {
                Ok(event) if event == *expected => return,
                Ok(_) => {}
                Err(_) => {
                    assert!(Instant::now() < deadline, "no {expected:?}");
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// A fresh, empty folder for `test`.
    fn empty_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-app-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// Plays to battery save 1 and progress 7, then leaves a checkpoint
    /// there.
    fn leave_checkpoint(dir: &Path) {
        let (emulator, _events) = start_cart(dir, 0, 0, None);
        poke(&emulator, 0, 1);
        poke(&emulator, 1, 7);
        emulator.send(Command::SaveCheckpoint(0));
        drop(emulator);
        assert_eq!(fs::read(dir.join("game.sav")).expect("saved"), [1]);
    }

    #[test]
    fn a_checkpoint_is_picked_up_with_the_battery_save_it_was_left_with() {
        let dir = empty_dir("checkpoint-same");
        leave_checkpoint(&dir);
        assert!(dir.join("game.ss0.saves").exists());
        let (emulator, events) = start_cart(&dir, 1, 0, None);
        emulator.send(Command::LoadCheckpoint(0));
        wait_for_event(&events, &Event::PickedUp(true));
        assert_eq!(cart_bytes(&emulator), (1, 7));
        drop(emulator);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_checkpoint_never_rolls_back_a_newer_battery_save() {
        let dir = empty_dir("checkpoint-newer");
        let save = dir.join("game.sav");
        // Left off at 1, then saved in the game at 2 and quit without a
        // new checkpoint, as after a crash.
        let (emulator, _events) = start_cart(&dir, 0, 0, None);
        poke(&emulator, 0, 1);
        poke(&emulator, 1, 7);
        emulator.send(Command::SaveCheckpoint(0));
        poke(&emulator, 0, 2);
        drop(emulator);
        assert_eq!(fs::read(&save).expect("saved"), [2]);

        let (emulator, events) = start_cart(&dir, 2, 0, None);
        emulator.send(Command::LoadCheckpoint(0));
        wait_for_event(&events, &Event::PickedUp(false));
        assert_eq!(cart_bytes(&emulator), (2, 0));
        drop(emulator);
        assert_eq!(fs::read(&save).expect("kept"), [2]);

        // Nor one copied in from elsewhere.
        fs::write(&save, [9]).expect("copied in");
        let (emulator, events) = start_cart(&dir, 9, 0, None);
        emulator.send(Command::LoadCheckpoint(0));
        wait_for_event(&events, &Event::PickedUp(false));
        drop(emulator);
        assert_eq!(fs::read(&save).expect("kept"), [9]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_checkpoint_from_before_records_is_picked_up_only_with_its_own_save() {
        let dir = empty_dir("checkpoint-unrecorded");
        let save = dir.join("game.sav");
        leave_checkpoint(&dir);
        fs::remove_file(dir.join("game.ss0.saves")).expect("as older versions left it");
        let (emulator, events) = start_cart(&dir, 1, 0, None);
        emulator.send(Command::LoadCheckpoint(0));
        wait_for_event(&events, &Event::PickedUp(true));
        assert_eq!(cart_bytes(&emulator), (1, 7));
        drop(emulator);

        fs::write(&save, [3]).expect("saved since");
        let (emulator, events) = start_cart(&dir, 3, 0, None);
        emulator.send(Command::LoadCheckpoint(0));
        wait_for_event(&events, &Event::PickedUp(false));
        assert_eq!(cart_bytes(&emulator), (3, 0), "undone");
        drop(emulator);
        assert_eq!(fs::read(&save).expect("kept"), [3]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn an_empty_checkpoint_slot_is_no_matter() {
        let dir = empty_dir("checkpoint-none");
        let (emulator, events) = start_cart(&dir, 0, 0, None);
        emulator.send(Command::LoadCheckpoint(0));
        emulator.send(Command::TogglePause);
        wait_for_notice(&events, "Paused");
        drop(emulator);
        let said: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(
            !said
                .iter()
                .any(|event| matches!(event, Event::PickedUp(_) | Event::Notice(_))),
            "{said:?}"
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn pausing_writes_the_battery_save() {
        let dir = empty_dir("pause-flush");
        let (emulator, events) = start_cart(&dir, 0, 1, None);
        thread::sleep(Duration::from_millis(20));
        emulator.send(Command::TogglePause);
        wait_for_notice(&events, "Paused");
        // Well before the timed flush, which never comes while paused.
        let (save, _) = cart_bytes(&emulator);
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read(dir.join("game.sav")).ok() != Some(vec![save]) {
            assert!(Instant::now() < deadline, "the save was not written");
            thread::sleep(Duration::from_millis(1));
        }
        drop(emulator);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// Waits for the emulation thread to say it stopped, returning what
    /// it said.
    fn wait_for_stopped(events: &async_channel::Receiver<Event>) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match events.try_recv() {
                Ok(Event::Stopped(text)) => return text,
                Ok(_) => {}
                Err(_) => {
                    assert!(Instant::now() < deadline, "never said it stopped");
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// Makes writing `save` fail, as a full or locked disk would, while
    /// other files in its folder can still be written.
    fn block(save: &Path) -> PathBuf {
        let mut temporary = save.as_os_str().to_owned();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        fs::create_dir(&temporary).expect("in the way");
        temporary
    }

    #[test]
    fn a_panic_keeps_the_battery_save_and_says_the_game_stopped() {
        let dir = empty_dir("panic");
        let (emulator, events) = start_cart(&dir, 0, 1, Some(5));
        let said = wait_for_stopped(&events);
        assert!(said.contains("the cartridge caught fire"), "{said}");
        assert!(said.contains("are kept"), "{said}");
        assert_eq!(fs::read(dir.join("game.sav")).expect("written"), [5]);
        // The handle still stops cleanly.
        emulator.send(Command::TogglePause);
        drop(emulator);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_panic_says_so_when_the_battery_save_could_not_be_written() {
        let dir = empty_dir("panic-unsaved");
        block(&dir.join("game.sav"));
        let (emulator, events) = start_cart(&dir, 0, 1, Some(5));
        let said = wait_for_stopped(&events);
        assert!(said.contains("may be lost"), "{said}");
        drop(emulator);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn no_checkpoint_is_left_while_its_battery_save_cannot_be_written() {
        let dir = empty_dir("checkpoint-unsaved");
        let (state, record) = (dir.join("game.ss0"), dir.join("game.ss0.saves"));
        let (emulator, events) = start_cart(&dir, 0, 0, None);
        poke(&emulator, 0, 1);
        let in_the_way = block(&dir.join("game.sav"));
        emulator.send(Command::SaveCheckpoint(0));
        wait_for_notice(&events, "Could not save where you left off");
        assert!(!state.exists() && !record.exists());
        // Once the save can be written, so can the checkpoint.
        fs::remove_dir(&in_the_way).expect("out of the way");
        emulator.send(Command::SaveCheckpoint(0));
        drop(emulator);
        assert!(state.exists() && record.exists());
        assert_eq!(fs::read(dir.join("game.sav")).expect("written"), [1]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_session_keeps_the_file_names_it_started_with() {
        let (dir, paths) = rom_dir("names-kept", &[("solo.gba", &gba_rom())]);
        let loaded = load(&paths, DmgColors::Grey).expect("loads");
        let (events, _events) = async_channel::unbounded();
        let emulator = Emulator::start(loaded, events, None);
        // A namesake arrives mid-game; this game's files stay as named.
        fs::write(dir.join("solo.nds"), nds_rom()).expect("written");
        emulator.send(Command::SaveCheckpoint(0));
        drop(emulator);
        assert!(dir.join("solo.ss0").exists());
        assert!(dir.join("solo.ss0.saves").exists());
        assert!(!dir.join("solo.gba.ss0").exists());
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
