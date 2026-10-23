//! The games the user has added or played, kept between launches as TOML
//! next to the settings file:
//!
//! ```toml
//! [[game]]
//! path = "/Users/me/ROMs/Minish Cap.gba"
//! platform = "gba"           # "gb", "gbc", "gba" or "nds"
//! crc32 = "ABCEBBB1"
//! serial = "BZME"            # GBA and DS games only
//! title = "The Legend of Zelda: The Minish Cap"
//! name = "Legend of Zelda, The - The Minish Cap (USA)"  # once looked up
//! looked_up = true           # the game database has been asked
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::game_db;
use crate::persist;
use crate::rom_info::{Platform, RomInfo};

/// `path` as the library keeps it: absolute, from the current folder if
/// it is relative, so a game opened as `./game.gba` is the same game
/// wherever the app runs from next. Links are left as they are, as the
/// game's saves sit next to the path the user gave.
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

/// One game in the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    /// The ROM file.
    pub path: PathBuf,
    /// The handheld it is for.
    pub platform: Platform,
    /// CRC-32 of the ROM.
    pub crc32: u32,
    /// A GBA or DS game's four-letter game code.
    pub serial: Option<String>,
    /// The title shown under its box art.
    pub title: String,
    /// The game's No-Intro name, once found in the game database.
    pub name: Option<String>,
    /// Whether the game database has been asked about the game, found or
    /// not, so it is asked once.
    pub looked_up: bool,
}

impl Game {
    /// The game in the ROM at `path`, which says `info`, before it is
    /// looked up.
    pub fn new(path: &Path, info: &RomInfo) -> Self {
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().replace('_', " "))
            .unwrap_or_default();
        let title = info
            .banner_title
            .clone()
            .or_else(|| Some(game_db::display_title(&stem)).filter(|title| !title.is_empty()))
            .or_else(|| info.header_title.clone())
            .unwrap_or(stem);
        Self {
            path: path.to_owned(),
            platform: info.platform,
            crc32: info.crc32,
            serial: info.serial.clone(),
            title,
            name: None,
            looked_up: false,
        }
    }

    /// Marks the game as made from a ROM-hack patch: titled with its file
    /// name, which is the patch's, and never looked up, as the game
    /// database would name it after the game it was made from, by the
    /// game code a hack keeps.
    pub fn made_from_patch(&mut self) {
        if let Some(stem) = self.path.file_stem() {
            self.title = stem.to_string_lossy().into_owned();
        }
        self.looked_up = true;
    }

    /// Records the game database's answer: the game's No-Intro `name`, if
    /// it knows the game, which also gives its title.
    pub fn found(&mut self, name: Option<String>) {
        if let Some(name) = &name {
            self.title = game_db::display_title(name);
        }
        self.name = name;
        self.looked_up = true;
    }
}

/// The games, in the order added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    /// Each game once, by path.
    pub games: Vec<Game>,
}

impl Library {
    /// Adds `game`, or replaces the game at the same path. A replaced game
    /// keeps what the database said of it while the ROM is unchanged.
    pub fn add(&mut self, mut game: Game) {
        match self.games.iter_mut().find(|known| known.path == game.path) {
            Some(known) => {
                if known.crc32 == game.crc32 && known.looked_up {
                    game.title.clone_from(&known.title);
                    game.name.clone_from(&known.name);
                    game.looked_up = true;
                }
                *known = game;
            }
            None => self.games.push(game),
        }
    }

    /// Whether the game at `path` is in the library.
    pub fn contains(&self, path: &Path) -> bool {
        self.games.iter().any(|game| game.path == path)
    }

    /// Takes the game at `path` out of the library.
    pub fn remove(&mut self, path: &Path) {
        self.games.retain(|game| game.path != path);
    }

    /// The games for `platform` (all of them for `None`) whose titles hold
    /// `query`, ignoring case, in title order.
    pub fn find(&self, query: &str, platform: Option<Platform>) -> Vec<&Game> {
        let query = query.trim().to_lowercase();
        let mut games: Vec<&Game> = self
            .games
            .iter()
            .filter(|game| platform.is_none_or(|platform| game.platform == platform))
            .filter(|game| game.title.to_lowercase().contains(&query))
            .collect();
        games.sort_by_cached_key(|game| game.title.to_lowercase());
        games
    }

