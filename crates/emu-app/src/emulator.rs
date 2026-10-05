//! Runs a core on its own thread at the system's frame rate.
//!
//! The UI thread sends [`Command`]s; the emulation thread publishes each
//! finished frame into a shared slot and sends an [`Event`] to wake the UI.
//! Wake-ups are coalesced, so a slow UI never queues stale frames.
//!
//! Pacing is a simple sleep to the next frame deadline. Audio-driven pacing
//! replaces it once cores produce sound (see `docs/ROADMAP.md`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, ensure};
use emu_core::{Core, CoreInfo, Frame, Input, Media};

use crate::persist;

/// How often dirty battery saves are written to disk.
const BACKUP_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// The only save-state slot until the UI grows a slot picker.
const STATE_SLOT: u8 = 1;

/// A game ready to run: the core plus what is needed to save it.
pub struct Loaded {
    core: Box<dyn Core>,
    rom_path: PathBuf,
    rom_crc32: u32,
}

impl Loaded {
    /// Static facts about the loaded core.
    pub fn info(&self) -> CoreInfo {
        self.core.info()
    }
}

/// Reads `rom_path` and its battery save, and powers on the right core.
///
/// Blocking file I/O; call it off the UI thread.
///
/// # Errors
///
/// Fails if the file cannot be read, its system cannot be identified, or
/// the core rejects it.
pub fn load(rom_path: &Path) -> anyhow::Result<Loaded> {
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
    let start_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        });
    let media = Media {
        backup: backup.as_deref(),
        start_time,
        ..Media::new(&rom)
    };
    let core = emu_cores::create(system, &media)?;
    Ok(Loaded {
        core,
        rom_path: rom_path.to_owned(),
        rom_crc32: crc32fast::hash(&rom),
    })
}

/// A request from the UI to the emulation thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Replaces the input held from the next frame on.
    SetInput(Input),
    /// Runs unthrottled while true.
    SetFastForward(bool),
    /// Pauses or resumes.
    TogglePause,
    /// Power-cycles the system.
    Reset,
    /// Writes a save state to the slot file.
    SaveState,
    /// Restores the save state in the slot file.
    LoadState,
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
    /// Starts running `loaded` on a new thread. Events go to `events`.
    pub fn start(loaded: Loaded, events: async_channel::Sender<Event>) -> Self {
        let (commands, receiver) = mpsc::channel();
        let latest = Arc::new(Latest::default());
        let runner = Runner {
            flushed_generation: loaded.core.backup_generation().unwrap_or(0),
            loaded,
            input: Input::default(),
            paused: false,
            fast_forward: false,
            events,
            latest: Arc::clone(&latest),
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
    input: Input,
    paused: bool,
    fast_forward: bool,
    flushed_generation: u64,
    events: async_channel::Sender<Event>,
    latest: Arc<Latest>,
}

impl Runner {
    fn run(mut self, commands: &Receiver<Command>) {
        let frame_duration = self.loaded.core.info().frame_rate.frame_duration();
        let mut audio = Vec::new();
        let mut deadline = Instant::now();
        let mut last_flush = Instant::now();

        'running: loop {
            loop {
                let next = if self.paused {
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

            self.loaded.core.run_frame(&self.input, &mut audio);
            audio.clear();
            self.publish();

            if last_flush.elapsed() >= BACKUP_FLUSH_INTERVAL {
                self.flush_backup();
                last_flush = Instant::now();
            }

            let now = Instant::now();
            if self.fast_forward {
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
            Command::SetInput(input) => self.input = input,
            Command::SetFastForward(on) => self.fast_forward = on,
            Command::TogglePause => {
                self.paused = !self.paused;
                self.notice(if self.paused { "Paused" } else { "Resumed" });
            }
            Command::Reset => {
                self.loaded.core.reset();
                self.notice("Reset");
            }
            Command::SaveState => match self.save_state() {
                Ok(()) => self.notice(format!("Saved state {STATE_SLOT}")),
                Err(error) => self.notice(format!("Could not save state: {error:#}")),
            },
            Command::LoadState => match self.load_state() {
                Ok(()) => self.notice(format!("Loaded state {STATE_SLOT}")),
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

    fn save_state(&self) -> anyhow::Result<()> {
        let info = self.loaded.core.info();
        let mut payload = Vec::new();
        self.loaded.core.save_state(&mut payload);
        let header = emu_state::Header {
            system: info.system,
            state_version: info.state_version,
            rom_crc32: self.loaded.rom_crc32,
        };
        let path = persist::state_path(&self.loaded.rom_path, STATE_SLOT);
        persist::write_atomic(&path, &emu_state::encode(&header, &payload))
            .with_context(|| format!("cannot write {}", path.display()))
    }

    fn load_state(&mut self) -> anyhow::Result<()> {
        let path = persist::state_path(&self.loaded.rom_path, STATE_SLOT);
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let (header, payload) = emu_state::decode(&bytes)?;
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

    fn flush_backup(&mut self) {
        let Some(generation) = self.loaded.core.backup_generation() else {
            return;
        };
        if generation == self.flushed_generation {
            return;
        }
        let mut backup = Vec::new();
        self.loaded.core.save_backup(&mut backup);
        let path = persist::backup_path(&self.loaded.rom_path);
        match persist::write_atomic(&path, &backup) {
            Ok(()) => self.flushed_generation = generation,
            Err(error) => self.notice(format!("Could not write {}: {error}", path.display())),
        }
    }

    fn notice(&self, text: impl Into<String>) {
        self.events.try_send(Event::Notice(text.into())).ok();
    }
}
