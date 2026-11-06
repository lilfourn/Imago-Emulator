//! Downloads Azahar's libretro core and MoltenVK at first use, at pinned
//! versions checked against pinned SHA-256 digests.
//!
//! Each part has a folder of its own in the data folder, named for its
//! version, so a later pin installs beside the earlier one:
//! `azahar-2126.2/azahar_libretro.dylib`, and
//! `moltenvk-1.4.2/libMoltenVK.dylib` with MoltenVK's license beside it.
//!
//! An install downloads each missing part with curl into a temporary
//! folder in the data folder, checks its digest, unpacks it with ditto or
//! tar, gathers what is kept into a folder of its own and renames that
//! folder into place, so a part's folder holding its library is always
//! complete. A lock file keeps two installs, from two games opened
//! together or the app and the headless runner, from racing: the second
//! waits for the first and then finds its parts.
//!
//! A part may come with a fix for a bug in it, which [`ensure`] applies to
//! the installed library, one already installed too: the core for Apple
//! silicon gets one for its shader JIT (`jit_fix`).
//!
//! Everything here blocks.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

use crate::jit_fix::{self, Fix};

/// The installed files the helper loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// Azahar's libretro core, `azahar_libretro.dylib`.
    pub core: PathBuf,
    /// MoltenVK, `libMoltenVK.dylib`, Vulkan on Metal.
    pub moltenvk: PathBuf,
}

/// Why the core or MoltenVK could not be installed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// A download failed.
    #[error("could not download {what}: {why}")]
    Download {
        /// What was being downloaded.
        what: String,
        /// curl's reason, or the server's answer.
        why: String,
    },
    /// A download was not the file pinned here, and was deleted.
    #[error(
        "the download of {what}, {file}, is not the file expected, so it was deleted (its SHA-256 is {found})"
    )]
    Digest {
        /// What was downloaded.
        what: String,
        /// The downloaded file's name.
        file: String,
        /// Its SHA-256, in hex.
        found: String,
    },
    /// A download could not be unpacked.
    #[error("could not unpack {what}: {why}")]
    Unpack {
        /// What was being unpacked.
        what: String,
        /// The unpacking tool's reason.
        why: String,
    },
    /// A download did not hold a file it should.
    #[error("the download of {what} has no {file} in it")]
    Missing {
        /// What was downloaded.
        what: String,
        /// The file's path in the download.
        file: String,
    },
    /// A file could not be read or written.
    #[error("could not install the 3DS core: {0}")]
    Io(#[from] io::Error),
}

/// About how many megabytes an install downloads, both parts together,
/// for the app to say so before it starts.
pub const DOWNLOAD_MB: u32 = if cfg!(target_arch = "x86_64") { 68 } else { 67 };

/// What a person would call the core, in messages.
const CORE_NAME: &str = "the 3DS core";

/// Azahar 2126.2's libretro core for Apple silicon (7.35 MB), a zip with
/// the library at its root. Azahar is GPL-3.0.
const CORE_ARM64: Download<'static> = Download {
    what: CORE_NAME,
    url: "https://github.com/azahar-emu/azahar/releases/download/2126.2/azahar-libretro-macos-arm64-2126.2.zip",
    sha256: "52c51e8c62f944a642590652a318fe9c5376fab383b59388384ba81556fb7494",
    folder: "azahar-2126.2",
    archive: Archive::Zip,
    library: "azahar_libretro.dylib",
    license: None,
    fix: Some(jit_fix::LOG2),
};

/// The same core for Intel Macs (8.28 MB), whose shader JIT has no such
/// bug.
const CORE_X86_64: Download<'static> = Download {
    url: "https://github.com/azahar-emu/azahar/releases/download/2126.2/azahar-libretro-macos-x86_64-2126.2.zip",
    sha256: "d65abafb775c964f9064a8319099b2346cd8983d4e58c079f899f6fb5d00aaae",
    fix: None,
    ..CORE_ARM64
};

/// MoltenVK 1.4.2, Vulkan on Metal (Apache-2.0): a 59.6 MB tar of the
/// whole SDK, of which only the macOS library and the license are kept.
const MOLTENVK: Download<'static> = Download {
    what: "MoltenVK for the 3DS core",
    url: "https://github.com/KhronosGroup/MoltenVK/releases/download/v1.4.2/MoltenVK-macos.tar",
    sha256: "f95765a6229cb7b915990a2890ce12ebe36a730b021545d3d52ae69ce4c4024e",
    folder: "moltenvk-1.4.2",
    archive: Archive::Tar,
    library: "MoltenVK/MoltenVK/dynamic/dylib/macOS/libMoltenVK.dylib",
    license: Some("MoltenVK/LICENSE"),
    fix: None,
};

