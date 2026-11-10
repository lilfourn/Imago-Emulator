//! A thread that does the jobs handed to it, in the order they were
//! handed over: what the battery-save writer, the rewind history and the
//! movie recorder each run on.
//!
//! A [`Worker`] owns the sending end of the thread's queue and the thread.
//! Stopping it, or dropping it, closes the queue and waits for the thread,
//! which first does every job handed over before: nothing handed over is
//! lost, and the thread never outlives its owner. The thread is started
//! in one place, so a system that will start no more threads is reported
//! the same way by all three.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};

/// The sending end of a worker's queue.
enum Queue<Job> {
    /// Any number of jobs may wait.
    Unbounded(Sender<Job>),
    /// So many jobs may wait; handing over one more waits for room.
    Bounded(SyncSender<Job>),
}

/// A thread doing jobs of type `Job` in turn, which returns a `Done` once
/// its queue has closed.
pub struct Worker<Job, Done = ()> {
    queue: Option<Queue<Job>>,
    thread: Option<JoinHandle<Done>>,
}

impl<Job: Send + 'static, Done: Send + 'static> Worker<Job, Done> {
    /// Starts a thread named `name` that runs `work` with the receiving
    /// end of the queue, which ends once the worker is stopped or dropped
    /// and every job before that has been taken. Any number of jobs may
    /// wait in the queue.
    ///
    /// # Errors
    ///
    /// Returns the error if the system will not start a thread.
    pub fn start(
        name: &str,
        work: impl FnOnce(Receiver<Job>) -> Done + Send + 'static,
    ) -> io::Result<Self> {
        let (jobs, receiver) = mpsc::channel();
        Self::spawn(name, Queue::Unbounded(jobs), receiver, work)
    }

    /// Starts a thread as [`Worker::start`] does, with a queue that holds
    /// at most `queue` jobs: [`Worker::send`] then waits for the thread to
    /// take one.
    ///
    /// # Errors
    ///
    /// Returns the error if the system will not start a thread.
    pub fn start_bounded(
        name: &str,
        queue: usize,
        work: impl FnOnce(Receiver<Job>) -> Done + Send + 'static,
    ) -> io::Result<Self> {
        let (jobs, receiver) = mpsc::sync_channel(queue);
        Self::spawn(name, Queue::Bounded(jobs), receiver, work)
    }

    fn spawn(
        name: &str,
        queue: Queue<Job>,
        receiver: Receiver<Job>,
        work: impl FnOnce(Receiver<Job>) -> Done + Send + 'static,
    ) -> io::Result<Self> {
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || work(receiver))?;
        Ok(Self {
            queue: Some(queue),
            thread: Some(thread),
        })
    }
}

impl<Job, Done> Worker<Job, Done> {
    /// Hands `job` over, waiting for room first if the queue is a bounded
    /// one and full. Returns false if the thread no longer takes jobs,
    /// having stopped of itself or been stopped: the job is dropped.
    pub fn send(&self, job: Job) -> bool {
        match &self.queue {
            Some(Queue::Unbounded(jobs)) => jobs.send(job).is_ok(),
            Some(Queue::Bounded(jobs)) => jobs.send(job).is_ok(),
            None => false,
        }
    }

    /// Closes the queue and waits for the thread, which does the jobs
    /// handed over before it ends. Returns what the thread returned, or
    /// `None` if it panicked or was stopped already.
    pub fn stop(&mut self) -> Option<Done> {
        self.queue = None;
        self.thread.take()?.join().ok()
    }
}

impl<Job, Done> Drop for Worker<Job, Done> {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;

    #[test]
    fn jobs_are_done_in_the_order_handed_over_and_dropping_waits_for_them_all() {
        let done = Arc::new(Mutex::new(Vec::new()));
        let worker = Worker::start("test worker", {
            let done = Arc::clone(&done);
            move |jobs: Receiver<u32>| {
                for job in jobs {
                    // Slower than the jobs are handed over, so they queue.
                    thread::sleep(Duration::from_millis(1));
                    done.lock().expect("unpoisoned").push(job);
                }
            }
        })
        .expect("the thread starts");
        for job in 0..20 {
            assert!(worker.send(job));
        }
        drop(worker);
        let done = done.lock().expect("unpoisoned");
        assert_eq!(*done, (0..20).collect::<Vec<u32>>());
    }

    #[test]
    fn stopping_returns_what_the_thread_returned_once() {
        let mut worker = Worker::start("test worker", |jobs: Receiver<u32>| -> u32 {
            jobs.into_iter().sum()
        })
        .expect("the thread starts");
        for job in 1..=4 {
            assert!(worker.send(job));
        }
        assert_eq!(worker.stop(), Some(10));
        assert_eq!(worker.stop(), None, "stopped already");
        assert!(!worker.send(5), "a stopped worker takes no job");
    }

    #[test]
    fn a_thread_that_ends_of_itself_takes_no_more_jobs_and_one_that_panics_returns_nothing() {
        let mut ended = Worker::start("test worker", |jobs: Receiver<u32>| {
            // Takes one job, then gives up, as a writer does after an
            // error.
            jobs.recv().ok()
        })
        .expect("the thread starts");
        assert!(ended.send(7));
        // The receiving end goes with the thread; sends fail from then on.
        while ended.send(0) {
            thread::yield_now();
        }
        assert_eq!(ended.stop(), Some(Some(7)));

        let mut panicked = Worker::start("test worker", |jobs: Receiver<u32>| {
            let _ = jobs.recv();
            panic!("as a job might");
        })
        .expect("the thread starts");
        panicked.send(1);
        assert_eq!(panicked.stop(), None);
    }

    #[test]
    fn a_bounded_queue_makes_the_sender_wait_and_loses_nothing() {
        let taken = Arc::new(AtomicUsize::new(0));
        let mut worker = Worker::start_bounded("test worker", 2, {
            let taken = Arc::clone(&taken);
            move |jobs: Receiver<usize>| {
                let mut seen = Vec::new();
                for job in jobs {
                    thread::sleep(Duration::from_millis(1));
                    taken.fetch_add(1, Ordering::SeqCst);
                    seen.push(job);
                }
                seen
            }
        })
        .expect("the thread starts");
        for job in 0..10 {
            assert!(worker.send(job));
            // At most the queue's two and the one being done are ahead
            // of the thread.
            assert!(taken.load(Ordering::SeqCst) + 3 >= job);
        }
        assert_eq!(worker.stop(), Some((0..10).collect()));
    }
}
