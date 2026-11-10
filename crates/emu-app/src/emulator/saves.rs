//! A running game's battery saves, save states and checkpoint: what the
//! emulation thread keeps on disk, apart from the core that runs the game,
//! which every call is handed.
//!
//! Battery saves are copied from the core when they change and written on a
//! thread of their own (see `save_writer.rs`). What there is to tell the
//! player is kept until it is asked for: notices with [`Saves::take_said`],
//! and warnings the player must not miss, such as a save that could not be
//! written or where a save was kept, with [`Saves::take_warned`]. A save
//! that cannot be written is tried again at every flush, quietly: its
//! warning is said again only when its words change.
//!
//! Loading a save state takes the game's save memory back with it, and
//! the cores count that as a save written. So that the file on disk, which
//! may hold hours more, is not lost to it, the write that follows a load
//! first keeps the file if it holds another save, as
//! `game.sav.before-load`, the newest of at most five
//! ([`persist::keep_before_load`]), and the player is told where it is. A
//! load that leaves the save as it is on disk keeps nothing, nor one that
//! changes only the time of a Game Boy cartridge's clock, which every
//! moment has its own.
//!
//! A player trying something over and over loads a state every minute or
//! so, for an hour, and each load may find another save on disk: one the
//! game made since, or, in a Game Boy game that uses its cartridge's
//! memory to work in, whatever was there. Keeping each would push the save
//! from before the first load out of the five. So loads less than
//! [`RUN_OF_LOADS`] apart, each from the one before, are one run, and only
//! the first load of a run that would change the save keeps it
//! ([`keeps_save`]). Rewinding is not a load: it undoes a save made in the
//! game as it undoes everything else, in front of the player.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, anyhow, ensure};
use emu_core::Core;
use emu_state::Mismatch;

use super::{Loaded, OwnSaves};
use crate::capture;
use crate::persist::{self, Found, GameFiles, Kept, StateFiles, file_name};
use crate::save_writer::SaveWriter;
use crate::video;

/// What came of picking up from a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PickUp {
    /// There is none.
    Empty,
    /// It was loaded.
    Loaded,
    /// A battery save changed after it was saved, so it was left alone.
    SavesChanged,
}

/// Why a state does not go with the game being played, in the words the
/// player has always been told. `emu-state` finds which of the three it
/// is ([`emu_state::Header::check`]); its own words are `emu-headless`'s.
fn refusal(mismatch: Mismatch) -> anyhow::Error {
    match mismatch {
        Mismatch::System { found, .. } => anyhow!("it is a {found} state"),
        Mismatch::Rom { .. } => anyhow!("it was saved from a different ROM"),
        Mismatch::StateVersion { found, reads } => {
            anyhow!("it is state version {found}, this build reads {reads}")
        }
    }
}

/// State loads less than this long apart, each from the one before, are
/// one run of loads.
const RUN_OF_LOADS: Duration = Duration::from_secs(10 * 60);

/// Whether a state load starts a new run of loads: it comes `since_last`
/// after the load before it, or is the session's first (`None`).
fn starts_run(since_last: Option<Duration>) -> bool {
    since_last.is_none_or(|gap| gap >= RUN_OF_LOADS)
}

/// Whether a state load may keep the save it is about to replace on disk.
/// It comes `since_last` after the load before it (`None` for the
/// session's first), and `kept_in_run` says whether a load of the run it
/// would belong to has kept that save already. Only the first load of a
/// run that would change the save keeps it; a load that starts a new run
/// may always.
fn keeps_save(since_last: Option<Duration>, kept_in_run: bool) -> bool {
    starts_run(since_last) || !kept_in_run
}

/// Each of the first `cartridges` cartridges' backup generation in
/// `core`, taken to be on disk already: 0 for one without a battery.
fn generations(core: &dyn Core, cartridges: usize) -> Vec<Option<u64>> {
    (0..cartridges)
        .map(|cartridge| Some(core.backup_generation(cartridge).unwrap_or(0)))
        .collect()
}

/// Each of the first `cartridges` cartridges' battery save as `core` has
/// it, taken to be on disk already: `None` for one without a battery.
fn backups_on_disk(core: &dyn Core, cartridges: usize) -> Vec<Option<Vec<u8>>> {
    (0..cartridges)
        .map(|cartridge| {
            core.backup_generation(cartridge)?;
            let mut backup = Vec::new();
            core.save_backup(cartridge, &mut backup);
            Some(backup)
        })
        .collect()
}

/// A running game's battery saves, save states and checkpoint.
pub(super) struct Saves {
    /// Where the session's save states are, and whose a file there is.
    states: StateFiles,
    /// Where an RPG Maker or 3DS game keeps the saves it makes itself.
    own_saves: OwnSaves,
    /// Per cartridge: the backup generation whose save is on disk or on
    /// its way there, or `None` if the cartridge has no battery or its
    /// last write failed.
    flushed_generations: Vec<Option<u64>>,
    /// Per cartridge: the backup generation of the save last handed to
    /// `writer`, if one has been. It falls behind `flushed_generations`
    /// when a save is not handed over for being the same as the last.
    handed_generations: Vec<Option<u64>>,
    /// Per cartridge: the save last handed to `writer`, or the one the
    /// game started with, which is what the file holds; `None` once a
    /// write has failed, or without a battery. A save the same as this is
    /// not written again, whatever the core counted: loading a state, as
    /// picking up a checkpoint does at every start, counts as a write
    /// though it often changes nothing.
    last_handed: Vec<Option<Vec<u8>>>,
    /// Per cartridge: its battery save file.
    backup_paths: Vec<PathBuf>,
    /// When a state was last loaded, if one has been: by the clock on the
    /// wall, which goes on while the computer sleeps, so a load the next
    /// morning is no part of last night's run.
    last_load: Option<SystemTime>,
    /// The number of the run of loads the last load belongs to.
    run: u64,
    /// Per cartridge: whether a load of that run has kept the save on
    /// disk, or found it kept already.
    kept_in_run: Vec<bool>,
    /// Per cartridge: the run of the load its save has not been handed to
    /// `writer` since, if that load may keep the file on disk. That write
    /// keeps it first if it holds another save.
    after_load: Vec<Option<u64>>,
    /// Writes the battery saves to disk.
    writer: SaveWriter,
    /// How many writes handed to `writer` have not been heard of since.
    unheard: usize,
    /// What there is to tell the player, oldest first, until it is taken.
    said: Vec<String>,
    /// What the player must not miss, oldest first, until it is taken.
    warned: Vec<String>,
    /// Per cartridge: the warning of its last write, if that failed.
    last_failure: Vec<Option<String>>,
}

