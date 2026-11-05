//! Measures how fast the game runs, for the speed readout that View >
//! Show FPS puts in the window's corner.

use std::time::Instant;

/// Counts frames from one sample to the next.
#[derive(Debug, Clone, Copy)]
pub struct Meter {
    at: Instant,
    frames: u64,
}

impl Meter {
    /// Starts measuring at `now`, with `frames` counted so far.
    pub fn new(now: Instant, frames: u64) -> Self {
        Self { at: now, frames }
    }

    /// Frames a second since the last sample, given `frames` counted so
    /// far at `now`; the next sample starts from here.
    pub fn sample(&mut self, now: Instant, frames: u64) -> f64 {
        let seconds = now.saturating_duration_since(self.at).as_secs_f64();
        let counted = frames.saturating_sub(self.frames);
        *self = Self::new(now, frames);
        if seconds > 0.0 {
            counted as f64 / seconds
        } else {
            0.0
        }
    }
}

/// `fps` as the readout shows it, with the speed that is for a system
/// whose own rate is `native` frames a second: "60 fps · 100%".
pub fn readout(fps: f64, native: f64) -> String {
    let percent = if native > 0.0 {
        fps / native * 100.0
    } else {
        0.0
    };
    format!("{fps:.0} fps · {percent:.0}%")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn frames_a_second_since_the_last_sample() {
        let start = Instant::now();
        let mut meter = Meter::new(start, 100);
        let half = start + Duration::from_millis(500);
        assert_eq!(meter.sample(half, 130), 60.0);
        let second = half + Duration::from_millis(500);
        assert_eq!(meter.sample(second, 130), 0.0, "paused");
        assert_eq!(meter.sample(second, 130), 0.0, "no time passed");
    }

    #[test]
    fn readout_gives_the_rate_and_the_speed() {
        assert_eq!(readout(59.826, 59.8261), "60 fps · 100%");
        assert_eq!(readout(179.0, 59.8261), "179 fps · 299%");
        assert_eq!(readout(0.0, 59.7275), "0 fps · 0%");
    }
}
