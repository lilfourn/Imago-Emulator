//! What the app writes for a while and then removes: the folders it
//! unpacks archives, downloads and randomizes games into, in the temporary
//! folder (on Linux, in a folder of the user's own inside it: see
//! [`emu_os::temp`]), and the games it is writing, as `….partial` files
//! beside where they go. A big 3DS game makes either 4 GB, so none may be
//! left behind.
//!
//! Each is locked while the app works in it. The system lets go of a lock
//! when its process ends, crashed or killed, so one that no running app
//! holds is what an app left behind; at launch, those are removed, and on
//! macOS any `tar` still unpacking into one is stopped. On quit, the app
//! removes its own, and stops the programs it started to work in them
//! (see [`WorkFolder::spawn`]). Files the app writes for a moment and keeps
//! no lock on, such as a save's `….tmp`, are removed once they are old,
//! where they are in a folder of the app's own.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Stdio;
use std::process::{Child, ChildStderr, Command, ExitStatus, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use crate::persist;

/// How the names of the work folders start, in the temporary folder:
/// `ds-emulator-unpack-<pid>-<n>`, `ds-emulator-store-<pid>-<n>`,
/// `ds-emulator-randomize-<pid>-<n>`.
const KINDS: [&str; 3] = [
    "ds-emulator-unpack-",
    "ds-emulator-store-",
    "ds-emulator-randomize-",
];

/// How often a program that is waited for is looked at.
const WAIT: Duration = Duration::from_millis(20);

/// The lock file in each work folder, held while the folder is in use.
const LOCK: &str = ".ds-emulator-lock";

/// Where patches unpacked from archives are kept until their games are
/// made, in the folder the work folders are in.
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
    /// The lock file in the folder, open while the folder is in use.
    lock: Option<File>,
}

impl WorkFolder {
    /// A work folder for `kind` of work: `"unpack"`, `"store"` or
    /// `"randomize"`.
    pub fn new(kind: &str) -> io::Result<Self> {
        Self::new_in(&emu_os::temp::work_dir()?, kind)
    }

    /// A work folder for `kind` of work in the folder `temp`.
    pub(crate) fn new_in(temp: &Path, kind: &str) -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = temp.join(format!(
            "ds-emulator-{kind}-{}-{number}",
            std::process::id()
        ));
        remove_folder(&path);
        fs::create_dir_all(&path)?;
        let lock = match emu_os::lock::lock(&path.join(LOCK)) {
            Ok(lock) => lock,
            Err(error) => {
                remove_folder(&path);
                return Err(error);
            }
        };
        hold(&path);
        Ok(Self {
            path,
            lock: Some(lock),
        })
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

