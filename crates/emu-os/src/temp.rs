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
//!   someone else's. If someone else took that name, the app works in
//!   `ds-emulator/tmp` in the user's own cache folder instead, so that
//!   one `mkdir` by another user turns nothing off.
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
/// or on Linux a folder of the user's own inside it, or failing that
/// inside their cache folder.
///
/// # Errors
///
/// On Linux, if neither folder can be made as the user's own.
pub fn work_dir() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        linux::work_dir(
            &std::env::temp_dir(),
            crate::cache_dir().as_deref(),
            user::id(),
        )
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

    /// The folder for work folders: `ds-emulator-<uid>` in `temp`, the
    /// temporary folder, for the user `uid` alone ([`private_in`]).
    ///
    /// Another user can take that name first, with one `mkdir` in `/tmp`.
    /// The app does not use what they made, and is not to be stopped by
    /// it either: it then works in `ds-emulator/tmp` in `cache`, the
    /// user's own cache folder, made and checked the same way. Its path
    /// is kept short, as an RPG Maker game's socket may go there too.
    /// Only without a cache folder does it fail, saying what is in its
    /// way in the temporary folder.
    pub(super) fn work_dir(temp: &Path, cache: Option<&Path>, uid: u32) -> io::Result<PathBuf> {
        let refused = match private_in(temp, uid) {
            Ok(dir) => return Ok(dir),
            Err(refused) => refused,
        };
        let Some(cache) = cache else {
            return Err(refused);
        };
        let own = cache.join("ds-emulator");
        fs::create_dir_all(&own)
            .and_then(|()| private(own.join("tmp"), uid))
            .map_err(|_| refused)
    }

    /// The folder `ds-emulator-<uid>` in `temp`, made if it is not there,
    /// for the user `uid` alone.
    ///
    /// Anyone may make a folder in `/tmp`, so one found there is only
    /// used if it is a folder, not a link, and `uid`'s own: only its owner
    /// can then put anything in it, or swap it for something else, as
    /// `/tmp` lets nobody remove what is another's.
    pub(super) fn private_in(temp: &Path, uid: u32) -> io::Result<PathBuf> {
        private(temp.join(format!("ds-emulator-{uid}")), uid)
    }

    /// The folder `dir`, made if it is not there, for the user `uid`
    /// alone, or why it is not theirs to use.
    fn private(dir: PathBuf, uid: u32) -> io::Result<PathBuf> {
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

    #[test]
    #[cfg_attr(not(unix), ignore = "Windows: the temporary folder is the user's own")]
    fn a_name_taken_in_the_shared_temporary_folder_sends_the_app_to_its_cache_folder() {
        #[cfg(unix)]
        {
            use std::fs;
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

            let root = std::env::temp_dir().join(format!("emu-os-taken-{}", std::process::id()));
            fs::remove_dir_all(&root).ok();
            let (temp, cache) = (root.join("tmp"), root.join("home").join(".cache"));
            fs::create_dir_all(&temp).expect("temp");
            fs::create_dir_all(&cache).expect("cache");
            let uid = fs::metadata(&root).expect("metadata").uid();
            let shared = temp.join(format!("ds-emulator-{uid}"));

            // Nothing in the way: the folder in the temporary folder.
            let dir = linux::work_dir(&temp, Some(&cache), uid).expect("made");
            assert_eq!(dir, shared);
            fs::remove_dir(&shared).expect("removed");

            // Another user took the name first, with a link to a folder of
            // theirs: one `mkdir` or `ln` in `/tmp` is all it takes. The
            // app does not use it, and is not stopped by it either.
            let theirs = root.join("theirs");
            fs::create_dir(&theirs).expect("theirs");
            std::os::unix::fs::symlink(&theirs, &shared).expect("taken");
            let dir = linux::work_dir(&temp, Some(&cache), uid).expect("the user's own");
            assert_eq!(dir, cache.join("ds-emulator").join("tmp"));
            let mode = fs::metadata(&dir).expect("metadata").permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "nobody else may open it");
            assert!(linux::is_private(&dir, uid));
            assert_eq!(
                fs::read_dir(&theirs).expect("theirs").count(),
                0,
                "untouched"
            );
            // Found again, and short: a socket's path may be 108 bytes,
            // and this adds 16 to the cache folder's.
            assert_eq!(
                linux::work_dir(&temp, Some(&cache), uid).expect("found"),
                dir
            );
            assert_eq!(dir.as_os_str().len(), cache.as_os_str().len() + 16);

            // Without a cache folder, or with its folder taken too, the
            // app says which folder is in its way.
            let error = linux::work_dir(&temp, None, uid).expect_err("nowhere");
            assert!(
                error.to_string().contains("is not this user's own folder"),
                "{error}"
            );
            fs::remove_dir(&dir).expect("removed");
            std::os::unix::fs::symlink(&theirs, &dir).expect("taken too");
            let error = linux::work_dir(&temp, Some(&cache), uid).expect_err("nowhere");
            assert!(
                error.to_string().contains(&shared.display().to_string()),
                "{error}"
            );
            fs::remove_dir_all(&root).expect("clean up");
        }
    }
}
