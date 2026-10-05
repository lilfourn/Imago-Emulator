use std::time::Duration;

use crate::{Frame, Input, ScreenSize, StereoSample, System};

/// One emulated system with a game loaded.
///
/// Each system crate constructs its core from a [`crate::Media`] (for
/// example `emu_gba::Gba::new`), so a core always has a game and there is no
/// "nothing loaded" state to handle.
///
/// # Determinism
///
/// Given the same `Media` and the same sequence of [`Input`]s, a core must
/// produce the same frames, audio and state bytes on every run and on every
/// host. Rewind, netplay and the frame-hash test suite depend on this, so a
/// core never reads the host clock, random numbers or the file system. Its
/// real-time clock starts at [`crate::Media::start_time`] and advances with
/// emulated cycles.
pub trait Core: Send {
    /// Static facts about this core: system, screens, timing and audio rate.
    fn info(&self) -> CoreInfo;

    /// Emulates exactly one video frame.
    ///
    /// `input` is held for the whole frame. Audio produced during the frame
    /// is appended to `audio` at [`CoreInfo::audio_sample_rate`]. The caller
    /// owns the buffer and clears it.
    fn run_frame(&mut self, input: &Input, audio: &mut Vec<StereoSample>);

    /// The screens as of the last completed frame, in [`CoreInfo::screens`]
    /// order. For the DS, index 0 is the top screen.
    fn screens(&self) -> &[Frame];

    /// Power-cycles the system. The loaded game and its backup memory stay.
    fn reset(&mut self);

    /// Appends the complete machine state to `out`.
    ///
    /// The bytes are this core's own format, versioned by
    /// [`CoreInfo::state_version`]. The `emu-state` crate wraps them in a
    /// container with a header and checksum.
    fn save_state(&self, out: &mut Vec<u8>);

    /// Restores state written by [`Core::save_state`] with the same
    /// [`CoreInfo::state_version`].
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] if the bytes are truncated or malformed. On
    /// error the core is left exactly as it was.
    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError>;

    /// How many times the game has written to battery-backed memory (SRAM,
    /// EEPROM, flash or a cartridge clock), or `None` if it has none.
    ///
    /// Frontends compare this with the value at their last flush and call
    /// [`Core::save_backup`] only when it changed, on a timer rather than on
    /// every write.
    fn backup_generation(&self) -> Option<u64> {
        None
    }

    /// Appends the battery save to `out` in the de facto `.sav` layout for
    /// the system (for example, Game Boy cartridge RAM followed by the
    /// 48-byte MBC3 clock trailer). Appends nothing if there is no backup.
    fn save_backup(&self, out: &mut Vec<u8>) {
        let _ = out;
    }

    /// A read-only view of a memory region, for debuggers, cheat search and
    /// game tools.
    fn memory(&self, region: MemoryRegion) -> Option<&[u8]> {
        let _ = region;
        None
    }
}

/// Static facts about a core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreInfo {
    /// The emulated system.
    pub system: System,
    /// Size of each screen, in [`Core::screens`] order.
    pub screens: &'static [ScreenSize],
    /// Index into `screens` of the touchscreen, if the system has one.
    pub touch_screen: Option<usize>,
    /// Exact video refresh rate.
    pub frame_rate: FrameRate,
    /// Rate of the samples appended by [`Core::run_frame`], in Hz.
    pub audio_sample_rate: u32,
    /// Version of the bytes written by [`Core::save_state`]. Bump it whenever
    /// the layout changes.
    pub state_version: u16,
}

/// An exact video refresh rate: the master clock divided by the number of
/// master-clock cycles in one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRate {
    /// Master clock frequency in Hz.
    pub clock_hz: u32,
    /// Master clock cycles per video frame.
    pub cycles_per_frame: u32,
}

impl FrameRate {
    /// Creates a frame rate from a clock and the cycles in one frame.
    #[must_use]
    pub const fn new(clock_hz: u32, cycles_per_frame: u32) -> Self {
        Self {
            clock_hz,
            cycles_per_frame,
        }
    }

    /// Frames per second, for display only. Use [`FrameRate::frame_duration`]
    /// for pacing.
    #[must_use]
    pub fn hz(self) -> f64 {
        f64::from(self.clock_hz) / f64::from(self.cycles_per_frame)
    }

    /// Wall-clock length of one frame, rounded to the nearest nanosecond.
    #[must_use]
    pub fn frame_duration(self) -> Duration {
        let clock = u128::from(self.clock_hz);
        let nanos = (u128::from(self.cycles_per_frame) * 1_000_000_000 + clock / 2) / clock;
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

/// A named memory region exposed through [`Core::memory`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryRegion {
    /// The system's main work RAM: GB WRAM, GBA EWRAM, DS main RAM.
    MainRam,
    /// Video RAM as the CPU sees it.
    VideoRam,
}

/// Why [`Core::load_state`] rejected some state bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    /// The bytes end before the state does.
    #[error("state data ended early")]
    Truncated,
    /// The bytes were written by an incompatible version of the core.
    #[error("state version {found} is not supported (expected {expected})")]
    Version {
        /// Version found in the data.
        found: u16,
        /// Version this core reads.
        expected: u16,
    },
    /// The bytes are the right length but describe an impossible machine.
    #[error("malformed state: {0}")]
    Malformed(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gba_frame_rate_matches_hardware() {
        let rate = FrameRate::new(16_777_216, 280_896);
        assert!((rate.hz() - 59.727_5).abs() < 0.000_1);
        assert_eq!(rate.frame_duration(), Duration::from_nanos(16_742_706));
    }

    #[test]
    fn nds_frame_rate_matches_hardware() {
        let rate = FrameRate::new(33_513_982, 560_190);
        assert!((rate.hz() - 59.826_1).abs() < 0.000_1);
    }
}
