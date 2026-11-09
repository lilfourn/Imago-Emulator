//! Randomized Pokémon games, made by the Universal Pokémon Randomizer FVX
//! from the player's own copy of a game: wild Pokémon, trainers, starters
//! and more shuffled, as a new game in the hacks folder (see `romhack`),
//! with its spoiler log beside it.
//!
//! FVX is GPL-3.0 Java, so it is not built into the app. A pinned release
//! for this system (macOS, Linux or Windows), which carries its own Java,
//! is downloaded at first use, checked against its pinned SHA-256 and
//! unpacked into the data folder, as the 3DS core is (`emu_3ds::install`).
//! Then each game is made by running it as a separate program, on its
//! command line.
//!
//! It randomizes generations 1 to 5: Red, Blue and Yellow; Gold, Silver and
//! Crystal; Ruby, Sapphire, Emerald, FireRed and LeafGreen; Diamond, Pearl,
//! Platinum, HeartGold and SoulSilver; Black, White, Black 2 and White 2.
//! FVX also randomizes the 3DS games, but those are not offered: every 3DS
//! game keeps its save by its title ID, which a randomized copy shares
//! with the game it was made from.
//!
//! Everything here blocks; run it off the UI thread.

use std::fs::{self, File};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::archive;
use crate::romhack;

/// The FVX release the app runs.
pub const VERSION: &str = "1.6.2";
/// The id FVX 1.6.2 gives its settings, which begin its settings strings.
/// FVX reads older ones, but not newer.
const SETTINGS_VERSION: u32 = 428;

/// A pinned FVX download.
#[derive(Debug, Clone, Copy)]
struct Download<'a> {
    url: &'a str,
    /// Its SHA-256, in lowercase hex.
    sha256: &'a str,
    /// About how many megabytes it is.
    megabytes: u32,
}

/// FVX 1.6.2 for Apple silicon, with Java 25 (41.4 MB).
const MAC_ARM64: Download<'static> = Download {
    url: "https://github.com/upr-fvx/universal-pokemon-randomizer-fvx/releases/download/vFVX1.6.2/UPR_FVX-v1_6_2-Mac_ARM.zip",
    sha256: "006bd1280ea1b95cfa0c85d909f993bb3a895b99776e03843a9ee297775422ec",
    megabytes: 41,
};

/// The same for Intel Macs (43.3 MB).
const MAC_X86_64: Download<'static> = Download {
    url: "https://github.com/upr-fvx/universal-pokemon-randomizer-fvx/releases/download/vFVX1.6.2/UPR_FVX-v1_6_2-Mac_x86.zip",
    sha256: "66b229d0deddd210916b5776f20f3b293328393c5020c3a60824ac90bfb5e057",
    megabytes: 43,
};

/// The same for Linux on x86-64 (51.1 MB), laid out as the Mac's.
const LINUX_X86_64: Download<'static> = Download {
    url: "https://github.com/upr-fvx/universal-pokemon-randomizer-fvx/releases/download/vFVX1.6.2/UPR_FVX-v1_6_2-Linux_x86.zip",
    sha256: "a4a8a87b0fea78ff44ea8b3a1406fe8e503ceab291c482d78001122c5ec5783b",
    megabytes: 51,
};

/// The same for Linux on arm64 (49.6 MB).
const LINUX_ARM64: Download<'static> = Download {
    url: "https://github.com/upr-fvx/universal-pokemon-randomizer-fvx/releases/download/vFVX1.6.2/UPR_FVX-v1_6_2-Linux_ARM.zip",
    sha256: "cb91e8dd3cd066bf239b485a6301ec420ae88640a658b1a4c37779b18d94ad66",
    megabytes: 50,
};

/// The same for Windows on x86-64 (41.5 MB), its Java's programs `.exe`s.
const WINDOWS_X86_64: Download<'static> = Download {
    url: "https://github.com/upr-fvx/universal-pokemon-randomizer-fvx/releases/download/vFVX1.6.2/UPR_FVX-v1_6_2-Windows.zip",
    sha256: "1cee4080967075e5babcc06e93d7c382ec2f00f05d99e17ce31fc0a99d581acb",
    megabytes: 41,
};

/// The download for this system, if FVX has one.
const PIN: Option<Download<'static>> = if cfg!(target_os = "macos") {
    if cfg!(target_arch = "x86_64") {
        Some(MAC_X86_64)
    } else {
        Some(MAC_ARM64)
    }
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some(LINUX_X86_64)
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    Some(LINUX_ARM64)
} else if cfg!(all(windows, target_arch = "x86_64")) {
    Some(WINDOWS_X86_64)
} else {
    None
};

/// Why the randomizer can't run on this system, or `None` where it can:
/// FVX is pinned for macOS, for Linux on x86-64 and arm64, and for Windows
/// on x86-64.
pub const UNSUPPORTED: Option<&str> = match PIN {
    Some(_) => None,
    None => Some("The randomizer isn't supported on this system"),
};

/// About how many megabytes the download is, for the app to say so first.
pub const DOWNLOAD_MB: u32 = match PIN {
    Some(pin) => pin.megabytes,
    None => 0,
};

/// The folder in the data folder FVX is kept in, named for its version.
const FOLDER: &str = "upr-fvx-1.6.2";
/// The randomizer, and its Java, in that folder. On Windows that is
/// `javaw.exe`, the Java that opens no console window.
const JAR: &str = "UPR-FVX.jar";
const JAVA: &str = if cfg!(windows) {
    "java/bin/javaw.exe"
} else {
    "java/bin/java"
};
/// The other programs in its Java that must be executable on macOS and
/// Linux: the zips keep no permissions. Windows runs programs by their
/// name.
const EXECUTABLES: [&str; 3] = ["java/bin/java", "java/bin/keytool", "java/lib/jspawnhelper"];
/// The file in the data folder an install holds locked.
const LOCK_FILE: &str = "install.lock";
/// How the temporary folders installs work in start.
const TEMP_PREFIX: &str = ".install-";
/// How long the download may take in all, in seconds.
const DOWNLOAD_SECONDS: u32 = 1200;
/// The most the download may be: over twice the biggest, Linux's 51 MB.
const MAX_DOWNLOAD: u64 = 128 << 20;

