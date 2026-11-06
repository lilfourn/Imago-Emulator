//! What the app writes for a while and then removes: the folders it
//! unpacks archives and downloads into, in the temporary folder, and the
//! games it is writing, as `….partial` files beside where they go. A big
//! 3DS game makes either 4 GB, so none may be left behind.
//!
//! Each is locked while the app works in it. The system lets go of a lock
//! when its process ends, crashed or killed, so one that no running app
//! holds is what an app left behind; at launch, those are removed, and on
//! macOS any `tar` still unpacking into one is stopped. On quit, the app
//! removes its own, and stops the programs it started to work in them
//! (see [`WorkFolder::spawn`]). Files the app writes for a moment and keeps
//! no lock on, such as a save's `….tmp`, are removed once they are old.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Stdio;
use std::process::{Child, ChildStderr, Command, ExitStatus};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::persist;

/// How the names of the work folders start, in the temporary folder:
/// `ds-emulator-unpack-<pid>-<n>`, `ds-emulator-store-<pid>-<n>`.
const KINDS: [&str; 2] = ["ds-emulator-unpack-", "ds-emulator-store-"];

/// The lock file in each work folder, held while the folder is in use.
const LOCK: &str = ".ds-emulator-lock";

/// Where patches unpacked from archives are kept until their games are
/// made, in the temporary folder.
pub const PATCHES: &str = "ds-emulator-patches";

/// How the names of games being written end, locked while they are.
pub const PARTIAL: &str = ".partial";

/// How old what has no lock must be to count as left behind: a folder made
/// before folders had locks, an unpacked patch, or a file the app keeps no
/// lock on, such as a save's `….tmp`. They live for seconds.
const STALE: Duration = Duration::from_secs(60 * 60);

/// What this app has under way, to remove on quit.
static OWN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn own() -> MutexGuard<'static, Vec<PathBuf>> {
    lock(&OWN)
}

/// The programs this app started to work in its work folders, such as a
/// `tar` unpacking into one, by folder, to stop on quit.
static WORKERS: Mutex<Vec<(PathBuf, Arc<Mutex<Child>>)>> = Mutex::new(Vec::new());

fn workers() -> MutexGuard<'static, Vec<(PathBuf, Arc<Mutex<Child>>)>> {
    lock(&WORKERS)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Holds `path` as this app's until dropped, to remove on quit.
fn hold(path: &Path) {
    own().push(path.to_owned());
}

fn let_go(path: &Path) {
    own().retain(|held| held != path);
}

/// A new, empty work folder in the temporary folder, locked while it
/// lives, and removed with everything in it when dropped.
pub struct WorkFolder {
    path: PathBuf,
    _lock: File,
}

impl WorkFolder {
    /// A work folder for `kind` of work, `"unpack"` or `"store"`.
    pub fn new(kind: &str) -> io::Result<Self> {
        Self::new_in(&std::env::temp_dir(), kind)
    }

    fn new_in(temp: &Path, kind: &str) -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = temp.join(format!(
            "ds-emulator-{kind}-{}-{number}",
            std::process::id()
        ));
        fs::remove_dir_all(&path).ok();
        fs::create_dir_all(&path)?;
        let lock = File::create(path.join(LOCK)).and_then(|lock| {
            lock.lock()?;
            Ok(lock)
        });
        let lock = match lock {
            Ok(lock) => lock,
            Err(error) => {
                fs::remove_dir_all(&path).ok();
                return Err(error);
            }
        };
        hold(&path);
        Ok(Self { path, _lock: lock })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Starts `command`, a program that works in this folder, such as one
    /// unpacking an archive into it. It is stopped if the app quits while
    /// it runs ([`remove_own`]), and when the [`Worker`] is dropped.
    pub fn spawn(&self, command: &mut Command) -> io::Result<Worker> {
        let child = Arc::new(Mutex::new(command.spawn()?));
        workers().push((self.path.clone(), Arc::clone(&child)));
        Ok(Worker { child })
    }
}

