//! Bindings for held inputs: emulated buttons, fast-forward, slow motion,
//! rewind, touching the screen at the gamepad cursor and blowing into the
//! DS microphone, from the keyboard or gamepads.
//!
//! One-shot commands (Open, Pause, Save State, ...) are GPUI actions bound
//! to menu shortcuts in `app.rs`. This module covers only inputs that are
//! held down, because those need release events.
//!
//! Users override the defaults in a TOML file that maps each action to one
//! or more GPUI key names or gilrs control names. Gamepad bindings can be
//! overridden per controller, keyed by its UUID:
//!
//! ```toml
//! [keys]
//! a = ["k", "space"]
//! rewind = ["`"]
//!
//! [gamepad]
//! a = ["East"]
//! rewind = ["LeftTrigger2"]
//! touch = ["RightThumb"]
//!
//! [gamepads.030000005e0400008e02000000007200]
//! a = ["South"]
//! ```
//!
//! Actions missing from a section keep their defaults; an action listed
//! with no inputs is unbound.
//!
//! A game can have its own bindings file, `game.bindings.toml` next to the
//! ROM, in the same format. It is laid over the user's file when the game
//! starts, so a game's bindings win over a controller's profile, which
//! wins over the `[gamepad]` section.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use emu_core::Button;
use serde::Deserialize;

/// Something a held input does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Held {
    /// Holds an emulated button.
    Button(Button),
    /// Runs emulation at the fast-forward speed while held.
    FastForward,
    /// Runs emulation at the slow-motion speed while held.
    SlowMotion,
    /// Steps backwards through recent history while held.
    Rewind,
    /// Touches the touchscreen at the gamepad cursor while held.
    Touch,
    /// Blows into the microphone while held (DS).
    Blow,
}

impl fmt::Display for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Button(button) => button.fmt(f),
            Self::FastForward => f.write_str("fast_forward"),
            Self::SlowMotion => f.write_str("slow_motion"),
            Self::Rewind => f.write_str("rewind"),
            Self::Touch => f.write_str("touch"),
            Self::Blow => f.write_str("blow"),
        }
    }
}

impl FromStr for Held {
    type Err = BindingsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fast_forward" => Ok(Self::FastForward),
            "slow_motion" => Ok(Self::SlowMotion),
            "rewind" => Ok(Self::Rewind),
            "touch" => Ok(Self::Touch),
            "blow" => Ok(Self::Blow),
            _ => s
                .parse()
                .map(Self::Button)
                .map_err(|_| BindingsError::UnknownAction(s.to_owned())),
        }
    }
}

/// Maps input names to held actions.
type Map = HashMap<String, Held>;

/// Keyboard and gamepad bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    keys: Map,
    /// For controllers without a profile.
    gamepad: Map,
    /// Per-controller gamepad maps, by UUID.
    gamepads: HashMap<String, Map>,
}

/// Laid out for playing on a keyboard: the left hand on WASD and the
/// shoulders either side of it, the right hand on a square of four keys in
/// the DS face-button positions (Y and X on top, B and A below). The arrow
/// keys steer too, and B, for blow, blows into the microphone.
const KEY_DEFAULTS: [(Held, &str); 20] = [
    (Held::Button(Button::Up), "w"),
    (Held::Button(Button::Left), "a"),
    (Held::Button(Button::Down), "s"),
    (Held::Button(Button::Right), "d"),
    (Held::Button(Button::Up), "up"),
    (Held::Button(Button::Left), "left"),
    (Held::Button(Button::Down), "down"),
    (Held::Button(Button::Right), "right"),
    (Held::Button(Button::Y), "u"),
    (Held::Button(Button::X), "i"),
    (Held::Button(Button::B), "j"),
    (Held::Button(Button::A), "k"),
    (Held::Button(Button::L), "q"),
    (Held::Button(Button::R), "e"),
    (Held::Button(Button::Start), "enter"),
    (Held::Button(Button::Select), "backspace"),
    (Held::FastForward, "tab"),
    (Held::SlowMotion, "1"),
    (Held::Rewind, "r"),
    (Held::Blow, "b"),
];

