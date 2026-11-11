//! The supporter's key in the app
//! ([decision 0017](../../../docs/decisions/0017-supporter-keys.md)).
//!
//! The app is free and whole for everyone. Someone who pays to support it
//! gets a key (`emu-supporter`), pastes it in Settings, and can then
//! choose an accent and a Dock icon: looks, and nothing a game needs. The
//! key is kept as a file beside the settings, `supporter.key`, and read
//! again at each launch, without the network.
//!
//! The choices themselves are settings ([`Settings::accent`] and
//! [`Settings::app_icon`]), and [`look`] says what shows: the choices
//! while a key is in, the app's own look otherwise.
//!
//! A key has a [`Tier`]. Every tier gets every accent and the first five
//! icons; the higher two each add an icon of their own ([`AppIcon::tier`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use emu_supporter::{KeyError, Supporter, Tier};
use gpui::{App, Global};

use crate::settings::Settings;
use crate::theme::Accent;

/// Where a supporter's key is bought and explained.
pub const PAGE: &str = "https://imagoemulator.com/supporter";

/// The Dock icons a supporter chooses between (Settings > App Icon): the
/// app's icon in other colours, drawn by `scripts/make-app-icons.sh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppIcon {
    /// The app's own icon, which everyone has.
    #[default]
    Classic,
    /// Its colours on a dark body.
    Night,
    /// Inks on white.
    Ink,
    /// Greys on a dark body.
    Chalk,
    /// Whites and blues on Imago's blue.
    Blueprint,
    /// Whites on the icon's coral: a Patron's.
    Ember,
    /// Golds on a dark body: a Founder's.
    Founder,
}

impl AppIcon {
    /// Every icon, in the order Settings offers them.
    pub const ALL: [Self; 7] = [
        Self::Classic,
        Self::Night,
        Self::Ink,
        Self::Chalk,
        Self::Blueprint,
        Self::Ember,
        Self::Founder,
    ];

    /// The tier a supporter needs for it: any, but for the two icons
    /// that are a higher tier's own.
    pub fn tier(self) -> Tier {
        match self {
            Self::Ember => Tier::Patron,
            Self::Founder => Tier::Founder,
            _ => Tier::Supporter,
        }
    }

    /// The icons a supporter of `tier` chooses between, in order.
    pub fn offered(tier: Tier) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|icon| icon.tier() <= tier)
            .collect()
    }

    /// What Settings calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::Night => "Night",
            Self::Ink => "Ink",
            Self::Chalk => "Chalk",
            Self::Blueprint => "Blueprint",
            Self::Ember => "Ember",
            Self::Founder => "Founder",
        }
    }

    /// What the settings file calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Night => "night",
            Self::Ink => "ink",
            Self::Chalk => "chalk",
            Self::Blueprint => "blueprint",
            Self::Ember => "ember",
            Self::Founder => "founder",
        }
    }

    /// The icon the settings file calls `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|icon| icon.name() == name)
    }

    /// The icon as a PNG file's bytes.
    pub fn png(self) -> &'static [u8] {
        match self {
            Self::Classic => crate::app::APP_ICON,
            Self::Night => include_bytes!("../../../assets/app-icons/night.png"),
            Self::Ink => include_bytes!("../../../assets/app-icons/ink.png"),
            Self::Chalk => include_bytes!("../../../assets/app-icons/chalk.png"),
            Self::Blueprint => include_bytes!("../../../assets/app-icons/blueprint.png"),
            Self::Ember => include_bytes!("../../../assets/app-icons/ember.png"),
            Self::Founder => include_bytes!("../../../assets/app-icons/founder.png"),
        }
    }
}

/// Whether the app can show a chosen icon: in the Dock, which only macOS
/// has. Elsewhere a program's icon is the one in its file or its package.
pub const CHOOSES_ICON: bool = cfg!(target_os = "macos");

/// The accent and the Dock icon to show for `settings`: what they choose
/// for a supporter of `tier`, and the app's own without a key, whatever
/// the file says. An icon that is a higher tier's shows as the app's own.
pub fn look(settings: &Settings, tier: Option<Tier>) -> (Accent, AppIcon) {
    match tier {
        Some(tier) if settings.app_icon.tier() <= tier => (settings.accent, settings.app_icon),
        Some(_) => (settings.accent, AppIcon::default()),
        None => (Accent::default(), AppIcon::default()),
    }
}

