//! The four timers (GBATEK, "GBA Timers").
//!
//! A running timer's counter is not stepped every cycle: it is computed from
//! the cycle it started counting, and the bus schedules an event at the
//! exact cycle it will overflow. Count-up (cascade) timers advance only
//! when the previous timer overflows.
//!
//! Writes take a cycle to land: the bus stages them here and applies them
//! one cycle later. A timer then needs one more cycle to load its reload
//! value before it counts. The prescalers divide one clock shared by all
//! timers, so a timer set to divide by 64 first counts at the next multiple
//! of 64 cycles, not 64 cycles after it starts. These timings are
//! NanoBoyAdvance's, which the mGBA suite's timer tests confirm.

use serde::{Deserialize, Serialize};

/// log2 of each prescaler: divide by 1, 64, 256 or 1024.
const PRESCALER_SHIFTS: [u32; 4] = [0, 6, 8, 10];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Timer {
    reload: u16,
    /// TMxCNT_H as written (bits 0-2, 6 and 7).
    control: u16,
    /// The counter's value at cycle `started` while the timer counts with
    /// time; its value otherwise.
    counter: u16,
    started: u64,
    /// Whether the counter advances with time: enabled and not count-up.
    running: bool,
    /// Written values that land a cycle after the write.
    staged_reload: u16,
    staged_control: u16,
}

impl Timer {
    fn enabled(&self) -> bool {
        self.control & 0x80 != 0
    }

    /// Whether this timer counts the previous one's overflows. Timer 0 has
    /// no previous timer and ignores the bit.
    fn counts_up(&self, index: usize) -> bool {
        index > 0 && self.control & 4 != 0
    }

    fn shift(&self) -> u32 {
        PRESCALER_SHIFTS[usize::from(self.control & 3)]
    }

    /// The counter at cycle `now`, which may run past 0xFFFF in the cycle
    /// it overflows.
    fn count_at(&self, now: u64) -> u32 {
        if !self.running {
            return u32::from(self.counter);
        }
        let ticks = now.saturating_sub(self.started) >> self.shift();
        u32::from(self.counter) + ticks as u32
    }

    /// Starts counting from `counter` at cycle `now`, `offset` cycles into
    /// the current prescaler period (−1: one cycle before the next one
    /// starts).
    fn start(&mut self, now: u64, offset: i64) {
        self.running = true;
        self.started = now.saturating_add_signed(-offset);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Timers {
    timers: [Timer; 4],
}

/// Which register a write staged, so the bus can apply it a cycle later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Staged {
    Reload(usize),
    Control(usize),
}

impl Timers {
    /// Reads a register at `offset` from 0x04000100 at cycle `now`.
    pub(crate) fn read(&self, offset: u32, now: u64) -> u16 {
        let index = (offset / 4) as usize;
        let timer = &self.timers[index];
        if offset.is_multiple_of(4) {
            timer.count_at(now) as u16
        } else {
            timer.control
        }
    }

    /// Stages a write to the register at `offset` from 0x04000100; the bus
    /// applies it a cycle later with [`Timers::apply_reload`] or
    /// [`Timers::apply_control`].
    pub(crate) fn stage(&mut self, offset: u32, value: u16) -> Staged {
        let index = (offset / 4) as usize;
        let timer = &mut self.timers[index];
        if offset.is_multiple_of(4) {
            timer.staged_reload = value;
            Staged::Reload(index)
        } else {
            timer.staged_control = value & 0x00C7;
            Staged::Control(index)
        }
    }

    /// Lands the reload value written to timer `index`.
    pub(crate) fn apply_reload(&mut self, index: usize) {
        let timer = &mut self.timers[index];
        timer.reload = timer.staged_reload;
    }

    /// Lands the control value written to timer `index`, at cycle `now`.
    /// Returns every timer that overflowed in the process, as a bit set:
    /// stopping a timer in the cycle it overflows still overflows it.
    pub(crate) fn apply_control(&mut self, index: usize, now: u64) -> u8 {
        let mut overflowed = 0;
        let timer = &mut self.timers[index];
        let was_enabled = timer.enabled();
        if timer.running {
            let count = timer.count_at(now);
            timer.running = false;
            if count >= 0x1_0000 {
                overflowed = self.reload_and_cascade(index);
            } else {
                self.timers[index].counter = count as u16;
            }
        }
        let timer = &mut self.timers[index];
        timer.control = timer.staged_control;
        if !timer.enabled() {
            return overflowed;
        }
        // How far `now` is into the shared prescaler's current period.
        let offset = (now & ((1 << timer.shift()) - 1)) as i64;
        if was_enabled {
            if !timer.counts_up(index) {
                timer.start(now, offset);
            }
        } else if timer.counts_up(index) {
            timer.counter = timer.reload;
        } else if timer.counter == 0xFFFF && offset == 0 {
            // Loading the reload value takes a cycle, and a counter left
            // at 0xFFFF can tick and overflow in it.
            timer.start(now, 0);
        } else {
            timer.counter = timer.reload;
            timer.start(now, offset - 1);
        }
        overflowed
    }

