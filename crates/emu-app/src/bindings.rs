//! Bindings for held inputs: emulated buttons and their turbo versions,
//! fast-forward, slow motion, rewind, touching the screen at the gamepad
//! cursor and blowing into the DS microphone, from the keyboard or
//! gamepads; and gamepad hotkeys.
//!
//! One-shot commands (Open, Pause, Save State, ...) are GPUI actions bound
//! to menu shortcuts in `app.rs`. This module covers inputs that are held
//! down, because those need release events, and the gamepad hotkeys that
//! run those commands from a controller (see `chords.rs`).
//!
//! Users override the defaults in a TOML file that maps each action to one
//! or more GPUI key names or gilrs control names. Gamepad bindings can be
//! overridden per controller, keyed by its UUID:
//!
//! ```toml
//! [keys]
//! a = ["k", "space"]
//! turbo_a = ["l"]         # A pressed and released over and over
//! rewind = ["`"]
//!
//! [gamepad]
//! a = ["East"]
//! rewind = ["LeftTrigger2"]
//! touch = ["RightThumb"]
//!
//! [gamepads.030000005e0400008e02000000007200]
//! a = ["South"]
//!
//! [hotkeys]               # gamepad chords: the modifier plus a control
//! modifier = ["Select"]
//! save_state = ["RightTrigger"]
//! reset = ["West"]
//!
//! [turbo]
//! frames = 3              # frames per turbo press, and per gap
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
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use emu_core::Button;
use serde::Deserialize;

use crate::turbo;

/// Something a held input does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Held {
    /// Holds an emulated button.
    Button(Button),
    /// Presses and releases an emulated button over and over while held
    /// (see `turbo.rs`).
    Turbo(Button),
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

/// What `turbo_a` and the like start with.
const TURBO_PREFIX: &str = "turbo_";

impl fmt::Display for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Button(button) => button.fmt(f),
            Self::Turbo(button) => write!(f, "{TURBO_PREFIX}{button}"),
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
        let unknown = || BindingsError::UnknownAction(s.to_owned());
        match s {
            "fast_forward" => Ok(Self::FastForward),
            "slow_motion" => Ok(Self::SlowMotion),
            "rewind" => Ok(Self::Rewind),
            "touch" => Ok(Self::Touch),
            "blow" => Ok(Self::Blow),
            _ => match s.strip_prefix(TURBO_PREFIX) {
                Some(button) => button.parse().map(Self::Turbo).map_err(|_| unknown()),
                None => s.parse().map(Self::Button).map_err(|_| unknown()),
            },
        }
    }
}

/// An app command a gamepad hotkey runs, as its menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shortcut {
    /// Saves a state to the current slot.
    SaveState,
    /// Loads the state in the current slot.
    LoadState,
    /// Selects the next state slot.
    NextSlot,
    /// Selects the previous state slot.
    PreviousSlot,
    /// Pauses or resumes.
    Pause,
    /// Power-cycles the system.
    Reset,
    /// Turns latched fast-forward on or off.
    ToggleFastForward,
    /// Turns latched slow motion on or off.
    ToggleSlowMotion,
    /// Swaps the DS screens.
    SwapScreens,
    /// Hands control to the next player in link play.
    SwitchPlayer,
    /// Enters or leaves full screen.
    FullScreen,
}

/// Each shortcut and its name in the `[hotkeys]` table.
const SHORTCUTS: [(Shortcut, &str); 11] = [
    (Shortcut::SaveState, "save_state"),
    (Shortcut::LoadState, "load_state"),
    (Shortcut::NextSlot, "next_slot"),
    (Shortcut::PreviousSlot, "previous_slot"),
    (Shortcut::Pause, "pause"),
    (Shortcut::Reset, "reset"),
    (Shortcut::ToggleFastForward, "toggle_fast_forward"),
    (Shortcut::ToggleSlowMotion, "toggle_slow_motion"),
    (Shortcut::SwapScreens, "swap_screens"),
    (Shortcut::SwitchPlayer, "switch_player"),
    (Shortcut::FullScreen, "full_screen"),
];