    /// Waits for it to end, and returns how it ended and all it printed,
    /// as [`Command::output`] does, for a program started with its output
    /// and errors piped. It can still be stopped while this waits, as when
    /// the app quits.
    pub fn output(&self) -> io::Result<Output> {
        let (stdout, stderr) = {
            let mut child = lock(&self.child);
            (child.stdout.take(), child.stderr.take())
        };
        // Read as it comes, so the program never waits on a full pipe.
        fn all(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
            thread::spawn(move || {
                let mut printed = Vec::new();
                if let Some(mut pipe) = pipe {
                    pipe.read_to_end(&mut printed).ok();
                }
                printed
            })
        }
        let (stdout, stderr) = (all(stdout), all(stderr));
        // Looked at, not waited on: stopping it takes the same lock.
        let status = loop {
            match self.try_wait()? {
                Some(status) => break status,
                None => thread::sleep(WAIT),
            }
        };
        Ok(Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
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
        // Closed first: Windows can't remove a folder while a file in it
        // is open, and the fields would only close after this.
        drop(self.lock.take());
        remove_folder(&self.path);
        let_go(&self.path);
    }
}

/// Removes the work folder at `path` with everything in it, as far as it
/// can. What is in it came out of an archive, which may hold anything:
///
/// - Folders thousands deep, deeper than a path can name (`tar` makes
///   them by changing folder as it goes). The standard library's removal
///   calls itself once for each folder down, with a folder held open at
///   each, and so overflows a small thread's stack, which ends the app,
///   or runs out of open files. On macOS and Linux the app therefore
///   removes the folder itself ([`remove_tree`]), by a list and by short
///   paths. Windows's own removal is a loop over a list already, takes a
///   path of any length and removes read-only files, so it stays.
/// - Folders nobody may open or write in (mode 000, say), under which
///   nothing could be listed or removed: each is first made its owner's.
///
/// Whatever cannot be removed is left, and the rest still removed: this
/// never fails and never ends the app, when work is done, at quit, or at
/// a later launch.
pub(crate) fn remove_folder(path: &Path) {
    #[cfg(unix)]
    remove_tree(path);
    #[cfg(not(unix))]
    {
        fs::remove_dir_all(path).ok();
    }
}

/// The most folders one removal empties: far more than an archive of
/// games unpacks to. It only keeps a removal from going on for ever after
/// a program that is still writing into the folder.
#[cfg(unix)]
const MAX_REMOVED_FOLDERS: u64 = 10_000_000;

/// How the names of folders moved up into a work folder to be removed
/// start, followed by a number.
#[cfg(unix)]
const REMOVING: &str = ".ds-emulator-removing-";

/// How many times over a folder is gone through to remove it. Once does
/// it, unless reading a folder while removing what is in it passed over
/// something, which a system may do.
#[cfg(unix)]
const REMOVAL_PASSES: u32 = 8;

/// Removes the folder at `root` with everything in it, as far as it can,
/// however deep it goes and whatever its folders' permissions.
///
/// It keeps a list of folders to empty, each of them directly in `root`.
/// A folder is emptied by removing its files and moving each folder in it
/// up into `root`, under a name of the app's, to be emptied in its turn;
/// then it is empty, and removed. So every path used is short, a folder a
/// thousand down is reached in a thousand steps of one, and nothing calls
/// itself. A folder that cannot be moved or removed is skipped, and what
/// can be is still removed.
#[cfg(unix)]
fn remove_tree(root: &Path) {
    match fs::symlink_metadata(root) {
        Ok(found) if found.is_dir() => {}
        // A file or a link by the name: it alone, as a folder would be.
        Ok(_) => {
            fs::remove_file(root).ok();
            return;
        }
        Err(_) => return,
    }
    make_own(root);
    let mut moved_up = 0u64;
    let mut emptied = 0u64;
    for _ in 0..REMOVAL_PASSES {
        // What is directly in `root`: its files go, its folders are listed.
        let mut removed = false;
        let mut folders = Vec::new();
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    folders.push(entry.path());
                } else {
                    removed |= fs::remove_file(entry.path()).is_ok();
                }
            }
        }
        while let Some(folder) = folders.pop() {
            emptied += 1;
            if emptied > MAX_REMOVED_FOLDERS {
                return;
            }
            make_own(&folder);
            if let Ok(entries) = fs::read_dir(&folder) {
                for entry in entries.flatten() {
                    let inner = entry.path();
                    if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        removed |= fs::remove_file(&inner).is_ok();
                        continue;
                    }
                    // Its own to write in, as moving a folder to another
                    // changes what is in it: where `..` leads.
                    make_own(&inner);
                    if let Some(up) = move_up(&inner, root, &mut moved_up) {
                        folders.push(up);
                    }
                }
            }
            removed |= fs::remove_dir(&folder).is_ok();
        }
        // Gone, or nothing of what is left can be removed.
        if fs::remove_dir(root).is_ok() || !removed {
            return;
        }
    }
}

