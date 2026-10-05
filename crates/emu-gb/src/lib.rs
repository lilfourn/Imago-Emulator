//! Game Boy and Game Boy Color core.
//!
//! Status: scaffold. It validates the cartridge header and draws a test
//! pattern. Next come the SM83 CPU, timer, PPU, APU and MBC1/MBC3/MBC5
//! cartridges; see `docs/ROADMAP.md`, Phases 1 and 2.

use emu_core::{
    Core, CoreInfo, Frame, FrameRate, Input, LoadError, Media, ScreenSize, StateError,
    StereoSample, System, test_pattern,
};

const SCREENS: [ScreenSize; 1] = [ScreenSize::new(160, 144)];

/// Static facts about the Game Boy core.
pub const INFO: CoreInfo = CoreInfo {
    system: System::Gb,
    screens: &SCREENS,
    touch_screen: None,
    frame_rate: FrameRate::new(4_194_304, 70_224),
    audio_sample_rate: 48_000,
    state_version: 1,
};

/// A Game Boy with a cartridge inserted.
#[derive(Debug, Clone)]
pub struct Gb {
    screens: [Frame; 1],
    frame_count: u64,
}

impl Gb {
    /// Powers on a Game Boy with `media`'s cartridge.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::InvalidRom`] if the cartridge header is missing
    /// or its checksum (Pan Docs, "0x014D") does not match.
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        let invalid = |reason| LoadError::InvalidRom {
            system: System::Gb,
            reason,
        };
        let header = media
            .rom
            .get(0x134..=0x14D)
            .ok_or(invalid("shorter than the 0x150-byte header"))?;
        let (title_to_version, checksum) = header.split_at(header.len() - 1);
        let computed = title_to_version
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        if computed != checksum[0] {
            return Err(invalid("header checksum does not match"));
        }
        Ok(Self {
            screens: [Frame::new(SCREENS[0])],
            frame_count: 0,
        })
    }
}

impl Core for Gb {
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
        let mut rom = vec![0; 0x150];
        rom[0x134..0x13A].copy_from_slice(b"SCAFFO");
        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        rom
    }

    #[test]
    fn accepts_a_valid_header() {
        assert!(Gb::new(&Media::new(&rom_with_valid_header())).is_ok());
    }

    #[test]
    fn rejects_a_bad_checksum_or_short_rom() {
        let mut rom = rom_with_valid_header();
        rom[0x14D] ^= 0xFF;
        assert!(Gb::new(&Media::new(&rom)).is_err());
        assert!(Gb::new(&Media::new(&rom[..0x100])).is_err());
    }

    #[test]
    fn save_state_round_trips() {
        let rom = rom_with_valid_header();
        let mut gb = Gb::new(&Media::new(&rom)).expect("valid header");
        for _ in 0..3 {
            gb.run_frame(&Input::default(), &mut Vec::new());
        }
        let mut state = Vec::new();
        gb.save_state(&mut state);
        gb.reset();
        assert_eq!(gb.load_state(&state), Ok(()));
        assert_eq!(gb.frame_count, 3);
    }
}
