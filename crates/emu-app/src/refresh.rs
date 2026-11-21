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
//! [`Lock`] runs frames a whole number of refreshes long instead, or a
//! whole number of frames to a refresh when fast-forward runs them faster,
//! when that is within half a percent of the frame the speed asks for,
//! and nudges each frame's start so that a frame of the usual length is
//! ready a margin before the refresh that shows it: soon enough for a
//! slower frame still to catch it, and late enough not to wait long. Sound
//! plays as much faster as the frames run ([`Lock::speed`]), which the
//! audio's rate control could not absorb on its own.
//!
//! The margin follows how much the game's frames vary. A frame that takes
//! longer than usual is ready later, and one that takes less is ready
//! sooner; each still shows at its own refresh while it is ready between
//! that refresh and the one before. So the margin is placed where the most
//! of the last two seconds' frames would land in that window:
//! [`MARGIN`] for a game whose frames all take about as long, as most do,
//! and more for one whose frames vary by milliseconds, as a busy DS scene
//! or a 3DS game's do, which then shows each frame that much later.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long before the window takes it a frame of the usual length should
/// be ready, at least.
const MARGIN: Duration = Duration::from_millis(2);

/// How far inside its refresh window a frame should be ready, at either
/// end: room for a late wake-up or a slow redraw, which no frame length
/// shows.
const SLACK: Duration = Duration::from_millis(1);

/// The steps the margin is chosen in.
const MARGIN_STEP: Duration = Duration::from_micros(250);

/// How many frames' lengths are kept to see how they vary: two seconds of
/// frames at 60 a second.
const LENGTHS_KEPT: usize = 120;

/// How many lengths it takes for them to count.
const LENGTHS_NEEDED: usize = 30;

/// One frame in this many may miss its refresh for a smaller margin.
const OUTLIERS: usize = 50;

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

/// A frame published and not yet drawn.
#[derive(Debug, Clone, Copy)]
struct Published {
    sequence: u64,
    at: Instant,
    /// From its start to its publishing, if it was run rather than shown
    /// again, as after a state loads.
    length: Option<Duration>,
}