/// The app's supporter, once a key is in.
#[derive(Default)]
struct Shared(Option<Supporter>);

impl Global for Shared {}

/// Where the key is kept, beside the settings, e.g.
/// `~/Library/Application Support/ds-emulator/supporter.key` on macOS.
pub fn default_path() -> Option<PathBuf> {
    Some(
        emu_os::config_dir()?
            .join("ds-emulator")
            .join("supporter.key"),
    )
}

/// Reads the key kept from an earlier launch, if there is a good one.
pub fn load(cx: &mut App) {
    let supporter = default_path().and_then(|path| kept(&path, &emu_supporter::PUBLIC_KEYS));
    cx.set_global(Shared(supporter));
}

/// The supporter whose key the app holds.
pub fn current(cx: &App) -> Option<&Supporter> {
    cx.try_global::<Shared>()?.0.as_ref()
}

/// The tier of the key the app holds, if it holds one.
pub fn tier(cx: &App) -> Option<Tier> {
    current(cx).map(|supporter| supporter.tier)
}

/// Why a key was not taken.
#[derive(Debug, thiserror::Error)]
pub enum EnterError {
    /// It is not a key that counts.
    #[error(transparent)]
    Key(#[from] KeyError),
    /// It is good, and could not be kept for the next launch.
    #[error("cannot keep the key: {0}")]
    Io(#[from] io::Error),
}

/// Takes `text` as the app's key, if it is a good one, and keeps it for
/// later launches.
///
/// # Errors
///
/// Returns [`EnterError`] if it is not a good key or cannot be kept. The
/// key the app already holds, if any, stays.
pub fn enter(text: &str, cx: &mut App) -> Result<(), EnterError> {
    let supporter = match default_path() {
        Some(path) => keep(&path, text, &emu_supporter::PUBLIC_KEYS)?,
        // Nowhere to keep it: good until the app quits.
        None => emu_supporter::read(text)?,
    };
    cx.set_global(Shared(Some(supporter)));
    Ok(())
}

/// Forgets the app's key.
///
/// # Errors
///
/// Returns the I/O error if the key's file cannot be removed; the app
/// then still holds the key.
pub fn remove(cx: &mut App) -> io::Result<()> {
    if let Some(path) = default_path() {
        forget(&path)?;
    }
    cx.set_global(Shared(None));
    Ok(())
}

/// What the key in the file at `path` says, if the file holds a key that
/// any of `trusted` signed and that was not taken back.
fn kept(path: &Path, trusted: &[[u8; 32]]) -> Option<Supporter> {
    let text = fs::read_to_string(path).ok()?;
    counted(&text, trusted).ok()
}

/// What `text` says, if it is a key that counts.
fn counted(text: &str, trusted: &[[u8; 32]]) -> Result<Supporter, KeyError> {
    let supporter = emu_supporter::read_with(trusted, text)?;
    if emu_supporter::REVOKED.contains(&supporter.id) {
        return Err(KeyError::Revoked);
    }
    Ok(supporter)
}

/// Writes `text` to `path` if it is a key that counts, without the spaces
/// and line breaks it was pasted with, and says what it says.
fn keep(path: &Path, text: &str, trusted: &[[u8; 32]]) -> Result<Supporter, EnterError> {
    let supporter = counted(text, trusted)?;
    let key: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    crate::persist::write_atomic(path, format!("{key}\n").as_bytes())?;
    Ok(supporter)
}

/// Removes the key's file at `path`, if there is one.
fn forget(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signing key for the tests alone.
    const SEED: [u8; 32] = [3; 32];

    fn trusted() -> [[u8; 32]; 1] {
        [emu_supporter::public_key(&SEED)]
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-app-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn a_good_key_is_kept_for_the_next_launch_and_a_bad_one_is_not() {
        let dir = temp_dir("supporter-keep");
        let path = dir.join("ds-emulator").join("supporter.key");
        assert_eq!(kept(&path, &trusted()), None, "no file yet");
        let key = emu_supporter::issue(&SEED, Tier::Patron, 5, 20_768, "Ada").expect("issued");
        // Pasted as an email wrapped it.
        let pasted = format!(" {}\n{} \n", &key[..30], &key[30..]);
        let supporter = keep(&path, &pasted, &trusted()).expect("kept");
        assert_eq!(
            (supporter.name.as_str(), supporter.tier),
            ("Ada", Tier::Patron)
        );
        assert_eq!(
            fs::read_to_string(&path).expect("file"),
            format!("{key}\n"),
            "kept on one line"
        );
        assert_eq!(kept(&path, &trusted()), Some(supporter.clone()));

        // What is not a key leaves the one kept.
        let forged =
            emu_supporter::issue(&[4; 32], Tier::Founder, 5, 20_768, "Eve").expect("issued");
        for bad in ["", "hello", &key[..40], &forged] {
            assert!(
                matches!(keep(&path, bad, &trusted()), Err(EnterError::Key(_))),
                "{bad:?}"
            );
            assert_eq!(kept(&path, &trusted()), Some(supporter.clone()));
        }
        // A file changed by hand counts for nothing.
        fs::write(&path, &forged).expect("written");
        assert_eq!(kept(&path, &trusted()), None);

        forget(&path).expect("forgotten");
        assert_eq!(kept(&path, &trusted()), None);
        forget(&path).expect("nothing to forget is no error");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_key_that_cannot_be_kept_says_so() {
        let dir = temp_dir("supporter-unwritable");
        fs::create_dir_all(&dir).expect("folder");
        // A file where the key's folder would go.
        fs::write(dir.join("ds-emulator"), "").expect("file");
        let key = emu_supporter::issue(&SEED, Tier::Supporter, 5, 20_768, "").expect("issued");
        let path = dir.join("ds-emulator").join("supporter.key");
        assert!(matches!(
            keep(&path, &key, &trusted()),
            Err(EnterError::Io(_))
        ));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_look_is_the_supporters_choice_and_the_apps_own_without_a_key() {
        let chosen = Settings {
            accent: Accent::Violet,
            app_icon: AppIcon::Night,
            ..Settings::default()
        };
        for tier in Tier::ALL {
            assert_eq!(look(&chosen, Some(tier)), (Accent::Violet, AppIcon::Night));
        }
        // The file may say anything: without a key it shows nothing.
        assert_eq!(look(&chosen, None), (Accent::Blue, AppIcon::Classic));
        assert_eq!(
            look(&Settings::default(), Some(Tier::Founder)),
            (Accent::Blue, AppIcon::Classic)
        );
    }

    #[test]
    fn a_higher_tiers_icon_shows_only_for_that_tier_and_above() {
        let founders = Settings {
            accent: Accent::Teal,
            app_icon: AppIcon::Founder,
            ..Settings::default()
        };
        // The accent is every supporter's; the icon is not.
        assert_eq!(
            look(&founders, Some(Tier::Supporter)),
            (Accent::Teal, AppIcon::Classic)
        );
        assert_eq!(
            look(&founders, Some(Tier::Patron)),
            (Accent::Teal, AppIcon::Classic)
        );
        assert_eq!(
            look(&founders, Some(Tier::Founder)),
            (Accent::Teal, AppIcon::Founder)
        );
        // Each tier is offered what the one before is, and its own after.
        let supporter = AppIcon::offered(Tier::Supporter);
        let patron = AppIcon::offered(Tier::Patron);
        let founder = AppIcon::offered(Tier::Founder);
        assert_eq!(supporter.len(), 5);
        assert_eq!(patron[..5], supporter[..]);
        assert_eq!(patron[5..], [AppIcon::Ember]);
        assert_eq!(founder[..6], patron[..]);
        assert_eq!(founder[6..], [AppIcon::Founder]);
        assert_eq!(founder, AppIcon::ALL);
    }

    #[test]
    fn icons_are_named_in_the_file_and_each_is_a_picture_of_its_own() {
        assert_eq!(AppIcon::default(), AppIcon::Classic);
        for (at, icon) in AppIcon::ALL.into_iter().enumerate() {
            assert_eq!(AppIcon::from_name(icon.name()), Some(icon));
            let png = icon.png();
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "{}", icon.label());
            // 1024 pixels a side, as the Dock is given the app's own.
            assert_eq!(&png[16..24], [0, 0, 4, 0, 0, 0, 4, 0], "{}", icon.label());
            for other in &AppIcon::ALL[at + 1..] {
                assert_ne!(png, other.png(), "{} and {}", icon.label(), other.label());
            }
        }
        assert_eq!(AppIcon::from_name("Night"), None);
        assert_eq!(AppIcon::from_name("gold"), None);
    }

    #[test]
    fn the_page_is_a_web_address() {
        assert!(crate::links::is_web_url(PAGE));
    }
}