/// The pinned parts for this Mac.
const PINS: Pins<'static> = Pins {
    core: if cfg!(target_arch = "x86_64") {
        CORE_X86_64
    } else {
        CORE_ARM64
    },
    moltenvk: MOLTENVK,
};

/// The file in the data folder an install holds locked.
const LOCK_FILE: &str = "install.lock";
/// How the names of the temporary folders installs work in start.
const TEMP_PREFIX: &str = ".install-";
/// How long a download may take in all, in seconds. MoltenVK is 60 MB.
const DOWNLOAD_SECONDS: &str = "1800";

/// The installed parts in `data`, if both are there.
#[must_use]
pub fn installed(data: &Path) -> Option<Parts> {
    PINS.installed(data)
}

/// Downloads and installs both parts into `data`.
///
/// # Errors
///
/// Fails if a download fails or does not match its digest, or a file
/// cannot be written.
pub fn install(data: &Path) -> Result<Parts, InstallError> {
    PINS.install(data, &curl)
}

/// The installed parts in `data`, installing them first if they are not
/// there, with their fixes applied.
///
/// # Errors
///
/// As [`install`], or if a fixed library cannot be written.
pub fn ensure(data: &Path) -> Result<Parts, InstallError> {
    let parts = match installed(data) {
        Some(parts) => parts,
        None => install(data)?,
    };
    PINS.core.fix(data)?;
    Ok(parts)
}

/// Fetches the file at a URL into a path, or says why it could not.
type Fetch<'f> = &'f dyn Fn(&str, &Path) -> Result<(), String>;

/// The parts an install puts in place.
#[derive(Debug, Clone, Copy)]
struct Pins<'a> {
    core: Download<'a>,
    moltenvk: Download<'a>,
}

impl Pins<'_> {
    /// Where the parts are, or would be, in `data`.
    fn parts(&self, data: &Path) -> Parts {
        Parts {
            core: self.core.library_path(data),
            moltenvk: self.moltenvk.library_path(data),
        }
    }

    /// The parts in `data`, if both are there.
    fn installed(&self, data: &Path) -> Option<Parts> {
        let parts = self.parts(data);
        (parts.core.is_file() && parts.moltenvk.is_file()).then_some(parts)
    }

    /// Installs whichever parts `data` is missing, getting each with
    /// `fetch`, once no other install is under way.
    fn install(&self, data: &Path, fetch: Fetch<'_>) -> Result<Parts, InstallError> {
        fs::create_dir_all(data)?;
        let _lock = lock(data)?;
        // Another install may have finished while this one waited.
        if let Some(parts) = self.installed(data) {
            return Ok(parts);
        }
        clear_unfinished(data);
        let temp = data.join(format!("{TEMP_PREFIX}{}", std::process::id()));
        fs::create_dir(&temp)?;
        let result = [self.core, self.moltenvk]
            .iter()
            .filter(|part| !part.library_path(data).is_file())
            .try_for_each(|part| part.install(data, &temp, fetch));
        // What is left behind is cleared by the next install.
        fs::remove_dir_all(&temp).ok();
        result.map(|()| self.parts(data))
    }
}

/// A pinned download, and what is kept from it.
#[derive(Debug, Clone, Copy)]
struct Download<'a> {
    /// What a person would call it, in messages.
    what: &'a str,
    /// Where it is downloaded from.
    url: &'a str,
    /// Its SHA-256, in lowercase hex.
    sha256: &'a str,
    /// The folder in the data folder it is kept in, named for its version.
    folder: &'a str,
    /// How it is packed.
    archive: Archive,
    /// The library's path in the archive.
    library: &'a str,
    /// The license's path in the archive, if it holds one, kept as
    /// `LICENSE` beside the library.
    license: Option<&'a str>,
    /// A fix for a bug in the library, applied once it is installed.
    fix: Option<Fix<'a>>,
}

/// How a download is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Archive {
    /// A zip, unpacked whole with ditto.
    Zip,
    /// A tar, of which only the files kept are unpacked.
    Tar,
}