/// Moves the folder `inner` up into `root` under a name not taken there,
/// and returns where it is now; `None` if it cannot be moved.
#[cfg(unix)]
fn move_up(inner: &Path, root: &Path, moved_up: &mut u64) -> Option<PathBuf> {
    // A name may be taken by what an earlier removal, cut short, left.
    for _ in 0..1000 {
        let up = root.join(format!("{REMOVING}{moved_up}"));
        *moved_up += 1;
        match fs::rename(inner, &up) {
            Ok(()) => return Some(up),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::DirectoryNotEmpty
                        | io::ErrorKind::AlreadyExists
                        | io::ErrorKind::NotADirectory
                ) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Makes the folder at `path` its owner's to read, write and search, if
/// it is not: an archive can hold one that nobody may open. A link is not
/// followed.
#[cfg(unix)]
fn make_own(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(found) = fs::symlink_metadata(path) else {
        return;
    };
    let mode = found.permissions().mode();
    if found.is_dir() && mode & 0o700 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700)).ok();
    }
}

/// A game being written, `….partial`, locked while it is and removed when
/// dropped unless [`Unfinished::finish`] has renamed it into place.
pub struct Unfinished {
    path: PathBuf,
    file: Option<File>,
}

/// How many writers of one game at once each get a file of their own.
const WRITERS: u32 = 16;

impl Unfinished {
    /// Creates the file at `path`, `….partial`, empty and locked.
    ///
    /// The file is locked before it is emptied, never after: one that is
    /// locked is being written by another unpacking of the same game, or
    /// by another copy of the app, and stays as it is. This writer then
    /// takes a file of its own beside it, `….2.partial` and so on.
    pub fn create(path: &Path) -> io::Result<Self> {
        for number in 1..=WRITERS {
            let path = numbered(path, number);
            let Some(file) = emu_os::lock::try_lock(&path)? else {
                continue;
            };
            // Its own now, whatever an app that quit left in it.
            if let Err(error) = file.set_len(0) {
                drop(file);
                fs::remove_file(&path).ok();
                return Err(error);
            }
            hold(&path);
            return Ok(Self {
                path,
                file: Some(file),
            });
        }
        Err(io::Error::other(format!(
            "{} is being written {WRITERS} times over",
            persist::file_name(path)
        )))
    }

    /// The file, to write to.
    pub fn file(&mut self) -> &mut File {
        self.file.as_mut().expect("open until finished")
    }

    /// Renames the whole file to `path`.
    pub fn finish(mut self, path: &Path) -> io::Result<()> {
        // Renamed while still locked: let go of first, another writer
        // could take the file and empty it before it is moved.
        let renamed = persist::rename(&self.path, path);
        // Unlocked once closed.
        self.file = None;
        // Should Windows refuse to rename a file still open here, it is
        // tried once more closed, as it always was.
        if cfg!(windows) && renamed.is_err() {
            persist::rename(&self.path, path)?;
        } else {
            renamed?;
        }
        let_go(&self.path);
        self.path = PathBuf::new();
        Ok(())
    }
}

/// `path`, a game's `….partial`, for the `number`th of its writers at
/// once: itself for the first, and `….2.partial` and so on after it.
fn numbered(path: &Path, number: u32) -> PathBuf {
    if number == 1 {
        return path.to_owned();
    }
    // Without its `.partial`, and as the system spells it: a game's name
    // need not be Unicode.
    let mut name = path.with_extension("").into_os_string();
    name.push(format!(".{number}{PARTIAL}"));
    PathBuf::from(name)
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
pub(crate) fn remove_own_where(wanted: impl Fn(&Path) -> bool) {
    let held: Vec<PathBuf> = {
        let mut own = own();
        let (held, kept) = own.drain(..).partition(|path| wanted(path));
        *own = kept;
        held
    };
    for path in held {
        if path.is_dir() {
            stop_working_in(&path);
            remove_folder(&path);
        } else {
            fs::remove_file(&path).ok();
        }
    }
}

/// Removes what apps that are no longer running left behind: in the
/// temporary folder, work folders and old unpacked patches; in each of
/// `folders`, where games are written, games half written and old
/// `….tmp` files; and old `….tmp` files in the app's own folders
/// ([`own_folders`]). Says how many bytes that freed.
///
/// A `….tmp` file beside a game somewhere else, in a folder of the
/// user's ROMs, is not looked for: the app does not know those folders
/// until a game in one is opened.
///
/// Blocking file I/O; call it off the UI thread.
pub fn remove_left_behind(folders: &[PathBuf]) -> u64 {
    let own: u64 = own_folders(folders)
        .iter()
        .map(|folder| remove_old_temporaries(folder, OWN_DEPTH))
        .sum();
    own + match emu_os::temp::work_dir() {
        Ok(temp) => remove_left_behind_in(&temp, folders),
        // No folder of the user's own to look in: the games' alone.
        Err(_) => remove_unfinished_games(folders),
    }
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
                remove_folder(&path);
            }
        }
    }
    freed += remove_unused_files(&temp.join(PATCHES), |_| true);
    freed + remove_unfinished_games(folders)
}

