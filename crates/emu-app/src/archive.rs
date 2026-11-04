//! Games downloaded as archives: `.zip` files, and `.7z` and `.rar`
//! ones, which macOS's own `tar` (libarchive) unpacks. The ROMs inside
//! are unpacked into the app's own games folder, where they and their
//! saves stay after the download is deleted, written as they are read, as
//! a 3DS game can be 4 GB. ROM-hack patches inside are unpacked into a
//! temporary folder, for [`crate::romhack`] to make their games from.
//! How far an unpacking has got is told through a [`Progress`], as a big
//! 3DS game takes most of a minute.

use std::fs::{self, File};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use zip::ZipArchive;

use crate::persist;
use crate::romhack;

/// The most a ROM unpacks to: a DS cartridge holds at most 512 MiB, so a
/// bigger file is not a game. Patches are held to it too.
pub const MAX_ROM: u64 = 512 << 20;

/// The most a 3DS game unpacks to: a 3DS cartridge holds at most 4 GiB.
pub const MAX_3DS_GAME: u64 = 4 << 30;

/// The archives games come in, by extension.
const ARCHIVES: [&str; 3] = ["zip", "7z", "rar"];

/// Where unpacked games are kept, e.g.
/// `~/Library/Application Support/ds-emulator/Games` on macOS.
pub fn games_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("ds-emulator").join("Games"))
}

/// Where patches unpacked from archives are kept until their games are
/// made.
fn patches_dir() -> PathBuf {
    std::env::temp_dir().join("ds-emulator-patches")
}

/// How often the files `tar` writes are measured while it unpacks.
const MEASURE_EVERY: Duration = Duration::from_millis(100);

/// How much of an archive has been unpacked, in bytes, set by the thread
/// unpacking it and read by the one showing it.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
}

impl Progress {
    /// The share unpacked, from 0 to 1, once the archive's size is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        let done = self.done.load(Ordering::Relaxed).min(total);
        (total > 0).then(|| done as f32 / total as f32)
    }

    /// A progress `done` bytes of `total` along, for tests.
    #[cfg(test)]
    pub fn at(done: u64, total: u64) -> Self {
        let progress = Self::default();
        progress.start(total);
        progress.set(done);
        progress
    }

    fn start(&self, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    fn set(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
    }

    fn add(&self, done: u64) {
        self.done.fetch_add(done, Ordering::Relaxed);
    }
}

/// A reader that counts what it reads into a [`Progress`].
struct Counted<'a, R> {
    inner: R,
    progress: &'a Progress,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.progress.add(read as u64);
        Ok(read)
    }
}