/// Where the randomizer is kept, e.g.
/// `~/Library/Application Support/ds-emulator/Randomizer` on macOS.
pub fn data_dir() -> Option<PathBuf> {
    Some(emu_os::data_dir()?.join("ds-emulator").join("Randomizer"))
}

/// An installed FVX.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Randomizer {
    /// Its folder, holding the jar and its Java.
    folder: PathBuf,
}

impl Randomizer {
    fn java(&self) -> PathBuf {
        self.folder.join(JAVA)
    }

    fn jar(&self) -> PathBuf {
        self.folder.join(JAR)
    }
}

/// The randomizer installed in `data`, if it is.
pub fn installed(data: &Path) -> Option<Randomizer> {
    let randomizer = Randomizer {
        folder: data.join(FOLDER),
    };
    (randomizer.jar().is_file() && randomizer.java().is_file()).then_some(randomizer)
}

/// The randomizer in `data`, downloading and installing it first if it is
/// not there.
///
/// # Errors
///
/// Says why, if the randomizer can't run on this system, the download
/// fails or is not the file pinned here, or a file cannot be written.
pub fn ensure(data: &Path) -> Result<Randomizer, String> {
    let Some(pin) = PIN else {
        return Err(UNSUPPORTED.unwrap_or_default().to_owned());
    };
    match installed(data) {
        Some(randomizer) => Ok(randomizer),
        None => install(data, pin, &curl),
    }
}

/// Fetches the file at a URL into a path, or says why it could not.
type Fetch<'f> = &'f dyn Fn(&str, &Path) -> Result<(), String>;

/// Installs `pin` into `data`, getting it with `fetch`, once no other
/// install is under way. The download is checked, unpacked into a
/// temporary folder and renamed into place, so the randomizer's folder is
/// always whole.
fn install(data: &Path, pin: Download<'_>, fetch: Fetch<'_>) -> Result<Randomizer, String> {
    let failed = |error: io::Error| format!("Cannot install the randomizer: {error}");
    fs::create_dir_all(data).map_err(failed)?;
    let _lock = lock(data).map_err(failed)?;
    // Another install may have finished while this one waited.
    if let Some(randomizer) = installed(data) {
        return Ok(randomizer);
    }
    clear_unfinished(data);
    let temp = data.join(format!("{TEMP_PREFIX}{}", std::process::id()));
    fs::create_dir(&temp).map_err(failed)?;
    let result = unpack_download(&temp, pin, fetch).and_then(|unpacked| {
        place(&unpacked, &data.join(FOLDER)).map_err(failed)?;
        installed(data).ok_or_else(|| "The randomizer did not install whole".to_owned())
    });
    // What is left behind is cleared by the next install.
    fs::remove_dir_all(&temp).ok();
    result
}

/// Downloads `pin` into `temp`, checks it and unpacks it there, with its
/// programs executable. Returns the unpacked folder.
fn unpack_download(temp: &Path, pin: Download<'_>, fetch: Fetch<'_>) -> Result<PathBuf, String> {
    let zip = temp.join("randomizer.zip");
    fetch(pin.url, &zip).map_err(|why| format!("Cannot download the randomizer: {why}"))?;
    let found = sha256(&zip).map_err(|error| format!("Cannot read the download: {error}"))?;
    if found != pin.sha256 {
        return Err(format!(
            "The randomizer's download is not the file expected (its SHA-256 is {found})"
        ));
    }
    let unpacked = temp.join(FOLDER);
    unzip(&zip, &unpacked).map_err(|why| format!("Cannot unpack the randomizer: {why}"))?;
    for needed in [JAR, JAVA] {
        if !unpacked.join(needed).is_file() {
            return Err(format!("The randomizer's download has no {needed} in it"));
        }
    }
    for program in EXECUTABLES {
        let path = unpacked.join(program);
        if path.is_file() {
            make_executable(&path)
                .map_err(|error| format!("Cannot install the randomizer: {error}"))?;
        }
    }
    Ok(unpacked)
}

/// Unpacks the zip at `zip` into the new folder `to`: on macOS with its own
/// `ditto`, as the app always has, and elsewhere in the app.
fn unzip(zip: &Path, to: &Path) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        let output = Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(zip)
            .arg(to)
            .output();
        return match output {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(reason(&output)),
            Err(error) => Err(error.to_string()),
        };
    }
    unzip_in_app(zip, to).map_err(|error| error.to_string())
}

/// Unpacks the zip at `zip` into `to` in the app. Each path is kept inside
/// `to`, and so is any link.
fn unzip_in_app(zip: &Path, to: &Path) -> io::Result<()> {
    let mut archive = ZipArchive::new(File::open(zip)?).map_err(io::Error::other)?;
    archive.extract(to).map_err(io::Error::other)
}

/// Lets the file at `path` run. Windows runs programs by their name, and
/// has no such bit.
fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Takes the lock on installing into `data`, waiting for any other
/// install to finish first. It is held until the file is dropped.
fn lock(data: &Path) -> io::Result<File> {
    let file = File::create(data.join(LOCK_FILE))?;
    file.lock()?;
    Ok(file)
}

/// Removes the temporary folders of installs that never finished. Only
/// called holding the lock, so none of them is in use.
fn clear_unfinished(data: &Path) {
    let Ok(entries) = fs::read_dir(data) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            fs::remove_dir_all(entry.path()).ok();
        }
    }
}

/// Moves the folder `prepared` to `target`, replacing whatever an earlier,
/// broken install left there.
fn place(prepared: &Path, target: &Path) -> io::Result<()> {
    match fs::remove_dir_all(target) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    // A virus scanner may still have the programs just unpacked open.
    crate::persist::rename(prepared, target)
}

/// Downloads `url` into the file `to` with curl, as [`emu_os::fetch`] does
/// a big file: from an `https://` address only, giving up on one stalled
/// for a minute and trying again twice after a passing failure.
fn curl(url: &str, to: &Path) -> Result<(), String> {
    let download = emu_os::fetch::Download {
        url,
        allow: emu_os::fetch::Allow::Https,
        seconds: DOWNLOAD_SECONDS,
        max_bytes: MAX_DOWNLOAD,
        patient: true,
    };
    download.to_file(to).map_err(|error| error.to_string())
}

