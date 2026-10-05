//! Runs a core on its own thread at the system's frame rate.
//!
//! The UI thread sends [`Command`]s; the emulation thread publishes each
//! finished frame into a shared slot and sends an [`Event`] to wake the UI.
//! Wake-ups are coalesced, so a slow UI never queues stale frames.
//!
//! Pacing is a sleep to the next frame deadline. Sound stays in sync
//! because the audio sink adjusts its resampling ratio to the buffer level
//! (see `resample.rs`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, ensure};
use emu_core::{Core, CoreInfo, Frame, Input, Media, System};

use crate::audio::AudioSink;
use crate::persist;
use crate::rewind::Rewind;

/// How often dirty battery saves are written to disk.
const BACKUP_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Memory for rewind history: about a minute of Game Boy play.
const REWIND_BUDGET_BYTES: usize = 64 << 20;

/// A game ready to run: the core plus what is needed to save it.
pub struct Loaded {
    core: Box<dyn Core>,
    /// Each player's ROM file; battery saves sit next to them.
    rom_paths: Vec<PathBuf>,
    /// Identifies the ROMs in save states.
    rom_crc32: u32,
}

impl Loaded {
    /// Static facts about the loaded core.
    pub fn info(&self) -> CoreInfo {
        self.core.info()
    }

    /// Each player's ROM file.
    pub fn rom_paths(&self) -> &[PathBuf] {
        &self.rom_paths
    }
}

/// A ROM file read from disk, with its battery save.
struct RomFile {
    system: System,
    rom: Vec<u8>,
    backup: Option<Vec<u8>>,
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

/// Reads each player's ROM and battery save and powers on the right core:
/// one game, or with two ROMs a pair joined by a link cable.
///
/// Blocking file I/O; call it off the UI thread.
///
/// # Errors
///
/// Fails if a file cannot be read, its system cannot be identified, linked
/// ROMs are for different systems, or the core rejects them.
pub fn load(rom_paths: &[PathBuf]) -> anyhow::Result<Loaded> {
    let files = rom_paths
        .iter()
        .map(|path| read_rom(path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let system = files.first().context("no ROM given")?.system;
    ensure!(
        files.iter().all(|file| file.system == system),
        "linked games must be for the same system"
    );
    let start_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        });
    let media: Vec<Media<'_>> = files
        .iter()
        .map(|file| Media {
            backup: file.backup.as_deref(),
            start_time,
            ..Media::new(&file.rom)
        })
        .collect();
    let core = match media.as_slice() {
        [one] => emu_cores::create(system, one)?,
        players => emu_cores::create_linked(system, players)?,
    };
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

/// A request from the UI to the emulation thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Replaces a player's input from the next frame on.
    SetInput {
        /// Index into the core's players.
        player: usize,
        /// What that player holds.
        input: Input,
    },
    /// Runs unthrottled while true.
    SetFastForward(bool),
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
}

/// A notification from the emulation thread to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A new frame is in the shared slot.
    Frame,
    /// Something the user should see briefly, such as "State saved".
    Notice(String),
}

/// The most recent frame, shared between the threads.
#[derive(Default)]
struct Latest {
    frames: Mutex<(u64, Vec<Frame>)>,
    unseen: AtomicBool,
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
            flushed_generations: (0..players)
                .map(|player| loaded.core.backup_generation(player).unwrap_or(0))
                .collect(),
            loaded,
            inputs: vec![Input::default(); players],
            paused: false,
            fast_forward: false,
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
    /// One per player.
    inputs: Vec<Input>,
    paused: bool,
    fast_forward: bool,
    rewinding: bool,
    rewind: Rewind,
    /// Per player: the backup generation last written to disk.
    flushed_generations: Vec<u64>,
    events: async_channel::Sender<Event>,
    latest: Arc<Latest>,
    audio: Option<AudioSink>,
}

