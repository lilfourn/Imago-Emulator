//! Colour formats and the arithmetic of colour special effects and master
//! brightness (GBATEK, "LCD I/O Color Special Effects", "DS Video Stuff",
//! "DS 3D Final 2D Output").
//!
//! Composition works in the LCD's 18-bit colour, packed as the 3D engine
//! packs it: red in bits 0–5, green 6–11, blue 12–17. A 15-bit BGR555
//! channel `x` widens to `x << 1`, plus 1 when `x` is non-zero, so black
//! stays 0 and 31 becomes full intensity 63.

use emu_core::rgb;

/// Full-intensity white in 18-bit colour.
pub(super) const WHITE: u32 = 0x3_FFFF;

fn channels(rgb666: u32) -> [u32; 3] {
    [rgb666 & 0x3F, (rgb666 >> 6) & 0x3F, (rgb666 >> 12) & 0x3F]
}

fn pack([r, g, b]: [u32; 3]) -> u32 {
    r | g << 6 | b << 12
}

/// Widens a BGR555 colour (bit 15 ignored) to 18-bit colour.
pub(super) fn widen(bgr555: u16) -> u32 {
    let channel = |shift: u16| {
        let x = u32::from((bgr555 >> shift) & 0x1F);
        x << 1 | u32::from(x != 0)
    };
    pack([channel(0), channel(5), channel(10)])
}

/// Narrows an 18-bit colour to BGR555 by dropping each channel's low bit.
pub(super) fn narrow(rgb666: u32) -> u16 {
    let [r, g, b] = channels(rgb666).map(|c| (c >> 1) as u16);
    r | g << 5 | b << 10
}

/// Converts an 18-bit colour to `0xFFRRGGBB`.
pub(super) fn to_argb(rgb666: u32) -> u32 {
    let [r, g, b] = channels(rgb666).map(|c| (c << 2 | c >> 4) as u8);
    rgb(r, g, b)
}

/// Alpha blending: `first * eva / 16 + second * evb / 16` per channel,
/// saturating.
pub(super) fn alpha_blend(first: u32, second: u32, eva: u32, evb: u32) -> u32 {
    let (a, b) = (channels(first), channels(second));
    pack([0, 1, 2].map(|i| ((a[i] * eva + b[i] * evb) >> 4).min(0x3F)))
}

/// Blends a 3D pixel over the layer below with its own 5-bit alpha.
/// GBATEK suggests EVA = A/2 and EVB = 16 - A/2; this keeps the fifth bit,
/// weighting `(alpha + 1) / 32` against `(31 - alpha) / 32`, so alpha 31
/// is opaque.
pub(super) fn blend_3d(first: u32, second: u32, alpha: u32) -> u32 {
    let (a, b) = (channels(first), channels(second));
    pack([0, 1, 2].map(|i| (a[i] * (alpha + 1) + b[i] * (31 - alpha)) >> 5))
}

/// Brightness increase: each channel moves `evy`/16 of the way to white.
pub(super) fn brighten(color: u32, evy: u32) -> u32 {
    pack(channels(color).map(|c| c + (((0x3F - c) * evy) >> 4)))
}

/// Brightness decrease: each channel moves `evy`/16 of the way to black.
pub(super) fn darken(color: u32, evy: u32) -> u32 {
    pack(channels(color).map(|c| c - ((c * evy) >> 4)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening_keeps_black_and_reaches_full_intensity() {
        assert_eq!(widen(0), 0);
        assert_eq!(widen(0x7FFF), WHITE);
        assert_eq!(widen(0x001F), 0x3F);
        assert_eq!(widen(0x0001), 3, "non-zero values gain a low bit");
        assert_eq!(widen(0x8000), 0, "bit 15 is not colour");
    }

    #[test]
    fn narrowing_undoes_widening() {
        for value in [0, 1, 0x1F, 0x3E0, 0x7C00, 0x7FFF, 0x1234] {
            assert_eq!(narrow(widen(value)), value);
        }
    }

    #[test]
    fn six_bit_channels_widen_to_eight_bits() {
        assert_eq!(to_argb(WHITE), 0xFFFF_FFFF);
        assert_eq!(to_argb(0x3F), 0xFFFF_0000);
        assert_eq!(to_argb(0x3F << 12), 0xFF00_00FF);
        assert_eq!(to_argb(0), 0xFF00_0000);
    }

    #[test]
    fn alpha_blend_mixes_and_saturates() {
        assert_eq!(alpha_blend(0x3F, 0x3F << 12, 8, 8), 31 | 31 << 12);
        assert_eq!(alpha_blend(WHITE, WHITE, 16, 16), WHITE);
    }

    #[test]
    fn opaque_3d_alpha_keeps_the_3d_colour() {
        assert_eq!(blend_3d(0x3F, 0x3F << 12, 31), 0x3F);
        assert_eq!(blend_3d(0x3F, 0x3F << 12, 15), 31 | 31 << 12);
    }

    #[test]
    fn brightness_moves_towards_white_or_black() {
        assert_eq!(brighten(0, 16), WHITE);
        assert_eq!(darken(WHITE, 16), 0);
        assert_eq!(brighten(0, 8), 31 | 31 << 6 | 31 << 12);
        assert_eq!(darken(WHITE, 8), 32 | 32 << 6 | 32 << 12);
    }
}