/// Removes, in each of `folders`, where games are written, the games half
/// written and old `….tmp` files that no running app is writing, and says
/// how many bytes that freed.
fn remove_unfinished_games(folders: &[PathBuf]) -> u64 {
    let unfinished = |name: &str| name.ends_with(PARTIAL) || name.ends_with(".tmp");
    folders
        .iter()
        .map(|folder| remove_unused_files(folder, unfinished))
        .sum()
}

/// How many folders below one of the app's own a file is written through
/// a temporary one: `Guide/<game>/`, `3DS/azahar-<version>/`,
/// `store/pictures/`. Stray temporary files are looked for no deeper,
/// where a core keeps files of its own.
const OWN_DEPTH: u32 = 2;

/// The folders the app keeps its own files in, written through a
/// temporary file beside each ([`emu_os::file::write_atomic`]): settings,
/// the library and bindings; Guide's key and chats, and the 3DS core; box
/// art, the store's catalogs and pictures, and Guide's pictures in the
/// cache; and the store's games, in `Store` in the games folder, whose
/// saves sit beside them.
fn own_folders(games: &[PathBuf]) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = [
        emu_os::config_dir(),
        emu_os::data_dir(),
        emu_os::cache_dir(),
    ]
    .into_iter()
    .flatten()
    .map(|folder| folder.join("ds-emulator"))
    .chain(games.iter().map(|folder| folder.join("Store")))
    .collect();
    // macOS keeps settings and data in one folder.
    folders.sort();
    folders.dedup();
    folders
}

/// Removes the `….tmp` files in `folder`, and in the folders up to `depth`
/// below it, that have not changed for an hour, and says how many bytes
/// that freed: what a crash left between writing a file and renaming it
/// into place. Links are not followed.
fn remove_old_temporaries(folder: &Path, depth: u32) -> u64 {
    let mut freed = remove_unused_files(folder, |name| name.ends_with(".tmp"));
    if depth > 0
        && let Ok(entries) = fs::read_dir(folder)
    {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                freed += remove_old_temporaries(&entry.path(), depth - 1);
            }
        }
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
        // Asked first, by what reading the folder already told: nothing
        // else in the folder is looked at, let alone opened.
        let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
        if !(is_file && wanted(&name)) {
            continue;
        }
        // A game being written is locked; anything else may be written
        // without a lock, so only an old one is done with.
        let unused = if name.ends_with(PARTIAL) {
            !in_use(&path, &path)
        } else {
            !changed_lately(&path)
        };
        if unused {
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
    // The name is read from disk and goes into a pattern, so only one the
    // app gives a folder is used: letters, digits and dashes, which need
    // no escaping. Any other could be a pattern of its own, and stop the
    // `tar` of a folder in use. It ends the folder's path, before a slash
    // or a space.
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return;
    }
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

