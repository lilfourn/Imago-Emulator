//! Games downloaded as archives: `.zip`, `.7z` and `.rar` files. The app
//! unpacks `.zip` files itself. On macOS, its own `tar` (libarchive)
//! unpacks `.7z` and `.rar` files; elsewhere the app unpacks `.7z` files
//! itself too, and a program the system has unpacks `.rar` files (see
//! [`unpackers`]): RAR's own code is under a licence of its own, so the
//! app carries none of it. The ROMs inside are unpacked into the app's own
//! games folder, where they and their saves stay after the download is
//! deleted, written as they are read, as a 3DS game can be 4 GB. ROM-hack
//! patches inside are unpacked into a temporary folder, for
//! [`crate::romhack`] to make their games from. How far an unpacking has
//! got is told through a [`Progress`], as a big 3DS game takes most of a
//! minute. What an unpacking cut short leaves is removed (see
//! [`crate::leftovers`]).

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use emu_os::process::why_not_started;
use sevenz_rust2::{ArchiveEntry, ArchiveReader, Password};
use zip::ZipArchive;

use crate::leftovers::{self, Unfinished, WorkFolder};
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
    Some(emu_os::data_dir()?.join("ds-emulator").join("Games"))
}

/// Where patches unpacked from archives are kept until their games are
/// made.
fn patches_dir() -> PathBuf {
    std::env::temp_dir().join(leftovers::PATCHES)
}

/// How often the files an unpacking program writes are measured while it
/// unpacks.
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
    } else if has_extension(archive, &["7z"]) && !cfg!(target_os = "macos") {
        unpack_7z_or_else(archive, folder, patches, progress)
    } else {
        unpack_with_programs(archive, folder, patches, progress)
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

/// Unpacks a `.7z` file in the app, as [`unpack_7z`] does, or, if it is
/// packed in a way the app can't unpack, with a program the system has.
fn unpack_7z_or_else(
    archive: &Path,
    folder: &Path,
    patches: &Path,
    progress: &Progress,
) -> io::Result<Vec<PathBuf>> {
    match unpack_7z(archive, folder, patches, progress) {
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            if unpackers(archive).is_empty() {
                return Err(io::Error::other(format!("{error}. {}", none_installed())));
            }
            // What the program says is no better than why the app can't.
            unpack_with_programs(archive, folder, patches, progress).map_err(|_| error)
        }
        unpacked => unpacked,
    }
}

/// Unpacks a `.7z` file in the app, as [`unpack_zip`] does a `.zip` file:
/// each game is written straight into `folder` as it is read. It reads
/// LZMA and LZMA2, with 7-Zip's filters, which is what 7-Zip writes. The
/// games come in the order of their paths in the archive, as from
/// [`unpack_with`], so every system gives the same.
fn unpack_7z(
    archive: &Path,
    folder: &Path,
    patches: &Path,
    progress: &Progress,
) -> io::Result<Vec<PathBuf>> {
    let mut reader = ArchiveReader::open(archive, Password::empty()).map_err(seven_zip_error)?;
    let total = reader
        .archive()
        .files
        .iter()
        .filter(|entry| entry_path(entry).is_some())
        .map(|entry| entry.size)
        .sum();
    progress.start(total);
    let mut games = Vec::new();
    let mut done = 0;
    // A folder for a game that comes without a checksum, made if one does.
    let mut scratch = None;
    reader
        .for_each_entries(|entry, contents| {
            let Some((inner, name)) = entry_path(entry).and_then(|inner| {
                let name = PathBuf::from(inner.file_name()?);
                Some((inner, name))
            }) else {
                // Read past: the next file's bytes follow it.
                io::copy(contents, &mut io::sink())?;
                return Ok(true);
            };
            let mut counted = Counted {
                inner: contents,
                progress,
            };
            let game = if romhack::is_patch(&name) {
                keep_patch(&read_capped(&mut counted, &name)?, patches, &name)?
            } else {
                if entry.size > size_cap(&name) {
                    return Err(too_big(&name).into());
                }
                let crc32 = entry.has_crc.then_some(entry.crc as u32);
                store_entry(folder, &name, crc32, &mut counted, &mut scratch)?
            };
            // What was not read, as the same game was there already.
            io::copy(&mut counted.inner, &mut io::sink())?;
            done += entry.size;
            progress.set(done);
            games.push((inner, game));
            Ok(true)
        })
        .map_err(seven_zip_error)?;
    // Stable, so a path given twice keeps the archive's order.
    games.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(games.into_iter().map(|(_, game)| game).collect())
}