/// A program working in a [`WorkFolder`], from [`WorkFolder::spawn`]. It is
/// stopped when dropped, if it is still running.
pub struct Worker {
    child: Arc<Mutex<Child>>,
}

impl Worker {
    /// Its standard error, if that was piped and is not yet taken.
    pub fn take_stderr(&self) -> Option<ChildStderr> {
        lock(&self.child).stderr.take()
    }

    /// How it ended, once it has.
    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        lock(&self.child).try_wait()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        stop(&self.child);
        workers().retain(|(_, child)| !Arc::ptr_eq(child, &self.child));
    }
}

/// Stops the program `child`, if it is still running, and waits for it to
/// end. It is killed outright (on macOS and Linux with SIGKILL): a `tar`
/// the app starts on macOS ignores being asked to stop (SIGTERM), which it
/// inherits from the app.
fn stop(child: &Mutex<Child>) {
    let mut child = lock(child);
    if matches!(child.try_wait(), Ok(None)) {
        child.kill().ok();
    }
    child.wait().ok();
}

/// Stops the programs this app started to work in the folder at `path`.
fn stop_working_in(path: &Path) {
    let children: Vec<Arc<Mutex<Child>>> = workers()
        .iter()
        .filter(|(folder, _)| folder == path)
        .map(|(_, child)| Arc::clone(child))
        .collect();
    for child in children {
        stop(&child);
    }
}

impl Drop for WorkFolder {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
        let_go(&self.path);
    }
}

/// A game being written, `….partial`, locked while it is and removed when
/// dropped unless [`Unfinished::finish`] has renamed it into place.
pub struct Unfinished {
    path: PathBuf,
    file: Option<File>,
}

impl Unfinished {
    /// Creates the file at `path`, empty and locked.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = File::create(path)?;
        if let Err(error) = file.lock() {
            drop(file);
            fs::remove_file(path).ok();
            return Err(error);
        }
        hold(path);
        Ok(Self {
            path: path.to_owned(),
            file: Some(file),
        })
    }

    /// The file, to write to.
    pub fn file(&mut self) -> &mut File {
        self.file.as_mut().expect("open until finished")
    }

    /// Renames the whole file to `path`.
    pub fn finish(mut self, path: &Path) -> io::Result<()> {
        // Unlocked once closed.
        self.file = None;
        persist::rename(&self.path, path)?;
        let_go(&self.path);
        self.path = PathBuf::new();
        Ok(())
    }
}

impl Drop for Unfinished {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            self.file = None;
            fs::remove_file(&self.path).ok();
            let_go(&self.path);
        }
    }
}

/// Removes what this app has under way, stopping the programs it started
/// to work in it: for quitting, when no drop will run.
pub fn remove_own() {
    remove_own_where(|_| true);
}

/// Removes what this app has under way that `wanted` takes.
fn remove_own_where(wanted: impl Fn(&Path) -> bool) {
    let held: Vec<PathBuf> = {
        let mut own = own();
        let (held, kept) = own.drain(..).partition(|path| wanted(path));
        *own = kept;
        held
    };
    for path in held {
        if path.is_dir() {
            stop_working_in(&path);
            fs::remove_dir_all(&path).ok();
        } else {
            fs::remove_file(&path).ok();
        }
    }
}

/// Removes what apps that are no longer running left behind: in the
/// temporary folder, work folders and old unpacked patches; in each of
/// `folders`, where games are written, games half written and old
/// `….tmp` files. Says how many bytes that freed.
///
/// Blocking file I/O; call it off the UI thread.
pub fn remove_left_behind(folders: &[PathBuf]) -> u64 {
    remove_left_behind_in(&std::env::temp_dir(), folders)
}