impl Saves {
    /// The saves of the game in `loaded`, whose battery saves are taken to
    /// be on disk as its core has them.
    ///
    /// # Errors
    ///
    /// Returns the error if the thread that writes them cannot start.
    pub(super) fn new(loaded: &Loaded) -> std::io::Result<Self> {
        Ok(Self {
            states: loaded.states(),
            own_saves: loaded.own_saves.clone(),
            flushed_generations: generations(&*loaded.core, loaded.rom_paths.len()),
            handed_generations: vec![None; loaded.rom_paths.len()],
            last_handed: backups_on_disk(&*loaded.core, loaded.rom_paths.len()),
            backup_paths: loaded.files.iter().map(GameFiles::backup).collect(),
            last_load: None,
            run: 0,
            kept_in_run: vec![false; loaded.rom_paths.len()],
            after_load: vec![None; loaded.rom_paths.len()],
            writer: SaveWriter::start()?,
            unheard: 0,
            said: Vec::new(),
            warned: Vec::new(),
            last_failure: vec![None; loaded.rom_paths.len()],
        })
    }

    /// What there is to tell the player since last asked, oldest first.
    pub(super) fn take_said(&mut self) -> Vec<String> {
        std::mem::take(&mut self.said)
    }

    /// What the player must not miss since last asked, oldest first: to
    /// be shown for longer than a notice.
    pub(super) fn take_warned(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warned)
    }

    /// Whether a battery save handed to the writer has not been heard of
    /// since: [`Saves::hear`] will have how it went, once it is written.
    pub(super) fn writing(&self) -> bool {
        self.unheard > 0
    }

    /// Notes how the battery saves written since last asked went, to tell
    /// the player, without handing over any more: a failed one waits for
    /// the next [`Saves::flush`].
    pub(super) fn hear(&mut self) {
        self.take_written();
    }

    /// The header of a state `core` saves now in this session, which a
    /// state it loads must go with.
    fn header(&self, core: &dyn Core) -> emu_state::Header {
        let info = core.info();
        emu_state::Header {
            system: info.system,
            state_version: info.state_version,
            rom_crc32: self.states.rom_crc32(),
        }
    }

    /// Writes `core`'s state to `slot`'s file. A file there that is not a
    /// state of this session's, such as one the same game left with
    /// another cartridge in its slot, is set aside first, never replaced,
    /// and the player is told where it is.
    pub(super) fn save_state(&mut self, core: &dyn Core, slot: u8) -> anyhow::Result<()> {
        let info = core.info();
        let mut payload = Vec::new();
        core.try_save_state(&mut payload)
            .map_err(anyhow::Error::msg)?;
        let header = self.header(core);
        // At the screen's own size, whatever the 3D resolution.
        let screen = video::shrink(&core.screens()[0], info.screens[0]);
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
        let path = self.states.path(slot);
        let found = self
            .states
            .found(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if found == Found::Another {
            let kept = persist::set_aside(&path)
                .with_context(|| format!("cannot set {} aside", path.display()))?;
            if let Some(kept) = kept {
                self.said.push(format!(
                    "{} was another game's, or this one's with another cartridge in; it is kept as {}",
                    file_name(&path),
                    file_name(&kept)
                ));
            }
        }
        persist::write_atomic(
            &path,
            &emu_state::encode(&header, &payload, Some(thumbnail)),
        )
        .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Restores `core` from the state in `slot` (see
    /// [`StateFiles::to_read`]). The next write of each battery save
    /// follows a load, and keeps the file on disk if this load may.
    pub(super) fn load_state(&mut self, core: &mut dyn Core, slot: u8) -> anyhow::Result<()> {
        self.load_state_at(core, slot, SystemTime::now())
    }

    /// Restores `core` from the state in `slot`, as [`Saves::load_state`]
    /// does, at the time `now`.
    fn load_state_at(
        &mut self,
        core: &mut dyn Core,
        slot: u8,
        now: SystemTime,
    ) -> anyhow::Result<()> {
        let path = self.states.to_read(slot);
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let emu_state::Decoded {
            header, payload, ..
        } = emu_state::decode(&bytes)?;
        header.check(&self.header(core)).map_err(refusal)?;
        core.load_state(payload)?;
        // A clock set back gives no gap, so that load starts a run.
        let since_last = self
            .last_load
            .and_then(|last| now.duration_since(last).ok());
        self.last_load = Some(now);
        if starts_run(since_last) {
            self.run += 1;
            self.kept_in_run.fill(false);
        }
        for (after_load, kept) in self.after_load.iter_mut().zip(&self.kept_in_run) {
            if keeps_save(since_last, *kept) {
                *after_load = Some(self.run);
            }
        }
        Ok(())
    }

    /// Writes the battery saves and waits for them, then saves a state to
    /// `slot` and records beside it the saves on disk (see
    /// [`Saves::saves`]), which `load_checkpoint` checks. Nothing is
    /// saved unless every battery save was written, and the record comes
    /// last: should the state not be written, the one before keeps the
    /// record that matches it.
    pub(super) fn save_checkpoint(&mut self, core: &dyn Core, slot: u8) -> anyhow::Result<()> {
        // A record of older saves on disk would let a later start load
        // this state over them once they could be written again.
        ensure!(
            self.write_all(core),
            "the battery save could not be written"
        );
        self.save_state(core, slot)?;
        let state = self.states.path(slot);
        // The library's picture of where the game was left: the first
        // screen, a DS's top one. It shows behind the game last played,
        // and in place of box art libretro doesn't have.
        // Not save data, so written without waiting for the disk.
        if let Some(screen) = core.screens().first() {
            let png = capture::encode_png(screen);
            // A DS game played with a cartridge in is still that game in
            // the library, which looks beside the game's own checkpoint:
            // the picture goes there too, and its time is when the game
            // was last played.
            let own = persist::state_picture_path(&state);
            for picture in std::iter::once(own).chain(self.states.library_picture(slot)) {
                let unfinished = picture.with_extension("png.tmp");
                fs::write(&unfinished, &png)
                    .and_then(|()| fs::rename(&unfinished, &picture))
                    .ok();
            }
        }
        let record = persist::saves_record_path(&state);
        let saves =
            persist::saves_fingerprint(&self.saves()).context("cannot read the game's saves")?;
        persist::write_atomic(&record, saves.as_bytes())
            .with_context(|| format!("cannot write {}", record.display()))
    }

    /// Loads the state in `slot` if the saves on disk are those its record
    /// says it was saved with. A state saved before records were kept is
    /// loaded only if its battery saves are the same as the game's now,
    /// and otherwise undone; for a game that saves only when asked, which
    /// cannot be undone so, it is left alone. A file there that is not a
    /// state of this session's is no checkpoint of its.
    pub(super) fn load_checkpoint(
        &mut self,
        core: &mut dyn Core,
        slot: u8,
    ) -> anyhow::Result<PickUp> {
        let state = self.states.to_read(slot);
        let found = self
            .states
            .found(&state)
            .with_context(|| format!("cannot read {}", state.display()))?;
        if found != Found::Ours {
            return Ok(PickUp::Empty);
        }
        let record = persist::saves_record_path(&state);
        match fs::read_to_string(&record) {
            Ok(recorded) => {
                let saves = persist::saves_fingerprint(&self.saves())
                    .context("cannot read the game's saves")?;
                if recorded != saves {
                    return Ok(PickUp::SavesChanged);
                }
                self.load_state(core, slot)?;
                Ok(PickUp::Loaded)
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && !core.saves_every_frame() =>
            {
                Ok(PickUp::Empty)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let started = self.backups(core);
                let mut undo = Vec::new();
                core.save_state(&mut undo);
                self.load_state(core, slot)?;
                if self.backups(core) == started {
                    return Ok(PickUp::Loaded);
                }
                let undone = core.load_state(&undo);
                // The battery saves are back as on disk: nothing to write.
                self.flushed_generations = generations(core, self.backup_paths.len());
                self.after_load.fill(None);
                undone.context("cannot undo loading it")?;
                Ok(PickUp::SavesChanged)
            }
            Err(error) => Err(error).with_context(|| format!("cannot read {}", record.display())),
        }
    }

    /// The saves a checkpoint goes with: each cartridge's battery save,
    /// and the saves an RPG Maker game makes itself.
    fn saves(&self) -> Vec<PathBuf> {
        let mut saves = self.backup_paths.clone();
        saves.extend(self.own_saves.files());
        saves
    }

    /// Each cartridge's battery save as it is now.
    fn backups(&self, core: &dyn Core) -> Vec<Vec<u8>> {
        (0..self.backup_paths.len())
            .map(|cartridge| {
                let mut backup = Vec::new();
                core.save_backup(cartridge, &mut backup);
                backup
            })
            .collect()
    }

    /// Hands each cartridge's battery save that changed since the last
    /// flush to the writer, and retries any whose write failed. A save
    /// handed over for the first time since a state was loaded is handed
    /// over as such, whether the core counted a write or not, so the file
    /// it replaces is kept if it holds another save.
    /// A save whose bytes are those last handed over is on disk already,
    /// and is not written again.
    pub(super) fn flush(&mut self, core: &dyn Core) {
        self.take_written();
        let system = core.info().system;
        for cartridge in 0..self.flushed_generations.len() {
            let Some(generation) = core.backup_generation(cartridge) else {
                continue;
            };
            let load = self.after_load[cartridge].take();
            if load.is_none() && self.flushed_generations[cartridge] == Some(generation) {
                continue;
            }
            let mut backup = Vec::new();
            core.save_backup(cartridge, &mut backup);
            self.flushed_generations[cartridge] = Some(generation);
            if self.last_handed[cartridge].as_deref() == Some(backup.as_slice()) {
                continue;
            }
            self.last_handed[cartridge] = Some(backup.clone());
            self.handed_generations[cartridge] = Some(generation);
            let path = self.backup_paths[cartridge].clone();
            match load {
                Some(run) => {
                    self.writer
                        .write_after_load(cartridge, generation, path, backup, run, system);
                }
                None => self.writer.write(cartridge, generation, path, backup),
            }
            self.unheard += 1;
        }
    }

    /// Writes the battery saves that changed and waits for them. Returns
    /// whether every one is now on disk as the game has it.
    pub(super) fn write_all(&mut self, core: &dyn Core) -> bool {
        self.flush(core);
        self.writer.wait();
        self.take_written();
        // A failed write leaves its cartridge unflushed (see `take_written`).
        (0..self.flushed_generations.len()).all(|cartridge| {
            match core.backup_generation(cartridge) {
                Some(generation) => self.flushed_generations[cartridge] == Some(generation),
                None => true,
            }
        })
    }

    /// Notes the battery save writes finished since last asked: a save
    /// kept from before a load is warned of, once, with where it is; a
    /// failed write is warned of, unless the write before it failed with
    /// the same words, and written again at the next flush unless a newer
    /// one is on its way.
    fn take_written(&mut self) {
        for written in self.writer.finished() {
            self.unheard = self.unheard.saturating_sub(1);
            // The run of loads has kept its save, if this is still that
            // run: a write heard of late, after a load has begun another,
            // says nothing of the new one.
            if written.kept_run == Some(self.run) {
                self.kept_in_run[written.cartridge] = true;
            }
            match &written.kept {
                Kept::Unneeded | Kept::Already => {}
                // A warning, which shows long enough to read: nothing else
                // says where the save went.
                Kept::New(kept) => {
                    self.warned.push(format!(
                        "Your save from before you loaded the state is kept as {}",
                        file_name(kept)
                    ));
                }
            }
            let Err(error) = written.result else {
                self.last_failure[written.cartridge] = None;
                continue;
            };
            let warning = format!("Could not write {}: {error}", written.path.display());
            let last = &mut self.last_failure[written.cartridge];
            if last.as_deref() != Some(warning.as_str()) {
                self.warned.push(warning.clone());
                *last = Some(warning);
            }
            // The last handed over: nothing newer is on its way. Saves the
            // same as it, not handed over since, are unwritten too.
            if self.handed_generations[written.cartridge] == Some(written.generation) {
                self.flushed_generations[written.cartridge] = None;
                // What the file holds now is anyone's guess.
                self.last_handed[written.cartridge] = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use emu_core::{
        CoreInfo, Frame, FrameRate, Input, ScreenSize, StateError, StereoSample, System,
    };

    use super::*;

    const SCREENS: [ScreenSize; 1] = [ScreenSize::new(1, 1)];

    /// A core with a one-byte battery save at address 0 and a byte of
    /// other progress at address 1, both kept in its save states.
    struct Battery {
        save: u8,
        progress: u8,
        generation: u64,
        frames: [Frame; 1],
    }

    impl Core for Battery {
        fn info(&self) -> CoreInfo {
            CoreInfo {
                system: System::Gba,
                screens: &SCREENS,
                touch_screen: None,
                players: 1,
                frame_rate: FrameRate::new(60, 1),
                audio_sample_rate: 48_000,
                state_version: 1,
            }
        }

        fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {}

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
    }

    /// A fresh, empty folder for `test`.
    fn empty_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-app-saves-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// The game `dir/game.gba`, a [`Battery`] whose save holds `save` as
    /// if read from disk, and its saves.
    fn game(dir: &Path, save: u8) -> (Box<dyn Core>, Saves) {
        let rom = dir.join("game.gba");
        let loaded = Loaded {
            core: Box::new(Battery {
                save,
                progress: 0,
                generation: 0,
                frames: [Frame::new(SCREENS[0])],
            }),
            files: vec![GameFiles::of(&rom)],
            rom_paths: vec![rom],
            rom_crc32: 0,
            live_sound: None,
            own_saves: OwnSaves::None,
        };
        let saves = Saves::new(&loaded).expect("the thread starts");
        (loaded.core, saves)
    }

    /// The save and the other progress in a [`Battery`].
    fn bytes(core: &dyn Core) -> (u8, u8) {
        (
            core.peek(0, 0).expect("the save"),
            core.peek(0, 1).expect("the progress"),
        )
    }

    #[test]
    fn battery_saves_are_written_when_they_change() {
        let dir = empty_dir("flush");
        let save = dir.join("game.sav");
        let (mut core, mut saves) = game(&dir, 0);
        // Nothing has changed: what the game started with is on disk.
        assert!(saves.write_all(&*core));
        assert!(!save.exists());
        core.poke(0, 0, 7);
        assert!(saves.write_all(&*core));
        assert_eq!(fs::read(&save).expect("written"), [7]);
        assert!(saves.take_said().is_empty());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_save_the_same_as_the_one_on_disk_is_not_written_again() {
        let dir = empty_dir("unchanged");
        let save = dir.join("game.sav");
        // The game writes its save memory with what it already holds, as
        // loading a state does: the core counts a write, and no file is
        // made for it.
        let (mut core, mut saves) = game(&dir, 7);
        core.poke(0, 0, 7);
        assert!(saves.write_all(&*core));
        assert!(!save.exists());
        // A save that differs is written, and then not again.
        core.poke(0, 0, 8);
        assert!(saves.write_all(&*core));
        assert_eq!(fs::read(&save).expect("written"), [8]);
        fs::remove_file(&save).expect("removed");
        core.poke(0, 0, 8);
        assert!(saves.write_all(&*core));
        assert!(!save.exists());
        // Going back to what the file last held is a change like any other.
        core.poke(0, 0, 7);
        assert!(saves.write_all(&*core));
        assert_eq!(fs::read(&save).expect("written"), [7]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_save_skipped_as_unchanged_is_still_written_if_the_write_before_it_fails() {
        let dir = empty_dir("unchanged-retry");
        let gone = dir.join("gone");
        let (mut core, mut saves) = game(&gone, 0);
        // Keeps the writer busy, so the save's own write is still waiting
        // when the game writes the same save again.
        saves
            .writer
            .write(0, u64::MAX, dir.join("busy.bin"), vec![0x55; 32 << 20]);
        core.poke(0, 0, 7);
        saves.flush(&*core);
        // The same bytes again, which are not handed over twice...
        core.poke(0, 0, 7);
        saves.flush(&*core);
        // ...and the one write of them fails: the folder is gone.
        assert!(!saves.write_all(&*core), "not on disk, and it says so");
        // Back again, the save is written without the game saving anew.
        fs::create_dir_all(&gone).expect("back");
        assert!(saves.write_all(&*core));
        assert_eq!(fs::read(gone.join("game.sav")).expect("written"), [7]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn picking_up_a_checkpoint_leaves_the_battery_save_file_as_it_was() {
        let dir = empty_dir("checkpoint-unwritten");
        let save = dir.join("game.sav");
        let (mut core, mut saves) = game(&dir, 0);
        core.poke(0, 0, 1);
        core.poke(0, 1, 7);
        saves.save_checkpoint(&*core, 0).expect("left");
        let written = fs::metadata(&save)
            .and_then(|file| file.modified())
            .expect("the save's time");
        // Long enough for any file system to tell a second write apart.
        std::thread::sleep(std::time::Duration::from_millis(20));

        // The next start picks the checkpoint up, which the core counts as
        // a save written, though it holds the save on disk.
        let (mut core, mut saves) = game(&dir, 1);
        assert_eq!(
            saves.load_checkpoint(&mut *core, 0).expect("picked up"),
            PickUp::Loaded
        );
        assert!(saves.write_all(&*core));
        let now = fs::metadata(&save)
            .and_then(|file| file.modified())
            .expect("the save's time");
        assert_eq!(now, written, "the library says when the game last saved");
        assert_eq!(fs::read(&save).expect("kept"), [1]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_save_that_cannot_be_written_is_warned_of_once_and_tried_again() {
        let dir = empty_dir("retry");
        // The game's folder is gone, as an unplugged drive's is.
        let gone = dir.join("gone");
        let (mut core, mut saves) = game(&gone, 0);
        core.poke(0, 0, 7);
        assert!(!saves.write_all(&*core));
        let warned = saves.take_warned();
        assert_eq!(warned.len(), 1, "{warned:?}");
        assert!(warned[0].starts_with("Could not write "), "{warned:?}");
        assert!(warned[0].contains("game.sav"), "{warned:?}");
        // Tried again at every flush, a second apart while the game runs,
        // and each time it fails the same way nothing more is said.
        for _ in 0..3 {
            assert!(!saves.write_all(&*core));
            assert!(saves.take_warned().is_empty());
        }
        assert!(saves.take_said().is_empty());
        // Back again: the same save is written without the game saving anew.
        fs::create_dir_all(&gone).expect("back");
        assert!(saves.write_all(&*core));
        assert_eq!(fs::read(gone.join("game.sav")).expect("written"), [7]);
        assert!(saves.take_warned().is_empty());
        // Gone once more: that is news again.
        fs::remove_dir_all(&gone).expect("gone again");
        core.poke(0, 0, 8);
        assert!(!saves.write_all(&*core));
        assert_eq!(saves.take_warned(), warned);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn states_round_trip_through_slot_files() {
        let dir = empty_dir("states");
        let (mut core, mut saves) = game(&dir, 1);
        core.poke(0, 1, 5);
        saves.save_state(&*core, 2).expect("saved");
        assert!(dir.join("game.ss2").exists());
        core.poke(0, 1, 9);
        saves.load_state(&mut *core, 2).expect("loaded");
        assert_eq!(bytes(&*core), (1, 5));
        let empty = saves.load_state(&mut *core, 3).expect_err("no state there");
        assert!(
            format!("{empty:#}").starts_with("cannot read "),
            "{empty:#}"
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_state_of_another_system_rom_or_version_is_refused_in_the_words_it_always_was() {
        let dir = empty_dir("refused");
        let (mut core, mut saves) = game(&dir, 1);
        core.poke(0, 1, 5);
        // What a state of this game's says it is for.
        let ours = emu_state::Header {
            system: System::Gba,
            state_version: 1,
            rom_crc32: 0,
        };
        let mut refused = |header: emu_state::Header| {
            fs::write(
                dir.join("game.ss2"),
                emu_state::encode(&header, &[9, 9], None),
            )
            .expect("write");
            let error = saves.load_state(&mut *core, 2).expect_err("refused");
            assert_eq!(bytes(&*core), (1, 5), "nothing of it was loaded");
            format!("{error:#}")
        };
        let other_system = emu_state::Header {
            system: System::Nds,
            ..ours
        };
        assert_eq!(refused(other_system), "it is a nds state");
        let other_rom = emu_state::Header {
            rom_crc32: 0xA,
            ..ours
        };
        assert_eq!(refused(other_rom), "it was saved from a different ROM");
        let other_version = emu_state::Header {
            state_version: 4,
            ..ours
        };
        assert_eq!(
            refused(other_version),
            "it is state version 4, this build reads 1"
        );
        // The first of the three is the one said: the system, then the ROM.
        let nothing_right = emu_state::Header {
            system: System::Gb,
            state_version: 4,
            rom_crc32: 0xA,
        };
        assert_eq!(refused(nothing_right), "it is a gb state");
        let rom_and_version = emu_state::Header {
            system: System::Gba,
            ..nothing_right
        };
        assert_eq!(
            refused(rom_and_version),
            "it was saved from a different ROM"
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The files in `dir` whose names start with `game.sav`, with what
    /// they hold.
    fn saves_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| file_name(path).starts_with("game.sav"))
            .map(|path| (file_name(&path), fs::read(&path).expect("read")))
            .collect();
        files.sort();
        files
    }

    /// `files` as [`saves_in`] gives them.
    fn held(files: &[(&str, u8)]) -> Vec<(String, Vec<u8>)> {
        let files = files.iter();
        files
            .map(|(name, byte)| ((*name).to_owned(), vec![*byte]))
            .collect()
    }

    const KEPT: &str = "Your save from before you loaded the state is kept as ";

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn only_the_first_load_of_a_run_that_changes_the_save_keeps_it() {
        // No clock: the rule is these two facts alone.
        let minutes = |minutes: u64| Some(Duration::from_secs(minutes * 60));
        // The session's first load, and any that starts a new run.
        assert!(keeps_save(None, false));
        assert!(keeps_save(minutes(10), true));
        assert!(keeps_save(minutes(11), true));
        assert!(keeps_save(minutes(600), true));
        // Within a run: until one load has kept the save, and not after.
        assert!(keeps_save(Some(Duration::ZERO), false));
        assert!(keeps_save(minutes(9), false));
        assert!(!keeps_save(Some(Duration::ZERO), true));
        assert!(!keeps_save(minutes(1), true));
        assert!(!keeps_save(Some(Duration::from_secs(599)), true));
        // A run ends ten minutes after its last load, however long it was.
        assert!(!starts_run(minutes(9)));
        assert!(starts_run(minutes(10)));
        assert!(starts_run(None));
    }

    #[test]
    fn loading_a_state_over_a_newer_save_keeps_that_save() {
        let dir = empty_dir("load-newer");
        let (mut core, mut saves) = game(&dir, 0);
        // Hour 2: saved in the game, and a state saved.
        core.poke(0, 0, 2);
        saves.save_state(&*core, 1).expect("saved");
        // Hour 20: saved in the game again.
        core.poke(0, 0, 20);
        assert!(saves.write_all(&*core));
        assert_eq!(saves_in(&dir), held(&[("game.sav", 20)]));

        // The state takes the game, and its save, back to hour 2.
        saves.load_state(&mut *core, 1).expect("loaded");
        assert!(saves.write_all(&*core));
        let kept = held(&[("game.sav", 2), ("game.sav.before-load", 20)]);
        assert_eq!(saves_in(&dir), kept);
        assert_eq!(saves.take_warned(), [format!("{KEPT}game.sav.before-load")]);

        // The same slot again changes nothing, so nothing more is kept.
        saves.load_state(&mut *core, 1).expect("loaded");
        assert!(saves.write_all(&*core));
        assert_eq!(saves_in(&dir), kept);
        assert!(saves.take_warned().is_empty());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn loading_a_state_with_the_save_on_disk_keeps_nothing() {
        let dir = empty_dir("load-same");
        let (mut core, mut saves) = game(&dir, 0);
        core.poke(0, 0, 2);
        core.poke(0, 1, 5);
        saves.save_state(&*core, 1).expect("saved");
        // Played on without saving in the game.
        core.poke(0, 1, 9);
        assert!(saves.write_all(&*core));
        saves.load_state(&mut *core, 1).expect("loaded");
        assert_eq!(bytes(&*core), (2, 5));
        assert!(saves.write_all(&*core));
        assert_eq!(saves_in(&dir), held(&[("game.sav", 2)]));
        // Nor does the save the game next makes of its own accord.
        core.poke(0, 0, 3);
        assert!(saves.write_all(&*core));
        assert_eq!(saves_in(&dir), held(&[("game.sav", 3)]));
        assert!(saves.take_warned().is_empty());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn twenty_loads_a_minute_apart_keep_only_the_save_from_before_the_first() {
        let dir = empty_dir("load-run");
        let (mut core, mut saves) = game(&dir, 0);
        core.poke(0, 0, 2);
        saves.save_state(&*core, 1).expect("saved");
        core.poke(0, 0, 20);
        assert!(saves.write_all(&*core));
        // Load, fight, load, fight: before each load the game has written
        // its save memory again, so every load finds another save on disk.
        let start = SystemTime::now();
        let first = held(&[("game.sav", 2), ("game.sav.before-load", 20)]);
        for turn in 0..20u8 {
            let now = start + MINUTE * u32::from(turn);
            saves.load_state_at(&mut *core, 1, now).expect("loaded");
            assert!(saves.write_all(&*core));
            assert_eq!(saves_in(&dir), first, "after load {turn}");
            core.poke(0, 0, 100 + turn);
            assert!(saves.write_all(&*core));
        }
        assert_eq!(saves.take_warned(), [format!("{KEPT}game.sav.before-load")]);

        // Eleven minutes after the last of them, a load starts a new run,
        // and keeps the save it finds.
        let later = start + MINUTE * 19 + MINUTE * 11;
        saves.load_state_at(&mut *core, 1, later).expect("loaded");
        assert!(saves.write_all(&*core));
        let second = held(&[
            ("game.sav", 2),
            ("game.sav.before-load", 119),
            ("game.sav.before-load2", 20),
        ]);
        assert_eq!(saves_in(&dir), second);
        assert_eq!(saves.take_warned(), [format!("{KEPT}game.sav.before-load")]);
        // Nine minutes on is the same run again.
        core.poke(0, 0, 50);
        assert!(saves.write_all(&*core));
        saves
            .load_state_at(&mut *core, 1, later + MINUTE * 9)
            .expect("loaded");
        assert!(saves.write_all(&*core));
        assert_eq!(saves_in(&dir), second);
        assert!(saves.take_warned().is_empty());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn alternating_two_slots_keeps_a_bounded_set_with_no_save_twice() {
        let dir = empty_dir("load-alternate");
        let (mut core, mut saves) = game(&dir, 0);
        for slot in [1, 2] {
            core.poke(0, 0, slot);
            saves.save_state(&*core, slot).expect("saved");
        }
        core.poke(0, 0, 9);
        assert!(saves.write_all(&*core));
        // Slot 1, slot 2, slot 1, each load long after the last, so each
        // is a run of its own and may keep the save it finds: the other
        // slot's, which is inside that slot's state too.
        let start = SystemTime::now();
        for turn in 0..12u32 {
            let slot = if turn % 2 == 0 { 1 } else { 2 };
            let now = start + MINUTE * 11 * turn;
            saves.load_state_at(&mut *core, slot, now).expect("loaded");
            assert!(saves.write_all(&*core));
        }
        // The save from before the first load, and each slot's once: the
        // one the last load replaced is the newest of them.
        assert_eq!(
            saves_in(&dir),
            held(&[
                ("game.sav", 2),
                ("game.sav.before-load", 1),
                ("game.sav.before-load2", 2),
                ("game.sav.before-load3", 9),
            ])
        );
        assert_eq!(saves.take_warned().len(), 3);
        fs::remove_dir_all(&dir).expect("clean up");

        // Alternated within one run, only the first load keeps a save.
        let dir = empty_dir("load-alternate-run");
        let (mut core, mut saves) = game(&dir, 0);
        for slot in [1, 2] {
            core.poke(0, 0, slot);
            saves.save_state(&*core, slot).expect("saved");
        }
        core.poke(0, 0, 9);
        assert!(saves.write_all(&*core));
        for turn in 0..12u32 {
            let slot = if turn % 2 == 0 { 1 } else { 2 };
            saves
                .load_state_at(&mut *core, slot, start + MINUTE * turn)
                .expect("loaded");
            assert!(saves.write_all(&*core));
        }
        assert_eq!(
            saves_in(&dir),
            held(&[("game.sav", 2), ("game.sav.before-load", 9)])
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_sixth_save_kept_drops_the_oldest_and_leaves_start_over_s_alone() {
        let dir = empty_dir("load-six");
        fs::write(dir.join("game.sav.bak"), [77]).expect("what Start Over set aside");
        let (mut core, mut saves) = game(&dir, 0);
        core.poke(0, 0, 2);
        saves.save_state(&*core, 1).expect("saved");
        let start = SystemTime::now();
        for run in 1..=6u8 {
            // Saved in the game, and long after the last load, the state
            // loaded over that save.
            core.poke(0, 0, 10 + run);
            assert!(saves.write_all(&*core));
            let now = start + MINUTE * 11 * u32::from(run);
            saves.load_state_at(&mut *core, 1, now).expect("loaded");
            assert!(saves.write_all(&*core));
        }
        assert_eq!(
            saves_in(&dir),
            held(&[
                ("game.sav", 2),
                ("game.sav.bak", 77),
                ("game.sav.before-load", 16),
                ("game.sav.before-load2", 15),
                ("game.sav.before-load3", 14),
                ("game.sav.before-load4", 13),
                ("game.sav.before-load5", 12),
            ])
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_save_kept_for_one_run_heard_of_in_the_next_does_not_settle_that_one() {
        let dir = empty_dir("load-late-report");
        let (mut core, mut saves) = game(&dir, 0);
        core.poke(0, 0, 2);
        saves.save_state(&*core, 1).expect("saved");
        core.poke(0, 0, 20);
        assert!(saves.write_all(&*core));
        let start = SystemTime::now();
        // A load keeps the save, but how its write went is not heard of
        // before the next run's first load, which changes nothing.
        saves.load_state_at(&mut *core, 1, start).expect("loaded");
        saves.flush(&*core);
        saves.writer.wait();
        saves
            .load_state_at(&mut *core, 1, start + MINUTE * 11)
            .expect("loaded");
        assert!(saves.write_all(&*core));
        assert_eq!(
            saves_in(&dir),
            held(&[("game.sav", 2), ("game.sav.before-load", 20)])
        );
        // Saved in the game, and the state loaded over that save a minute
        // later: the first load of its run to change the save keeps it.
        core.poke(0, 0, 30);
        assert!(saves.write_all(&*core));
        saves
            .load_state_at(&mut *core, 1, start + MINUTE * 12)
            .expect("loaded");
        assert!(saves.write_all(&*core));
        assert_eq!(
            saves_in(&dir),
            held(&[
                ("game.sav", 2),
                ("game.sav.before-load", 30),
                ("game.sav.before-load2", 20),
            ])
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_clock_set_back_starts_a_run_of_loads() {
        let dir = empty_dir("load-clock-back");
        let (mut core, mut saves) = game(&dir, 0);
        core.poke(0, 0, 2);
        saves.save_state(&*core, 1).expect("saved");
        core.poke(0, 0, 20);
        assert!(saves.write_all(&*core));
        let start = SystemTime::now();
        saves.load_state_at(&mut *core, 1, start).expect("loaded");
        assert!(saves.write_all(&*core));
        // Saved in the game, then a load that by the clock comes before
        // the last one: how long ago that was cannot be told, so it is no
        // part of its run, and keeps the save it replaces.
        core.poke(0, 0, 30);
        assert!(saves.write_all(&*core));
        saves
            .load_state_at(&mut *core, 1, start - MINUTE)
            .expect("loaded");
        assert!(saves.write_all(&*core));
        assert_eq!(
            saves_in(&dir),
            held(&[
                ("game.sav", 2),
                ("game.sav.before-load", 30),
                ("game.sav.before-load2", 20),
            ])
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn rewinding_past_a_save_made_in_the_game_keeps_no_copy() {
        let dir = empty_dir("rewind");
        let (mut core, mut saves) = game(&dir, 0);
        core.poke(0, 0, 1);
        let mut before = Vec::new();
        core.save_state(&mut before);
        // Saved in the game, and on disk.
        core.poke(0, 0, 3);
        assert!(saves.write_all(&*core));
        assert_eq!(saves_in(&dir), held(&[("game.sav", 3)]));
        // Rewind steps the core back itself, a moment at a time, with no
        // state loaded from a slot: the save is undone with all the rest.
        core.load_state(&before).expect("a state of its own");
        assert!(saves.write_all(&*core));
        assert_eq!(saves_in(&dir), held(&[("game.sav", 1)]));
        assert!(saves.take_warned().is_empty());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// A Game Boy game on a cartridge with a clock and 32 KiB of save
    /// memory (MBC3), which writes `value` to that memory and then waits.
    fn clock_game(value: u8) -> Box<dyn Core> {
        let mut rom = vec![0; 0x8000];
        // LD A, 0x0A; LD (0x0000), A enables the memory; LD A, value;
        // LD (0xA000), A writes it; JR -2 waits.
        let program = [
            0x3E, 0x0A, 0xEA, 0x00, 0x00, 0x3E, value, 0xEA, 0x00, 0xA0, 0x18, 0xFE,
        ];
        rom[0x100..0x100 + program.len()].copy_from_slice(&program);
        rom[0x147] = 0x10;
        rom[0x149] = 0x03;
        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        emu_cores::create(System::Gb, &emu_core::Media::new(&rom)).expect("valid rom")
    }

    #[test]
    fn a_clock_s_time_alone_never_keeps_a_save() {
        let dir = empty_dir("clock");
        let save = dir.join("game.sav");
        // The saves of any game will do, told they are a Game Boy game's:
        // they are handed the core.
        let (_, mut saves) = game(&dir, 0);
        let files = GameFiles::of(&dir.join("game.gba"));
        saves.states = StateFiles::new(files, false, None, System::Gb, 0);
        let mut core = clock_game(0x42);
        let mut sound = Vec::new();
        let mut run = |core: &mut dyn Core, frames: usize| {
            for _ in 0..frames {
                core.run_frame(&[Input::default()], &mut sound);
                sound.clear();
            }
        };
        // The game saves, and the save reaches the disk with the time.
        run(&mut *core, 5);
        assert!(saves.write_all(&*core));
        let first = fs::read(&save).expect("written");
        assert_eq!((first.len(), first[0]), (0x8000 + 48, 0x42));

        // Three seconds on, a state is saved; then a second more.
        run(&mut *core, 180);
        saves.save_state(&*core, 1).expect("saved");
        run(&mut *core, 60);
        // The state loaded: the save memory is as on disk, and only the
        // clock's time is another.
        saves.load_state(&mut *core, 1).expect("loaded");
        assert!(saves.write_all(&*core));
        let second = fs::read(&save).expect("written");
        assert_ne!(second, first, "the clock's time is kept up to date");
        assert_eq!(second[..0x8000], first[..0x8000]);
        assert_eq!(saves_in(&dir).len(), 1, "and no copy was kept");
        assert!(saves.take_warned().is_empty());

        // A state whose save memory differs is another matter, and though
        // it is of the same run of loads, none has kept the save yet.
        let mut other = clock_game(0x43);
        run(&mut *other, 5);
        saves.save_state(&*other, 2).expect("saved");
        saves.load_state(&mut *core, 2).expect("loaded");
        assert!(saves.write_all(&*core));
        assert_eq!(
            fs::read(dir.join("game.sav.before-load")).expect("kept"),
            second
        );
        assert_eq!(saves.take_warned(), [format!("{KEPT}game.sav.before-load")]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The saves of a [`Battery`] as [`game`] makes it, but one whose
    /// states say they are for the ROMs `rom_crc32`, with the game at
    /// `cartridge` in its second slot.
    fn session(
        dir: &Path,
        save: u8,
        rom_crc32: u32,
        cartridge: Option<&str>,
    ) -> (Box<dyn Core>, Saves) {
        let (core, mut saves) = game(dir, save);
        let cartridge = cartridge.map(|name| dir.join(name));
        saves.states = StateFiles::new(
            GameFiles::of(&dir.join("game.gba")),
            false,
            cartridge.as_deref(),
            core.info().system,
            rom_crc32,
        );
        (core, saves)
    }

    /// Every file in `dir`, by name.
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| file_name(&entry.expect("entry").path()))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_game_with_a_cartridge_in_keeps_states_and_a_checkpoint_of_its_own() {
        let dir = empty_dir("slotted");
        // The game alone saves a state and leaves a checkpoint.
        let (mut alone, mut alone_saves) = session(&dir, 0, 0xA, None);
        alone.poke(0, 1, 1);
        alone_saves.save_state(&*alone, 1).expect("saved");
        alone_saves.save_checkpoint(&*alone, 0).expect("left");
        let alone_state = fs::read(dir.join("game.ss1")).expect("its state");
        let alone_checkpoint = fs::read(dir.join("game.ss0")).expect("its checkpoint");

        // With a cartridge in, the same slots are other files.
        let (mut slotted, mut slotted_saves) = session(&dir, 0, 0xB, Some("emerald.gba"));
        assert_eq!(
            slotted_saves
                .load_checkpoint(&mut *slotted, 0)
                .expect("none of its own"),
            PickUp::Empty
        );
        assert!(slotted_saves.load_state(&mut *slotted, 1).is_err());
        slotted.poke(0, 1, 2);
        slotted_saves.save_state(&*slotted, 1).expect("saved");
        slotted_saves.save_checkpoint(&*slotted, 0).expect("left");
        assert_eq!(
            names_in(&dir),
            [
                "game.emerald.ss0",
                "game.emerald.ss0.png",
                "game.emerald.ss0.saves",
                "game.emerald.ss1",
                "game.ss0",
                "game.ss0.png",
                "game.ss0.saves",
                "game.ss1",
            ]
        );
        assert_eq!(fs::read(dir.join("game.ss1")).expect("kept"), alone_state);
        assert_eq!(
            fs::read(dir.join("game.ss0")).expect("kept"),
            alone_checkpoint
        );
        assert!(slotted_saves.take_said().is_empty());

        // Each picks up its own.
        let (mut alone, mut alone_saves) = session(&dir, 0, 0xA, None);
        assert_eq!(
            alone_saves
                .load_checkpoint(&mut *alone, 0)
                .expect("its own"),
            PickUp::Loaded
        );
        assert_eq!(bytes(&*alone), (0, 1));
        let (mut slotted, mut slotted_saves) = session(&dir, 0, 0xB, Some("emerald.gba"));
        assert_eq!(
            slotted_saves
                .load_checkpoint(&mut *slotted, 0)
                .expect("its own"),
            PickUp::Loaded
        );
        assert_eq!(bytes(&*slotted), (0, 2));
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_game_played_with_a_cartridge_in_leaves_its_picture_where_the_library_looks() {
        let dir = empty_dir("slotted-picture");
        // Never played alone: only with the cartridge in.
        let (mut slotted, mut saves) = session(&dir, 0, 0xB, Some("emerald.gba"));
        slotted.poke(0, 1, 2);
        saves.save_checkpoint(&*slotted, 0).expect("left");
        assert_eq!(
            names_in(&dir),
            [
                "game.emerald.ss0",
                "game.emerald.ss0.png",
                "game.emerald.ss0.saves",
                // The game's tile in the library: its picture, and by the
                // picture's time, when it was last played.
                "game.ss0.png",
            ]
        );
        assert_eq!(
            fs::read(dir.join("game.ss0.png")).expect("the tile's picture"),
            fs::read(dir.join("game.emerald.ss0.png")).expect("the session's")
        );
        // The game alone has no checkpoint of its own for all that.
        let (mut alone, mut alone_saves) = session(&dir, 0, 0xA, None);
        assert_eq!(
            alone_saves.load_checkpoint(&mut *alone, 0).expect("none"),
            PickUp::Empty
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn states_from_before_the_names_were_apart_load_and_are_never_replaced_unsaid() {
        let dir = empty_dir("slotted-former");
        // As older builds left them: the game with a cartridge in saved
        // slot 1 and its checkpoint under the game's own names.
        let (mut slotted, mut old_saves) = session(&dir, 0, 0xB, None);
        slotted.poke(0, 1, 7);
        old_saves.save_state(&*slotted, 1).expect("saved");
        old_saves.save_checkpoint(&*slotted, 0).expect("left");
        let old_state = fs::read(dir.join("game.ss1")).expect("the old state");
        let old_checkpoint = fs::read(dir.join("game.ss0")).expect("the old checkpoint");

        // The game with its cartridge still loads both.
        let (mut slotted, mut slotted_saves) = session(&dir, 0, 0xB, Some("emerald.gba"));
        assert_eq!(
            slotted_saves
                .load_checkpoint(&mut *slotted, 0)
                .expect("the old one"),
            PickUp::Loaded
        );
        assert_eq!(bytes(&*slotted), (0, 7));
        slotted.poke(0, 1, 0);
        slotted_saves
            .load_state(&mut *slotted, 1)
            .expect("the old one");
        assert_eq!(bytes(&*slotted), (0, 7));

        // The game alone does not take the checkpoint for its own, and
        // says nothing untrue about its battery save.
        let (mut alone, mut alone_saves) = session(&dir, 0, 0xA, None);
        assert_eq!(
            alone_saves
                .load_checkpoint(&mut *alone, 0)
                .expect("not its own"),
            PickUp::Empty
        );
        let refused = alone_saves
            .load_state(&mut *alone, 1)
            .expect_err("not its own");
        assert_eq!(format!("{refused:#}"), "it was saved from a different ROM");
        // Saving there sets the other's state aside, and says so.
        alone_saves.save_state(&*alone, 1).expect("saved");
        assert_eq!(fs::read(dir.join("game.ss1.bak")).expect("kept"), old_state);
        assert_eq!(
            alone_saves.take_said(),
            [
                "game.ss1 was another game's, or this one's with another cartridge in; it is kept as game.ss1.bak"
            ]
        );
        alone_saves.save_checkpoint(&*alone, 0).expect("left");
        assert_eq!(
            fs::read(dir.join("game.ss0.bak")).expect("kept"),
            old_checkpoint
        );
        assert_eq!(alone_saves.take_said().len(), 1);
        // Its own states it replaces as ever.
        alone_saves.save_state(&*alone, 1).expect("saved");
        alone_saves.save_checkpoint(&*alone, 0).expect("left");
        assert!(alone_saves.take_said().is_empty());
        assert!(!dir.join("game.ss1.bak2").exists());
        assert!(!dir.join("game.ss0.bak2").exists());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_checkpoint_goes_with_the_saves_it_was_left_with() {
        let dir = empty_dir("checkpoint");
        let save = dir.join("game.sav");
        let (mut core, mut saves) = game(&dir, 0);
        assert_eq!(
            saves.load_checkpoint(&mut *core, 0).expect("none"),
            PickUp::Empty
        );
        core.poke(0, 0, 1);
        core.poke(0, 1, 7);
        saves.save_checkpoint(&*core, 0).expect("saved");
        assert_eq!(fs::read(&save).expect("written first"), [1]);
        assert_eq!(
            fs::read_to_string(dir.join("game.ss0.saves")).expect("its record"),
            format!("game.sav {:08X}\n", crc32fast::hash(&[1]))
        );
        assert!(dir.join("game.ss0.png").exists(), "the library's picture");

        // The game starts again from its battery save, and picks it up.
        let (mut core, mut saves) = game(&dir, 1);
        assert_eq!(
            saves.load_checkpoint(&mut *core, 0).expect("picked up"),
            PickUp::Loaded
        );
        assert_eq!(bytes(&*core), (1, 7));

        // A save made since is never rolled back.
        fs::write(&save, [2]).expect("saved since");
        let (mut core, mut saves) = game(&dir, 2);
        assert_eq!(
            saves.load_checkpoint(&mut *core, 0).expect("left alone"),
            PickUp::SavesChanged
        );
        assert_eq!(bytes(&*core), (2, 0));
        assert!(saves.write_all(&*core));
        assert_eq!(fs::read(&save).expect("kept"), [2]);
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
