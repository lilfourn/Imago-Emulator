//! A running game's battery saves, save states and checkpoint: what the
//! emulation thread keeps on disk, apart from the core that runs the game,
//! which every call is handed.
//!
//! Battery saves are copied from the core when they change and written on a
//! thread of their own (see `save_writer.rs`). What there is to tell the
//! player, such as a save that could not be written, is kept until
//! [`Saves::take_said`] is asked for it.
//!
//! Going back to an earlier moment, by loading a state or rewinding, takes
//! the game's save memory back too, and the cores count that as a save
//! written. So that the file on disk, which may hold hours more, is not
//! lost to it, the first write after each jump back sets the file aside if
//! it holds anything else (`game.sav.bak`, never replacing an earlier one),
//! and the player is told where it is. A jump back that leaves the save as
//! it is on disk sets nothing aside.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use emu_core::Core;

use super::{Loaded, OwnSaves};
use crate::capture;
use crate::persist::{self, GameFiles, file_name};
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

/// Each of the first `cartridges` cartridges' backup generation in
/// `core`, taken to be on disk already: 0 for one without a battery.
fn generations(core: &dyn Core, cartridges: usize) -> Vec<Option<u64>> {
    (0..cartridges)
        .map(|cartridge| Some(core.backup_generation(cartridge).unwrap_or(0)))
        .collect()
}

/// A running game's battery saves, save states and checkpoint.
pub(super) struct Saves {
    /// The first player's game's files, which name the session's states.
    states: GameFiles,
    /// Whether the session links several players, whose states are named
    /// apart from one player's.
    linked: bool,
    /// Identifies the ROMs in save states.
    rom_crc32: u32,
    /// Where an RPG Maker or 3DS game keeps the saves it makes itself.
    own_saves: OwnSaves,
    /// Per cartridge: the backup generation last handed to `writer`, or
    /// `None` if the cartridge has no battery or its last write failed.
    flushed_generations: Vec<Option<u64>>,
    /// Per cartridge: its battery save file.
    backup_paths: Vec<PathBuf>,
    /// How many times the game has gone back to an earlier moment: once
    /// for each state loaded, and once for each stretch of rewinding.
    jumps: u64,
    /// Per cartridge: the jump back its save has not been handed to
    /// `writer` since, if there is one. That write sets the file on disk
    /// aside first if it holds another save.
    went_back: Vec<Option<u64>>,
    /// Writes the battery saves to disk.
    writer: SaveWriter,
    /// How many writes handed to `writer` have not been heard of since.
    unheard: usize,
    /// What there is to tell the player, oldest first, until it is taken.
    said: Vec<String>,
}

impl Saves {
    /// The saves of the game in `loaded`, whose battery saves are taken to
    /// be on disk as its core has them.
    pub(super) fn new(loaded: &Loaded) -> Self {
        Self {
            states: loaded.files[0].clone(),
            linked: loaded.core.info().players > 1,
            rom_crc32: loaded.rom_crc32,
            own_saves: loaded.own_saves.clone(),
            flushed_generations: generations(&*loaded.core, loaded.rom_paths.len()),
            backup_paths: loaded.files.iter().map(GameFiles::backup).collect(),
            jumps: 0,
            went_back: vec![None; loaded.rom_paths.len()],
            writer: SaveWriter::start(),
            unheard: 0,
            said: Vec::new(),
        }
    }

    /// What there is to tell the player since last asked, oldest first.
    pub(super) fn take_said(&mut self) -> Vec<String> {
        std::mem::take(&mut self.said)
    }

    /// Whether a battery save handed to the writer has not been heard of
    /// since: [`Saves::flush`] will have how it went, once it is written.
    pub(super) fn writing(&self) -> bool {
        self.unheard > 0
    }

    /// The session's save state file in `slot`.
    fn state_path(&self, slot: u8) -> PathBuf {
        self.states.state(slot, self.linked)
    }