/// Nintendo layout by position: A is the right face button, B the bottom.
/// The right stick moves the touch cursor (see `gamepad.rs`); clicking
/// either stick touches.
const GAMEPAD_DEFAULTS: [(Held, &str); 20] = [
    (Held::Button(Button::A), "East"),
    (Held::Button(Button::B), "South"),
    (Held::Button(Button::X), "North"),
    (Held::Button(Button::Y), "West"),
    (Held::Button(Button::L), "LeftTrigger"),
    (Held::Button(Button::R), "RightTrigger"),
    (Held::Button(Button::Start), "Start"),
    (Held::Button(Button::Select), "Select"),
    (Held::Button(Button::Up), "DPadUp"),
    (Held::Button(Button::Down), "DPadDown"),
    (Held::Button(Button::Left), "DPadLeft"),
    (Held::Button(Button::Right), "DPadRight"),
    (Held::Button(Button::Up), "LeftStickUp"),
    (Held::Button(Button::Down), "LeftStickDown"),
    (Held::Button(Button::Left), "LeftStickLeft"),
    (Held::Button(Button::Right), "LeftStickRight"),
    (Held::FastForward, "RightTrigger2"),
    (Held::Rewind, "LeftTrigger2"),
    (Held::Touch, "RightThumb"),
    (Held::Touch, "LeftThumb"),
];

fn map_of(defaults: &[(Held, &str)]) -> Map {
    defaults
        .iter()
        .map(|(held, name)| ((*name).to_owned(), *held))
        .collect()
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            keys: map_of(&KEY_DEFAULTS),
            gamepad: map_of(&GAMEPAD_DEFAULTS),
            gamepads: HashMap::new(),
        }
    }
}

/// Action name to input names, as written in the file.
type Section = BTreeMap<String, Vec<String>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingsFile {
    #[serde(default)]
    keys: Section,
    #[serde(default)]
    gamepad: Section,
    #[serde(default)]
    gamepads: BTreeMap<String, Section>,
}

impl Bindings {
    /// What keyboard key `key` does, if it is bound.
    pub fn key_action(&self, key: &str) -> Option<Held> {
        self.keys.get(key).copied()
    }

    /// What `control` on the controller with UUID `controller` does.
    pub fn pad_action(&self, controller: &str, control: &str) -> Option<Held> {
        self.gamepads
            .get(controller)
            .unwrap_or(&self.gamepad)
            .get(control)
            .copied()
    }

    /// These bindings with `text`, a bindings file, laid over them: each
    /// action it lists is bound to exactly the inputs listed, and the
    /// rest keep theirs. A controller's bindings come from, lowest first,
    /// these bindings for it, the file's `[gamepad]` section and the
    /// file's section for that controller.
    ///
    /// # Errors
    ///
    /// Returns [`BindingsError`] if the TOML is malformed, names an unknown
    /// action, or binds one input to two actions.
    pub fn overlay(&self, text: &str) -> Result<Self, BindingsError> {
        let file: BindingsFile =
            toml::from_str(text).map_err(|error| BindingsError::Syntax(error.to_string()))?;
        let profiles: BTreeMap<String, Section> = file
            .gamepads
            .into_iter()
            .map(|(uuid, section)| (uuid.to_ascii_lowercase(), section))
            .collect();
        let controllers: BTreeSet<&String> = self.gamepads.keys().chain(profiles.keys()).collect();
        let gamepads = controllers
            .into_iter()
            .map(|uuid| {
                let base = self.gamepads.get(uuid).unwrap_or(&self.gamepad);
                let general = rebind(base, &file.gamepad)?;
                let map = match profiles.get(uuid) {
                    Some(profile) => rebind(&general, profile)?,
                    None => general,
                };
                Ok((uuid.clone(), map))
            })
            .collect::<Result<_, BindingsError>>()?;
        Ok(Self {
            keys: rebind(&self.keys, &file.keys)?,
            gamepad: rebind(&self.gamepad, &file.gamepad)?,
            gamepads,
        })
    }

