use std::fmt;
use std::str::FromStr;

/// One physical button on a GB, GBA, DS or 3DS.
///
/// The discriminants follow the GBA `KEYINPUT` register (bits 0 to 9), with
/// the DS's X and Y after them, so the GBA core can build `KEYINPUT`
/// straight from [`Buttons::bits`], then the 3DS's ZL and ZR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Button {
    /// A.
    A = 0,
    /// B.
    B = 1,
    /// Select.
    Select = 2,
    /// Start.
    Start = 3,
    /// D-pad right.
    Right = 4,
    /// D-pad left.
    Left = 5,
    /// D-pad up.
    Up = 6,
    /// D-pad down.
    Down = 7,
    /// Right shoulder (GBA, DS).
    R = 8,
    /// Left shoulder (GBA, DS).
    L = 9,
    /// X (DS).
    X = 10,
    /// Y (DS).
    Y = 11,
    /// Left trigger (New 3DS).
    Zl = 12,
    /// Right trigger (New 3DS).
    Zr = 13,
}

impl Button {
    /// Every button, in bit order.
    pub const ALL: [Self; 14] = [
        Self::A,
        Self::B,
        Self::Select,
        Self::Start,
        Self::Right,
        Self::Left,
        Self::Up,
        Self::Down,
        Self::R,
        Self::L,
        Self::X,
        Self::Y,
        Self::Zl,
        Self::Zr,
    ];

    /// The lowercase name used in binding files and logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::Select => "select",
            Self::Start => "start",
            Self::Right => "right",
            Self::Left => "left",
            Self::Up => "up",
            Self::Down => "down",
            Self::R => "r",
            Self::L => "l",
            Self::X => "x",
            Self::Y => "y",
            Self::Zl => "zl",
            Self::Zr => "zr",
        }
    }

    const fn mask(self) -> u16 {
        1 << self as u16
    }
}

impl fmt::Display for Button {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A string that names no [`Button`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown button {0:?}")]
pub struct ParseButtonError(String);

impl FromStr for Button {
    type Err = ParseButtonError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|button| button.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| ParseButtonError(s.to_owned()))
    }
}

/// The set of buttons held during a frame. Bit `n` is set when the button
/// whose discriminant is `n` is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Buttons(u16);

impl Buttons {
    /// No buttons held.
    pub const NONE: Self = Self(0);

    /// The raw bitmask.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Whether `button` is held.
    #[must_use]
    pub const fn contains(self, button: Button) -> bool {
        self.0 & button.mask() != 0
    }

    /// Whether no button is held.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Marks `button` as held or released.
    pub fn set(&mut self, button: Button, held: bool) {
        if held {
            self.0 |= button.mask();
        } else {
            self.0 &= !button.mask();
        }
    }
}

impl FromIterator<Button> for Buttons {
    fn from_iter<I: IntoIterator<Item = Button>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .fold(0, |bits, button| bits | button.mask()),
        )
    }
}

/// A touchscreen contact in bottom-screen pixels (DS and 3DS), with the
/// origin at the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TouchPoint {
    /// Column: `0..256` on a DS, `0..320` on a 3DS.
    pub x: u16,
    /// Row: `0..192` on a DS, `0..240` on a 3DS.
    pub y: u16,
}

/// Everything the player does during one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Input {
    /// Held buttons. Cores ignore buttons their system lacks.
    pub buttons: Buttons,
    /// Touchscreen contact, if the screen is being touched.
    pub touch: Option<TouchPoint>,
    /// How loud the microphone hears the player during the frame (DS
    /// only), from 0 for silence to [`Input::LOUDEST`] for as loud as it
    /// reads. The core turns it into noise of that loudness, which is what
    /// games listening for blowing or shouting look for; no voice comes
    /// through.
    pub microphone: u8,
}

impl Input {
    /// The loudest [`Input::microphone`] level: blowing straight into the
    /// microphone.
    pub const LOUDEST: u8 = u8::MAX;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_bits_follow_gba_keyinput_order() {
        let held: Buttons = [Button::A, Button::L].into_iter().collect();
        assert_eq!(held.bits(), 0b10_0000_0001);
    }

    #[test]
    fn set_and_clear_round_trip() {
        let mut held = Buttons::NONE;
        held.set(Button::Start, true);
        assert!(held.contains(Button::Start));
        held.set(Button::Start, false);
        assert!(held.is_empty());
    }

    #[test]
    fn names_parse_back_case_insensitively() {
        for button in Button::ALL {
            assert_eq!(button.name().to_uppercase().parse::<Button>(), Ok(button));
        }
        assert!("turbo".parse::<Button>().is_err());
    }
}