/// The SHA-256 of the file at `path`, in lowercase hex.
fn sha256(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Why the program that gave `output` failed: the last line it printed as
/// an error, or else its exit status.
fn reason(output: &Output) -> String {
    let errors = String::from_utf8_lossy(&output.stderr);
    errors
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map_or_else(|| output.status.to_string(), str::to_owned)
}

/// The first three letters of the game codes FVX knows, for each
/// generation: every region of each game. FVX itself refuses a release it
/// does not know, and says so.
const GEN_3: [&[u8]; 5] = [b"AXV", b"AXP", b"BPE", b"BPR", b"BPG"];
const GEN_4: [&[u8]; 5] = [b"ADA", b"APA", b"CPU", b"IPK", b"IPG"];
const GEN_5: [&[u8]; 4] = [b"IRB", b"IRA", b"IRE", b"IRD"];
/// The titles in the Game Boy games' headers. Gold and Silver have their
/// game code after the title, and the European Yellows a shorter title.
const GEN_1: [&[u8]; 5] = [
    b"POKEMON RED",
    b"POKEMON BLUE",
    b"POKEMON GREEN",
    b"POKEMON YELLOW",
    b"POKEMON YELAPS",
];
const GEN_2: [&[u8]; 3] = [b"POKEMON_GLD", b"POKEMON_SLV", b"PM_CRYSTAL"];

/// The generation of the Pokémon game in the ROM at `path`, if the
/// randomizer can randomize it, judged by its header. Games in the hacks
/// folder are not offered: the randomizer is made for the games as sold,
/// and a hack may break when randomized.
pub fn generation(path: &Path) -> Option<u8> {
    if romhack::is_hack(path) {
        return None;
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let mut header = Vec::with_capacity(0x150);
    File::open(path)
        .ok()?
        .take(0x150)
        .read_to_end(&mut header)
        .ok()?;
    generation_of(&extension, &header)
}

/// The generation of the Pokémon game whose file has `extension` and
/// begins with `header`.
fn generation_of(extension: &str, header: &[u8]) -> Option<u8> {
    let starts = |at: usize, names: &[&[u8]]| {
        header
            .get(at..)
            .is_some_and(|rest| names.iter().any(|name| rest.starts_with(name)))
    };
    match extension {
        "gb" | "gbc" if starts(0x134, &GEN_1) => Some(1),
        "gb" | "gbc" if starts(0x134, &GEN_2) => Some(2),
        "gba" if starts(0xAC, &GEN_3) => Some(3),
        "nds" if starts(0x0C, &GEN_4) => Some(4),
        "nds" if starts(0x0C, &GEN_5) => Some(5),
        _ => None,
    }
}

/// What to randomize, from the presets the app offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Wild Pokémon only.
    Wild,
    /// Starters, wild Pokémon, trainers, gifts and trades.
    Classic,
    /// Classic, and every Pokémon's types, abilities and moves, and items.
    Everything,
}

impl Preset {
    /// The presets, as the window lists them.
    pub const ALL: [Self; 3] = [Self::Wild, Self::Classic, Self::Everything];

    pub fn label(self) -> &'static str {
        match self {
            Self::Wild => "Wild Pokémon",
            Self::Classic => "Classic",
            Self::Everything => "Everything",
        }
    }

    /// What the preset changes, in a sentence.
    pub fn description(self) -> &'static str {
        match self {
            Self::Wild => {
                "New wild Pokémon, the same ones in each place every visit. Trainers, starters and items stay as they were."
            }
            Self::Classic => {
                "A new starter, wild Pokémon, trainers' teams, gifts and trades, each about as strong as what it replaces."
            }
            Self::Everything => {
                "Classic, and every Pokémon's types, abilities and moves, the TMs, the tutors and the items lying around."
            }
        }
    }

    /// Its FVX settings string. All three also turn evolutions by trade
    /// into evolutions by level, as there is no one to trade with. Made by
    /// `scripts/randomizer/MakePresets.java`.
    pub fn settings(self) -> &'static str {
        match self {
            Self::Wild => {
                "428AQgEBQQAAAAAAAAEAAHkIAIRBAEUAAAUAEAEAAEY/wAAAAAAAADkBOQBAAgJ5AAAAOQAAgABAAEBAAAAAAAJAAAAKAEAEmRzLWVtdWxhdG9yIHByZXNldLar+TMAAAAA"
            }
            Self::Classic => {
                "428AQgEBQgAAAAAAAAEAALkIAIRBAIUAAAUAAIEAAEZ/wAAAAAAAADkBOQBAAgJ5AAAAOQAAgABAAEBAAAAAAAJAAAAKAEAEmRzLWVtdWxhdG9yIHByZXNldCYalIMAAAAA"
            }
            Self::Everything => {
                "428AQgBBggAAAAAAAACAALkIAIRBAIKAAAKAAICAAEZ/wAAAAAAAADkBOQBAAgJ5AAAAOQAAgABAAEBAAAAAAAJAAAAKAEAEmRzLWVtdWxhdG9yIHByZXNldLjI4AsAAAAA"
            }
        }
    }
}