/// The path within a `.7z` file of a file in it, if it is a game or a
/// patch to unpack, whichever slash it is written with: `./a/Game.nds`, as
/// libarchive writes it, is `a/Game.nds`. Only its last part, the name, is
/// unpacked. Folders and the like have none.
fn entry_path(entry: &ArchiveEntry) -> Option<PathBuf> {
    if entry.is_directory || entry.is_anti_item {
        return None;
    }
    let inner: PathBuf = entry
        .name
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    (wanted(&inner) && inner.file_name().is_some()).then_some(inner)
}

/// Puts the game called `name`, read from `contents`, into `folder` as
/// [`store`] does, if its CRC-32 is known; or else unpacks it into a work
/// folder first, made in `scratch` if there is none yet, to learn it.
fn store_entry(
    folder: &Path,
    name: &Path,
    crc32: Option<u32>,
    contents: &mut impl Read,
    scratch: &mut Option<WorkFolder>,
) -> io::Result<PathBuf> {
    if let Some(crc32) = crc32 {
        return store(folder, name, crc32, contents);
    }
    let work = match scratch {
        Some(work) => work,
        None => scratch.insert(WorkFolder::new("unpack")?),
    };
    let unpacked = work.path().join(name);
    write_capped(&unpacked, contents, name, size_cap(name))?;
    store_unpacked(folder, name, &unpacked)
}

/// Why a `.7z` file could not be unpacked, in words for the user. One
/// packed in a way the app can't unpack is [`io::ErrorKind::Unsupported`].
fn seven_zip_error(error: sevenz_rust2::Error) -> io::Error {
    use sevenz_rust2::Error;
    let unsupported = |why: String| io::Error::new(io::ErrorKind::Unsupported, why);
    let damaged = || io::Error::new(io::ErrorKind::InvalidData, "it is damaged");
    match error {
        Error::Io(error, _) | Error::FileOpen(error, _) => {
            // A checksum that fails as a file is read comes inside the
            // read's error.
            if error.get_ref().is_some_and(|inner| inner.is::<Error>()) {
                let inner = error
                    .into_inner()
                    .and_then(|inner| inner.downcast::<Error>().ok());
                return inner.map_or_else(damaged, |inner| seven_zip_error(*inner));
            }
            match error.kind() {
                io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => damaged(),
                _ => error,
            }
        }
        Error::BadSignature(_) => {
            io::Error::new(io::ErrorKind::InvalidData, "it is not a 7-Zip file")
        }
        Error::PasswordRequired | Error::MaybeBadPassword(_) => {
            io::Error::other("it is locked with a password")
        }
        Error::UnsupportedCompressionMethod(method) if method.starts_with("AES") => {
            io::Error::other("it is locked with a password")
        }
        Error::UnsupportedCompressionMethod(method) => unsupported(format!(
            "it is packed with {method}, which the app can't unpack"
        )),
        Error::UnsupportedVersion { .. } | Error::ExternalUnsupported | Error::Unsupported(_) => {
            unsupported("it is packed in a way the app can't unpack".to_owned())
        }
        _ => damaged(),
    }
}

/// A program that unpacks the archives the app can't unpack itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unpacker {
    /// libarchive's `bsdtar`: macOS's `/usr/bin/tar`, Windows's `tar.exe`
    /// or Linux's `bsdtar`. It reads `.7z` and `.rar` files, and by default
    /// writes nothing outside the folder it is given.
    Tar(PathBuf),
    /// 7-Zip's `7z`, `7zz` or `7za`.
    SevenZip(PathBuf),
    /// RARLAB's `unrar`, which comes with WinRAR as `UnRAR.exe`.
    Unrar(PathBuf),
}

impl Unpacker {
    fn program(&self) -> &Path {
        match self {
            Self::Tar(program) | Self::SevenZip(program) | Self::Unrar(program) => program,
        }
    }

    /// The program's name, as its messages start: `tar` for `/usr/bin/tar`.
    fn name(&self) -> String {
        self.program()
            .file_stem()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }

    /// The command that unpacks `archive`, with the paths inside it, into
    /// `folder`. None asks for a password: a locked archive fails.
    fn unpack(&self, archive: &Path, folder: &Path) -> Command {
        let mut command = emu_os::process::command(self.program());
        match self {
            Self::Tar(_) => {
                command.arg("-xf").arg(archive).arg("-C").arg(folder);
            }
            Self::SevenZip(_) => {
                let mut into = std::ffi::OsString::from("-o");
                into.push(folder);
                command.args(["x", "-y", "-p-"]).arg(into).arg(archive);
            }
            Self::Unrar(_) => {
                // The folder must end with a slash, or it is a file's name.
                let mut into = folder.as_os_str().to_owned();
                into.push(std::path::MAIN_SEPARATOR_STR);
                command
                    .args(["x", "-y", "-o+", "-p-", "-idq", "--"])
                    .arg(archive)
                    .arg(into);
            }
        }
        command
    }

