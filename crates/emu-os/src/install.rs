//! Installing what the app downloads at first use: the 3DS core, mkxp-z
//! and the randomizer. Each is a program the app then runs, so each is
//! pinned: it comes from one address, and nothing of it is unpacked or run
//! until its bytes have the SHA-256 written in the app.
//!
//! An install takes the lock on its folder ([`Install::begin`]), so two
//! installs, from two games opened together or the app and the headless
//! runner, do not race: the second waits and then finds what the first
//! put there. It downloads into a temporary folder inside that folder
//! ([`Install::temp`]), checks the download ([`Pinned::download`]), lets
//! its caller unpack what it keeps, and moves that into place by a rename
//! ([`place`]), so what is installed is always whole.
//!
//! Everything here blocks.

use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The file in a folder that an install into it holds locked.
pub const LOCK_FILE: &str = "install.lock";
/// How the names of the temporary folders installs work in start.
pub const TEMP_PREFIX: &str = ".install-";

/// Fetches the file at a URL into a path, or says why it could not.
pub type Fetch<'f> = &'f dyn Fn(&str, &Path) -> Result<(), String>;

/// An install into a folder, under way: it holds the folder's lock until
/// it is dropped.
#[derive(Debug)]
pub struct Install {
    folder: PathBuf,
    _lock: File,
}

impl Install {
    /// Takes the lock on installing into `folder`, which is made if it is
    /// not there, waiting for any other install into it to finish first.
    ///
    /// # Errors
    ///
    /// If the folder or its lock file cannot be made or locked.
    pub fn begin(folder: &Path) -> io::Result<Self> {
        fs::create_dir_all(folder)?;
        Ok(Self {
            folder: folder.to_owned(),
            _lock: crate::lock::lock(&folder.join(LOCK_FILE))?,
        })
    }

    /// A new, empty temporary folder in the folder, to download and unpack
    /// in, removed with what is left in it when dropped. Those of installs
    /// that never finished, as when the app quit during one, are removed
    /// first: this install holds the lock, so none is in use.
    ///
    /// # Errors
    ///
    /// If the temporary folder cannot be made.
    pub fn temp(&self) -> io::Result<Temp> {
        if let Ok(entries) = fs::read_dir(&self.folder) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                    fs::remove_dir_all(entry.path()).ok();
                }
            }
        }
        let path = self
            .folder
            .join(format!("{TEMP_PREFIX}{}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Temp(path))
    }
}

/// An install's temporary folder, removed when dropped.
#[derive(Debug)]
pub struct Temp(PathBuf);

impl Temp {
    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        // What is left behind is cleared by the next install.
        fs::remove_dir_all(&self.0).ok();
    }
}

/// A download pinned to what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pinned<'a> {
    /// Where it is downloaded from.
    pub url: &'a str,
    /// Its SHA-256, in lowercase hex.
    pub sha256: &'a str,
    /// Its size in bytes, if that is pinned too: then a download of another
    /// size is refused without reading it through.
    pub size: Option<u64>,
}

/// Why a pinned download is not to be had.
#[derive(Debug)]
pub enum Error {
    /// It could not be downloaded: why, in the fetch's words.
    Download(String),
    /// What was downloaded is not the file pinned, and was deleted.
    NotPinned(Mismatch),
    /// The downloaded file could not be read.
    Io(io::Error),
}

/// How a download differs from the one pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// It is not as long.
    Size {
        /// Its size, in bytes.
        found: u64,
        /// The size pinned.
        pinned: u64,
    },
    /// Its bytes are others.
    Digest {
        /// Its SHA-256, in lowercase hex.
        found: String,
    },
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Size { found, pinned } => write!(f, "it is {found} bytes, not {pinned}"),
            Self::Digest { found } => write!(f, "its SHA-256 is {found}"),
        }
    }
}

impl Pinned<'_> {
    /// The download's name: the last part of its address.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.url.rsplit_once('/').map_or(self.url, |(_, name)| name)
    }

    /// Downloads it into the file `to` with `fetch`, and checks that what
    /// came is the file pinned ([`Pinned::verify`]). Only then is `to`
    /// there for the caller to unpack.
    ///
    /// # Errors
    ///
    /// If it cannot be downloaded or read, or is not the file pinned, in
    /// which case it was deleted.
    pub fn download(&self, to: &Path, fetch: Fetch<'_>) -> Result<(), Error> {
        fetch(self.url, to).map_err(Error::Download)?;
        self.verify(to)
    }

    /// Checks that the file at `path` is the one pinned: as long, if its
    /// size is pinned, and with its SHA-256. One that is not is deleted.
    ///
    /// # Errors
    ///
    /// [`Error::NotPinned`] if it differs, or [`Error::Io`] if it cannot
    /// be read.
    pub fn verify(&self, path: &Path) -> Result<(), Error> {
        let differs = match self.size {
            Some(pinned) => {
                let found = fs::metadata(path).map_err(Error::Io)?.len();
                (found != pinned).then_some(Mismatch::Size { found, pinned })
            }
            None => None,
        };
        let differs = match differs {
            Some(differs) => Some(differs),
            None => {
                let found = sha256(path).map_err(Error::Io)?;
                (found != self.sha256).then_some(Mismatch::Digest { found })
            }
        };
        match differs {
            Some(differs) => {
                fs::remove_file(path).ok();
                Err(Error::NotPinned(differs))
            }
            None => Ok(()),
        }
    }
}