/// Whether `path` is an archive, judged by its extension.
pub fn is_archive(path: &Path) -> bool {
    has_extension(path, &ARCHIVES)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Unpacks the games and patches in the archive at `archive`, games into
/// the games folder, and returns where each is now: at least one, in the
/// archive's order. Tells `progress` how far it has got.
///
/// Blocking file I/O; call it off the UI thread.
///
/// # Errors
///
/// Fails if the archive cannot be read, holds no game or patch, or a
/// file cannot be written.
pub fn unpack(archive: &Path, progress: &Progress) -> Result<Vec<PathBuf>, String> {
    let folder = games_dir().ok_or("There is no folder to unpack games into")?;
    unpack_into(archive, &folder, &patches_dir(), progress)
}

/// Unpacks the archive at `archive` as [`unpack`] does, but its games into
/// `folder`.
///
/// # Errors
///
/// As [`unpack`].
pub fn unpack_to(archive: &Path, folder: &Path) -> Result<Vec<PathBuf>, String> {
    unpack_into(archive, folder, &patches_dir(), &Progress::default())
}

fn unpack_into(
    archive: &Path,
    folder: &Path,
    patches: &Path,
    progress: &Progress,
) -> Result<Vec<PathBuf>, String> {
    let unpacked = if has_extension(archive, &["zip"]) {
        unpack_zip(archive, folder, patches, progress)
    } else {
        unpack_with_tar(archive, folder, patches, progress)
    };
    let games =
        unpacked.map_err(|error| format!("Cannot unpack {}: {error}", archive.display()))?;
    if games.is_empty() {
        return Err(format!(
            "{} holds no .gb, .gbc, .gba, .nds or .3ds game, or patch for one",
            archive.display()
        ));
    }
    Ok(games)
}

/// Whether to unpack the file `inner` from an archive: a game or a patch,
/// but not macOS's copy of a file's metadata under `__MACOSX`, which has
/// the file's own name and extension.
fn wanted(inner: &Path) -> bool {
    !inner.starts_with("__MACOSX")
        && (emu_cores::system_for_path(inner).is_some() || romhack::is_patch(inner))
}

/// The most the game or patch called `name` unpacks to.
fn size_cap(name: &Path) -> u64 {
    if emu_3ds::is_game(name) {
        MAX_3DS_GAME
    } else {
        MAX_ROM
    }
}

fn unpack_zip(
    zip: &Path,
    folder: &Path,
    patches: &Path,
    progress: &Progress,
) -> io::Result<Vec<PathBuf>> {
    let mut archive = ZipArchive::new(File::open(zip)?).map_err(io::Error::other)?;
    let mut total = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(io::Error::other)?;
        let name = entry.enclosed_name();
        if entry.is_file() && name.is_some_and(|inner| wanted(&inner)) {
            total += entry.size();
        }
    }
    progress.start(total);
    let mut games = Vec::new();
    let mut done = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(io::Error::other)?;
        let name = entry
            .enclosed_name()
            .filter(|inner| entry.is_file() && wanted(inner))
            .and_then(|inner| inner.file_name().map(PathBuf::from));
        let Some(name) = name else {
            continue;
        };
        let size = entry.size();
        let mut counted = Counted {
            inner: &mut entry,
            progress,
        };
        let game = if romhack::is_patch(&name) {
            keep_patch(&read_capped(&mut counted, &name)?, patches, &name)?
        } else {
            if size > size_cap(&name) {
                return Err(too_big(&name));
            }
            let crc32 = counted.inner.crc32();
            store(folder, &name, crc32, &mut counted)?
        };
        // All of it, even if a copy already unpacked was used instead.
        done += size;
        progress.set(done);
        games.push(game);
    }
    Ok(games)
}

/// Unpacks a `.7z` or `.rar` file with `/usr/bin/tar`, which on macOS is
/// libarchive's `bsdtar`: it reads both, and by default writes nothing
/// outside the folder it is given. It unpacks everything into a scratch
/// folder, from which the games and patches are taken in name order.
/// How far it has got is the size of what it has written so far, out of
/// the size of what the archive lists.
fn unpack_with_tar(
    archive: &Path,
    folder: &Path,
    patches: &Path,
    progress: &Progress,
) -> io::Result<Vec<PathBuf>> {
    let scratch = Scratch::new()?;
    progress.start(listed_size(archive));
    let mut tar = Command::new("/usr/bin/tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(&scratch.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    // Read as it comes, so `tar` never waits on a full pipe.
    let stderr = tar.stderr.take();
    let said = thread::spawn(move || {
        let mut said = String::new();
        if let Some(mut stderr) = stderr {
            stderr.read_to_string(&mut said).ok();
        }
        said
    });
    let status = loop {
        match tar.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                progress.set(folder_size(&scratch.0));
                thread::sleep(MEASURE_EVERY);
            }
            Err(error) => {
                tar.kill().ok();
                tar.wait().ok();
                return Err(error);
            }
        }
    };
    let said = said.join().unwrap_or_default();
    if !status.success() {
        // `tar: Error opening archive: …`
        let said = said.trim().trim_start_matches("tar: ");
        return Err(io::Error::other(said.to_owned()));
    }
    progress.set(folder_size(&scratch.0));
    let mut files = Vec::new();
    find_files(&scratch.0, &scratch.0, &mut files)?;
    files.sort();
    let mut games = Vec::new();
    for inner in files.iter().filter(|inner| wanted(inner)) {
        let Some(name) = inner.file_name().map(PathBuf::from) else {
            continue;
        };
        let path = scratch.0.join(inner);
        let game = if romhack::is_patch(&name) {
            keep_patch(
                &read_capped(&mut File::open(&path)?, &name)?,
                patches,
                &name,
            )?
        } else {
            if fs::metadata(&path)?.len() > size_cap(&name) {
                return Err(too_big(&name));
            }
            store(folder, &name, file_crc32(&path)?, File::open(&path)?)?
        };
        games.push(game);
    }
    Ok(games)
}