/// For tests: a chain of `levels` folders at `dir/name`, each called
/// `name` and holding the next, with the five bytes of `Game.gb` in the
/// innermost. Returns the outermost.
///
/// It is built from the bottom up, each step by a short path: the
/// innermost folder is made first, then each new folder is made beside
/// the chain so far and the chain renamed into it. So it can be deeper
/// than any path can name, which no `create_dir_all` could make, and
/// which is how `tar` leaves one, changing folder as it goes.
///
/// Each rename is the one that tries again on Windows, where a virus
/// scanner may still have the new game open, which holds every folder
/// above it in place for a moment.
#[cfg(test)]
pub(crate) fn deep_tree(dir: &Path, levels: usize, name: &str) -> PathBuf {
    let mut chain = dir.join("level-0");
    fs::create_dir_all(&chain).expect("innermost");
    fs::write(chain.join("Game.gb"), b"12345").expect("game");
    for level in 1..levels {
        let outer = dir.join(format!("level-{level}"));
        fs::create_dir(&outer).expect("outer");
        persist::rename(&chain, &outer.join(name)).expect("moved in");
        chain = outer;
    }
    let top = dir.join(name);
    persist::rename(&chain, &top).expect("named");
    top
}

/// For tests: runs `work` on a thread with a 256 KiB stack, as small as
/// any the app's work runs on (macOS gives a background worker 512 KiB),
/// and returns what it gave. A walk that calls itself once for each
/// folder overflows it a few dozen folders down, which ends the process.
#[cfg(test)]
pub(crate) fn on_a_small_stack<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn_scoped(scope, work)
            .expect("a thread")
            .join()
            .expect("no panic")
    })
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

/// The most files and folders [`size`] looks at: what it gives is only
/// said in a log, and a launch should not wait on millions.
const MAX_SIZED: u64 = 1_000_000;

