//! Cheat devices: the code formats a `[name]` line can name with a tag.

use std::fmt;

use emu_core::System;

/// A code format, named after the cheat device that read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Device {
    /// `ADDRESS:VALUE` memory patches, on every system. Tag `raw`.
    Raw,
    /// Game Boy GameShark, `TTVVAAAA`. Tag `gs`.
    GameShark,
    /// Game Boy Game Genie, `ABC-DEF` or `ABC-DEF-GHI`: ROM patches. Tag
    /// `gg`.
    GameGenie,
    /// GBA CodeBreaker (Xploder), `XXXXXXXX YYYY`. Tag `cb`.
    CodeBreaker,
    /// GBA GameShark Advance or Action Replay v1/v2, `XXXXXXXX YYYYYYYY`,
    /// encrypted. Tag `gsa`.
    GameSharkAdvance,
    /// [`Device::GameSharkAdvance`] codes written out unencrypted. Tag
    /// `gsa-raw`.
    GameSharkAdvanceRaw,
    /// GBA Pro Action Replay v3 (Action Replay MAX), `XXXXXXXX YYYYYYYY`,
    /// encrypted. Tag `par3`.
    ProActionReplay,
    /// [`Device::ProActionReplay`] codes written out unencrypted. Tag
    /// `par3-raw`.
    ProActionReplayRaw,
    /// Action Replay DS, `XXXXXXXX YYYYYYYY`. Tag `ards`.
    ActionReplayDs,
}

impl Device {
    /// Every device.
    pub const ALL: [Self; 9] = [
        Self::Raw,
        Self::GameShark,
        Self::GameGenie,
        Self::CodeBreaker,
        Self::GameSharkAdvance,
        Self::GameSharkAdvanceRaw,
        Self::ProActionReplay,
        Self::ProActionReplayRaw,
        Self::ActionReplayDs,
    ];

    /// The tag that names this device after a `[name]`.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::GameShark => "gs",
            Self::GameGenie => "gg",
            Self::CodeBreaker => "cb",
            Self::GameSharkAdvance => "gsa",
            Self::GameSharkAdvanceRaw => "gsa-raw",
            Self::ProActionReplay => "par3",
            Self::ProActionReplayRaw => "par3-raw",
            Self::ActionReplayDs => "ards",
        }
    }

    /// The device whose tag is `tag`, ignoring case.
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|device| device.tag().eq_ignore_ascii_case(tag))
    }

    /// Human-readable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Raw => "raw patch",
            Self::GameShark => "GameShark",
            Self::GameGenie => "Game Genie",
            Self::CodeBreaker => "CodeBreaker",
            Self::GameSharkAdvance => "GameShark Advance",
            Self::GameSharkAdvanceRaw => "GameShark Advance (unencrypted)",
            Self::ProActionReplay => "Pro Action Replay v3",
            Self::ProActionReplayRaw => "Pro Action Replay v3 (unencrypted)",
            Self::ActionReplayDs => "Action Replay DS",
        }
    }

    /// The system this device plugs into, or `None` for every system.
    #[must_use]
    pub const fn system(self) -> Option<System> {
        match self {
            Self::Raw => None,
            Self::GameShark | Self::GameGenie => Some(System::Gb),
            Self::CodeBreaker
            | Self::GameSharkAdvance
            | Self::GameSharkAdvanceRaw
            | Self::ProActionReplay
            | Self::ProActionReplayRaw => Some(System::Gba),
            Self::ActionReplayDs => Some(System::Nds),
        }
    }

    /// Hex digits in one code, or `None` where codes are not cut by
    /// length: raw patches, and Game Genie codes of 6 or 9 digits.
    pub(crate) const fn digits(self) -> Option<usize> {
        match self {
            Self::Raw | Self::GameGenie => None,
            Self::GameShark => Some(8),
            Self::CodeBreaker => Some(12),
            Self::GameSharkAdvance
            | Self::GameSharkAdvanceRaw
            | Self::ProActionReplay
            | Self::ProActionReplayRaw
            | Self::ActionReplayDs => Some(16),
        }
    }

    /// The tags, comma-separated, for messages.
    pub(crate) fn tags() -> String {
        Self::ALL.map(Self::tag).join(", ")
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_round_trip_and_ignore_case() {
        for device in Device::ALL {
            assert_eq!(Device::from_tag(device.tag()), Some(device));
        }
        assert_eq!(Device::from_tag("ARDS"), Some(Device::ActionReplayDs));
        assert_eq!(Device::from_tag("xyz"), None);
    }
}
