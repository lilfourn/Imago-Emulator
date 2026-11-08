//! How the screens look: View > Picture ([PRD](../../../docs/PRD-upscaler.md)).
//!
//! Auto, the default, draws every game at the resolution of the screen it
//! is shown on, as the game looks: Sharp Pixels for the games drawn in
//! pixel art (the Game Boy family, the DS and RPG Maker), each console
//! pixel a crisp, even square, and Smooth (MetalFX) for the 3DS, whose
//! screens are drawn at a higher resolution already. Rounded Pixels
//! (OmniScale) reshapes pixel art into smooth curves for those who want
//! it, and the LCD grid and scanlines shade sharp pixels.
//!
//! An upscaled screen is drawn at its exact size in device pixels, so it
//! fits every display: 1× or Retina, any size, in a window or full screen.
//!
//! The DS and 3DS also draw their 3D larger (View > 3D Resolution). Match
//! Display, the default, draws it as large as the screens are shown, up
//! to [`match_display_cap`], and the upscaler covers the rest. It keeps
//! the speed asked for first: when the game falls behind, it draws 3D a
//! step smaller ([`keep_speed`]).

use emu_core::{ScreenSize, System, TextureUpscaling};

use crate::gpu_upscale::Method;

/// A look for the screens, chosen from View > Picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Picture {
    /// What suits the game's system: Sharp Pixels, or Smooth for the 3DS.
    #[default]
    Auto,
    /// Each console pixel a crisp, even square, as the screens showed
    /// them, at the display's full resolution.
    SharpPixels,
    /// Smooth enlargement, for painted art and 3D.
    Smooth,
    /// Pixel art's edges reshaped into smooth curves and lines (OmniScale).
    RoundedPixels,
    /// Dark lines between pixels, as on the handhelds' LCDs.
    LcdGrid,
    /// Dark lines between rows, as on a CRT television.
    Scanlines,
}

/// What a picture means for one game: [`Picture`] with Auto decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Drawn at the screen's size on the GPU, as `Method` says.
    Upscaled(Method),
    /// Sharp squares with an LCD grid.
    LcdGrid,
    /// Sharp squares with scanlines.
    Scanlines,
}

