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
//! step smaller, and tries a step larger again once it has kept up a
//! while ([`SpeedKeeper`]).

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

/// Seconds in a row the game must fall behind before Match Display draws
/// 3D a step smaller: one slow second is often a loading screen, a
/// shader the 3DS compiles for the first time, or another app's burst.
const SLOW_CHECKS: u32 = 2;

/// Seconds the game must keep up before Match Display first tries a step
/// larger again.
const FIRST_PATIENCE: u32 = 10;

/// The longest Match Display waits between tries, about five minutes.
/// Each try that fails doubles the wait up to this.
const MOST_PATIENCE: u32 = 320;

/// About the most speed drawing 3D smaller buys, all the way down to 1×:
/// Pokémon Platinum ran 1.4 times as fast at 1× as at 2× (3.75 ms a frame
/// against 5.2 ms), and Omega Ruby 1.4 times as fast as at 4× (80 frames
/// a second against 57, with the GPU busy). A game fast-forwarded further
/// ahead of itself than this, such as a 3DS game at 3×, cannot catch up
/// by drawing smaller, so it keeps its picture. At normal speed or
/// slower, every step that helps counts.
const MOST_GAIN: f64 = 1.6;

/// Match Display's check that the game keeps the speed asked for, once
/// every [`SPEED_CHECK`]: the most it may draw 3D at now.
///
/// When the game falls behind by more than 5% for [`SLOW_CHECKS`] seconds
/// in a row while running at all (not paused or loading), and, when fast
/// forwarded, not so far that drawing smaller could not catch up
/// ([`MOST_GAIN`]), 3D is drawn a step smaller. Once the game has kept up for a while, it tries a step
/// larger, up to the scale that matches the screens. A try the game
/// cannot keep up at goes back at its first slow second, and doubles the
/// wait for the next, so a Mac that is busy for good (another app's
/// work on the GPU, say) settles on a scale rather than switching back
/// and forth. The view starts it afresh when the speed asked for or the
/// screens change.
///
/// The DS's 3D is the costliest thing the app does: in Pokémon Platinum's
/// overworld, headless on an M5 Max, the core took 3.75 ms a frame with 3D
/// at 1×, 5.2 ms at 2× and 7.35 ms at 4×, against 5.6 ms a frame at 3×
/// fast-forward. The 3DS draws on the GPU, so it slows when other apps
/// fill the GPU, and speeds up again when they stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedKeeper {
    /// The scale that matches the screens: the most it ever allows.
    most: u32,
    ceiling: u32,
    /// Checks in a row the game fell behind.
    slow: u32,
    /// Checks in a row it kept up.
    steady: u32,
    /// Checks it must keep up before the next try at a step larger.
    patience: u32,
    /// The last change was a try at a step larger, not yet proven.
    trying: bool,
}

impl SpeedKeeper {
    /// Starting at `most`, the scale that matches the screens.
    pub fn new(most: u32) -> Self {
        let most = most.max(1);
        Self {
            most,
            ceiling: most,
            slow: 0,
            steady: 0,
            patience: FIRST_PATIENCE,
            trying: false,
        }
    }

    /// The most Match Display may draw 3D at now.
    pub fn ceiling(&self) -> u32 {
        self.ceiling
    }

    /// Takes one second's measure: the game ran `ran` frames a second
    /// while 3D was drawn at `scale`, asked for `wanted` (`None` for
    /// unlimited speed) on a system of `normal` frames a second.
    pub fn check(&mut self, scale: u32, ran: f64, wanted: Option<f64>, normal: f64) {
        let running = ran >= 0.25 * wanted.unwrap_or(normal).min(normal);
        if !running {
            // Paused or loading: neither behind nor keeping up.
            return;
        }
        let behind = wanted.is_none_or(|wanted| ran < 0.95 * wanted);
        let hopeless =
            wanted.is_some_and(|wanted| wanted > normal * 1.05 && ran * MOST_GAIN < wanted);
        if hopeless && !self.trying {
            // Fast-forward too far ahead for the picture to be what holds
            // the game back: keep it, and try no larger either.
            self.slow = 0;
            self.steady = 0;
            return;
        }
        if behind {
            self.steady = 0;
            self.slow += 1;
            if scale > 1 && (self.trying || self.slow >= SLOW_CHECKS) {
                if self.trying {
                    self.patience = (self.patience * 2).min(MOST_PATIENCE);
                }
                self.ceiling = self.ceiling.min(scale - 1);
                self.trying = false;
                self.slow = 0;
            }
        } else {
            self.slow = 0;
            self.steady += 1;
            if self.trying && self.steady >= SLOW_CHECKS {
                // The step held: it stays until the game falls behind
                // again like any other.
                self.trying = false;
            }
            if scale == self.ceiling && self.ceiling < self.most && self.steady >= self.patience {
                self.ceiling += 1;
                self.trying = true;
                self.steady = 0;
            }
        }
    }
}