/// The settings string the player pasted, made ready for FVX: its
/// whitespace taken out, and a Universal Pokémon Randomizer ZX 4.6.1
/// string relabelled as 4.6.0's. The two are written alike, but FVX takes
/// 4.6.1's version id for another fork's, whose settings are laid out
/// differently, and fails reading them.
///
/// # Errors
///
/// Says why if `text` is not a settings string, is cut short or
/// mistyped, or comes from a newer randomizer than the one the app runs.
pub fn pasted_settings(text: &str) -> Result<String, String> {
    let text: String = text.split_whitespace().collect();
    let not_settings = || "That is not a randomizer settings string".to_owned();
    let (version, body) = text
        .split_at_checked(3)
        .filter(|(version, _)| version.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(not_settings)?;
    let version: u32 = version.parse().map_err(|_| not_settings())?;
    let data = BASE64.decode(body).map_err(|_| not_settings())?;
    if version > SETTINGS_VERSION {
        return Err(format!(
            "Those settings are from a newer randomizer than the app's, FVX {VERSION}"
        ));
    }
    // Since ZX 3.0, the data ends with a CRC-32 of what comes before it,
    // then four more bytes: ZX's checksum of its name lists, which FVX
    // keeps as padding.
    let checked = data.len().checked_sub(8).is_some_and(|end| {
        let stored = u32::from_be_bytes([data[end], data[end + 1], data[end + 2], data[end + 3]]);
        crc32fast::hash(&data[..end]) == stored
    });
    if version >= 300 && !checked {
        return Err("That settings string is cut short or mistyped".to_owned());
    }
    if version == ZX_4_6_1 && is_zx(&data) {
        return Ok(format!("{ZX_4_6_0}{body}"));
    }
    Ok(text)
}

/// The version ids of ZX 4.6.0 and 4.6.1.
const ZX_4_6_0: u32 = 321;
const ZX_4_6_1: u32 = 322;
/// How many bytes of settings ZX 4.x writes before the ROM's name.
const ZX_SETTINGS: usize = 51;

/// Whether `data` is laid out as ZX 4.x writes settings: 51 bytes, the
/// ROM's name after its length, then the checksum and padding.
fn is_zx(data: &[u8]) -> bool {
    data.get(ZX_SETTINGS)
        .is_some_and(|&name| data.len() == ZX_SETTINGS + 1 + usize::from(name) + 8)
}

/// A new seed: six digits, short enough to read out, and to tell games
/// made from the same settings apart by.
pub fn new_seed() -> u32 {
    let random = RandomState::new().hash_one(SystemTime::now());
    100_000 + u32::try_from(random % 900_000).unwrap_or(0)
}

/// The extensions FVX gives the games it writes, by generation: `gbc`
/// for both Game Boy generations, `gba` and `nds`.
const WRITTEN: [&str; 3] = ["gbc", "gba", "nds"];

/// Runs so far, which tells their work folders apart.
static RUNS: AtomicU32 = AtomicU32::new(0);

/// A game the randomizer made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Made {
    /// The game, in the hacks folder.
    pub rom: PathBuf,
    /// Its spoiler log, beside it: what each thing became. None if it
    /// could not be kept, which doesn't lose the game.
    pub log: Option<PathBuf>,
}

/// Randomizes the game at `base`, which the library calls `title`, with
/// the FVX `settings` string and `seed`, into the hacks folder `hacks`:
/// `Pokemon - Platinum Version (Random 123456).nds`, with its spoiler log
/// beside it as `….nds.log`. The same game made before is used as it is.
///
/// # Errors
///
/// Says why if the randomizer cannot run or refuses the game, or a file
/// cannot be read or written.
pub fn randomize(
    randomizer: &Randomizer,
    base: &Path,
    title: &str,
    settings: &str,
    seed: u32,
    hacks: &Path,
) -> Result<Made, String> {
    let temp = std::env::temp_dir();
    clear_stale_work(&temp);
    let work = temp.join(format!(
        "{WORK_PREFIX}{}-{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    fs::remove_dir_all(&work).ok();
    fs::create_dir_all(&work).map_err(|error| format!("Cannot randomize: {error}"))?;
    let made = randomize_in(randomizer, base, title, settings, seed, hacks, &work);
    fs::remove_dir_all(&work).ok();
    made
}

/// How the names of the runs' work folders start, in the temporary folder.
const WORK_PREFIX: &str = "ds-emulator-randomize-";
/// How old a work folder must be to be one a run left behind, as when the
/// app quit during it: runs take seconds.
const STALE_WORK: Duration = Duration::from_secs(60 * 60);

/// Removes the work folders, in `temp`, of runs that never finished.
fn clear_stale_work(temp: &Path) {
    let Ok(entries) = fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_WORK));
        if stale && entry.file_name().to_string_lossy().starts_with(WORK_PREFIX) {
            fs::remove_dir_all(entry.path()).ok();
        }
    }
}

fn randomize_in(
    randomizer: &Randomizer,
    base: &Path,
    title: &str,
    settings: &str,
    seed: u32,
    hacks: &Path,
    work: &Path,
) -> Result<Made, String> {
    // On Windows it opens no console window.
    let output = emu_os::process::command(randomizer.java())
        // What FVX's own launcher gives it, which the 3DS games need.
        .arg("-Xmx4608M")
        // No Dock icon, nor any window.
        .arg("-Djava.awt.headless=true")
        .arg("-jar")
        .arg(randomizer.jar())
        .arg("cli")
        .arg("-i")
        .arg(base)
        // FVX adds the extension it gives the game's generation.
        .arg("-o")
        .arg(work.join("randomized"))
        .args(["-S", settings, "-z", &seed.to_string(), "-l"])
        // It reads its name lists from its own `data` folder.
        .current_dir(&randomizer.folder)
        // Java reads options from these, which could change how it runs.
        .env_remove("JAVA_TOOL_OPTIONS")
        .env_remove("JDK_JAVA_OPTIONS")
        .env_remove("_JAVA_OPTIONS")
        .output()
        .map_err(|error| format!("Cannot run the randomizer: {error}"))?;
    let rom = fs::read_dir(work)
        .map_err(|error| format!("Cannot randomize: {error}"))?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| WRITTEN.iter().any(|written| extension == *written))
        });
    let rom = match (output.status.success(), rom) {
        (true, Some(rom)) => rom,
        (true, None) => return Err("The randomizer wrote no game".to_owned()),
        (false, _) => return Err(failure(&output)),
    };
    // FVX names Red and Blue `.gbc`, which the Game Boy Color would play;
    // the game keeps its own extension.
    let extension = base
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let name = format!("{} (Random {seed}).{extension}", file_title(title));
    let failed = |error: io::Error| format!("Cannot keep the randomized game: {error}");
    let crc32 = archive::file_crc32(&rom).map_err(failed)?;
    let stored = archive::store(
        hacks,
        Path::new(&name),
        crc32,
        File::open(&rom).map_err(failed)?,
    )
    .map_err(failed)?;
    let mut log = stored.clone().into_os_string();
    log.push(".log");
    let log = PathBuf::from(log);
    let mut written = rom.into_os_string();
    written.push(".log");
    let log = fs::copy(written, &log).is_ok().then_some(log);
    Ok(Made { rom: stored, log })
}

