//! The app's colours, taken from Imago Light so the two apps read as one
//! family: white paper, ink type, cool grey hairlines, Imago's blue for
//! what to press, and the four colour blocks of the app icon (blue, coral,
//! amber, green) for the systems' plates. Each is `0xRRGGBB`, for
//! `gpui::rgb`.
//!
//! One colour is not fixed: the accent, which a supporter may change
//! ([`Accent`], [decision 0017](../../../docs/decisions/0017-supporter-keys.md)).

use std::sync::atomic::{AtomicU32, Ordering};

/// Behind everything: the window, the library, the store and settings.
pub const BACKGROUND: u32 = 0x00FF_FFFF;
/// Raised surfaces: tiles, fields, segmented controls.
pub const PANEL: u32 = 0x00F4_F4F5;
/// Hairlines and outlines.
pub const LINE: u32 = 0x00E4_E4E7;
/// Text.
pub const TEXT: u32 = 0x0023_2327;
/// Secondary text: hints, captions, placeholders.
pub const MUTED: u32 = 0x006E_6E78;
/// The accent the app starts with: Imago's blue, darkened as Imago Light
/// darkens it so white type on it reads.
const DEFAULT_ACCENT: u32 = 0x0000_70B5;

/// The accent in use, which a supporter may change ([`Accent`]).
static ACCENT: AtomicU32 = AtomicU32::new(DEFAULT_ACCENT);

/// The one accent: what to press, such as Play, and the caret. What is
/// chosen is ink instead (see `ui.rs`).
pub fn accent() -> u32 {
    ACCENT.load(Ordering::Relaxed)
}

/// Makes `chosen` the accent from the next frame drawn.
pub fn set_accent(chosen: Accent) {
    ACCENT.store(chosen.colour(), Ordering::Relaxed);
}

/// The accents a supporter chooses between (Settings > Accent). Each is
/// dark enough for white type on it, as the blue is. None is a red or an
/// ink: red marks only what destroys something, and ink what is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accent {
    /// Imago's blue, which everyone has.
    #[default]
    Blue,
    /// A sea green.
    Teal,
    /// The green of the icon's pad, darkened.
    Green,
    /// A gold: the amber of the icon's buttons, taken toward yellow, as
    /// darkened amber is too near the red.
    Gold,
    /// A violet.
    Violet,
}

impl Accent {
    /// Every accent, in the order Settings offers them.
    pub const ALL: [Self; 5] = [
        Self::Blue,
        Self::Teal,
        Self::Green,
        Self::Gold,
        Self::Violet,
    ];

    /// What Settings calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Teal => "Teal",
            Self::Green => "Green",
            Self::Gold => "Gold",
            Self::Violet => "Violet",
        }
    }

    /// What the settings file calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Teal => "teal",
            Self::Green => "green",
            Self::Gold => "gold",
            Self::Violet => "violet",
        }
    }

    /// The accent the settings file calls `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|accent| accent.name() == name)
    }

    /// Its colour.
    pub fn colour(self) -> u32 {
        match self {
            Self::Blue => DEFAULT_ACCENT,
            Self::Teal => 0x0000_796B,
            Self::Green => 0x001E_7B34,
            Self::Gold => 0x008A_6A00,
            Self::Violet => 0x006D_4AD6,
        }
    }
}

/// Text and marks on the accent.
pub const ON_ACCENT: u32 = 0x00FF_FFFF;
/// The only red: the action that destroys something, such as taking a
/// game out of the library.
pub const DANGER: u32 = 0x00C8_3314;
/// Around the screens while a game runs: Imago Dark's ink, so the games'
/// own colours and the ambient glow show as they would on a dark screen.
pub const STAGE: u32 = 0x000B_0B0D;

/// `colour` with `alpha`, for `gpui::rgba`.
pub const fn with_alpha(colour: u32, alpha: u8) -> u32 {
    colour << 8 | alpha as u32
}

/// Room between the window's sides and what is in it, in logical pixels:
/// as far in as the close button.
pub const INSET: f32 = 16.0;
/// Behind a control under the pointer.
pub const HOVER: u32 = with_alpha(TEXT, 0x0F);
/// Behind the chosen game, in the library and the store.
pub const CHOSEN: u32 = with_alpha(TEXT, 0x14);

/// The plate of a game without a picture, from the top left to the
/// bottom right, in its system's colour block: a Game Boy's green, a Game
/// Boy Color's amber, a Game Boy Advance's blue, a DS's coral, and the
/// deeper coral and amber for the 3DS and RPG Maker games. Each starts a
/// little darker than its block and ends darker still, enough for white
/// type.
pub fn tint(platform: crate::rom_info::Platform) -> (u32, u32) {
    match platform.tag() {
        "gb" => (0x004F_AE4C, 0x002A_6B24),
        "gbc" => (0x00E0_8506, 0x008F_5100),
        "gba" => (0x001F_9FE8, 0x0000_5D9A),
        "nds" => (0x00EC_4A2E, 0x00A8_290F),
        "3ds" => (0x00C8_3314, 0x007A_1D09),
        "rpgmaker" => (0x00A4_5E00, 0x005C_3400),
        _ => (MUTED, TEXT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How bright `colour` is, from 0 to 1, as WCAG 2 counts it.
    fn luminance(colour: u32) -> f64 {
        let channel = |shift: u32| {
            let value = f64::from((colour >> shift) & 0xFF) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    /// WCAG 2's contrast between two colours, from 1 to 21.
    fn contrast(a: u32, b: u32) -> f64 {
        let (light, dark) = (
            luminance(a).max(luminance(b)),
            luminance(a).min(luminance(b)),
        );
        (light + 0.05) / (dark + 0.05)
    }

    #[test]
    fn white_type_reads_on_every_accent() {
        for accent in Accent::ALL {
            let ratio = contrast(accent.colour(), ON_ACCENT);
            assert!(ratio >= 4.5, "{}: {ratio:.2} to 1", accent.label());
        }
    }

    #[test]
    fn no_accent_is_mistaken_for_the_red_or_another_accent() {
        let distance = |a: u32, b: u32| {
            [16, 8, 0]
                .into_iter()
                .map(|shift| ((a >> shift) & 0xFF).abs_diff((b >> shift) & 0xFF))
                .sum::<u32>()
        };
        for (at, accent) in Accent::ALL.into_iter().enumerate() {
            assert!(
                distance(accent.colour(), DANGER) > 120,
                "{} beside the red",
                accent.label()
            );
            for other in &Accent::ALL[at + 1..] {
                assert!(
                    distance(accent.colour(), other.colour()) > 60,
                    "{} beside {}",
                    accent.label(),
                    other.label()
                );
            }
        }
    }

    #[test]
    fn accents_keep_the_names_settings_files_have() {
        for (accent, name) in [
            (Accent::Blue, "blue"),
            (Accent::Teal, "teal"),
            (Accent::Green, "green"),
            (Accent::Gold, "gold"),
            (Accent::Violet, "violet"),
        ] {
            assert_eq!(accent.name(), name);
            assert_eq!(Accent::from_name(name), Some(accent));
        }
        assert_eq!(Accent::from_name("Blue"), None);
    }
}
