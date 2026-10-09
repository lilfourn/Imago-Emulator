//! One CPU's four timers (GBATEK, "DS Timers", which work like the GBA's).
//!
//! Both CPUs' timers count the 33.51 MHz bus clock, whatever the CPU's own
//! speed.
//!
//! A running timer's counter is not stepped every cycle: it is computed from
//! the cycle it last started counting, and the bus schedules an event at
//! the exact cycle it will overflow. Count-up (cascade) timers advance only
//! when the previous timer overflows.
//!
//! Each timer's prescaler starts counting when the timer starts. A control
//! write that leaves it counting keeps its progress into the current tick,
//! as a fraction of the tick when the prescaler changes, as melonDS does.

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

    /// Cycles since the current tick of a running timer began.
    fn progress(&self, now: u64) -> u64 {
        now.saturating_sub(self.since) % self.prescaler()
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

    /// Writes TMxCNT_L, the reload value, of timer `index`.
    pub(crate) fn write_reload(&mut self, index: usize, value: u16) {
        self.timers[index].reload = value;
    }

    /// Writes TMxCNT_H of timer `index` at cycle `now`. An overflow due by
    /// then whose event has not run yet happens first, under the old
    /// settings. Returns the timers whose interrupts such overflows
    /// requested, as a bit set (bit `n` for timer `n`). The caller must
    /// reschedule timer `index`'s overflow event.
    pub(crate) fn write_control(&mut self, index: usize, value: u16, now: u64) -> u8 {
        let overflowed = self.overflow_until(index, now);
        let requested = self.requesting(overflowed);
        let timer = &mut self.timers[index];
        let was_enabled = timer.enabled();
        // Bring the counter up to date under the old settings first.
        let progress = timer
            .running(index)
            .then(|| (timer.progress(now), timer.prescaler()));
        timer.counter = timer.counter_at(index, now);
        timer.control = value & 0x00C7;
        timer.since = match progress {
            Some((cycles, period)) => now.saturating_sub(cycles * timer.prescaler() / period),
            None => now,
        };
        if !was_enabled && timer.enabled() {
            timer.counter = timer.reload;
        }
        requested
    }

    /// Runs the overflows of timer `index` due by cycle `now`, for a write
    /// that lands before their event: at most a few instructions late.
    /// Returns every timer that overflowed, as a bit set.
    fn overflow_until(&mut self, index: usize, now: u64) -> u8 {
        let mut overflowed = 0;
        while let Some(time) = self.overflow_time(index).filter(|&time| time <= now) {
            overflowed |= self.overflow(index, time);
        }
        overflowed
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

    /// Those of the `overflowed` timers (bit `n` for timer `n`) that
    /// request an interrupt when they overflow.
    pub(crate) fn requesting(&self, overflowed: u8) -> u8 {
        (0..4)
            .filter(|&index| self.timers[index].control & 0x40 != 0)
            .fold(0, |requested, index| requested | (overflowed & 1 << index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `value` to the register at `offset` from 0x04000100 at `now`.
    /// Returns the timers whose interrupts a control write requested.
    fn write(timers: &mut Timers, offset: u32, value: u16, now: u64) -> u8 {
        let index = (offset / 4) as usize;
        if offset.is_multiple_of(4) {
            timers.write_reload(index, value);
            0
        } else {
            timers.write_control(index, value, now)
        }
    }

    #[test]
    fn counts_at_the_prescaled_rate() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFF0, 0);
        write(&mut timers, 2, 0x81, 100); // enabled, divide by 64
        assert_eq!(timers.read(0, 100 + 64 * 5), 0xFFF5);
        assert_eq!(timers.overflow_time(0), Some(100 + 16 * 64));
    }

    #[test]
    fn overflow_reloads_and_cascades() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFFF, 0);
        write(&mut timers, 2, 0xC0, 0); // enabled, IRQ
        write(&mut timers, 4, 0xFFFF, 0);
        write(&mut timers, 6, 0x84, 0); // enabled, count-up
        assert_eq!(timers.overflow_time(1), None);
        let overflowed = timers.overflow(0, 1);
        assert_eq!(overflowed, 0b11);
        assert_eq!(timers.read(0, 1), 0xFFFF);
        assert_eq!(timers.requesting(overflowed), 0b01);
    }

    #[test]
    fn stopping_freezes_the_counter() {
        let mut timers = Timers::default();
        write(&mut timers, 2, 0x80, 0);
        write(&mut timers, 2, 0x00, 50);
        assert_eq!(timers.read(0, 1_000), 50);
    }

    #[test]
    fn control_writes_keep_the_progress_into_a_tick() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFF0, 0);
        write(&mut timers, 2, 0x81, 1_000); // enabled, divide by 64
        // 100 cycles in: one tick and 36 cycles into the next.
        write(&mut timers, 2, 0xC1, 1_100); // the same, with IRQ
        assert_eq!(timers.read(0, 1_127), 0xFFF1);
        assert_eq!(timers.read(0, 1_128), 0xFFF2);
        assert_eq!(timers.overflow_time(0), Some(1_000 + 16 * 64));
        // Half a tick in, a slower prescaler is half a slower tick in.
        write(&mut timers, 2, 0x83, 1_160); // divide by 1024
        assert_eq!(timers.read(0, 1_160 + 511), 0xFFF2);
        assert_eq!(timers.read(0, 1_160 + 512), 0xFFF3);
    }

    #[test]
    fn a_control_write_after_the_overflow_cycle_keeps_the_overflow() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFF0, 0);
        write(&mut timers, 2, 0xC0, 0); // enabled, IRQ, divide by 1
        write(&mut timers, 4, 0x0010, 0);
        write(&mut timers, 6, 0x84, 0); // enabled, count-up
        assert_eq!(timers.overflow_time(0), Some(16));
        // Stopping at cycle 20 lands before the overflow's event runs.
        assert_eq!(write(&mut timers, 2, 0x00, 20), 0b01, "IRQ as set before");
        assert_eq!(timers.read(0, 20), 0xFFF4, "reloaded at 16, then 4 ticks");
        assert_eq!(timers.read(4, 20), 0x0011, "the cascade counted it");
        assert_eq!(timers.overflow_time(0), None);
        // Writing in the overflow's own cycle overflows too.
        write(&mut timers, 0, 0xFFFF, 20);
        write(&mut timers, 2, 0x80, 20);
        assert_eq!(write(&mut timers, 2, 0x80, 21), 0, "no IRQ asked");
        assert_eq!(timers.read(0, 21), 0xFFFF);
        assert_eq!(timers.read(4, 21), 0x0012);
    }
}
