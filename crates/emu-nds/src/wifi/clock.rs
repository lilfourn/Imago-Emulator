//! Wifi time: microseconds against the bus clock.
//!
//! The wifi board counts microseconds from its own 22 MHz crystal (GBATEK,
//! "DS Wifi Timers"); the machine counts bus cycles at 33.51 MHz. Both are
//! exact here: a microsecond is 33.513982 bus cycles, and conversions
//! round so that a count reaches `n` at the first cycle on or after `n`
//! microseconds. Counters are worked out when read rather than ticked, so
//! an idle radio costs nothing.

use serde::{Deserialize, Serialize};

/// Bus cycles per second.
const CLOCK_HZ: u128 = 33_513_982;

/// The bus cycles `us` microseconds take, rounded up.
pub(super) fn cycles(us: u64) -> u64 {
    (u128::from(us) * CLOCK_HZ).div_ceil(1_000_000) as u64
}

/// Whole microseconds in `cycles` bus cycles.
pub(super) fn micros(cycles: u64) -> u64 {
    (u128::from(cycles) * 1_000_000 / CLOCK_HZ) as u64
}

/// A count of microseconds that runs while switched on, like W_US_COUNT,
/// or counts down to zero, like W_CMD_COUNT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(super) struct Counter {
    /// The value at bus cycle `mark`.
    value: u64,
    mark: u64,
    running: bool,
    /// Counts down and stops at zero, rather than up.
    down: bool,
}

impl Counter {
    /// A stopped counter at zero that counts up.
    pub(super) const fn up() -> Self {
        Self {
            value: 0,
            mark: 0,
            running: false,
            down: false,
        }
    }

    /// A stopped counter at zero that counts down.
    pub(super) const fn down() -> Self {
        Self {
            down: true,
            ..Self::up()
        }
    }

    /// The value at bus cycle `now`.
    pub(super) fn value(&self, now: u64) -> u64 {
        if !self.running {
            return self.value;
        }
        let elapsed = micros(now.saturating_sub(self.mark));
        if self.down {
            self.value.saturating_sub(elapsed)
        } else {
            self.value.wrapping_add(elapsed)
        }
    }

    /// Sets the value at bus cycle `now`.
    pub(super) fn set(&mut self, value: u64, now: u64) {
        self.value = value;
        self.mark = now;
    }

    /// Starts or stops the count at bus cycle `now`.
    pub(super) fn run(&mut self, running: bool, now: u64) {
        if running != self.running {
            self.value = self.value(now);
            self.mark = now;
            self.running = running;
        }
    }

    pub(super) fn running(&self) -> bool {
        self.running
    }

    /// The bus cycle at which a counter counting up reaches `target`, if
    /// it is running and has not passed it.
    pub(super) fn time_of(&self, target: u64) -> Option<u64> {
        let to_go = target.checked_sub(self.value)?;
        (self.running && !self.down).then(|| self.mark + cycles(to_go))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_of_microseconds_is_a_second_of_cycles() {
        assert_eq!(cycles(1_000_000), 33_513_982);
        assert_eq!(micros(33_513_982), 1_000_000);
        assert_eq!(cycles(1), 34, "rounded up");
        assert_eq!(micros(33), 0);
        assert_eq!(micros(34), 1);
    }

    #[test]
    fn a_counter_reaches_a_value_exactly_when_it_says() {
        let mut counter = Counter::up();
        counter.set(1000, 50);
        counter.run(true, 50);
        let at = counter.time_of(2024).expect("running");
        assert_eq!(counter.value(at), 2024);
        assert_eq!(counter.value(at - 1), 2023);
        assert_eq!(counter.time_of(999), None, "already past");
    }

    #[test]
    fn stopped_counters_hold_and_countdowns_stop_at_zero() {
        let mut counter = Counter::up();
        counter.run(true, 0);
        counter.run(false, cycles(30));
        assert_eq!(counter.value(cycles(500)), 30);
        assert_eq!(counter.time_of(40), None, "stopped");

        let mut countdown = Counter::down();
        countdown.set(100, 0);
        countdown.run(true, 0);
        assert_eq!(countdown.value(cycles(40)), 60);
        assert_eq!(countdown.value(cycles(400)), 0);
    }
}
