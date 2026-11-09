//! Lock files: a file held locked for as long as something is worked on,
//! so that another copy of the app, or the headless runner, waits for it
//! or leaves it alone. The system lets go of a lock when its process ends,
//! however it ends, so a lock nobody holds is one that was left behind.
//!
//! A lock file is opened as it is and never emptied. Windows may refuse to
//! empty a file another process holds locked, and where the file locked is
//! also the one being written, emptying it would lose what its holder
//! wrote.

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// Opens the file at `path` to lock it, making it if it is not there, and
/// leaving what it holds as it is.
///
/// # Errors
///
/// If the file cannot be opened or made.
pub fn open(path: &Path) -> io::Result<File> {
    File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
}

/// Takes the lock on the file at `path`, waiting for whoever holds it to
/// let go. It is held until the file returned is dropped.
///
/// # Errors
///
/// If the file cannot be opened or locked.
pub fn lock(path: &Path) -> io::Result<File> {
    let file = open(path)?;
    file.lock()?;
    Ok(file)
}

/// Takes the lock on the file at `path` if nobody holds it, and returns
/// the file, to hold until it is dropped; `None` if somebody does.
///
/// # Errors
///
/// If the file cannot be opened or locked.
pub fn try_lock(path: &Path) -> io::Result<Option<File>> {
    let file = open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-os-lock-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn a_lock_file_is_made_once_and_never_emptied() {
        let dir = temp_dir("kept");
        let path = dir.join("install.lock");
        drop(lock(&path).expect("made and locked"));
        assert_eq!(fs::read(&path).expect("made"), b"");
        // A file that is locked and written, as a game half unpacked is.
        fs::write(&path, b"what its holder wrote").expect("write");
        let held = lock(&path).expect("locked");
        assert!(try_lock(&path).expect("opened").is_none(), "held");
        assert_eq!(fs::read(&path).expect("read"), b"what its holder wrote");
        drop(held);
        // Waited for, not tried: a program another test starts at this
        // moment holds every open file for an instant, this one too.
        drop(lock(&path).expect("let go"));
        assert_eq!(fs::read(&path).expect("read"), b"what its holder wrote");
        let free = try_lock(&dir.join("other.lock")).expect("opened");
        assert!(free.is_some(), "nobody holds a new one");
        // A folder that is not there holds no lock file to make.
        assert!(lock(&dir.join("missing").join("install.lock")).is_err());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_second_lock_waits_for_the_first() {
        let dir = temp_dir("waits");
        let path = dir.join("install.lock");
        let first = lock(&path).expect("locked");
        thread::scope(|scope| {
            let second = scope.spawn(|| lock(&path).map(drop));
            thread::sleep(Duration::from_millis(100));
            assert!(!second.is_finished(), "the second waits");
            drop(first);
            second.join().expect("no panic").expect("locked in turn");
        });
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
