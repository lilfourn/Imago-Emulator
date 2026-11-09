//! Writes battery saves to disk on a thread of their own, so a slow disk
//! never holds up a frame: the emulation thread copies a save's bytes and
//! hands them over. Writes happen in the order handed over, and each one
//! reports back how it went.
//!
//! A save written after the game went back to an earlier moment (a state
//! loaded, or rewind) may be older than the file it replaces, which may be
//! the only copy of the player's latest save. So the first such write after
//! each jump back sets the file aside first, with [`persist::set_aside`], if
//! it holds anything else.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::persist;

/// A battery save written, or not.
#[derive(Debug)]
pub struct Written {
    /// The cartridge whose save it is.
    pub cartridge: usize,
    /// The cartridge's backup generation the bytes are from.
    pub generation: u64,
    /// Where it went.
    pub path: PathBuf,
    /// Where the file there before went, if it was set aside first: it
    /// held another save than the one the game went back to.
    pub kept: Option<PathBuf>,
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
        /// The jump back this is the first write after, if it is.
        jump: Option<u64>,
    },
    /// Say when every write before this one is done.
    Wait(Sender<()>),
}

/// What the writing thread remembers of one cartridge's save between
/// writes, to set the file aside once after each jump back.
#[derive(Debug, Default)]
struct Guard {
    /// The jump back whose write failed, so the next write is checked in
    /// its place.
    owed: Option<u64>,
    /// The jump back the file was last set aside for: once is enough, as
    /// the writes that follow replace only what this thread wrote.
    kept_for: Option<u64>,
}

impl Guard {
    /// Writes `bytes` to `path`. If this is the first write after the jump
    /// back `jump`, or stands in for one that failed, the file there is
    /// set aside first if it holds anything else, and nothing is written
    /// unless that worked. Returns where it went, and how the write went.
    fn write(
        &mut self,
        path: &Path,
        bytes: &[u8],
        jump: Option<u64>,
    ) -> (Option<PathBuf>, io::Result<()>) {
        let jump = jump.or(self.owed);
        let aside = match jump {
            Some(jump) if self.kept_for != Some(jump) => {
                persist::set_aside_unless_same(path, bytes)
            }
            _ => Ok(None),
        };
        let (kept, result) = match aside {
            Ok(kept) => {
                if kept.is_some() {
                    self.kept_for = jump;
                }
                (kept, persist::write_atomic(path, bytes))
            }
            Err(error) => (None, Err(error)),
        };
        self.owed = jump.filter(|_| result.is_err());
        (kept, result)
    }
}

/// The battery save writing thread. Dropping it waits for every write
/// handed over.
pub struct SaveWriter {
    jobs: Option<Sender<Job>>,
    written: Receiver<Written>,
    thread: Option<JoinHandle<()>>,
}

