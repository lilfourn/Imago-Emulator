//! Randomized runs of Pokémon Essentials games: PC fan games made with
//! RPG Maker, which play through mkxp-z (`emu-mkxp`).
//!
//! Such a game is Ruby scripts and data, not a ROM, and FVX knows none of
//! them. Nothing is rewritten. The run is a folder in the hacks folder that
//! names the game's folder (`emu_mkxp::Run`), with a script in it,
//! `essentials.rb` under the run's seed and settings, which the game's
//! hidden player loads after the game's own and which swaps species as the
//! game makes Pokémon: each for another of about the same strength, the
//! same one every time. The run has a hidden player and a save folder of
//! its own, so the game as it is keeps its save.
//!
//! The script knows Essentials as v18 has it. In a game whose scripts it
//! does not know it changes nothing, and the run's log says so.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use emu_cores::rpg_maker::{self, Run};

use super::{Made, log_path};
use crate::library::file_title;
use crate::persist;

/// The randomizer, as the run's player loads it, after its settings.
const SCRIPT: &str = include_str!("essentials.rb");
/// Its name in a run's folder.
const SCRIPT_NAME: &str = "randomizer.rb";

/// What the run of an Essentials game shuffles: two of the presets the
/// ROMs have. "Everything" changes each species' types, abilities and
/// moves, which these games keep in data the script does not rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shuffle {
    /// Wild Pokémon only.
    Wild,
    /// Wild Pokémon, trainers' teams, and the Pokémon the game gives and
    /// trades.
    Classic,
}

impl Shuffle {
    /// What the library keeps as the run's settings, where a ROM's run has
    /// FVX's settings string.
    pub fn settings(self) -> &'static str {
        match self {
            Self::Wild => "essentials:wild",
            Self::Classic => "essentials:classic",
        }
    }

    /// What the run whose settings are `settings` shuffles, if it is the
    /// run of an Essentials game.
    pub fn of(settings: &str) -> Option<Self> {
        [Self::Wild, Self::Classic]
            .into_iter()
            .find(|shuffle| shuffle.settings() == settings)
    }
}

/// Whether `path` names a Pokémon Essentials game a run can be made of: an
/// RPG Maker game's folder, not itself a run, with Essentials' own data in
/// it. A game packed into one encrypted archive shows none, and is not
/// offered.
pub fn is_game(path: &Path) -> bool {
    let Some(folder) = rpg_maker::game_folder(path) else {
        return false;
    };
    let essentials = folder.join("PBS").is_dir()
        || ["species.dat", "dexdata.dat"]
            .iter()
            .any(|data| folder.join("Data").join(data).is_file());
    essentials && !is_run(&folder)
}

/// Whether `path` is a run's folder.
pub fn is_run(path: &Path) -> bool {
    Run::read(path).is_some()
}

/// Makes the run of the Essentials game `game` names, which the library
/// calls `title`, with `seed`, in the hacks folder `hacks`: the folder
/// `Pokemon This Gym of Mine (Random 123456)`. Its spoiler log is written
/// beside it, as `….log`, when the run is first played: the script reads
/// the species from the game as it runs. A run made before is made again
/// the same, and keeps its save.
///
/// # Errors
///
/// Says why if `game` names no game, or a file cannot be written.
pub fn make(
    game: &Path,
    title: &str,
    shuffle: Shuffle,
    seed: u32,
    hacks: &Path,
) -> Result<Made, String> {
    let failed = |error: io::Error| format!("Cannot keep the randomized game: {error}");
    let game = rpg_maker::game_folder(game)
        .filter(|folder| !is_run(folder))
        .ok_or_else(|| format!("Cannot randomize: {} is not a game", game.display()))?;
    let name = format!("{} (Random {seed})", file_title(title));
    let folder = hacks.join(persist::safe_file_name(&name).as_ref());
    // Saved beside the game's own saves, under the game's name for them.
    let saves = save_name(&game).unwrap_or_else(|| file_title(title));
    let run = Run {
        game,
        preload: vec![SCRIPT_NAME.to_owned()],
        saves: format!("{saves} (Random {seed})"),
    };
    fs::create_dir_all(&folder).map_err(failed)?;
    let script = format!(
        "{}{SCRIPT}",
        script_settings(shuffle, seed, &log_path(&folder))
    );
    emu_os::file::write_atomic(&folder.join(SCRIPT_NAME), script.as_bytes()).map_err(failed)?;
    run.write(&folder).map_err(failed)?;
    Ok(Made {
        rom: folder,
        log: None,
    })
}