/// Why FVX failed, in a sentence, from what it printed. It prints its
/// `ERROR:` lines and how to use it on its output, and an exception it
/// caught there too, but one it didn't on its errors, so both are read.
fn failure(output: &Output) -> String {
    let printed = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // FVX ends every failure with "Randomization failed", then how to use
    // it; the line before says why.
    let said = printed.lines().rev().find_map(|line| {
        let said = line.trim().strip_prefix("ERROR:")?.trim();
        (!said.is_empty() && said != "Randomization failed").then_some(said)
    });
    let refused = |why: &str| format!("The randomizer refused the game: {why}");
    let Some(said) = said else {
        let why = exception(&printed).unwrap_or_else(|| output.status.to_string());
        return format!("The randomizer stopped: {why}");
    };
    // "Could not load <path>; <why>", why being a RomOpener.FailType.
    match said.rsplit_once("; ").map(|(_, why)| why) {
        Some("UNSUPPORTED_ROM") => refused("it does not know this release of the game"),
        Some("ENCRYPTED_ROM") => refused("the game is encrypted"),
        Some("EXTRA_MEMORY_NOT_AVAILABLE") => refused("Java ran out of memory"),
        Some("UNREADABLE" | "INVALID_TOO_SHORT") => refused("it cannot read the game"),
        _ => format!("The randomizer stopped: {said}"),
    }
}

/// What the last exception Java printed in `printed` says: the root cause
/// last, as `Caused by: java.io.IOException: why`, or the exception
/// itself, as `Exception in thread "main" java.lang.Error: why` or
/// `java.lang.Error: why`. Its class names it if it says nothing.
fn exception(printed: &str) -> Option<String> {
    printed.lines().rev().find_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix("Caused by:").unwrap_or(line).trim();
        let line = line
            .strip_prefix("Exception in thread \"main\"")
            .unwrap_or(line)
            .trim();
        let (class, why) = line.split_once(": ").unwrap_or((line, ""));
        let named = !class.contains(char::is_whitespace)
            && class.contains('.')
            && (class.ends_with("Exception") || class.ends_with("Error"));
        let why = why.trim();
        named.then(|| {
            if why.is_empty() {
                class.rsplit('.').next().unwrap_or(class).to_owned()
            } else {
                why.to_owned()
            }
        })
    })
}