    /// The size of the files the archive at `archive` holds, unpacked, as
    /// the program lists them, or 0 if it cannot list them.
    fn listed_size(&self, archive: &Path) -> u64 {
        let mut command = emu_os::process::command(self.program());
        match self {
            Self::Tar(_) => command.arg("-tvf"),
            Self::SevenZip(_) => command.args(["l", "-slt", "-p-"]),
            Self::Unrar(_) => command.args(["lt", "-p-", "--"]),
        };
        let listed = command
            .arg(archive)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let Some(listed) = listed.ok().filter(|listed| listed.status.success()) else {
            return 0;
        };
        let listed = String::from_utf8_lossy(&listed.stdout);
        match self {
            Self::Tar(_) => tar_listed_size(&listed),
            // `Size = 1234` for each file, among `Packed Size = …` and the
            // archive's own `Physical Size = …`.
            Self::SevenZip(_) => listed_values(&listed, "Size = "),
            // `Size: 1234`, among `Packed size: …`.
            Self::Unrar(_) => listed_values(&listed, "Size: "),
        }
    }

    /// Why the program failed, from what it said on its errors: all of
    /// `tar`'s message, or the last line of another's, without the name it
    /// starts with (`tar: Error opening archive: …`).
    fn reason(&self, said: &str, status: ExitStatus) -> String {
        let said = said.trim();
        let said = match self {
            Self::Tar(_) => said,
            Self::SevenZip(_) | Self::Unrar(_) => {
                said.lines().map(str::trim).next_back().unwrap_or("")
            }
        };
        // `tar.exe: …` on Windows.
        let file_name = self
            .program()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let said = said
            .trim_start_matches(&format!("{file_name}: "))
            .trim_start_matches(&format!("{}: ", self.name()))
            .trim_start_matches("ERROR: ");
        if said.is_empty() {
            format!("{} stopped: {status}", self.name())
        } else {
            said.to_owned()
        }
    }
}

/// The programs on this system that can unpack `archive`, a `.7z` or
/// `.rar` file, in the order they are tried: each one after another fails.
///
/// On macOS, its own `tar`. On Windows, 7-Zip and WinRAR, which read every
/// `.rar` file, then Windows's own `tar.exe`, which reads newer ones
/// (RAR 5) only from Windows 11. On Linux, `bsdtar`, 7-Zip and `unrar`,
/// from `PATH`. `unrar` only ever reads `.rar` files.
fn unpackers(archive: &Path) -> Vec<Unpacker> {
    if cfg!(target_os = "macos") {
        return vec![Unpacker::Tar(PathBuf::from("/usr/bin/tar"))];
    }
    let rar = has_extension(archive, &["rar"]);
    let folder = |variable: &str, parts: &[&str]| {
        std::env::var_os(variable).map(|folder| {
            parts
                .iter()
                .fold(PathBuf::from(folder), |path, part| path.join(part))
        })
    };
    let candidates: Vec<Candidate> = if cfg!(windows) {
        vec![
            (on_path("7z.exe"), Unpacker::SevenZip),
            (
                folder("ProgramFiles", &["7-Zip", "7z.exe"]),
                Unpacker::SevenZip,
            ),
            (
                folder("ProgramFiles(x86)", &["7-Zip", "7z.exe"]),
                Unpacker::SevenZip,
            ),
            (
                folder("ProgramFiles", &["WinRAR", "UnRAR.exe"]),
                Unpacker::Unrar,
            ),
            (
                folder("ProgramFiles(x86)", &["WinRAR", "UnRAR.exe"]),
                Unpacker::Unrar,
            ),
            (on_path("unrar.exe"), Unpacker::Unrar),
            (
                folder("SystemRoot", &["System32", "tar.exe"]),
                Unpacker::Tar,
            ),
        ]
    } else {
        vec![
            (on_path("bsdtar"), Unpacker::Tar),
            (on_path("7zz"), Unpacker::SevenZip),
            (on_path("7z"), Unpacker::SevenZip),
            (on_path("7za"), Unpacker::SevenZip),
            (on_path("unrar"), Unpacker::Unrar),
        ]
    };
    let mut found: Vec<Unpacker> = Vec::new();
    for (path, make) in candidates {
        let Some(path) = path.filter(|path| path.is_file()) else {
            continue;
        };
        let unpacker = make(path);
        if matches!(unpacker, Unpacker::Unrar(_)) && !rar {
            continue;
        }
        // One of each kind: 7-Zip found on `PATH` and in its own folder is
        // one 7-Zip, and a second would fail as the first did.
        let kind = std::mem::discriminant(&unpacker);
        if !found
            .iter()
            .any(|known| std::mem::discriminant(known) == kind)
        {
            found.push(unpacker);
        }
    }
    found
}

/// Where an unpacking program may be, and which kind it is.
type Candidate = (Option<PathBuf>, fn(PathBuf) -> Unpacker);

