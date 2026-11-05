//! A cycle-timestamped event scheduler.
//!
//! Instead of ticking every component on every cycle, a core asks the
//! scheduler when the next event is due ("PPU enters HBlank", "timer 2
//! overflows", "DMA finishes"), runs the CPU until then, and handles the
//! events that came due. This is faster than lock-step ticking and exact
//! enough for GB M-cycle, GBA and DS timing.
//!
//! Time is a `u64` count of master-clock cycles; each core picks its unit
//! (for example, the DS core counts 33 MHz ARM7 cycles, as GBATEK does).
//! Events due at the same cycle come out in the order they were scheduled,
//! so runs are deterministic.
//!
//! ```
//! use emu_sched::Scheduler;
//!
//! #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//! enum Event { HBlank, TimerOverflow }
//!
//! let mut sched = Scheduler::new();
//! sched.schedule_in(960, Event::HBlank);
//! sched.schedule_in(1_000, Event::TimerOverflow);
//!
//! // Run the CPU up to the next event, then handle everything due.
//! let budget = sched.cycles_until_next().unwrap_or(0);
//! sched.advance(budget);
//! while let Some(due) = sched.pop_due() {
//!     assert_eq!(due.event, Event::HBlank);
//!     // Reschedule from the intended time, not `now`, so lateness never drifts.
//!     sched.schedule_at(due.time + 1_232, Event::HBlank);
//! }
//! assert_eq!(sched.next_time(), Some(1_000));
//! ```

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use serde::{Deserialize, Serialize};

/// A priority queue of events keyed by the cycle they are due.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "E: Serialize", deserialize = "E: Deserialize<'de>"))]
pub struct Scheduler<E> {
    now: u64,
    next_seq: u64,
    queue: BinaryHeap<Entry<E>>,
}

/// An event that has come due, and the cycle it was scheduled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Due<E> {
    /// The cycle the event was scheduled for. It can be earlier than
    /// [`Scheduler::now`] if the CPU overshot; handlers reschedule relative
    /// to this value to avoid drift.
    pub time: u64,
    /// The event.
    pub event: E,
}

impl<E> Scheduler<E> {
    /// An empty scheduler at cycle 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            now: 0,
            next_seq: 0,
            queue: BinaryHeap::new(),
        }
    }

    /// The current cycle.
    #[must_use]
    pub const fn now(&self) -> u64 {
        self.now
    }

    /// Moves time forward by `cycles`.
    pub const fn advance(&mut self, cycles: u64) {
        self.now += cycles;
    }

    /// Schedules `event` for cycle `time`. A time in the past comes due on
    /// the next [`Scheduler::pop_due`].
    pub fn schedule_at(&mut self, time: u64, event: E) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push(Entry { time, seq, event });
    }

    /// Schedules `event` for `delay` cycles from now.
    pub fn schedule_in(&mut self, delay: u64, event: E) {
        self.schedule_at(self.now + delay, event);
    }

    /// The cycle of the earliest pending event.
    #[must_use]
    pub fn next_time(&self) -> Option<u64> {
        self.queue.peek().map(|entry| entry.time)
    }

    /// Cycles from now until the earliest pending event, or zero if it is
    /// already due.
    #[must_use]
    pub fn cycles_until_next(&self) -> Option<u64> {
        self.next_time().map(|time| time.saturating_sub(self.now))
    }

    /// Whether an event is due at or before now: whether
    /// [`Scheduler::pop_due`] would return one.
    #[must_use]
    pub fn has_due(&self) -> bool {
        self.next_time().is_some_and(|time| time <= self.now)
    }

    /// Removes and returns the earliest event due at or before now.
    pub fn pop_due(&mut self) -> Option<Due<E>> {
        if self.next_time()? > self.now {
            return None;
        }
        self.queue.pop().map(|entry| Due {
            time: entry.time,
            event: entry.event,
        })
    }

    /// Removes every pending event for which `matches` returns true.
    pub fn cancel(&mut self, mut matches: impl FnMut(&E) -> bool) {
        self.queue.retain(|entry| !matches(&entry.event));
    }

    /// Pending events in the order they will come due, for save states.
    #[must_use]
    pub fn pending(&self) -> Vec<Due<E>>
    where
        E: Clone,
    {
        let mut entries: Vec<&Entry<E>> = self.queue.iter().collect();
        entries.sort_unstable_by(|a, b| b.cmp(a));
        entries
            .into_iter()
            .map(|entry| Due {
                time: entry.time,
                event: entry.event.clone(),
            })
            .collect()
    }

    /// Rebuilds a scheduler from [`Scheduler::now`] and
    /// [`Scheduler::pending`], preserving the order of same-cycle events.
    #[must_use]
    pub fn restore(now: u64, pending: impl IntoIterator<Item = Due<E>>) -> Self {
        let mut sched = Self::new();
        sched.now = now;
        for due in pending {
            sched.schedule_at(due.time, due.event);
        }
        sched
    }
}