/// The SHA-256 of the file at `path`, in lowercase hex, read a piece at a
/// time.
///
/// # Errors
///
/// If the file cannot be read.
pub fn sha256(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Moves the folder `prepared` to `target`, replacing whatever an earlier,
/// broken install left there. On Windows the rename waits out a virus
/// scanner that still has the programs just unpacked open
/// ([`crate::file::rename`]).
///
/// # Errors
///
/// If what is at `target` cannot be removed, or `prepared` moved.
pub fn place(prepared: &Path, target: &Path) -> io::Result<()> {
    match fs::remove_dir_all(target) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    crate::file::rename(prepared, target)
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// The SHA-256 of `abc`.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("emu-os-install-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// The names in the folder `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("folder")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn pinned(size: Option<u64>) -> Pinned<'static> {
        Pinned {
            url: "https://example.com/release/v1/core.zip",
            sha256: ABC,
            size,
        }
    }

    /// A fetch that "downloads" `bytes`.
    fn gives(bytes: &'static [u8]) -> impl Fn(&str, &Path) -> Result<(), String> {
        move |url, to| {
            assert_eq!(url, "https://example.com/release/v1/core.zip");
            fs::write(to, bytes).map_err(|error| error.to_string())
        }
    }

    #[test]
    fn a_download_is_handed_over_only_if_it_is_the_one_pinned() {
        let dir = temp_dir("pinned");
        let to = dir.join("core.zip");
        assert_eq!(pinned(None).file_name(), "core.zip");
        for size in [None, Some(3)] {
            pinned(size).download(&to, &gives(b"abc")).expect("pinned");
            assert_eq!(fs::read(&to).expect("kept"), b"abc");
        }
        // Other bytes, as long or not: deleted, and said how they differ.
        for size in [None, Some(3)] {
            let error = pinned(size)
                .download(&to, &gives(b"abd"))
                .expect_err("not it");
            let Error::NotPinned(differs) = error else {
                panic!("{error:?}");
            };
            assert_eq!(
                differs.to_string(),
                "its SHA-256 is a52d159f262b2c6ddb724a61840befc36eb30c88877a4030b65cbe86298449c9"
            );
            assert!(!to.exists(), "deleted");
        }
        let error = pinned(Some(3))
            .download(&to, &gives(b"abcd"))
            .expect_err("longer");
        let Error::NotPinned(differs) = error else {
            panic!("{error:?}");
        };
        assert_eq!(
            differs,
            Mismatch::Size {
                found: 4,
                pinned: 3
            }
        );
        assert_eq!(differs.to_string(), "it is 4 bytes, not 3");
        assert!(!to.exists(), "deleted");
        // A download that fails says why, and one that left no file is an
        // error, not a pass.
        let offline = |_: &str, _: &Path| Err("Could not resolve host: example.com".to_owned());
        let error = pinned(None).download(&to, &offline).expect_err("offline");
        assert!(
            matches!(&error, Error::Download(why) if why == "Could not resolve host: example.com"),
            "{error:?}"
        );
        let nothing = |_: &str, _: &Path| Ok(());
        for size in [None, Some(3)] {
            let error = pinned(size).download(&to, &nothing).expect_err("no file");
            assert!(matches!(error, Error::Io(_)), "{error:?}");
        }
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn an_install_works_in_a_temporary_folder_of_its_folder() {
        let dir = temp_dir("temp");
        let folder = dir.join("3DS");
        // An install the app quit during left this behind.
        fs::create_dir_all(folder.join(".install-1")).expect("leftover");
        fs::write(folder.join(".install-1/core.zip"), b"half").expect("leftover");
        fs::create_dir_all(folder.join("azahar-2126.2")).expect("installed");
        let install = Install::begin(&folder).expect("locked");
        assert!(folder.join(".install-1").exists(), "not until it works");
        let temp = install.temp().expect("made");
        let path = temp.path().to_owned();
        assert_eq!(
            path,
            folder.join(format!(".install-{}", std::process::id()))
        );
        assert_eq!(fs::read_dir(&path).expect("made").count(), 0, "empty");
        assert!(!folder.join(".install-1").exists(), "cleared");
        fs::write(path.join("core.zip"), b"abc").expect("download");
        drop(temp);
        assert_eq!(names(&folder), ["azahar-2126.2", "install.lock"]);
        drop(install);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_second_install_waits_for_the_first() {
        let dir = temp_dir("lock");
        let first = Install::begin(&dir).expect("locked");
        thread::scope(|scope| {
            let second = scope.spawn(|| Install::begin(&dir).map(drop));
            thread::sleep(Duration::from_millis(100));
            assert!(!second.is_finished(), "the second install waits");
            drop(first);
            second.join().expect("no panic").expect("locked in turn");
        });
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn what_is_prepared_replaces_what_a_broken_install_left() {
        let dir = temp_dir("place");
        let (prepared, target) = (dir.join(".install-1/core"), dir.join("core"));
        fs::create_dir_all(&prepared).expect("prepared");
        fs::write(prepared.join("core.so"), b"whole").expect("library");
        place(&prepared, &target).expect("placed");
        assert_eq!(fs::read(target.join("core.so")).expect("placed"), b"whole");
        // Over a folder that holds something else.
        fs::create_dir_all(&prepared).expect("prepared");
        fs::write(prepared.join("core.dylib"), b"new").expect("library");
        place(&prepared, &target).expect("replaced");
        assert_eq!(names(&target), ["core.dylib"]);
        assert!(!prepared.exists());
        assert!(place(&prepared, &target).is_err(), "nothing to place");
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