/// Frame timing locked to the display's refreshes, when it can be.
#[derive(Debug)]
pub struct Lock {
    /// The system's own frame length.
    native: Duration,
    /// The display's refresh interval, if known.
    refresh: Option<Duration>,
    /// When the frame under way started, until it is published.
    started: Option<Instant>,
    /// Recent frames published, oldest first.
    published: VecDeque<Published>,
    /// The lengths of recent frames run, oldest first.
    lengths: VecDeque<Duration>,
    /// The margin the last frame drawn was measured against.
    margin: Duration,
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
            started: None,
            published: VecDeque::with_capacity(KEPT),
            lengths: VecDeque::with_capacity(LENGTHS_KEPT),
            margin: MARGIN,
            drawn: 0,
            drawn_at: None,
            nudge: 0.0,
        }
    }

    /// Takes the display's refresh interval, or `None` if unknown.
    pub fn set_refresh(&mut self, refresh: Option<Duration>) {
        self.refresh = refresh.filter(|refresh| !refresh.is_zero());
    }

    /// Notes that a frame started running at `at`.
    pub fn started(&mut self, at: Instant) {
        self.started = Some(at);
    }

    /// Notes that frame `sequence` was published at `at`: the frame
    /// started last, if one did since the last was published, and
    /// otherwise the same one again, as after a state loads.
    pub fn published(&mut self, sequence: u64, at: Instant) {
        let length = self
            .started
            .take()
            .map(|started| at.saturating_duration_since(started));
        if let Some(length) = length {
            if self.lengths.len() == LENGTHS_KEPT {
                self.lengths.pop_front();
            }
            self.lengths.push_back(length);
        }
        if self.published.len() == KEPT {
            self.published.pop_front();
        }
        self.published.push_back(Published {
            sequence,
            at,
            length,
        });
    }

    /// Notes that the window took frame `sequence` to draw at `at`, and
    /// works out how to move the next frame's start.
    pub fn drawn(&mut self, sequence: u64, at: Instant) {
        if sequence <= self.drawn {
            return;
        }
        self.drawn = sequence;
        self.drawn_at = Some(at);
        let (Some(refresh), Some(&published)) = (
            self.refresh,
            self.published
                .iter()
                .find(|published| published.sequence == sequence),
        ) else {
            return;
        };
        let typical = self.typical();
        self.margin = self.margin(refresh, typical);
        let refresh = refresh.as_secs_f64();
        let mut waited = at.saturating_duration_since(published.at).as_secs_f64();
        // How long a frame of the usual length would have waited, had it
        // started when this one did: the frames' starts are what moves,
        // and this one's own length would only shake them.
        if let (Some(length), Some(typical)) = (published.length, typical) {
            waited += length.as_secs_f64() - typical;
        }
        // Past the margin, from half a refresh early to half one late. A
        // frame that just missed its refresh waited most of the next one,
        // which is just late.
        let error = waited - self.margin.as_secs_f64();
        let error = error - refresh * (error / refresh).round();
        let most = MAX_NUDGE.as_secs_f64();
        self.nudge = (error * GAIN).clamp(-most, most);
    }

    /// The margin frames are measured against: how long the window took
    /// the last frame drawn after a frame of the usual length would be
    /// ready.
    pub fn margin_used(&self) -> Duration {
        self.margin
    }

    /// The mean length of recent frames, in seconds, once there are
    /// enough of them.
    fn typical(&self) -> Option<f64> {
        (self.lengths.len() >= LENGTHS_NEEDED).then(|| {
            self.lengths.iter().map(Duration::as_secs_f64).sum::<f64>() / self.lengths.len() as f64
        })
    }

    /// The least margin, at least [`MARGIN`], that lets about as many
    /// recent frames as any show at their own refresh, `refresh` apart,
    /// when a frame of the `typical` length is ready that long before it.
    /// Up to one frame in [`OUTLIERS`] may be given up for a smaller one:
    /// a frame now and then that takes far longer, as when a game loads,
    /// misses its refresh whatever the margin.
    fn margin(&self, refresh: Duration, typical: Option<f64>) -> Duration {
        let Some(typical) = typical else {
            return MARGIN;
        };
        // How much longer than usual each frame took, shortest first.
        let mut over: Vec<f64> = self
            .lengths
            .iter()
            .map(|length| length.as_secs_f64() - typical)
            .collect();
        over.sort_by(f64::total_cmp);
        let (refresh, slack) = (refresh.as_secs_f64(), SLACK.as_secs_f64());
        // With the margin `margin`, a frame `over` longer than usual is
        // ready `margin - over` before its refresh: in time while that is
        // at least the slack, and a refresh early once it is more than a
        // refresh less the slack.
        let caught = |margin: f64| {
            let in_time = over.partition_point(|&over| over <= margin - slack);
            let early = over.partition_point(|&over| over < margin - refresh + slack);
            in_time.saturating_sub(early)
        };
        let margins = std::iter::successors(Some(MARGIN), |margin| Some(*margin + MARGIN_STEP))
            .take_while(|margin| margin.as_secs_f64() <= refresh - slack);
        let counted: Vec<(Duration, usize)> = margins
            .map(|margin| (margin, caught(margin.as_secs_f64())))
            .collect();
        let most = counted.iter().map(|(_, count)| *count).max().unwrap_or(0);
        let enough = most.saturating_sub(over.len() / OUTLIERS);
        counted
            .iter()
            .find(|(_, count)| *count >= enough)
            .map_or(MARGIN, |(margin, _)| *margin)
    }

    /// The length that frames asked to last `frame` are locked to at
    /// `now`, if they are: a whole number of refreshes, or of frames to a
    /// refresh, near it.
    fn locked(&self, now: Instant, frame: Duration) -> Option<Duration> {
        let refresh = self.refresh?;
        let recent = self
            .drawn_at
            .is_some_and(|at| now.saturating_duration_since(at) <= DRAWN_RECENTLY);
        if !recent || frame.is_zero() {
            return None;
        }
        let ratio = frame.as_secs_f64() / refresh.as_secs_f64();
        // Whole numbers of at least one, within u32 as `frame` is a
        // frame's length and `refresh` a display's.
        let locked = if ratio >= 1.0 {
            refresh * ratio.round() as u32
        } else {
            refresh / (1.0 / ratio).round() as u32
        };
        let off = (locked.as_secs_f64() / frame.as_secs_f64() - 1.0).abs();
        (off <= TOLERANCE).then_some(locked)
    }

    /// How long after one frame's start the next starts, at `now`, for
    /// frames asked to last `frame`: locked to the refreshes and nudged
    /// while they can be, and otherwise `frame`.
    pub fn step(&mut self, now: Instant, frame: Duration) -> Duration {
        let Some(locked) = self.locked(now, frame) else {
            self.nudge = 0.0;
            return frame;
        };
        let nudge = std::mem::take(&mut self.nudge);
        Duration::from_secs_f64((locked.as_secs_f64() + nudge).max(0.0))
    }

    /// How many times faster than the system's own clock frames run at
    /// normal speed at `now`, for the sound to keep up: 1 unless locked.
    pub fn speed(&self, now: Instant) -> f64 {
        self.locked(now, self.native).map_or(1.0, |locked| {
            self.native.as_secs_f64() / locked.as_secs_f64()
        })
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

    /// A lock at 120 Hz that has run frames of `lengths`, in turn, each
    /// drawn `waited` after it was published.
    fn ran(lengths: &[Duration], waited: Duration) -> Lock {
        let mut lock = Lock::new(DS);
        lock.set_refresh(Some(HZ_120));
        let mut now = Instant::now();
        for (sequence, length) in (1..).zip(lengths) {
            lock.started(now);
            now += *length;
            lock.published(sequence, now);
            lock.drawn(sequence, now + waited);
            now += HZ_120 * 2 - *length;
        }
        lock
    }

    fn millis(millis: f64) -> Duration {
        Duration::from_secs_f64(millis / 1000.0)
    }

    #[test]
    fn frames_last_whole_refreshes_when_one_is_close() {
        let now = Instant::now();
        let drawn = now + MARGIN;
        let step = |native, refresh, frame| drawing(native, refresh, now).step(drawn, frame);
        assert_eq!(step(DS, HZ_120, DS), HZ_120 * 2);
        assert_eq!(step(GBA, HZ_120, GBA), HZ_120 * 2);
        assert_eq!(step(RPG_MAKER, HZ_120, RPG_MAKER), HZ_120 * 3);
        let hz_60 = Duration::from_nanos(16_666_667);
        assert_eq!(step(DS, hz_60, DS), hz_60);
        // At 144 Hz two refreshes are 17% short of a DS frame.
        let hz_144 = Duration::from_nanos(6_944_444);
        assert_eq!(step(DS, hz_144, DS), DS);
        // Fast-forward: 2x and 4x fit one and two frames to a refresh;
        // 3x, one and a half, does not.
        assert_eq!(step(DS, HZ_120, DS / 2), HZ_120);
        assert_eq!(step(DS, HZ_120, DS / 4), HZ_120 / 2);
        assert_eq!(step(DS, HZ_120, DS / 3), DS / 3);
        assert_eq!(step(RPG_MAKER, HZ_120, RPG_MAKER / 3), HZ_120);
        let speed = drawing(DS, HZ_120, now).speed(drawn);
        assert!((speed - 1.0029).abs() < 0.0001, "{speed}");
    }

    #[test]
    fn frames_keep_their_own_clock_while_the_window_draws_none() {
        let now = Instant::now();
        let mut unknown = Lock::new(DS);
        assert_eq!(unknown.step(now, DS), DS);
        let mut lock = drawing(DS, HZ_120, now);
        let later = now + DRAWN_RECENTLY * 2;
        assert_eq!(lock.step(later, DS), DS);
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
            lock.step(now + waited, DS)
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
        // Frame 1 again, drawn 5 ms later than first: counted, it would
        // nudge the next start.
        lock.drawn(1, now + Duration::from_millis(7));
        assert_eq!(lock.step(now, DS), HZ_120 * 2);
    }

    #[test]
    fn frames_of_one_length_keep_the_least_margin() {
        let lock = ran(&[millis(4.0); 60], MARGIN);
        assert_eq!(lock.margin_used(), MARGIN);
        // A little either way still fits.
        let lengths: Vec<Duration> = (0..60)
            .map(|i| millis(4.0 + f64::from(i % 3) * 0.3))
            .collect();
        assert_eq!(ran(&lengths, MARGIN).margin_used(), MARGIN);
    }

    #[test]
    fn frames_that_vary_get_room_for_the_slow_ones() {
        // Mostly 5 ms, every fifth 9 ms: as a busy scene runs.
        let lengths: Vec<Duration> = (0..100)
            .map(|i| millis(if i % 5 == 0 { 9.0 } else { 5.0 }))
            .collect();
        let lock = ran(&lengths, MARGIN);
        let margin = lock.margin_used().as_secs_f64() * 1000.0;
        // The usual frame takes 5.8 ms on average; a 9 ms one is 3.2 ms
        // over it, and needs the slack beyond.
        assert!((4.2..=5.0).contains(&margin), "{margin} ms");
        // Not so much that the 5 ms frames are ready a refresh early:
        // 0.8 ms under usual, they are ready margin + 0.8 ms ahead.
        assert!(margin + 0.8 < 8.33 - 1.0, "{margin} ms");
    }

    #[test]
    fn frames_too_varied_for_one_refresh_keep_the_most() {
        // Half 2 ms, half 12 ms: no margin catches both, as they are
        // more than a refresh apart, so it settles for one half without
        // going past a refresh.
        let lengths: Vec<Duration> = (0..100)
            .map(|i| millis(if i % 2 == 0 { 2.0 } else { 12.0 }))
            .collect();
        let margin = ran(&lengths, MARGIN).margin_used();
        assert!(margin >= MARGIN && margin <= HZ_120 - SLACK, "{margin:?}");
    }

    #[test]
    fn a_slow_frame_does_not_shake_the_ones_after() {
        // Frames of 4 ms, drawn on time, then one of 7 ms started as they
        // were: a 4 ms frame would be ready 2 ms before the window takes
        // it, 6 ms in, but this one is ready at 7 ms and waits for the
        // refresh after. Its start was right, so the next start barely
        // moves, and one such frame leaves the margin as it was.
        let mut lock = ran(&[millis(4.0); 60], MARGIN);
        let now = Instant::now();
        lock.started(now);
        lock.published(1000, now + millis(7.0));
        lock.drawn(1000, now + millis(6.0) + HZ_120);
        assert_eq!(lock.margin_used(), MARGIN);
        let step = lock.step(now + millis(8.0), DS).as_secs_f64();
        assert!(
            (step - (HZ_120 * 2).as_secs_f64()).abs() < 0.000_1,
            "{step}"
        );
    }
}