/// The size of the files the archive at `archive` holds, unpacked, as
/// `tar -tv` lists them, or 0 if it cannot list them. Each file's line
/// reads like `ls -l`'s, its size the fifth word:
/// `-rw-r--r--  0 0      0  2147483648 Dec 25  1996 Game.3ds`.
fn listed_size(archive: &Path) -> u64 {
    let listed = Command::new("/usr/bin/tar")
        .arg("-tvf")
        .arg(archive)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Some(listed) = listed.ok().filter(|listed| listed.status.success()) else {
        return 0;
    };
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| line.starts_with('-'))
        .filter_map(|line| line.split_whitespace().nth(4)?.parse::<u64>().ok())
        .sum()
}

/// The size of the files under `folder`, without following links. What
/// cannot be read counts for nothing.
fn folder_size(folder: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(folder) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => folder_size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |metadata| metadata.len()),
            _ => 0,
        })
        .sum()
}

/// Adds the files under `folder`, a folder in `root`, to `files`, as
/// paths within `root`. Links are not followed.
fn find_files(root: &Path, folder: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            find_files(root, &entry.path(), files)?;
        } else if kind.is_file()
            && let Ok(inner) = entry.path().strip_prefix(root)
        {
            files.push(inner.to_owned());
        }
    }
    Ok(())
}

/// A new, empty temporary folder, removed with everything in it when
/// dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ds-emulator-unpack-{}-{number}",
            std::process::id()
        ));
        fs::remove_dir_all(&path).ok();
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// All of `file`, a patch called `name`, unless it is too big to be one.
fn read_capped(file: &mut impl Read, name: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_ROM + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ROM {
        return Err(too_big(name));
    }
    Ok(bytes)
}

/// Why the file called `name` is not unpacked.
fn too_big(name: &Path) -> io::Error {
    io::Error::other(format!("{} is too big to be a game", name.display()))
}

/// The CRC-32 of the file at `path`, read a piece at a time.
pub fn file_crc32(path: &Path) -> io::Result<u32> {
    let mut file = File::open(path)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(hasher.finalize()),
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Writes `contents`, the game called `name`, to `path` as it reads it,
/// through a temporary file, so `path` is only ever the whole game. Stops
/// past `cap` bytes, leaving nothing behind, as it is too big to be a game.
fn write_capped(path: &Path, contents: impl Read, name: &Path, cap: u64) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let written = File::create(&temporary).and_then(|mut file| {
        let written = io::copy(&mut contents.take(cap + 1), &mut file)?;
        if written > cap {
            return Err(too_big(name));
        }
        file.flush()?;
        file.sync_all()
    });
    if let Err(error) = written {
        fs::remove_file(&temporary).ok();
        return Err(error);
    }
    fs::rename(&temporary, path)
}

/// Writes the patch `bytes` into `patches` as `name`, replacing any patch
/// of that name, and returns its path.
fn keep_patch(bytes: &[u8], patches: &Path, name: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(patches)?;
    let path = patches.join(name);
    persist::write_atomic(&path, bytes)?;
    Ok(path)
}