/// `title` as a file name: without the colons and slashes macOS's Finder
/// and its file system disagree on. `Pokemon: Platinum Version` becomes
/// `Pokemon - Platinum Version`.
fn file_title(title: &str) -> String {
    let title = title.replace(": ", " - ").replace([':', '/'], "-");
    let title = title.trim();
    if title.is_empty() {
        "Pokémon".to_owned()
    } else {
        title.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("emu-app-randomizer-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        fs::write(path, bytes).expect("write");
    }

    /// Checks the Unix permissions of the file at `path`. Windows files have
    /// none to check.
    fn assert_mode(path: &Path, mode: u32) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let found = fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(found, mode, "{path:?}");
        }
        #[cfg(not(unix))]
        let _ = (path, mode);
    }

    /// The first `0x150` bytes of a ROM with `bytes` at `at`.
    fn header(at: usize, bytes: &[u8]) -> Vec<u8> {
        let mut header = vec![0; 0x150];
        header[at..at + bytes.len()].copy_from_slice(bytes);
        header
    }

    #[test]
    fn the_games_of_generations_1_to_5_are_known_by_their_headers() {
        let cases: [(&str, usize, &[u8], Option<u8>); 12] = [
            ("gb", 0x134, b"POKEMON RED\0\0\0\0\0", Some(1)),
            ("gbc", 0x134, b"POKEMON YELLOW\0\0", Some(1)),
            ("gb", 0x134, b"POKEMON YELAPSD", Some(1)),
            ("gbc", 0x134, b"POKEMON_GLDAAUE", Some(2)),
            ("gbc", 0x134, b"PM_CRYSTAL\0BYTE", Some(2)),
            ("gb", 0x134, b"TETRIS\0", None),
            ("gba", 0xAC, b"BPRE", Some(3)),
            ("gba", 0xAC, b"BPEE", Some(3)),
            ("gba", 0xAC, b"BZME", None),
            ("nds", 0x0C, b"CPUE", Some(4)),
            ("nds", 0x0C, b"IRDO", Some(5)),
            ("nds", 0x0C, b"AMCE", None),
        ];
        for (extension, at, bytes, generation) in cases {
            assert_eq!(
                generation_of(extension, &header(at, bytes)),
                generation,
                "{extension} {bytes:?}"
            );
        }
        // The code at the wrong place for the system, or a file cut short.
        assert_eq!(generation_of("gba", &header(0x0C, b"CPUE")), None);
        assert_eq!(generation_of("nds", b"short"), None);
        assert_eq!(generation_of("3ds", &header(0x0C, b"CPUE")), None);
    }

    #[test]
    fn a_rom_on_disk_is_read_for_its_generation() {
        let dir = temp_dir("generation");
        let rom = dir.join("Platinum.nds");
        let mut bytes = header(0x0C, b"CPUE");
        bytes.extend(vec![0; 4096]);
        write(&rom, &bytes);
        assert_eq!(generation(&rom), Some(4));
        assert_eq!(generation(&dir.join("Missing.nds")), None, "no such file");
        let upper = dir.join("Emerald.GBA");
        write(&upper, &header(0xAC, b"BPEE"));
        assert_eq!(generation(&upper), Some(3), "any case of extension");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_presets_are_fvx_settings_of_its_own_version() {
        for preset in Preset::ALL {
            let settings = preset.settings();
            assert!(settings.starts_with(&SETTINGS_VERSION.to_string()));
            assert_eq!(pasted_settings(settings).as_deref(), Ok(settings));
            assert!(!preset.label().is_empty() && preset.description().ends_with('.'));
        }
    }

    /// A settings string from ZX 4.6.1's own window, for Platinum.
    const ZX_4_6_1_SETTINGS: &str = "322AAgEBQL///////8EAAIeJAIBFAAAFABABAABGAAAAAAAAAAAMgQyAQAICTIAAAAyAAIAFFBva2Vtb24gUGxhdGludW0gKFUpgy/bsOPDOIo=";

    #[test]
    fn a_zx_4_6_1_string_is_relabelled_for_fvx() {
        let fixed = pasted_settings(ZX_4_6_1_SETTINGS).expect("ZX settings");
        assert_eq!(fixed, format!("321{}", &ZX_4_6_1_SETTINGS[3..]));
        // Pasted across lines and with spaces around it, as from a forum.
        let (head, tail) = ZX_4_6_1_SETTINGS.split_at(40);
        assert_eq!(pasted_settings(&format!("  {head}\n{tail} \n")), Ok(fixed));
        // A string of the same id laid out otherwise is another fork's,
        // and left for FVX to read as such.
        let mut data = BASE64.decode(&ZX_4_6_1_SETTINGS[3..]).expect("base64");
        data.splice(10..10, [0, 0]);
        let end = data.len() - 8;
        let crc = crc32fast::hash(&data[..end]).to_be_bytes();
        data[end..end + 4].copy_from_slice(&crc);
        let other = format!("322{}", BASE64.encode(&data));
        assert_eq!(pasted_settings(&other), Ok(other.clone()));
    }

    #[test]
    fn what_is_not_settings_is_refused_saying_why() {
        let refused = |text: &str| pasted_settings(text).expect_err(text);
        assert_eq!(refused(""), "That is not a randomizer settings string");
        assert_eq!(refused("hello"), "That is not a randomizer settings string");
        assert_eq!(refused("32"), "That is not a randomizer settings string");
        assert_eq!(
            refused("428!!!!"),
            "That is not a randomizer settings string"
        );
        let newer = format!("999{}", &Preset::Wild.settings()[3..]);
        assert_eq!(
            refused(&newer),
            "Those settings are from a newer randomizer than the app's, FVX 1.6.2"
        );
        // One letter changed, or the end lost.
        let settings = Preset::Classic.settings();
        let typo = settings.replacen("AQgEB", "AQgEC", 1);
        assert_eq!(
            refused(&typo),
            "That settings string is cut short or mistyped"
        );
        assert_eq!(
            refused(&settings[..settings.len() - 8]),
            "That settings string is cut short or mistyped"
        );
    }

    #[test]
    fn titles_become_file_names() {
        assert_eq!(
            file_title("Pokemon: Platinum Version"),
            "Pokemon - Platinum Version"
        );
        assert_eq!(file_title("Red/Blue:Yellow"), "Red-Blue-Yellow");
        assert_eq!(file_title("  "), "Pokémon");
    }

    #[test]
    fn seeds_have_six_digits() {
        for _ in 0..100 {
            let seed = new_seed();
            assert!((100_000..1_000_000).contains(&seed), "{seed}");
        }
    }

    /// A zip like FVX's: the jar, its data and its Java, none executable,
    /// with this system's name for its Java.
    fn fvx_zip(dir: &Path) -> PathBuf {
        let files: [(&str, &[u8]); 4] = [
            (JAR, b"not really a jar"),
            (JAVA, b"#!/bin/sh\n"),
            ("java/lib/jspawnhelper", b"helper"),
            ("data/readme.txt", b"names"),
        ];
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (file, bytes) in files {
            writer.start_file(file, options).expect("start");
            io::Write::write_all(&mut writer, bytes).expect("write");
        }
        let zip = dir.join("fvx.zip");
        fs::write(&zip, writer.finish().expect("finish").into_inner()).expect("zip written");
        zip
    }

    /// Fetches a "URL" that is a local path by copying it.
    fn copy(url: &str, to: &Path) -> Result<(), String> {
        fs::copy(url, to)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn never_fetch(url: &str, _: &Path) -> Result<(), String> {
        panic!("fetched {url}");
    }

    #[test]
    fn installs_from_its_download_with_its_java_executable() {
        let dir = temp_dir("install");
        let zip = fvx_zip(&dir);
        let sha = sha256(&zip).expect("sha");
        let url = zip.to_string_lossy();
        let pin = Download {
            url: &url,
            sha256: &sha,
            megabytes: 1,
        };
        let data = dir.join("Randomizer");
        // An install the app quit during left this behind.
        write(&data.join(".install-1/randomizer.zip"), b"half a download");
        assert_eq!(installed(&data), None);

        let randomizer = install(&data, pin, &copy).expect("install");
        assert_eq!(randomizer.folder, data.join(FOLDER));
        assert_eq!(
            fs::read(randomizer.jar()).expect("jar"),
            b"not really a jar"
        );
        assert_mode(&randomizer.java(), 0o755);
        assert_mode(&data.join(FOLDER).join("java/lib/jspawnhelper"), 0o755);
        assert!(data.join(FOLDER).join("data/readme.txt").is_file());
        assert!(!data.join(".install-1").exists());
        assert_eq!(installed(&data), Some(randomizer.clone()));
        // Installed, it downloads nothing more.
        assert_eq!(install(&data, pin, &never_fetch), Ok(randomizer));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_app_unpacks_a_zip_as_ditto_does() {
        let dir = temp_dir("unzip");
        let zip = fvx_zip(&dir);
        let (in_app, with_unzip) = (dir.join("App"), dir.join("Unzip"));
        unzip_in_app(&zip, &in_app).expect("unpacked in the app");
        unzip(&zip, &with_unzip).expect("unpacked");
        for file in [JAR, JAVA, "java/lib/jspawnhelper", "data/readme.txt"] {
            assert_eq!(
                fs::read(in_app.join(file)).expect("in the app"),
                fs::read(with_unzip.join(file)).expect("unpacked"),
                "{file}"
            );
        }
        assert!(unzip(&dir.join("missing.zip"), &dir.join("Missing")).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn each_system_has_its_own_download() {
        let pins = [
            MAC_ARM64,
            MAC_X86_64,
            LINUX_X86_64,
            LINUX_ARM64,
            WINDOWS_X86_64,
        ];
        for pin in pins {
            assert!(pin.url.contains("/vFVX1.6.2/"), "{}", pin.url);
            assert_eq!(pin.sha256.len(), 64);
            assert!(pin.megabytes > 30);
        }
        let urls: std::collections::HashSet<&str> = pins.iter().map(|pin| pin.url).collect();
        assert_eq!(urls.len(), pins.len(), "one download each");
        // Every system the app is built for has one.
        assert!(PIN.is_some());
        assert_eq!(UNSUPPORTED, None);
        assert_eq!(Some(DOWNLOAD_MB), PIN.map(|pin| pin.megabytes));
        let expected = if cfg!(windows) {
            "java/bin/javaw.exe"
        } else {
            "java/bin/java"
        };
        assert_eq!(JAVA, expected);
    }

    #[test]
    fn a_download_that_is_not_the_one_pinned_installs_nothing() {
        let dir = temp_dir("digest");
        let zip = fvx_zip(&dir);
        let url = zip.to_string_lossy();
        let data = dir.join("Randomizer");
        let error = install(
            &data,
            Download {
                url: &url,
                ..MAC_ARM64
            },
            &copy,
        )
        .expect_err("digest");
        assert!(error.starts_with("The randomizer's download is not the file expected"));
        assert_eq!(installed(&data), None);
        let error = install(&data, MAC_ARM64, &|_, _| {
            Err("Could not resolve host: github.com".to_owned())
        })
        .expect_err("offline");
        assert_eq!(
            error,
            "Cannot download the randomizer: Could not resolve host: github.com"
        );
        let leftovers = fs::read_dir(&data)
            .expect("data")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .count();
        assert_eq!(leftovers, 0);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_install_waits_for_the_first() {
        let data = temp_dir("lock");
        let first = lock(&data).expect("lock");
        thread::scope(|scope| {
            let second = scope.spawn(|| install(&data, MAC_ARM64, &never_fetch));
            thread::sleep(Duration::from_millis(100));
            assert!(!second.is_finished(), "the second install waits");
            write(&data.join(FOLDER).join(JAR), b"jar");
            write(&data.join(FOLDER).join(JAVA), b"java");
            drop(first);
            let installed = second.join().expect("no download").expect("install");
            assert_eq!(installed.folder, data.join(FOLDER));
        });
        fs::remove_dir_all(&data).ok();
    }

    /// A randomizer whose "Java" is `script`, a shell script given FVX's
    /// arguments.
    fn fake_randomizer(dir: &Path, script: &str) -> Randomizer {
        let folder = dir.join(FOLDER);
        write(&folder.join(JAR), b"jar");
        write(&folder.join(JAVA), script.as_bytes());
        make_executable(&folder.join(JAVA)).expect("chmod");
        Randomizer { folder }
    }

    /// Writes what FVX would for a Game Boy game, `<-o>.gbc` and its log,
    /// and records its arguments and folder beside them.
    const WRITES_A_GAME: &str = r#"#!/bin/sh
out=""; prev=""
for arg in "$@"; do [ "$prev" = "-o" ] && out="$arg"; prev="$arg"; done
printf '%s\n' "$@" > "$out.args"; pwd > "$out.pwd"
printf 'RANDOM RED' > "$out.gbc"
printf 'Random Seed: 123456' > "$out.gbc.log"
echo "Randomized successfully!"
"#;

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: the stand-in for FVX's Java is a shell script; randomizes_for_real runs FVX itself"
    )]
    fn a_randomized_game_goes_into_the_hacks_folder_with_its_log() {
        let dir = temp_dir("randomize");
        let randomizer = fake_randomizer(&dir, WRITES_A_GAME);
        let base = dir.join("Pokemon Red.gb");
        write(&base, b"RED");
        let hacks = dir.join("Hacks");
        let settings = Preset::Wild.settings();

        let made = randomize(
            &randomizer,
            &base,
            "Pokemon: Red",
            settings,
            123_456,
            &hacks,
        )
        .expect("randomized");
        let rom = hacks.join("Pokemon - Red (Random 123456).gb");
        assert_eq!(made.rom, rom, "kept as a Game Boy game");
        assert_eq!(fs::read(&rom).expect("rom"), b"RANDOM RED");
        let log = hacks.join("Pokemon - Red (Random 123456).gb.log");
        assert_eq!(made.log.as_ref(), Some(&log));
        assert_eq!(fs::read(&log).expect("log"), b"Random Seed: 123456");
        // Made again, the same game is used as it is.
        let again = randomize(
            &randomizer,
            &base,
            "Pokemon: Red",
            settings,
            123_456,
            &hacks,
        );
        assert_eq!(again, Ok(made));
        assert_eq!(fs::read_dir(&hacks).expect("hacks").count(), 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: the stand-in for FVX's Java is a shell script; randomizes_for_real runs FVX itself"
    )]
    fn the_randomizer_is_given_the_game_settings_and_seed() {
        let dir = temp_dir("arguments");
        // Keeps its work folder's files, to read them back.
        let script = WRITES_A_GAME.replace(
            "echo \"Randomized",
            &format!("cp \"$out\".* \"{}\"; echo \"Randomized", dir.display()),
        );
        let randomizer = fake_randomizer(&dir, &script);
        let base = dir.join("Red.gb");
        write(&base, b"RED");
        let settings = Preset::Classic.settings();
        randomize(
            &randomizer,
            &base,
            "Red",
            settings,
            654_321,
            &dir.join("Hacks"),
        )
        .expect("randomized");
        let args = fs::read_to_string(dir.join("randomized.args")).expect("args");
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(args[..3], ["-Xmx4608M", "-Djava.awt.headless=true", "-jar"]);
        assert_eq!(args[3], randomizer.jar().to_string_lossy());
        assert_eq!(args[4..7], ["cli", "-i", &*base.to_string_lossy()]);
        assert!(args[8].ends_with("/randomized"));
        assert_eq!(args[9..], ["-S", settings, "-z", "654321", "-l"]);
        let pwd = fs::read_to_string(dir.join("randomized.pwd")).expect("pwd");
        let pwd = fs::canonicalize(pwd.trim()).expect("folder");
        assert_eq!(pwd, fs::canonicalize(&randomizer.folder).expect("folder"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: the stand-in for FVX's Java is a shell script; randomizes_for_real runs FVX itself"
    )]
    fn a_refusal_says_what_the_randomizer_said() {
        let dir = temp_dir("refused");
        // As FVX 1.6.2 says it, on its output, with how to use it after.
        let script = "#!/bin/sh\n\
            echo 'ERROR: Could not load /games/Tetris.gb; UNSUPPORTED_ROM'\n\
            echo 'ERROR: Randomization failed'\n\
            echo 'Usage: java [-Xmx4096M] -jar UPR-FVX.jar cli -i <path to source ROM>'\n\
            echo '-h --help          : Print usage/help info.'\nexit 1\n";
        let randomizer = fake_randomizer(&dir, script);
        let base = dir.join("Tetris.gb");
        write(&base, b"TETRIS");
        let hacks = dir.join("Hacks");
        let error = randomize(&randomizer, &base, "Tetris", "428", 1, &hacks).expect_err("refused");
        assert_eq!(
            error,
            "The randomizer refused the game: it does not know this release of the game"
        );
        // A settings string it cannot read, as FVX 1.6.2 says it.
        let script = "#!/bin/sh\n\
            echo 'java.lang.IllegalArgumentException: Last unit does not have enough valid bits'\n\
            echo '\tat java.base/java.util.Base64$Decoder.decode0(Base64.java:872)'\n\
            echo 'ERROR: Invalid settings string.'\n\
            echo 'Usage: java [-Xmx4096M] -jar UPR-FVX.jar cli -i <path to source ROM>'\nexit 1\n";
        let randomizer = fake_randomizer(&dir, script);
        let error = randomize(&randomizer, &base, "Tetris", "428", 1, &hacks).expect_err("refused");
        assert_eq!(error, "The randomizer stopped: Invalid settings string.");
        // An exception it didn't catch, on its errors, says what it was.
        let script = "#!/bin/sh\n\
            echo 'Exception in thread \"main\" java.lang.StringIndexOutOfBoundsException: Range [70, 70 + 111) out of bounds for length 98' >&2\n\
            echo '\tat com.uprfvx.random.Settings.fromString(Settings.java:431)' >&2\nexit 1\n";
        let randomizer = fake_randomizer(&dir, script);
        let error = randomize(&randomizer, &base, "Tetris", "428", 1, &hacks).expect_err("refused");
        assert_eq!(
            error,
            "The randomizer stopped: Range [70, 70 + 111) out of bounds for length 98"
        );
        // The root cause, if there is one, and the exception's class if it
        // says nothing.
        let printed = "java.lang.RuntimeException: wrapped\n\tat a.b(C.java:1)\n\
            Caused by: java.io.IOException: No space left on device\n\tat d.e(F.java:2)\n";
        assert_eq!(
            exception(printed).as_deref(),
            Some("No space left on device")
        );
        assert_eq!(
            exception("Exception in thread \"main\" java.lang.OutOfMemoryError\n").as_deref(),
            Some("OutOfMemoryError")
        );
        assert_eq!(
            exception("Usage: java -jar UPR-FVX.jar cli\nError: oops\n"),
            None
        );
        // Nothing said at all.
        let randomizer = fake_randomizer(&dir, "#!/bin/sh\nexit 3\n");
        let error = randomize(&randomizer, &base, "Tetris", "428", 1, &hacks).expect_err("stopped");
        assert_eq!(error, "The randomizer stopped: exit status: 3");
        // Success said, but no game written.
        let randomizer = fake_randomizer(&dir, "#!/bin/sh\necho done\n");
        let error = randomize(&randomizer, &base, "Tetris", "428", 1, &hacks).expect_err("no game");
        assert_eq!(error, "The randomizer wrote no game");
        assert!(!hacks.exists(), "nothing kept");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: the stand-in for FVX's Java is a shell script; randomizes_for_real runs FVX itself"
    )]
    fn a_game_whose_log_cannot_be_kept_is_kept_without_it() {
        let dir = temp_dir("no-log");
        let script = WRITES_A_GAME.replace("printf 'Random Seed: 123456' > \"$out.gbc.log\"\n", "");
        let randomizer = fake_randomizer(&dir, &script);
        let base = dir.join("Red.gb");
        write(&base, b"RED");
        let hacks = dir.join("Hacks");
        let made = randomize(&randomizer, &base, "Red", "428", 222_222, &hacks).expect("kept");
        assert_eq!(made.rom, hacks.join("Red (Random 222222).gb"));
        assert_eq!(made.log, None);
        assert!(made.rom.is_file());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn work_folders_left_by_runs_that_never_finished_are_cleared() {
        let temp = temp_dir("stale");
        let old = SystemTime::now() - 2 * STALE_WORK;
        let stale = temp.join(format!("{WORK_PREFIX}1-0"));
        let running = temp.join(format!("{WORK_PREFIX}2-0"));
        let other = temp.join("someone-elses-folder");
        for folder in [&stale, &running, &other] {
            write(&folder.join("randomized.nds"), b"ROM");
        }
        for folder in [&stale, &other] {
            crate::leftovers::set_modified(folder, old).expect("age the folder");
        }
        clear_stale_work(&temp);
        assert!(!stale.exists(), "an hour old: left behind");
        assert!(running.exists(), "a run under way keeps its folder");
        assert!(other.exists(), "not the randomizer's");
        fs::remove_dir_all(&temp).ok();
    }

    /// Downloads FVX for real (41 MB) into `DS_EMULATOR_RANDOMIZER_DATA`,
    /// or a temporary folder, and randomizes the ROM at
    /// `DS_EMULATOR_RANDOMIZE_ROM` with each preset into a temporary hacks
    /// folder, printing where each game went.
    #[test]
    #[ignore = "downloads from GitHub and needs a Pokémon ROM"]
    fn randomizes_for_real() {
        let rom = PathBuf::from(
            std::env::var_os("DS_EMULATOR_RANDOMIZE_ROM").expect("DS_EMULATOR_RANDOMIZE_ROM"),
        );
        let data = std::env::var_os("DS_EMULATOR_RANDOMIZER_DATA")
            .map_or_else(|| temp_dir("real"), PathBuf::from);
        let randomizer = ensure(&data).expect("install");
        assert_mode(&randomizer.java(), 0o755);
        assert!(
            generation(&rom).is_some(),
            "{} is not a game FVX knows",
            rom.display()
        );
        let hacks = temp_dir("real-hacks");
        for preset in Preset::ALL {
            let started = std::time::Instant::now();
            let made = randomize(
                &randomizer,
                &rom,
                "Test",
                preset.settings(),
                482_913,
                &hacks,
            )
            .expect("randomized");
            eprintln!(
                "{}: {} in {:.1?}",
                preset.label(),
                made.rom.display(),
                started.elapsed()
            );
            let log = fs::read_to_string(made.log.expect("a log")).expect("log");
            assert!(log.contains("482913"), "the log names the seed");
        }
    }
}
