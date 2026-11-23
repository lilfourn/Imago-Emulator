//! Emulation speed: which speed the player asked for, how fast that is,
//! how far apart it puts frame deadlines, and sleeping to them.
//!
//! The emulation thread sleeps to each frame's deadline (see
//! `docs/decisions/0003-frame-pacing.md`). A speed divides the gap between
//! deadlines; an unlimited speed drops them and runs flat out.

use std::thread;
use std::time::{Duration, Instant};

/// The slowest rate allowed. One frame then waits at most ten frames'
/// time, so the emulation thread stays quick to pause or quit.
pub const MIN_RATE: f64 = 0.1;

/// Which speed the player asked for, from held inputs and toggles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pace {
    /// The system's own frame rate.
    #[default]
    Normal,
    /// The fast-forward speed.
    FastForward,
    /// The slow-motion speed.
    SlowMotion,
}

/// A multiple of normal speed: finite and at least [`MIN_RATE`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate(f64);

// Rates are finite, so every rate equals itself.
impl Eq for Rate {}

impl Rate {
    /// Normal speed.
    pub const NORMAL: Self = Self(1.0);

    /// `times` normal speed, if it is finite and at least [`MIN_RATE`].
    pub fn new(times: f64) -> Option<Self> {
        (times.is_finite() && times >= MIN_RATE).then_some(Self(times))
    }

    /// The multiple of normal speed.
    pub fn get(self) -> f64 {
        self.0
    }
}

/// How fast emulation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    /// As fast as the host can go.
    Unlimited,
    /// A multiple of the system's frame rate.
    Times(Rate),
}

impl Speed {
    /// The system's own frame rate.
    pub const NORMAL: Self = Self::Times(Rate::NORMAL);

    /// The speed as menus and notices show it: "2×", "0.5×" or
    /// "Unlimited".
    pub fn label(self) -> String {
        match self {
            Self::Unlimited => "Unlimited".to_owned(),
            Self::Times(rate) => format!("{}×", rate.get()),
        }
    }

    /// How far apart frame deadlines are at this speed, for frames
    /// `frame` long at normal speed; `None` if unlimited.
    pub fn frame_step(self, frame: Duration) -> Option<Duration> {
        match self {
            Self::Unlimited => None,
            Self::Times(rate) => Some(frame.div_f64(rate.get())),
        }
    }
}

/// The fast-forward speeds the Emulation menu offers, in its order.
pub const FAST_FORWARD_CHOICES: [Speed; 4] = [
    Speed::Times(Rate(2.0)),
    Speed::Times(Rate(3.0)),
    Speed::Times(Rate(4.0)),
    Speed::Unlimited,
];

/// The speeds fast-forward and slow motion run at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speeds {
    /// Twice normal speed unless set.
    pub fast_forward: Speed,
    /// Half speed unless set.
    pub slow_motion: Rate,
}

impl Default for Speeds {
    fn default() -> Self {
        Self {
            fast_forward: Speed::Times(Rate(2.0)),
            slow_motion: Rate(0.5),
        }
    }
}

impl Speeds {
    /// The speed `pace` runs at.
    pub fn speed(self, pace: Pace) -> Speed {
        match pace {
            Pace::Normal => Speed::NORMAL,
            Pace::FastForward => self.fast_forward,
            Pace::SlowMotion => Speed::Times(self.slow_motion),
        }
    }
}

/// How near a deadline [`sleep_until`] stops sleeping.
const CLOSE_ENOUGH: Duration = Duration::from_micros(100);

/// Sleeps until `deadline`, waking within about a tenth of a millisecond
/// of it.
///
/// macOS wakes a sleeping thread late by up to a quarter of the time it
/// asked for, so a single sleep to a deadline 10 ms away ends 2 to 2.5 ms
/// late: every frame, and the input it reads, would start that much after
/// its time. Sleeping three quarters of what is left, again and again,
/// lands on the deadline in two or three wake-ups instead.
///
/// Linux wakes a thread within its timer slack, 50 µs, so the same naps
/// land a few microseconds late. On Windows, Rust sleeps on a
/// high-resolution waitable timer (from Windows 10's 1803 update on),
/// which wakes within about half a millisecond, whatever the system timer
/// ticks at; [`crate::qos::Timers`] makes that tick every millisecond as
/// well, for older systems and for waits other than sleeps.
pub fn sleep_until(deadline: Instant) {
    while let Some(nap) = nap(deadline.saturating_duration_since(Instant::now())) {
        thread::sleep(nap);
    }
}

/// How long to sleep with `left` to go: three quarters of it, or not at
/// all once that is under [`CLOSE_ENOUGH`].
fn nap(left: Duration) -> Option<Duration> {
    (left >= CLOSE_ENOUGH).then(|| left * 3 / 4)
}

