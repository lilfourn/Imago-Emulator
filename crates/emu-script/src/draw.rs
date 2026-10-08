//! What scripts draw over the screens: plain data the frontend paints.

/// A colour as `0xAARRGGBB`: alpha 255 is opaque, 0 invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color(pub u32);

impl Color {
    /// Draws nothing.
    pub const NONE: Self = Self(0);
    /// Opaque white.
    pub const WHITE: Self = Self(0xFFFF_FFFF);

    const NAMES: [(&'static str, u32); 14] = [
        ("white", 0xFFFF_FFFF),
        ("black", 0xFF00_0000),
        ("red", 0xFFFF_0000),
        ("green", 0xFF00_FF00),
        ("blue", 0xFF00_00FF),
        ("yellow", 0xFFFF_FF00),
        ("cyan", 0xFF00_FFFF),
        ("magenta", 0xFFFF_00FF),
        ("orange", 0xFFFF_A500),
        ("purple", 0xFF80_0080),
        ("gray", 0xFF80_8080),
        ("grey", 0xFF80_8080),
        ("clear", 0),
        ("none", 0),
    ];

    /// How opaque it is, from 0 (invisible) to 255.
    #[must_use]
    pub const fn alpha(self) -> u8 {
        self.0.to_be_bytes()[0]
    }

    /// Whether drawing with it shows anything.
    #[must_use]
    pub const fn is_visible(self) -> bool {
        self.alpha() != 0
    }

    /// The colour as `0xRRGGBBAA`, the order CSS and GPUI use.
    #[must_use]
    pub const fn rgba(self) -> u32 {
        self.0.rotate_left(8)
    }

    /// A colour written as a number: `0xRRGGBB` is opaque, and a number
    /// above `0xFFFFFF` is `0xAARRGGBB`. `None` past 32 bits or below 0.
    #[must_use]
    pub fn from_number(number: i64) -> Option<Self> {
        let value = u32::try_from(number).ok()?;
        Some(Self(if value <= 0x00FF_FFFF {
            0xFF00_0000 | value
        } else {
            value
        }))
    }

    /// A colour written as text: a name such as `"red"` or `"clear"`, or
    /// `"#RRGGBB"` or `"#RRGGBBAA"`, case-insensitively.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(hex) = text.strip_prefix('#') {
            if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            let value = u32::from_str_radix(hex, 16).ok()?;
            return match hex.len() {
                6 => Some(Self(0xFF00_0000 | value)),
                8 => Some(Self(value.rotate_right(8))),
                _ => None,
            };
        }
        Self::NAMES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(text))
            .map(|&(_, argb)| Self(argb))
    }
}

/// One thing a script drew, in the pixels of one of the core's screens,
/// with the origin at its top-left corner. Screens are numbered as in
/// [`emu_core::Core::screens`], from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draw {
    /// A line of text whose top-left corner is at `x`, `y`, about
    /// [`Draw::TEXT_HEIGHT`] pixels tall.
    Text {
        /// Which screen.
        screen: usize,
        /// Left edge.
        x: i32,
        /// Top edge.
        y: i32,
        /// What it says.
        text: String,
        /// Its colour.
        color: Color,
    },
    /// A rectangle, filled and outlined.
    Rect {
        /// Which screen.
        screen: usize,
        /// Left edge.
        x: i32,
        /// Top edge.
        y: i32,
        /// Width in pixels, at least 1.
        width: u32,
        /// Height in pixels, at least 1.
        height: u32,
        /// Colour inside; [`Color::NONE`] for none.
        fill: Color,
        /// Colour of the one-pixel border; [`Color::NONE`] for none.
        outline: Color,
    },
}

impl Draw {
    /// How tall a line of text is, in screen pixels.
    pub const TEXT_HEIGHT: u32 = 8;

    /// The screen it is drawn on.
    #[must_use]
    pub const fn screen(&self) -> usize {
        match self {
            Self::Text { screen, .. } | Self::Rect { screen, .. } => *screen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_without_alpha_are_opaque() {
        assert_eq!(Color::from_number(0xFF0000), Some(Color(0xFFFF_0000)));
        assert_eq!(Color::from_number(0), Some(Color(0xFF00_0000)));
        assert_eq!(Color::from_number(0x80FF_0000), Some(Color(0x80FF_0000)));
        assert_eq!(Color::from_number(-1), None);
        assert_eq!(Color::from_number(0x1_0000_0000), None);
    }

    #[test]
    fn text_colours_put_alpha_last() {
        assert_eq!(Color::parse("#FF8000"), Some(Color(0xFFFF_8000)));
        assert_eq!(Color::parse("#ff800040"), Some(Color(0x40FF_8000)));
        assert_eq!(Color::parse("Red"), Some(Color(0xFFFF_0000)));
        assert_eq!(Color::parse("clear"), Some(Color::NONE));
        assert_eq!(Color::parse("#12345"), None);
        assert_eq!(Color::parse("chartreuse"), None);
    }

    #[test]
    fn rgba_moves_alpha_to_the_end() {
        assert_eq!(Color(0x80FF_8000).rgba(), 0xFF80_0080);
        assert!(!Color::NONE.is_visible());
        assert_eq!(Color::WHITE.alpha(), 255);
    }
}
