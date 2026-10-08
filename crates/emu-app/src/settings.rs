//! Settings kept between launches, so the app opens as it was left: the
//! screen layout, fast-forward and slow motion and their speeds, the
//! resolution of DS 3D, the video filter and colour correction, what
//! fills the window around the screens, the save-state slot, and the
//! window's place, size and controls.
//!
//! The app writes them whenever one changes, as TOML next to the bindings
//! file:
//!
//! ```toml
//! [layout]
//! arrangement = "hybrid"  # "vertical", "horizontal", "single" or "hybrid"
//! swapped = false
//! rotation = 270          # degrees clockwise: 0, 90, 180 or 270
//! gap = 90                # emulated pixels between the screens
//!
//! [speed]
//! fast_forward = 2.0      # times normal speed, or "unlimited"
//! slow_motion = 0.5       # times normal speed
//! fast_forward_on = false # left on with ⌘F
//! slow_motion_on = false  # left on with ⌘⇧F
//!
//! [video]
//! resolution_3d = 2       # DS 3D at 1 to 8 times its own resolution
//! filter = "lcd-grid"     # "none", "lcd-grid", "scanlines", "scale2x" or "scale3x"
//! show_fps = true        # the frame rate and speed in the window's corner
//! backdrop = "ambient"    # around the screens: "ambient" (the game's colours, blurred) or "solid"
//! color_correction = true # GBC and GBA colours as their screens showed them
//!
//! [game_boy]
//! dmg_colors = "game-boy-color"  # games for the original Game Boy: "grey", or
//!                               # on a Game Boy Color, by title or with a
//!                               # button combination such as "left+b"
//!
//! [states]
//! slot = 1                # the save-state slot ⌘S and ⌘L use, 1 to 4
//!
//! [window]
//! controls = true         # the close, minimise and zoom buttons
//! key_guide = true        # the keys that play the game, along the foot
//! fullscreen = false
//! display = "37d8832a…"   # the display it was on, by its UUID
//! x = 320.0               # where the window was on it and its size outside
//! y = 120.0               # full screen, in logical pixels; left out
//! width = 640.0           # until the window has been moved or resized
//! height = 960.0
//! ```
//!
//! Settings missing from the file keep their defaults. Files saved before
//! fast-forward ran at 2x by default left `fast_forward` out for no
//! limit; they now get 2x, and the menu or `"unlimited"` brings it back.

use std::path::{Path, PathBuf};

use emu_core::DmgColors;
use serde::{Deserialize, Serialize};

use crate::ambient::Backdrop;
use crate::filter::Filter;
use crate::layout::{Arrangement, Layout, Rotation};
use crate::persist;
use crate::slots::STATE_SLOTS;
use crate::speed::{MIN_RATE, Rate, Speed, Speeds};

/// Each arrangement and its name in the file.
const ARRANGEMENTS: [(Arrangement, &str); 4] = [
    (Arrangement::Vertical, "vertical"),
    (Arrangement::Horizontal, "horizontal"),
    (Arrangement::Single, "single"),
    (Arrangement::Hybrid, "hybrid"),
];

/// The highest DS 3D resolution, as a multiple of the native one: what the
/// core draws at most.
pub const MAX_RESOLUTION_3D: u32 = 8;

/// Everything remembered between launches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// How the screens are laid out.
    pub layout: Layout,
    /// How fast fast-forward and slow motion run.
    pub speed: Speeds,
    /// Whether fast-forward was left on.
    pub fast_forward_on: bool,
    /// Whether slow motion was left on.
    pub slow_motion_on: bool,
    /// How many times its own resolution DS 3D is drawn at, 1 to
    /// [`MAX_RESOLUTION_3D`] (see `Core::set_render_scale`).
    pub resolution_3d: u32,
    /// How the screens are filtered on their way to the window.
    pub filter: Filter,
    /// Whether the speed readout shows in the window's corner.
    pub show_fps: bool,
    /// What fills the window around the screens.
    pub backdrop: Backdrop,
    /// Whether GBC and GBA colours are corrected to look as on their
    /// screens.
    pub color_correction: bool,
    /// The save-state slot saving and loading use, from 1 to
    /// [`STATE_SLOTS`].
    pub state_slot: u8,
    /// How games made for the original Game Boy are shown, from the next
    /// game opened.
    pub dmg_colors: DmgColors,
    /// Whether the main window shows its close, minimise and zoom buttons
    /// and the bar they sit in.
    pub window_controls: bool,
    /// Whether the keys that play the game show along the foot of the
    /// window.
    pub key_guide: bool,
    /// Where the main window was left, if it has been placed.
    pub window_place: Option<WindowPlace>,
}

