//! What the library reads from disk: the games chosen to add, each game's
//! pictures and when it was last played, and what a game's files say of
//! it for its menu.

// A part of the library's view, split off: it shares its names.
use super::*;

/// The box art and icon of `game` in `cache`.
pub(super) fn cached_pictures(cache: Option<&Path>, game: &Game) -> Pictures {
    let art = cache
        .map(|cache| boxart::art_path(cache, game.crc32))
        .filter(|art| boxart::is_png(art));
    let icon = cache
        .map(|cache| boxart::icon_path(cache, game.crc32))
        .filter(|icon| icon.exists());
    Pictures {
        art,
        icon,
        ..Pictures::default()
    }
}

/// The pictures of `game` on disk, with `cache` holding its box art and
/// icon, and when it was last played: the time of its checkpoint or of
/// the picture beside it, whichever is newer. A DS game played with a GBA
/// cartridge in keeps that session's checkpoint under another name, and
/// leaves only its picture here (see `persist::StateFiles`).
pub(super) fn pictures_on_disk(cache: Option<&Path>, game: &Game) -> Pictures {
    let Pictures { art, icon, .. } = cached_pictures(cache, game);
    let checkpoint = library::checkpoint_path(game);
    let picture = persist::state_picture_path(&checkpoint);
    let modified = |path: &Path| {
        let metadata = fs::metadata(path);
        metadata.and_then(|metadata| metadata.modified()).ok()
    };
    let played = modified(&checkpoint).max(modified(&picture));
    let left_at = played
        .and_then(|_| fs::read(&picture).ok())
        .map(|bytes| Arc::new(gpui::Image::from_bytes(ImageFormat::Png, bytes)));
    Pictures {
        art,
        icon,
        left_at,
        played,
    }
}

/// The games at `path`: the ROM or RPG Maker game there, or each ROM in it
/// if it is an archive, unpacked into the games folder; and the game each
/// ROM-hack patch there makes from the game it names among `known`
/// (checksum and path) or the archive's ROMs. Tells `progress` how far an
/// archive's unpacking has got.
pub(super) fn read_games(
    path: &Path,
    cache: Option<&Path>,
    known: &[(u32, PathBuf)],
    progress: &archive::Progress,
) -> Vec<Result<Game, String>> {
    let files = if archive::is_archive(path) {
        match archive::unpack(path, progress) {
            Ok(files) => files,
            Err(error) => return vec![Err(error)],
        }
    } else {
        vec![path.to_owned()]
    };
    let (patches, roms): (Vec<PathBuf>, Vec<PathBuf>) =
        files.into_iter().partition(|file| romhack::is_patch(file));
    let mut games: Vec<Result<Game, String>> =
        roms.iter().map(|rom| read_game(rom, cache)).collect();
    let mut known = known.to_vec();
    known.extend(
        games
            .iter()
            .flatten()
            .map(|game| (game.crc32, game.path.clone())),
    );
    for patch in patches {
        games.push(match romhack::make(&patch, None, &known) {
            Ok(rom) => read_game(&rom, cache),
            Err(MakeError::NoBase(_)) => Err(format!(
                "To add {}, open it with {} and choose the game it patches",
                path.file_name().unwrap_or_default().to_string_lossy(),
                keys::shortcut(shortcut::OPEN_ROM)
            )),
            Err(MakeError::Failed(error)) => Err(error),
        });
    }
    games
}