/// The name mkxp-z keeps the saves of the game in `game` under: the one
/// its settings give, or else its title.
fn save_name(game: &Path) -> Option<String> {
    let settings = rpg_maker::game_settings(game);
    let named = settings
        .as_ref()
        .and_then(|settings| settings.get("dataPathApp"))
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    named.or_else(|| rpg_maker::title(game))
}

/// The run's settings as the script reads them, in Ruby: the lines above
/// the script's own text.
fn script_settings(shuffle: Shuffle, seed: u32, log: &Path) -> String {
    let others = shuffle == Shuffle::Classic;
    // In single quotes Ruby reads everything as it is but `\` and `'`.
    let log = log
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('\'', "\\'");
    format!(
        "module DSRandomizer\n  SEED = {seed}\n  TRAINERS = {others}\n  GIFTS = {others}\n  LOG = '{log}'\nend\n\n"
    )
}

/// The folder the game's own saves are in, if its settings name it.
fn game_saves(game: &Path, root: &Path) -> Option<PathBuf> {
    let folder = rpg_maker::game_folder(game)?;
    rpg_maker::run::save_folder(&rpg_maker::game_settings(&folder)?, root)
}

/// The folder the saves of the run in `run` are in.
fn run_saves(run: &Path, root: &Path) -> Option<PathBuf> {
    let read = Run::read(run)?;
    Some(read.save_folder(rpg_maker::game_settings(&read.game).as_ref(), root))
}

/// The extensions of the saves RPG Maker games make: XP's, VX's and VX
/// Ace's. Essentials saves as XP does.
const SAVES: [&str; 3] = ["rxdata", "rvdata", "rvdata2"];

/// The saves in `folder`, a save folder: its files that are saves, not
/// the logs a game writes there too.
fn saves_in(folder: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(folder).into_iter().flatten().flatten();
    entries
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| {
                SAVES
                    .iter()
                    .any(|save| extension.eq_ignore_ascii_case(save))
            })
        })
        .collect()
}

/// Whether the game `game` names has a save a run could go on from.
pub fn has_save(game: &Path) -> bool {
    rpg_maker::run::saves_root().is_some_and(|root| has_save_in(game, &root))
}

/// [`has_save`], with mkxp-z's saves kept in `root`.
fn has_save_in(game: &Path, root: &Path) -> bool {
    game_saves(game, root).is_some_and(|folder| !saves_in(&folder).is_empty())
}

/// Copies the saves of the game `game` names to the run in `run`, which
/// then goes on from them. Nothing the run already has is replaced.
///
/// # Errors
///
/// Returns the error if a save is there but cannot be copied.
pub fn copy_save(game: &Path, run: &Path) -> io::Result<()> {
    match rpg_maker::run::saves_root() {
        Some(root) => copy_save_in(game, run, &root),
        None => Ok(()),
    }
}

/// [`copy_save`], with mkxp-z's saves kept in `root`.
fn copy_save_in(game: &Path, run: &Path, root: &Path) -> io::Result<()> {
    let (Some(from), Some(to)) = (game_saves(game, root), run_saves(run, root)) else {
        return Ok(());
    };
    let saves = saves_in(&from);
    if !saves.is_empty() {
        fs::create_dir_all(&to)?;
    }
    for save in saves {
        let Some(name) = save.file_name() else {
            continue;
        };
        let copy = to.join(name);
        if !copy.exists() {
            fs::copy(&save, copy)?;
        }
    }
    Ok(())
}

/// Whether the run in `run` has saved: whether its save folder holds a
/// file.
pub fn has_saved(run: &Path) -> bool {
    rpg_maker::run::saves_root().is_some_and(|root| has_saved_in(run, &root))
}