/// What a gamepad control does in the `[hotkeys]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hotkey {
    /// Held down, makes the other hotkey controls run their hotkeys.
    Modifier,
    /// Pressed with the modifier, runs an app command.
    Run(Shortcut),
    /// Pressed with the modifier, holds an action until released: `rewind`,
    /// `fast_forward` or any other held action.
    Hold(Held),
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Modifier => f.write_str("modifier"),
            Self::Run(shortcut) => {
                let name = SHORTCUTS
                    .iter()
                    .find(|(each, _)| each == shortcut)
                    .map_or("?", |(_, name)| name);
                f.write_str(name)
            }
            Self::Hold(held) => held.fmt(f),
        }
    }
}

impl FromStr for Hotkey {
    type Err = BindingsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "modifier" {
            return Ok(Self::Modifier);
        }
        match SHORTCUTS.iter().find(|(_, name)| *name == s) {
            Some((shortcut, _)) => Ok(Self::Run(*shortcut)),
            None => s.parse().map(Self::Hold),
        }
    }
}

/// Maps input names to what they do.
type Map<A = Held> = HashMap<String, A>;

/// Keyboard and gamepad bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    keys: Map,
    /// For controllers without a profile.
    gamepad: Map,
    /// Per-controller gamepad maps, by UUID.
    gamepads: HashMap<String, Map>,
    /// Gamepad hotkeys, for every controller.
    hotkeys: Map<Hotkey>,
    /// Frames per turbo press, and per gap; at least 1.
    turbo_frames: u8,
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

/// Select is the modifier, as in RetroArch. The shoulders save and load,
/// Start pauses, the D-pad's sides pick the state slot, up swaps the
/// screens and down hands control to the other player in link play. The
/// right trigger, which fast-forwards while held, latches it on or off.
/// Reset, which loses unsaved play, has no default.
const HOTKEY_DEFAULTS: [(Hotkey, &str); 9] = [
    (Hotkey::Modifier, "Select"),
    (Hotkey::Run(Shortcut::SaveState), "RightTrigger"),
    (Hotkey::Run(Shortcut::LoadState), "LeftTrigger"),
    (Hotkey::Run(Shortcut::Pause), "Start"),
    (Hotkey::Run(Shortcut::NextSlot), "DPadRight"),
    (Hotkey::Run(Shortcut::PreviousSlot), "DPadLeft"),
    (Hotkey::Run(Shortcut::SwapScreens), "DPadUp"),
    (Hotkey::Run(Shortcut::SwitchPlayer), "DPadDown"),
    (Hotkey::Run(Shortcut::ToggleFastForward), "RightTrigger2"),
];

