use std::fmt;

/// Width and height of one screen in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenSize {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
}

impl ScreenSize {
    /// Creates a screen size.
    #[must_use]
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }

    /// Number of pixels in a frame of this size.
    #[must_use]
    pub const fn pixel_count(self) -> usize {
        self.width as usize * self.height as usize
    }
}

/// Packs an opaque colour into the pixel format used by [`Frame`].
#[must_use]
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// One screen's pixels, row-major from the top-left corner.
///
/// Each pixel is `0xAARRGGBB` with alpha always `0xFF`; build them with
/// [`rgb`]. In memory on a little-endian host the bytes are B, G, R, A,
/// which is the BGRA8 layout Metal and GPUI upload without conversion.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    size: ScreenSize,
    pixels: Box<[u32]>,
}

impl Frame {
    /// A frame of `size` filled with opaque black.
    #[must_use]
    pub fn new(size: ScreenSize) -> Self {
        Self {
            size,
            pixels: vec![rgb(0, 0, 0); size.pixel_count()].into_boxed_slice(),
        }
    }

    /// The frame's dimensions.
    #[must_use]
    pub const fn size(&self) -> ScreenSize {
        self.size
    }

    /// All pixels, row-major.
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// All pixels, row-major, for the core to draw into.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    /// Iterates over the rows, top to bottom.
    pub fn rows(&self) -> impl Iterator<Item = &[u32]> {
        self.pixels.chunks_exact(usize::from(self.size.width))
    }

    /// Iterates mutably over the rows, top to bottom.
    pub fn rows_mut(&mut self) -> impl Iterator<Item = &mut [u32]> {
        self.pixels.chunks_exact_mut(usize::from(self.size.width))
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// How a core that draws on a GPU of its own (the 3DS) enlarges the game's
/// textures: the frontend's picture, carried into the game's own drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextureUpscaling {
    /// Textures as the game made them.
    #[default]
    Off,
    /// Smoothly, for painted and rendered art.
    Smooth,
    /// With pixel art's edges kept.
    PixelArt,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_is_opaque_bgra_in_little_endian_memory() {
        assert_eq!(
            rgb(0x12, 0x34, 0x56).to_le_bytes(),
            [0x56, 0x34, 0x12, 0xFF]
        );
    }

    #[test]
    fn rows_cover_every_pixel() {
        let frame = Frame::new(ScreenSize::new(240, 160));
        assert_eq!(frame.rows().count(), 160);
        assert!(frame.rows().all(|row| row.len() == 240));
    }
}
