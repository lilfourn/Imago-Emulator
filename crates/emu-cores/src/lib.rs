//! Picks and constructs the right core for a game.
//!
//! This is the only crate that knows every system crate, so frontends
//! depend on it instead of on `emu-gb`, `emu-gba` and `emu-nds` directly.

use std::path::Path;

use emu_core::{Core, CoreInfo, LoadError, Media, System};

/// File extensions this build recognises, and the system each belongs to.
pub const ROM_EXTENSIONS: [(&str, System); 5] = [
    ("gb", System::Gb),
    ("gbc", System::Gb),
    ("gba", System::Gba),
    ("nds", System::Nds),
    ("srl", System::Nds),
];

/// The system a ROM file is for, judged by its extension.
#[must_use]
pub fn system_for_path(path: &Path) -> Option<System> {
    let extension = path.extension()?.to_str()?;
    ROM_EXTENSIONS
        .into_iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map(|(_, system)| system)
}

/// Static facts about `system`'s core.
#[must_use]
pub const fn info(system: System) -> CoreInfo {
    match system {
        System::Gb => emu_gb::INFO,
        System::Gba => emu_gba::INFO,
        System::Nds => emu_nds::INFO,
    }
}

/// Powers on `system` with `media`.
///
/// # Errors
///
/// Returns the core's [`LoadError`] if it rejects the ROM.
pub fn create(system: System, media: &Media<'_>) -> Result<Box<dyn Core>, LoadError> {
    Ok(match system {
        System::Gb => Box::new(emu_gb::Gb::new(media)?),
        System::Gba => Box::new(emu_gba::Gba::new(media)?),
        System::Nds => Box::new(emu_nds::Nds::new(media)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_systems_by_extension() {
        assert_eq!(system_for_path(Path::new("red.GB")), Some(System::Gb));
        assert_eq!(system_for_path(Path::new("crystal.gbc")), Some(System::Gb));
        assert_eq!(system_for_path(Path::new("emerald.gba")), Some(System::Gba));
        assert_eq!(
            system_for_path(Path::new("platinum.nds")),
            Some(System::Nds)
        );
        assert_eq!(system_for_path(Path::new("notes.txt")), None);
        assert_eq!(system_for_path(Path::new("no_extension")), None);
    }

    #[test]
    fn created_cores_report_their_own_info() {
        let mut rom = vec![0; 0xC0];
        rom[0xB2] = 0x96;
        let core = create(System::Gba, &Media::new(&rom)).expect("valid gba header");
        assert_eq!(core.info(), info(System::Gba));
    }
}
