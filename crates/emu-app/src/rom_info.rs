//! What a ROM's header says about the game, for the library: its system,
//! title and game code, and a DS game's icon and banner title.

use std::path::Path;

/// The system a game is for, as the library groups them. Game Boy Color
/// games are told apart from Game Boy ones, which the cores need not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Game Boy.
    Gb,
    /// Game Boy Color, including games that also run on a Game Boy.
    Gbc,
    /// Game Boy Advance.
    Gba,
    /// Nintendo DS.
    Nds,
    /// An RPG Maker game, such as a Pokémon fan game, which mkxp-z runs.
    /// It is a folder, not a ROM.
    RpgMaker,
}

/// Each platform and its name in the library file.
const TAGS: [(Platform, &str); 5] = [
    (Platform::Gb, "gb"),
    (Platform::Gbc, "gbc"),
    (Platform::Gba, "gba"),
    (Platform::Nds, "nds"),
    (Platform::RpgMaker, "rpgmaker"),
];

impl Platform {
    /// The platform's name in the library file.
    pub fn tag(self) -> &'static str {
        TAGS.iter()
            .find(|(platform, _)| *platform == self)
            .map_or("gb", |(_, tag)| tag)
    }

    /// The platform named `tag` in the library file.
    pub fn from_tag(tag: &str) -> Option<Self> {
        TAGS.iter()
            .find(|(_, name)| *name == tag)
            .map(|(platform, _)| *platform)
    }

    /// The platform's short name, as the library's filter shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Gb => "GB",
            Self::Gbc => "GBC",
            Self::Gba => "GBA",
            Self::Nds => "DS",
            Self::RpgMaker => "RPG",
        }
    }

    /// The system's name in libretro's databases and thumbnails.
    ///
    /// # Panics
    ///
    /// For RPG Maker games, which libretro does not list, so that
    /// `boxart::databases_for` gives no database to look them up in.
    pub fn libretro_name(self) -> &'static str {
        match self {
            Self::Gb => "Nintendo - Game Boy",
            Self::Gbc => "Nintendo - Game Boy Color",
            Self::Gba => "Nintendo - Game Boy Advance",
            Self::Nds => "Nintendo - Nintendo DS",
            Self::RpgMaker => unreachable!("libretro lists no RPG Maker games"),
        }
    }
}

/// What a ROM's header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomInfo {
    /// The handheld the game is for.
    pub platform: Platform,
    /// CRC-32 of the whole file, as No-Intro lists it.
    pub crc32: u32,
    /// The four-letter game code of GBA and DS games, such as `BZME`.
    pub serial: Option<String>,
    /// The title in the header: capitals, at most 16 letters.
    pub header_title: Option<String>,
    /// A DS game's English title from its banner, first line only.
    pub banner_title: Option<String>,
}

/// Reads `rom`, the bytes of the file at `path`, whose extension says
/// which system it is for.
pub fn read(path: &Path, rom: &[u8]) -> Option<RomInfo> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let platform = match extension.as_str() {
        "gb" | "gbc" | "cgb" => {
            // Bit 7 of the CGB flag: the game uses Game Boy Color features.
            if rom.get(0x143).is_some_and(|flag| flag & 0x80 != 0) {
                Platform::Gbc
            } else {
                Platform::Gb
            }
        }
        "gba" | "agb" => Platform::Gba,
        "nds" | "srl" => Platform::Nds,
        _ => return None,
    };
    let (title, serial) = match platform {
        // The colour games shortened the title to make room for a flag.
        Platform::Gb => (ascii(rom, 0x134..0x144), None),
        Platform::Gbc => (ascii(rom, 0x134..0x143), None),
        Platform::Gba => (ascii(rom, 0xA0..0xAC), ascii(rom, 0xAC..0xB0)),
        Platform::Nds => (ascii(rom, 0x00..0x0C), ascii(rom, 0x0C..0x10)),
        Platform::RpgMaker => return None,
    };
    Some(RomInfo {
        platform,
        crc32: crc32fast::hash(rom),
        serial: serial.filter(|serial| serial.len() == 4),
        header_title: title,
        banner_title: (platform == Platform::Nds)
            .then(|| banner_title(rom))
            .flatten(),
    })
}

/// The printable ASCII in `rom[range]`, up to the first NUL, trimmed.
fn ascii(rom: &[u8], range: std::ops::Range<usize>) -> Option<String> {
    let bytes = rom.get(range)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let text: String = bytes[..end]
        .iter()
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|&b| char::from(b))
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Where a DS ROM's banner starts, if it has one.
fn banner(rom: &[u8]) -> Option<&[u8]> {
    let offset = u32::from_le_bytes(rom.get(0x68..0x6C)?.try_into().ok()?) as usize;
    // Version 1 banners, the first, are 0x840 bytes.
    (offset != 0)
        .then(|| rom.get(offset..offset + 0x840))
        .flatten()
}

/// The first line of a DS game's English banner title: `"Mario Kart DS"`
/// of `"Mario Kart DS\nNintendo"`.
fn banner_title(rom: &[u8]) -> Option<String> {
    let english = banner(rom)?.get(0x340..0x440)?;
    let units: Vec<u16> = english
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    let title = String::from_utf16_lossy(&units);
    let first = title.lines().next()?.trim();
    (!first.is_empty()).then(|| first.to_owned())
}

