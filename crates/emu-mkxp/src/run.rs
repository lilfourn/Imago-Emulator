//! A run of a game: the game played another way, as its randomized run
//! is, without a second copy of it.
//!
//! A run is a folder that holds [`FILE`], `run.json`, which names the
//! game's folder, the scripts in the run's folder to load after the game's
//! own preload scripts, and the name mkxp-z keeps the run's saves under.
//! [`crate::game_folder`] takes such a folder for a game, and
//! [`crate::Mkxp::open`] plays it in a hidden player of its own, so the
//! game as it is keeps its saves, and nothing in its folder is changed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The file in a run's folder that says what the run is.
pub const FILE: &str = "run.json";

/// The organisation mkxp-z keeps a run's saves under when the game names
/// none: mkxp-z's own name, as the games made for it use.
const ORGANISATION: &str = "mkxp-z";

/// What a run's [`FILE`] says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The game's folder.
    pub game: PathBuf,
    /// The scripts in the run's folder that are loaded after the game's
    /// own preload scripts, by name, in order.
    pub preload: Vec<String>,
    /// The name mkxp-z keeps the run's saves under, in place of the
    /// game's (its `dataPathApp`), so the two never share a save.
    pub saves: String,
}

impl Run {
    /// The run in `folder`, if it is one: if its [`FILE`] is there and
    /// names a game's folder and where the run's saves go.
    #[must_use]
    pub fn read(folder: &Path) -> Option<Self> {
        let text = fs::read_to_string(folder.join(FILE)).ok()?;
        let run: Value = serde_json::from_str(&text).ok()?;
        let preload = match run.get("preload") {
            Some(Value::Array(scripts)) => scripts
                .iter()
                .filter_map(|script| script.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        Some(Self {
            game: PathBuf::from(run.get("game")?.as_str()?),
            preload,
            saves: run
                .get("saves")?
                .as_str()
                .filter(|saves| !saves.is_empty())?
                .to_owned(),
        })
    }

    /// Writes the run's [`FILE`] into `folder`, which is made if it is not
    /// there.
    ///
    /// # Errors
    ///
    /// Fails if the game's folder is not UTF-8, or the file cannot be
    /// written.
    pub fn write(&self, folder: &Path) -> io::Result<()> {
        let game = self.game.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a UTF-8 path", self.game.display()),
            )
        })?;
        let run = json!({"game": game, "preload": self.preload, "saves": self.saves});
        let text = serde_json::to_string_pretty(&run).expect("a JSON object always serializes");
        fs::create_dir_all(folder)?;
        emu_os::file::write_atomic(&folder.join(FILE), format!("{text}\n").as_bytes())
    }

    /// The run's scripts, in `folder`, the run's own.
    #[must_use]
    pub fn scripts(&self, folder: &Path) -> Vec<PathBuf> {
        self.preload.iter().map(|name| folder.join(name)).collect()
    }

    /// Where mkxp-z keeps the run's saves, in `root` ([`saves_root`]), for
    /// a game whose own settings are `game_settings` (its `mkxp.json`, as
    /// plain JSON): under the game's organisation, or mkxp-z's.
    #[must_use]
    pub fn save_folder(&self, game_settings: Option<&Value>, root: &Path) -> PathBuf {
        root.join(organisation(game_settings)).join(&self.saves)
    }
}

/// The organisation the saves of the game with `settings` are kept under:
/// its own `dataPathOrg`, or for a run of a game that names none, mkxp-z's.
pub(crate) fn organisation(settings: Option<&Value>) -> String {
    settings
        .and_then(|settings| settings.get("dataPathOrg"))
        .and_then(Value::as_str)
        .filter(|organisation| !organisation.is_empty())
        .unwrap_or(ORGANISATION)
        .to_owned()
}

/// Where mkxp-z keeps the saves of a game whose settings are `settings`
/// (its `mkxp.json`, as plain JSON), in `root` ([`saves_root`]), if they
/// name the folder: its `dataPathApp`, under its `dataPathOrg` if it has
/// one. A game that names none saves where mkxp-z chooses, which is not
/// known here.
#[must_use]
pub fn save_folder(settings: &Value, root: &Path) -> Option<PathBuf> {
    let name = |key: &str| {
        settings
            .get(key)
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
    };
    let mut folder = root.to_owned();
    folder.extend(name("dataPathOrg"));
    Some(folder.join(name("dataPathApp")?))
}

/// The folder mkxp-z keeps every game's saves in, as SDL names it: the
/// user's application data, which on Windows is the roaming one.
#[must_use]
pub fn saves_root() -> Option<PathBuf> {
    if cfg!(windows) {
        emu_os::config_dir()
    } else {
        emu_os::data_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_kept_in_its_run_json() {
        let dir = std::env::temp_dir().join(format!("emu-mkxp-run-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let folder = dir.join("My Game (Random 123456)");
        assert_eq!(Run::read(&folder), None, "no folder");
        let game = dir.join("My Game");
        let run = Run {
            game: game.clone(),
            preload: vec!["randomizer.rb".to_owned()],
            saves: "My Game (Random 123456)".to_owned(),
        };
        run.write(&folder).expect("written");
        let file = folder.join("run.json");
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("run.json")).expect("JSON");
        assert_eq!(
            written,
            json!({
                "game": game.to_str(),
                "preload": ["randomizer.rb"],
                "saves": "My Game (Random 123456)",
            })
        );
        assert_eq!(Run::read(&folder), Some(run.clone()));
        assert_eq!(run.scripts(&folder), [folder.join("randomizer.rb")]);
        // A file that names no game, or no saves, is no run.
        fs::write(&file, "{\"saves\": \"x\"}").expect("written");
        assert_eq!(Run::read(&folder), None);
        fs::write(&file, "{\"game\": \"/g\", \"saves\": \"\"}").expect("written");
        assert_eq!(Run::read(&folder), None);
        fs::write(&file, "not JSON").expect("written");
        assert_eq!(Run::read(&folder), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saves_are_kept_by_the_names_in_the_settings() {
        let root = Path::new("/data");
        let game = json!({"dataPathOrg": "mkxp-z", "dataPathApp": "My Game"});
        assert_eq!(
            save_folder(&game, root),
            Some(root.join("mkxp-z").join("My Game"))
        );
        let no_org = json!({"dataPathApp": "My Game"});
        assert_eq!(save_folder(&no_org, root), Some(root.join("My Game")));
        assert_eq!(save_folder(&json!({"dataPathOrg": "mkxp-z"}), root), None);
        // A run's are under its own name, in the game's organisation.
        let run = Run {
            game: PathBuf::from("/g"),
            preload: Vec::new(),
            saves: "My Game (Random 5)".to_owned(),
        };
        assert_eq!(
            run.save_folder(Some(&game), root),
            root.join("mkxp-z").join("My Game (Random 5)")
        );
        assert_eq!(
            run.save_folder(Some(&json!({"dataPathOrg": "Studio"})), root),
            root.join("Studio").join("My Game (Random 5)")
        );
        assert_eq!(
            run.save_folder(None, root),
            root.join("mkxp-z").join("My Game (Random 5)")
        );
        assert!(saves_root().is_some());
    }
}
