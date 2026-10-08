//! Runs a core on its own thread at the system's frame rate.
//!
//! The UI thread sends [`Command`]s; the emulation thread publishes each
//! finished frame into a shared slot and sends an [`Event`] to wake the UI.
//! Wake-ups are coalesced, so a slow UI never queues stale frames.
//!
//! Screenshots and recordings take the core's own frames here, every one
//! of them for a recording, rather than the coalesced ones the UI shows.
//!
//! Pacing is a sleep to the next frame deadline, which fast-forward and
//! slow motion move closer or further apart (see `speed.rs`). Sound stays
//! in sync because the audio sink adjusts its resampling ratio to the
//! buffer level (see `resample.rs`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use emu_cheats::Cheat;
use emu_core::debug::{Breakpoint, RunOutcome, StopReason};
use emu_core::{Buttons, Cartridge, Core, CoreInfo, Frame, Input, Media, System};

use crate::audio::AudioSink;
use crate::capture;
use crate::debugger::{self, Pending, RunState, Snapshot};
use crate::latch::Latch;
use crate::persist;
use crate::recorder::Recorder;
use crate::rewind::Rewind;
use crate::speed::{self, Speed};
use crate::turbo::Turbo;
use crate::video;

/// How often dirty battery saves are written to disk.
const BACKUP_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Memory for rewind history: about a minute of Game Boy play.
const REWIND_BUDGET_BYTES: usize = 64 << 20;

/// A game ready to run: the core plus what is needed to save it.
pub struct Loaded {
    core: Box<dyn Core>,
    /// Each cartridge's ROM file, in the core's cartridge order: one per
    /// player, then any in a second slot. Battery saves sit next to them.
    rom_paths: Vec<PathBuf>,
    /// Identifies the ROMs in save states.
    rom_crc32: u32,
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
}

/// A ROM file read from disk, with its battery save.
struct RomFile {
    system: System,
    rom: Vec<u8>,
    backup: Option<Vec<u8>>,
}