/// The game in the ROM at `path`, or the RPG Maker game there, with its DS
/// icon or its player app's icon saved in `cache`. A 3DS game's headers
/// are read without reading the rest of its file.
pub fn read_game(path: &Path, cache: Option<&Path>) -> Result<Game, String> {
    let cannot_read = |error: std::io::Error| format!("Cannot read {}: {error}", path.display());
    if let Some(folder) = rpg_maker::game_folder(path) {
        let game = Game::rpg_maker(&folder, rpg_maker::title(&folder));
        let player = rpg_maker::player_app(&folder);
        if let (Some(cache), Some(player)) = (cache, player) {
            // Its only picture: no game database lists RPG Maker games.
            app_icon::save(&player, &boxart::icon_path(cache, game.crc32)).ok();
        }
        return Ok(game);
    }
    if emu_3ds::is_game(path) {
        return Ok(Game::new(
            path,
            &rom_info::read_3ds(path).map_err(cannot_read)?,
        ));
    }
    let rom = fs::read(path).map_err(cannot_read)?;
    let info = rom_info::read(path, &rom).ok_or_else(|| {
        format!(
            "{} is not a .gb, .gbc, .gba, .nds or .3ds game",
            path.display()
        )
    })?;
    if let (Some(cache), Some(icon)) = (cache, rom_info::nds_icon(&rom)) {
        // The icon is only a fallback for missing box art.
        boxart::save_icon(cache, info.crc32, &icon).ok();
    }
    let mut game = Game::new(path, &info);
    if romhack::is_hack(path) {
        game.made_from_patch();
    }
    Ok(game)
}

