//! What a save-state slot holds, for showing it before loading.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Save-state slots, numbered from 1.
pub const STATE_SLOTS: u8 = 4;

/// A slot's screenshot and when it was saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotPreview {
    /// Width, height and RGB bytes of the screenshot, if the state has one.
    pub thumbnail: Option<(u16, u16, Vec<u8>)>,
    /// When the slot file was last written.
    pub saved: SystemTime,
}

/// Reads the save state at `path`; `None` if there is none or it is
/// unreadable.
pub fn preview(path: &Path) -> Option<SlotPreview> {
    let saved = fs::metadata(path).ok()?.modified().ok()?;
    let bytes = fs::read(path).ok()?;
    let decoded = emu_state::decode(&bytes).ok()?;
    Some(SlotPreview {
        thumbnail: decoded
            .thumbnail
            .map(|t| (t.width, t.height, t.rgb.to_vec())),
        saved,
    })
}

/// "just now", "5 min ago", "3 h ago" or "2 days ago".
pub fn describe_age(age: Duration) -> String {
    let minutes = age.as_secs() / 60;
    match minutes {
        0 => "just now".to_owned(),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_naturally() {
        assert_eq!(describe_age(Duration::from_secs(30)), "just now");
        assert_eq!(describe_age(Duration::from_secs(5 * 60)), "5 min ago");
        assert_eq!(describe_age(Duration::from_secs(3 * 3600 + 10)), "3 h ago");
        assert_eq!(describe_age(Duration::from_secs(49 * 3600)), "2 days ago");
    }

    #[test]
    fn empty_slots_have_no_preview() {
        assert_eq!(preview(Path::new("/nonexistent/game.ss1")), None);
    }
}