    /// Writes `core`'s state to `slot`'s file.
    pub(super) fn save_state(&self, core: &dyn Core, slot: u8) -> anyhow::Result<()> {
        let info = core.info();
        let mut payload = Vec::new();
        core.try_save_state(&mut payload)
            .map_err(anyhow::Error::msg)?;
        let header = emu_state::Header {
            system: info.system,
            state_version: info.state_version,
            rom_crc32: self.rom_crc32,
        };
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
        let path = self.state_path(slot);
        persist::write_atomic(
            &path,
            &emu_state::encode(&header, &payload, Some(thumbnail)),
        )
        .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Notes that rewinding has begun: every step back until it next
    /// begins is part of one jump back (see [`Saves::rewound`]).
    pub(super) fn rewind_began(&mut self) {
        self.jumps += 1;
    }

    /// Notes that the game has just stepped back through rewind, so the
    /// next write of each battery save follows a jump back.
    pub(super) fn rewound(&mut self) {
        self.went_back.fill(Some(self.jumps));
    }

    /// Restores `core` from the state in `slot`'s file. The next write of
    /// each battery save follows a jump back.
    pub(super) fn load_state(&mut self, core: &mut dyn Core, slot: u8) -> anyhow::Result<()> {
        let path = self.state_path(slot);
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let emu_state::Decoded {
            header, payload, ..
        } = emu_state::decode(&bytes)?;
        let info = core.info();
        ensure!(
            header.system == info.system,
            "it is a {} state",
            header.system
        );
        ensure!(
            header.rom_crc32 == self.rom_crc32,
            "it was saved from a different ROM"
        );
        ensure!(
            header.state_version == info.state_version,
            "it is state version {}, this build reads {}",
            header.state_version,
            info.state_version,
        );
        core.load_state(payload)?;
        self.jumps += 1;
        self.went_back.fill(Some(self.jumps));
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
        let state = self.state_path(slot);
        // The library's picture of where the game was left: the first
        // screen, a DS's top one. It shows behind the game last played,
        // and in place of box art libretro doesn't have.
        // Not save data, so written without waiting for the disk.
        if let Some(screen) = core.screens().first() {
            let picture = persist::state_picture_path(&state);
            let unfinished = picture.with_extension("png.tmp");
            fs::write(&unfinished, capture::encode_png(screen))
                .and_then(|()| fs::rename(&unfinished, &picture))
                .ok();
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
    /// cannot be undone so, it is left alone.
    pub(super) fn load_checkpoint(
        &mut self,
        core: &mut dyn Core,
        slot: u8,
    ) -> anyhow::Result<PickUp> {
        let state = self.state_path(slot);
        if !state.exists() {
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
                self.went_back.fill(None);
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
    /// handed over for the first time since the game went back to an
    /// earlier moment is handed over as such, whether the core counted a
    /// write or not, so the file it replaces is set aside if it differs.
    pub(super) fn flush(&mut self, core: &dyn Core) {
        self.take_written();
        for cartridge in 0..self.flushed_generations.len() {
            let Some(generation) = core.backup_generation(cartridge) else {
                continue;
            };
            let jump = self.went_back[cartridge].take();
            if jump.is_none() && self.flushed_generations[cartridge] == Some(generation) {
                continue;
            }
            let mut backup = Vec::new();
            core.save_backup(cartridge, &mut backup);
            let path = self.backup_paths[cartridge].clone();
            match jump {
                Some(jump) => {
                    self.writer
                        .write_after_jump(cartridge, generation, path, backup, jump);
                }
                None => self.writer.write(cartridge, generation, path, backup),
            }
            self.unheard += 1;
            self.flushed_generations[cartridge] = Some(generation);
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
    /// set aside is said, once, with where it is; a failed write is said,
    /// and written again at the next flush unless a newer one is on its
    /// way.
    fn take_written(&mut self) {
        for written in self.writer.finished() {
            self.unheard = self.unheard.saturating_sub(1);
            if let Some(kept) = &written.kept {
                self.said.push(format!(
                    "Your save from before you went back is kept as {}",
                    file_name(kept)
                ));
            }
            let Err(error) = written.result else {
                continue;
            };
            self.said.push(format!(
                "Could not write {}: {error}",
                written.path.display()
            ));
            let flushed = &mut self.flushed_generations[written.cartridge];
            if *flushed == Some(written.generation) {
                *flushed = None;
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
        let saves = Saves::new(&loaded);
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
    fn a_save_that_cannot_be_written_is_said_and_tried_again() {
        let dir = empty_dir("retry");
        // The game's folder is gone, as an unplugged drive's is.
        let gone = dir.join("gone");
        let (mut core, mut saves) = game(&gone, 0);
        core.poke(0, 0, 7);
        assert!(!saves.write_all(&*core));
        let said = saves.take_said();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].starts_with("Could not write "), "{said:?}");
        assert!(said[0].contains("game.sav"), "{said:?}");
        // Back again: the same save is written without the game saving anew.
        fs::create_dir_all(&gone).expect("back");
        assert!(saves.write_all(&*core));
        assert_eq!(fs::read(gone.join("game.sav")).expect("written"), [7]);
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

    const KEPT: &str = "Your save from before you went back is kept as ";

    #[test]
    fn loading_a_state_over_a_newer_save_sets_that_save_aside() {
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
        assert_eq!(
            saves_in(&dir),
            held(&[("game.sav", 2), ("game.sav.bak", 20)])
        );
        assert_eq!(saves.take_said(), [format!("{KEPT}game.sav.bak")]);

        // The same slot again changes nothing, so nothing more is kept.
        saves.load_state(&mut *core, 1).expect("loaded");
        assert!(saves.write_all(&*core));
        assert_eq!(
            saves_in(&dir),
            held(&[("game.sav", 2), ("game.sav.bak", 20)])
        );
        assert!(saves.take_said().is_empty());

        // Saving in the game from there is the game's own doing.
        core.poke(0, 0, 3);
        assert!(saves.write_all(&*core));
        assert_eq!(
            saves_in(&dir),
            held(&[("game.sav", 3), ("game.sav.bak", 20)])
        );
        // Loading over that save keeps it too, beside the first.
        saves.load_state(&mut *core, 1).expect("loaded");
        assert!(saves.write_all(&*core));
        assert_eq!(
            saves_in(&dir),
            held(&[("game.sav", 2), ("game.sav.bak", 20), ("game.sav.bak2", 3)])
        );
        assert_eq!(saves.take_said(), [format!("{KEPT}game.sav.bak2")]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn loading_a_state_with_the_save_on_disk_sets_nothing_aside() {
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
        assert!(saves.take_said().is_empty());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn rewinding_past_a_save_sets_it_aside_once_for_the_whole_rewind() {
        let dir = empty_dir("rewind");
        let (mut core, mut saves) = game(&dir, 0);
        // Three moments rewind will step back through.
        let mut moments = Vec::new();
        for save in [1, 2, 3] {
            core.poke(0, 0, save);
            let mut state = Vec::new();
            core.save_state(&mut state);
            moments.push(state);
        }
        assert!(saves.write_all(&*core));
        assert_eq!(saves_in(&dir), held(&[("game.sav", 3)]));

        // Held down past the save at 3, the saves written a second apart.
        saves.rewind_began();
        for (moment, save) in [(&moments[1], 2), (&moments[0], 1)] {
            core.load_state(moment).expect("a state of its own");
            saves.rewound();
            assert!(saves.write_all(&*core));
            assert_eq!(
                saves_in(&dir),
                held(&[("game.sav", save), ("game.sav.bak", 3)])
            );
        }
        assert_eq!(saves.take_said(), [format!("{KEPT}game.sav.bak")]);

        // Rewinding that passes no save keeps nothing.
        saves.rewind_began();
        core.load_state(&moments[0]).expect("a state of its own");
        saves.rewound();
        assert!(saves.write_all(&*core));
        assert_eq!(
            saves_in(&dir),
            held(&[("game.sav", 1), ("game.sav.bak", 3)])
        );
        assert!(saves.take_said().is_empty());
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