    /// Reads bindings from `path`, or the defaults if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`BindingsError`] if the file exists but cannot be read or
    /// parsed.
    pub fn load(path: &Path) -> Result<Self, BindingsError> {
        Self::default().overlay_file(path)
    }

    /// These bindings with the file at `path` laid over them (see
    /// [`Bindings::overlay`]), or unchanged if there is no such file.
    ///
    /// # Errors
    ///
    /// Returns [`BindingsError`] if the file exists but cannot be read or
    /// parsed.
    pub fn overlay_file(&self, path: &Path) -> Result<Self, BindingsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => self.overlay(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(self.clone()),
            Err(error) => Err(BindingsError::Io(error.to_string())),
        }
    }
}

/// `base` with every action named in `section` bound to exactly the inputs
/// listed there, none if the list is empty. An input the section claims
/// is taken from whatever action had it in `base`.
fn rebind(base: &Map, section: &Section) -> Result<Map, BindingsError> {
    let mut named = HashSet::new();
    let mut claimed = Map::new();
    for (action, inputs) in section {
        let held: Held = action.parse()?;
        named.insert(held);
        for input in inputs {
            if let Some(other) = claimed.insert(input.clone(), held)
                && other != held
            {
                return Err(BindingsError::Conflict {
                    input: input.clone(),
                    first: other,
                    second: held,
                });
            }
        }
    }
    let mut map: Map = base
        .iter()
        .filter(|(input, held)| !claimed.contains_key(*input) && !named.contains(*held))
        .map(|(input, held)| (input.clone(), *held))
        .collect();
    map.extend(claimed);
    Ok(map)
}

/// Where the user's bindings file lives, e.g.
/// `~/Library/Application Support/ds-emulator/bindings.toml` on macOS.
pub fn default_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("ds-emulator")
            .join("bindings.toml"),
    )
}

