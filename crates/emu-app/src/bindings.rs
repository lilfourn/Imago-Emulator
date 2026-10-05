//! Keyboard bindings for held inputs: emulated buttons and fast-forward.
//!
//! One-shot commands (Open, Pause, Save State, ...) are GPUI actions bound
//! to menu shortcuts in `app.rs`. This module covers only keys that are held
//! down, because those need key-up events, which GPUI actions do not give.
//!
//! Users override the defaults in a TOML file that maps each action to one
//! or more GPUI key names:
//!
//! ```toml
//! [keys]
//! a = ["x", "k"]
//! start = ["enter"]
//! fast_forward = ["tab"]
//! ```
//!
//! Actions missing from the file keep their default keys.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use emu_core::Button;
use serde::Deserialize;

/// Something a held key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Held {
    /// Holds an emulated button.
    Button(Button),
    /// Runs emulation unthrottled while held.
    FastForward,
}

impl fmt::Display for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Button(button) => button.fmt(f),
            Self::FastForward => f.write_str("fast_forward"),
        }
    }
}

impl FromStr for Held {
    type Err = BindingsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "fast_forward" {
            return Ok(Self::FastForward);
        }
        s.parse()
            .map(Self::Button)
            .map_err(|_| BindingsError::UnknownAction(s.to_owned()))
    }
}

/// Maps GPUI key names (`"x"`, `"up"`, `"tab"`) to held actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    by_key: HashMap<String, Held>,
}

const DEFAULTS: [(Held, &str); 13] = [
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
];

impl Default for Bindings {
    fn default() -> Self {
        let by_key = DEFAULTS
            .into_iter()
            .map(|(held, key)| (key.to_owned(), held))
            .collect();
        Self { by_key }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingsFile {
    #[serde(default)]
    keys: BTreeMap<String, Vec<String>>,
}

impl Bindings {
    /// What `key` does, if it is bound.
    pub fn action(&self, key: &str) -> Option<Held> {
        self.by_key.get(key).copied()
    }

    /// The defaults with the actions listed in `text` rebound.
    ///
    /// # Errors
    ///
    /// Returns [`BindingsError`] if the TOML is malformed, names an unknown
    /// action, or binds one key to two actions.
    pub fn from_toml(text: &str) -> Result<Self, BindingsError> {
        let file: BindingsFile =
            toml::from_str(text).map_err(|error| BindingsError::Syntax(error.to_string()))?;
        let overrides: Vec<(Held, Vec<String>)> = file
            .keys
            .into_iter()
            .map(|(action, keys)| Ok((action.parse()?, keys)))
            .collect::<Result<_, BindingsError>>()?;

        let mut bindings = Self::default();
        bindings
            .by_key
            .retain(|_, held| overrides.iter().all(|(overridden, _)| overridden != held));
        for (held, keys) in overrides {
            for key in keys {
                match bindings.by_key.insert(key.clone(), held) {
                    Some(other) if other != held => {
                        return Err(BindingsError::Conflict {
                            key,
                            first: other,
                            second: held,
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(bindings)
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
    /// An action name is not a button or `fast_forward`.
    #[error("unknown action {0:?} in bindings")]
    UnknownAction(String),
    /// One key is bound to two different actions.
    #[error("key {key:?} is bound to both {first} and {second}")]
    Conflict {
        /// The doubly-bound key.
        key: String,
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
    fn defaults_cover_every_button_and_fast_forward() {
        let bindings = Bindings::default();
        for button in Button::ALL {
            assert!(
                bindings
                    .by_key
                    .values()
                    .any(|held| *held == Held::Button(button))
            );
        }
        assert_eq!(bindings.action("tab"), Some(Held::FastForward));
    }

    #[test]
    fn overrides_replace_only_the_listed_actions() {
        let bindings = Bindings::from_toml("[keys]\na = [\"k\", \"j\"]\n").expect("valid file");
        assert_eq!(bindings.action("k"), Some(Held::Button(Button::A)));
        assert_eq!(bindings.action("j"), Some(Held::Button(Button::A)));
        assert_eq!(bindings.action("x"), None);
        assert_eq!(bindings.action("z"), Some(Held::Button(Button::B)));
    }

    #[test]
    fn rejects_unknown_actions_and_conflicts() {
        assert_eq!(
            Bindings::from_toml("[keys]\nturbo = [\"t\"]\n"),
            Err(BindingsError::UnknownAction("turbo".to_owned()))
        );
        assert!(matches!(
            Bindings::from_toml("[keys]\na = [\"z\"]\n"),
            Err(BindingsError::Conflict { .. })
        ));
    }

    #[test]
    fn missing_file_means_defaults() {
        let path = Path::new("/nonexistent/ds-emulator/bindings.toml");
        assert_eq!(Bindings::load(path), Ok(Bindings::default()));
    }
}
