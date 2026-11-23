//! What a ROM's header says about the game, for the library: its system,
//! title and game code, and a DS game's icon and banner title. A 3DS
//! game's headers are read from its file, which can be 4 GB.

use std::io;
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
    /// Nintendo 3DS.
    N3ds,
}

/// Each platform and its name in the library file.
const TAGS: [(Platform, &str); 6] = [
    (Platform::Gb, "gb"),
    (Platform::Gbc, "gbc"),
    (Platform::Gba, "gba"),
    (Platform::Nds, "nds"),
    (Platform::RpgMaker, "rpgmaker"),
    (Platform::N3ds, "3ds"),
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
            Self::N3ds => "3DS",
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
            Self::N3ds => "Nintendo - Nintendo 3DS",
            Self::RpgMaker => unreachable!("libretro lists no RPG Maker games"),
        }
    }
}

/// What a ROM's header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomInfo {
    /// The handheld the game is for.
    pub platform: Platform,
    /// CRC-32 of the whole file, as No-Intro lists it; a 3DS game's is
    /// the quicker one that identifies it in save states.
    pub crc32: u32,
    /// The four-letter game code of GBA and DS games, such as `BZME`, or
    /// a 3DS game's product code, such as `CTR-P-AREE`.
    pub serial: Option<String>,
    /// The title in the header: capitals, at most 16 letters.
    pub header_title: Option<String>,
    /// A DS game's English title from its banner, first line only, or a
    /// 3DS game's from its icon.
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
        Platform::RpgMaker | Platform::N3ds => return None,
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

/// What the headers of the 3DS game at `path` say, read from the file
/// without reading all of it. Its CRC-32 is the one that identifies it in
/// save states ([`emu_3ds::header::identity`]), not the whole file's. A
/// game whose title cannot be read, as an encrypted one's cannot, is
/// titled from its file name; one the core will refuse is still listed,
/// and says why when it is opened.
///
/// # Errors
///
/// Fails if the file cannot be read.
pub fn read_3ds(path: &Path) -> io::Result<RomInfo> {
    let crc32 = emu_3ds::header::identity(path)?;
    let header = emu_3ds::header::read(path).ok();
    let header = header.as_ref();
    Ok(RomInfo {
        platform: Platform::N3ds,
        crc32,
        serial: header.and_then(|header| header.product_code.clone()),
        header_title: None,
        banner_title: header.and_then(|header| header.title.clone()),
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
    icon_in_banner(banner(rom)?)
}

/// The banner icon of the DS ROM at `path`, as [`nds_icon`] gives it,
/// reading only the header and the banner rather than the whole ROM.
pub fn nds_icon_in_file(path: &Path) -> Option<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0; 0x6C];
    file.read_exact(&mut header).ok()?;
    let offset = u32::from_le_bytes(header[0x68..0x6C].try_into().ok()?);
    if offset == 0 {
        return None;
    }
    let mut banner = vec![0; 0x840];
    file.seek(SeekFrom::Start(u64::from(offset))).ok()?;
    file.read_exact(&mut banner).ok()?;
    icon_in_banner(&banner)
}

/// The icon in a DS banner, as [`nds_icon`] gives it.
fn icon_in_banner(banner: &[u8]) -> Option<Vec<u8>> {
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
        assert_eq!(info.crc32, 0x625D_D382);
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
    fn a_ds_icon_is_read_from_the_file_without_the_rest_of_the_rom() {
        let dir = std::env::temp_dir().join(format!("emu-app-icon-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("mkds.nds");
        let rom = nds_rom("Mario Kart DS");
        std::fs::write(&path, &rom).expect("written");
        assert_eq!(nds_icon_in_file(&path), nds_icon(&rom));
        assert!(nds_icon_in_file(&path).is_some());

        // No banner, a banner past the end, and no file have no icon.
        let mut bare = rom.clone();
        bare[0x68..0x6C].fill(0);
        std::fs::write(&path, &bare).expect("written");
        assert_eq!(nds_icon_in_file(&path), None);
        bare[0x68..0x6C].copy_from_slice(&0x10_0000_u32.to_le_bytes());
        std::fs::write(&path, &bare).expect("written");
        assert_eq!(nds_icon_in_file(&path), None);
        assert_eq!(nds_icon_in_file(&dir.join("gone.nds")), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_3ds_game_without_a_title_is_named_by_its_file() {
        let dir = std::env::temp_dir().join(format!("emu-app-rom-info-3ds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("Homebrew.3dsx");
        let mut file = vec![0; 0x20];
        file[0..4].copy_from_slice(b"3DSX");
        file[4..6].copy_from_slice(&0x20_u16.to_le_bytes());
        std::fs::write(&path, &file).expect("written");

        let info = read_3ds(&path).expect("a 3DS game");
        assert_eq!(info.platform, Platform::N3ds);
        assert_eq!(info.banner_title, None, "homebrew without an icon");
        assert_eq!(info.serial, None);
        assert_eq!(read(&path, &file), None, "not read as a ROM");
        assert!(read_3ds(&dir.join("missing.3ds")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn platforms_keep_the_tags_library_files_have() {
        for (platform, tag) in [
            (Platform::Gb, "gb"),
            (Platform::Gbc, "gbc"),
            (Platform::Gba, "gba"),
            (Platform::Nds, "nds"),
            (Platform::RpgMaker, "rpgmaker"),
            (Platform::N3ds, "3ds"),
        ] {
            assert_eq!(platform.tag(), tag);
            assert_eq!(Platform::from_tag(tag), Some(platform));
        }
    }
}
