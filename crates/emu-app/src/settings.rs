//! Settings kept between launches: the screen layout, the speeds of
//! fast-forward and slow motion, and the resolution of DS 3D.
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
//! fast_forward = 4.0      # times normal speed; leave out for no limit
//! slow_motion = 0.5       # times normal speed
//!
//! [video]
//! resolution_3d = 2       # DS 3D at 1 to 8 times its own resolution
//! ```
//!
//! Settings missing from the file keep their defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::{Arrangement, Layout, Rotation};
use crate::persist;
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
    /// How many times its own resolution DS 3D is drawn at, 1 to
    /// [`MAX_RESOLUTION_3D`] (see `Core::set_render_scale`).
    pub resolution_3d: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            layout: Layout::default(),
            speed: Speeds::default(),
            resolution_3d: 1,
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct SettingsFile {
    layout: LayoutSection,
    speed: SpeedSection,
    video: VideoSection,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct VideoSection {
    resolution_3d: u32,
}

impl Default for VideoSection {
    fn default() -> Self {
        Self {
            resolution_3d: Settings::default().resolution_3d,
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
    /// Left out for no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    fast_forward: Option<f64>,
    slow_motion: f64,
}

impl Default for SpeedSection {
    fn default() -> Self {
        SpeedSection::from(&Speeds::default())
    }
}

impl From<&Speeds> for SpeedSection {
    fn from(speeds: &Speeds) -> Self {
        Self {
            fast_forward: match speeds.fast_forward {
                Speed::Unlimited => None,
                Speed::Times(rate) => Some(rate.get()),
            },
            slow_motion: speeds.slow_motion.get(),
        }
    }
}

impl SpeedSection {
    fn to_speeds(&self) -> Result<Speeds, SettingsError> {
        let rate =
            |key, value| Rate::new(value).ok_or(SettingsError::UnsupportedSpeed { key, value });
        Ok(Speeds {
            fast_forward: match self.fast_forward {
                Some(value) => Speed::Times(rate("fast_forward", value)?),
                None => Speed::Unlimited,
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
        Ok(Self {
            layout: file.layout.to_layout()?,
            speed: file.speed.to_speeds()?,
            resolution_3d,
        })
    }

    /// The settings as the TOML [`Settings::from_toml`] reads.
    pub fn to_toml(self) -> String {
        let file = SettingsFile {
            layout: LayoutSection::from(&self.layout),
            speed: SpeedSection::from(&self.speed),
            video: VideoSection {
                resolution_3d: self.resolution_3d,
            },
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
    /// The 3D resolution is not 1 to [`MAX_RESOLUTION_3D`] times.
    #[error("unsupported 3D resolution {0}x in settings; use 1 to {max}", max = MAX_RESOLUTION_3D)]
    UnsupportedResolution(u32),
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
            resolution_3d: 4,
        };
        let text = settings.to_toml();
        assert!(text.contains("arrangement = \"hybrid\""), "{text}");
        assert!(text.contains("resolution_3d = 4"), "{text}");
        assert!(text.contains("rotation = 270"), "{text}");
        assert!(text.contains("fast_forward = 3.0"), "{text}");
        assert_eq!(Settings::from_toml(&text), Ok(settings));
    }

    #[test]
    fn unlimited_fast_forward_is_left_out() {
        let text = Settings::default().to_toml();
        assert!(!text.contains("fast_forward"), "{text}");
        assert!(text.contains("slow_motion = 0.5"), "{text}");
        assert_eq!(Settings::from_toml(&text), Ok(Settings::default()));
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
