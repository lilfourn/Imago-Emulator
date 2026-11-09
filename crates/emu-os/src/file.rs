//! Files written whole or not at all, and renames that wait out a file
//! another program holds for a moment.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Renames the file or folder at `from` to `to`, replacing a file there,
/// as [`fs::rename`] does.
///
/// On Windows that can fail for a moment while another program, such as a
/// virus scanner or the search indexer, has either file open, so it is
/// tried again for about a second. A read-only file at `to` is made
/// writable first, as macOS and Linux replace one anyway.
///
/// # Errors
///
/// As [`fs::rename`], once every try has failed.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::rename(from, to)
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)
    }
}

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    /// How many times a rename is tried.
    const TRIES: u32 = 10;
    /// How long to wait after a rename fails, at first and at most: in all,
    /// about a second.
    const FIRST_WAIT: Duration = Duration::from_millis(10);
    const LONGEST_WAIT: Duration = Duration::from_millis(200);

    /// Windows's `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION` and
    /// `ERROR_LOCK_VIOLATION`: another program has the file open.
    const IN_USE: [i32; 3] = [5, 32, 33];

    pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
        let mut wait = FIRST_WAIT;
        for _ in 1..TRIES {
            match fs::rename(from, to) {
                Err(error)
                    if error
                        .raw_os_error()
                        .is_some_and(|code| IN_USE.contains(&code)) =>
                {
                    if error.kind() == io::ErrorKind::PermissionDenied {
                        make_writable(to);
                    }
                    thread::sleep(wait);
                    wait = (wait * 2).min(LONGEST_WAIT);
                }
                done => return done,
            }
        }
        fs::rename(from, to)
    }

    /// Clears the read-only flag of the file at `path`, if it has one.
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows: this clears the file's read-only flag, nothing more"
    )]
    fn make_writable(path: &Path) {
        if let Ok(metadata) = fs::metadata(path)
            && metadata.is_file()
            && metadata.permissions().readonly()
        {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions).ok();
        }
    }
}

/// Replaces `path` with `bytes` so that a crash or power cut mid-write
/// never leaves a half-written file: the bytes go to a sibling temporary
/// file first, reach the disk, and only then is it renamed over `path`
/// (with [`rename`], which on Windows waits out a virus scanner).
///
/// The file that was at `path` is never written to: it is replaced, by
/// the rename, with a new file. So another name for the old file, such as
/// the hard link the app keeps of a battery save before a state load
/// replaces it, goes on holding the old bytes. Callers rely on that; keep
/// it so.
///
/// The temporary file is this write's alone, `name.<process>-<n>.tmp`, so
/// two writers of one file, in this app or in two copies of it, never
/// write through each other's: the later rename wins, whole. A write that
/// fails removes it. One cut short by a crash leaves it, with whatever of
/// the file it holds. At a later launch the app removes those that are an
/// hour old from its own folders (emu-app's `leftovers`): its settings,
/// data and cache folders, to two folders deep, and the games it unpacked,
/// patched or got from the store. One beside a game in a folder of the
/// user's own ROMs stays until the user removes it: the app does not go
/// looking through those.
///
/// On macOS the folder is not synced after the rename: syncing there
/// flushes the whole drive's cache, which is slow, and without it a power
/// cut can at worst undo the rename, leaving the old file whole. Windows
/// can't open a folder to sync it, and NTFS journals the rename itself.
///
/// # Errors
///
/// If the temporary file cannot be made, written or renamed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, None, |file| file.write_all(bytes))
}

/// Replaces `path` as [`write_atomic`] does, with what `fill` writes to
/// the temporary file, for a file too big to hold in memory.
///
/// With a `mode`, the file has those Unix permissions and never looser
/// ones, from the moment it is made: `0o600` for a file only its owner may
/// read. Windows files have no such bits; a file there is as private as
/// its folder.
///
/// # Errors
///
/// If the temporary file cannot be made, filled or renamed.
pub fn write_atomic_with(
    path: &Path,
    mode: Option<u32>,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    /// Writes so far, which tells this process's temporary files apart.
    static WRITES: AtomicU32 = AtomicU32::new(0);
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = PathBuf::from(temporary);
    let written = (|| {
        let mut file = create(&temporary, mode)?;
        fill(&mut file)?;
        file.sync_all()?;
        // Closed before it is renamed, and before it is removed below.
        drop(file);
        rename(&temporary, path)
    })();
    if written.is_err() {
        fs::remove_file(&temporary).ok();
    }
    written?;
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let folder = match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
        // Best effort: not every file system can sync a folder.
        File::open(folder).and_then(|folder| folder.sync_all()).ok();
    }
    Ok(())
}

