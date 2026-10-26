//! Keeps frames in step with the display's refreshes (decision 0003).
//!
//! A 120 Hz ProMotion display refreshes every 8.3 ms. A DS frame lasts
//! 16.7 ms, a Game Boy's 16.74 ms and an RPG Maker game's 25 ms: about two
//! or three refreshes, but not quite. On its own clock each frame landed a
//! little later against the refreshes than the one before, and while it
//! landed near one, the time a frame takes to run decided whether it
//! caught it, so frames showed for one refresh or three instead of two,
//! several times a second.
//!
//! [`Lock`] runs frames a whole number of refreshes long instead, when one
//! is within half a percent of the system's own frame, and nudges each
//! frame's start so that it is ready [`MARGIN`] before the refresh that
//! shows it: soon enough for a slow frame still to catch it, and late
//! enough not to wait long. Sound plays as much faster as the frames run
//! ([`Lock::speed`]), which the audio's rate control could not absorb on
//! its own.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long before the window takes a frame it should be ready.
const MARGIN: Duration = Duration::from_millis(2);

/// The most a frame's start moves to line it up: 6% of a 60 Hz frame,
/// so lining up after a pause takes a few frames without hurrying any.
const MAX_NUDGE: Duration = Duration::from_millis(1);

/// How much of the error one nudge corrects.
const GAIN: f64 = 0.3;

/// The furthest a whole number of refreshes may be from the system's
/// frame, as a fraction of it: the audio's rate control absorbs the rest.
const TOLERANCE: f64 = 0.005;

/// How long the window may go without drawing a frame before frames go
/// back to their own clock, as when it is covered and draws nothing.
const DRAWN_RECENTLY: Duration = Duration::from_millis(250);

/// Frames whose publishing times are kept until the window draws them.
const KEPT: usize = 8;

/// Frame timing locked to the display's refreshes, when it can be.
#[derive(Debug)]
pub struct Lock {
    /// The system's own frame length.
    native: Duration,
    /// The display's refresh interval, if known.
    refresh: Option<Duration>,
    /// When recent frames were published, by sequence number.
    published: VecDeque<(u64, Instant)>,
    /// The newest frame drawn that has been counted.
    drawn: u64,
    /// When the window last drew a frame.
    drawn_at: Option<Instant>,
    /// How much to move the next frame's start, in seconds: later if
    /// positive.
    nudge: f64,
}

impl Lock {
    /// Unlocked frames `native` long, until the display is known.
    pub fn new(native: Duration) -> Self {
        Self {
            native,
            refresh: None,
            published: VecDeque::with_capacity(KEPT),
            drawn: 0,
            drawn_at: None,
            nudge: 0.0,
        }
    }

    /// Takes the display's refresh interval, or `None` if unknown.
    pub fn set_refresh(&mut self, refresh: Option<Duration>) {
        self.refresh = refresh.filter(|refresh| !refresh.is_zero());
    }

    /// Notes that frame `sequence` was published at `at`.
    pub fn published(&mut self, sequence: u64, at: Instant) {
        if self.published.len() == KEPT {
            self.published.pop_front();
        }
        self.published.push_back((sequence, at));
    }

    /// Notes that the window took frame `sequence` to draw at `at`, and
    /// works out how to move the next frame's start.
    pub fn drawn(&mut self, sequence: u64, at: Instant) {
        if sequence <= self.drawn {
            return;
        }
        self.drawn = sequence;
        self.drawn_at = Some(at);
        let (Some(refresh), Some(&(_, published))) = (
            self.refresh,
            self.published
                .iter()
                .find(|(published, _)| *published == sequence),
        ) else {
            return;
        };
        let refresh = refresh.as_secs_f64();
        let waited = at.saturating_duration_since(published).as_secs_f64();
        // Past the margin, from half a refresh early to half one late. A
        // frame that just missed its refresh waited most of the next one,
        // which is just late.
        let error = waited - MARGIN.as_secs_f64();
        let error = error - refresh * (error / refresh).round();
        let most = MAX_NUDGE.as_secs_f64();
        self.nudge = (error * GAIN).clamp(-most, most);
    }

    /// The whole number of refreshes that frames last, if frames are
    /// locked to them at `now`.
    fn refreshes(&self, now: Instant) -> Option<(u32, Duration)> {
        let refresh = self.refresh?;
        let recent = self
            .drawn_at
            .is_some_and(|at| now.saturating_duration_since(at) <= DRAWN_RECENTLY);
        if !recent {
            return None;
        }
        let count = (self.native.as_secs_f64() / refresh.as_secs_f64()).round();
        // At least one refresh, and within u32 as the check below holds.
        let count = count.max(1.0) as u32;
        let frame = refresh * count;
        let off = (frame.as_secs_f64() / self.native.as_secs_f64() - 1.0).abs();
        (off <= TOLERANCE).then_some((count, frame))
    }