/// The program called `name` in a folder on `PATH`, if there is one.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|folder| folder.join(name))
        .find(|program| program.is_file())
}

/// What to say when no program to unpack an archive is installed: which to
/// install.
fn none_installed() -> &'static str {
    if cfg!(windows) {
        "Unpacking it needs 7-Zip (7-zip.org) or WinRAR, and neither is installed"
    } else {
        "Unpacking it needs bsdtar, 7-Zip or unrar, and none is installed: bsdtar is in the libarchive-tools package on Debian and Ubuntu, and the bsdtar package on Fedora"
    }
}

/// Unpacks a `.7z` or `.rar` file with each of the [`unpackers`] in turn,
/// until one does, as [`unpack_with`] does. Says why the first failed if
/// all do.
fn unpack_with_programs(
    archive: &Path,
    folder: &Path,
    patches: &Path,
    progress: &Progress,
) -> io::Result<Vec<PathBuf>> {
    let unpackers = unpackers(archive);
    let mut first_failure = None;
    for unpacker in &unpackers {
        match unpack_with(unpacker, archive, folder, patches, progress) {
            Ok(games) => return Ok(games),
            Err(error) => {
                first_failure.get_or_insert(error);
            }
        }
    }
    let Some(error) = first_failure else {
        return Err(io::Error::other(none_installed()));
    };
    let has_7_zip = unpackers
        .iter()
        .any(|unpacker| matches!(unpacker, Unpacker::SevenZip(_)));
    if cfg!(windows) && !has_7_zip {
        // Windows 10's own `tar` reads only older `.rar` files.
        return Err(io::Error::other(format!(
            "{error}. Installing 7-Zip (7-zip.org) may let the app unpack it"
        )));
    }
    Err(error)
}

/// Unpacks a `.7z` or `.rar` file with `unpacker`, which by default writes
/// nothing outside the folder it is given: everything goes into a work
/// folder, from which the games are moved, and the patches taken, in name
/// order. How far it has got is the size of what it has written so far,
/// out of the size of what the archive lists. On macOS `unpacker` is the
/// system's own `/usr/bin/tar`, libarchive's `bsdtar`, which reads both.
fn unpack_with(
    unpacker: &Unpacker,
    archive: &Path,
    folder: &Path,
    patches: &Path,
    progress: &Progress,
) -> io::Result<Vec<PathBuf>> {
    let work = WorkFolder::new("unpack")?;
    let scratch = work.path();
    progress.start(unpacker.listed_size(archive));
    let mut command = unpacker.unpack(archive, scratch);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // Stopped if the app quits while it unpacks, or this returns early.
    let program = work
        .spawn(&mut command)
        .map_err(|error| io::Error::new(error.kind(), why_not_started(&unpacker.name(), &error)))?;
    // Read as it comes, so the program never waits on a full pipe.
    let stderr = program.take_stderr();
    let said = thread::spawn(move || {
        let mut said = String::new();
        if let Some(mut stderr) = stderr {
            stderr.read_to_string(&mut said).ok();
        }
        said
    });
    let status = loop {
        match program.try_wait()? {
            Some(status) => break status,
            None => {
                progress.set(folder_size(scratch));
                thread::sleep(MEASURE_EVERY);
            }
        }
    };
    let said = said.join().unwrap_or_default();
    if !status.success() {
        return Err(io::Error::other(unpacker.reason(&said, status)));
    }
    progress.set(folder_size(scratch));
    let mut files = Vec::new();
    find_files(scratch, scratch, &mut files)?;
    files.sort();
    let mut games = Vec::new();
    for inner in files.iter().filter(|inner| wanted(inner)) {
        let Some(name) = inner.file_name().map(PathBuf::from) else {
            continue;
        };
        let path = scratch.join(inner);
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
            store_unpacked(folder, &name, &path)?
        };
        games.push(game);
    }
    Ok(games)
}

/// The size of the files `tar -tv` lists in `listed`. Each file's line
/// reads like `ls -l`'s, its size the fifth word:
/// `-rw-r--r--  0 0      0  2147483648 Dec 25  1996 Game.3ds`.
fn tar_listed_size(listed: &str) -> u64 {
    listed
        .lines()
        .filter(|line| line.starts_with('-'))
        .filter_map(|line| line.split_whitespace().nth(4)?.parse::<u64>().ok())
        .sum()
}

/// The sum of the numbers after `label` that start lines of `listed`.
fn listed_values(listed: &str, label: &str) -> u64 {
    listed
        .lines()
        .filter_map(|line| line.trim().strip_prefix(label)?.trim().parse::<u64>().ok())
        .sum()
}