impl RomFile {
    /// The file as a core's media, its clock starting at `start_time`.
    fn media(&self, start_time: i64) -> Media<'_> {
        Media {
            backup: self.backup.as_deref(),
            start_time,
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

fn read_rom(rom_path: &Path) -> anyhow::Result<RomFile> {
    let system = emu_cores::system_for_path(rom_path).with_context(|| {
        format!(
            "{} is not a .gb, .gbc, .gba or .nds file",
            rom_path.display()
        )
    })?;
    let rom = fs::read(rom_path).with_context(|| format!("cannot read {}", rom_path.display()))?;
    let backup_path = persist::backup_path(rom_path);
    let backup = match fs::read(&backup_path) {
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
/// games for the same system joined by a link cable.
///
/// Blocking file I/O; call it off the UI thread.
///
/// # Errors
///
/// Fails if a file cannot be read, its system cannot be identified, linked
/// ROMs are for different systems, two games would share a battery save
/// file, or the core rejects them.
pub fn load(rom_paths: &[PathBuf]) -> anyhow::Result<Loaded> {
    let files = rom_paths
        .iter()
        .map(|path| read_rom(path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let start_time = local_clock_seconds();
    let core = match files.as_slice() {
        [] => bail!("no ROM given"),
        [one] => emu_cores::create(one.system, &one.media(start_time))?,
        [first, second] if emu_cores::second_slot(first.system) == Some(second.system) => {
            let slotted = Media {
                second_slot: Some(Cartridge {
                    rom: &second.rom,
                    backup: second.backup.as_deref(),
                }),
                ..first.media(start_time)
            };
            emu_cores::create(first.system, &slotted)?
        }
        [first, ..] => {
            let system = first.system;
            ensure!(
                files.iter().all(|file| file.system == system),
                "linked games must be for the same system"
            );
            let players: Vec<Media<'_>> = files.iter().map(|file| file.media(start_time)).collect();
            emu_cores::create_linked(system, &players)?
        }
    };
    ensure_separate_saves(&*core, rom_paths)?;
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
        rom_crc32,
    })
}

/// Fails if two cartridges with battery saves would use one `.sav` file,
/// as `game.nds` and `game.gba` in one folder would: each would overwrite
/// the other's save.
fn ensure_separate_saves(core: &dyn Core, rom_paths: &[PathBuf]) -> anyhow::Result<()> {
    let saving: Vec<&PathBuf> = (0..)
        .zip(rom_paths)
        .filter(|(cartridge, _)| core.backup_generation(*cartridge).is_some())
        .map(|(_, rom)| rom)
        .collect();
    let keys: Vec<String> = saving.iter().map(|rom| persist::backup_key(rom)).collect();
    for (index, rom) in saving.iter().enumerate() {
        if let Some(other) = (0..index).find(|other| keys[*other] == keys[index]) {
            bail!(
                "{} and {} would both save to {}; rename one of them",
                file_name(saving[other]),
                file_name(rom),
                file_name(&persist::backup_path(rom)),
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
    /// Runs at this speed from the next frame on.
    SetSpeed(Speed),
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
    /// What the debugger window shows, as of that frame, while it is open.
    debug: Mutex<Option<Arc<Snapshot>>>,
    unseen: AtomicBool,
    debug_unseen: AtomicBool,
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
        let runner = Runner {
            flushed_generations: (0..loaded.rom_paths.len())
                .map(|cartridge| loaded.core.backup_generation(cartridge).unwrap_or(0))
                .collect(),
            loaded,
            latches: vec![Latch::default(); players],
            turbos: vec![Turbo::default(); players],
            cheats: vec![Vec::new(); players],
            debug: Debugging::default(),
            recorder: None,
            paused: false,
            speed: Speed::NORMAL,
            rewinding: false,
            rewind: Rewind::new(REWIND_BUDGET_BYTES),
            events,
            latest: Arc::clone(&latest),
            audio,
        };
        let thread = thread::Builder::new()
            .name("emulation".to_owned())
            .spawn(move || runner.run(&receiver))
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
        Some(*sequence)
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
    paused: bool,
    /// How fast frames run; rewinding always steps at normal speed.
    speed: Speed,
    rewinding: bool,
    rewind: Rewind,
    /// Per cartridge: the backup generation last written to disk.
    flushed_generations: Vec<u64>,
    events: async_channel::Sender<Event>,
    latest: Arc<Latest>,
    audio: Option<AudioSink>,
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

impl Runner {
    fn run(mut self, commands: &Receiver<Command>) {
        let frame_duration = self.loaded.core.info().frame_rate.frame_duration();
        let mut inputs = Vec::new();
        let mut audio = Vec::new();
        let mut state = Vec::new();
        let mut deadline = Instant::now();
        let mut last_flush = Instant::now();

        'running: loop {
            loop {
                // Rewinding and debugger steps work while paused;
                // everything else waits.
                let next = if self.paused && !self.rewinding && self.debug.pending.is_none() {
                    commands.recv().map_err(|_| TryRecvError::Disconnected)
                } else {
                    commands.try_recv()
                };
                match next {
                    Ok(command) => self.apply(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'running,
                }
                if !self.paused || self.rewinding {
                    // Don't race to catch up on time spent paused.
                    deadline = deadline.max(Instant::now());
                }
            }

            if self.rewinding {
                if let Some(previous) = self.rewind.pop() {
                    // States in the history came from this core.
                    self.loaded.core.load_state(&previous).ok();
                    self.clear_latches();
                    self.publish();
                }
            } else {
                inputs.clear();
                for (latch, turbo) in self.latches.iter().zip(&self.turbos) {
                    inputs.push(turbo.apply(latch.input()));
                }
                let outcome = self.run_frame(&inputs, &mut audio);
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
                    sink.push(&audio);
                }
                self.record(&audio);
                audio.clear();
                self.settle(outcome);
                self.publish();
                state.clear();
                self.loaded.core.save_state(&mut state);
                self.rewind.push(&state);
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
            let now = Instant::now();
            deadline = speed::next_deadline(deadline, now, speed.frame_step(frame_duration));
            if deadline > now {
                thread::sleep(deadline - now);
            }
        }
        if let Some(recorder) = self.recorder.take() {
            self.finish_recording(recorder);
        }
        self.flush_backup();
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
            Command::SetSpeed(speed) => self.speed = speed,
            Command::SetRewind(on) => self.rewinding = on,
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
                self.rewind.clear();
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
                    self.rewind.clear();
                    self.clear_latches();
                    self.debug.stopped = None;
                    self.publish();
                    self.notice(format!("Loaded state {slot}"));
                }
                Err(error) => self.notice(format!("Could not load state: {error:#}")),
            },
            Command::SaveScreenshot => self.save_screenshot(),
            Command::ToggleRecording => match self.recorder.take() {
                Some(recorder) => self.finish_recording(recorder),
                None => self.start_recording(),
            },
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
                let path = persist::capture_path(&rom, &stamp, "png");
                let text = match persist::write_atomic(&path, &capture::encode_png(&picture)) {
                    Ok(()) => format!("Saved {}", file_name(&path)),
                    Err(error) => format!("Could not save {}: {error}", path.display()),
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
        let path = persist::capture_path(rom, &capture_stamp(), "mov");
        let info = self.loaded.core.info();
        match Recorder::start(path, &info, self.loaded.core.screens()) {
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

    /// Drops releases still waiting out a press's minimum hold, and turbo
    /// presses being finished, and starts held turbo buttons again with a
    /// press: after the game jumps to another moment, a press from before
    /// would land out of place.
    fn clear_latches(&mut self) {
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
            let screens = self.loaded.core.screens();
            if frames.len() == screens.len() {
                frames.clone_from_slice(screens);
            } else {
                *frames = screens.to_vec();
            }
        }
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
        self.loaded.core.save_state(&mut payload);
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
        let path = persist::state_path(self.loaded.player_rom_paths(), slot);
        persist::write_atomic(
            &path,
            &emu_state::encode(&header, &payload, Some(thumbnail)),
        )
        .with_context(|| format!("cannot write {}", path.display()))
    }

    fn load_state(&mut self, slot: u8) -> anyhow::Result<()> {
        let path = persist::state_path(self.loaded.player_rom_paths(), slot);
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

    /// Writes each cartridge's battery save that changed since the last
    /// flush.
    fn flush_backup(&mut self) {
        for cartridge in 0..self.flushed_generations.len() {
            let Some(generation) = self.loaded.core.backup_generation(cartridge) else {
                continue;
            };
            if generation == self.flushed_generations[cartridge] {
                continue;
            }
            let mut backup = Vec::new();
            self.loaded.core.save_backup(cartridge, &mut backup);
            let path = persist::backup_path(&self.loaded.rom_paths[cartridge]);
            match persist::write_atomic(&path, &backup) {
                Ok(()) => self.flushed_generations[cartridge] = generation,
                Err(error) => self.notice(format!("Could not write {}: {error}", path.display())),
            }
        }
    }

    fn notice(&self, text: impl Into<String>) {
        self.events.try_send(Event::Notice(text.into())).ok();
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
            rom_paths: vec![rom_path],
            rom_crc32: 0,
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
            rom_paths: vec![PathBuf::from("/nonexistent/recorder.gba")],
            rom_crc32: 0,
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
            rom_paths: vec![PathBuf::from("/nonexistent/keypad.gba")],
            rom_crc32: 0,
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
            rom_paths: vec![PathBuf::from("/nonexistent/debugged.gb")],
            rom_crc32: 0,
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
            rom_paths: vec![PathBuf::from("/nonexistent/patchable.gb")],
            rom_crc32: 0,
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
        let loaded = load(&paths).expect("loads");
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
        let loaded = load(&paths).expect("loads");
        assert_eq!(
            (loaded.info().system, loaded.info().players),
            (System::Gba, 2)
        );
        assert_eq!(loaded.player_rom_paths(), &paths[..]);
        let (dir_mixed, mixed) = rom_dir(
            "mixed",
            &[("ruby.gba", &gba_rom()), ("platinum.nds", &nds_rom())],
        );
        let error = load(&mixed)
            .err()
            .expect("a GBA game cannot take a DS game");
        assert!(format!("{error:#}").contains("same system"), "{error:#}");
        fs::remove_dir_all(&dir).expect("clean up");
        fs::remove_dir_all(&dir_mixed).expect("clean up");
    }

    #[test]
    fn games_that_would_share_a_save_file_are_refused() {
        let (dir, paths) = rom_dir(
            "shared-save",
            &[("pokemon.nds", &nds_rom()), ("pokemon.gba", &gba_rom())],
        );
        let error = load(&paths).err().expect("refused");
        assert_eq!(
            format!("{error:#}"),
            "pokemon.nds and pokemon.gba would both save to pokemon.sav; rename one of them"
        );
        assert!(!dir.join("pokemon.sav").exists());
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