/// How near the size it is shown at a frame must be, along each side, for
/// Smooth to show it as it is: within a tenth.
const NEAR_ENOUGH: f64 = 1.1;

/// Whether Smooth shows a `frame`-sized picture `target` device pixels
/// across as it is, rather than on the GPU: when it is within a tenth of
/// that size along both sides, as a 3DS game drawn at 4× is on the
/// MacBook Pro's display (1600 pixels across for 1636). There MetalFX
/// would only resample it, at the cost of a trip to the GPU and back
/// each frame, which with other apps busy on the GPU took up to 30 ms.
/// GPUI's own filter covers the difference.
pub fn shown_as_drawn(frame: (u16, u16), target: (u32, u32)) -> bool {
    let near = |frame: u16, target: u32| {
        let ratio = f64::from(target) / f64::from(frame.max(1));
        (1.0 / NEAR_ENOUGH..=NEAR_ENOUGH).contains(&ratio)
    };
    near(frame.0, target.0) && near(frame.1, target.1)
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

    fn normal_rate() -> f64 {
        59.83
    }

    /// Runs `keeper` for `seconds` checks at whatever it allows, the game
    /// keeping up at `fast_enough` and below and running `slow` above.
    fn play(keeper: &mut SpeedKeeper, seconds: u32, fast_enough: u32, wanted: f64) {
        let normal = 59.83;
        for _ in 0..seconds {
            let scale = keeper.ceiling();
            let ran = if scale <= fast_enough {
                wanted
            } else {
                wanted * 0.8
            };
            keeper.check(scale, ran, Some(wanted), normal);
        }
    }

    #[test]
    fn match_display_steps_3d_down_to_keep_the_speed_asked_for() {
        let normal = 59.83;
        let mut keeper = SpeedKeeper::new(4);
        // 3× fast-forward at 2× speed: one slow second is let pass, two in
        // a row take a step down, and so on while it stays behind.
        keeper.check(4, 119.0, Some(179.5), normal);
        assert_eq!(keeper.ceiling(), 4);
        keeper.check(4, 119.0, Some(179.5), normal);
        assert_eq!(keeper.ceiling(), 3);
        keeper.check(3, 140.0, Some(179.5), normal);
        keeper.check(3, 140.0, Some(179.5), normal);
        assert_eq!(keeper.ceiling(), 2);
        // Keeping up, or nearly: no change.
        let mut keeper = SpeedKeeper::new(4);
        for _ in 0..5 {
            keeper.check(4, 59.8, Some(normal), normal);
            keeper.check(4, 172.0, Some(179.5), normal);
        }
        assert_eq!(keeper.ceiling(), 4);
        // Paused or loading between two slow seconds: not behind, just
        // stopped, and the slow seconds still count as in a row.
        let mut keeper = SpeedKeeper::new(4);
        keeper.check(4, 119.0, Some(179.5), normal);
        keeper.check(4, 0.0, Some(normal), normal);
        keeper.check(4, 10.0, Some(179.5), normal);
        assert_eq!(keeper.ceiling(), 4);
        keeper.check(4, 119.0, Some(179.5), normal);
        assert_eq!(keeper.ceiling(), 3);
        // A 3DS game at 3× fast-forward, at a third of it: drawing smaller
        // could not catch up, so the picture stays.
        let mut keeper = SpeedKeeper::new(4);
        for _ in 0..10 {
            keeper.check(4, 57.0, Some(179.5), normal);
        }
        assert_eq!(keeper.ceiling(), 4);
        // At normal speed a game that slow still steps down: every bit
        // of speed counts.
        let mut keeper = SpeedKeeper::new(4);
        keeper.check(4, 30.0, Some(normal), normal);
        keeper.check(4, 30.0, Some(normal), normal);
        assert_eq!(keeper.ceiling(), 3);
        // Slow motion at a quarter: 15 frames a second is keeping up.
        let mut keeper = SpeedKeeper::new(4);
        for _ in 0..5 {
            keeper.check(4, 15.0, Some(normal / 4.0), normal);
        }
        assert_eq!(keeper.ceiling(), 4);
        // Unlimited wants all it can get, down to 1×, and no lower.
        let mut keeper = SpeedKeeper::new(2);
        for _ in 0..6 {
            keeper.check(keeper.ceiling(), 400.0, None, normal);
        }
        assert_eq!(keeper.ceiling(), 1);
    }

    #[test]
    fn match_display_steps_back_up_once_the_game_keeps_up() {
        let wanted = 59.83;
        // The Mac was busy for a moment: 4× fell behind and went to 3×.
        let mut keeper = SpeedKeeper::new(4);
        play(&mut keeper, 2, 3, wanted);
        assert_eq!(keeper.ceiling(), 3);
        // Once the Mac is free again, 4× keeps up: after ten steady
        // seconds it tries 4× and keeps it.
        play(&mut keeper, 9, 4, wanted);
        assert_eq!(keeper.ceiling(), 3);
        play(&mut keeper, 1, 4, wanted);
        assert_eq!(keeper.ceiling(), 4);
        play(&mut keeper, 60, 4, wanted);
        assert_eq!(keeper.ceiling(), 4);
        // A try that leaves fast-forward hopelessly behind still goes back.
        let mut keeper = SpeedKeeper::new(4);
        keeper.check(4, 120.0, Some(179.5), normal_rate());
        keeper.check(4, 120.0, Some(179.5), normal_rate());
        assert_eq!(keeper.ceiling(), 3);
        for _ in 0..FIRST_PATIENCE {
            keeper.check(3, 179.5, Some(179.5), normal_rate());
        }
        assert_eq!(keeper.ceiling(), 4);
        keeper.check(4, 60.0, Some(179.5), normal_rate());
        assert_eq!(keeper.ceiling(), 3);
        // Never past the scale that matches the screens.
        let mut keeper = SpeedKeeper::new(2);
        play(&mut keeper, 100, 8, wanted);
        assert_eq!(keeper.ceiling(), 2);
    }

    #[test]
    fn a_failed_try_waits_twice_as_long_for_the_next() {
        let wanted = 59.83;
        // Another app keeps the GPU busy: only 2× keeps up.
        let mut keeper = SpeedKeeper::new(4);
        play(&mut keeper, 4, 2, wanted);
        assert_eq!(keeper.ceiling(), 2);
        // Ten seconds on, it tries 3× and goes back at the first slow
        // second, not two.
        play(&mut keeper, 10, 2, wanted);
        assert_eq!(keeper.ceiling(), 3);
        play(&mut keeper, 1, 2, wanted);
        assert_eq!(keeper.ceiling(), 2);
        // The next try waits 20 seconds, then 40.
        play(&mut keeper, 19, 2, wanted);
        assert_eq!(keeper.ceiling(), 2);
        play(&mut keeper, 1, 2, wanted);
        assert_eq!(keeper.ceiling(), 3);
        play(&mut keeper, 1, 2, wanted);
        play(&mut keeper, 39, 2, wanted);
        assert_eq!(keeper.ceiling(), 2);
        play(&mut keeper, 1, 2, wanted);
        assert_eq!(keeper.ceiling(), 3);
        // Tries never come further apart than about five minutes.
        let mut keeper = SpeedKeeper::new(2);
        play(&mut keeper, 2, 1, wanted);
        assert_eq!(keeper.ceiling(), 1);
        let mut waits = Vec::new();
        for _ in 0..8 {
            let mut waited = 0;
            while keeper.ceiling() == 1 {
                play(&mut keeper, 1, 1, wanted);
                waited += 1;
            }
            waits.push(waited);
            // The try falls behind at once.
            play(&mut keeper, 1, 1, wanted);
        }
        assert_eq!(waits, [10, 20, 40, 80, 160, 320, 320, 320]);
    }

    #[test]
    fn smooth_shows_a_frame_near_its_size_as_it_is() {
        // The 3DS's top screen at 4× on the MacBook Pro: 1600 for 1636.
        assert!(shown_as_drawn((1600, 960), (1636, 982)));
        assert!(shown_as_drawn((1280, 960), (1309, 982)));
        // A little smaller is as near.
        assert!(shown_as_drawn((1600, 960), (1500, 900)));
        // At 3×, 1200 for 1636 is MetalFX's to enlarge.
        assert!(!shown_as_drawn((1200, 720), (1636, 982)));
        // Both sides must be near.
        assert!(!shown_as_drawn((1600, 960), (1636, 1300)));
        assert!(!shown_as_drawn((0, 0), (1636, 982)));
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