impl SaveWriter {
    /// Starts the thread.
    ///
    /// # Errors
    ///
    /// Returns the error if the system will not start a thread.
    pub fn start() -> io::Result<Self> {
        let (jobs, receiver) = mpsc::channel();
        let (report, written) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("battery saves".to_owned())
            .spawn(move || {
                let mut guards: HashMap<usize, Guard> = HashMap::new();
                for job in receiver {
                    match job {
                        Job::Write {
                            cartridge,
                            generation,
                            path,
                            bytes,
                            jump,
                        } => {
                            let guard = guards.entry(cartridge).or_default();
                            let (kept, result) = guard.write(&path, &bytes, jump);
                            report
                                .send(Written {
                                    cartridge,
                                    generation,
                                    path,
                                    kept,
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
        Ok(Self {
            jobs: Some(jobs),
            written,
            thread: Some(thread),
        })
    }

    /// Hands over cartridge `cartridge`'s battery save, `bytes` as of
    /// backup generation `generation`, to be written to `path`.
    pub fn write(&self, cartridge: usize, generation: u64, path: PathBuf, bytes: Vec<u8>) {
        self.send(Job::Write {
            cartridge,
            generation,
            path,
            bytes,
            jump: None,
        });
    }

    /// Hands over a battery save as [`SaveWriter::write`] does, as the
    /// first since the game went back to an earlier moment: `jump` numbers
    /// the jump back, one for a state loaded and one for a whole stretch of
    /// rewinding. The file at `path` is set aside first if it holds
    /// anything else, once per jump.
    pub fn write_after_jump(
        &self,
        cartridge: usize,
        generation: u64,
        path: PathBuf,
        bytes: Vec<u8>,
        jump: u64,
    ) {
        self.send(Job::Write {
            cartridge,
            generation,
            path,
            bytes,
            jump: Some(jump),
        });
    }

    /// Waits until every write handed over so far is done.
    pub fn wait(&self) {
        let (done, finished) = mpsc::channel();
        self.send(Job::Wait(done));
        // An error means the thread is gone, and its writes with it.
        finished.recv().ok();
    }

    /// The writes finished since last asked.
    pub fn finished(&self) -> Vec<Written> {
        self.written.try_iter().collect()
    }

    fn send(&self, job: Job) {
        if let Some(jobs) = &self.jobs {
            jobs.send(job).ok();
        }
    }
}

impl Drop for SaveWriter {
    fn drop(&mut self) {
        // Closing the channel lets the thread finish its writes and stop.
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
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

    #[test]
    fn the_first_write_after_a_jump_back_sets_a_different_save_aside_once() {
        let dir = std::env::temp_dir().join(format!("emu-app-writer-jump-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        let save = dir.join("game.sav");
        let held = |files: &[(&str, &[u8])]| -> Vec<(String, Vec<u8>)> {
            let files = files.iter();
            files
                .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
                .collect()
        };
        let writer = SaveWriter::start().expect("the thread starts");
        writer.write(0, 1, save.clone(), b"hour 20".to_vec());
        writer.wait();
        assert_eq!(files_in(&dir), held(&[("game.sav", b"hour 20")]));

        // A state from hour 2 is loaded: the save from hour 20 is kept.
        writer.write_after_jump(0, 2, save.clone(), b"hour 2".to_vec(), 1);
        // More writes after the same jump replace only what was written.
        writer.write_after_jump(0, 3, save.clone(), b"hour 1".to_vec(), 1);
        writer.wait();
        let after_first = held(&[("game.sav", b"hour 1"), ("game.sav.bak", b"hour 20")]);
        assert_eq!(files_in(&dir), after_first);
        let kept: Vec<Option<PathBuf>> = writer
            .finished()
            .into_iter()
            .map(|written| written.kept)
            .collect();
        assert_eq!(kept, [None, Some(dir.join("game.sav.bak")), None]);

        // Another jump to a moment with the save already on disk: nothing
        // to keep.
        writer.write_after_jump(0, 4, save.clone(), b"hour 1".to_vec(), 2);
        writer.wait();
        assert_eq!(files_in(&dir), after_first);
        // The game saving on from there is no jump back.
        writer.write(0, 5, save.clone(), b"hour 3".to_vec());
        writer.wait();
        // A third jump, over that save, keeps it beside the first.
        writer.write_after_jump(0, 6, save.clone(), b"hour 2".to_vec(), 3);
        writer.wait();
        assert_eq!(
            files_in(&dir),
            held(&[
                ("game.sav", b"hour 2"),
                ("game.sav.bak", b"hour 20"),
                ("game.sav.bak2", b"hour 3"),
            ])
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_failed_write_after_a_jump_back_leaves_the_check_to_the_next() {
        let dir = std::env::temp_dir().join(format!("emu-app-writer-owed-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let save = dir.join("game.sav");
        let writer = SaveWriter::start().expect("the thread starts");
        // The folder is gone, as an unplugged drive's is.
        writer.write_after_jump(0, 1, save.clone(), b"hour 2".to_vec(), 1);
        writer.wait();
        assert!(writer.finished()[0].result.is_err());
        // Back, with the save it held: the retry, an ordinary write, still
        // keeps it.
        fs::create_dir_all(&dir).expect("back");
        fs::write(&save, b"hour 20").expect("as it was");
        writer.write(0, 1, save.clone(), b"hour 2".to_vec());
        writer.wait();
        assert_eq!(fs::read(&save).expect("written"), b"hour 2");
        assert_eq!(
            fs::read(dir.join("game.sav.bak")).expect("kept"),
            b"hour 20"
        );
        let written = writer.finished();
        assert_eq!(written[0].kept, Some(dir.join("game.sav.bak")));
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