/// Makes the new, empty file `temporary`, with the Unix permissions `mode`
/// if given. One left under the same name by a write cut short, in a
/// process that had this one's number, goes first: opened again, it would
/// keep the permissions it had.
fn create(temporary: &Path, mode: Option<u32>) -> io::Result<File> {
    match fs::remove_file(temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let file = options.mode(mode).open(temporary)?;
        // Exactly `mode`: the process's umask may have taken bits away.
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        return Ok(file);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(temporary)
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-os-file-{test}-{}", std::process::id()));
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

    #[test]
    fn a_rename_replaces_the_file_there() {
        let dir = temp_dir("rename");
        let (from, to) = (dir.join("game.sav.tmp"), dir.join("game.sav"));
        fs::write(&from, b"new").expect("write");
        fs::write(&to, b"old").expect("write");
        // Read-only, as macOS and Linux replace it anyway.
        let mut permissions = fs::metadata(&to).expect("metadata").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&to, permissions).expect("read-only");
        rename(&from, &to).expect("renamed");
        assert_eq!(fs::read(&to).expect("read"), b"new");
        assert!(!from.exists());
        assert!(rename(&from, &to).is_err(), "nothing left to rename");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_file_is_replaced_whole_and_no_temporary_is_left() {
        let dir = temp_dir("atomic");
        let path = dir.join("game.sav");
        write_atomic(&path, b"first").expect("first write");
        write_atomic(&path, b"second").expect("second write");
        assert_eq!(fs::read(&path).expect("read back"), b"second");
        assert_eq!(names(&dir), ["game.sav"]);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_file_is_filled_a_piece_at_a_time() {
        let dir = temp_dir("filled");
        let path = dir.join("Game.cxi");
        fs::write(&path, b"old").expect("write");
        let mut running = File::open(&path).expect("open");
        write_atomic_with(&path, None, |file| {
            file.write_all(b"one ")?;
            file.write_all(b"two")
        })
        .expect("written");
        assert_eq!(fs::read(&path).expect("read back"), b"one two");
        // A program that had the old file open keeps it.
        let mut kept = String::new();
        running.read_to_string(&mut kept).expect("read the old");
        assert_eq!(kept, "old");
        // Closed before its folder goes: Windows may not remove a folder
        // that holds an open file.
        drop(running);
        // A write that fails leaves the file as it was.
        let failed = write_atomic_with(&path, None, |_| Err(io::Error::other("disk full")));
        assert_eq!(failed.expect_err("fails").to_string(), "disk full");
        assert_eq!(fs::read(&path).expect("read back"), b"one two");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn the_old_file_is_replaced_not_written_to() {
        let dir = temp_dir("replaced");
        let (save, copy) = (dir.join("game.sav"), dir.join("game.sav.before-state"));
        fs::write(&save, b"the battery save").expect("write");
        // A copy by hard link, as the app keeps of a save before a state
        // load replaces it: one file under two names.
        fs::hard_link(&save, &copy).expect("linked");
        write_atomic(&save, b"what the state held").expect("written");
        write_atomic_with(&save, None, |file| file.write_all(b"and again")).expect("written");
        assert_eq!(fs::read(&save).expect("read"), b"and again");
        assert_eq!(
            fs::read(&copy).expect("read"),
            b"the battery save",
            "the copy is the old file, untouched"
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_write_that_fails_leaves_no_temporary_file() {
        let dir = temp_dir("failed");
        let path = dir.join("game.sav");
        fs::write(&path, b"old").expect("write");
        // The disk fills as it is written.
        let failed = write_atomic_with(&path, None, |file| {
            file.write_all(b"half")?;
            Err(io::Error::other("disk full"))
        });
        assert!(failed.is_err());
        assert_eq!(names(&dir), ["game.sav"], "nothing beside the save");
        // It cannot be renamed into place: a folder is in the way.
        let taken = dir.join("Saves");
        fs::create_dir(&taken).expect("folder");
        fs::write(taken.join("kept"), b"kept").expect("write");
        assert!(write_atomic(&taken, b"new").is_err());
        assert_eq!(names(&dir), ["Saves", "game.sav"]);
        assert_eq!(fs::read(&path).expect("read back"), b"old");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn two_writers_of_one_file_each_write_a_temporary_of_their_own() {
        use std::sync::Barrier;

        let dir = temp_dir("writers");
        let path = dir.join("library.toml");
        // Both have made their temporary file before either is done, as
        // two windows saving the library at once would.
        let both_writing = Barrier::new(2);
        let write = |bytes: &'static [u8]| {
            write_atomic_with(&path, None, |file| {
                file.write_all(bytes)?;
                both_writing.wait();
                Ok(())
            })
        };
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| write(b"the first writer's, whole"));
            let second = scope.spawn(|| write(b"the second's"));
            (
                first.join().expect("no panic"),
                second.join().expect("no panic"),
            )
        });
        first.expect("the first is written");
        second.expect("the second is written");
        let kept = fs::read(&path).expect("read back");
        assert!(
            kept == b"the first writer's, whole" || kept == b"the second's",
            "one of the two, whole: {:?}",
            String::from_utf8_lossy(&kept)
        );
        assert_eq!(names(&dir), ["library.toml"]);
        // The name still ends as what is left behind is looked for by.
        let seen = std::sync::Mutex::new(Vec::new());
        write_atomic_with(&path, None, |_| {
            seen.lock().expect("lock").extend(names(&dir));
            Ok(())
        })
        .expect("written");
        let seen = seen.into_inner().expect("lock");
        let temporary = seen
            .iter()
            .find(|name| *name != "library.toml")
            .expect("one");
        assert!(temporary.starts_with("library.toml."), "{temporary}");
        assert!(temporary.ends_with(".tmp"), "{temporary}");
        assert!(
            temporary.contains(&std::process::id().to_string()),
            "{temporary}"
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "Windows: files have no permission bits")]
    fn a_private_file_is_never_readable_by_others() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = temp_dir("private");
            let path = dir.join("auth.json");
            let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode();
            write_atomic_with(&path, Some(0o600), |file| {
                // As it is written, too.
                assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
                file.write_all(b"secret")
            })
            .expect("written");
            assert_eq!(mode(&path) & 0o777, 0o600);
            assert_eq!(fs::read(&path).expect("read back"), b"secret");
            // Bits the umask would take away are set all the same.
            let program = dir.join("core.dylib");
            write_atomic_with(&program, Some(0o755), |file| file.write_all(b"code"))
                .expect("written");
            assert_eq!(mode(&program) & 0o777, 0o755);
            assert_eq!(names(&dir), ["auth.json", "core.dylib"]);
            fs::remove_dir_all(&dir).expect("clean up");
        }
    }
}
