//! Writes battery saves to disk on a thread of their own, so a slow disk
//! never holds up a frame: the emulation thread copies a save's bytes and
//! hands them over. Writes happen in the order handed over, and each one
//! reports back how it went.
//!
//! A save written after a save state was loaded may be older than the file
//! it replaces, which may be the only copy of the player's latest save. So
//! such a write first keeps the file, with [`persist::keep_before_load`],
//! if it holds another save: once for each run of loads (see `saves.rs`),
//! however many of them change the save. The file is copied, not moved, so
//! a write that then fails, as on a full disk, leaves the save where it
//! was.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use emu_core::System;

use crate::persist::{self, Kept};
use crate::worker::Worker;

/// A battery save written, or not.
#[derive(Debug)]
pub struct Written {
    /// The cartridge whose save it is.
    pub cartridge: usize,
    /// The cartridge's backup generation the bytes are from.
    pub generation: u64,
    /// Where it went.
    pub path: PathBuf,
    /// What became of the file there before, if this was the first write
    /// after a state was loaded: kept, if it held another save.
    pub kept: Kept,
    /// The run of loads the file was kept for, or found kept already for,
    /// if it was. A write that failed and was tried again reports the run
    /// of the load it first followed, which may be over by then.
    pub kept_run: Option<u64>,
    /// How it went.
    pub result: io::Result<()>,
}

/// What the writing thread is asked to do.
enum Job {
    /// Write a battery save, and report how it went.
    Write {
        cartridge: usize,
        generation: u64,
        path: PathBuf,
        bytes: Vec<u8>,
        /// The run of state loads this is the first write after one of,
        /// if it is, and the system the save is for.
        load: Option<(u64, System)>,
    },
    /// Say when every write before this one is done.
    Wait(Sender<()>),
}

/// What the writing thread remembers of one cartridge's save between
/// writes, to keep the file once for each run of state loads.
#[derive(Debug, Default)]
struct Guard {
    /// The run of loads whose write failed, so the next write is checked
    /// in its place.
    owed: Option<(u64, System)>,
    /// The run of loads the file was last kept for, or found kept already:
    /// only the first load of a run that would change the save keeps it.
    kept_for: Option<u64>,
}

impl Guard {
    /// Writes `bytes` to `path`. If this is the first write after a state
    /// load of the run `load`, or stands in for one that failed, the file
    /// there is kept first if it holds another save and none was kept for
    /// the run yet, and nothing is written unless that worked. Returns
    /// what became of the file, and how the write went.
    fn write(
        &mut self,
        path: &Path,
        bytes: &[u8],
        load: Option<(u64, System)>,
    ) -> (Kept, io::Result<()>) {
        self.write_with(path, bytes, load, persist::write_atomic)
    }

    /// Writes as [`Guard::write`] does, with `write` to replace the file.
    fn write_with(
        &mut self,
        path: &Path,
        bytes: &[u8],
        load: Option<(u64, System)>,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
    ) -> (Kept, io::Result<()>) {
        let load = load.or(self.owed);
        let kept = match load {
            Some((run, system)) if self.kept_for != Some(run) => {
                persist::keep_before_load(path, bytes, system)
            }
            _ => Ok(Kept::Unneeded),
        };
        let (kept, result) = match kept {
            Ok(kept) => {
                if kept != Kept::Unneeded {
                    self.kept_for = load.map(|(run, _)| run);
                }
                (kept, write(path, bytes))
            }
            Err(error) => (Kept::Unneeded, Err(error)),
        };
        self.owed = load.filter(|_| result.is_err());
        (kept, result)
    }
}

/// The battery save writing thread. Dropping it waits for every write
/// handed over.
pub struct SaveWriter {
    /// Dropped first, so the thread finishes its writes and stops while
    /// `written` still takes their reports.
    thread: Worker<Job>,
    written: Receiver<Written>,
}

impl SaveWriter {
    /// Starts the thread.
    ///
    /// # Errors
    ///
    /// Returns the error if the system will not start a thread.
    pub fn start() -> io::Result<Self> {
        let (report, written) = mpsc::channel();
        let thread = Worker::start("battery saves", move |jobs| {
            let mut guards: HashMap<usize, Guard> = HashMap::new();
            for job in jobs {
                match job {
                    Job::Write {
                        cartridge,
                        generation,
                        path,
                        bytes,
                        load,
                    } => {
                        let guard = guards.entry(cartridge).or_default();
                        let (kept, result) = guard.write(&path, &bytes, load);
                        // Set by the write just made, if it kept.
                        let kept_run = guard.kept_for.filter(|_| kept != Kept::Unneeded);
                        report
                            .send(Written {
                                cartridge,
                                generation,
                                path,
                                kept,
                                kept_run,
                                result,
                            })
                            .ok();
                    }
                    Job::Wait(done) => {
                        done.send(()).ok();
                    }
                }
            }
        })?;
        Ok(Self { thread, written })
    }