/// [`has_saved`], with mkxp-z's saves kept in `root`.
fn has_saved_in(run: &Path, root: &Path) -> bool {
    run_saves(run, root).is_some_and(|folder| !saves_in(&folder).is_empty())
}

/// Removes the run in `run`: its folder, which holds only what [`make`]
/// wrote, and its save folder if that is empty. Whether it was removed.
pub fn remove(run: &Path) -> bool {
    remove_in(run, rpg_maker::run::saves_root().as_deref())
}

/// [`remove`], with mkxp-z's saves kept in `root`.
fn remove_in(run: &Path, root: Option<&Path>) -> bool {
    if !is_run(run) {
        return false;
    }
    if let Some(saves) = root.and_then(|root| run_saves(run, root)) {
        // Only if empty: remove_dir removes nothing else.
        fs::remove_dir(saves).ok();
    }
    fs::remove_dir_all(run).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("emu-app-essentials-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        fs::write(path, bytes).expect("write");
    }

    /// An RPG Maker game's folder in `dir`, with a player for any system,
    /// that saves as `My Game` in mkxp-z's folder.
    fn game_in(dir: &Path) -> PathBuf {
        let game = dir.join("Games/My Game");
        write(&game.join("Game.ini"), b"[Game]\r\nTitle=My Game\r\n");
        fs::create_dir_all(game.join("My Game.app/Contents")).expect("player app");
        write(&game.join("Game.exe"), b"player");
        write(
            &game.join("mkxp.json"),
            b"{\n  // The game's own.\n  \"dataPathOrg\": \"mkxp-z\",\n  \"dataPathApp\": \"My Game\",\n}\n",
        );
        game
    }

    #[test]
    fn an_essentials_game_is_an_rpg_maker_game_with_its_data() {
        let dir = temp_dir("is-game");
        let game = game_in(&dir);
        assert!(!is_game(&game), "RPG Maker, but nothing of Essentials");
        fs::create_dir_all(game.join("PBS")).expect("PBS");
        assert!(is_game(&game));
        fs::remove_dir(game.join("PBS")).expect("removed");
        write(&game.join("Data/species.dat"), b"species");
        assert!(is_game(&game), "by its data, without its PBS files");
        assert!(!is_game(&dir.join("Games")), "no game");
        // Its run is played as a game, but no run is made of a run.
        let made = make(&game, "My Game", Shuffle::Wild, 5, &dir.join("Hacks")).expect("made");
        assert!(rpg_maker::game_folder(&made.rom).is_some());
        assert!(is_run(&made.rom) && !is_game(&made.rom));
        assert!(make(&made.rom, "My Game", Shuffle::Wild, 6, &dir.join("Hacks")).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_run_is_a_folder_that_names_the_game_with_the_script_set_for_it() {
        let dir = temp_dir("make");
        let game = game_in(&dir);
        let hacks = dir.join("Hacks");
        let made = make(&game, "My: Game", Shuffle::Classic, 123_456, &hacks).expect("made");
        let folder = hacks.join("My - Game (Random 123456)");
        assert_eq!(
            made,
            Made {
                rom: folder.clone(),
                log: None
            }
        );
        let run = Run::read(&folder).expect("a run");
        assert_eq!(run.game, game);
        assert_eq!(run.preload, ["randomizer.rb"]);
        assert_eq!(
            run.saves, "My Game (Random 123456)",
            "by the game's own name"
        );
        let script = fs::read_to_string(folder.join("randomizer.rb")).expect("script");
        let log = hacks.join("My - Game (Random 123456).log");
        let settings = format!(
            "module DSRandomizer\n  SEED = 123456\n  TRAINERS = true\n  GIFTS = true\n  LOG = '{}'\nend\n\n",
            log.display()
        );
        if !cfg!(windows) {
            assert!(script.starts_with(&settings), "{}", &script[..200]);
        }
        assert!(script.ends_with(SCRIPT));
        // Wild Pokémon only, and a path Ruby would read otherwise.
        assert_eq!(
            script_settings(Shuffle::Wild, 7, Path::new("C:\\it's\\run.log")),
            "module DSRandomizer\n  SEED = 7\n  TRAINERS = false\n  GIFTS = false\n  LOG = 'C:\\\\it\\'s\\\\run.log'\nend\n\n"
        );
        assert_eq!(Shuffle::of("essentials:classic"), Some(Shuffle::Classic));
        assert_eq!(Shuffle::of(Shuffle::Wild.settings()), Some(Shuffle::Wild));
        assert_eq!(Shuffle::of("428AQgEB"), None, "FVX's settings");
        fs::remove_dir_all(&dir).ok();
    }

    /// Makes a run of the Essentials game at `DS_EMULATOR_ESSENTIALS_GAME`
    /// in a temporary hacks folder, gives it a copy of the game's save
    /// from where mkxp-z really keeps it, and prints where the run and its
    /// saves are, to play it from. Use a copy of a game, whose settings
    /// name a save folder of its own.
    #[test]
    #[ignore = "needs a Pokémon Essentials game"]
    fn makes_a_run_for_real() {
        let game = PathBuf::from(
            std::env::var_os("DS_EMULATOR_ESSENTIALS_GAME").expect("DS_EMULATOR_ESSENTIALS_GAME"),
        );
        assert!(is_game(&game), "{} is not a game", game.display());
        let hacks = temp_dir("real-hacks");
        let made = make(&game, "Test", Shuffle::Classic, 482_913, &hacks).expect("made");
        let root = rpg_maker::run::saves_root().expect("a data folder");
        eprintln!("the game has a save: {}", has_save(&game));
        copy_save(&game, &made.rom).expect("copied");
        let saves = run_saves(&made.rom, &root).expect("a save folder");
        eprintln!("run: {}", made.rom.display());
        eprintln!("its saves: {} {:?}", saves.display(), saves_in(&saves));
        assert_eq!(has_saved(&made.rom), has_save(&game));
    }

    #[test]
    fn a_run_goes_on_from_a_copy_of_the_games_save_and_is_removed_whole() {
        let dir = temp_dir("saves");
        let game = game_in(&dir);
        let (hacks, root) = (dir.join("Hacks"), dir.join("Saves"));
        let made = make(&game, "My Game", Shuffle::Wild, 5, &hacks).expect("made");
        let own = root.join("mkxp-z/My Game/Game.rxdata");
        let copy = root.join("mkxp-z/My Game (Random 5)/Game.rxdata");

        assert!(!has_save_in(&game, &root) && !has_saved_in(&made.rom, &root));
        copy_save_in(&game, &made.rom, &root).expect("nothing to copy");
        assert!(!copy.exists());
        // A log the game left is no save.
        write(&root.join("mkxp-z/My Game/errorlog.txt"), b"log");
        assert!(!has_save_in(&game, &root));
        write(&own, b"save");
        assert!(has_save_in(&game, &root));
        copy_save_in(&game, &made.rom, &root).expect("copied");
        assert_eq!(fs::read(&copy).expect("copy"), b"save");
        assert!(has_saved_in(&made.rom, &root));
        assert!(!root.join("mkxp-z/My Game (Random 5)/errorlog.txt").exists());
        // Never replaced, and the game's own is left as it is.
        write(&copy, b"played on");
        copy_save_in(&game, &made.rom, &root).expect("copied");
        assert_eq!(fs::read(&copy).expect("copy"), b"played on");
        assert_eq!(fs::read(&own).expect("own"), b"save");

        assert!(!remove_in(&game, Some(&root)), "a game is not a run");
        assert!(game.join("Game.ini").is_file());
        // The run's folder goes; its save folder stays while it holds one.
        assert!(remove_in(&made.rom, Some(&root)));
        assert!(!made.rom.exists() && copy.is_file());
        let unplayed = make(&game, "My Game", Shuffle::Wild, 6, &hacks).expect("made");
        let empty = root.join("mkxp-z/My Game (Random 6)");
        fs::create_dir_all(&empty).expect("its save folder");
        assert!(remove_in(&unplayed.rom, Some(&root)));
        assert!(!unplayed.rom.exists() && !empty.exists());
        fs::remove_dir_all(&dir).ok();
    }
}