impl Download<'_> {
    /// Where the library is, or would be, once installed in `data`.
    fn library_path(&self, data: &Path) -> PathBuf {
        data.join(self.folder).join(file_name(self.library))
    }

    /// Downloads, checks and unpacks this part in `temp`, then moves what
    /// is kept into its folder in `data`.
    fn install(&self, data: &Path, temp: &Path, fetch: Fetch<'_>) -> Result<(), InstallError> {
        let archive = temp.join(file_name(self.url));
        fetch(self.url, &archive).map_err(|why| InstallError::Download {
            what: self.what.to_owned(),
            why,
        })?;
        self.verify(&archive)?;
        let unpacked = temp.join(format!("{}.unpacked", self.folder));
        self.unpack(&archive, &unpacked)?;
        fs::remove_file(&archive)?;
        let prepared = temp.join(self.folder);
        self.prepare(&unpacked, &prepared)?;
        place(&prepared, &data.join(self.folder))?;
        Ok(())
    }

    /// Applies this part's fix to its library in `data`, if it has one the
    /// library still wants, once no install is under way.
    fn fix(&self, data: &Path) -> Result<(), InstallError> {
        let Some(fix) = self.fix else {
            return Ok(());
        };
        let library = self.library_path(data);
        if fix.wanted(&library)? {
            let _lock = lock(data)?;
            // Another process may have fixed it while this one waited,
            // and then it is no longer the library the fix is for.
            fix.apply(&library)?;
        }
        Ok(())
    }

    /// Checks that `archive` has the pinned digest, deleting it if not.
    fn verify(&self, archive: &Path) -> Result<(), InstallError> {
        let found = sha256(archive)?;
        if found == self.sha256 {
            return Ok(());
        }
        fs::remove_file(archive)?;
        Err(InstallError::Digest {
            what: self.what.to_owned(),
            file: archive
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            found,
        })
    }

    /// Unpacks `archive` into `into`: a zip whole, a tar only the files
    /// kept.
    fn unpack(&self, archive: &Path, into: &Path) -> Result<(), InstallError> {
        fs::create_dir_all(into)?;
        let (tool, output) = match self.archive {
            Archive::Zip => (
                "ditto",
                Command::new("/usr/bin/ditto")
                    .args(["-x", "-k"])
                    .arg(archive)
                    .arg(into)
                    .output(),
            ),
            Archive::Tar => (
                "tar",
                Command::new("/usr/bin/tar")
                    .arg("-xf")
                    .arg(archive)
                    .arg("-C")
                    .arg(into)
                    .arg(self.library)
                    .args(self.license)
                    .output(),
            ),
        };
        let why = match output {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => reason(&output),
            Err(error) => format!("could not run {tool}: {error}"),
        };
        Err(InstallError::Unpack {
            what: self.what.to_owned(),
            why,
        })
    }

    /// Gathers the files kept from `unpacked` into the folder `prepared`:
    /// the library, executable, and the license.
    fn prepare(&self, unpacked: &Path, prepared: &Path) -> Result<(), InstallError> {
        fs::create_dir_all(prepared)?;
        let library = prepared.join(file_name(self.library));
        self.keep(unpacked, self.library, &library, 0o755)?;
        if let Some(license) = self.license {
            self.keep(unpacked, license, &prepared.join("LICENSE"), 0o644)?;
        }
        Ok(())
    }

    /// Moves `member` from `unpacked` to `to`, with permissions `mode`.
    fn keep(
        &self,
        unpacked: &Path,
        member: &str,
        to: &Path,
        mode: u32,
    ) -> Result<(), InstallError> {
        let from = unpacked.join(member);
        if !from.is_file() {
            return Err(InstallError::Missing {
                what: self.what.to_owned(),
                file: member.to_owned(),
            });
        }
        fs::rename(&from, to)?;
        set_mode(to, mode)?;
        Ok(())
    }
}

/// Gives the file at `path` the Unix permissions `mode`. Windows files have
/// no such bits, and need none to be loaded.
pub(crate) fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
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

/// Removes the temporary folders of installs that never finished, as
/// when the app quit during one. Only called holding the lock, so none of
/// them is in use.
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
    fs::rename(prepared, target)
}