impl<E> Default for Scheduler<E> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry<E> {
    time: u64,
    seq: u64,
    event: E,
}

// `BinaryHeap` is a max-heap, so the earliest (time, seq) compares greatest.
impl<E> Ord for Entry<E> {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.time, other.seq).cmp(&(self.time, self.seq))
    }
}

impl<E> PartialOrd for Entry<E> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<E> PartialEq for Entry<E> {
    fn eq(&self, other: &Self) -> bool {
        (self.time, self.seq) == (other.time, other.seq)
    }
}

impl<E> Eq for Entry<E> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(sched: &mut Scheduler<char>) -> String {
        std::iter::from_fn(|| sched.pop_due())
            .map(|due| due.event)
            .collect()
    }

    #[test]
    fn events_come_due_in_time_order() {
        let mut sched = Scheduler::new();
        sched.schedule_at(30, 'c');
        sched.schedule_at(10, 'a');
        sched.schedule_at(20, 'b');
        sched.advance(30);
        assert_eq!(drain(&mut sched), "abc");
    }

    #[test]
    fn same_cycle_events_keep_scheduling_order() {
        let mut sched = Scheduler::new();
        for event in ['x', 'y', 'z'] {
            sched.schedule_at(5, event);
        }
        sched.advance(5);
        assert_eq!(drain(&mut sched), "xyz");
    }

    #[test]
    fn future_events_are_not_due() {
        let mut sched = Scheduler::new();
        sched.schedule_in(100, 'a');
        sched.advance(99);
        assert!(!sched.has_due());
        assert_eq!(sched.pop_due(), None);
        assert_eq!(sched.cycles_until_next(), Some(1));
        sched.advance(1);
        assert!(sched.has_due());
        assert_eq!(
            sched.pop_due(),
            Some(Due {
                time: 100,
                event: 'a'
            })
        );
    }

    #[test]
    fn overshoot_reports_the_scheduled_time() {
        let mut sched = Scheduler::new();
        sched.schedule_in(10, 'a');
        sched.advance(14);
        assert_eq!(sched.pop_due().map(|due| due.time), Some(10));
    }

    #[test]
    fn cancel_removes_matching_events() {
        let mut sched = Scheduler::new();
        sched.schedule_at(1, 'a');
        sched.schedule_at(2, 'b');
        sched.cancel(|event| *event == 'a');
        sched.advance(2);
        assert_eq!(drain(&mut sched), "b");
    }

    #[test]
    fn restore_preserves_order_and_time() {
        let mut sched = Scheduler::new();
        sched.schedule_at(50, 'b');
        sched.schedule_at(50, 'c');
        sched.schedule_at(20, 'a');
        sched.advance(7);

        let mut restored = Scheduler::restore(sched.now(), sched.pending());
        assert_eq!(restored.now(), 7);
        restored.advance(50);
        assert_eq!(drain(&mut restored), "abc");
    }
}