/// Side of a DS banner icon, in pixels.
pub const ICON_SIZE: u32 = 32;

/// A DS game's 32×32 banner icon as RGBA pixels, row by row, with colour
/// 0 transparent.
pub fn nds_icon(rom: &[u8]) -> Option<Vec<u8>> {
    let banner = banner(rom)?;
    let bitmap = banner.get(0x20..0x220)?;
    let palette: Vec<[u8; 4]> = banner
        .get(0x220..0x240)?
        .chunks_exact(2)
        .enumerate()
        .map(|(index, pair)| {
            let colour = u16::from_le_bytes([pair[0], pair[1]]);
            let channel = |shift: u16| {
                let five = ((colour >> shift) & 0x1F) as u8;
                (five << 3) | (five >> 2)
            };
            let alpha = if index == 0 { 0 } else { 0xFF };
            [channel(0), channel(5), channel(10), alpha]
        })
        .collect();
    let side = ICON_SIZE as usize;
    let mut pixels = vec![0; side * side * 4];
    // 4×4 tiles of 8×8 pixels, two to a byte, the left one in the low
    // nibble.
    for (index, byte) in bitmap.iter().enumerate() {
        let tile = index / 32;
        let within = index % 32;
        let (tile_x, tile_y) = (tile % 4, tile / 4);
        let (row, column) = (within / 4, (within % 4) * 2);
        for (half, colour) in [byte & 0x0F, byte >> 4].into_iter().enumerate() {
            let x = tile_x * 8 + column + half;
            let y = tile_y * 8 + row;
            let at = (y * side + x) * 4;
            pixels[at..at + 4].copy_from_slice(&palette[usize::from(colour)]);
        }
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gba_rom() -> Vec<u8> {
        let mut rom = vec![0; 0x200];
        rom[0xA0..0xAC].copy_from_slice(b"ZELDA MINISH");
        rom[0xAC..0xB0].copy_from_slice(b"BZME");
        rom
    }

    /// A DS ROM whose banner, at 0x200, says `title` and draws its icon in
    /// colour 1, red, except the top-left pixel.
    fn nds_rom(title: &str) -> Vec<u8> {
        let mut rom = vec![0; 0x200 + 0x840];
        rom[0..12].copy_from_slice(b"MARIOKARTDS\0");
        rom[0x0C..0x10].copy_from_slice(b"AMCE");
        rom[0x68..0x6C].copy_from_slice(&0x200_u32.to_le_bytes());
        let banner = &mut rom[0x200..];
        banner[0x20..0x220].fill(0x11);
        banner[0x20] = 0x10;
        banner[0x222..0x224].copy_from_slice(&0x001F_u16.to_le_bytes());
        for (index, unit) in title.encode_utf16().enumerate() {
            let at = 0x340 + index * 2;
            banner[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        rom
    }

    #[test]
    fn reads_a_gba_header() {
        let rom = gba_rom();
        let info = read(Path::new("Zelda.GBA"), &rom).expect("a GBA ROM");
        assert_eq!(info.platform, Platform::Gba);
        assert_eq!(info.serial.as_deref(), Some("BZME"));
        assert_eq!(info.header_title.as_deref(), Some("ZELDA MINISH"));
        assert_eq!(info.crc32, crc32fast::hash(&rom));
        assert_eq!(info.banner_title, None);
    }

    #[test]
    fn colour_games_are_told_apart_by_their_flag() {
        let mut rom = vec![0; 0x150];
        rom[0x134..0x13A].copy_from_slice(b"TETRIS");
        let read_as = |rom: &[u8]| read(Path::new("game.gb"), rom).map(|info| info.platform);
        assert_eq!(read_as(&rom), Some(Platform::Gb));
        rom[0x143] = 0x80;
        assert_eq!(read_as(&rom), Some(Platform::Gbc));
        assert_eq!(read(Path::new("notes.txt"), &rom), None);
    }

    #[test]
    fn reads_a_ds_banner_title_and_icon() {
        let rom = nds_rom("Mario Kart DS\nNintendo");
        let info = read(Path::new("mkds.nds"), &rom).expect("a DS ROM");
        assert_eq!(info.platform, Platform::Nds);
        assert_eq!(info.serial.as_deref(), Some("AMCE"));
        assert_eq!(info.header_title.as_deref(), Some("MARIOKARTDS"));
        assert_eq!(info.banner_title.as_deref(), Some("Mario Kart DS"));

        let icon = nds_icon(&rom).expect("an icon");
        assert_eq!(icon.len(), 32 * 32 * 4);
        assert_eq!(icon[0..4], [0, 0, 0, 0], "colour 0 is see-through");
        assert_eq!(icon[4..8], [0xFF, 0, 0, 0xFF]);
        assert_eq!(icon[icon.len() - 4..], [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn platforms_round_trip_through_their_tags() {
        for (platform, _) in TAGS {
            assert_eq!(Platform::from_tag(platform.tag()), Some(platform));
        }
        assert_eq!(Platform::from_tag("snes"), None);
    }
}