    /// The library in `text`.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] if the TOML is malformed or names a
    /// platform the app does not know.
    pub fn from_toml(text: &str) -> Result<Self, LibraryError> {
        let file: LibraryFile =
            toml::from_str(text).map_err(|error| LibraryError::Syntax(error.to_string()))?;
        let games = file
            .game
            .into_iter()
            .map(|entry| {
                Ok(Game {
                    platform: Platform::from_tag(&entry.platform)
                        .ok_or_else(|| LibraryError::UnknownPlatform(entry.platform.clone()))?,
                    crc32: u32::from_str_radix(&entry.crc32, 16)
                        .map_err(|_| LibraryError::BadChecksum(entry.crc32.clone()))?,
                    path: entry.path,
                    serial: entry.serial,
                    title: entry.title,
                    name: entry.name,
                    looked_up: entry.looked_up,
                })
            })
            .collect::<Result<_, LibraryError>>()?;
        Ok(Self { games })
    }

    /// The library as the TOML [`Library::from_toml`] reads.
    pub fn to_toml(&self) -> String {
        let file = LibraryFile {
            game: self
                .games
                .iter()
                .map(|game| GameEntry {
                    path: game.path.clone(),
                    platform: game.platform.tag().to_owned(),
                    crc32: format!("{:08X}", game.crc32),
                    serial: game.serial.clone(),
                    title: game.title.clone(),
                    name: game.name.clone(),
                    looked_up: game.looked_up,
                })
                .collect(),
        };
        let body = toml::to_string(&file).expect("plain tables and values always serialize");
        format!("# Saved by ds-emulator whenever the library changes.\n\n{body}")
    }

    /// Reads the library from `path`, or an empty one if it does not
    /// exist.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError`] if the file exists but cannot be read or
    /// parsed.
    pub fn load(path: &Path) -> Result<Self, LibraryError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(LibraryError::Io(error.to_string())),
        }
    }

    /// Writes the library to `path`, creating its folder if needed.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the folder or file cannot be written.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        persist::write_atomic(path, self.to_toml().as_bytes())
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct LibraryFile {
    game: Vec<GameEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GameEntry {
    path: PathBuf,
    platform: String,
    crc32: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    serial: Option<String>,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default)]
    looked_up: bool,
}

/// Where the library file lives, next to the settings, e.g.
/// `~/Library/Application Support/ds-emulator/library.toml` on macOS.
pub fn default_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("ds-emulator").join("library.toml"))
}