fn remove_left_behind_in(temp: &Path, folders: &[PathBuf]) -> u64 {
    let mut freed = 0;
    if let Ok(entries) = fs::read_dir(temp) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let path = entry.path();
            if KINDS.iter().any(|kind| name.starts_with(kind))
                && path.is_dir()
                && !in_use(&path.join(LOCK), &path)
            {
                #[cfg(target_os = "macos")]
                stop_tar_left_unpacking_into(&path);
                freed += size(&path);
                fs::remove_dir_all(&path).ok();
            }
        }
    }
    freed += remove_unused_files(&temp.join(PATCHES), |_| true);
    for folder in folders {
        freed += remove_unused_files(folder, |name| {
            name.ends_with(PARTIAL) || name.ends_with(".tmp")
        });
    }
    freed
}

/// Removes the files in `folder` whose names `wanted` takes and that are
/// not in use, and says how many bytes that freed.
fn remove_unused_files(folder: &Path, wanted: impl Fn(&str) -> bool) -> u64 {
    let Ok(entries) = fs::read_dir(folder) else {
        return 0;
    };
    let mut freed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
        // A game being written is locked; anything else may be written
        // without a lock, so only an old one is done with.
        let unused = if name.ends_with(PARTIAL) {
            !in_use(&path, &path)
        } else {
            !changed_lately(&path)
        };
        if is_file && wanted(&name) && unused {
            freed += size(&path);
            fs::remove_file(&path).ok();
        }
    }
    freed
}

/// Whether what is at `path` is still in use: `lock` is held, or, if
/// there is no lock to be had, `path` changed lately.
fn in_use(lock: &Path, path: &Path) -> bool {
    match File::open(lock) {
        Ok(lock) => matches!(lock.try_lock(), Err(fs::TryLockError::WouldBlock)),
        Err(_) => changed_lately(path),
    }
}

/// Whether what is at `path` changed less than [`STALE`] ago.
fn changed_lately(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified.elapsed().map_or(true, |age| age < STALE))
}

/// Stops any `tar` still unpacking into the folder at `path`, left behind
/// by an app that crashed or was killed: one whose command line names it.
/// It is killed outright, as a `tar` the app started ignores being asked to
/// stop (SIGTERM), which it inherits from the app.
///
/// macOS only: there the app unpacks `.7z` and `.rar` files with `tar`,
/// which the system gives to launchd, still running, when the app dies,
/// and nothing ties its life to the app's. Elsewhere `.7z` files unpack in
/// the app itself, and only a `.rar` file, or a `.7z` one packed in a way
/// the app can't unpack, has a program unpack it (see `archive`): a crashed
/// app's one runs to its end, and what it wrote is removed at a later
/// launch.
#[cfg(target_os = "macos")]
fn stop_tar_left_unpacking_into(path: &Path) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    // The folder's name is letters, digits and dashes, so it needs no
    // escaping; it ends the folder's path, before a slash or a space.
    Command::new("/usr/bin/pkill")
        .arg("-KILL")
        .arg("-f")
        .arg(format!("^/usr/bin/tar .*{name}([/ ]|$)"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
}

/// Sets when the file or folder at `path` last changed, for tests that
/// make what they leave look old. A folder is opened as macOS and Linux
/// open one, but Windows opens one only to change its times, and only
/// with the flag that lets a handle be a folder's.
#[cfg(test)]
pub fn set_modified(path: &Path, time: std::time::SystemTime) -> io::Result<()> {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        /// `FILE_WRITE_ATTRIBUTES` and `FILE_FLAG_BACKUP_SEMANTICS`, from
        /// Windows's `winnt.h` and `WinBase.h`.
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?
    };
    #[cfg(not(windows))]
    let file = File::open(path)?;
    file.set_modified(time)
}