/// Where the main window was and how large, to open it there again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPlace {
    /// Left edge, in logical pixels across the desktop.
    pub x: f32,
    /// Top edge, in logical pixels down the desktop.
    pub y: f32,
    /// Width outside full screen.
    pub width: f32,
    /// Height outside full screen.
    pub height: f32,
    /// Whether the window was in full screen.
    pub fullscreen: bool,
    /// The display it was on, which `x` and `y` are measured across, by
    /// its UUID; `None` for the main display.
    pub display: Option<u128>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            layout: Layout::default(),
            speed: Speeds::default(),
            fast_forward_on: false,
            slow_motion_on: false,
            resolution_3d: 1,
            filter: Filter::None,
            show_fps: false,
            backdrop: Backdrop::Ambient,
            color_correction: true,
            state_slot: 1,
            dmg_colors: DmgColors::ByTitle,
            window_controls: true,
            key_guide: true,
            window_place: None,
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SettingsFile {
    layout: LayoutSection,
    speed: SpeedSection,
    video: VideoSection,
    game_boy: GameBoySection,
    states: StatesSection,
    window: WindowSection,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct StatesSection {
    slot: u8,
}

impl Default for StatesSection {
    fn default() -> Self {
        Self {
            slot: Settings::default().state_slot,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct WindowSection {
    controls: bool,
    key_guide: bool,
    fullscreen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display: Option<String>,
}

impl Default for WindowSection {
    fn default() -> Self {
        WindowSection::from(&Settings::default())
    }
}

impl From<&Settings> for WindowSection {
    fn from(settings: &Settings) -> Self {
        let place = settings.window_place;
        Self {
            controls: settings.window_controls,
            key_guide: settings.key_guide,
            fullscreen: place.is_some_and(|place| place.fullscreen),
            x: place.map(|place| place.x),
            y: place.map(|place| place.y),
            width: place.map(|place| place.width),
            height: place.map(|place| place.height),
            display: place
                .and_then(|place| place.display)
                .map(|uuid| format!("{uuid:032x}")),
        }
    }
}

impl WindowSection {
    /// The window's place, if the file gives all of it and its size is
    /// one a window can have. Anything less opens the window afresh.
    fn place(&self) -> Option<WindowPlace> {
        let place = WindowPlace {
            x: self.x?,
            y: self.y?,
            width: self.width?,
            height: self.height?,
            fullscreen: self.fullscreen,
            // A display not named, or not readably, is the main one.
            display: self
                .display
                .as_deref()
                .and_then(|uuid| u128::from_str_radix(uuid, 16).ok()),
        };
        let finite = [place.x, place.y, place.width, place.height]
            .iter()
            .all(|value| value.is_finite());
        (finite && place.width >= 1.0 && place.height >= 1.0).then_some(place)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct GameBoySection {
    dmg_colors: String,
}

impl Default for GameBoySection {
    fn default() -> Self {
        Self {
            dmg_colors: dmg_colors_name(Settings::default().dmg_colors),
        }
    }
}

/// The button combinations a Game Boy Color takes as it starts, in the
/// order of [`DmgColors::Buttons`], as the settings file names them.
const BUTTON_COMBINATIONS: [&str; 12] = [
    "right", "left", "up", "down", "right+a", "left+a", "up+a", "down+a", "right+b", "left+b",
    "up+b", "down+b",
];

/// `colors` as the settings file names it.
fn dmg_colors_name(colors: DmgColors) -> String {
    let name = match colors {
        DmgColors::Grey => "grey",
        DmgColors::ByTitle => "game-boy-color",
        DmgColors::Buttons(index) => BUTTON_COMBINATIONS
            .get(usize::from(index))
            .copied()
            .unwrap_or("game-boy-color"),
    };
    name.to_owned()
}

/// The colours a settings file's `name` stands for.
fn dmg_colors_from_name(name: &str) -> Option<DmgColors> {
    match name {
        "grey" => Some(DmgColors::Grey),
        "game-boy-color" => Some(DmgColors::ByTitle),
        _ => BUTTON_COMBINATIONS
            .iter()
            .position(|combination| *combination == name)
            .and_then(|index| u8::try_from(index).ok())
            .map(DmgColors::Buttons),
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct VideoSection {
    resolution_3d: u32,
    filter: String,
    show_fps: bool,
    backdrop: String,
    color_correction: bool,
}

impl Default for VideoSection {
    fn default() -> Self {
        VideoSection::from(&Settings::default())
    }
}

impl From<&Settings> for VideoSection {
    fn from(settings: &Settings) -> Self {
        Self {
            resolution_3d: settings.resolution_3d,
            filter: settings.filter.name().to_owned(),
            show_fps: settings.show_fps,
            backdrop: settings.backdrop.name().to_owned(),
            color_correction: settings.color_correction,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct LayoutSection {
    arrangement: String,
    swapped: bool,
    rotation: u16,
    gap: u16,
}

impl Default for LayoutSection {
    fn default() -> Self {
        LayoutSection::from(&Layout::default())
    }
}

impl From<&Layout> for LayoutSection {
    fn from(layout: &Layout) -> Self {
        let arrangement = ARRANGEMENTS
            .iter()
            .find(|(arrangement, _)| *arrangement == layout.arrangement)
            .map_or("vertical", |(_, name)| name);
        Self {
            arrangement: arrangement.to_owned(),
            swapped: layout.swapped,
            rotation: layout.rotation.degrees(),
            // Gaps are whole pixels; `as` saturates.
            gap: layout.gap.round() as u16,
        }
    }
}

impl LayoutSection {
    fn to_layout(&self) -> Result<Layout, SettingsError> {
        let arrangement = ARRANGEMENTS
            .iter()
            .find(|(_, name)| *name == self.arrangement)
            .map(|(arrangement, _)| *arrangement)
            .ok_or_else(|| SettingsError::UnknownArrangement(self.arrangement.clone()))?;
        let rotation = Rotation::from_degrees(self.rotation)
            .ok_or(SettingsError::UnsupportedRotation(self.rotation))?;
        Ok(Layout {
            arrangement,
            swapped: self.swapped,
            rotation,
            gap: f32::from(self.gap),
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SpeedSection {
    fast_forward: FastForward,
    slow_motion: f64,
    fast_forward_on: bool,
    slow_motion_on: bool,
}

/// The fast-forward speed as the file gives it: a multiple of normal
/// speed, or a word, of which [`UNLIMITED`] is the only one known.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
enum FastForward {
    Times(f64),
    Word(String),
}

/// The word in the file for fast-forward without a limit.
const UNLIMITED: &str = "unlimited";

impl Default for SpeedSection {
    fn default() -> Self {
        SpeedSection::from(&Settings::default())
    }
}

impl From<&Settings> for SpeedSection {
    fn from(settings: &Settings) -> Self {
        let speeds = settings.speed;
        Self {
            fast_forward: match speeds.fast_forward {
                Speed::Unlimited => FastForward::Word(UNLIMITED.to_owned()),
                Speed::Times(rate) => FastForward::Times(rate.get()),
            },
            slow_motion: speeds.slow_motion.get(),
            fast_forward_on: settings.fast_forward_on,
            slow_motion_on: settings.slow_motion_on,
        }
    }
}

impl SpeedSection {
    fn to_speeds(&self) -> Result<Speeds, SettingsError> {
        let rate =
            |key, value| Rate::new(value).ok_or(SettingsError::UnsupportedSpeed { key, value });
        Ok(Speeds {
            fast_forward: match &self.fast_forward {
                FastForward::Times(value) => Speed::Times(rate("fast_forward", *value)?),
                FastForward::Word(word) if word == UNLIMITED => Speed::Unlimited,
                FastForward::Word(word) => return Err(SettingsError::UnknownSpeed(word.clone())),
            },
            slow_motion: rate("slow_motion", self.slow_motion)?,
        })
    }
}

impl Settings {
    /// The settings in `text`, with defaults for any it leaves out.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError`] if the TOML is malformed or holds a value
    /// the app does not know.
    pub fn from_toml(text: &str) -> Result<Self, SettingsError> {
        let file: SettingsFile =
            toml::from_str(text).map_err(|error| SettingsError::Syntax(error.to_string()))?;
        let resolution_3d = file.video.resolution_3d;
        if !(1..=MAX_RESOLUTION_3D).contains(&resolution_3d) {
            return Err(SettingsError::UnsupportedResolution(resolution_3d));
        }
        let filter = Filter::from_name(&file.video.filter)
            .ok_or_else(|| SettingsError::UnknownFilter(file.video.filter.clone()))?;
        let backdrop = Backdrop::from_name(&file.video.backdrop)
            .ok_or_else(|| SettingsError::UnknownBackdrop(file.video.backdrop.clone()))?;
        let dmg_colors = dmg_colors_from_name(&file.game_boy.dmg_colors)
            .ok_or_else(|| SettingsError::UnknownDmgColors(file.game_boy.dmg_colors.clone()))?;
        let state_slot = file.states.slot;
        if !(1..=STATE_SLOTS).contains(&state_slot) {
            return Err(SettingsError::UnknownStateSlot(state_slot));
        }
        Ok(Self {
            layout: file.layout.to_layout()?,
            speed: file.speed.to_speeds()?,
            fast_forward_on: file.speed.fast_forward_on,
            slow_motion_on: file.speed.slow_motion_on,
            resolution_3d,
            filter,
            show_fps: file.video.show_fps,
            backdrop,
            color_correction: file.video.color_correction,
            state_slot,
            dmg_colors,
            window_controls: file.window.controls,
            key_guide: file.window.key_guide,
            window_place: file.window.place(),
        })
    }

    /// The settings as the TOML [`Settings::from_toml`] reads.
    pub fn to_toml(self) -> String {
        let file = SettingsFile {
            layout: LayoutSection::from(&self.layout),
            speed: SpeedSection::from(&self),
            video: VideoSection::from(&self),
            game_boy: GameBoySection {
                dmg_colors: dmg_colors_name(self.dmg_colors),
            },
            states: StatesSection {
                slot: self.state_slot,
            },
            window: WindowSection::from(&self),
        };
        let body = toml::to_string(&file).expect("plain tables and values always serialize");
        format!("# Saved by ds-emulator whenever a setting changes.\n\n{body}")
    }

    /// Reads settings from `path`, or the defaults if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError`] if the file exists but cannot be read or
    /// parsed.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(SettingsError::Io(error.to_string())),
        }
    }

    /// Writes the settings to `path`, creating its folder if needed.
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

/// Where the settings file lives, next to the bindings, e.g.
/// `~/Library/Application Support/ds-emulator/settings.toml` on macOS.
pub fn default_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("ds-emulator")
            .join("settings.toml"),
    )
}

/// Why a settings file was rejected.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SettingsError {
    /// The file could not be read.
    #[error("cannot read settings: {0}")]
    Io(String),
    /// The file is not valid TOML of the expected shape.
    #[error("invalid settings file: {0}")]
    Syntax(String),
    /// The arrangement is not one of the four names.
    #[error("unknown screen arrangement {0:?} in settings")]
    UnknownArrangement(String),
    /// The rotation is not 0, 90, 180 or 270 degrees.
    #[error("unsupported rotation {0}° in settings")]
    UnsupportedRotation(u16),
    /// The filter is not one of the five names.
    #[error("unknown video filter {0:?} in settings")]
    UnknownFilter(String),
    /// The backdrop is not "solid" or "ambient".
    #[error("unknown backdrop {0:?} in settings")]
    UnknownBackdrop(String),
    /// The colours for original Game Boy games are not one of the names.
    #[error("unknown Game Boy colours {0:?} in settings")]
    UnknownDmgColors(String),
    /// The save-state slot is not 1 to [`STATE_SLOTS`].
    #[error("unknown save-state slot {0} in settings; use 1 to {max}", max = STATE_SLOTS)]
    UnknownStateSlot(u8),
    /// The 3D resolution is not 1 to [`MAX_RESOLUTION_3D`] times.
    #[error("unsupported 3D resolution {0}x in settings; use 1 to {max}", max = MAX_RESOLUTION_3D)]
    UnsupportedResolution(u32),
    /// The fast-forward speed is a word other than "unlimited".
    #[error("unknown fast-forward speed {0:?} in settings; use a number or \"unlimited\"")]
    UnknownSpeed(String),
    /// A speed is not a finite number of at least [`MIN_RATE`].
    #[error("unsupported speed {key} = {value} in settings; use {min} or more", min = MIN_RATE)]
    UnsupportedSpeed {
        /// The setting's name.
        key: &'static str,
        /// What the file gave.
        value: f64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_round_trip_through_toml() {
        let settings = Settings {
            layout: Layout {
                arrangement: Arrangement::Hybrid,
                swapped: true,
                rotation: Rotation::Anticlockwise,
                gap: 90.0,
            },
            speed: Speeds {
                fast_forward: Speed::Times(Rate::new(3.0).expect("valid rate")),
                slow_motion: Rate::new(0.25).expect("valid rate"),
            },
            fast_forward_on: true,
            slow_motion_on: true,
            resolution_3d: 4,
            filter: Filter::Scanlines,
            show_fps: true,
            backdrop: Backdrop::Solid,
            color_correction: false,
            state_slot: 3,
            dmg_colors: DmgColors::Buttons(9),
            window_controls: false,
            key_guide: false,
            window_place: Some(WindowPlace {
                x: -1200.0,
                y: 40.5,
                width: 640.0,
                height: 980.0,
                fullscreen: true,
                display: Some(0x37d8_832a_2d66_02ca_b9f7_8f6a_8a6b_1fd0),
            }),
        };
        let text = settings.to_toml();
        assert!(text.contains("arrangement = \"hybrid\""), "{text}");
        assert!(text.contains("resolution_3d = 4"), "{text}");
        assert!(text.contains("filter = \"scanlines\""), "{text}");
        assert!(text.contains("show_fps = true"), "{text}");
        assert!(text.contains("backdrop = \"solid\""), "{text}");
        assert!(text.contains("dmg_colors = \"left+b\""), "{text}");
        assert!(text.contains("rotation = 270"), "{text}");
        assert!(text.contains("fast_forward = 3.0"), "{text}");
        assert!(text.contains("controls = false"), "{text}");
        assert!(text.contains("key_guide = false"), "{text}");
        assert!(text.contains("fast_forward_on = true"), "{text}");
        assert!(text.contains("color_correction = false"), "{text}");
        assert!(text.contains("slot = 3"), "{text}");
        assert!(text.contains("fullscreen = true"), "{text}");
        assert!(text.contains("x = -1200.0"), "{text}");
        assert!(
            text.contains("display = \"37d8832a2d6602cab9f78f6a8a6b1fd0\""),
            "{text}"
        );
        assert_eq!(Settings::from_toml(&text), Ok(settings));
    }

    #[test]
    fn game_boy_colours_are_named_in_the_file() {
        let grey = Settings::from_toml("[game_boy]\ndmg_colors = \"grey\"\n");
        assert_eq!(
            grey.map(|settings| settings.dmg_colors),
            Ok(DmgColors::Grey)
        );
        let colour = Settings::from_toml("[game_boy]\ndmg_colors = \"game-boy-color\"\n");
        assert_eq!(
            colour.map(|settings| settings.dmg_colors),
            Ok(DmgColors::ByTitle)
        );
        let buttons = Settings::from_toml("[game_boy]\ndmg_colors = \"down+b\"\n");
        assert_eq!(
            buttons.map(|settings| settings.dmg_colors),
            Ok(DmgColors::Buttons(11))
        );
        assert_eq!(
            Settings::from_toml("[game_boy]\ndmg_colors = \"sepia\"\n"),
            Err(SettingsError::UnknownDmgColors("sepia".to_owned()))
        );
        assert_eq!(Settings::default().dmg_colors, DmgColors::ByTitle);
    }

    #[test]
    fn fast_forward_is_a_multiple_or_unlimited() {
        let text = Settings::default().to_toml();
        assert!(text.contains("fast_forward = 2.0"), "{text}");
        assert!(text.contains("slow_motion = 0.5"), "{text}");
        assert_eq!(Settings::from_toml(&text), Ok(Settings::default()));

        let unlimited = Settings {
            speed: Speeds {
                fast_forward: Speed::Unlimited,
                ..Speeds::default()
            },
            ..Settings::default()
        };
        let text = unlimited.to_toml();
        assert!(text.contains("fast_forward = \"unlimited\""), "{text}");
        assert_eq!(Settings::from_toml(&text), Ok(unlimited));
        assert_eq!(
            Settings::from_toml("[speed]\nfast_forward = \"ludicrous\"\n"),
            Err(SettingsError::UnknownSpeed("ludicrous".to_owned()))
        );
    }

    #[test]
    fn files_without_a_fast_forward_speed_get_2x() {
        // Before 2x was the default, leaving the speed out meant no limit.
        let settings = Settings::from_toml("[speed]\nslow_motion = 0.5\n").expect("valid file");
        assert_eq!(settings.speed.fast_forward.label(), "2×");
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn missing_settings_keep_their_defaults() {
        let settings = Settings::from_toml("[layout]\ngap = 16\n").expect("valid file");
        assert_eq!(
            settings.layout,
            Layout {
                gap: 16.0,
                ..Layout::default()
            }
        );
        assert_eq!(Settings::from_toml(""), Ok(Settings::default()));

        // Whole numbers are speeds too.
        let settings = Settings::from_toml("[speed]\nfast_forward = 4\n").expect("valid file");
        assert_eq!(
            settings.speed,
            Speeds {
                fast_forward: Speed::Times(Rate::new(4.0).expect("valid rate")),
                ..Speeds::default()
            }
        );
    }

    #[test]
    fn rejects_unknown_values_and_fields() {
        assert_eq!(
            Settings::from_toml("[layout]\nrotation = 45\n"),
            Err(SettingsError::UnsupportedRotation(45))
        );
        assert_eq!(
            Settings::from_toml("[layout]\narrangement = \"diagonal\"\n"),
            Err(SettingsError::UnknownArrangement("diagonal".to_owned()))
        );
        assert!(matches!(
            Settings::from_toml("[layout]\nzoom = 2\n"),
            Err(SettingsError::Syntax(_))
        ));
        assert!(matches!(
            Settings::from_toml("[speed]\nturbo = 2\n"),
            Err(SettingsError::Syntax(_))
        ));
    }

    #[test]
    fn the_3d_resolution_is_1_to_8_times() {
        let settings = Settings::from_toml("[video]\nresolution_3d = 3\n").expect("valid file");
        assert_eq!(settings.resolution_3d, 3);
        assert_eq!(Settings::from_toml("").map(|s| s.resolution_3d), Ok(1));
        for wrong in [0, 9] {
            assert_eq!(
                Settings::from_toml(&format!("[video]\nresolution_3d = {wrong}\n")),
                Err(SettingsError::UnsupportedResolution(wrong))
            );
        }
    }

    #[test]
    fn the_filter_is_one_of_five_names() {
        let settings = Settings::from_toml("[video]\nfilter = \"lcd-grid\"\n").expect("valid file");
        assert_eq!(settings.filter, Filter::LcdGrid);
        assert_eq!(Settings::from_toml("").map(|s| s.filter), Ok(Filter::None));
        assert_eq!(
            Settings::from_toml("[video]\nfilter = \"xbrz\"\n"),
            Err(SettingsError::UnknownFilter("xbrz".to_owned()))
        );
    }

    #[test]
    fn the_backdrop_is_ambient_unless_the_file_says_solid() {
        assert_eq!(
            Settings::from_toml("").map(|s| s.backdrop),
            Ok(Backdrop::Ambient)
        );
        let solid = Settings::from_toml("[video]\nbackdrop = \"solid\"\n");
        assert_eq!(solid.map(|s| s.backdrop), Ok(Backdrop::Solid));
        assert_eq!(
            Settings::from_toml("[video]\nbackdrop = \"boxart\"\n"),
            Err(SettingsError::UnknownBackdrop("boxart".to_owned()))
        );
    }

    #[test]
    fn rejects_speeds_that_are_not_positive_and_finite() {
        for (text, key) in [
            ("fast_forward = 0", "fast_forward"),
            ("fast_forward = -2.0", "fast_forward"),
            ("fast_forward = inf", "fast_forward"),
            ("slow_motion = 0.0", "slow_motion"),
            ("slow_motion = nan", "slow_motion"),
            ("slow_motion = 0.01", "slow_motion"),
        ] {
            let result = Settings::from_toml(&format!("[speed]\n{text}\n"));
            assert!(
                matches!(result, Err(SettingsError::UnsupportedSpeed { key: k, .. }) if k == key),
                "{text}: {result:?}"
            );
        }
    }

    #[test]
    fn a_window_never_placed_is_left_out_of_the_file() {
        let text = Settings::default().to_toml();
        assert!(!text.contains("width"), "{text}");
        assert_eq!(Settings::from_toml(&text), Ok(Settings::default()));
    }

    #[test]
    fn a_window_place_needs_all_of_it_and_a_real_size() {
        let place = |text: &str| Settings::from_toml(text).map(|settings| settings.window_place);
        assert_eq!(
            place("[window]\nx = 10.0\ny = 20.0\nwidth = 300.0\n"),
            Ok(None)
        );
        assert_eq!(
            place("[window]\nx = 10.0\ny = 20.0\nwidth = 0.0\nheight = 300.0\n"),
            Ok(None)
        );
        assert_eq!(
            place("[window]\nx = 10.0\ny = 20.0\nwidth = 300.0\nheight = 400.0\n"),
            Ok(Some(WindowPlace {
                x: 10.0,
                y: 20.0,
                width: 300.0,
                height: 400.0,
                fullscreen: false,
                display: None,
            }))
        );
    }

    #[test]
    fn state_slots_are_1_to_4() {
        assert_eq!(
            Settings::from_toml("[states]\nslot = 4\n").map(|settings| settings.state_slot),
            Ok(4)
        );
        assert_eq!(
            Settings::from_toml("[states]\nslot = 5\n"),
            Err(SettingsError::UnknownStateSlot(5))
        );
        assert_eq!(
            Settings::from_toml("[states]\nslot = 0\n"),
            Err(SettingsError::UnknownStateSlot(0))
        );
    }

    #[test]
    fn missing_file_means_defaults() {
        let path = Path::new("/nonexistent/ds-emulator/settings.toml");
        assert_eq!(Settings::load(path), Ok(Settings::default()));
    }

    #[test]
    fn save_creates_the_folder_and_load_reads_it_back() {
        let dir = std::env::temp_dir().join(format!("emu-app-settings-{}", std::process::id()));
        let path = dir.join("ds-emulator").join("settings.toml");
        let settings = Settings {
            layout: Layout {
                rotation: Rotation::Clockwise,
                ..Layout::default()
            },
            ..Settings::default()
        };
        settings.save(&path).expect("save");
        assert_eq!(Settings::load(&path), Ok(settings));
        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
