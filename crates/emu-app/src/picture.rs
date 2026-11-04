//! How the screens look: View > Picture ([PRD](../../../docs/PRD-upscaler.md)).
//!
//! Auto, the default, upscales every game to the screen it is shown on:
//! Pixel Art (OmniScale, on the GPU) for the Game Boy family and the DS,
//! Smooth (MetalFX) for RPG Maker and the 3DS. Sharp Pixels is the original
//! look, and the LCD grid and scanlines shade it.
//!
//! An upscaled screen is drawn at its exact size in device pixels, so it
//! fits every display: 1× or Retina, any size, in a window or full screen.
//!
//! The DS and 3DS also draw their 3D larger (View > 3D Resolution). Match
//! Display, the default, draws it as large as the screens are shown, up
//! to [`MATCH_DISPLAY_CAP`], and the upscaler covers the rest.

use emu_core::{ScreenSize, System, TextureUpscaling};

use crate::gpu_upscale::Method;

/// A look for the screens, chosen from View > Picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Picture {
    /// The upscaler that suits the game's system.
    #[default]
    Auto,
    /// Each console pixel a sharp square, as the screens showed them.
    SharpPixels,
    /// Pixel art's edges and curves rebuilt at the display's resolution.
    PixelArt,
    /// Smooth enlargement, for painted art and 3D.
    Smooth,
    /// Dark lines between pixels, as on the handhelds' LCDs.
    LcdGrid,
    /// Dark lines between rows, as on a CRT television.
    Scanlines,
}

/// What a picture means for one game: [`Picture`] with Auto decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Console pixels as sharp squares.
    SharpPixels,
    /// Upscaled on the GPU.
    Upscaled(Method),
    /// Sharp squares with an LCD grid.
    LcdGrid,
    /// Sharp squares with scanlines.
    Scanlines,
}

impl Look {
    /// How a core that draws on a GPU of its own (the 3DS) enlarges the
    /// game's textures for this look: as the screens are upscaled, or not
    /// at all for sharp pixels and the shading filters.
    pub fn texture_upscaling(self) -> TextureUpscaling {
        match self {
            Self::Upscaled(Method::Smooth) => TextureUpscaling::Smooth,
            Self::Upscaled(Method::PixelArt) => TextureUpscaling::PixelArt,
            Self::SharpPixels | Self::LcdGrid | Self::Scanlines => TextureUpscaling::Off,
        }
    }
}

/// Each picture and its name in the settings file.
const NAMES: [(Picture, &str); 6] = [
    (Picture::Auto, "auto"),
    (Picture::SharpPixels, "sharp-pixels"),
    (Picture::PixelArt, "pixel-art"),
    (Picture::Smooth, "smooth"),
    (Picture::LcdGrid, "lcd-grid"),
    (Picture::Scanlines, "scanlines"),
];

/// How many times its own resolution DS and 3DS 3D is drawn at, chosen
/// from View > 3D Resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resolution3d {
    /// As large as the screens are shown ([`matched_resolution`]).
    #[default]
    MatchDisplay,
    /// So many times, 1 to `MAX_RESOLUTION_3D`.
    Times(u32),
}

impl Resolution3d {
    /// The choice as the View menu and settings window name it.
    pub fn label(self) -> String {
        match self {
            Self::MatchDisplay => "Match Display".to_owned(),
            Self::Times(1) => "Native (1×)".to_owned(),
            Self::Times(scale) => format!("{scale}×"),
        }
    }
}

/// The 3D resolutions View > 3D Resolution and the settings window offer.
pub const RESOLUTIONS_3D: [Resolution3d; 7] = [
    Resolution3d::MatchDisplay,
    Resolution3d::Times(1),
    Resolution3d::Times(2),
    Resolution3d::Times(3),
    Resolution3d::Times(4),
    Resolution3d::Times(6),
    Resolution3d::Times(8),
];

/// The most Match Display draws 3D at. The DS's 3D costs the most CPU of
/// anything the app does: in Pokémon White's bedroom, headless on an M5
/// Max, the DS ran at 3.2× full speed with 3D at 2× and 2.7× at 4×, so 4×
/// keeps fast-forward near 3×. The 3DS's helper reads back at most 4×
/// (`emu-3ds` `host.rs`). The upscaler enlarges past it.
pub const MATCH_DISPLAY_CAP: u32 = 4;

/// The 3D resolution that draws each screen at least about as large as it
/// is shown: for each `(target, native)` pair, a screen shown `target`
/// device pixels across whose console draws it `native` pixels across
/// (turned as shown), the whole factor its device pixels need, rounded up
/// unless a quarter or less above a whole number, as the upscaler covers
/// that much; the largest, from 1 to [`MATCH_DISPLAY_CAP`].
pub fn matched_resolution(screens: impl IntoIterator<Item = ((u32, u32), ScreenSize)>) -> u32 {
    let factor = |device: u32, console: u16| {
        let console = u32::from(console.max(1));
        // ceil(device / console - 1/4), in whole numbers.
        (4 * device).saturating_sub(console).div_ceil(4 * console)
    };
    screens
        .into_iter()
        .map(|((width, height), native)| {
            factor(width, native.width).max(factor(height, native.height))
        })
        .max()
        .unwrap_or(1)
        .clamp(1, MATCH_DISPLAY_CAP)
}