    /// The cycle timer `index` next overflows, if it counts with time.
    pub(crate) fn overflow_time(&self, index: usize) -> Option<u64> {
        let timer = &self.timers[index];
        if !timer.running {
            return None;
        }
        let remaining = 0x1_0000 - u64::from(timer.counter);
        Some(timer.started + (remaining << timer.shift()))
    }

    /// Timer `index` overflowed at cycle `now`. Reloads it, advances any
    /// count-up timers chained after it, and returns every timer that
    /// overflowed as a bit set (bit `n` for timer `n`).
    pub(crate) fn overflow(&mut self, index: usize, now: u64) -> u8 {
        let overflowed = self.reload_and_cascade(index);
        self.timers[index].start(now, 0);
        overflowed
    }

    /// Reloads timer `index` and counts one overflow in the count-up
    /// timers after it, reloading each that overflows in turn.
    fn reload_and_cascade(&mut self, index: usize) -> u8 {
        let mut overflowed = 1 << index;
        let timer = &mut self.timers[index];
        timer.counter = timer.reload;
        for next in index + 1..4 {
            let timer = &mut self.timers[next];
            if !(timer.enabled() && timer.counts_up(next)) {
                break;
            }
            let (counter, carry) = timer.counter.overflowing_add(1);
            timer.counter = if carry { timer.reload } else { counter };
            if !carry {
                break;
            }
            overflowed |= 1 << next;
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

    /// Writes `value` to the register at `offset` and lands it at `now`.
    fn write(timers: &mut Timers, offset: u32, value: u16, now: u64) -> u8 {
        match timers.stage(offset, value) {
            Staged::Reload(index) => {
                timers.apply_reload(index);
                0
            }
            Staged::Control(index) => timers.apply_control(index, now),
        }
    }

    #[test]
    fn counts_from_the_cycle_after_it_lands() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFF0, 0);
        write(&mut timers, 2, 0x80, 100); // enabled, divide by 1
        assert_eq!(timers.read(0, 101), 0xFFF0);
        assert_eq!(timers.read(0, 102), 0xFFF1);
        assert_eq!(timers.overflow_time(0), Some(101 + 16));
    }

    #[test]
    fn prescalers_tick_on_a_shared_clock() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFF0, 0);
        write(&mut timers, 2, 0x81, 100); // enabled, divide by 64
        // The shared clock ticks at 128, 192, ...; the first tick only
        // loads the counter, a cycle late.
        assert_eq!(timers.read(0, 127), 0xFFF0);
        assert_eq!(timers.read(0, 129), 0xFFF1);
        assert_eq!(timers.read(0, 129 + 64 * 4), 0xFFF5);
        assert_eq!(timers.overflow_time(0), Some(65 + 16 * 64));
    }

    #[test]
    fn overflow_reloads_and_cascades() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFFF, 0);
        write(&mut timers, 2, 0xC0, 0); // enabled, IRQ
        write(&mut timers, 4, 0xFFFF, 0);
        write(&mut timers, 6, 0x84, 0); // enabled, count-up
        assert_eq!(timers.overflow_time(1), None);
        let overflowed = timers.overflow(0, 2);
        assert_eq!(overflowed, 0b11);
        assert_eq!(timers.read(0, 2), 0xFFFF);
        assert_eq!(timers.read(4, 2), 0xFFFF);
        assert!(timers.interrupt_enabled(0));
    }

    #[test]
    fn stopping_freezes_the_counter() {
        let mut timers = Timers::default();
        write(&mut timers, 2, 0x80, 0);
        write(&mut timers, 2, 0x00, 51);
        assert_eq!(timers.read(0, 1_000), 50);
    }

    #[test]
    fn stopping_in_the_overflow_cycle_still_overflows() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFFE, 0);
        write(&mut timers, 2, 0x80, 0); // overflows at cycle 3
        assert_eq!(timers.overflow_time(0), Some(3));
        assert_eq!(write(&mut timers, 2, 0x00, 3), 0b1);
        assert_eq!(timers.read(0, 10), 0xFFFE);
    }

    #[test]
    fn a_counter_left_at_ffff_can_overflow_while_it_loads() {
        let mut timers = Timers::default();
        write(&mut timers, 0, 0xFFFF, 0);
        write(&mut timers, 2, 0x80, 0);
        write(&mut timers, 2, 0x00, 1); // stopped at 0xFFFF
        write(&mut timers, 0, 0x1234, 1);
        write(&mut timers, 2, 0x80, 64);
        assert_eq!(timers.overflow_time(0), Some(65));
    }
}
