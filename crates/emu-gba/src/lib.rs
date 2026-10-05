//! Game Boy Advance core.
//!
//! Status: scaffold. It validates the cartridge header and draws a test
//! pattern. The ARM7TDMI will come from `emu-arm`; this crate will own the
//! bus, PPU, APU, DMA, timers, cartridge backup and GPIO RTC. See
//! `docs/ROADMAP.md`, Phase 3.

use emu_core::{
    Core, CoreInfo, Frame, FrameRate, Input, LoadError, Media, ScreenSize, StateError,
    StereoSample, System, test_pattern,
};

const SCREENS: [ScreenSize; 1] = [ScreenSize::new(240, 160)];

/// Static facts about the Game Boy Advance core.
pub const INFO: CoreInfo = CoreInfo {
    system: System::Gba,
    screens: &SCREENS,
    touch_screen: None,
    frame_rate: FrameRate::new(16_777_216, 280_896),
    audio_sample_rate: 32_768,
    state_version: 1,
};

/// A Game Boy Advance with a cartridge inserted.
#[derive(Debug, Clone)]
pub struct Gba {
    screens: [Frame; 1],
    frame_count: u64,
}

impl Gba {
    /// Powers on a Game Boy Advance with `media`'s cartridge.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::InvalidRom`] if the cartridge header is missing
    /// or lacks its fixed value (GBATEK, "GBA Cartridge Header", 0xB2).
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        let invalid = |reason| LoadError::InvalidRom {
            system: System::Gba,
            reason,
        };
        let fixed = media
            .rom
            .get(0xB2)
            .ok_or(invalid("shorter than the 192-byte header"))?;
        if *fixed != 0x96 {
            return Err(invalid("header fixed value at 0xB2 is not 0x96"));
        }
        Ok(Self {
            screens: [Frame::new(SCREENS[0])],
            frame_count: 0,
        })
    }
}

impl Core for Gba {
    fn info(&self) -> CoreInfo {
        INFO
    }

    fn run_frame(&mut self, input: &Input, _audio: &mut Vec<StereoSample>) {
        self.frame_count += 1;
        let [screen] = &mut self.screens;
        test_pattern::color_bars(screen, self.frame_count);
        if !input.buttons.is_empty() {
            test_pattern::invert(screen);
        }
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        self.frame_count = 0;
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.frame_count.to_le_bytes());
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let bytes: [u8; 8] = state
            .try_into()
            .map_err(|_| StateError::Malformed("scaffold state is 8 bytes"))?;
        self.frame_count = u64::from_le_bytes(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with_valid_header() -> Vec<u8> {
        let mut rom = vec![0; 0xC0];
        rom[0xB2] = 0x96;
        rom
    }

    #[test]
    fn accepts_a_valid_header() {
        assert!(Gba::new(&Media::new(&rom_with_valid_header())).is_ok());
    }

    #[test]
    fn rejects_a_missing_fixed_value_or_short_rom() {
        assert!(Gba::new(&Media::new(&[0; 0xC0])).is_err());
        assert!(Gba::new(&Media::new(&[0; 0x10])).is_err());
    }

    #[test]
    fn save_state_round_trips() {
        let rom = rom_with_valid_header();
        let mut gba = Gba::new(&Media::new(&rom)).expect("valid header");
        gba.run_frame(&Input::default(), &mut Vec::new());
        let mut state = Vec::new();
        gba.save_state(&mut state);
        gba.reset();
        assert_eq!(gba.load_state(&state), Ok(()));
        assert_eq!(gba.frame_count, 1);
        assert!(gba.load_state(&state[..4]).is_err());
    }
}
