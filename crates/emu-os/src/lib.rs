//! The folders the app keeps its files in, on each system. Each function
//! gives the system's folder of that kind, which the app's own folder,
//! `ds-emulator`, goes in, or `None` if there is no home folder.
//!
//! | | macOS | Linux | Windows |
//! |---|---|---|---|
//! | [`data_dir`] | `~/Library/Application Support` | `$XDG_DATA_HOME`, or `~/.local/share` | `%LOCALAPPDATA%` |
//! | [`config_dir`] | `~/Library/Application Support` | `$XDG_CONFIG_HOME`, or `~/.config` | `%APPDATA%` |
//! | [`cache_dir`] | `~/Library/Caches` | `$XDG_CACHE_HOME`, or `~/.cache` | `%LOCALAPPDATA%` |
//!
//! The data is the games unpacked from archives, the 3DS core and its
//! console's files, and the like: on Windows it stays in the local folder,
//! as it is far too big to roam with the user.
//!
//! [`HOME_VARIABLE`] (`DS_EMULATOR_HOME`) stands in for the home folder,
//! so that a test run can keep to a home of its own: each folder is then
//! where the system puts it in a home by default, whatever `XDG_*` says.
//! Setting `HOME` does that on macOS and Linux, but Windows finds its
//! folders without it.
//!
//! [`process`] runs other programs alike on every system, [`fetch`]
//! downloads through `curl`, [`file`] writes files whole or not at all,
//! [`lock`] holds a lock file, and [`ThreadClass`] passes a thread's
//! scheduling class to the threads that work for it.

use std::path::{Path, PathBuf};

pub mod fetch;
pub mod file;
pub mod lock;
pub mod process;
#[allow(unsafe_code)]
mod thread_class;

pub use thread_class::ThreadClass;

/// The environment variable that stands in for the home folder.
pub const HOME_VARIABLE: &str = "DS_EMULATOR_HOME";

/// The folder for the app's data: games, downloads and the files the cores
/// keep.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    Folder::Data.path(home().as_deref())
}

/// The folder for the app's settings: small files the user may want on
/// every computer they use.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    Folder::Config.path(home().as_deref())
}

/// The folder for what the app can download or make again: box art,
/// databases and pictures.
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    Folder::Cache.path(home().as_deref())
}

/// The home folder [`HOME_VARIABLE`] names, if it is set and not empty.
fn home() -> Option<PathBuf> {
    std::env::var_os(HOME_VARIABLE)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// A kind of folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Folder {
    Data,
    Config,
    Cache,
}

impl Folder {
    /// This folder in `home`, or the system's own without one.
    fn path(self, home: Option<&Path>) -> Option<PathBuf> {
        match home {
            Some(home) => Some(
                self.in_home()
                    .iter()
                    .fold(home.to_owned(), |path, part| path.join(part)),
            ),
            None => match self {
                Self::Data => dirs::data_local_dir(),
                Self::Config => dirs::config_dir(),
                Self::Cache => dirs::cache_dir(),
            },
        }
    }

    /// Where the system puts this folder in a home by default, folder by
    /// folder.
    fn in_home(self) -> &'static [&'static str] {
        if cfg!(target_os = "macos") {
            match self {
                Self::Data | Self::Config => &["Library", "Application Support"],
                Self::Cache => &["Library", "Caches"],
            }
        } else if cfg!(windows) {
            match self {
                Self::Data | Self::Cache => &["AppData", "Local"],
                Self::Config => &["AppData", "Roaming"],
            }
        } else {
            match self {
                Self::Data => &[".local", "share"],
                Self::Config => &[".config"],
                Self::Cache => &[".cache"],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    const KINDS: [Folder; 3] = [Folder::Data, Folder::Config, Folder::Cache];

    /// `path` as it is on this system, for comparing paths exactly: `Path`
    /// equality would let `a/b` and `a//b` pass.
    fn bytes(path: &Path) -> OsString {
        path.as_os_str().to_owned()
    }

    #[test]
    fn the_folders_are_the_ones_the_app_always_used() {
        // The app asked `dirs` for its data folder before, which is the
        // local one everywhere but Windows.
        let data = if cfg!(windows) {
            dirs::data_local_dir()
        } else {
            dirs::data_dir()
        };
        assert_eq!(Folder::Data.path(None), data);
        assert_eq!(Folder::Config.path(None), dirs::config_dir());
        assert_eq!(Folder::Cache.path(None), dirs::cache_dir());
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "macOS's own folders")]
    fn on_macos_every_folder_is_byte_for_byte_where_it_was() {
        let home = dirs::home_dir().expect("a home folder");
        let home = home.to_str().expect("a UTF-8 home");
        let expected = [
            format!("{home}/Library/Application Support"),
            format!("{home}/Library/Application Support"),
            format!("{home}/Library/Caches"),
        ];
        for (kind, expected) in KINDS.into_iter().zip(expected) {
            let found = kind.path(None).expect("a folder");
            assert_eq!(bytes(&found), OsString::from(&expected), "{kind:?}");
        }
        // emu-headless and Guide's example joined their folders to this
        // home by hand.
        let by_hand = std::env::home_dir().expect("a home folder");
        assert_eq!(bytes(&by_hand), OsString::from(home));
    }

    #[test]
    fn a_home_of_its_own_holds_the_folders_where_the_system_puts_them() {
        let home = Path::new("test-home");
        let expected: [&[&str]; 3] = if cfg!(target_os = "macos") {
            [
                &["Library", "Application Support"],
                &["Library", "Application Support"],
                &["Library", "Caches"],
            ]
        } else if cfg!(windows) {
            [
                &["AppData", "Local"],
                &["AppData", "Roaming"],
                &["AppData", "Local"],
            ]
        } else {
            [&[".local", "share"], &[".config"], &[".cache"]]
        };
        for (kind, parts) in KINDS.into_iter().zip(expected) {
            let mut path = home.to_owned();
            path.extend(parts);
            assert_eq!(kind.path(Some(home)).map(|p| bytes(&p)), Some(bytes(&path)));
        }
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows's folders don't follow HOME")]
    fn at_the_real_home_a_home_of_its_own_is_the_system_itself() {
        let real = dirs::home_dir().expect("a home folder");
        for kind in KINDS {
            let variable = match kind {
                Folder::Data => "XDG_DATA_HOME",
                Folder::Config => "XDG_CONFIG_HOME",
                Folder::Cache => "XDG_CACHE_HOME",
            };
            // Linux's XDG folders may be moved; the override ignores them.
            if cfg!(target_os = "linux") && std::env::var_os(variable).is_some() {
                continue;
            }
            assert_eq!(kind.path(Some(&real)), kind.path(None), "{kind:?}");
        }
    }
}
