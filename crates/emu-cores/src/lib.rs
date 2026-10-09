//! Picks and constructs the right core for a game.
//!
//! This is the only crate that knows every system crate, so frontends
//! depend on it instead of on `emu-gb`, `emu-gba` and `emu-nds` directly.

use std::path::Path;

use emu_core::{Core, CoreInfo, LoadError, Media, System};

/// File extensions this build recognises, and the system each belongs to.
/// `.cgb` and `.agb` are older names for Game Boy Color and Game Boy
/// Advance ROMs, the same files as `.gbc` and `.gba`.
pub const ROM_EXTENSIONS: [(&str, System); 7] = [
    ("gb", System::Gb),
    ("gbc", System::Gb),
    ("cgb", System::Gb),
    ("gba", System::Gba),
    ("agb", System::Gba),
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

/// The system whose games fit in `system`'s second cartridge slot, if it
/// has one: a DS takes Game Boy Advance games in its GBA slot.
#[must_use]
pub const fn second_slot(system: System) -> Option<System> {
    match system {
        System::Nds => Some(System::Gba),
        System::Gb | System::Gba => None,
    }
}

/// Powers on `system` with `media`, including any cartridge in
/// [`Media::second_slot`].
///
/// # Errors
///
/// Returns the core's [`LoadError`] if it rejects the ROM, or a second-slot
/// cartridge on a system without one.
pub fn create(system: System, media: &Media<'_>) -> Result<Box<dyn Core>, LoadError> {
    Ok(match system {
        System::Gb => Box::new(emu_gb::Gb::new(media)?),
        System::Gba => Box::new(emu_gba::Gba::new(media)?),
        System::Nds => Box::new(emu_nds::Nds::new(media)?),
    })
}

/// Powers on `system` with one game per entry of `media`, joined by a
/// link cable (Game Boy, Game Boy Advance) or in local wireless range of
/// each other (DS): one core with a player per game.
///
/// # Errors
///
/// Returns the core's error if it rejects a ROM or the number of games.
pub fn create_linked(system: System, media: &[Media<'_>]) -> Result<Box<dyn Core>, LoadError> {
    Ok(match system {
        System::Gb => Box::new(emu_gb::Linked::new(media)?),
        System::Gba => Box::new(emu_gba::Linked::new(media)?),
        System::Nds => Box::new(emu_nds::Linked::new(media)?),
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
        assert_eq!(system_for_path(Path::new("crystal.CGB")), Some(System::Gb));
        assert_eq!(system_for_path(Path::new("emerald.agb")), Some(System::Gba));
        assert_eq!(
            system_for_path(Path::new("platinum.nds")),
            Some(System::Nds)
        );
        assert_eq!(system_for_path(Path::new("notes.txt")), None);
        assert_eq!(system_for_path(Path::new("no_extension")), None);
    }

    #[test]
    fn only_the_ds_has_a_second_slot() {
        assert_eq!(second_slot(System::Nds), Some(System::Gba));
        assert_eq!(second_slot(System::Gba), None);
        let mut rom = vec![0; 0xC0];
        rom[0xB2] = 0x96;
        let media = Media {
            second_slot: Some(emu_core::Cartridge {
                rom: &rom,
                backup: None,
            }),
            ..Media::new(&rom)
        };
        assert!(matches!(
            create(System::Gba, &media),
            Err(LoadError::Unsupported(_))
        ));
    }

    #[test]
    fn two_ds_games_link_over_local_wireless() {
        let mut rom = vec![0; 0x400];
        for (offset, value) in [
            (0x20, 0x200u32),
            (0x24, 0x0200_4000),
            (0x30, 0x300),
            (0x34, 0x0238_0000),
        ] {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        let core = create_linked(System::Nds, &[Media::new(&rom), Media::new(&rom)])
            .expect("two DS games link");
        let info = core.info();
        assert_eq!((info.players, info.screens.len()), (2, 4));
        assert!(create_linked(System::Nds, &[Media::new(&rom)]).is_err());
    }

    #[test]
    fn created_cores_report_their_own_info() {
        let mut rom = vec![0; 0xC0];
        rom[0xB2] = 0x96;
        let core = create(System::Gba, &Media::new(&rom)).expect("valid gba header");
        assert_eq!(core.info(), info(System::Gba));
    }
}
