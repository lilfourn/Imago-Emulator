//! Nintendo DS core.
//!
//! Status: scaffold. It validates the cartridge header and draws a test
//! pattern on both screens, with a crosshair at the touch point so the
//! frontend's touch mapping can be checked. The ARM946E-S and ARM7TDMI will
//! come from `emu-arm`; this crate will own dual-CPU scheduling, IPC, VRAM
//! banking, both 2D engines, the 3D engine, SPI devices, the cartridge
//! protocol and sound. See `docs/ROADMAP.md`, Phase 4.

use emu_core::{
    Core, CoreInfo, Frame, FrameRate, Input, LoadError, Media, ScreenSize, StateError,
    StereoSample, System, test_pattern,
};

const SCREEN: ScreenSize = ScreenSize::new(256, 192);
const SCREENS: [ScreenSize; 2] = [SCREEN, SCREEN];

/// Static facts about the Nintendo DS core. Screen 0 is the top screen and
/// screen 1 the bottom touchscreen.
pub const INFO: CoreInfo = CoreInfo {
    system: System::Nds,
    screens: &SCREENS,
    touch_screen: Some(1),
    players: 1,
    frame_rate: FrameRate::new(33_513_982, 560_190),
    audio_sample_rate: 32_768,
    state_version: 1,
};

/// CRC-16 of the Nintendo logo in every licensed cartridge header (GBATEK,
/// "DS Cartridge Header", 0x15C).
const LOGO_CRC: u16 = 0xCF56;

/// A Nintendo DS with a cartridge inserted.
#[derive(Debug, Clone)]
pub struct Nds {
    screens: [Frame; 2],
    frame_count: u64,
}

impl Nds {
    /// Powers on a DS with `media`'s cartridge.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::InvalidRom`] if the cartridge header is missing
    /// or its logo checksum is wrong.
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        let invalid = |reason| LoadError::InvalidRom {
            system: System::Nds,
            reason,
        };
        let logo_crc = media
            .rom
            .get(0x15C..0x15E)
            .ok_or(invalid("shorter than the 0x180-byte header"))?;
        if u16::from_le_bytes([logo_crc[0], logo_crc[1]]) != LOGO_CRC {
            return Err(invalid("logo checksum at 0x15C is not 0xCF56"));
        }
        Ok(Self {
            screens: [Frame::new(SCREEN), Frame::new(SCREEN)],
            frame_count: 0,
        })
    }
}

impl Core for Nds {
    fn info(&self) -> CoreInfo {
        INFO
    }

    fn run_frame(&mut self, inputs: &[Input], _audio: &mut Vec<StereoSample>) {
        let input = inputs.first().copied().unwrap_or_default();
        self.frame_count += 1;
        let [top, bottom] = &mut self.screens;
        test_pattern::color_bars(top, self.frame_count);
        test_pattern::color_bars(bottom, self.frame_count.wrapping_mul(2));
        if !input.buttons.is_empty() {
            test_pattern::invert(top);
        }
        if let Some(point) = input.touch {
            test_pattern::crosshair(bottom, point);
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
    use emu_core::{TouchPoint, rgb};

    fn rom_with_valid_header() -> Vec<u8> {
        let mut rom = vec![0; 0x200];
        rom[0x15C..0x15E].copy_from_slice(&LOGO_CRC.to_le_bytes());
        rom
    }

    #[test]
    fn rejects_a_bad_logo_checksum() {
        assert!(Nds::new(&Media::new(&[0; 0x200])).is_err());
        assert!(Nds::new(&Media::new(&rom_with_valid_header())).is_ok());
    }

    #[test]
    fn touch_draws_on_the_bottom_screen() {
        let rom = rom_with_valid_header();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        let input = Input {
            touch: Some(TouchPoint { x: 10, y: 20 }),
            ..Input::default()
        };
        nds.run_frame(&[input], &mut Vec::new());
        let bottom = &nds.screens()[1];
        assert_eq!(bottom.pixels()[20 * 256 + 10], rgb(0, 0, 0));
    }
}
