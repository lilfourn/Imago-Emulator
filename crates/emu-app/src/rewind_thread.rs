//! Runs the rewind history on a thread of its own.
//!
//! Recording a frame for rewind takes two steps: serializing the core,
//! which only the emulation thread can do, then XORing the state with the
//! one before and compressing that, which for a DS takes as long again and
//! needs nothing from the core. [`RewindThread`] does the second step on a
//! worker thread. The emulation thread serializes into a buffer the worker
//! handed back earlier, so steady play allocates nothing, and if the worker
//! falls behind, frames are skipped rather than queued. Stepping back
//! waits for the worker to finish what it was sent, then for the state.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use crate::rewind::Rewind;
use crate::worker::Worker;

/// States sent but not yet recorded, beyond which frames are skipped.
const MAX_PENDING: usize = 2;

/// What the emulation thread asks of the worker.
enum Job {
    /// Record this state as the newest.
    Push(Vec<u8>),
    /// Step back: send the newest state and forget it.
    Pop,
    /// A state from [`Job::Pop`], finished with, to rebuild the next in.
    Reuse(Vec<u8>),
    /// Forget every state.
    Clear,
}

/// What the worker sends back.
enum Reply {
    /// A state was recorded; here is a buffer to serialize another into.
    Pushed(Option<Vec<u8>>),
    /// The newest state, if there was one.
    Popped(Option<Vec<u8>>),
}

/// The emulation thread's handle on the rewind history. Dropping it stops
/// the worker.
pub struct RewindThread {
    /// Dropped first, which stops the worker after the jobs it was sent,
    /// while `replies` still takes what it sends back.
    worker: Worker<Job>,
    replies: Receiver<Reply>,
    /// Buffers to serialize states into.
    spares: Vec<Vec<u8>>,
    /// States sent that the worker has not yet recorded.
    pending: usize,
}

impl RewindThread {
    /// Starts a worker keeping at most `budget_bytes` of history.
    ///
    /// # Errors
    ///
    /// Returns the error if the system will not start a thread.
    pub fn start(budget_bytes: usize) -> std::io::Result<Self> {
        let (reply_sender, replies) = mpsc::channel();
        let history = Rewind::new(budget_bytes);
        let worker = Worker::start("rewind", move |jobs| work(history, &jobs, &reply_sender))?;
        Ok(Self {
            worker,
            replies,
            spares: Vec::new(),
            pending: 0,
        })
    }

    /// Records the state `save` writes into the buffer it is given, unless
    /// the worker is still busy with earlier states: then `save` is not
    /// called and the frame is skipped. Returns whether it was recorded.
    pub fn push_with(&mut self, save: impl FnOnce(&mut Vec<u8>)) -> bool {
        while let Ok(reply) = self.replies.try_recv().map_err(|_: TryRecvError| ()) {
            self.take(reply);
        }
        if self.pending >= MAX_PENDING {
            return false;
        }
        let mut state = self.spares.pop().unwrap_or_default();
        state.clear();
        save(&mut state);
        self.pending += 1;
        self.send(Job::Push(state));
        true
    }

    /// Steps back one state: hands the newest to `load`, then forgets it.
    /// Returns false, without calling `load`, if there is none.
    pub fn pop_with(&mut self, load: impl FnOnce(&[u8])) -> bool {
        self.send(Job::Pop);
        // Replies come in order, so states sent earlier are recorded first.
        while let Ok(reply) = self.replies.recv() {
            match reply {
                Reply::Popped(Some(state)) => {
                    load(&state);
                    self.send(Job::Reuse(state));
                    return true;
                }
                Reply::Popped(None) => return false,
                pushed @ Reply::Pushed(_) => self.take(pushed),
            }
        }
        false
    }

    /// Forgets every state, e.g. after loading a save state.
    pub fn clear(&mut self) {
        self.send(Job::Clear);
    }

    fn send(&self, job: Job) {
        // False means the worker stopped; there is no history.
        self.worker.send(job);
    }

    /// Notes a reply to a push, keeping the buffer it brings.
    fn take(&mut self, reply: Reply) {
        if let Reply::Pushed(spare) = reply {
            self.pending = self.pending.saturating_sub(1);
            if self.spares.len() < MAX_PENDING {
                self.spares.extend(spare);
            }
        }
    }
}

/// The worker: does each job in turn until the channel closes.
fn work(mut history: Rewind, jobs: &Receiver<Job>, replies: &Sender<Reply>) {
    for job in jobs {
        let reply = match job {
            Job::Push(state) => Reply::Pushed(history.push(state)),
            Job::Pop => Reply::Popped(history.pop()),
            Job::Reuse(buffer) => {
                history.reuse(buffer);
                continue;
            }
            Job::Clear => {
                history.clear();
                continue;
            }
        };
        if replies.send(reply).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state of `len` bytes that differs from its neighbours in a few.
    fn state(frame: u32, len: usize) -> Vec<u8> {
        let mut state = vec![0x5A; len];
        state[..4].copy_from_slice(&frame.to_le_bytes());
        state[len / 2] = frame as u8;
        state
    }

    fn pop(rewind: &mut RewindThread) -> Option<Vec<u8>> {
        let mut popped = None;
        rewind.pop_with(|state| popped = Some(state.to_vec()));
        popped
    }

    #[test]
    fn steps_back_through_the_states_recorded() {
        let mut rewind = RewindThread::start(1 << 24).expect("the thread starts");
        let mut recorded = Vec::new();
        // Large states, sent as fast as possible, so the worker may fall
        // behind and some are skipped; what was recorded must come back.
        for frame in 0..40 {
            if rewind.push_with(|out| out.extend_from_slice(&state(frame, 1 << 20))) {
                recorded.push(frame);
            }
        }
        assert!(!recorded.is_empty());
        for frame in recorded.iter().rev() {
            assert_eq!(pop(&mut rewind), Some(state(*frame, 1 << 20)));
        }
        assert_eq!(pop(&mut rewind), None);
    }

    #[test]
    fn records_again_after_stepping_back() {
        let mut rewind = RewindThread::start(1 << 20).expect("the thread starts");
        for frame in 0..5 {
            assert!(rewind.push_with(|out| out.extend_from_slice(&state(frame, 64))));
            // Give the worker time, so none is skipped.
            while rewind.pending > 0 {
                let reply = rewind.replies.recv().expect("worker running");
                rewind.take(reply);
            }
        }
        assert_eq!(pop(&mut rewind), Some(state(4, 64)));
        assert_eq!(pop(&mut rewind), Some(state(3, 64)));
        assert!(rewind.push_with(|out| out.extend_from_slice(&state(9, 64))));
        assert_eq!(pop(&mut rewind), Some(state(9, 64)));
        assert_eq!(pop(&mut rewind), Some(state(2, 64)));
    }

    #[test]
    fn clearing_forgets_states_still_on_their_way() {
        let mut rewind = RewindThread::start(1 << 20).expect("the thread starts");
        rewind.push_with(|out| out.extend_from_slice(&state(1, 64)));
        rewind.clear();
        assert_eq!(pop(&mut rewind), None);
    }
}
