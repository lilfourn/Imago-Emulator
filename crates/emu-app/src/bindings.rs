//! Bindings for held inputs: emulated buttons, fast-forward, rewind and
//! touching the screen at the gamepad cursor, from the keyboard or
//! gamepads.
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
//! a = ["x", "k"]
//! fast_forward = ["tab"]
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
//! Actions missing from a section keep their defaults.

use std::collections::{BTreeMap, HashMap};
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
    /// Runs emulation unthrottled while held.
    FastForward,
    /// Steps backwards through recent history while held.
    Rewind,
    /// Touches the touchscreen at the gamepad cursor while held.
    Touch,
}

impl fmt::Display for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Button(button) => button.fmt(f),
            Self::FastForward => f.write_str("fast_forward"),
            Self::Rewind => f.write_str("rewind"),
            Self::Touch => f.write_str("touch"),
        }
    }
}

impl FromStr for Held {
    type Err = BindingsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fast_forward" => Ok(Self::FastForward),
            "rewind" => Ok(Self::Rewind),
            "touch" => Ok(Self::Touch),
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
    gamepad: Map,
    /// Per-controller gamepad maps, by UUID.
    gamepads: HashMap<String, Map>,
}

const KEY_DEFAULTS: [(Held, &str); 14] = [
    (Held::Button(Button::A), "x"),
    (Held::Button(Button::B), "z"),
    (Held::Button(Button::X), "s"),
    (Held::Button(Button::Y), "a"),
    (Held::Button(Button::L), "q"),
    (Held::Button(Button::R), "w"),
    (Held::Button(Button::Start), "enter"),
    (Held::Button(Button::Select), "backspace"),
    (Held::Button(Button::Up), "up"),
    (Held::Button(Button::Down), "down"),
    (Held::Button(Button::Left), "left"),
    (Held::Button(Button::Right), "right"),
    (Held::FastForward, "tab"),
    (Held::Rewind, "`"),
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

    /// The defaults with the actions listed in `text` rebound.
    ///
    /// # Errors
    ///
    /// Returns [`BindingsError`] if the TOML is malformed, names an unknown
    /// action, or binds one input to two actions.
    pub fn from_toml(text: &str) -> Result<Self, BindingsError> {
        let file: BindingsFile =
            toml::from_str(text).map_err(|error| BindingsError::Syntax(error.to_string()))?;
        let defaults = Self::default();
        let gamepad = rebind(&defaults.gamepad, file.gamepad)?;
        let gamepads = file
            .gamepads
            .into_iter()
            .map(|(uuid, section)| Ok((uuid.to_ascii_lowercase(), rebind(&gamepad, section)?)))
            .collect::<Result<_, BindingsError>>()?;
        Ok(Self {
            keys: rebind(&defaults.keys, file.keys)?,
            gamepad,
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
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(BindingsError::Io(error.to_string())),
        }
    }
}

/// `base` with every action named in `section` bound to exactly the inputs
/// listed there. An input the section claims is taken from whatever action
/// had it in `base`.
fn rebind(base: &Map, section: Section) -> Result<Map, BindingsError> {
    let mut claimed = Map::new();
    for (action, inputs) in section {
        let held: Held = action.parse()?;
        for input in inputs {
            if let Some(other) = claimed.insert(input.clone(), held)
                && other != held
            {
                return Err(BindingsError::Conflict {
                    input,
                    first: other,
                    second: held,
                });
            }
        }
    }
    let mut map: Map = base
        .iter()
        .filter(|(input, held)| {
            !claimed.contains_key(*input) && !claimed.values().any(|c| c == *held)
        })
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
    /// An action name is not a button, `fast_forward`, `rewind` or `touch`.
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
    }

    #[test]
    fn overrides_replace_only_the_listed_actions() {
        let bindings = Bindings::from_toml("[keys]\na = [\"k\", \"j\"]\n").expect("valid file");
        assert_eq!(bindings.key_action("k"), Some(Held::Button(Button::A)));
        assert_eq!(bindings.key_action("j"), Some(Held::Button(Button::A)));
        assert_eq!(bindings.key_action("x"), None);
        assert_eq!(bindings.key_action("z"), Some(Held::Button(Button::B)));
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
        let bindings = Bindings::from_toml("[keys]\na = [\"z\"]\n").expect("valid file");
        assert_eq!(bindings.key_action("z"), Some(Held::Button(Button::A)));
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
}