/// Why a bindings file was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingsError {
    /// The file could not be read.
    #[error("cannot read bindings: {0}")]
    Io(String),
    /// The file is not valid TOML of the expected shape.
    #[error("invalid bindings file: {0}")]
    Syntax(String),
    /// An action name is not a button, `fast_forward`, `slow_motion`,
    /// `rewind`, `touch` or `blow`.
    #[error("unknown action {0:?} in bindings")]
    UnknownAction(String),
    /// One input is bound to two different actions.
    #[error("{input:?} is bound to both {first} and {second}")]
    Conflict {
        /// The doubly-bound input.
        input: String,
        /// The action it was bound to first.
        first: Held,
        /// The action that tried to take it.
        second: Held,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Bindings {
        /// The defaults with the actions listed in `text` rebound.
        fn from_toml(text: &str) -> Result<Self, BindingsError> {
            Self::default().overlay(text)
        }
    }

    #[test]
    fn defaults_cover_every_button_fast_forward_and_rewind() {
        let bindings = Bindings::default();
        for map in [&bindings.keys, &bindings.gamepad] {
            for button in Button::ALL {
                assert!(map.values().any(|held| *held == Held::Button(button)));
            }
            assert!(map.values().any(|held| *held == Held::FastForward));
            assert!(map.values().any(|held| *held == Held::Rewind));
        }
        assert_eq!(bindings.pad_action("any", "RightThumb"), Some(Held::Touch));
        assert_eq!(bindings.key_action("1"), Some(Held::SlowMotion));
    }

    #[test]
    fn default_keys_are_each_bound_once() {
        assert_eq!(map_of(&KEY_DEFAULTS).len(), KEY_DEFAULTS.len());
        assert_eq!(map_of(&GAMEPAD_DEFAULTS).len(), GAMEPAD_DEFAULTS.len());
    }

    #[test]
    fn default_keys_suit_a_keyboard() {
        let bindings = Bindings::default();
        let key = |name| bindings.key_action(name);
        for (wasd, arrow, button) in [
            ("w", "up", Button::Up),
            ("a", "left", Button::Left),
            ("s", "down", Button::Down),
            ("d", "right", Button::Right),
        ] {
            assert_eq!(key(wasd), Some(Held::Button(button)), "{wasd}");
            assert_eq!(key(arrow), Some(Held::Button(button)), "{arrow}");
        }
        // Face buttons in the DS's diamond, squared off: Y X over B A.
        assert_eq!(key("u"), Some(Held::Button(Button::Y)));
        assert_eq!(key("i"), Some(Held::Button(Button::X)));
        assert_eq!(key("j"), Some(Held::Button(Button::B)));
        assert_eq!(key("k"), Some(Held::Button(Button::A)));
        assert_eq!(key("q"), Some(Held::Button(Button::L)));
        assert_eq!(key("e"), Some(Held::Button(Button::R)));
        assert_eq!(key("tab"), Some(Held::FastForward));
        assert_eq!(key("r"), Some(Held::Rewind));
        assert_eq!(key("b"), Some(Held::Blow));
    }

    #[test]
    fn blowing_can_be_bound_to_a_gamepad() {
        let bindings = Bindings::from_toml("[gamepad]\nblow = [\"Mode\"]\n").expect("valid file");
        assert_eq!(bindings.pad_action("any", "Mode"), Some(Held::Blow));
        assert_eq!(
            bindings.key_action("b"),
            Some(Held::Blow),
            "keys keep theirs"
        );
    }

    #[test]
    fn slow_motion_can_be_bound_to_a_gamepad() {
        let bindings =
            Bindings::from_toml("[gamepad]\nslow_motion = [\"Mode\"]\n").expect("valid file");
        assert_eq!(bindings.pad_action("any", "Mode"), Some(Held::SlowMotion));
    }

    #[test]
    fn overrides_replace_only_the_listed_actions() {
        let bindings = Bindings::from_toml("[keys]\na = [\"x\", \"space\"]\n").expect("valid file");
        assert_eq!(bindings.key_action("x"), Some(Held::Button(Button::A)));
        assert_eq!(bindings.key_action("space"), Some(Held::Button(Button::A)));
        assert_eq!(bindings.key_action("k"), None);
        assert_eq!(bindings.key_action("j"), Some(Held::Button(Button::B)));
    }

    #[test]
    fn an_empty_list_unbinds_an_action() {
        let bindings = Bindings::from_toml("[keys]\nrewind = []\n").expect("valid file");
        assert_eq!(bindings.key_action("r"), None);
        assert_eq!(bindings.key_action("tab"), Some(Held::FastForward));
    }

    #[test]
    fn controller_profiles_override_the_gamepad_section() {
        let text = "[gamepad]\nb = [\"West\"]\ny = [\"South\"]\n\n[gamepads.ABCD]\na = [\"South\"]\nb = [\"East\"]\n";
        let bindings = Bindings::from_toml(text).expect("valid file");
        assert_eq!(
            bindings.pad_action("other", "West"),
            Some(Held::Button(Button::B))
        );
        assert_eq!(
            bindings.pad_action("abcd", "South"),
            Some(Held::Button(Button::A))
        );
        assert_eq!(
            bindings.pad_action("abcd", "East"),
            Some(Held::Button(Button::B))
        );
        assert_eq!(bindings.pad_action("abcd", "West"), None);
    }

    #[test]
    fn touch_can_be_rebound() {
        let bindings = Bindings::from_toml("[gamepad]\ntouch = [\"West\"]\n").expect("valid file");
        assert_eq!(bindings.pad_action("any", "West"), Some(Held::Touch));
        assert_eq!(bindings.pad_action("any", "RightThumb"), None);
    }

    #[test]
    fn rejects_unknown_actions_and_conflicts() {
        assert_eq!(
            Bindings::from_toml("[keys]\nturbo = [\"t\"]\n"),
            Err(BindingsError::UnknownAction("turbo".to_owned()))
        );
        assert!(matches!(
            Bindings::from_toml("[keys]\na = [\"k\"]\nb = [\"k\"]\n"),
            Err(BindingsError::Conflict { .. })
        ));
    }

    #[test]
    fn rebinding_takes_the_input_from_its_old_action() {
        let bindings = Bindings::from_toml("[keys]\na = [\"j\"]\n").expect("valid file");
        assert_eq!(bindings.key_action("j"), Some(Held::Button(Button::A)));
        assert!(
            !bindings
                .keys
                .values()
                .any(|held| *held == Held::Button(Button::B))
        );
    }

    #[test]
    fn missing_file_means_defaults() {
        let path = Path::new("/nonexistent/ds-emulator/bindings.toml");
        assert_eq!(Bindings::load(path), Ok(Bindings::default()));
    }

    /// The user's file, with a profile for one controller.
    const USER: &str = "\
[keys]
x = [\"o\"]

[gamepad]
a = [\"South\"]
b = [\"East\"]

[gamepads.pro]
fast_forward = [\"Mode\"]
x = [\"West\"]
";

    #[test]
    fn a_game_file_wins_over_profiles_and_the_user_file() {
        let user = Bindings::from_toml(USER).expect("valid file");
        let game = "\
[keys]
a = [\"l\"]

[gamepad]
x = [\"North\"]
rewind = [\"RightTrigger2\"]
";
        let bindings = user.overlay(game).expect("valid file");
        let key = |name| bindings.key_action(name);
        assert_eq!(key("l"), Some(Held::Button(Button::A)));
        assert_eq!(key("k"), None);
        assert_eq!(key("o"), Some(Held::Button(Button::X)), "the user's keys");
        for controller in ["pro", "other"] {
            let pad = |control| bindings.pad_action(controller, control);
            // The game's [gamepad] beats the controller's own profile...
            assert_eq!(pad("North"), Some(Held::Button(Button::X)), "{controller}");
            assert_eq!(pad("RightTrigger2"), Some(Held::Rewind));
            // ...and the user's choices it doesn't touch stay.
            assert_eq!(pad("South"), Some(Held::Button(Button::A)));
            assert_eq!(pad("East"), Some(Held::Button(Button::B)));
        }
        assert_eq!(bindings.pad_action("pro", "Mode"), Some(Held::FastForward));
        assert_eq!(bindings.pad_action("pro", "West"), None, "x moved");
        assert_eq!(bindings.pad_action("other", "Mode"), None);
    }

    #[test]
    fn a_game_file_can_have_controller_profiles() {
        let user = Bindings::from_toml(USER).expect("valid file");
        let game = "\
[gamepads.PRO]
a = [\"North\"]

[gamepads.new]
start = [\"Mode\"]
";
        let bindings = user.overlay(game).expect("valid file");
        assert_eq!(
            bindings.pad_action("pro", "North"),
            Some(Held::Button(Button::A))
        );
        assert_eq!(
            bindings.pad_action("pro", "Mode"),
            Some(Held::FastForward),
            "the user's profile underneath"
        );
        assert_eq!(
            bindings.pad_action("new", "Mode"),
            Some(Held::Button(Button::Start))
        );
        assert_eq!(
            bindings.pad_action("new", "South"),
            Some(Held::Button(Button::A)),
            "the user's [gamepad] underneath"
        );
    }

    #[test]
    fn game_files_reject_what_user_files_reject() {
        let user = Bindings::default();
        assert!(matches!(
            user.overlay("[keyz]\na = [\"k\"]\n"),
            Err(BindingsError::Syntax(_))
        ));
        assert_eq!(
            user.overlay("[gamepad]\njump = [\"South\"]\n"),
            Err(BindingsError::UnknownAction("jump".to_owned()))
        );
    }

    #[test]
    fn a_missing_game_file_changes_nothing() {
        let user = Bindings::from_toml(USER).expect("valid file");
        let path = Path::new("/nonexistent/game.bindings.toml");
        assert_eq!(user.overlay_file(path), Ok(user));
    }
}
