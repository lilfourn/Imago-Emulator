use std::fmt;
use std::str::FromStr;

/// An emulated system. Game Boy Color runs on the [`System::Gb`] core in
/// CGB mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum System {
    /// Game Boy and Game Boy Color.
    Gb = 1,
    /// Game Boy Advance.
    Gba = 2,
    /// Nintendo DS.
    Nds = 3,
    /// An RPG Maker game, which mkxp-z runs in a process of its own.
    RpgMaker = 4,
    /// Nintendo 3DS, which Azahar's libretro core runs in a helper process.
    N3ds = 5,
}

impl System {
    /// Every system.
    pub const ALL: [Self; 5] = [Self::Gb, Self::Gba, Self::Nds, Self::RpgMaker, Self::N3ds];

    /// Short identifier used on the command line, in logs and in file
    /// names: `gb`, `gba`, `nds`, `rpgmaker` or `3ds`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Gb => "gb",
            Self::Gba => "gba",
            Self::Nds => "nds",
            Self::RpgMaker => "rpgmaker",
            Self::N3ds => "3ds",
        }
    }

    /// Human-readable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gb => "Game Boy / Game Boy Color",
            Self::Gba => "Game Boy Advance",
            Self::Nds => "Nintendo DS",
            Self::RpgMaker => "RPG Maker",
            Self::N3ds => "Nintendo 3DS",
        }
    }

    /// Stable one-byte tag for binary formats such as save states.
    #[must_use]
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// The system whose [`System::tag`] is `tag`.
    #[must_use]
    pub fn from_tag(tag: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|system| system.tag() == tag)
    }
}

impl fmt::Display for System {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// A string that is not a [`System::id`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown system {0:?}; expected gb, gba, nds, rpgmaker or 3ds")]
pub struct ParseSystemError(String);

impl FromStr for System {
    type Err = ParseSystemError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|system| system.id().eq_ignore_ascii_case(s))
            .ok_or_else(|| ParseSystemError(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_tags_round_trip() {
        for system in System::ALL {
            assert_eq!(system.id().parse(), Ok(system));
            assert_eq!(System::from_tag(system.tag()), Some(system));
        }
        assert_eq!(System::from_tag(0), None);
    }
}
