//! One CPU's four timers (GBATEK, "DS Timers", which work like the GBA's).
//!
//! Both CPUs' timers count the 33.51 MHz bus clock, whatever the CPU's own
//! speed.
//!
//! A running timer's counter is not stepped every cycle: it is computed from
//! the cycle it last started counting, and the bus schedules an event at
//! the exact cycle it will overflow. Count-up (cascade) timers advance only
//! when the previous timer overflows.

use serde::{Deserialize, Serialize};

const PRESCALERS: [u64; 4] = [1, 64, 256, 1024];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Timer {
    reload: u16,
    control: u16,
    /// The counter's value at cycle `since`.
    counter: u16,
    since: u64,
}

impl Timer {
    fn enabled(&self) -> bool {
        self.control & 0x80 != 0
    }

    fn prescaler(&self) -> u64 {
        PRESCALERS[usize::from(self.control & 3)]
    }

    /// Whether the counter advances with time (enabled and not count-up).
    fn running(&self, index: usize) -> bool {
        self.enabled() && !(index > 0 && self.control & 4 != 0)
    }

    fn counter_at(&self, index: usize, now: u64) -> u16 {
        if !self.running(index) {
            return self.counter;
        }
        let ticks = now.saturating_sub(self.since) / self.prescaler();
        (u64::from(self.counter) + ticks) as u16
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Timers {
    timers: [Timer; 4],
}

impl Timers {
    /// Reads a register at `offset` from 0x04000100.
    pub(crate) fn read(&self, offset: u32, now: u64) -> u16 {
        let index = (offset / 4) as usize;
        let timer = &self.timers[index];
        if offset.is_multiple_of(4) {
            timer.counter_at(index, now)
        } else {
            timer.control
        }
    }

    /// Writes a register at `offset` from 0x04000100. Returns the timer
    /// whose overflow event must be rescheduled, if any.
    pub(crate) fn write(&mut self, offset: u32, value: u16, now: u64) -> Option<usize> {
        let index = (offset / 4) as usize;
        let timer = &mut self.timers[index];
        if offset.is_multiple_of(4) {
            timer.reload = value;
            return None;
        }
        let was_enabled = timer.enabled();
        // Bring the counter up to date under the old settings first.
        timer.counter = timer.counter_at(index, now);
        timer.since = now;
        timer.control = value & 0x00C7;
        if !was_enabled && timer.enabled() {
            timer.counter = timer.reload;
        }
        Some(index)
    }

    /// The cycle timer `index` next overflows, if it counts with time.
    pub(crate) fn overflow_time(&self, index: usize) -> Option<u64> {
        let timer = &self.timers[index];
        if !timer.running(index) {
            return None;
        }
        let remaining = 0x1_0000 - u64::from(timer.counter);
        Some(timer.since + remaining * timer.prescaler())
    }

    /// Timer `index` overflowed at cycle `now`. Reloads it, advances any
    /// count-up timers chained after it, and returns every timer that
    /// overflowed as a bit set (bit `n` for timer `n`).
    pub(crate) fn overflow(&mut self, index: usize, now: u64) -> u8 {
        let mut overflowed = 1 << index;
        let timer = &mut self.timers[index];
        timer.counter = timer.reload;
        timer.since = now;
        // Cascade into the next count-up timer.
        let mut next = index + 1;
        while next < 4 {
            let timer = &mut self.timers[next];
            if !(timer.enabled() && timer.control & 4 != 0) {
                break;
            }
            let (counter, carry) = timer.counter.overflowing_add(1);
            timer.counter = if carry { timer.reload } else { counter };
            if !carry {
                break;
            }
            overflowed |= 1 << next;
            next += 1;
        }
        overflowed
    }

    /// Whether timer `index` requests an interrupt when it overflows.
    pub(crate) fn interrupt_enabled(&self, index: usize) -> bool {
        self.timers[index].control & 0x40 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_at_the_prescaled_rate() {
        let mut timers = Timers::default();
        timers.write(0, 0xFFF0, 0);
        timers.write(2, 0x81, 100); // enabled, divide by 64
        assert_eq!(timers.read(0, 100 + 64 * 5), 0xFFF5);
        assert_eq!(timers.overflow_time(0), Some(100 + 16 * 64));
    }

    #[test]
    fn overflow_reloads_and_cascades() {
        let mut timers = Timers::default();
        timers.write(0, 0xFFFF, 0);
        timers.write(2, 0xC0, 0); // enabled, IRQ
        timers.write(4, 0xFFFF, 0);
        timers.write(6, 0x84, 0); // enabled, count-up
        assert_eq!(timers.overflow_time(1), None);
        let overflowed = timers.overflow(0, 1);
        assert_eq!(overflowed, 0b11);
        assert_eq!(timers.read(0, 1), 0xFFFF);
        assert!(timers.interrupt_enabled(0));
    }

    #[test]
    fn stopping_freezes_the_counter() {
        let mut timers = Timers::default();
        timers.write(2, 0x80, 0);
        timers.write(2, 0x00, 50);
        assert_eq!(timers.read(0, 1_000), 50);
    }
}