/// When the next frame is due, given that the last was due at `deadline`
/// and finished at `now`: `step` later, or now if unlimited (`None`) or
/// more than four steps behind, too far to catch up smoothly.
pub fn next_deadline(deadline: Instant, now: Instant, step: Option<Duration>) -> Instant {
    let Some(step) = step else {
        return now;
    };
    let next = deadline + step;
    if now.saturating_duration_since(next) > step * 4 {
        now
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(16);

    fn times(rate: f64) -> Speed {
        Speed::Times(Rate::new(rate).expect("valid rate"))
    }

    #[test]
    fn a_speed_divides_the_frame() {
        assert_eq!(Speed::NORMAL.frame_step(FRAME), Some(FRAME));
        assert_eq!(times(4.0).frame_step(FRAME), Some(FRAME / 4));
        assert_eq!(times(0.5).frame_step(FRAME), Some(FRAME * 2));
        assert_eq!(Speed::Unlimited.frame_step(FRAME), None);
    }

    #[test]
    fn rates_must_be_finite_and_not_too_slow() {
        for rate in [0.0, -1.0, 0.05, f64::NAN, f64::INFINITY] {
            assert_eq!(Rate::new(rate), None, "{rate}");
        }
        assert_eq!(Rate::new(0.1).map(Rate::get), Some(0.1));
        assert_eq!(Rate::new(1000.0).map(Rate::get), Some(1000.0));
    }

    #[test]
    fn each_pace_has_its_speed() {
        let speeds = Speeds::default();
        assert_eq!(speeds.speed(Pace::Normal), Speed::NORMAL);
        assert_eq!(speeds.speed(Pace::FastForward), times(2.0));
        assert_eq!(speeds.speed(Pace::SlowMotion), times(0.5));
        let unlimited = Speeds {
            fast_forward: Speed::Unlimited,
            ..speeds
        };
        assert_eq!(unlimited.speed(Pace::FastForward), Speed::Unlimited);
    }

    #[test]
    fn speeds_are_labelled_as_multiples() {
        let labels = FAST_FORWARD_CHOICES.map(Speed::label);
        assert_eq!(labels, ["2×", "3×", "4×", "Unlimited"]);
        assert_eq!(times(0.5).label(), "0.5×");
    }

    #[test]
    fn deadlines_advance_by_the_step() {
        let start = Instant::now();
        let early = start + Duration::from_millis(1);
        assert_eq!(next_deadline(start, early, Some(FRAME)), start + FRAME);
        let step = times(4.0).frame_step(FRAME);
        assert_eq!(next_deadline(start, early, step), start + FRAME / 4);
    }

    #[test]
    fn a_small_debt_is_caught_up_and_a_large_one_dropped() {
        let start = Instant::now();
        let late = start + FRAME * 3;
        assert_eq!(next_deadline(start, late, Some(FRAME)), start + FRAME);
        let far_behind = start + FRAME * 6;
        assert_eq!(next_deadline(start, far_behind, Some(FRAME)), far_behind);
        // Four quarter-frame steps behind is far behind at 4x.
        let step = times(4.0).frame_step(FRAME);
        let behind = start + FRAME * 3 / 2;
        assert_eq!(next_deadline(start, behind, step), behind);
    }

    #[test]
    fn naps_take_three_quarters_of_the_time_left() {
        assert_eq!(
            nap(Duration::from_millis(10)),
            Some(Duration::from_micros(7500))
        );
        assert_eq!(
            nap(Duration::from_micros(400)),
            Some(Duration::from_micros(300))
        );
        assert_eq!(nap(Duration::from_micros(99)), None);
        assert_eq!(nap(Duration::ZERO), None);
    }

    #[test]
    fn sleeps_until_the_deadline_and_not_long_past_it() {
        let start = Instant::now();
        let deadline = start + Duration::from_millis(5);
        sleep_until(deadline);
        let woke = Instant::now();
        assert!(woke + CLOSE_ENOUGH >= deadline, "woke early");
        // Generous, for a busy machine: one plain sleep would be 1 ms late.
        assert!(woke < deadline + Duration::from_millis(50), "woke late");
        // A deadline already past returns at once.
        sleep_until(start);
    }

    #[test]
    #[ignore = "timing on real hardware: CI's virtual machines wake sleeps 10 ms late or more; run with --ignored"]
    fn sleeps_wake_well_within_a_millisecond_of_their_deadlines() {
        // As the emulation thread sleeps: with its timers, a frame's worth
        // of time at a time. A system timer ticking every 15.6 ms, as
        // Windows' does by default, would wake most of these 4 to 15 ms
        // late.
        let _timers = crate::qos::Timers::fine();
        let mut late: Vec<_> = (0..40)
            .map(|_| {
                let deadline = Instant::now() + Duration::from_micros(4_200);
                sleep_until(deadline);
                Instant::now().saturating_duration_since(deadline)
            })
            .collect();
        late.sort();
        let (median, worst) = (late[late.len() / 2], late[late.len() - 1]);
        eprintln!("sleep_until woke late by {median:?} (median), {worst:?} (worst)");
        // Generous, for a busy machine: the median is what a coarse timer
        // would push past a millisecond.
        assert!(median < Duration::from_millis(1), "{late:?}");
    }

    #[test]
    fn unlimited_is_due_now() {
        let start = Instant::now();
        let now = start + Duration::from_millis(5);
        assert_eq!(next_deadline(start, now, None), now);
    }
}
