//! Files written whole or not at all, and renames that wait out a file
//! another program holds for a moment.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

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
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = create(&temporary, mode)?;
    fill(&mut file)?;
    file.sync_all()?;
    drop(file);
    rename(&temporary, path)?;
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
/// if given. One left by a write cut short goes first: opened again, it
/// would keep the permissions it had.
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
        // A write that fails leaves the file as it was.
        let failed = write_atomic_with(&path, None, |_| Err(io::Error::other("disk full")));
        assert_eq!(failed.expect_err("fails").to_string(), "disk full");
        assert_eq!(fs::read(&path).expect("read back"), b"one two");
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
            // A temporary file left by a write cut short, readable by all.
            let leftover = dir.join("auth.json.tmp");
            fs::write(&leftover, b"old").expect("leftover");
            fs::set_permissions(&leftover, fs::Permissions::from_mode(0o644)).expect("mode");
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