fn map_of<A: Copy>(defaults: &[(A, &str)]) -> Map<A> {
    defaults
        .iter()
        .map(|(action, name)| ((*name).to_owned(), *action))
        .collect()
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            keys: map_of(&KEY_DEFAULTS),
            gamepad: map_of(&GAMEPAD_DEFAULTS),
            gamepads: HashMap::new(),
            hotkeys: map_of(&HOTKEY_DEFAULTS),
            turbo_frames: turbo::DEFAULT_FRAMES,
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
    #[serde(default)]
    hotkeys: Section,
    #[serde(default)]
    turbo: TurboSection,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TurboSection {
    frames: Option<u8>,
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

    /// What gamepad `control` does in the hotkey table, on any controller.
    pub fn hotkey(&self, control: &str) -> Option<Hotkey> {
        self.hotkeys.get(control).copied()
    }

    /// Frames each turbo press lasts, and each gap between presses.
    pub fn turbo_frames(&self) -> u8 {
        self.turbo_frames
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
    /// action, binds one input to two actions, or asks for zero turbo
    /// frames.
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
        let turbo_frames = match file.turbo.frames {
            Some(0) => return Err(BindingsError::NoTurboFrames),
            Some(frames) => frames,
            None => self.turbo_frames,
        };
        Ok(Self {
            keys: rebind(&self.keys, &file.keys)?,
            gamepad: rebind(&self.gamepad, &file.gamepad)?,
            gamepads,
            hotkeys: rebind(&self.hotkeys, &file.hotkeys)?,
            turbo_frames,
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
fn rebind<A>(base: &Map<A>, section: &Section) -> Result<Map<A>, BindingsError>
where
    A: FromStr<Err = BindingsError> + Copy + Eq + Hash + fmt::Display,
{
    let mut named = HashSet::new();
    let mut claimed: Map<A> = Map::new();
    for (action, inputs) in section {
        let action: A = action.parse()?;
        named.insert(action);
        for input in inputs {
            if let Some(other) = claimed.insert(input.clone(), action)
                && other != action
            {
                return Err(BindingsError::Conflict {
                    input: input.clone(),
                    first: other.to_string(),
                    second: action.to_string(),
                });
            }
        }
    }
    let mut map: Map<A> = base
        .iter()
        .filter(|(input, action)| !claimed.contains_key(*input) && !named.contains(*action))
        .map(|(input, action)| (input.clone(), *action))
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
    /// An action name is not a button, a turbo button such as `turbo_a`,
    /// `fast_forward`, `slow_motion`, `rewind`, `touch` or `blow`, nor in
    /// `[hotkeys]` one of the shortcuts or `modifier`.
    #[error("unknown action {0:?} in bindings")]
    UnknownAction(String),
    /// One input is bound to two different actions.
    #[error("{input:?} is bound to both {first} and {second}")]
    Conflict {
        /// The doubly-bound input.
        input: String,
        /// The action it was bound to first.
        first: String,
        /// The action that tried to take it.
        second: String,
    },
    /// `[turbo]` asks for presses zero frames long.
    #[error("turbo frames must be at least 1")]
    NoTurboFrames,
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
        assert_eq!(map_of(&HOTKEY_DEFAULTS).len(), HOTKEY_DEFAULTS.len());
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
        let text = "[keys]\nrewind = []\n\n[hotkeys]\nsave_state = []\n";
        let bindings = Bindings::from_toml(text).expect("valid file");
        assert_eq!(bindings.key_action("r"), None);
        assert_eq!(bindings.hotkey("RightTrigger"), None);
        assert_eq!(bindings.hotkey("Select"), Some(Hotkey::Modifier));
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
        assert_eq!(
            Bindings::from_toml("[keys]\nturbo_z = [\"t\"]\n"),
            Err(BindingsError::UnknownAction("turbo_z".to_owned()))
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

    #[test]
    fn turbo_buttons_parse_and_print() {
        let text = "[keys]\nturbo_a = [\"l\"]\n\n[gamepad]\nturbo_x = [\"Mode\"]\n";
        let bindings = Bindings::from_toml(text).expect("valid file");
        assert_eq!(bindings.key_action("l"), Some(Held::Turbo(Button::A)));
        assert_eq!(bindings.key_action("k"), Some(Held::Button(Button::A)));
        assert_eq!(
            bindings.pad_action("any", "Mode"),
            Some(Held::Turbo(Button::X))
        );
        assert_eq!(Held::Turbo(Button::Start).to_string(), "turbo_start");
        assert_eq!("turbo_start".parse(), Ok(Held::Turbo(Button::Start)));
    }

    #[test]
    fn no_turbo_is_bound_by_default() {
        let bindings = Bindings::default();
        let maps = [&bindings.keys, &bindings.gamepad];
        assert!(
            maps.iter()
                .flat_map(|map| map.values())
                .all(|held| !matches!(held, Held::Turbo(_)))
        );
        assert_eq!(bindings.turbo_frames(), turbo::DEFAULT_FRAMES);
    }

    #[test]
    fn turbo_frames_are_read_and_must_be_positive() {
        let bindings = Bindings::from_toml("[turbo]\nframes = 2\n").expect("valid file");
        assert_eq!(bindings.turbo_frames(), 2);
        assert_eq!(
            Bindings::from_toml("[turbo]\nframes = 0\n"),
            Err(BindingsError::NoTurboFrames)
        );
        assert!(matches!(
            Bindings::from_toml("[turbo]\nrate = 2\n"),
            Err(BindingsError::Syntax(_))
        ));
    }

    #[test]
    fn default_hotkeys_hang_off_select() {
        let bindings = Bindings::default();
        let hotkey = |control| bindings.hotkey(control);
        assert_eq!(hotkey("Select"), Some(Hotkey::Modifier));
        assert_eq!(
            hotkey("RightTrigger"),
            Some(Hotkey::Run(Shortcut::SaveState))
        );
        assert_eq!(
            hotkey("LeftTrigger"),
            Some(Hotkey::Run(Shortcut::LoadState))
        );
        assert_eq!(hotkey("Start"), Some(Hotkey::Run(Shortcut::Pause)));
        assert_eq!(hotkey("DPadRight"), Some(Hotkey::Run(Shortcut::NextSlot)));
        assert_eq!(hotkey("East"), None);
        assert!(
            !bindings
                .hotkeys
                .values()
                .any(|hotkey| *hotkey == Hotkey::Run(Shortcut::Reset)),
            "reset loses play, so it has no default"
        );
    }

    #[test]
    fn hotkeys_are_rebound_like_actions() {
        let text = "[hotkeys]\nmodifier = [\"Mode\"]\nreset = [\"West\"]\nrewind = [\"LeftTrigger2\"]\nload_state = [\"Start\"]\n";
        let bindings = Bindings::from_toml(text).expect("valid file");
        let hotkey = |control| bindings.hotkey(control);
        assert_eq!(hotkey("Mode"), Some(Hotkey::Modifier));
        assert_eq!(hotkey("Select"), None, "the modifier moved");
        assert_eq!(hotkey("West"), Some(Hotkey::Run(Shortcut::Reset)));
        assert_eq!(hotkey("LeftTrigger2"), Some(Hotkey::Hold(Held::Rewind)));
        // Start was pause's; load_state takes it, and leaves the shoulder.
        assert_eq!(hotkey("Start"), Some(Hotkey::Run(Shortcut::LoadState)));
        assert_eq!(hotkey("LeftTrigger"), None);
        assert_eq!(
            hotkey("RightTrigger"),
            Some(Hotkey::Run(Shortcut::SaveState))
        );
    }

    #[test]
    fn hotkeys_reject_unknown_names_and_doubly_bound_controls() {
        assert_eq!(
            Bindings::from_toml("[hotkeys]\nquit = [\"Mode\"]\n"),
            Err(BindingsError::UnknownAction("quit".to_owned()))
        );
        assert_eq!(
            Bindings::from_toml("[hotkeys]\nmodifier = [\"Mode\"]\npause = [\"Mode\"]\n"),
            Err(BindingsError::Conflict {
                input: "Mode".to_owned(),
                first: "modifier".to_owned(),
                second: "pause".to_owned(),
            })
        );
    }

    #[test]
    fn hotkey_names_round_trip() {
        for (shortcut, name) in SHORTCUTS {
            assert_eq!(Hotkey::Run(shortcut).to_string(), name);
            assert_eq!(name.parse(), Ok(Hotkey::Run(shortcut)));
        }
        assert_eq!("modifier".parse(), Ok(Hotkey::Modifier));
        assert_eq!("turbo_b".parse(), Ok(Hotkey::Hold(Held::Turbo(Button::B))));
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
turbo_a = [\"l\"]

[gamepad]
x = [\"North\"]
turbo_a = [\"RightTrigger2\"]

[turbo]
frames = 1
";
        let bindings = user.overlay(game).expect("valid file");
        let key = |name| bindings.key_action(name);
        assert_eq!(key("l"), Some(Held::Turbo(Button::A)));
        assert_eq!(key("o"), Some(Held::Button(Button::X)), "the user's keys");
        for controller in ["pro", "other"] {
            let pad = |control| bindings.pad_action(controller, control);
            // The game's [gamepad] beats the controller's own profile...
            assert_eq!(pad("North"), Some(Held::Button(Button::X)), "{controller}");
            assert_eq!(pad("RightTrigger2"), Some(Held::Turbo(Button::A)));
            // ...and the user's choices it doesn't touch stay.
            assert_eq!(pad("South"), Some(Held::Button(Button::A)));
            assert_eq!(pad("East"), Some(Held::Button(Button::B)));
        }
        assert_eq!(bindings.pad_action("pro", "Mode"), Some(Held::FastForward));
        assert_eq!(bindings.pad_action("pro", "West"), None, "x moved");
        assert_eq!(bindings.pad_action("other", "Mode"), None);
        assert_eq!(bindings.turbo_frames(), 1);
    }

    #[test]
    fn a_game_file_can_have_controller_profiles_and_hotkeys() {
        let user = Bindings::from_toml(USER).expect("valid file");
        let game = "\
[gamepads.PRO]
a = [\"North\"]

[gamepads.new]
start = [\"Mode\"]

[hotkeys]
reset = [\"West\"]
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
        assert_eq!(bindings.hotkey("West"), Some(Hotkey::Run(Shortcut::Reset)));
        assert_eq!(bindings.hotkey("Select"), Some(Hotkey::Modifier));
        assert_eq!(bindings.turbo_frames(), turbo::DEFAULT_FRAMES);
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
