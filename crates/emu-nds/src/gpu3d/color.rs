//! Colour formats of the 3D engine (GBATEK, "DS 3D Polygon Attributes":
//! COLOR, and "DS 3D Final 2D Output").
//!
//! Registers and VRAM hold BGR555. Internally every channel has 6 bits,
//! expanded as `x * 2 + 1` for non-zero `x`. Output pixels pack red, green
//! and blue in bits 0–17 and a 5-bit alpha in bits 18–22.

/// A 5-bit channel widened to 6 bits.
pub(crate) fn expand(channel: u32) -> u8 {
    let channel = (channel & 0x1F) as u8;
    if channel == 0 { 0 } else { channel * 2 + 1 }
}

/// BGR555 as 6-bit red, green, blue.
pub(crate) fn rgb6(bgr555: u32) -> [u8; 3] {
    [expand(bgr555), expand(bgr555 >> 5), expand(bgr555 >> 10)]
}

/// An output pixel: 6-bit red, green, blue and 5-bit alpha.
pub(crate) fn pack(rgb: [u8; 3], alpha: u8) -> u32 {
    u32::from(rgb[0]) | u32::from(rgb[1]) << 6 | u32::from(rgb[2]) << 12 | u32::from(alpha) << 18
}

/// The red, green and blue of an output pixel.
pub(crate) fn unpack(pixel: u32) -> [u8; 3] {
    [
        (pixel & 0x3F) as u8,
        ((pixel >> 6) & 0x3F) as u8,
        ((pixel >> 12) & 0x3F) as u8,
    ]
}

/// The alpha of an output pixel.
pub(crate) fn alpha(pixel: u32) -> u8 {
    ((pixel >> 18) & 0x1F) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_widen_and_pack() {
        assert_eq!(rgb6(0x7FFF), [63; 3]);
        assert_eq!(rgb6(0x0001), [3, 0, 0]);
        let pixel = pack([1, 2, 3], 31);
        assert_eq!((unpack(pixel), alpha(pixel)), ([1, 2, 3], 31));
    }
}