    /// How long after one frame's start the next starts, at normal speed,
    /// from `now`: a whole number of refreshes, nudged, while locked, and
    /// otherwise the system's own frame.
    pub fn step(&mut self, now: Instant) -> Duration {
        let Some((_, frame)) = self.refreshes(now) else {
            self.nudge = 0.0;
            return self.native;
        };
        let nudge = std::mem::take(&mut self.nudge);
        Duration::from_secs_f64((frame.as_secs_f64() + nudge).max(0.0))
    }

    /// How many times faster than the system's own clock frames run at
    /// `now`, for the sound to keep up: 1 unless locked.
    pub fn speed(&self, now: Instant) -> f64 {
        self.refreshes(now).map_or(1.0, |(_, frame)| {
            self.native.as_secs_f64() / frame.as_secs_f64()
        })
    }

    /// The newest frame drawn that has been counted.
    #[cfg(test)]
    fn frames_drawn(&self) -> u64 {
        self.drawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ_120: Duration = Duration::from_nanos(8_333_333);
    const DS: Duration = Duration::from_nanos(16_715_000);
    const GBA: Duration = Duration::from_nanos(16_742_706);
    const RPG_MAKER: Duration = Duration::from_millis(25);

    fn drawing(native: Duration, refresh: Duration, now: Instant) -> Lock {
        let mut lock = Lock::new(native);
        lock.set_refresh(Some(refresh));
        lock.published(1, now);
        lock.drawn(1, now + MARGIN);
        lock
    }

    #[test]
    fn frames_last_whole_refreshes_when_one_is_close() {
        let now = Instant::now();
        let drawn = now + MARGIN;
        assert_eq!(drawing(DS, HZ_120, now).step(drawn), HZ_120 * 2);
        assert_eq!(drawing(GBA, HZ_120, now).step(drawn), HZ_120 * 2);
        assert_eq!(drawing(RPG_MAKER, HZ_120, now).step(drawn), HZ_120 * 3);
        let hz_60 = Duration::from_nanos(16_666_667);
        assert_eq!(drawing(DS, hz_60, now).step(drawn), hz_60);
        // At 144 Hz two refreshes are 17% short of a DS frame.
        let hz_144 = Duration::from_nanos(6_944_444);
        assert_eq!(drawing(DS, hz_144, now).step(drawn), DS);
        let speed = drawing(DS, HZ_120, now).speed(drawn);
        assert!((speed - 1.0029).abs() < 0.0001, "{speed}");
    }

    #[test]
    fn frames_keep_their_own_clock_while_the_window_draws_none() {
        let now = Instant::now();
        let mut unknown = Lock::new(DS);
        assert_eq!(unknown.step(now), DS);
        let mut lock = drawing(DS, HZ_120, now);
        let later = now + DRAWN_RECENTLY * 2;
        assert_eq!(lock.step(later), DS);
        assert_eq!(lock.speed(later), 1.0);
    }

    #[test]
    fn frames_move_towards_being_ready_just_before_their_refresh() {
        let now = Instant::now();
        let step = |waited: Duration| {
            let mut lock = Lock::new(DS);
            lock.set_refresh(Some(HZ_120));
            lock.published(7, now);
            lock.drawn(7, now + waited);
            lock.step(now + waited)
        };
        assert_eq!(step(MARGIN), HZ_120 * 2, "on time");
        assert!(step(Duration::from_millis(5)) > HZ_120 * 2, "early: later");
        assert!(
            step(Duration::from_micros(500)) < HZ_120 * 2,
            "cutting it fine"
        );
        // Just missed its refresh: drawn at the next, most of one later.
        let missed = step(HZ_120 + Duration::from_micros(500));
        assert!(missed < HZ_120 * 2, "late: sooner");
        assert!(step(Duration::from_millis(100)) >= HZ_120 * 2 - MAX_NUDGE);
        assert!(step(Duration::from_millis(100)) <= HZ_120 * 2 + MAX_NUDGE);
    }

    #[test]
    fn only_newer_frames_count() {
        let now = Instant::now();
        let mut lock = drawing(DS, HZ_120, now);
        lock.drawn(1, now + Duration::from_millis(7));
        assert_eq!(lock.step(now), HZ_120 * 2);
        assert_eq!(lock.frames_drawn(), 1);
    }
}