impl Runner {
    fn run(mut self, commands: &Receiver<Command>) {
        let frame_duration = self.loaded.core.info().frame_rate.frame_duration();
        let mut audio = Vec::new();
        let mut state = Vec::new();
        let mut deadline = Instant::now();
        let mut last_flush = Instant::now();

        'running: loop {
            loop {
                // Rewinding works while paused; everything else waits.
                let next = if self.paused && !self.rewinding {
                    commands.recv().map_err(|_| TryRecvError::Disconnected)
                } else {
                    commands.try_recv()
                };
                match next {
                    Ok(command) => self.apply(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'running,
                }
                if !self.paused {
                    // Don't race to catch up on time spent paused.
                    deadline = deadline.max(Instant::now());
                }
            }

            if self.rewinding {
                if let Some(previous) = self.rewind.pop() {
                    // States in the history came from this core.
                    self.loaded.core.load_state(&previous).ok();
                    self.publish();
                }
            } else {
                self.loaded.core.run_frame(&self.inputs, &mut audio);
                // Fast-forward plays silence rather than sped-up sound.
                if !self.fast_forward
                    && let Some(sink) = &mut self.audio
                {
                    sink.push(&audio);
                }
                audio.clear();
                self.publish();
                state.clear();
                self.loaded.core.save_state(&mut state);
                self.rewind.push(&state);
            }

            if last_flush.elapsed() >= BACKUP_FLUSH_INTERVAL {
                self.flush_backup();
                last_flush = Instant::now();
            }

            let now = Instant::now();
            if self.fast_forward && !self.rewinding {
                deadline = now;
                continue;
            }
            deadline += frame_duration;
            if deadline > now {
                thread::sleep(deadline - now);
            } else if now - deadline > frame_duration * 4 {
                // Too far behind to catch up smoothly; drop the debt.
                deadline = now;
            }
        }
        self.flush_backup();
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::SetInput { player, input } => {
                if let Some(slot) = self.inputs.get_mut(player) {
                    *slot = input;
                }
            }
            Command::SetFastForward(on) => self.fast_forward = on,
            Command::SetRewind(on) => self.rewinding = on,
            Command::TogglePause => {
                self.paused = !self.paused;
                self.notice(if self.paused { "Paused" } else { "Resumed" });
            }
            Command::Reset => {
                self.loaded.core.reset();
                self.rewind.clear();
                self.notice("Reset");
            }
            Command::SaveState(slot) => match self.save_state(slot) {
                Ok(()) => self.notice(format!("Saved state {slot}")),
                Err(error) => self.notice(format!("Could not save state: {error:#}")),
            },
            Command::LoadState(slot) => match self.load_state(slot) {
                Ok(()) => {
                    self.rewind.clear();
                    self.publish();
                    self.notice(format!("Loaded state {slot}"));
                }
                Err(error) => self.notice(format!("Could not load state: {error:#}")),
            },
        }
    }

    fn publish(&self) {
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
        if !self.latest.unseen.swap(true, Ordering::AcqRel) {
            self.events.try_send(Event::Frame).ok();
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
        let screen = &self.loaded.core.screens()[0];
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
        let path = persist::state_path(&self.loaded.rom_paths, slot);
        persist::write_atomic(
            &path,
            &emu_state::encode(&header, &payload, Some(thumbnail)),
        )
        .with_context(|| format!("cannot write {}", path.display()))
    }

    fn load_state(&mut self, slot: u8) -> anyhow::Result<()> {
        let path = persist::state_path(&self.loaded.rom_paths, slot);
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

    /// Writes each player's battery save that changed since the last flush.
    fn flush_backup(&mut self) {
        for player in 0..self.flushed_generations.len() {
            let Some(generation) = self.loaded.core.backup_generation(player) else {
                continue;
            };
            if generation == self.flushed_generations[player] {
                continue;
            }
            let mut backup = Vec::new();
            self.loaded.core.save_backup(player, &mut backup);
            let path = persist::backup_path(&self.loaded.rom_paths[player]);
            match persist::write_atomic(&path, &backup) {
                Ok(()) => self.flushed_generations[player] = generation,
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

    use emu_core::{CoreInfo, FrameRate, ScreenSize, StateError, StereoSample, System};

    use super::*;

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
}