/// Each picture and its name in the settings file.
const NAMES: [(Picture, &str); 6] = [
    (Picture::Auto, "auto"),
    (Picture::SharpPixels, "sharp-pixels"),
    (Picture::Smooth, "smooth"),
    (Picture::RoundedPixels, "rounded-pixels"),
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

/// The most Match Display draws 3D at for a game of `system`; the
/// upscaler enlarges past it.
///
/// The DS draws its 3D on the CPU, the costliest thing the app does, and
/// the cost grows with the pixels: in Pokémon Platinum's overworld,
/// headless on an M5 Max, a frame took 6.5 ms of CPU time at 1×, 22 ms at
/// 2× and 70 ms at 4×. At 60 frames a second 4× kept four to five cores
/// busy and slowed the video and windows of the apps beside it, for a
/// picture barely different from 2×: the DS's textures are pixel art, so
/// only polygon edges get finer. Larger stays a menu choice.
///
/// The 3DS draws on the GPU, where resolution costs little, and its
/// helper reads back at most 4× (`emu-3ds` `host.rs`).
///
/// The other systems draw no 3D, so 1.
pub fn match_display_cap(system: System) -> u32 {
    match system {
        System::Nds => 2,
        System::N3ds => 4,
        System::Gb | System::Gba | System::RpgMaker => 1,
    }
}

/// The 3D resolution that draws each screen at least about as large as it
/// is shown: for each `(target, native)` pair, a screen shown `target`
/// device pixels across whose console draws it `native` pixels across
/// (turned as shown), the whole factor its device pixels need, rounded up
/// unless a quarter or less above a whole number, as the upscaler covers
/// that much; the largest, from 1 to `cap` ([`match_display_cap`]).
pub fn matched_resolution(
    screens: impl IntoIterator<Item = ((u32, u32), ScreenSize)>,
    cap: u32,
) -> u32 {
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
        .clamp(1, cap.max(1))
}

/// How often Match Display checks that the game keeps its speed.
pub const SPEED_CHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// The most Match Display may draw 3D at from now, given that it may draw
/// at most `ceiling` and drew at `scale` while the game ran `ran` frames a
/// second, asked for `wanted` (`None` for unlimited speed) on a system of
/// `normal` frames a second: a step smaller if the game fell behind, by
/// more than 5%, and was running at all (not paused or loading), else as
/// it was. It never steps up; the view starts it again at
/// [`match_display_cap`] when the speed asked for or the screens change.
///
/// The DS's 3D is the costliest thing the app does: in Pokémon Platinum's
/// overworld, headless on an M5 Max, the core took 3.75 ms a frame with 3D
/// at 1×, 5.2 ms at 2× and 7.35 ms at 4×, against 5.6 ms a frame at 3×
/// fast-forward.
pub fn keep_speed(ceiling: u32, scale: u32, ran: f64, wanted: Option<f64>, normal: f64) -> u32 {
    let running = ran >= 0.25 * wanted.unwrap_or(normal).min(normal);
    let behind = wanted.is_none_or(|wanted| ran < 0.95 * wanted);
    if running && behind && scale > 1 {
        ceiling.min(scale - 1)
    } else {
        ceiling
    }
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
        Picture::Smooth,
        Picture::RoundedPixels,
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

    /// The picture with this name in the settings file. `pixel-art` is
    /// what Rounded Pixels was first called.
    pub fn from_name(name: &str) -> Option<Self> {
        if name == "pixel-art" {
            return Some(Self::RoundedPixels);
        }
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

    /// How a core that draws on a GPU of its own (the 3DS) enlarges the
    /// game's textures: not at all unless the player chose Smooth or
    /// Rounded Pixels, as filtering changes the art, and 3DS games hold
    /// pixel art too (Omega Ruby's AreaNav map, which ScaleForce blurs).
    /// Auto draws the 3DS's 3D larger instead.
    pub fn texture_upscaling(self) -> TextureUpscaling {
        match self {
            Self::Smooth => TextureUpscaling::Smooth,
            Self::RoundedPixels => TextureUpscaling::PixelArt,
            Self::Auto | Self::SharpPixels | Self::LcdGrid | Self::Scanlines => {
                TextureUpscaling::Off
            }
        }
    }

    /// Whether the picture draws on the GPU: all but the shading filters.
    pub fn upscales(self) -> bool {
        !matches!(self, Self::LcdGrid | Self::Scanlines)
    }

    /// The picture as the View menu and settings window name it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::SharpPixels => "Sharp Pixels",
            Self::Smooth => "Smooth",
            Self::RoundedPixels => "Rounded Pixels",
            Self::LcdGrid => "LCD Grid",
            Self::Scanlines => "Scanlines",
        }
    }

    /// How a game of `system` looks with this picture. Auto keeps pixel
    /// art as crisp pixels for the systems drawn in it (RPG Maker games
    /// included, whose art is pixel art drawn at twice its size), and is
    /// smooth for the 3DS, whose screens are already drawn at a higher
    /// resolution.
    pub fn look(self, system: System) -> Look {
        match self {
            Self::Auto => match system {
                System::Gb | System::Gba | System::Nds | System::RpgMaker => {
                    Look::Upscaled(Method::Sharp)
                }
                System::N3ds => Look::Upscaled(Method::Smooth),
            },
            Self::SharpPixels => Look::Upscaled(Method::Sharp),
            Self::RoundedPixels => Look::Upscaled(Method::Rounded),
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
        assert_eq!(
            Picture::from_name("pixel-art"),
            Some(Picture::RoundedPixels)
        );
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
    fn auto_keeps_pixel_art_sharp_and_smooths_the_3ds() {
        let sharp = Look::Upscaled(Method::Sharp);
        let smooth = Look::Upscaled(Method::Smooth);
        let rounded = Look::Upscaled(Method::Rounded);
        for system in [System::Gb, System::Gba, System::Nds, System::RpgMaker] {
            assert_eq!(Picture::Auto.look(system), sharp, "{system:?}");
        }
        assert_eq!(Picture::Auto.look(System::N3ds), smooth);
        assert!(Picture::Auto.upscales() && Picture::SharpPixels.upscales());
        assert!(!Picture::LcdGrid.upscales() && !Picture::Scanlines.upscales());
        // Other pictures look the same for every system.
        assert_eq!(Picture::Smooth.look(System::Gb), smooth);
        assert_eq!(Picture::SharpPixels.look(System::N3ds), sharp);
        assert_eq!(Picture::RoundedPixels.look(System::N3ds), rounded);
        assert_eq!(Picture::LcdGrid.look(System::Nds), Look::LcdGrid);
    }

    #[test]
    fn match_display_follows_the_displays_pixels() {
        let ds = ScreenSize::new(256, 192);
        let three_ds_top = ScreenSize::new(400, 240);
        // Any cap; 4 is the 3DS's, past the DS's own.
        let cap = 4;
        // A DS stacked to fill the MacBook Pro's height would want 5.1×,
        // the external display 6.75×: both capped.
        assert_eq!(matched_resolution([((1309, 982), ds)], cap), 4);
        assert_eq!(matched_resolution([((1728, 1296), ds)], cap), 4);
        // On a 1080p 1× monitor, 2.8× rounds up to 3×.
        assert_eq!(matched_resolution([((720, 540), ds)], cap), 3);
        // A quarter or less above a whole number rounds down: the 3DS's
        // top screen at 4.09× draws at 4×, a small DS window at 2.08× at 2×.
        assert_eq!(matched_resolution([((1636, 982), three_ds_top)], cap), 4);
        assert_eq!(matched_resolution([((533, 400), ds)], cap), 2);
        // The largest screen decides; nothing shown, or tiny, draws at 1×.
        assert_eq!(
            matched_resolution([((256, 192), ds), ((720, 540), ds)], cap),
            3
        );
        assert_eq!(matched_resolution([], cap), 1);
        assert_eq!(matched_resolution([((40, 30), ds)], cap), 1);
        // A turned screen compares across its turned sides.
        assert_eq!(
            matched_resolution([((540, 720), ScreenSize::new(192, 256))], cap),
            3
        );
    }

    #[test]
    fn the_ds_matches_the_display_at_most_twice_over() {
        let ds = ScreenSize::new(256, 192);
        let three_ds_top = ScreenSize::new(400, 240);
        let ds_cap = match_display_cap(System::Nds);
        let three_ds_cap = match_display_cap(System::N3ds);
        // The MacBook Pro's 5.1× and a 1080p monitor's 2.8× both draw the
        // DS's 3D at 2×; a small window still at less.
        assert_eq!(matched_resolution([((1309, 982), ds)], ds_cap), 2);
        assert_eq!(matched_resolution([((720, 540), ds)], ds_cap), 2);
        assert_eq!(matched_resolution([((300, 225), ds)], ds_cap), 1);
        // The 3DS's GPU keeps going to 4×.
        assert_eq!(
            matched_resolution([((1636, 982), three_ds_top)], three_ds_cap),
            4
        );
    }

    #[test]
    fn the_3ds_textures_are_filtered_only_when_chosen() {
        assert_eq!(
            Picture::Smooth.texture_upscaling(),
            TextureUpscaling::Smooth
        );
        assert_eq!(
            Picture::RoundedPixels.texture_upscaling(),
            TextureUpscaling::PixelArt
        );
        for off in [
            Picture::Auto,
            Picture::SharpPixels,
            Picture::LcdGrid,
            Picture::Scanlines,
        ] {
            assert_eq!(off.texture_upscaling(), TextureUpscaling::Off, "{off:?}");
        }
    }

    #[test]
    fn match_display_steps_3d_down_to_keep_the_speed_asked_for() {
        let normal = 59.83;
        // 3× fast-forward at 2× speed: a step down, and so on each second
        // it stays behind.
        assert_eq!(keep_speed(4, 4, 119.0, Some(179.5), normal), 3);
        assert_eq!(keep_speed(3, 3, 140.0, Some(179.5), normal), 2);
        // Keeping up, or nearly: no change.
        assert_eq!(keep_speed(4, 4, 59.8, Some(normal), normal), 4);
        assert_eq!(keep_speed(2, 2, 172.0, Some(179.5), normal), 2);
        // Paused or loading: not behind, just stopped.
        assert_eq!(keep_speed(4, 4, 0.0, Some(normal), normal), 4);
        assert_eq!(keep_speed(4, 4, 10.0, Some(179.5), normal), 4);
        // Slow motion at a quarter: 15 frames a second is keeping up.
        assert_eq!(keep_speed(4, 4, 15.0, Some(normal / 4.0), normal), 4);
        // Unlimited wants all it can get, down to 1×, and no lower.
        assert_eq!(keep_speed(4, 2, 400.0, None, normal), 1);
        assert_eq!(keep_speed(1, 1, 400.0, None, normal), 1);
        // A ceiling already below the scale drawn stays.
        assert_eq!(keep_speed(2, 4, 100.0, Some(179.5), normal), 2);
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