    /// Hands over cartridge `cartridge`'s battery save, `bytes` as of
    /// backup generation `generation`, to be written to `path`.
    pub fn write(&self, cartridge: usize, generation: u64, path: PathBuf, bytes: Vec<u8>) {
        self.thread.send(Job::Write {
            cartridge,
            generation,
            path,
            bytes,
            load: None,
        });
    }

    /// Hands over a battery save as [`SaveWriter::write`] does, as the
    /// first since a save state was loaded: `run` numbers the run of loads
    /// it belongs to. The file at `path` is kept first if it holds another
    /// save for `system`, unless one was kept for this run already.
    pub fn write_after_load(
        &self,
        cartridge: usize,
        generation: u64,
        path: PathBuf,
        bytes: Vec<u8>,
        run: u64,
        system: System,
    ) {
        self.thread.send(Job::Write {
            cartridge,
            generation,
            path,
            bytes,
            load: Some((run, system)),
        });
    }

    /// Waits until every write handed over so far is done.
    pub fn wait(&self) {
        let (done, finished) = mpsc::channel();
        self.thread.send(Job::Wait(done));
        // An error means the thread is gone, and its writes with it.
        finished.recv().ok();
    }

    /// The writes finished since last asked.
    pub fn finished(&self) -> Vec<Written> {
        self.written.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn writes_in_order_and_reports_each() {
        let dir = std::env::temp_dir().join(format!("emu-app-writer-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let save = dir.join("game.sav");
        let writer = SaveWriter::start().expect("the thread starts");
        writer.write(0, 1, save.clone(), b"first".to_vec());
        writer.write(0, 2, save.clone(), b"second".to_vec());
        writer.write(1, 7, dir.join("missing/other.sav"), b"lost".to_vec());
        writer.wait();
        assert_eq!(fs::read(&save).expect("written"), b"second");
        let written = writer.finished();
        let reports: Vec<(usize, u64, bool)> = written
            .iter()
            .map(|written| {
                (
                    written.cartridge,
                    written.generation,
                    written.result.is_ok(),
                )
            })
            .collect();
        assert_eq!(reports, [(0, 1, true), (0, 2, true), (1, 7, false)]);
        assert!(writer.finished().is_empty(), "each is reported once");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The files in `dir`, by name, with what they hold.
    fn files_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                let path = entry.expect("entry").path();
                (persist::file_name(&path), fs::read(&path).expect("read"))
            })
            .collect();
        files.sort();
        files
    }

    /// `files` as [`files_in`] gives them.
    fn held(files: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
        let files = files.iter();
        files
            .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
            .collect()
    }

    #[test]
    fn a_run_of_loads_keeps_the_save_on_disk_once() {
        let dir = std::env::temp_dir().join(format!("emu-app-writer-load-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        let save = dir.join("game.sav");
        let writer = SaveWriter::start().expect("the thread starts");
        let load = |generation: u64, bytes: &[u8], run: u64| {
            writer.write_after_load(
                0,
                generation,
                save.clone(),
                bytes.to_vec(),
                run,
                System::Gba,
            );
        };
        writer.write(0, 1, save.clone(), b"hour 20".to_vec());
        writer.wait();
        assert_eq!(files_in(&dir), held(&[("game.sav", b"hour 20")]));

        // A state from hour 2 is loaded: the save from hour 20 is kept.
        load(2, b"hour 2", 1);
        // The game saves on, and another state of the same run of loads
        // replaces that save unkept.
        writer.write(0, 3, save.clone(), b"hour 3".to_vec());
        load(4, b"hour 1", 1);
        writer.wait();
        let after_first = held(&[
            ("game.sav", b"hour 1"),
            ("game.sav.before-load", b"hour 20"),
        ]);
        assert_eq!(files_in(&dir), after_first);
        let kept: Vec<Kept> = writer
            .finished()
            .into_iter()
            .map(|written| written.kept)
            .collect();
        let newest = dir.join("game.sav.before-load");
        assert_eq!(
            kept,
            [
                Kept::Unneeded,
                Kept::New(newest.clone()),
                Kept::Unneeded,
                Kept::Unneeded
            ]
        );

        // A load of another run, of the save already on disk: nothing to
        // keep, and the run may still keep one.
        load(5, b"hour 1", 2);
        writer.wait();
        assert_eq!(files_in(&dir), after_first);
        writer.write(0, 6, save.clone(), b"hour 4".to_vec());
        load(7, b"hour 2", 2);
        writer.wait();
        assert_eq!(
            files_in(&dir),
            held(&[
                ("game.sav", b"hour 2"),
                ("game.sav.before-load", b"hour 4"),
                ("game.sav.before-load2", b"hour 20"),
            ])
        );
        // A save kept already is not kept again, and that settles its run.
        writer.write(0, 8, save.clone(), b"hour 20".to_vec());
        load(9, b"hour 2", 3);
        writer.write(0, 10, save.clone(), b"hour 5".to_vec());
        load(11, b"hour 2", 3);
        writer.wait();
        let kept: Vec<Kept> = writer
            .finished()
            .into_iter()
            .map(|written| written.kept)
            .collect();
        assert_eq!(
            kept,
            [
                // The second run: its first load, a save, the load over it.
                Kept::Unneeded,
                Kept::Unneeded,
                Kept::New(newest),
                // The third: a save, the load over it, a save, a load.
                Kept::Unneeded,
                Kept::Already,
                Kept::Unneeded,
                Kept::Unneeded,
            ]
        );
        // It is the newest of those kept again, as the one last replaced.
        assert_eq!(
            files_in(&dir),
            held(&[
                ("game.sav", b"hour 2"),
                ("game.sav.before-load", b"hour 20"),
                ("game.sav.before-load2", b"hour 4"),
            ])
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_save_kept_before_a_write_that_fails_is_still_where_it_was() {
        let dir = std::env::temp_dir().join(format!("emu-app-writer-full-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        let save = dir.join("game.sav");
        persist::write_atomic(&save, b"hour 20").expect("written");
        // The disk is full: keeping the save takes no room and works, and
        // the write of the state's save fails.
        let full = |_: &Path, _: &[u8]| Err(io::Error::from(io::ErrorKind::StorageFull));
        let mut guard = Guard::default();
        let (kept, result) = guard.write_with(&save, b"hour 2", Some((1, System::Gba)), full);
        assert_eq!(kept, Kept::New(dir.join("game.sav.before-load")));
        assert!(result.is_err());
        // The save is where it was, whole: a game started now loads it.
        let unwritten = held(&[
            ("game.sav", b"hour 20"),
            ("game.sav.before-load", b"hour 20"),
        ]);
        assert_eq!(files_in(&dir), unwritten);
        let (_, result) = guard.write_with(&save, b"hour 2", None, full);
        assert!(result.is_err());
        assert_eq!(files_in(&dir), unwritten, "nor does a failed retry move it");

        // With room again, the retry writes, and keeps nothing twice.
        let (kept, result) = guard.write(&save, b"hour 2", None);
        assert_eq!(kept, Kept::Unneeded);
        result.expect("written");
        let written = held(&[
            ("game.sav", b"hour 2"),
            ("game.sav.before-load", b"hour 20"),
        ]);
        assert_eq!(files_in(&dir), written);

        // Had the app been closed with the disk still full, the next
        // session's load finds the save kept already.
        persist::write_atomic(&save, b"hour 20").expect("as it was left");
        let (kept, result) = Guard::default().write(&save, b"hour 2", Some((1, System::Gba)));
        assert_eq!(kept, Kept::Already);
        result.expect("written");
        assert_eq!(files_in(&dir), written);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_failed_write_after_a_load_leaves_the_check_to_the_next() {
        let dir = std::env::temp_dir().join(format!("emu-app-writer-owed-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let save = dir.join("game.sav");
        let writer = SaveWriter::start().expect("the thread starts");
        // The folder is gone, as an unplugged drive's is.
        writer.write_after_load(0, 1, save.clone(), b"hour 2".to_vec(), 1, System::Gba);
        writer.wait();
        assert!(writer.finished()[0].result.is_err());
        // Back, with the save it held: the retry, an ordinary write, still
        // keeps it.
        fs::create_dir_all(&dir).expect("back");
        fs::write(&save, b"hour 20").expect("as it was");
        writer.write(0, 1, save.clone(), b"hour 2".to_vec());
        writer.wait();
        assert_eq!(
            files_in(&dir),
            held(&[
                ("game.sav", b"hour 2"),
                ("game.sav.before-load", b"hour 20"),
            ])
        );
        let written = writer.finished();
        assert_eq!(written[0].kept, Kept::New(dir.join("game.sav.before-load")));
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn dropping_waits_for_the_writes() {
        let dir = std::env::temp_dir().join(format!("emu-app-writer-drop-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let save = dir.join("game.sav");
        let writer = SaveWriter::start().expect("the thread starts");
        writer.write(0, 1, save.clone(), vec![0x55; 64 << 10]);
        drop(writer);
        assert_eq!(fs::read(&save).expect("written").len(), 64 << 10);
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