/// Why a library file was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LibraryError {
    /// The file could not be read.
    #[error("cannot read the library: {0}")]
    Io(String),
    /// The file is not valid TOML of the expected shape.
    #[error("invalid library file: {0}")]
    Syntax(String),
    /// A game's platform is not one of the four names.
    #[error("unknown platform {0:?} in the library")]
    UnknownPlatform(String),
    /// A game's checksum is not eight hex digits.
    #[error("bad checksum {0:?} in the library")]
    BadChecksum(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(platform: Platform, crc32: u32) -> RomInfo {
        RomInfo {
            platform,
            crc32,
            serial: None,
            header_title: Some("HEADER".to_owned()),
            banner_title: None,
        }
    }

    #[test]
    fn titles_come_from_the_banner_then_the_file_name() {
        let path = Path::new("/roms/Metroid_Fusion (USA).gba");
        assert_eq!(
            Game::new(path, &info(Platform::Gba, 1)).title,
            "Metroid Fusion"
        );
        let mut ds = info(Platform::Nds, 2);
        ds.banner_title = Some("Mario Kart DS".to_owned());
        assert_eq!(
            Game::new(Path::new("/roms/mkds.nds"), &ds).title,
            "Mario Kart DS"
        );
    }

    #[test]
    fn found_names_give_the_title() {
        let mut game = Game::new(Path::new("/roms/zelda.gba"), &info(Platform::Gba, 1));
        game.found(Some(
            "Legend of Zelda, The - The Minish Cap (USA)".to_owned(),
        ));
        assert_eq!(game.title, "The Legend of Zelda: The Minish Cap");
        assert!(game.looked_up);
        let mut unknown = Game::new(Path::new("/roms/hack.gba"), &info(Platform::Gba, 2));
        unknown.found(None);
        assert_eq!(unknown.title, "hack");
        assert!(unknown.looked_up);
    }

    #[test]
    fn adding_a_known_path_keeps_its_lookup_while_the_rom_is_unchanged() {
        let path = Path::new("/roms/zelda.gba");
        let mut library = Library::default();
        let mut game = Game::new(path, &info(Platform::Gba, 1));
        game.found(Some(
            "Legend of Zelda, The - The Minish Cap (USA)".to_owned(),
        ));
        library.add(game.clone());
        library.add(Game::new(path, &info(Platform::Gba, 1)));
        assert_eq!(library.games, vec![game]);
        library.add(Game::new(path, &info(Platform::Gba, 9)));
        assert_eq!(library.games.len(), 1);
        assert!(
            !library.games[0].looked_up,
            "a changed ROM is looked up again"
        );
    }

    #[test]
    fn find_filters_by_platform_and_title_in_title_order() {
        let mut library = Library::default();
        for (name, platform) in [
            ("Wario Land.gb", Platform::Gb),
            ("Metroid Fusion.gba", Platform::Gba),
            ("Advance Wars.gba", Platform::Gba),
        ] {
            library.add(Game::new(Path::new(name), &info(platform, 1)));
        }
        let titles = |games: Vec<&Game>| -> Vec<String> {
            games.into_iter().map(|game| game.title.clone()).collect()
        };
        assert_eq!(
            titles(library.find("", Some(Platform::Gba))),
            ["Advance Wars", "Metroid Fusion"]
        );
        assert_eq!(
            titles(library.find(" WAR", None)),
            ["Advance Wars", "Wario Land"]
        );
        library.remove(Path::new("Wario Land.gb"));
        assert_eq!(titles(library.find("war", None)), ["Advance Wars"]);
    }

    #[test]
    fn round_trips_through_toml() {
        let mut library = Library::default();
        let mut ds = info(Platform::Nds, 0xABCE_BBB1);
        ds.serial = Some("AMCE".to_owned());
        let mut game = Game::new(Path::new("/roms/Mario Kart DS.nds"), &ds);
        game.found(Some("Mario Kart DS (USA)".to_owned()));
        library.add(game);
        library.add(Game::new(
            Path::new("/roms/tetris.gb"),
            &info(Platform::Gb, 7),
        ));
        let text = library.to_toml();
        assert!(text.contains("crc32 = \"ABCEBBB1\""), "{text}");
        assert!(text.contains("platform = \"nds\""), "{text}");
        assert_eq!(Library::from_toml(&text), Ok(library));
    }

    #[test]
    fn rejects_unknown_platforms_and_bad_checksums() {
        let game = |platform: &str, crc: &str| {
            format!(
                "[[game]]\npath = \"a\"\nplatform = \"{platform}\"\ncrc32 = \"{crc}\"\ntitle = \"A\"\n"
            )
        };
        assert_eq!(
            Library::from_toml(&game("snes", "00")),
            Err(LibraryError::UnknownPlatform("snes".to_owned()))
        );
        assert_eq!(
            Library::from_toml(&game("gb", "xyz")),
            Err(LibraryError::BadChecksum("xyz".to_owned()))
        );
        assert!(matches!(
            Library::from_toml("game = 1"),
            Err(LibraryError::Syntax(_))
        ));
    }

    #[test]
    fn relative_paths_become_absolute_from_the_current_folder() {
        let here = std::env::current_dir().expect("a current folder");
        assert_eq!(absolute(Path::new("roms/a.gba")), here.join("roms/a.gba"));
        assert_eq!(absolute(Path::new("./a.gba")), here.join("a.gba"));
        assert_eq!(absolute(Path::new("/roms/a.gba")), Path::new("/roms/a.gba"));
    }

    #[test]
    fn a_missing_file_is_an_empty_library() {
        let path = std::env::temp_dir().join("ds-emulator-no-such-library.toml");
        assert_eq!(Library::load(&path), Ok(Library::default()));
    }
}