/// The most pixels the GPU draws in all a frame's screens together. Past
/// it, screens are drawn as large as fits, and GPUI's linear filter
/// enlarges them the rest of the way, which on an upscaled picture barely
/// shows. Each frame's pixels go to the GPU and back and into GPUI's atlas;
/// filling the 240 Hz external display (about 10 million) took up to
/// 2.5 ms, too much at 3× fast-forward. Revisit with zero-copy drawing
/// (PRD UP-16).
pub const MAX_UPSCALED_PIXELS: u64 = 6_000_000;

impl Picture {
    /// Every picture, in the order the menus list them.
    pub const ALL: [Picture; 6] = [
        Picture::Auto,
        Picture::SharpPixels,
        Picture::PixelArt,
        Picture::Smooth,
        Picture::LcdGrid,
        Picture::Scanlines,
    ];

    /// The picture's name in the settings file.
    pub fn name(self) -> &'static str {
        NAMES
            .iter()
            .find(|(picture, _)| *picture == self)
            .map_or("auto", |(_, name)| name)
    }

    /// The picture with this name in the settings file.
    pub fn from_name(name: &str) -> Option<Self> {
        NAMES
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(picture, _)| *picture)
    }

    /// The picture for a settings file from before View > Picture, whose
    /// `filter` was one of the five old names: Auto, whatever the filter,
    /// as upscaling is on until the player turns it off.
    pub fn from_filter_name(name: &str) -> Option<Self> {
        ["none", "lcd-grid", "scanlines", "scale2x", "scale3x"]
            .contains(&name)
            .then_some(Self::Auto)
    }

    /// Whether the picture upscales some system's games on the GPU.
    pub fn upscales(self) -> bool {
        matches!(self, Self::Auto | Self::PixelArt | Self::Smooth)
    }

    /// The picture as the View menu and settings window name it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::SharpPixels => "Sharp Pixels",
            Self::PixelArt => "Pixel Art",
            Self::Smooth => "Smooth",
            Self::LcdGrid => "LCD Grid",
            Self::Scanlines => "Scanlines",
        }
    }

    /// How a game of `system` looks with this picture. Auto upscales pixel
    /// art for the systems drawn in it, and smoothly for RPG Maker games,
    /// whose art is painted or drawn at twice its pixel size, and the 3DS,
    /// whose screens are already drawn at a higher resolution.
    pub fn look(self, system: System) -> Look {
        match self {
            Self::Auto => match system {
                System::Gb | System::Gba | System::Nds => Look::Upscaled(Method::PixelArt),
                System::RpgMaker | System::N3ds => Look::Upscaled(Method::Smooth),
            },
            Self::SharpPixels => Look::SharpPixels,
            Self::PixelArt => Look::Upscaled(Method::PixelArt),
            Self::Smooth => Look::Upscaled(Method::Smooth),
            Self::LcdGrid => Look::LcdGrid,
            Self::Scanlines => Look::Scanlines,
        }
    }
}

/// The size in whole device pixels of a screen `width` × `height` logical
/// pixels across, on a display of `scale_factor` device pixels to a
/// logical one.
pub fn device_size(width: f32, height: f32, scale_factor: f32) -> (u32, u32) {
    // Saturating float-to-int conversion: negative or NaN sizes become 0.
    let side = |length: f32| (length * scale_factor).round() as u32;
    (side(width), side(height))
}