/// Downloads `url` into the file `to` with curl.
fn curl(url: &str, to: &Path) -> Result<(), String> {
    let output = Command::new("/usr/bin/curl")
        .args(["--silent", "--show-error", "--location"])
        .args([
            "--user-agent",
            concat!(
                "ImagoEmulator/",
                env!("CARGO_PKG_VERSION"),
                " (+https://imagoemulator.com)"
            ),
        ])
        .args(["--connect-timeout", "10", "--max-time", DOWNLOAD_SECONDS])
        // Gives up on a download stalled below 1 KB/s for a minute, and
        // tries again twice after a passing failure.
        .args([
            "--speed-limit",
            "1024",
            "--speed-time",
            "60",
            "--retry",
            "2",
        ])
        .arg("--output")
        .arg(to)
        .args(["--write-out", "%{http_code}"])
        .arg(url)
        .output()
        .map_err(|error| format!("could not run curl: {error}"))?;
    // A transfer cut short can still say 200; only curl's own status tells.
    if !output.status.success() {
        return Err(reason(&output));
    }
    match String::from_utf8_lossy(&output.stdout).trim() {
        "200" => Ok(()),
        status => Err(format!("the server answered with HTTP status {status}")),
    }
}

/// Why the command that gave `output` failed: the last line it printed as
/// an error, or else its exit status.
fn reason(output: &Output) -> String {
    let errors = String::from_utf8_lossy(&output.stderr);
    errors
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map_or_else(|| output.status.to_string(), str::to_owned)
}