/// The size of the files under `folder`, without following links. What
/// cannot be read counts for nothing.
pub fn folder_size(folder: &Path) -> u64 {
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
/// through a `….partial` file, so `path` is only ever the whole game.
/// Stops past `cap` bytes, leaving nothing behind, as it is too big to be
/// a game.
fn write_capped(path: &Path, contents: impl Read, name: &Path, cap: u64) -> io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(leftovers::PARTIAL);
    let mut unfinished = Unfinished::create(Path::new(&partial))?;
    let written = io::copy(&mut contents.take(cap + 1), unfinished.file())?;
    if written > cap {
        return Err(too_big(name));
    }
    unfinished.file().sync_all()?;
    unfinished.finish(path)
}

/// Moves the file at `from`, the game called `name`, to `path` once it is
/// all on disk, so `path` is only ever the whole game; or copies it, if
/// `path` is on another disk.
fn move_capped(from: &Path, path: &Path, name: &Path) -> io::Result<()> {
    // Opened for writing, as Windows syncs only a file open so.
    fs::OpenOptions::new().write(true).open(from)?.sync_all()?;
    match persist::rename(from, path) {
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            write_capped(path, File::open(from)?, name, size_cap(name))
        }
        moved => moved,
    }
}

/// Writes the patch `bytes` into `patches` as `name`, replacing any patch
/// of that name, and returns its path. The name is made one the system can
/// hold ([`persist::safe_file_name`]).
fn keep_patch(bytes: &[u8], patches: &Path, name: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(patches)?;
    let path = patches.join(&*persist::safe_file_name(&name.to_string_lossy()));
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
    store_with(folder, name, crc32, |path| {
        write_capped(path, contents, name, size_cap(name))
    })
}

/// Moves the game called `name`, unpacked at `unpacked`, into `folder` as
/// [`store`] writes one: the same game there before is used, and
/// `unpacked` left where it is.
fn store_unpacked(folder: &Path, name: &Path, unpacked: &Path) -> io::Result<PathBuf> {
    store_with(folder, name, file_crc32(unpacked)?, |path| {
        move_capped(unpacked, path, name)
    })
}

