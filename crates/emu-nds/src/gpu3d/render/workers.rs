//! Threads kept for drawing the bands of each frame, and of the larger
//! screen around it, so that a frame starts none: starting a thread costs
//! about as much as drawing a small band. They start when first needed and
//! stop when the renderer that owns them is dropped.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// Work for one thread.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

/// Jobs waiting for a thread.
#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// The threads should stop.
    closed: bool,
}

/// What the threads share with their owner.
#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    /// Signalled when jobs arrive or the queue closes.
    ready: Condvar,
}

impl Shared {
    /// The queue. A job never runs while holding it, so it cannot be left
    /// half-changed by a panic.
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The next job, waiting for one; `None` once the queue closes.
    fn next(&self) -> Option<Job> {
        let mut queue = self.lock();
        loop {
            if queue.closed {
                return None;
            }
            if let Some(job) = queue.jobs.pop_front() {
                return Some(job);
            }
            queue = self
                .ready
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A few threads that run jobs in the order they arrive.
pub(crate) struct Workers {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    /// The most threads to start: [`most_threads`].
    most: usize,
}

impl Default for Workers {
    fn default() -> Self {
        Self {
            shared: Arc::default(),
            threads: Vec::new(),
            most: most_threads(std::thread::available_parallelism().map_or(1, NonZeroUsize::get)),
        }
    }
}

/// How many threads to draw with on a machine of `processors`: half of
/// them, rounded up, so a frame never wakes every processor at once and
/// the apps beside a game, playing video or drawing windows, find some
/// free. More gains little: Pokémon Platinum's overworld with 3D at 4×,
/// headless on an M5 Max, ran 126 frames a second on 18 threads and about
/// 120 on 9, twice what the game needs, on 4–10% less CPU time and half
/// the time in the kernel.
fn most_threads(processors: usize) -> usize {
    processors.div_ceil(2).max(1)
}

impl std::fmt::Debug for Workers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workers")
            .field("threads", &self.threads.len())
            .finish_non_exhaustive()
    }
}

impl Workers {
    /// Queues `jobs`, first to last, starting threads until there is one
    /// for each job or [`most_threads`], whichever is fewer. If no
    /// thread will start, runs them here instead. A job that panics takes
    /// its thread down with it, so jobs catch their own panics.
    ///
    /// Threads take the scheduling class of the thread that starts them,
    /// the one running the core, which waits for their bands each frame.
    pub(crate) fn run(&mut self, jobs: Vec<Job>) {
        while self.threads.len() < jobs.len().min(self.most) {
            let shared = Arc::clone(&self.shared);
            let class = emu_os::ThreadClass::current();
            let started = std::thread::Builder::new()
                .name("3D renderer".into())
                .spawn(move || {
                    class.apply();
                    while let Some(job) = shared.next() {
                        job();
                    }
                });
            match started {
                Ok(thread) => self.threads.push(thread),
                // Carry on with the threads there are.
                Err(_) => break,
            }
        }
        if self.threads.is_empty() {
            jobs.into_iter().for_each(|job| job());
            return;
        }
        self.shared.lock().jobs.extend(jobs);
        self.shared.ready.notify_all();
    }

    /// How many threads have started.
    #[cfg(test)]
    pub(crate) fn threads(&self) -> usize {
        self.threads.len()
    }
}

impl Drop for Workers {
    /// Drops the jobs still waiting and stops the threads, waiting for
    /// each to finish the job it is on.
    fn drop(&mut self) {
        let waiting = {
            let mut queue = self.shared.lock();
            queue.closed = true;
            std::mem::take(&mut queue.jobs)
        };
        drop(waiting);
        self.shared.ready.notify_all();
        for thread in self.threads.drain(..) {
            // Jobs catch their own panics; there is nothing to report.
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn jobs_run_and_threads_stop_when_dropped() {
        let mut workers = Workers::default();
        let (done, results) = mpsc::channel();
        let jobs = (0..6)
            .map(|n| {
                let done = done.clone();
                Box::new(move || done.send(n * n).expect("the test listens")) as Job
            })
            .collect();
        workers.run(jobs);
        let mut squares: Vec<i32> = results.iter().take(6).collect();
        squares.sort_unstable();
        assert_eq!(squares, [0, 1, 4, 9, 16, 25]);
        assert!((1..=6).contains(&workers.threads()));
        // Every thread holds the shared queue until it stops.
        let shared = Arc::downgrade(&workers.shared);
        drop(workers);
        assert!(shared.upgrade().is_none(), "every thread has stopped");
    }

    #[test]
    fn half_the_processors_draw() {
        assert_eq!(most_threads(18), 9);
        assert_eq!(most_threads(8), 4);
        assert_eq!(most_threads(3), 2);
        assert_eq!(most_threads(1), 1);
        assert_eq!(most_threads(0), 1);
    }

    #[test]
    fn threads_start_only_as_needed() {
        let mut workers = Workers::default();
        workers.run(vec![Box::new(|| {})]);
        assert_eq!(workers.threads(), 1);
        workers.run(Vec::new());
        assert_eq!(workers.threads(), 1);
    }
}