/// Writes the ROM whose CRC-32 is `crc32` into `folder` as `name`, read
/// from `contents` as it is written, and returns its path. The same game
/// written before is used as it is, and `contents` is not read; a
/// different one of that name is left alone, as it may have saves beside
/// it, and this one is numbered: `Game (2).gba`.
///
/// # Errors
///
/// Fails if a file cannot be read or written, or the game is bigger than
/// a cartridge of its system holds.
pub fn store(folder: &Path, name: &Path, crc32: u32, contents: impl Read) -> io::Result<PathBuf> {
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let extension = name.extension().unwrap_or_default().to_string_lossy();
    for number in 1.. {
        let path = if number == 1 {
            folder.join(name)
        } else {
            folder.join(format!("{stem} ({number}).{extension}"))
        };
        match file_crc32(&path) {
            Ok(existing) if existing == crc32 => return Ok(path),
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::create_dir_all(folder)?;
        write_capped(&path, contents, name, size_cap(name))?;
        return Ok(path);
    }
    unreachable!("a free name is always found")
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;

    /// A zip file in `dir` named `name` holding `files`, compressed as
    /// downloads are.
    fn zip_file(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (file, bytes) in files {
            writer.start_file(*file, deflated).expect("start");
            writer.write_all(bytes).expect("write");
        }
        let bytes = writer.finish().expect("finish").into_inner();
        let path = dir.join(name);
        fs::write(&path, bytes).expect("zip written");
        path
    }

    /// Taken by every test that unpacks a 7-Zip file: unpacking one makes
    /// a scratch folder in the shared temporary folder, and a test that
    /// counts those must not see another test's, made at the same moment.
    static SCRATCH: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scratch_lock() -> std::sync::MutexGuard<'static, ()> {
        SCRATCH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A 7-Zip file in `dir` named `name` holding `files`, made by
    /// macOS's `tar`, which writes 7-Zip as well as reading it.
    fn seven_zip_file(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let contents = dir.join(format!("{name} contents"));
        for (file, bytes) in files {
            let path = contents.join(file);
            fs::create_dir_all(path.parent().expect("folder")).expect("folder");
            fs::write(path, bytes).expect("file");
        }
        let path = dir.join(name);
        let status = Command::new("/usr/bin/tar")
            .args(["--format", "7zip", "-cf"])
            .arg(&path)
            .arg("-C")
            .arg(&contents)
            .arg(".")
            .status()
            .expect("tar runs");
        assert!(status.success());
        path
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-app-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn recognises_archives_by_extension() {
        assert!(is_archive(Path::new("Tetris (World).ZIP")));
        assert!(is_archive(Path::new("Platinum.7z")));
        assert!(is_archive(Path::new("Platinum.rar")));
        assert!(!is_archive(Path::new("tetris.gb")));
        assert!(!is_archive(Path::new("zip")));
    }

    #[test]
    fn unpacks_only_the_games_and_patches() {
        let dir = temp_dir("unpack-games");
        let (games, patches) = (dir.join("Games"), dir.join("Patches"));
        let zip = zip_file(
            &dir,
            "Tetris.zip",
            &[
                ("Tetris/Tetris (World).gb", b"tetris"),
                ("__MACOSX/Tetris/._Tetris (World).gb", b"metadata"),
                ("Tetris/readme.txt", b"hello"),
                ("Tetris/Tetris DX.ips", b"PATCHEOF"),
            ],
        );
        let progress = Progress::default();
        assert_eq!(
            progress.fraction(),
            None,
            "no share before the size is known"
        );
        let unpacked = unpack_into(&zip, &games, &patches, &progress).expect("unpacks");
        assert_eq!(
            unpacked,
            [
                games.join("Tetris (World).gb"),
                patches.join("Tetris DX.ips")
            ]
        );
        assert_eq!(fs::read(&unpacked[0]).expect("unpacked"), b"tetris");
        assert_eq!(fs::read(&unpacked[1]).expect("unpacked"), b"PATCHEOF");
        assert_eq!(fs::read_dir(&games).expect("games").count(), 1);
        // The game and the patch, not the notes or macOS's metadata.
        assert_eq!(progress.total.load(Ordering::Relaxed), 6 + 8);
        assert_eq!(progress.fraction(), Some(1.0));
        // Unpacked again, the game already there is not read, but counts.
        let again = Progress::default();
        unpack_into(&zip, &games, &patches, &again).expect("unpacks again");
        assert_eq!(again.fraction(), Some(1.0));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpacks_7_zip_files_with_tar() {
        let _scratch = scratch_lock();
        let dir = temp_dir("unpack-7z");
        let (games, patches) = (dir.join("Games"), dir.join("Patches"));
        let archive = seven_zip_file(
            &dir,
            "Platinum.7z",
            &[
                ("b/Hack.xdelta", b"\xD6\xC3\xC4\x00"),
                ("a/Platinum.nds", b"platinum"),
                ("notes.txt", b"hello"),
            ],
        );
        assert_eq!(listed_size(&archive), 4 + 8 + 5, "every file is listed");
        let progress = Progress::default();
        let unpacked = unpack_into(&archive, &games, &patches, &progress).expect("unpacks");
        assert_eq!(
            unpacked,
            [games.join("Platinum.nds"), patches.join("Hack.xdelta")]
        );
        assert_eq!(fs::read(&unpacked[0]).expect("unpacked"), b"platinum");
        assert_eq!(progress.fraction(), Some(1.0));
        let leftovers = fs::read_dir(std::env::temp_dir())
            .expect("temp")
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(&format!("ds-emulator-unpack-{}-", std::process::id()))
            })
            .count();
        assert_eq!(leftovers, 0, "the scratch folder is removed");
        let broken = dir.join("broken.7z");
        fs::write(&broken, b"not a 7-Zip file").expect("written");
        assert_eq!(listed_size(&broken), 0);
        assert!(
            unpack_into(&broken, &games, &patches, &Progress::default())
                .unwrap_err()
                .starts_with("Cannot unpack")
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpacks_3ds_games_up_to_a_3ds_cartridge_s_size() {
        let _scratch = scratch_lock();
        let dir = temp_dir("unpack-3ds");
        let (games, patches) = (dir.join("Games"), dir.join("Patches"));
        let zip = zip_file(
            &dir,
            "Homebrew.zip",
            &[
                ("Homebrew/Homebrew.3dsx", b"3DSX"),
                ("Homebrew/Game.cia", b"cia"),
            ],
        );
        let unpacked = unpack_into(&zip, &games, &patches, &Progress::default()).expect("unpacks");
        assert_eq!(
            unpacked,
            [games.join("Homebrew.3dsx"), games.join("Game.cia")]
        );
        assert_eq!(fs::read(&unpacked[0]).expect("unpacked"), b"3DSX");
        assert_eq!(size_cap(Path::new("Game.cia")), MAX_3DS_GAME);
        let seven = seven_zip_file(&dir, "Cartridge.7z", &[("Cartridge.3ds", b"NCSD")]);
        let unpacked =
            unpack_into(&seven, &games, &patches, &Progress::default()).expect("unpacks");
        assert_eq!(unpacked, [games.join("Cartridge.3ds")]);

        assert_eq!(size_cap(Path::new("Cartridge.3DS")), 4 << 30);
        assert_eq!(size_cap(Path::new("Homebrew.z3dsx")), MAX_3DS_GAME);
        assert_eq!(size_cap(Path::new("Platinum.nds")), MAX_ROM);
        assert_eq!(size_cap(Path::new("Hack.bps")), MAX_ROM);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_game_too_big_for_its_cartridge_leaves_nothing_behind() {
        let dir = temp_dir("unpack-too-big");
        let path = dir.join("Game.3ds");
        let name = Path::new("Game.3ds");
        let error = write_capped(&path, &b"12345"[..], name, 4).expect_err("too big");
        assert_eq!(error.to_string(), "Game.3ds is too big to be a game");
        assert_eq!(fs::read_dir(&dir).expect("folder").count(), 0);
        write_capped(&path, &b"1234"[..], name, 4).expect("just fits");
        assert_eq!(fs::read(&path).expect("written"), b"1234");
        assert_eq!(file_crc32(&path).expect("read"), crc32fast::hash(b"1234"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpacking_again_reuses_the_game_and_keeps_a_different_one() {
        let dir = temp_dir("unpack-again");
        let (games, patches) = (dir.join("Games"), dir.join("Patches"));
        let first = zip_file(&dir, "a.zip", &[("Game.gba", b"first")]);
        let second = zip_file(&dir, "b.zip", &[("Game.gba", b"second")]);
        let path = games.join("Game.gba");
        assert_eq!(
            unpack_into(&first, &games, &patches, &Progress::default()).expect("first"),
            [path.as_path()]
        );
        assert_eq!(
            unpack_into(&first, &games, &patches, &Progress::default()).expect("again"),
            [path.as_path()]
        );
        let renamed = games.join("Game (2).gba");
        assert_eq!(
            unpack_into(&second, &games, &patches, &Progress::default()).expect("second"),
            [renamed.as_path()]
        );
        assert_eq!(fs::read(&path).expect("kept"), b"first");
        assert_eq!(fs::read(&renamed).expect("numbered"), b"second");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_archive_without_a_game_or_not_an_archive_is_refused() {
        let dir = temp_dir("unpack-refused");
        let (games, patches) = (dir.join("Games"), dir.join("Patches"));
        let empty = zip_file(&dir, "notes.zip", &[("notes.txt", b"notes")]);
        assert!(
            unpack_into(&empty, &games, &patches, &Progress::default())
                .unwrap_err()
                .contains("holds no")
        );
        let fake = dir.join("fake.zip");
        fs::write(&fake, b"not a zip").expect("written");
        assert!(
            unpack_into(&fake, &games, &patches, &Progress::default())
                .unwrap_err()
                .starts_with("Cannot unpack")
        );
        assert!(!games.exists());
        fs::remove_dir_all(&dir).ok();
    }
}
