//! Where the app keeps what it writes only for a while: the folders it
//! unpacks, downloads and randomizes games in, patches on their way to
//! becoming games, and the socket and pipe an RPG Maker game's player
//! talks to it through.
//!
//! On macOS and Windows that is the temporary folder, which is the user's
//! own. On Linux the temporary folder is `/tmp`, which every user of the
//! computer shares: another user could make a folder, or a link, by a name
//! the app is about to use, and the app would write into it. So on Linux:
//!
//! - [`work_dir`] is a folder of the app's inside the temporary folder,
//!   `ds-emulator-<user id>`, made for its user alone and refused if it is
//!   someone else's.
//! - [`runtime_dir`] is `$XDG_RUNTIME_DIR` when the session has one, such
//!   as `/run/user/1000`: the folder the system makes for the user alone,
//!   for sockets and the like. It is a small disk in memory, so only the
//!   socket and the pipe go there, never a game, which can be 4 GB.
//!
//! Both are short, as a socket's path may be about a hundred bytes.

use std::io;
use std::path::PathBuf;

/// The folder for work folders and other files the app removes when it is
/// done with them, which may be as big as a game: the temporary folder,
/// or on Linux a folder of the user's own inside it.
///
/// # Errors
///
/// On Linux, if that folder cannot be made, or one is there by its name
/// that is not the user's own folder.
pub fn work_dir() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        linux::private_in(&std::env::temp_dir(), user::id())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(std::env::temp_dir())
    }
}

/// The folder for sockets and named pipes, which are small and need short
/// paths: the temporary folder, or on Linux the session's own
/// `$XDG_RUNTIME_DIR`, and without one, [`work_dir`].
///
/// # Errors
///
/// As [`work_dir`], where it falls back to it.
pub fn runtime_dir() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let own = user::id();
        let session = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        match session.filter(|dir| dir.is_absolute() && linux::is_private(dir, own)) {
            Some(dir) => Ok(dir),
            None => work_dir(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(std::env::temp_dir())
    }
}

/// Who the app runs as, on Linux.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod user {
    /// The id of the user the app runs as.
    pub(super) fn id() -> u32 {
        // SAFETY: geteuid takes no arguments, changes nothing and cannot
        // fail.
        unsafe { libc::geteuid() }
    }
}

/// Linux's folders. Built for tests on macOS too, which has the same
/// permissions, so they run where the app is developed.
#[cfg(any(target_os = "linux", all(test, unix)))]
mod linux {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};

    /// Whether `dir` is a folder, not a link to one, that belongs to the
    /// user `uid` and that nobody else may open.
    pub(super) fn is_private(dir: &Path, uid: u32) -> bool {
        fs::symlink_metadata(dir).is_ok_and(|found| {
            found.file_type().is_dir() && found.uid() == uid && found.mode() & 0o077 == 0
        })
    }

    /// The folder `ds-emulator-<uid>` in `temp`, made if it is not there,
    /// for the user `uid` alone.
    ///
    /// Anyone may make a folder in `/tmp`, so one found there is only
    /// used if it is a folder, not a link, and `uid`'s own: only its owner
    /// can then put anything in it, or swap it for something else, as
    /// `/tmp` lets nobody remove what is another's.
    pub(super) fn private_in(temp: &Path, uid: u32) -> io::Result<PathBuf> {
        let dir = temp.join(format!("ds-emulator-{uid}"));
        match fs::DirBuilder::new().mode(0o700).create(&dir) {
            Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
            _ => {}
        }
        let found = fs::symlink_metadata(&dir)?;
        if !found.file_type().is_dir() || found.uid() != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is not this user's own folder: remove it, or set TMPDIR to another temporary folder",
                    dir.display()
                ),
            ));
        }
        // The user's own, but opened to others since: closed again.
        if found.mode() & 0o077 != 0 {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folders_are_short_enough_for_a_socket() {
        let (work, runtime) = (work_dir().expect("made"), runtime_dir().expect("found"));
        assert!(work.is_dir() && runtime.is_dir());
        if cfg!(target_os = "linux") {
            assert_ne!(work, std::env::temp_dir(), "a folder of the user's own");
            // `ds-emulator-XXXXXXXX.sock`, in a path of at most 108 bytes.
            assert!(runtime.as_os_str().len() < 80, "{}", runtime.display());
        } else {
            // macOS and Windows: where they always were.
            assert_eq!(work, std::env::temp_dir());
            assert_eq!(runtime, std::env::temp_dir());
        }
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "Windows: the temporary folder is the user's own")]
    fn a_folder_in_a_shared_temporary_folder_is_used_only_if_it_is_the_users_own() {
        #[cfg(unix)]
        {
            use std::fs;
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

            let temp = std::env::temp_dir().join(format!("emu-os-temp-{}", std::process::id()));
            fs::remove_dir_all(&temp).ok();
            fs::create_dir_all(&temp).expect("temp");
            // The user the tests run as: whoever made this folder.
            let uid = fs::metadata(&temp).expect("metadata").uid();
            let mode = |dir: &std::path::Path| {
                fs::metadata(dir).expect("metadata").permissions().mode() & 0o777
            };

            let dir = linux::private_in(&temp, uid).expect("made");
            assert_eq!(dir, temp.join(format!("ds-emulator-{uid}")));
            assert_eq!(mode(&dir), 0o700, "nobody else may open it");
            assert!(linux::is_private(&dir, uid));
            fs::write(dir.join("kept"), b"work").expect("write");
            // Found again as it is.
            assert_eq!(linux::private_in(&temp, uid).expect("found"), dir);
            assert!(dir.join("kept").exists());
            // Opened to others since, it is closed again.
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("opened");
            assert!(!linux::is_private(&dir, uid));
            assert_eq!(linux::private_in(&temp, uid).expect("found"), dir);
            assert_eq!(mode(&dir), 0o700);

            // Another user made a folder by the name first: for this test,
            // the folder is this user's, and the app runs as another.
            let other = uid.wrapping_add(1);
            fs::create_dir(temp.join(format!("ds-emulator-{other}"))).expect("made first");
            let error = linux::private_in(&temp, other).expect_err("someone else's");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(
                error.to_string().contains("is not this user's own folder"),
                "{error}"
            );
            assert!(!linux::is_private(&dir, other));
            // Or a link by that name, to a folder that is the user's own.
            let third = uid.wrapping_add(2);
            let link = temp.join(format!("ds-emulator-{third}"));
            std::os::unix::fs::symlink(&dir, &link).expect("link");
            assert!(
                linux::private_in(&temp, third).is_err(),
                "a link, not a folder"
            );
            assert!(!linux::is_private(&link, uid), "a link, not a folder");
            assert!(dir.join("kept").exists(), "nothing was written through it");
            fs::remove_dir_all(&temp).expect("clean up");
        }
    }
}
