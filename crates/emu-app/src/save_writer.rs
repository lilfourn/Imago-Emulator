//! Writes battery saves to disk on a thread of their own, so a slow disk
//! never holds up a frame: the emulation thread copies a save's bytes and
//! hands them over. Writes happen in the order handed over, and each one
//! reports back how it went.

use std::io;
use std::path::PathBuf;
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
    },
    /// Say when every write before this one is done.
    Wait(Sender<()>),
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
    pub fn start() -> Self {
        let (jobs, receiver) = mpsc::channel();
        let (report, written) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("battery saves".to_owned())
            .spawn(move || {
                for job in receiver {
                    match job {
                        Job::Write {
                            cartridge,
                            generation,
                            path,
                            bytes,
                        } => {
                            let result = persist::write_atomic(&path, &bytes);
                            report
                                .send(Written {
                                    cartridge,
                                    generation,
                                    path,
                                    result,
                                })
                                .ok();
                        }
                        Job::Wait(done) => {
                            done.send(()).ok();
                        }
                    }
                }
            })
            .expect("spawning the battery save thread");
        Self {
            jobs: Some(jobs),
            written,
            thread: Some(thread),
        }
    }

    /// Hands over cartridge `cartridge`'s battery save, `bytes` as of
    /// backup generation `generation`, to be written to `path`.
    pub fn write(&self, cartridge: usize, generation: u64, path: PathBuf, bytes: Vec<u8>) {
        self.send(Job::Write {
            cartridge,
            generation,
            path,
            bytes,
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
        let writer = SaveWriter::start();
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

    #[test]
    fn dropping_waits_for_the_writes() {
        let dir = std::env::temp_dir().join(format!("emu-app-writer-drop-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let save = dir.join("game.sav");
        let writer = SaveWriter::start();
        writer.write(0, 1, save.clone(), vec![0x55; 64 << 10]);
        drop(writer);
        assert_eq!(fs::read(&save).expect("written").len(), 64 << 10);
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
