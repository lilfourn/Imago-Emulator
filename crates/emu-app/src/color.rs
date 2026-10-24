//! Colour correction: shows Game Boy Color and Game Boy Advance colours
//! the way the handheld's own screen did.
//!
//! Games were drawn for small LCDs whose primaries bled into each other,
//! and the GBA's unlit panel showed everything much darker than the
//! numbers say, so artists brightened their palettes. Shown as-is on a
//! modern display they look garish. A model decodes each colour with the
//! panel's gamma, mixes some of each primary into the others in linear
//! light, and encodes the result for a gamma 2.2 display. The constants
//! are our own, chosen by eye.

use emu_core::{Frame, System};

/// Gamma of the display the corrected image is shown on.
const DISPLAY_GAMMA: f32 = 2.2;

/// How one handheld screen renders colour.
struct Model {
    /// Gamma of the handheld's panel.
    panel_gamma: f32,
    /// Rows: output red, green, blue; columns: input red, green, blue.
    /// Each row sums to 1, so white stays white.
    mix: [[f32; 3]; 3],
}

/// The Game Boy Color's reflective LCD: muted colours, true greys. Its
/// gamma matches the display's, so greys (and DMG games) are unchanged.
const GBC: Model = Model {
    panel_gamma: DISPLAY_GAMMA,
    mix: [[0.82, 0.12, 0.06], [0.04, 0.78, 0.18], [0.16, 0.10, 0.74]],
};

/// The Game Boy Advance's unlit LCD: muted and much darker in the mids.
const GBA: Model = Model {
    panel_gamma: 3.2,
    mix: [[0.80, 0.18, 0.02], [0.06, 0.82, 0.12], [0.16, 0.06, 0.78]],
};

/// A lookup table from every 15-bit console colour to its corrected
/// 0xFFRRGGBB pixel.
pub struct Correction {
    table: Box<[u32]>,
}

impl Correction {
    /// The correction for `system`'s screen, or `None` if its colours need
    /// none (the DS has a backlit screen, and RPG Maker games were made on
    /// a computer's).
    pub fn for_system(system: System) -> Option<Self> {
        match system {
            System::Gb => Some(Self::new(&GBC)),
            System::Gba => Some(Self::new(&GBA)),
            System::Nds | System::RpgMaker => None,
        }
    }

    fn new(model: &Model) -> Self {
        let table = (0..0x8000u32)
            .map(|color| {
                let linear = [0, 5, 10]
                    .map(|shift| ((color >> shift) & 31) as f32 / 31.0)
                    .map(|level| level.powf(model.panel_gamma));
                let [red, green, blue] = model.mix.map(|row| {
                    let mixed: f32 = row
                        .iter()
                        .zip(linear)
                        .map(|(weight, level)| weight * level)
                        .sum();
                    (mixed.clamp(0.0, 1.0).powf(1.0 / DISPLAY_GAMMA) * 255.0).round() as u32
                });
                0xFF00_0000 | red << 16 | green << 8 | blue
            })
            .collect();
        Self { table }
    }

    /// The corrected colour of `pixel` (0xFFRRGGBB). Cores widen a 15-bit
    /// palette colour's channels to eight bits as `x << 3 | x >> 2`; any
    /// other pixel (the DMG's greys) passes through unchanged.
    pub fn pixel(&self, pixel: u32) -> u32 {
        let [red, green, blue] = [16, 8, 0].map(|shift| (pixel >> shift) & 0xFF);
        let widened = |channel: u32| channel == (channel >> 3) << 3 | channel >> 5;
        if !(widened(red) && widened(green) && widened(blue)) {
            return pixel;
        }
        self.table[(red >> 3 | (green >> 3) << 5 | (blue >> 3) << 10) as usize]
    }

    /// A corrected copy of `frame`.
    pub fn frame(&self, frame: &Frame) -> Frame {
        let mut out = Frame::new(frame.size());
        for (corrected, pixel) in out.pixels_mut().iter_mut().zip(frame.pixels()) {
            *corrected = self.pixel(*pixel);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::rgb;

    #[test]
    fn game_boy_color_keeps_greys() {
        let correction = Correction::for_system(System::Gb).expect("GBC has a model");
        for level in 0..32 {
            let grey = level << 3 | level >> 2;
            let corrected = correction.pixel(rgb(grey, grey, grey));
            let [blue, green, red, _] = corrected.to_le_bytes();
            assert!(red == green && green == blue, "{corrected:#x}");
            assert!(red.abs_diff(grey) <= 1, "{grey:#x} became {red:#x}");
        }
    }

    #[test]
    fn dmg_greys_pass_through() {
        let correction = Correction::for_system(System::Gb).expect("GBC has a model");
        for grey in [0x55, 0xAA] {
            assert_eq!(
                correction.pixel(rgb(grey, grey, grey)),
                rgb(grey, grey, grey)
            );
        }
    }

    #[test]
    fn game_boy_advance_darkens_mids_and_mutes_primaries() {
        let correction = Correction::for_system(System::Gba).expect("GBA has a model");
        assert_eq!(
            correction.pixel(rgb(0xFF, 0xFF, 0xFF)),
            rgb(0xFF, 0xFF, 0xFF)
        );
        let mid = correction.pixel(rgb(0x84, 0x84, 0x84)) & 0xFF;
        assert!(mid < 0x70, "{mid:#x}");
        let red = correction.pixel(rgb(0xFF, 0, 0));
        let (r, g, b) = ((red >> 16) & 0xFF, (red >> 8) & 0xFF, red & 0xFF);
        assert!(r > g && g > 0 && b > 0, "{red:#x}");
    }

    #[test]
    fn the_ds_needs_none() {
        assert!(Correction::for_system(System::Nds).is_none());
    }
}