/// How many bytes the file, or the files in the folder, at `path` take.
fn size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    fs::read_dir(path).map_or(0, |entries| {
        entries.flatten().map(|entry| size(&entry.path())).sum()
    })
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;
    use std::time::SystemTime;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("emu-app-leftovers-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// Makes what is at `path` look `age` old.
    fn age(path: &Path, age: Duration) {
        set_modified(path, SystemTime::now() - age).expect("aged");
    }

    #[test]
    fn a_work_folder_is_held_until_dropped() {
        let temp = temp_dir("held");
        let folder = WorkFolder::new_in(&temp, "unpack").expect("made");
        let path = folder.path().to_owned();
        fs::write(path.join("Game.3ds"), b"NCSD").expect("written");
        assert!(own().contains(&path));
        // Another app's launch leaves it alone while this one works in it.
        assert_eq!(remove_left_behind_in(&temp, &[]), 0);
        assert!(path.join("Game.3ds").exists());
        drop(folder);
        assert!(!path.exists(), "removed when done");
        assert!(!own().contains(&path));
        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn what_an_app_that_quit_left_behind_is_removed() {
        let temp = temp_dir("left");
        // A folder whose lock nobody holds, as when the app was killed.
        let left = temp.join("ds-emulator-unpack-1-0");
        fs::create_dir_all(&left).expect("folder");
        fs::write(left.join(LOCK), b"").expect("lock");
        fs::write(left.join("Game.3ds"), [0; 1000]).expect("game");
        // One from before folders had locks: removed once it is old.
        let old = temp.join("ds-emulator-store-1-0");
        let new = temp.join("ds-emulator-store-1-1");
        for folder in [&old, &new] {
            fs::create_dir_all(folder).expect("folder");
            fs::write(folder.join("game.zip"), [0; 10]).expect("download");
        }
        age(&old, Duration::from_secs(2 * 60 * 60));
        // Not the app's own.
        let other = temp.join("ds-emulator-randomize-1-0");
        fs::create_dir_all(&other).expect("folder");
        // Unpacked patches, old and new.
        let patches = temp.join(PATCHES);
        fs::create_dir_all(&patches).expect("patches");
        fs::write(patches.join("Old.ips"), [0; 5]).expect("patch");
        fs::write(patches.join("New.ips"), [0; 5]).expect("patch");
        age(&patches.join("Old.ips"), Duration::from_secs(2 * 60 * 60));
        // Games: one half written by an app that quit, one being written,
        // and a game and its save. A save's `.tmp` holds no lock, so only
        // an old one is done with.
        let games = temp.join("Games");
        fs::create_dir_all(&games).expect("games");
        fs::write(games.join("Half.3ds.partial"), [0; 100]).expect("half");
        let writing = Unfinished::create(&games.join("Writing.3ds.partial")).expect("writing");
        age(
            &games.join("Writing.3ds.partial"),
            Duration::from_secs(2 * 60 * 60),
        );
        fs::write(games.join("Old.sav.tmp"), [0; 20]).expect("old save");
        age(&games.join("Old.sav.tmp"), Duration::from_secs(2 * 60 * 60));
        fs::write(games.join("Game.sav.tmp"), [0; 20]).expect("save being written");
        fs::write(games.join("Game.gba"), [0; 7]).expect("game");
        fs::write(games.join("Game.sav"), [0; 7]).expect("save");
        age(&games.join("Game.gba"), Duration::from_secs(2 * 60 * 60));

        let freed = remove_left_behind_in(&temp, std::slice::from_ref(&games));
        assert_eq!(freed, 1000 + 10 + 5 + 100 + 20);
        assert!(!left.exists() && !old.exists());
        assert!(new.exists() && other.exists());
        assert!(!patches.join("Old.ips").exists());
        assert!(patches.join("New.ips").exists());
        assert!(!games.join("Half.3ds.partial").exists());
        assert!(
            games.join("Writing.3ds.partial").exists(),
            "still being written"
        );
        assert!(!games.join("Old.sav.tmp").exists());
        assert!(games.join("Game.sav.tmp").exists());
        assert!(games.join("Game.gba").exists() && games.join("Game.sav").exists());
        drop(writing);
        assert!(!games.join("Writing.3ds.partial").exists());
        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "Linux, Windows: only macOS unpacks .7z files with tar, and stops a crashed app's one at launch"
    )]
    fn a_tar_still_unpacking_into_a_folder_left_behind_is_stopped() {
        let temp = temp_dir("tar");
        let left = temp.join(format!("ds-emulator-unpack-{}-99", std::process::id()));
        fs::create_dir_all(&left).expect("folder");
        fs::write(left.join(LOCK), b"").expect("lock");
        // A `tar` unpacking into it, which waits for the rest of its
        // archive while this test holds its input open. Like one the app
        // starts, it ignores SIGTERM.
        let mut tar = Command::new("/bin/sh")
            .arg("-c")
            .arg("trap '' TERM; exec /usr/bin/tar -xf - -C \"$0\"")
            .arg(&left)
            .stdin(Stdio::piped())
            .spawn()
            .expect("runs");
        std::thread::sleep(Duration::from_millis(100));
        assert!(tar.try_wait().expect("waits").is_none(), "tar waits");
        remove_left_behind_in(&temp, &[]);
        let stopped = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            tar.try_wait().expect("waits").is_some()
        });
        if !stopped {
            tar.kill().ok();
        }
        assert!(stopped, "tar is stopped");
        assert!(!left.exists());
        fs::remove_dir_all(&temp).ok();
    }

    /// A program that waits for its input to end, as a `tar` unpacking an
    /// archive it reads from the app would. On macOS and Linux, like one
    /// the app starts there, it ignores SIGTERM.
    fn waiting_program() -> Command {
        if cfg!(windows) {
            // Reads all its input before it sorts it.
            emu_os::process::command("sort")
        } else {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg("trap '' TERM; exec cat");
            command
        }
    }

    #[test]
    fn quitting_stops_the_programs_working_in_a_work_folder() {
        let temp = temp_dir("workers");
        let folder = WorkFolder::new_in(&temp, "unpack").expect("made");
        let path = folder.path().to_owned();
        let worker = folder
            .spawn(waiting_program().stdin(Stdio::piped()))
            .expect("runs");
        std::thread::sleep(Duration::from_millis(100));
        assert!(worker.try_wait().expect("waits").is_none(), "it waits");
        // As if the app quit: the folder is not dropped.
        std::mem::forget(folder);
        remove_own_where(|held| held == path);
        let status = worker.try_wait().expect("waits");
        assert!(status.is_some_and(|status| !status.success()), "stopped");
        assert!(!path.exists());
        drop(worker);
        assert!(
            !workers().iter().any(|(folder, _)| folder == &path),
            "let go"
        );
        // A worker dropped while it runs is stopped too.
        let folder = WorkFolder::new_in(&temp, "unpack").expect("made");
        let worker = folder
            .spawn(waiting_program().stdin(Stdio::piped()))
            .expect("runs");
        let child = Arc::clone(&worker.child);
        drop(worker);
        assert!(lock(&child).try_wait().expect("waits").is_some(), "stopped");
        drop(folder);
        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn quitting_removes_what_is_under_way() {
        let temp = temp_dir("quit");
        let folder = WorkFolder::new_in(&temp, "store").expect("made");
        let file_path = temp.join("Game.nds.partial");
        let file = Unfinished::create(&file_path).expect("made");
        let folder_path = folder.path().to_owned();
        // As if the app quit: neither is dropped.
        std::mem::forget(folder);
        std::mem::forget(file);
        // Only this test's, as other tests hold their own meanwhile.
        remove_own_where(|path| path.starts_with(&temp));
        assert!(!folder_path.exists() && !file_path.exists());
        assert!(!own().contains(&folder_path) && !own().contains(&file_path));
        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn a_finished_file_is_kept_where_it_was_put() {
        let temp = temp_dir("finish");
        let partial = temp.join("Game.gba.partial");
        let mut file = Unfinished::create(&partial).expect("made");
        io::Write::write_all(file.file(), b"game").expect("written");
        file.finish(&temp.join("Game.gba")).expect("renamed");
        assert_eq!(fs::read(temp.join("Game.gba")).expect("kept"), b"game");
        assert!(!partial.exists());
        assert!(!own().contains(&partial));
        fs::remove_dir_all(&temp).ok();
    }
}