/// What `game`'s files say of it: whether its ROM or folder is there, its
/// size, when its battery save was written and how many save states it
/// has.
pub(super) fn read_facts(game: &Game) -> Facts {
    let Ok(metadata) = fs::metadata(&game.path) else {
        return Facts::default();
    };
    let size = if metadata.is_dir() {
        archive::folder_size(&game.path)
    } else {
        metadata.len()
    };
    if game.platform == Platform::RpgMaker {
        return Facts {
            found: true,
            size,
            ..Facts::default()
        };
    }
    let files = library::files_of(game);
    let saved = fs::metadata(files.backup())
        .and_then(|metadata| metadata.modified())
        .ok();
    let held = |slots: std::ops::RangeInclusive<u8>| {
        slots
            .filter(|&slot| files.state(slot, false).is_file())
            .count()
    };
    Facts {
        found: true,
        size,
        saved,
        states: held(1..=STATE_SLOTS),
        script_states: held(STATE_SLOTS + 1..=LAST_FILE_SLOT),
        randomizable: randomizer::kind(&game.path).is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::draw::states_line;
    use super::*;

    #[test]
    fn a_3ds_game_is_read_from_its_headers_and_titled_by_its_file() {
        let dir = std::env::temp_dir().join(format!("emu-app-read-3ds-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("Cool_Homebrew.3dsx");
        let mut file = vec![0; 0x20];
        file[0..4].copy_from_slice(b"3DSX");
        file[4..6].copy_from_slice(&0x20_u16.to_le_bytes());
        fs::write(&path, &file).expect("written");

        let game = read_game(&path, None).expect("listed");
        assert_eq!(game.platform, rom_info::Platform::N3ds);
        assert_eq!(game.title, "Cool Homebrew");
        assert_eq!(game.path, path);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_games_pictures_and_last_play_are_read_from_disk() {
        let dir = std::env::temp_dir().join(format!("emu-app-pictures-{}", std::process::id()));
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).expect("temp dir");
        let info = rom_info::RomInfo {
            platform: rom_info::Platform::Gba,
            crc32: 0x1234,
            serial: None,
            header_title: None,
            banner_title: None,
        };
        let game = Game::new(&dir.join("Golden Sun.gba"), &info);

        // Never played, nor looked up.
        let pictures = pictures_on_disk(Some(&cache), &game);
        assert!(pictures.art.is_none() && pictures.icon.is_none());
        assert!(pictures.played.is_none() && pictures.left_at.is_none());

        let png = b"\x89PNG\r\n\x1a\n";
        boxart::write(&boxart::art_path(&cache, 0x1234), png).expect("art");
        boxart::write(&boxart::icon_path(&cache, 0x1234), png).expect("icon");
        let checkpoint = library::checkpoint_path(&game);
        fs::write(&checkpoint, b"state").expect("checkpoint");
        fs::write(persist::state_picture_path(&checkpoint), png).expect("picture");
        let pictures = pictures_on_disk(Some(&cache), &game);
        assert_eq!(pictures.art, Some(boxart::art_path(&cache, 0x1234)));
        assert_eq!(pictures.icon, Some(boxart::icon_path(&cache, 0x1234)));
        assert!(pictures.played.is_some());
        assert_eq!(
            pictures.left_at.map(|picture| picture.bytes().len()),
            Some(8)
        );

        // Without a cache folder, the game still has where it was left.
        let pictures = pictures_on_disk(None, &game);
        assert!(pictures.art.is_none() && pictures.left_at.is_some());

        // A DS game played only with a GBA cartridge in: that session's
        // checkpoint has another name, and its picture is left here.
        let info = rom_info::RomInfo {
            platform: rom_info::Platform::Nds,
            ..info
        };
        let slotted = Game::new(&dir.join("Platinum.nds"), &info);
        assert!(pictures_on_disk(None, &slotted).played.is_none());
        fs::write(dir.join("Platinum.Emerald.ss0"), b"state").expect("checkpoint");
        fs::write(dir.join("Platinum.ss0.png"), png).expect("picture");
        let pictures = pictures_on_disk(None, &slotted);
        assert!(pictures.played.is_some(), "last played with the cartridge");
        assert!(pictures.left_at.is_some());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_games_facts_are_read_from_its_files() {
        let dir = std::env::temp_dir().join(format!("emu-app-facts-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let info = |platform| rom_info::RomInfo {
            platform,
            crc32: 1,
            serial: None,
            header_title: None,
            banner_title: None,
        };
        let rom = dir.join("Golden Sun.gba");
        let game = Game::new(&rom, &info(Platform::Gba));
        assert_eq!(read_facts(&game), Facts::default(), "a missing ROM");

        fs::write(&rom, [0; 2048]).expect("rom");
        let facts = read_facts(&game);
        assert!(facts.found);
        assert_eq!((facts.size, facts.saved, facts.states), (2048, None, 0));

        fs::write(dir.join("Golden Sun.sav"), [0; 8]).expect("save");
        fs::write(dir.join("Golden Sun.ss1"), [0; 8]).expect("state");
        fs::write(dir.join("Golden Sun.ss3"), [0; 8]).expect("state");
        // The checkpoint is no slot.
        fs::write(dir.join("Golden Sun.ss0"), [0; 8]).expect("checkpoint");
        let facts = read_facts(&game);
        assert!(facts.saved.is_some());
        assert_eq!(facts.states, 2);
        assert!(!facts.randomizable, "not a Pokémon game");
        // States a script saved, in slots past the player's, count apart.
        assert_eq!(facts.script_states, 0);
        fs::write(dir.join("Golden Sun.ss5"), [0; 8]).expect("state");
        fs::write(dir.join("Golden Sun.ss9"), [0; 8]).expect("state");
        let facts = read_facts(&game);
        assert_eq!((facts.states, facts.script_states), (2, 2));
        assert_eq!(states_line(0, 0), "None");
        assert_eq!(states_line(2, 0), "2 of 4");
        assert_eq!(states_line(2, 2), "2 of 4, and 2 from scripts");
        assert_eq!(states_line(0, 1), "1 from scripts");

        // FireRed's game code says the randomizer knows it.
        let mut fire_red = vec![0; 2048];
        fire_red[0xAC..0xB0].copy_from_slice(b"BPRE");
        let rom = dir.join("FireRed.gba");
        fs::write(&rom, fire_red).expect("rom");
        assert!(read_facts(&Game::new(&rom, &info(Platform::Gba))).randomizable);

        // An RPG Maker game is its folder, all of it.
        let folder = dir.join("Fan Game");
        fs::create_dir_all(folder.join("Data")).expect("folder");
        fs::write(folder.join("Game.ini"), [0; 100]).expect("ini");
        fs::write(folder.join("Data/Map001.rxdata"), [0; 900]).expect("map");
        let facts = read_facts(&Game::rpg_maker(&folder, None));
        assert!(facts.found);
        assert_eq!(facts.size, 1000);
        fs::remove_dir_all(&dir).ok();
    }
}