/// How many bytes the file, or the files in the folder, at `path` take.
///
/// It keeps a list of the folders still to look in and calls nothing for
/// each folder down: what an unpacking cut short left can be thousands of
/// folders deep, and this runs at every launch. A folder too deep for a
/// path to name, or that cannot be read, counts for nothing.
fn size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    let mut bytes = 0;
    let mut looked_at = 0u64;
    let mut folders = vec![path.to_owned()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            looked_at += 1;
            if looked_at > MAX_SIZED {
                return bytes;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(entry.path()),
                // A link's own size, not what it leads to.
                Ok(_) => bytes += entry.metadata().map_or(0, |metadata| metadata.len()),
                Err(_) => {}
            }
        }
    }
    bytes
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
        // A randomizer's run, from before its folders had locks, and one
        // under way.
        let random = temp.join("ds-emulator-randomize-1-0");
        fs::create_dir_all(&random).expect("folder");
        fs::write(random.join("randomized.nds"), [0; 30]).expect("game");
        age(&random, Duration::from_secs(2 * 60 * 60));
        let running = WorkFolder::new_in(&temp, "randomize").expect("made");
        // Not the app's own.
        let other = temp.join("ds-emulator-shaders-1-0");
        fs::create_dir_all(&other).expect("folder");
        age(&other, Duration::from_secs(2 * 60 * 60));
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
        assert_eq!(freed, 1000 + 10 + 30 + 5 + 100 + 20);
        assert!(!left.exists() && !old.exists() && !random.exists());
        assert!(new.exists() && other.exists());
        assert!(running.path().exists(), "a run under way keeps its folder");
        drop(running);
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

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "Linux, Windows: only macOS unpacks .7z files with tar, and stops a crashed app's one at launch"
    )]
    fn a_folder_s_name_is_never_a_pattern_that_stops_another_folder_s_tar() {
        let temp = temp_dir("tar-pattern");
        // A folder in use: its lock is held, and a `tar` unpacks into it.
        let lock_and_tar = |name: &str| {
            let folder = temp.join(name);
            fs::create_dir_all(&folder).expect("folder");
            let lock = emu_os::lock::lock(&folder.join(LOCK)).expect("locked");
            let tar = Command::new("/bin/sh")
                .arg("-c")
                .arg("trap '' TERM; exec /usr/bin/tar -xf - -C \"$0\"")
                .arg(&folder)
                .stdin(Stdio::piped())
                .spawn()
                .expect("runs");
            (lock, tar)
        };
        let (lock, mut tar) =
            lock_and_tar(&format!("ds-emulator-unpack-{}-97", std::process::id()));
        // Left behind beside it, a folder whose name, read as a pattern,
        // takes in every folder the app unpacks into.
        let odd = temp.join("ds-emulator-unpack-.*");
        fs::create_dir_all(&odd).expect("folder");
        fs::write(odd.join(LOCK), b"").expect("lock");
        std::thread::sleep(Duration::from_millis(100));
        remove_left_behind_in(&temp, &[]);
        std::thread::sleep(Duration::from_millis(300));
        let still_unpacking = tar.try_wait().expect("waits").is_none();
        tar.kill().ok();
        tar.wait().ok();
        assert!(still_unpacking, "the tar of a folder in use was stopped");
        assert!(
            !odd.exists(),
            "the folder left behind is removed all the same"
        );
        drop(lock);
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
    #[cfg_attr(not(unix), ignore = "Windows: folders have no permission bits")]
    fn a_work_folder_holding_folders_nobody_may_open_is_removed_all_the_same() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let temp = temp_dir("closed-folders");
            // As an archive can unpack: a folder nobody may open, and one
            // nobody may write in, each with something in it.
            let fill = |work: &Path| {
                for (folder, mode) in [("closed", 0o000), ("read-only", 0o500)] {
                    let folder = work.join("Game").join(folder);
                    fs::create_dir_all(folder.join("inner")).expect("folders");
                    fs::write(folder.join("inner/Game.3ds"), [0; 100]).expect("file");
                    fs::set_permissions(folder.join("inner"), fs::Permissions::from_mode(mode))
                        .expect("mode");
                    fs::set_permissions(&folder, fs::Permissions::from_mode(mode)).expect("mode");
                }
            };
            // When the work is done, or stopped.
            let folder = WorkFolder::new_in(&temp, "unpack").expect("made");
            let path = folder.path().to_owned();
            fill(&path);
            drop(folder);
            assert!(!path.exists(), "removed when done");
            // When the app quits.
            let folder = WorkFolder::new_in(&temp, "unpack").expect("made");
            let path = folder.path().to_owned();
            fill(&path);
            std::mem::forget(folder);
            remove_own_where(|held| held == path);
            assert!(!path.exists(), "removed on quit");
            // And one an app that was killed left behind, at a later launch.
            let left = temp.join("ds-emulator-unpack-1-0");
            fs::create_dir_all(&left).expect("folder");
            fs::write(left.join(LOCK), b"").expect("lock");
            fill(&left);
            remove_left_behind_in(&temp, &[]);
            assert!(!left.exists(), "removed at launch");
            fs::remove_dir_all(&temp).ok();
        }
    }

    #[test]
    fn old_temporary_files_in_the_app_s_own_folders_are_removed() {
        let temp = temp_dir("own-folders");
        let old = Duration::from_secs(2 * 60 * 60);
        // The app's data folder, as a crash between writing a file and
        // renaming it into place leaves it.
        let data = temp.join("ds-emulator");
        let file = |path: &str, bytes: usize, aged: bool| {
            let path = data.join(path);
            fs::create_dir_all(path.parent().expect("folder")).expect("folder");
            fs::write(&path, vec![0; bytes]).expect("written");
            if aged {
                age(&path, old);
            }
            path
        };
        let removed = [
            file("settings.toml.4242-7.tmp", 1, true),
            // From a build that named them so.
            file("library.toml.tmp", 2, true),
            // It holds the key.
            file("OpenRouter/auth.json.4242-0.tmp", 4, true),
            file("Guide/Platinum/chat-1.json.4242-3.tmp", 8, true),
            file(
                "3DS/azahar-2126.2/azahar_libretro.dylib.4242-1.tmp",
                16,
                true,
            ),
            file("store/pictures/0000ABCD-12.png.4242-9.tmp", 32, true),
        ];
        let kept = [
            file("settings.toml", 1, true),
            file("OpenRouter/auth.json", 1, true),
            file("Guide/Platinum/chat-1.json", 1, true),
            // Being written now, by this app or another copy of it.
            file("OpenRouter/auth.json.5151-0.tmp", 1, false),
            // Deeper than the app writes: a core's own files.
            file("3DS/Azahar/sdmc/title/save.tmp", 1, true),
        ];
        // A folder is no file, whatever its name.
        fs::create_dir_all(data.join("Guide/Game.tmp")).expect("folder");
        age(&data.join("Guide/Game.tmp"), old);

        let freed = remove_old_temporaries(&data, 2);
        assert_eq!(freed, 1 + 2 + 4 + 8 + 16 + 32);
        for path in &removed {
            assert!(!path.exists(), "{} is removed", path.display());
        }
        for path in &kept {
            assert!(path.exists(), "{} is kept", path.display());
        }
        assert!(data.join("Guide/Game.tmp").is_dir());
        // A folder that is not there is nothing to look in.
        assert_eq!(remove_old_temporaries(&temp.join("missing"), 2), 0);
        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn a_work_folder_is_measured_and_removed_however_deep_it_goes() {
        let temp = temp_dir("deep");
        // 300 folders down: within what a path can name, and far past
        // where a walk that calls itself runs out of stack.
        let folder = WorkFolder::new_in(&temp, "unpack").expect("made");
        let path = folder.path().to_owned();
        deep_tree(&path, 300, "d");
        assert_eq!(
            on_a_small_stack(|| size(&path)),
            5,
            "the game at the bottom"
        );
        on_a_small_stack(|| drop(folder));
        assert!(!path.exists(), "removed when done");
        remove_folder(&temp);
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: a path there may be 32,767 characters, and its own removal is a loop"
    )]
    fn a_tree_deeper_than_a_path_can_name_is_removed() {
        let temp = temp_dir("deeper");
        // 1,500 folders of ten letters each: 16,500 bytes to the bottom,
        // where a path may be 1,024 on macOS and 4,096 on Linux.
        let fill = |work: &Path| {
            deep_tree(work, 1_500, "a-folder-b");
            fs::write(work.join("Game.3ds"), [0; 100]).expect("game");
            // What a removal cut short left, under the names it moves
            // folders up to: a folder with something in it, and a file.
            let taken = work.join(".ds-emulator-removing-0");
            fs::create_dir_all(taken.join("inner")).expect("folders");
            fs::write(taken.join("inner").join("Game.cia"), [0; 10]).expect("game");
            fs::write(work.join(".ds-emulator-removing-1"), [0; 10]).expect("file");
        };
        // When the work is done, or stopped.
        let folder = WorkFolder::new_in(&temp, "unpack").expect("made");
        let path = folder.path().to_owned();
        fill(&path);
        on_a_small_stack(|| drop(folder));
        assert!(!path.exists(), "removed when done");
        // When the app quits.
        let folder = WorkFolder::new_in(&temp, "unpack").expect("made");
        let path = folder.path().to_owned();
        fill(&path);
        std::mem::forget(folder);
        on_a_small_stack(|| remove_own_where(|held| held == path));
        assert!(!path.exists(), "removed on quit");
        remove_folder(&temp);
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: a path there may be 32,767 characters, and its own removal is a loop"
    )]
    fn the_launch_clean_up_survives_whatever_a_crashed_unpack_left() {
        let temp = temp_dir("crashed");
        // As the app left it when it was ended part way: its lock free,
        // and in it folders 3,000 down, deeper than a path can name.
        let left = temp.join("ds-emulator-unpack-1-0");
        fs::create_dir_all(&left).expect("folder");
        fs::write(left.join(LOCK), b"").expect("lock");
        fs::write(left.join("Game.3ds"), [0; 1000]).expect("game");
        deep_tree(&left, 3_000, "a-folder-b");
        let freed = on_a_small_stack(|| remove_left_behind_in(&temp, &[]));
        assert!(freed >= 1000, "what could be measured: {freed}");
        assert!(!left.exists(), "removed at launch");
        // And again, as at every later launch: nothing to trip on.
        assert_eq!(on_a_small_stack(|| remove_left_behind_in(&temp, &[])), 0);
        remove_folder(&temp);
    }

    #[test]
    #[cfg_attr(windows, ignore = "Windows: a named pipe is not a file in a folder")]
    fn the_sweep_opens_only_the_files_it_is_looking_for() {
        #[cfg(unix)]
        {
            use std::sync::mpsc;

            let temp = temp_dir("sweep");
            // Not a file, though named as a game being written is: a pipe,
            // which opening would wait on for ever. And a file the sweep is
            // not asked for, which it has no reason to look at.
            let made = Command::new("mkfifo")
                .arg(temp.join("Game.3ds.partial"))
                .status()
                .expect("mkfifo runs");
            assert!(made.success());
            fs::write(temp.join("Other.gba.partial"), [0; 7]).expect("written");
            fs::write(temp.join("settings.toml.1-0.tmp"), [0; 3]).expect("written");
            age(
                &temp.join("settings.toml.1-0.tmp"),
                Duration::from_secs(2 * 60 * 60),
            );
            let (done, freed) = mpsc::channel();
            let folder = temp.clone();
            std::thread::spawn(move || {
                done.send(remove_unused_files(&folder, |name| name.ends_with(".tmp")))
            });
            let freed = freed.recv_timeout(Duration::from_secs(5));
            assert_eq!(freed, Ok(3), "the old temporary file, and no waiting");
            assert!(temp.join("Other.gba.partial").exists(), "not asked for");
            remove_folder(&temp);
        }
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
    fn a_second_writer_of_a_game_leaves_the_first_s_file_alone() {
        use std::sync::mpsc;

        let temp = temp_dir("second-writer");
        let partial = temp.join("Game.3ds.partial");
        let mut first = Unfinished::create(&partial).expect("made");
        io::Write::write_all(first.file(), b"half a game").expect("written");
        // Another unpacking of the same game, or another copy of the app,
        // starts writing it: on a thread of its own, as it once waited for
        // the first here, after emptying its file.
        let (made, second) = mpsc::channel();
        let again = partial.clone();
        std::thread::spawn(move || made.send(Unfinished::create(&again)));
        let second = second.recv_timeout(Duration::from_secs(2));
        // Measured, not read: on Windows nobody but its holder can read a
        // locked file. What is in it is read once it is in place, below.
        let length = |path: &Path| fs::metadata(path).expect("there").len();
        assert_eq!(
            length(&partial),
            b"half a game".len() as u64,
            "the second writer emptied the file the first is writing"
        );
        let mut second = second
            .expect("the second writer does not wait")
            .expect("made");
        assert_eq!(second.path, temp.join("Game.3ds.2.partial"), "its own file");
        io::Write::write_all(second.file(), b"the same game").expect("written");
        // Each is whole when it is put in place, the later over the earlier.
        io::Write::write_all(first.file(), b", and the rest").expect("written");
        let game = temp.join("Game.3ds");
        first.finish(&game).expect("renamed");
        assert_eq!(fs::read(&game).expect("kept"), b"half a game, and the rest");
        second.finish(&game).expect("renamed");
        assert_eq!(fs::read(&game).expect("kept"), b"the same game");
        let left: Vec<_> = fs::read_dir(&temp).expect("folder").flatten().collect();
        assert_eq!(left.len(), 1, "the game alone: {left:?}");
        // A file left by an app that quit, which nobody holds, is taken
        // over and emptied.
        fs::write(&partial, b"left behind").expect("left");
        let mut third = Unfinished::create(&partial).expect("made");
        assert_eq!(third.path, partial);
        assert_eq!(length(&partial), 0, "emptied");
        io::Write::write_all(third.file(), b"new").expect("written");
        drop(third);
        assert!(!partial.exists());
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