/// Puts the ROM whose CRC-32 is `crc32` into `folder` as `name`, or as a
/// numbered name if a different game has that one, with `put`, and
/// returns its path; or returns that of the same game put there before.
/// The name is first made one the system can hold
/// ([`persist::safe_file_name`]).
fn store_with(
    folder: &Path,
    name: &Path,
    crc32: u32,
    put: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let safe = persist::safe_file_name(&name.to_string_lossy()).into_owned();
    let name = Path::new(&safe);
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
        put(&path)?;
        return Ok(path);
    }
    unreachable!("a free name is always found")
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    use crate::leftovers::PARTIAL;
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

    /// Tiny 7-Zip files, made with macOS's `tar` (libarchive 3.7.4) from
    /// files of a few made-up bytes, as `.7z` files can't be committed:
    ///
    /// ```sh
    /// printf platinum > a/Platinum.nds; printf '\xD6\xC3\xC4\x00' > b/Hack.xdelta
    /// printf hello > notes.txt; tar --format 7zip -cf Platinum.7z .
    /// ```
    ///
    /// `Platinum.7z` holds those three, LZMA-packed, under `./`.
    const PLATINUM_7Z: &str = "N3q8ryccAAOgmkaK3AAAAAAAAAAiAAAAAAAAAFmdL2AANBlJ7o5OWh1wayuBmEvsNxqamc+l//+aQgAAAACBMweuD8/Hr2gP6+qeAQ1iA43TTEI/DnfVZUfQlaYDL+0NSxMfiwsJmWiJJPgtvUS8JtsumXpTq5JbUS8wIYPMJ7BY4fINwusDtRCVzjTpGCT5WZxoxawNL939KndzbtdrwNgd348/ZYm/EaiMo3QRBWnhsO2n5Mdc2RENM/f3WyWCmBfFIDZQQdGjXu8p5VzF+2G5lYcl/fBrdsKYLXzAYzx+WFTe7ZN3VxkIqJX8DLemSCcrtXN8t//4X6wAFwYcAQmAwAAHCwEAASMDAQEFXQAAgAAMgWAKAT0uGvkAAA==";
    /// `Cartridge.7z` holds `Cartridge.3ds`, the four bytes `NCSD`.
    const CARTRIDGE_7Z: &str = "N3q8ryccAANKMJjKhwAAAAAAAAAhAAAAAAAAAGh6EhgAJxDGq6oLIB//+xYAAAAAgTMHrg/Ox1yBCSqu1WJk20+km3rWOLR0951/EGstPu6MmEb9ZzBIdTGC8CM6oEt4Fra0JTgNTx2A/WEYA+tVet5X6v18wvvKAwo/rGjrZUKGEkzzNN2SL6v0UkktxLTE6AefZjcpHOLcL6CZZ/nh6MRT//aTkAAXBg4BCXkABwsBAAEjAwEBBV0AAIAADICZCgFTTsPDAAA=";
    /// `Ppmd.7z` holds `Game.gb`, the bytes `ppmd`, packed with PPMd
    /// (`--options 7zip:compression=ppmd`), which the app can't unpack.
    const PPMD_7Z: &str = "N3q8ryccAAPrFrq2CQAAAAAAAABmAAAAAAAAADe//z8AcAFdU4L3ZAABBAYAAQkJAAcLAQABIwMEAQUGAAAAAQwEAAgKAXunBoQAAAUBEREARwBhAG0AZQAuAGcAYgAAABQKAQANxhZ1NG7dARIKAQANxhZ1NG7dARMKAQCoxBZ1NG7dARUGAQAggKSBAAA=";

    /// The 7-Zip file `base64` in `dir` named `name`.
    fn seven_zip_file(dir: &Path, name: &str, base64: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, BASE64.decode(base64).expect("base64")).expect("7-Zip file written");
        path
    }

    /// A program on this system that reads 7-Zip as macOS's own `tar`
    /// does: that `tar`, or another `bsdtar`.
    fn bsdtar() -> Option<Unpacker> {
        if cfg!(target_os = "macos") {
            return unpackers(Path::new("Game.7z")).into_iter().next();
        }
        on_path("bsdtar").map(Unpacker::Tar)
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
    fn unpacks_7_zip_files() {
        let _scratch = scratch_lock();
        let dir = temp_dir("unpack-7z");
        let (games, patches) = (dir.join("Games"), dir.join("Patches"));
        let archive = seven_zip_file(&dir, "Platinum.7z", PLATINUM_7Z);
        if let Some(tar) = bsdtar() {
            assert_eq!(tar.listed_size(&archive), 4 + 8 + 5, "every file is listed");
        }
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
        if let Some(tar) = bsdtar() {
            assert_eq!(tar.listed_size(&broken), 0);
        }
        let error = unpack_into(&broken, &games, &patches, &Progress::default()).unwrap_err();
        assert!(error.starts_with("Cannot unpack"), "{error}");
        if !cfg!(target_os = "macos") {
            assert!(
                error.ends_with("broken.7z: it is not a 7-Zip file"),
                "{error}"
            );
        }
        // Cut short, it is damaged, and what was unpacked of it is kept
        // whole or not at all.
        let bytes = BASE64.decode(PLATINUM_7Z).expect("base64");
        let cut = dir.join("cut.7z");
        fs::write(&cut, &bytes[..bytes.len() - 40]).expect("written");
        let error = unpack_into(&cut, &dir.join("Cut"), &patches, &Progress::default());
        let error = error.expect_err("cut short");
        // A byte changed in what is packed fails its file's checksum.
        let mut changed = bytes.clone();
        changed[40] ^= 0x55;
        let damaged = dir.join("damaged.7z");
        fs::write(&damaged, &changed).expect("written");
        let changed = unpack_into(
            &damaged,
            &dir.join("Damaged"),
            &patches,
            &Progress::default(),
        );
        let changed = changed.expect_err("damaged");
        if !cfg!(target_os = "macos") {
            assert!(error.ends_with("cut.7z: it is damaged"), "{error}");
            assert!(changed.ends_with("damaged.7z: it is damaged"), "{changed}");
            let half_written = fs::read_dir(dir.join("Damaged")).map_or(0, |games| {
                games
                    .flatten()
                    .filter(|game| game.file_name().to_string_lossy().ends_with(PARTIAL))
                    .count()
            });
            assert_eq!(half_written, 0, "no game is left half written");
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_7_zip_file_unpacks_alike_in_the_app_and_with_tar() {
        let Some(tar) = bsdtar() else {
            eprintln!("skipped: no bsdtar to compare with");
            return;
        };
        let _scratch = scratch_lock();
        let dir = temp_dir("unpack-7z-alike");
        let patches = dir.join("Patches");
        let archive = seven_zip_file(&dir, "Platinum.7z", PLATINUM_7Z);
        let (in_app, with_tar) = (dir.join("App"), dir.join("Tar"));
        let ours = unpack_7z(&archive, &in_app, &patches, &Progress::default()).expect("app");
        let theirs =
            unpack_with(&tar, &archive, &with_tar, &patches, &Progress::default()).expect("tar");
        let names = |games: &[PathBuf]| -> Vec<(PathBuf, Vec<u8>)> {
            games
                .iter()
                .map(|game| {
                    let name = game.file_name().expect("name").into();
                    (name, fs::read(game).expect("unpacked"))
                })
                .collect()
        };
        assert_eq!(names(&ours), names(&theirs), "the same games, in order");
        assert_eq!(names(&ours)[0].0, Path::new("Platinum.nds"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_7_zip_file_the_app_cannot_unpack_goes_to_a_program_that_can() {
        let _scratch = scratch_lock();
        let dir = temp_dir("unpack-7z-ppmd");
        let (games, patches) = (dir.join("Games"), dir.join("Patches"));
        let archive = seven_zip_file(&dir, "Ppmd.7z", PPMD_7Z);
        let error = unpack_7z(&archive, &games, &patches, &Progress::default()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "it is packed with PPMD, which the app can't unpack"
        );
        match unpack_into(&archive, &games, &patches, &Progress::default()) {
            Ok(unpacked) => {
                assert!(!unpackers(&archive).is_empty());
                assert_eq!(unpacked, [games.join("Game.gb")]);
                assert_eq!(fs::read(&unpacked[0]).expect("unpacked"), b"ppmd");
            }
            Err(error) => {
                assert!(unpackers(&archive).is_empty(), "{error}");
                assert!(
                    error.ends_with(&format!(
                        "Ppmd.7z: it is packed with PPMD, which the app can't unpack. {}",
                        none_installed()
                    )),
                    "{error}"
                );
            }
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_rar_file_is_unpacked_by_a_program_or_says_which_to_install() {
        let dir = temp_dir("unpack-rar");
        let (games, patches) = (dir.join("Games"), dir.join("Patches"));
        let rar = dir.join("Game.rar");
        fs::write(&rar, b"not a RAR file").expect("written");
        let error = unpack_into(&rar, &games, &patches, &Progress::default()).unwrap_err();
        assert!(error.starts_with("Cannot unpack"), "{error}");
        if unpackers(&rar).is_empty() {
            assert!(error.ends_with(none_installed()), "{error}");
        }
        assert!(!games.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_the_last_part_of_a_7_zip_file_s_path_names_its_game() {
        let named = |name: &str| {
            entry_path(&ArchiveEntry::new_file(name))
                .and_then(|inner| inner.file_name().map(PathBuf::from))
        };
        assert_eq!(named("./a/Platinum.nds"), Some("Platinum.nds".into()));
        assert_eq!(named("..\\..\\Windows\\Game.gba"), Some("Game.gba".into()));
        assert_eq!(named("/etc/../Game.gb"), Some("Game.gb".into()));
        assert_eq!(named("Hack.bps"), Some("Hack.bps".into()));
        assert_eq!(named("notes.txt"), None);
        assert_eq!(named("__MACOSX/._Game.gba"), None);
        assert_eq!(named("./__MACOSX/._Game.gba"), None);
        assert_eq!(named("Game.gba/.."), None);
        assert_eq!(entry_path(&ArchiveEntry::new_directory("Games.nds")), None);
        assert_eq!(
            entry_path(&ArchiveEntry::new_file("./a/Platinum.nds")),
            Some(Path::new("a").join("Platinum.nds"))
        );
    }

    #[test]
    fn a_game_without_a_checksum_is_unpacked_first_to_learn_it() {
        let _scratch = scratch_lock();
        let dir = temp_dir("unpack-no-crc");
        let games = dir.join("Games");
        let mut scratch = None;
        let name = Path::new("Game.gba");
        let stored =
            store_entry(&games, name, None, &mut &b"game"[..], &mut scratch).expect("stored");
        assert_eq!(stored, games.join("Game.gba"));
        assert!(scratch.is_some(), "a work folder was made");
        // The same again is used as it is; another is numbered.
        let again = store_entry(&games, name, None, &mut &b"game"[..], &mut scratch);
        assert_eq!(again.expect("again"), stored);
        let other = store_entry(&games, name, None, &mut &b"other"[..], &mut scratch);
        assert_eq!(other.expect("other"), games.join("Game (2).gba"));
        let scratch_path = scratch.as_ref().expect("work folder").path().to_owned();
        drop(scratch);
        assert!(!scratch_path.exists(), "the work folder is removed");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn programs_say_why_they_failed_and_how_big_an_archive_is() {
        let status = Command::new(std::env::current_exe().expect("test binary"))
            .arg("--no-such-option")
            .stderr(Stdio::null())
            .status()
            .expect("runs");
        let tar = Unpacker::Tar("/usr/bin/tar".into());
        assert_eq!(
            tar.reason(
                "tar: Error opening archive: Unrecognized archive format\n",
                status
            ),
            "Error opening archive: Unrecognized archive format"
        );
        let bsdtar = Unpacker::Tar("/usr/bin/bsdtar".into());
        assert_eq!(
            bsdtar.reason("bsdtar: Damaged archive", status),
            "Damaged archive"
        );
        let windows = Unpacker::Tar(Path::new("System32").join("tar.exe"));
        assert_eq!(
            windows.reason("tar.exe: Damaged archive", status),
            "Damaged archive"
        );
        let seven = Unpacker::SevenZip("/usr/bin/7zz".into());
        assert_eq!(
            seven.reason("\nERROR: Game.rar\nGame.rar\nOpen ERROR: Can not open the file as [rar] archive\n\n\nERRORS:\nIs not archive\n", status),
            "Is not archive"
        );
        let unrar = Unpacker::Unrar(Path::new("WinRAR").join("UnRAR.exe"));
        assert_eq!(unrar.reason("", status), format!("UnRAR stopped: {status}"));

        let tar_listing = "-rw-r--r--  0 0      0  2147483648 Dec 25  1996 Game.3ds\n\
            drwxr-xr-x  0 0      0           0 Dec 25  1996 Folder/\n\
            -rw-r--r--  0 0      0           5 Dec 25  1996 notes.txt\n";
        assert_eq!(tar_listed_size(tar_listing), 2_147_483_653);
        let seven_listing = "Path = Game.7z\nType = 7z\nPhysical Size = 300\n\n----------\n\
            Path = Game.nds\nSize = 1024\nPacked Size = 200\n\nPath = notes.txt\nSize = 5\nPacked Size = \n";
        assert_eq!(listed_values(seven_listing, "Size = "), 1029);
        let unrar_listing = "Archive: Game.rar\nDetails: RAR 5\n\n        Name: Game.gba\n        Type: File\n\
            Size: 4096\n Packed size: 100\n        Name: notes.txt\n        Size: 5\n";
        assert_eq!(listed_values(unrar_listing, "Size: "), 4101);
    }

    #[test]
    fn each_program_is_told_to_unpack_into_its_folder_without_asking_anything() {
        let args = |unpacker: Unpacker| -> Vec<String> {
            unpacker
                .unpack(Path::new("Game.rar"), Path::new("work"))
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(Unpacker::Tar("/usr/bin/tar".into())),
            ["-xf", "Game.rar", "-C", "work"]
        );
        assert_eq!(
            args(Unpacker::SevenZip("7z".into())),
            ["x", "-y", "-p-", "-owork", "Game.rar"]
        );
        let into = format!("work{}", std::path::MAIN_SEPARATOR);
        assert_eq!(
            args(Unpacker::Unrar("unrar".into())),
            [
                "x",
                "-y",
                "-o+",
                "-p-",
                "-idq",
                "--",
                "Game.rar",
                into.as_str()
            ]
        );
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
        let seven = seven_zip_file(&dir, "Cartridge.7z", CARTRIDGE_7Z);
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

    /// Unpacks each archive named in `DS_EMULATOR_ARCHIVES` (a list, as
    /// `PATH` is written) with every program here that unpacks archives,
    /// and a `.7z` one in the app too, into temporary folders. The app must
    /// unpack it, or else one program, and all that do must give the same
    /// games. Prints what each gave.
    #[test]
    #[ignore = "needs archives, and programs that unpack them"]
    fn unpacks_archives_for_real() {
        let archives = std::env::var_os("DS_EMULATOR_ARCHIVES").expect("DS_EMULATOR_ARCHIVES");
        let dir = temp_dir("unpack-real");
        let patches = dir.join("Patches");
        let games = |games: io::Result<Vec<PathBuf>>| -> Result<Vec<(PathBuf, u32)>, String> {
            let games = games.map_err(|error| error.to_string())?;
            Ok(games
                .iter()
                .map(|game| {
                    let name = game.file_name().expect("name").into();
                    (name, file_crc32(game).expect("read"))
                })
                .collect())
        };
        for (number, archive) in std::env::split_paths(&archives).enumerate() {
            let mut results = Vec::new();
            if has_extension(&archive, &["7z"]) {
                let folder = dir.join(format!("{number}-app"));
                let unpacked = unpack_7z(&archive, &folder, &patches, &Progress::default());
                let unpacked = games(unpacked);
                assert!(unpacked.is_ok(), "the app: {unpacked:?}");
                results.push(("the app".to_owned(), unpacked));
            }
            for unpacker in unpackers(&archive) {
                let folder = dir.join(format!("{number}-{}", unpacker.name()));
                let progress = Progress::default();
                let unpacked = unpack_with(&unpacker, &archive, &folder, &patches, &progress);
                let listed = progress.total.load(Ordering::Relaxed);
                let name = format!("{} (lists {listed} bytes)", unpacker.program().display());
                results.push((name, games(unpacked)));
            }
            for (by, unpacked) in &results {
                eprintln!("{}: {by}: {unpacked:?}", archive.display());
            }
            let done: Vec<_> = results
                .iter()
                .filter_map(|(_, games)| games.as_ref().ok())
                .collect();
            assert!(
                !done.is_empty(),
                "nothing here unpacks {}",
                archive.display()
            );
            assert!(
                done.windows(2).all(|pair| pair[0] == pair[1]),
                "all that unpack it give the same games"
            );
        }
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