/// `sizes`, each shrunk by the same factor if needed so that together they
/// are at most `budget` pixels, keeping their shapes.
pub fn within_budget(sizes: &[(u32, u32)], budget: u64) -> Vec<(u32, u32)> {
    let total: u64 = sizes
        .iter()
        .map(|&(width, height)| u64::from(width) * u64::from(height))
        .sum();
    if total <= budget {
        return sizes.to_vec();
    }
    // Pixel counts are far below 2^52, so f64 holds them exactly.
    let shrink = (budget as f64 / total as f64).sqrt();
    sizes
        .iter()
        .map(|&(width, height)| {
            let side = |length: u32| ((f64::from(length) * shrink).floor() as u32).max(1);
            (side(width), side(height))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for (picture, name) in NAMES {
            assert_eq!(picture.name(), name);
            assert_eq!(Picture::from_name(name), Some(picture));
        }
        assert_eq!(Picture::from_name("xbrz"), None);
        assert_eq!(Picture::ALL.len(), NAMES.len());
    }

    #[test]
    fn every_old_filter_starts_upscaled() {
        for name in ["none", "lcd-grid", "scanlines", "scale2x", "scale3x"] {
            assert_eq!(
                Picture::from_filter_name(name),
                Some(Picture::Auto),
                "{name}"
            );
        }
        assert_eq!(Picture::from_filter_name("auto"), None);
    }

    #[test]
    fn auto_upscales_pixel_art_and_smooths_painted_and_3d_games() {
        let pixel_art = Look::Upscaled(Method::PixelArt);
        let smooth = Look::Upscaled(Method::Smooth);
        for system in [System::Gb, System::Gba, System::Nds] {
            assert_eq!(Picture::Auto.look(system), pixel_art, "{system:?}");
        }
        for system in [System::RpgMaker, System::N3ds] {
            assert_eq!(Picture::Auto.look(system), smooth, "{system:?}");
        }
        assert!(Picture::Auto.upscales() && Picture::Smooth.upscales());
        assert!(!Picture::SharpPixels.upscales() && !Picture::LcdGrid.upscales());
        // Other pictures look the same for every system.
        assert_eq!(Picture::Smooth.look(System::Gb), smooth);
        assert_eq!(Picture::PixelArt.look(System::N3ds), pixel_art);
        assert_eq!(Picture::LcdGrid.look(System::Nds), Look::LcdGrid);
    }

    #[test]
    fn match_display_follows_the_displays_pixels() {
        let ds = ScreenSize::new(256, 192);
        let three_ds_top = ScreenSize::new(400, 240);
        // A stacked DS filling the MacBook Pro's height wants 5.1×, the
        // external display's 6.75×: both capped.
        assert_eq!(matched_resolution([((1309, 982), ds)]), 4);
        assert_eq!(matched_resolution([((1728, 1296), ds)]), 4);
        // On a 1080p 1× monitor, 2.8× rounds up to 3×.
        assert_eq!(matched_resolution([((720, 540), ds)]), 3);
        // A quarter or less above a whole number rounds down: the 3DS's
        // top screen at 4.09× draws at 4×, a small DS window at 2.08× at 2×.
        assert_eq!(matched_resolution([((1636, 982), three_ds_top)]), 4);
        assert_eq!(matched_resolution([((533, 400), ds)]), 2);
        // The largest screen decides; nothing shown, or tiny, draws at 1×.
        assert_eq!(matched_resolution([((256, 192), ds), ((720, 540), ds)]), 3);
        assert_eq!(matched_resolution([]), 1);
        assert_eq!(matched_resolution([((40, 30), ds)]), 1);
        // A turned screen compares across its turned sides.
        assert_eq!(
            matched_resolution([((540, 720), ScreenSize::new(192, 256))]),
            3
        );
    }

    #[test]
    fn the_3ds_textures_follow_the_picture() {
        assert_eq!(
            Picture::Auto.look(System::N3ds).texture_upscaling(),
            TextureUpscaling::Smooth
        );
        assert_eq!(
            Picture::PixelArt.look(System::N3ds).texture_upscaling(),
            TextureUpscaling::PixelArt
        );
        for off in [Picture::SharpPixels, Picture::LcdGrid, Picture::Scanlines] {
            assert_eq!(
                off.look(System::N3ds).texture_upscaling(),
                TextureUpscaling::Off
            );
        }
    }

    #[test]
    fn resolution_labels() {
        assert_eq!(Resolution3d::MatchDisplay.label(), "Match Display");
        assert_eq!(Resolution3d::Times(1).label(), "Native (1×)");
        assert_eq!(Resolution3d::Times(6).label(), "6×");
        assert_eq!(Resolution3d::default(), Resolution3d::MatchDisplay);
    }

    #[test]
    fn device_sizes_for_every_kind_of_display() {
        // A DS screen filling half the MacBook Pro's height, on its Retina
        // panel; the same on a 1× 1080p monitor; and a GBA screen at a
        // fractional size on the external display.
        assert_eq!(device_size(654.5, 491.0, 2.0), (1309, 982));
        assert_eq!(device_size(720.0, 540.0, 1.0), (720, 540));
        assert_eq!(device_size(1944.25, 1296.0, 2.0), (3889, 2592));
        assert_eq!(device_size(-3.0, f32::NAN, 2.0), (0, 0));
    }

    #[test]
    fn sizes_over_the_budget_shrink_alike() {
        let within = [(1309, 982), (1309, 982)];
        assert_eq!(within_budget(&within, 6_000_000), within);
        // GB filling the external display: about 7.5 million pixels.
        let shrunk = within_budget(&[(2880, 2592)], 6_000_000);
        let (width, height) = shrunk[0];
        assert!(u64::from(width) * u64::from(height) <= 6_000_000);
        assert!(width > 2500 && width < 2880, "{width}");
        // The shape stays: 2880 / 2592 = 10 / 9.
        assert!((f64::from(width) / f64::from(height) - 10.0 / 9.0).abs() < 0.01);
        // Two screens shrink by the same factor.
        let two = within_budget(&[(2000, 2000), (1000, 1000)], 1_250_000);
        assert_eq!(two, [(1000, 1000), (500, 500)]);
    }
}