/// The SHA-256 of the file at `path`, in lowercase hex, read a piece at a
/// time.
fn sha256(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// The last part of the `/`-separated `path`.
fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    const CORE_BYTES: &[u8] = b"not really a core";
    const MOLTENVK_BYTES: &[u8] = b"not really MoltenVK";
    const LICENSE_BYTES: &[u8] = b"Apache License, Version 2.0";

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("emu-3ds-install-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Writes `bytes` to `path`, making its folder.
    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        fs::write(path, bytes).expect("write");
    }

    /// Runs one of macOS's tools, which must succeed.
    fn run(tool: &str, args: &[&Path]) {
        let status = Command::new(tool).args(args).status().expect("run");
        assert!(status.success(), "{tool} {args:?}");
    }

    /// A zip like Azahar's, with the core at its root, made in `dir`.
    fn core_zip(dir: &Path) -> PathBuf {
        let source = dir.join("core-source");
        write(&source.join("azahar_libretro.dylib"), CORE_BYTES);
        let zip = dir.join("core.zip");
        run(
            "/usr/bin/ditto",
            &[Path::new("-c"), Path::new("-k"), &source, &zip],
        );
        zip
    }

    /// A tar like MoltenVK's, with more in it than is kept, made in `dir`.
    fn moltenvk_tar(dir: &Path) -> PathBuf {
        let source = dir.join("moltenvk-source");
        write(&source.join(MOLTENVK.library), MOLTENVK_BYTES);
        write(&source.join("MoltenVK/LICENSE"), LICENSE_BYTES);
        write(
            &source.join("MoltenVK/MoltenVK/include/vulkan.h"),
            b"// headers",
        );
        let tar = dir.join("moltenvk.tar");
        run(
            "/usr/bin/tar",
            &[
                Path::new("-cf"),
                &tar,
                Path::new("-C"),
                &source,
                Path::new("MoltenVK"),
            ],
        );
        tar
    }

    /// The pins, downloading from local `zip` and `tar` paths instead.
    fn local_pins<'a>(zip: &'a str, zip_sha: &'a str, tar: &'a str, tar_sha: &'a str) -> Pins<'a> {
        Pins {
            core: Download {
                url: zip,
                sha256: zip_sha,
                ..CORE_ARM64
            },
            moltenvk: Download {
                url: tar,
                sha256: tar_sha,
                ..MOLTENVK
            },
        }
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

    fn leftovers(data: &Path) -> Vec<String> {
        fs::read_dir(data)
            .expect("read data")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(TEMP_PREFIX))
            .collect()
    }

    #[test]
    fn parts_are_kept_in_folders_named_for_their_versions() {
        let data = Path::new("/data/3DS");
        assert_eq!(
            PINS.parts(data),
            Parts {
                core: data.join("azahar-2126.2/azahar_libretro.dylib"),
                moltenvk: data.join("moltenvk-1.4.2/libMoltenVK.dylib"),
            }
        );
        assert!(PINS.core.url.ends_with(if cfg!(target_arch = "x86_64") {
            "x86_64-2126.2.zip"
        } else {
            "arm64-2126.2.zip"
        }));
    }

    #[test]
    fn both_libraries_must_be_there_to_be_installed() {
        let data = temp_dir("installed");
        assert_eq!(installed(&data), None);
        let parts = PINS.parts(&data);
        write(&parts.core, CORE_BYTES);
        assert_eq!(installed(&data), None, "no MoltenVK yet");
        fs::create_dir_all(&parts.moltenvk).expect("a folder in its place");
        assert_eq!(installed(&data), None, "a folder is not a library");
        fs::remove_dir(&parts.moltenvk).expect("remove the folder");
        write(&parts.moltenvk, MOLTENVK_BYTES);
        assert_eq!(installed(&data), Some(parts.clone()));
        assert_eq!(ensure(&data).expect("already there"), parts);
        fs::remove_dir_all(&data).ok();
    }

    #[test]
    fn a_download_must_match_its_digest() {
        let dir = temp_dir("digest");
        let file = dir.join("download.zip");
        write(&file, b"abc");
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let download = Download {
            sha256: sha,
            ..CORE_ARM64
        };
        download.verify(&file).expect("the digest matches");
        assert!(file.exists());
        let error = CORE_ARM64.verify(&file).expect_err("the digest differs");
        assert!(
            matches!(&error, InstallError::Digest { file, found, .. }
                if file == "download.zip" && found == sha),
            "{error}"
        );
        assert!(!file.exists(), "a download that does not match is deleted");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "Linux, Windows: zips unpack with macOS's ditto; the 3DS stream picks how Azahar's Linux and Windows cores unpack"
    )]
    fn a_zip_unpacks_whole() {
        let dir = temp_dir("zip");
        let zip = core_zip(&dir);
        let unpacked = dir.join("unpacked");
        CORE_ARM64.unpack(&zip, &unpacked).expect("unpack");
        let core = fs::read(unpacked.join("azahar_libretro.dylib")).expect("the core");
        assert_eq!(core, CORE_BYTES);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: MoltenVK's tar unpacks with /usr/bin/tar, and only macOS needs MoltenVK (3DS stream)"
    )]
    fn a_tar_unpacks_only_the_files_kept() {
        let dir = temp_dir("tar");
        let tar = moltenvk_tar(&dir);
        let unpacked = dir.join("unpacked");
        MOLTENVK.unpack(&tar, &unpacked).expect("unpack");
        assert_eq!(
            fs::read(unpacked.join(MOLTENVK.library)).expect("library"),
            MOLTENVK_BYTES
        );
        assert_eq!(
            fs::read(unpacked.join("MoltenVK/LICENSE")).expect("license"),
            LICENSE_BYTES
        );
        assert!(!unpacked.join("MoltenVK/MoltenVK/include").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "Linux, Windows: zips unpack with macOS's ditto; the 3DS stream picks how Azahar's Linux and Windows cores unpack"
    )]
    fn an_archive_without_the_library_is_refused() {
        let dir = temp_dir("missing");
        let source = dir.join("source");
        write(&source.join("README"), b"no core here");
        let zip = dir.join("core.zip");
        run(
            "/usr/bin/ditto",
            &[Path::new("-c"), Path::new("-k"), &source, &zip],
        );
        let unpacked = dir.join("unpacked");
        CORE_ARM64.unpack(&zip, &unpacked).expect("unpack");
        let error = CORE_ARM64
            .prepare(&unpacked, &dir.join("prepared"))
            .expect_err("no library");
        assert!(matches!(error, InstallError::Missing { .. }), "{error}");
        // A tar is refused by tar itself.
        let tar = dir.join("moltenvk.tar");
        run(
            "/usr/bin/tar",
            &[
                Path::new("-cf"),
                &tar,
                Path::new("-C"),
                &source,
                Path::new("README"),
            ],
        );
        let error = MOLTENVK
            .unpack(&tar, &dir.join("unpacked-tar"))
            .expect_err("no library");
        assert!(matches!(error, InstallError::Unpack { .. }), "{error}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "Linux, Windows: zips unpack with macOS's ditto; the 3DS stream picks how Azahar's Linux and Windows cores unpack"
    )]
    fn installs_from_downloads_and_keeps_only_the_parts() {
        let dir = temp_dir("local");
        let (zip, tar) = (core_zip(&dir), moltenvk_tar(&dir));
        let (zip_sha, tar_sha) = (sha256(&zip).expect("sha"), sha256(&tar).expect("sha"));
        let (zip, tar) = (zip.to_string_lossy(), tar.to_string_lossy());
        let pins = local_pins(&zip, &zip_sha, &tar, &tar_sha);
        let data = dir.join("3DS");
        // An install the app quit during left this behind.
        write(&data.join(".install-1/core.zip"), b"half a download");

        let parts = pins.install(&data, &copy).expect("install");
        assert_eq!(parts, PINS.parts(&data));
        assert_eq!(fs::read(&parts.core).expect("core"), CORE_BYTES);
        assert_eq!(fs::read(&parts.moltenvk).expect("MoltenVK"), MOLTENVK_BYTES);
        assert_mode(&parts.core, 0o755);
        assert_mode(&parts.moltenvk, 0o755);
        let license = data.join("moltenvk-1.4.2/LICENSE");
        assert_eq!(fs::read(&license).expect("license"), LICENSE_BYTES);
        let mut kept: Vec<_> = fs::read_dir(data.join("moltenvk-1.4.2"))
            .expect("folder")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        kept.sort();
        assert_eq!(kept, ["LICENSE", "libMoltenVK.dylib"]);
        assert_eq!(leftovers(&data), Vec::<String>::new());

        // Installed, it downloads nothing more.
        assert_eq!(pins.install(&data, &never_fetch).expect("again"), parts);
        // Without its library, a part is installed again, and only it.
        fs::remove_file(&parts.moltenvk).expect("remove");
        let core_only = Pins {
            core: Download {
                url: "never fetched",
                ..pins.core
            },
            ..pins
        };
        assert_eq!(core_only.install(&data, &copy).expect("reinstall"), parts);
        assert_eq!(fs::read(&parts.moltenvk).expect("MoltenVK"), MOLTENVK_BYTES);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "Linux, Windows: zips unpack with macOS's ditto; the 3DS stream picks how Azahar's Linux and Windows cores unpack"
    )]
    fn a_failed_install_leaves_nothing_behind() {
        let dir = temp_dir("failed");
        let (zip, tar) = (core_zip(&dir), moltenvk_tar(&dir));
        let zip_sha = sha256(&zip).expect("sha");
        let (zip, tar) = (zip.to_string_lossy(), tar.to_string_lossy());
        let pins = local_pins(&zip, &zip_sha, &tar, MOLTENVK.sha256);
        let data = dir.join("3DS");
        let error = pins
            .install(&data, &copy)
            .expect_err("MoltenVK does not match");
        assert!(matches!(error, InstallError::Digest { .. }), "{error}");
        assert!(!data.join("moltenvk-1.4.2").exists());
        assert_eq!(leftovers(&data), Vec::<String>::new());
        assert_eq!(installed(&data), None);

        let error = pins
            .install(&data, &|_, _| {
                Err("Could not resolve host: github.com".to_owned())
            })
            .expect_err("offline");
        assert_eq!(
            error.to_string(),
            "could not download MoltenVK for the 3DS core: Could not resolve host: github.com"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_install_waits_for_the_first_and_uses_its_parts() {
        let data = temp_dir("lock");
        let first = lock(&data).expect("lock");
        thread::scope(|scope| {
            let second = scope.spawn(|| PINS.install(&data, &never_fetch));
            thread::sleep(Duration::from_millis(100));
            assert!(!second.is_finished(), "the second install waits");
            let parts = PINS.parts(&data);
            write(&parts.core, CORE_BYTES);
            write(&parts.moltenvk, MOLTENVK_BYTES);
            drop(first);
            let installed = second.join().expect("no download").expect("install");
            assert_eq!(installed, parts);
        });
        fs::remove_dir_all(&data).ok();
    }

    /// Downloads both parts for real, about 67 MB.
    #[test]
    #[ignore = "downloads from GitHub"]
    fn installs_for_real() {
        let data = temp_dir("real");
        let started = Instant::now();
        let parts = install(&data).expect("install");
        eprintln!("installed in {:.1?}", started.elapsed());
        for library in [&parts.core, &parts.moltenvk] {
            let bytes = fs::read(library).expect("library");
            // A Mach-O library, for one architecture or several.
            let magic = u32::from_le_bytes(bytes[..4].try_into().expect("magic"));
            assert!(
                matches!(magic, 0xFEED_FACF | 0xBEBA_FECA),
                "{library:?}: {magic:#x}"
            );
            assert_mode(library, 0o755);
        }
        assert!(data.join("moltenvk-1.4.2/LICENSE").is_file());
        assert_eq!(installed(&data), Some(parts.clone()));
        assert_eq!(leftovers(&data), Vec::<String>::new());

        // The core gets its fix, signed so that macOS still loads it.
        assert_eq!(ensure(&data).expect("ensure"), parts);
        if let Some(fix) = PINS.core.fix {
            assert!(!fix.wanted(&parts.core).expect("wanted"), "fixed");
            run("/usr/bin/codesign", &[Path::new("--verify"), &parts.core]);
        }
        assert_mode(&parts.core, 0o755);
        fs::remove_dir_all(&data).ok();
    }
}
