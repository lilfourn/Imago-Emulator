//! The games the user has added or played, kept between launches as TOML
//! next to the settings file:
//!
//! ```toml
//! [[game]]
//! path = "/Users/me/ROMs/Minish Cap.gba"
//! platform = "gba"           # "gb", "gbc", "gba", "nds", "3ds" or "rpgmaker"
//! crc32 = "ABCEBBB1"
//! serial = "BZME"            # GBA, DS and 3DS games only
//! title = "The Legend of Zelda: The Minish Cap"
//! name = "Legend of Zelda, The - The Minish Cap (USA)"  # once looked up
//! looked_up = true           # the game database has been asked
//! play_time = 4520           # seconds played, once it has been
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    /// The ROM file, or an RPG Maker game's folder.
    pub path: PathBuf,
    /// The system it is for.
    pub platform: Platform,
    /// CRC-32 of the ROM. An RPG Maker game has none, so it is of its
    /// folder's path; a 3DS game's is of parts of its file (see
    /// `rom_info::read_3ds`).
    pub crc32: u32,
    /// A GBA or DS game's four-letter game code, or a 3DS game's product
    /// code.
    pub serial: Option<String>,
    /// The title shown under its box art.
    pub title: String,
    /// The game's No-Intro name, once found in the game database.
    pub name: Option<String>,
    /// Whether the game database has been asked about the game, found or
    /// not, so it is asked once.
    pub looked_up: bool,
    /// How long it has been played, counted since play time was first
    /// kept (see [`PlayClock`]).
    pub play_time: Duration,
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
            play_time: Duration::ZERO,
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

    /// The RPG Maker game in `folder`, titled `title` (from its
    /// `Game.ini`), or else with the folder's name. No game database lists
    /// it, so it is not looked up.
    pub fn rpg_maker(folder: &Path, title: Option<String>) -> Self {
        let name = || {
            folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        Self {
            path: folder.to_owned(),
            platform: Platform::RpgMaker,
            crc32: crc32fast::hash(folder.as_os_str().as_encoded_bytes()),
            serial: None,
            title: title.unwrap_or_else(name),
            name: None,
            looked_up: true,
            play_time: Duration::ZERO,
        }
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
    /// keeps what the database said of it while the ROM is unchanged, and
    /// how long it has been played.
    pub fn add(&mut self, mut game: Game) {
        match self.games.iter_mut().find(|known| known.path == game.path) {
            Some(known) => {
                if known.crc32 == game.crc32 && known.looked_up {
                    game.title.clone_from(&known.title);
                    game.name.clone_from(&known.name);
                    game.looked_up = true;
                }
                game.play_time = game.play_time.max(known.play_time);
                *known = game;
            }
            None => self.games.push(game),
        }
    }

    /// Whether the game at `path` is in the library.
    pub fn contains(&self, path: &Path) -> bool {
        self.games.iter().any(|game| game.path == path)
    }

    /// Takes the game at `path` out of the library: what it was and where,
    /// to put it back with [`Library::restore`].
    pub fn remove(&mut self, path: &Path) -> Option<(usize, Game)> {
        let at = self.games.iter().position(|game| game.path == path)?;
        Some((at, self.games.remove(at)))
    }

    /// Puts back a game taken out `at` that place, unless it has been
    /// added again since.
    pub fn restore(&mut self, at: usize, game: Game) {
        if !self.contains(&game.path) {
            self.games.insert(at.min(self.games.len()), game);
        }
    }

    /// Adds `time` to how long the game at `path` has been played. Whether
    /// the library has it.
    pub fn add_play_time(&mut self, path: &Path, time: Duration) -> bool {
        let Some(game) = self.games.iter_mut().find(|game| game.path == path) else {
            return false;
        };
        game.play_time += time;
        true
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
                    play_time: Duration::from_secs(entry.play_time),
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
                    play_time: game.play_time.as_secs(),
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

    /// The path of every game, as `known` for [`Library::save_keeping`].
    pub fn paths(&self) -> HashSet<PathBuf> {
        self.games.iter().map(|game| game.path.clone()).collect()
    }

    /// Writes the library to `path` as [`Library::save`] does, but first
    /// reads the file and takes in the games another copy of the app added
    /// to it: those it holds that this library has not, and that were not
    /// in the file when this copy last read or wrote it (`known`, which is
    /// brought up to date). A game this copy took out stays out. Returns
    /// how many games were taken in.
    ///
    /// Each game's other facts are still as the last copy to write has
    /// them, and two copies writing at the same moment can still miss each
    /// other's game until one writes again.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the folder or file cannot be written.
    pub fn save_keeping(
        &mut self,
        path: &Path,
        known: &mut HashSet<PathBuf>,
    ) -> std::io::Result<usize> {
        let mut added = 0;
        // A file that cannot be read now is replaced, as it always was.
        if let Ok(disk) = Self::load(path) {
            for game in disk.games {
                if !known.contains(&game.path) && !self.contains(&game.path) {
                    self.games.push(game);
                    added += 1;
                }
            }
        }
        self.save(path)?;
        *known = self.paths();
        Ok(added)
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
    /// Seconds played.
    #[serde(default, skip_serializing_if = "is_zero")]
    play_time: u64,
}

/// Whether no time was played: serde passes it by reference.
fn is_zero(seconds: &u64) -> bool {
    *seconds == 0
}

/// Counts how long a game is played: the time between the frames it
/// shows, so a paused game, or one under the game menu, which shows none,
/// adds no time while it waits.
#[derive(Debug, Clone, Default)]
pub struct PlayClock {
    last_frame: Option<Instant>,
    played: Duration,
}

impl PlayClock {
    /// The longest gap between two frames that counts as play: longer
    /// gaps are pauses, of which only this much counts.
    const MOST: Duration = Duration::from_millis(500);

    /// Counts a frame shown `now`.
    pub fn frame(&mut self, now: Instant) {
        if let Some(last) = self.last_frame {
            self.played += now.saturating_duration_since(last).min(Self::MOST);
        }
        self.last_frame = Some(now);
    }

    /// The time played since last taken, which starts the count again.
    pub fn take(&mut self) -> Duration {
        self.last_frame = None;
        std::mem::take(&mut self.played)
    }
}

/// Where `game` was left off: the save state of its checkpoint, which it
/// picks up from when it next starts, next to its ROM or folder. Its
/// time is when the game was last played, and the picture beside it
/// ([`persist::state_picture_path`]) shows where.
pub fn checkpoint_path(game: &Game) -> PathBuf {
    let files = if game.platform == Platform::RpgMaker {
        persist::GameFiles::of_folder(&game.path)
    } else {
        persist::GameFiles::of(&game.path)
    };
    files.state(crate::emulator::CHECKPOINT, false)
}

/// Where the library file lives, next to the settings, e.g.
/// `~/Library/Application Support/ds-emulator/library.toml` on macOS.
pub fn default_path() -> Option<PathBuf> {
    Some(
        emu_os::config_dir()?
            .join("ds-emulator")
            .join("library.toml"),
    )
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
    /// A game's platform is not one of the five names.
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
    fn saving_keeps_the_games_another_copy_of_the_app_added() {
        let dir = std::env::temp_dir().join(format!("emu-app-library-two-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("library.toml");
        let game = |name: &str| Game::new(&Path::new("/roms").join(name), &info(Platform::Gba, 1));
        let titles = |library: &Library| -> Vec<String> {
            let games = library.games.iter();
            games.map(|game| game.title.clone()).collect()
        };
        // Two copies of the app start with the same library file.
        let mut first = Library::default();
        first.add(game("Ruby.gba"));
        first.save(&path).expect("saved");
        let mut second = Library::load(&path).expect("read");
        let (mut first_known, mut second_known) = (first.paths(), second.paths());

        // Each adds a game; the second to write keeps the first's.
        second.add(game("Sapphire.gba"));
        assert_eq!(
            second
                .save_keeping(&path, &mut second_known)
                .expect("saved"),
            0
        );
        first.add(game("Emerald.gba"));
        assert_eq!(
            first.save_keeping(&path, &mut first_known).expect("saved"),
            1
        );
        assert_eq!(titles(&first), ["Ruby", "Emerald", "Sapphire"]);
        assert_eq!(
            titles(&Library::load(&path).expect("read")),
            ["Ruby", "Emerald", "Sapphire"]
        );
        // And the other copy takes that one in when it next saves.
        assert_eq!(
            second
                .save_keeping(&path, &mut second_known)
                .expect("saved"),
            1
        );
        assert_eq!(titles(&second), ["Ruby", "Sapphire", "Emerald"]);

        // A game this copy takes out stays out, though the file has it.
        first
            .remove(Path::new("/roms/Sapphire.gba"))
            .expect("there");
        assert_eq!(
            first.save_keeping(&path, &mut first_known).expect("saved"),
            0
        );
        assert_eq!(
            titles(&Library::load(&path).expect("read")),
            ["Ruby", "Emerald"]
        );
        // With no file yet, it is written as it is.
        let fresh = dir.join("new/library.toml");
        assert_eq!(
            first
                .save_keeping(&fresh, &mut HashSet::new())
                .expect("saved"),
            0
        );
        assert_eq!(Library::load(&fresh), Ok(first));
        std::fs::remove_dir_all(&dir).expect("clean up");
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
    fn play_time_is_kept_in_whole_seconds_and_survives_adding_again() {
        let path = Path::new("/roms/tetris.gb");
        let mut library = Library::default();
        library.add(Game::new(path, &info(Platform::Gb, 7)));
        let unplayed = library.to_toml();
        assert!(!unplayed.contains("play_time"), "{unplayed}");
        assert!(library.add_play_time(path, Duration::from_millis(90_400)));
        assert!(!library.add_play_time(Path::new("/roms/other.gb"), Duration::from_secs(1)));
        let text = library.to_toml();
        assert!(text.contains("play_time = 90"), "{text}");
        let read = Library::from_toml(&text).expect("reads");
        assert_eq!(read.games[0].play_time, Duration::from_secs(90));
        // Added again, as by the store, it keeps its time.
        library.add(Game::new(path, &info(Platform::Gb, 7)));
        assert_eq!(library.games[0].play_time, Duration::from_millis(90_400));
    }

    #[test]
    fn a_removed_game_goes_back_where_it_was() {
        let mut library = Library::default();
        for name in ["a.gb", "b.gb", "c.gb"] {
            library.add(Game::new(Path::new(name), &info(Platform::Gb, 1)));
        }
        let (at, game) = library.remove(Path::new("b.gb")).expect("was there");
        assert_eq!(at, 1);
        assert!(library.remove(Path::new("b.gb")).is_none());
        library.restore(at, game.clone());
        let paths: Vec<&Path> = library
            .games
            .iter()
            .map(|game| game.path.as_path())
            .collect();
        assert_eq!(paths, ["a.gb", "b.gb", "c.gb"].map(Path::new));
        library.restore(at, game);
        assert_eq!(library.games.len(), 3, "a game is never in twice");
    }

    #[test]
    fn the_play_clock_counts_frames_and_not_pauses() {
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut clock = PlayClock::default();
        for frame in 0..=60 {
            clock.frame(at(frame * 1000 / 60));
        }
        // Paused for a minute, then a frame.
        clock.frame(at(61_000));
        assert_eq!(clock.take(), Duration::from_millis(1500));
        assert_eq!(clock.take(), Duration::ZERO, "taken once");
        clock.frame(at(70_000));
        assert_eq!(
            clock.take(),
            Duration::ZERO,
            "a first frame starts the count"
        );
    }

    #[test]
    fn a_games_checkpoint_sits_beside_its_rom_or_folder() {
        let rom = Game::new(Path::new("/games/Golden Sun.gba"), &info(Platform::Gba, 1));
        assert_eq!(checkpoint_path(&rom), Path::new("/games/Golden Sun.ss0"));
        let folder = Game::rpg_maker(Path::new("/games/Fan.Game"), None);
        assert_eq!(checkpoint_path(&folder), Path::new("/games/Fan.Game.ss0"));
    }

    #[test]
    fn rpg_maker_games_are_titled_from_game_ini_and_told_apart_by_folder() {
        let game = Game::rpg_maker(
            Path::new("/Games/TGOM 4.2"),
            Some("This Gym of Mine".into()),
        );
        assert_eq!(game.title, "This Gym of Mine");
        assert_eq!(game.platform, Platform::RpgMaker);
        assert!(game.looked_up, "no database lists RPG Maker games");
        let other = Game::rpg_maker(Path::new("/Games/Pokemon Uranium"), None);
        assert_eq!(other.title, "Pokemon Uranium");
        assert_ne!(game.crc32, other.crc32);
        let mut library = Library::default();
        library.add(game);
        library.add(other);
        let text = library.to_toml();
        assert!(text.contains("platform = \"rpgmaker\""), "{text}");
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
        // A whole path stays as it is.
        let whole = if cfg!(windows) {
            Path::new(r"C:\roms\a.gba")
        } else {
            Path::new("/roms/a.gba")
        };
        assert_eq!(absolute(whole), whole);
        // On Windows, one from the root without a drive is on the current
        // folder's drive.
        #[cfg(windows)]
        {
            let drive = here.components().next().expect("a drive");
            let mut on_drive = drive.as_os_str().to_owned();
            on_drive.push(r"\roms\a.gba");
            assert_eq!(absolute(Path::new("/roms/a.gba")), PathBuf::from(on_drive));
        }
    }

    #[test]
    fn a_missing_file_is_an_empty_library() {
        let path = std::env::temp_dir().join("ds-emulator-no-such-library.toml");
        assert_eq!(Library::load(&path), Ok(Library::default()));
    }
}
