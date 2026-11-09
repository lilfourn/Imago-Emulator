//! Downloads Azahar's libretro core, and on macOS MoltenVK, at first use,
//! at pinned versions checked against pinned SHA-256 digests.
//!
//! Each part has a folder of its own in the data folder, named for its
//! version, so a later pin installs beside the earlier one:
//! `azahar-2126.2/azahar_libretro.dylib` (`.so` on Linux, `.dll` on
//! Windows), and on macOS `moltenvk-1.4.2/libMoltenVK.dylib` with
//! MoltenVK's license beside it. Linux and Windows have no MoltenVK: the
//! helper loads the system's own Vulkan loader there.
//!
//! An install downloads each missing part with curl into a temporary
//! folder in the data folder, checks its digest, unpacks it (with ditto
//! or tar on macOS, as it always has, and with the zip crate elsewhere),
//! gathers what is kept into a folder of its own and renames that folder
//! into place, so a part's folder holding its library is always complete.
//! A lock file keeps two installs, from two games opened together or the
//! app and the headless runner, from racing: the second waits for the
//! first and then finds its parts. The lock, the temporary folder, the
//! check and the rename are [`emu_os::install`]'s, which mkxp-z and the
//! randomizer install by too.
//!
//! A part may come with a fix for a bug in it, which [`ensure`] applies to
//! the installed library, one already installed too: the core for Apple
//! silicon gets one for its shader JIT (`jit_fix`). The x86-64 cores need
//! none.
//!
//! Azahar 2126.2 makes cores for macOS, Linux x86-64 and Windows x86-64,
//! but none for Linux on ARM, so there is nothing to install there
//! ([`crate::UNSUPPORTED`]).
//!
//! Everything here blocks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use emu_os::install::{self as pinned, Fetch, Install, Mismatch, Pinned};

use crate::jit_fix::{self, Fix};

/// The installed files the helper loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// Azahar's libretro core: `azahar_libretro.dylib` on macOS,
    /// `azahar_libretro.so` on Linux, `azahar_libretro.dll` on Windows.
    pub core: PathBuf,
    /// MoltenVK, `libMoltenVK.dylib`, Vulkan on Metal, on macOS. Linux and
    /// Windows have none: the helper loads the system's Vulkan loader
    /// (`libvulkan.so.1`, `vulkan-1.dll`).
    pub moltenvk: Option<PathBuf>,
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
    /// There is no 3DS core for this system ([`crate::UNSUPPORTED`]).
    #[error("{0}")]
    Unsupported(&'static str),
}

/// About how many megabytes an install downloads, every part together,
/// for the app to say so before it starts: 67 on a Mac with Apple
/// silicon, 68 on an Intel Mac, 15 on Linux and 12 on Windows.
pub const DOWNLOAD_MB: u32 = match PINS {
    Some(pins) => pins.download_mb(),
    None => 0,
};

/// What a person would call the core, in messages.
const CORE_NAME: &str = "the 3DS core";

/// Azahar 2126.2's libretro core for Apple silicon (7.35 MB), a zip with
/// the library at its root. Azahar is GPL-3.0.
const CORE_MACOS_ARM64: Download<'static> = Download {
    what: CORE_NAME,
    url: "https://github.com/azahar-emu/azahar/releases/download/2126.2/azahar-libretro-macos-arm64-2126.2.zip",
    sha256: "52c51e8c62f944a642590652a318fe9c5376fab383b59388384ba81556fb7494",
    size: 7_351_896,
    folder: "azahar-2126.2",
    archive: Archive::Zip,
    library: "azahar_libretro.dylib",
    license: None,
    fix: Some(jit_fix::LOG2),
};

/// The same core for Intel Macs (8.28 MB), whose shader JIT has no such
/// bug.
const CORE_MACOS_X86_64: Download<'static> = Download {
    url: "https://github.com/azahar-emu/azahar/releases/download/2126.2/azahar-libretro-macos-x86_64-2126.2.zip",
    sha256: "d65abafb775c964f9064a8319099b2346cd8983d4e58c079f899f6fb5d00aaae",
    size: 8_280_438,
    fix: None,
    ..CORE_MACOS_ARM64
};

/// The same core for x86-64 Linux (14.7 MB), a zip holding
/// `azahar_libretro.so` alone. It needs glibc 2.34 and GCC 12's
/// libstdc++ (Ubuntu 22.04, Debian 12 or newer), and gets Vulkan from
/// the system's loader.
const CORE_LINUX_X86_64: Download<'static> = Download {
    url: "https://github.com/azahar-emu/azahar/releases/download/2126.2/azahar-libretro-linux-x86_64-2126.2.zip",
    sha256: "2223444b74de2c922bae1efcfed60002f696b3c2aaf4fc73fc03e66a395dc26e",
    size: 14_742_826,
    library: "azahar_libretro.so",
    fix: None,
    ..CORE_MACOS_ARM64
};

/// The same core for x86-64 Windows (12.1 MB), a zip holding
/// `azahar_libretro.dll` alone. A MinGW build on the system's
/// `msvcrt.dll`, so it needs no Visual C++ runtime; it gets Vulkan from
/// the system's loader.
const CORE_WINDOWS_X86_64: Download<'static> = Download {
    url: "https://github.com/azahar-emu/azahar/releases/download/2126.2/azahar-libretro-windows-x86_64-2126.2.zip",
    sha256: "d863711cff775ed697ba0cc5a963f2145b9672c4ad9a891367821d7f3cc1f888",
    size: 12_097_452,
    library: "azahar_libretro.dll",
    fix: None,
    ..CORE_MACOS_ARM64
};

/// MoltenVK 1.4.2, Vulkan on Metal (Apache-2.0): a 59.6 MB tar of the
/// whole SDK, of which only the macOS library and the license are kept.
const MOLTENVK: Download<'static> = Download {
    what: "MoltenVK for the 3DS core",
    url: "https://github.com/KhronosGroup/MoltenVK/releases/download/v1.4.2/MoltenVK-macos.tar",
    sha256: "f95765a6229cb7b915990a2890ce12ebe36a730b021545d3d52ae69ce4c4024e",
    size: 59_563_008,
    folder: "moltenvk-1.4.2",
    archive: Archive::Tar,
    library: "MoltenVK/MoltenVK/dynamic/dylib/macOS/libMoltenVK.dylib",
    license: Some("MoltenVK/LICENSE"),
    fix: None,
};

/// The pinned parts for this system, if Azahar makes a core for it.
const PINS: Option<Pins<'static>> = if cfg!(target_os = "macos") {
    Some(Pins {
        core: if cfg!(target_arch = "x86_64") {
            CORE_MACOS_X86_64
        } else {
            CORE_MACOS_ARM64
        },
        moltenvk: Some(MOLTENVK),
    })
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some(Pins {
        core: CORE_LINUX_X86_64,
        moltenvk: None,
    })
} else if cfg!(all(windows, target_arch = "x86_64")) {
    Some(Pins {
        core: CORE_WINDOWS_X86_64,
        moltenvk: None,
    })
} else {
    None
};

/// How long a download may take in all, in seconds. MoltenVK is 60 MB.
const DOWNLOAD_SECONDS: u32 = 1800;
/// The most a download may be: twice MoltenVK, the biggest.
const MAX_DOWNLOAD: u64 = 128 << 20;

/// The installed parts in `data`, if all of them are there.
#[must_use]
pub fn installed(data: &Path) -> Option<Parts> {
    PINS?.installed(data)
}

/// Downloads and installs every part into `data`.
///
/// # Errors
///
/// Fails if there is no core for this system, a download fails or does not
/// match its digest, or a file cannot be written.
pub fn install(data: &Path) -> Result<Parts, InstallError> {
    pins()?.install(data, &curl)
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
    pins()?.core.fix(data)?;
    Ok(parts)
}

/// This system's pins, or why it has none.
fn pins() -> Result<Pins<'static>, InstallError> {
    PINS.ok_or(InstallError::Unsupported(match crate::UNSUPPORTED {
        Some(why) => why,
        None => "there is no 3DS core for this system",
    }))
}

/// The parts an install puts in place.
#[derive(Debug, Clone, Copy)]
struct Pins<'a> {
    core: Download<'a>,
    /// MoltenVK, on macOS only.
    moltenvk: Option<Download<'a>>,
}

impl Pins<'_> {
    /// Where the parts are, or would be, in `data`.
    fn parts(&self, data: &Path) -> Parts {
        Parts {
            core: self.core.library_path(data),
            moltenvk: self.moltenvk.map(|part| part.library_path(data)),
        }
    }

    /// The parts in `data`, if all of them are there.
    fn installed(&self, data: &Path) -> Option<Parts> {
        let parts = self.parts(data);
        let there =
            parts.core.is_file() && parts.moltenvk.as_ref().is_none_or(|path| path.is_file());
        there.then_some(parts)
    }

    /// About how many megabytes the parts are to download, together.
    const fn download_mb(&self) -> u32 {
        let mut bytes = self.core.size;
        if let Some(moltenvk) = self.moltenvk {
            bytes += moltenvk.size;
        }
        ((bytes + 500_000) / 1_000_000) as u32
    }

    /// Installs whichever parts `data` is missing, getting each with
    /// `fetch`, once no other install is under way.
    fn install(&self, data: &Path, fetch: Fetch<'_>) -> Result<Parts, InstallError> {
        let install = lock(data)?;
        // Another install may have finished while this one waited.
        if let Some(parts) = self.installed(data) {
            return Ok(parts);
        }
        let temp = install.temp()?;
        std::iter::once(self.core)
            .chain(self.moltenvk)
            .filter(|part| !part.library_path(data).is_file())
            .try_for_each(|part| part.install(data, temp.path(), fetch))?;
        Ok(self.parts(data))
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
    /// Its size in bytes, for saying how much an install downloads.
    size: u64,
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
    /// A zip, unpacked whole: with ditto on macOS, as it always was, and
    /// with the zip crate elsewhere.
    Zip,
    /// A tar, of which only the files kept are unpacked, with macOS's tar.
    /// Only MoltenVK, which only macOS needs, comes as one.
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
        pinned::place(&prepared, &data.join(self.folder))?;
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
        let pinned = Pinned {
            url: self.url,
            sha256: self.sha256,
            // Said to the player before an install; the digest is the check.
            size: None,
        };
        pinned.verify(archive).map_err(|error| match error {
            pinned::Error::NotPinned(differs) => InstallError::Digest {
                what: self.what.to_owned(),
                file: archive
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                found: match differs {
                    Mismatch::Digest { found } => found,
                    size @ Mismatch::Size { .. } => size.to_string(),
                },
            },
            pinned::Error::Download(why) => InstallError::Download {
                what: self.what.to_owned(),
                why,
            },
            pinned::Error::Io(error) => InstallError::Io(error),
        })
    }

    /// Unpacks `archive` into `into`: a zip whole, a tar only the files
    /// kept.
    fn unpack(&self, archive: &Path, into: &Path) -> Result<(), InstallError> {
        fs::create_dir_all(into)?;
        let unpacked = match self.archive {
            Archive::Zip => unzip(archive, into),
            Archive::Tar => run(
                "tar",
                Command::new("/usr/bin/tar")
                    .arg("-xf")
                    .arg(archive)
                    .arg("-C")
                    .arg(into)
                    .arg(self.library)
                    .args(self.license),
            ),
        };
        unpacked.map_err(|why| InstallError::Unpack {
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

/// Unpacks the whole zip `archive` into `into` with macOS's ditto.
#[cfg(target_os = "macos")]
fn unzip(archive: &Path, into: &Path) -> Result<(), String> {
    run(
        "ditto",
        Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(archive)
            .arg(into),
    )
}

/// Unpacks the whole zip `archive` into `into` with the zip crate, which
/// keeps every name inside `into`.
#[cfg(not(target_os = "macos"))]
fn unzip(archive: &Path, into: &Path) -> Result<(), String> {
    let file = fs::File::open(archive).map_err(|error| error.to_string())?;
    zip::ZipArchive::new(file)
        .and_then(|mut zip| zip.extract(into))
        .map_err(|error| error.to_string())
}

/// Runs `command`, the tool `tool`, which must succeed.
fn run(tool: &str, command: &mut Command) -> Result<(), String> {
    match command.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(reason(&output)),
        Err(error) => Err(format!("could not run {tool}: {error}")),
    }
}

/// Starts `command` without a console window of its own, which Windows
/// would otherwise open for a console program started from the app. Does
/// nothing elsewhere.
pub(crate) fn without_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command
}

/// Takes the lock on installing into `data`, which is made if it is not
/// there, waiting for any other install to finish first. It is held until
/// the install is dropped.
fn lock(data: &Path) -> io::Result<Install> {
    Install::begin(data)
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
    download.to_file(to).map_err(|error| match error {
        emu_os::fetch::Error::NotStarted(error) => match error.kind() {
            // Every Mac has curl; a Linux system may not.
            io::ErrorKind::NotFound if !cfg!(target_os = "macos") => {
                "curl, which downloads it, is not installed".to_owned()
            }
            _ => format!("could not run curl: {error}"),
        },
        error => error.to_string(),
    })
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

/// The last part of the `/`-separated `path`.
fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_os::install::{TEMP_PREFIX, sha256};
    use std::fs::File;
    use std::io::Write as _;
    use std::thread;
    use std::time::{Duration, Instant};

    const CORE_BYTES: &[u8] = b"not really a core";
    const MOLTENVK_BYTES: &[u8] = b"not really MoltenVK";
    #[cfg(unix)]
    const LICENSE_BYTES: &[u8] = b"Apache License, Version 2.0";

    /// This system's core, or Linux's where there is none, to test with.
    const CORE: Download<'static> = match PINS {
        Some(pins) => pins.core,
        None => CORE_LINUX_X86_64,
    };

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

    /// Runs one of the system's tools, which must succeed.
    #[cfg(unix)]
    fn run(tool: &str, args: &[&Path]) {
        let status = Command::new(tool).args(args).status().expect("run");
        assert!(status.success(), "{tool} {args:?}");
    }

    /// Writes a zip of `files`, each a name and its bytes, to `path`.
    fn zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).expect("zip"));
        for (name, bytes) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start a file");
            zip.write_all(bytes).expect("write a file");
        }
        zip.finish().expect("finish the zip");
    }

    /// A zip like Azahar's, with the core at its root, made in `dir`, and a
    /// file that is not kept.
    fn core_zip(dir: &Path) -> PathBuf {
        let path = dir.join("core.zip");
        zip(
            &path,
            &[(CORE.library, CORE_BYTES), ("README.txt", b"not kept")],
        );
        path
    }

    /// A tar like MoltenVK's, with more in it than is kept, made in `dir`.
    #[cfg(unix)]
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

    /// Pins downloading the core from the local path `zip`, and MoltenVK
    /// from `moltenvk`'s, if given.
    fn local_pins<'a>(
        zip: &'a str,
        zip_sha: &'a str,
        moltenvk: Option<(&'a str, &'a str)>,
    ) -> Pins<'a> {
        Pins {
            core: Download {
                url: zip,
                sha256: zip_sha,
                ..CORE
            },
            moltenvk: moltenvk.map(|(tar, tar_sha)| Download {
                url: tar,
                sha256: tar_sha,
                ..MOLTENVK
            }),
        }
    }

    /// `path` as a "URL" for [`copy`]: with `/` between its parts on every
    /// system, as a URL has, so the file name the install takes from it
    /// is the file's own. Windows opens paths written this way too.
    fn url(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
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
    fn parts_are_kept_in_folders_named_for_their_versions() {
        let Some(pins) = PINS else {
            assert!(crate::UNSUPPORTED.is_some(), "no core, so no 3DS games");
            return;
        };
        let data = Path::new("/data/3DS");
        let (library, os) = if cfg!(target_os = "macos") {
            ("azahar_libretro.dylib", "macos")
        } else if cfg!(windows) {
            ("azahar_libretro.dll", "windows")
        } else {
            ("azahar_libretro.so", "linux")
        };
        assert_eq!(
            pins.parts(data),
            Parts {
                core: data.join("azahar-2126.2").join(library),
                moltenvk: cfg!(target_os = "macos")
                    .then(|| data.join("moltenvk-1.4.2").join("libMoltenVK.dylib")),
            }
        );
        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else {
            "arm64"
        };
        assert!(
            pins.core.url.ends_with(&format!("{os}-{arch}-2126.2.zip")),
            "{}",
            pins.core.url
        );
        // Only the core for Apple silicon has the shader JIT's bug.
        assert_eq!(
            pins.core.fix.is_some(),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
    }

    #[test]
    fn the_app_says_how_much_an_install_downloads() {
        let expected = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            67
        } else if cfg!(target_os = "macos") {
            68
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            15
        } else if cfg!(all(windows, target_arch = "x86_64")) {
            12
        } else {
            0
        };
        assert_eq!(DOWNLOAD_MB, expected);
    }

    #[test]
    fn without_a_core_for_this_system_nothing_is_installed() {
        assert_eq!(PINS.is_none(), crate::UNSUPPORTED.is_some());
        if let Some(why) = crate::UNSUPPORTED {
            let data = temp_dir("unsupported").join("3DS");
            assert_eq!(installed(&data), None);
            let error = ensure(&data).expect_err("no core");
            assert!(matches!(error, InstallError::Unsupported(_)), "{error}");
            assert_eq!(error.to_string(), why);
            assert!(!data.exists(), "nothing is written");
        }
    }

    #[test]
    fn every_library_must_be_there_to_be_installed() {
        let Some(pins) = PINS else {
            return;
        };
        let data = temp_dir("installed");
        assert_eq!(installed(&data), None);
        let parts = pins.parts(&data);
        fs::create_dir_all(&parts.core).expect("a folder in the core's place");
        assert_eq!(installed(&data), None, "a folder is not a library");
        fs::remove_dir(&parts.core).expect("remove the folder");
        write(&parts.core, CORE_BYTES);
        if let Some(moltenvk) = &parts.moltenvk {
            assert_eq!(installed(&data), None, "no MoltenVK yet");
            write(moltenvk, MOLTENVK_BYTES);
        }
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
            ..CORE
        };
        download.verify(&file).expect("the digest matches");
        assert!(file.exists());
        let error = CORE.verify(&file).expect_err("the digest differs");
        assert!(
            matches!(&error, InstallError::Digest { file, found, .. }
                if file == "download.zip" && found == sha),
            "{error}"
        );
        assert!(!file.exists(), "a download that does not match is deleted");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_zip_unpacks_whole() {
        let dir = temp_dir("zip");
        let zip = core_zip(&dir);
        let unpacked = dir.join("unpacked");
        CORE.unpack(&zip, &unpacked).expect("unpack");
        let core = fs::read(unpacked.join(CORE.library)).expect("the core");
        assert_eq!(core, CORE_BYTES);
        assert_eq!(names(&unpacked), ["README.txt", CORE.library]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_broken_zip_is_refused() {
        let dir = temp_dir("broken-zip");
        let zip = dir.join("core.zip");
        write(&zip, b"PK\x03\x04 and then nothing a zip should have");
        let error = CORE
            .unpack(&zip, &dir.join("unpacked"))
            .expect_err("not a zip");
        assert!(matches!(error, InstallError::Unpack { .. }), "{error}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: never unpacks a tar; only macOS's MoltenVK comes as one, unpacked with /usr/bin/tar"
    )]
    fn a_tar_unpacks_only_the_files_kept() {
        #[cfg(unix)]
        {
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
    }

    #[test]
    fn an_archive_without_the_library_is_refused() {
        let dir = temp_dir("missing");
        let zip_path = dir.join("core.zip");
        zip(&zip_path, &[("README", b"no core here")]);
        let unpacked = dir.join("unpacked");
        CORE.unpack(&zip_path, &unpacked).expect("unpack");
        let error = CORE
            .prepare(&unpacked, &dir.join("prepared"))
            .expect_err("no library");
        assert!(matches!(error, InstallError::Missing { .. }), "{error}");
        // A tar is refused by tar itself.
        #[cfg(unix)]
        {
            let source = dir.join("source");
            write(&source.join("README"), b"no MoltenVK here");
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
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn installs_the_core_alone_where_vulkan_is_the_system_s() {
        let dir = temp_dir("core-alone");
        let zip = core_zip(&dir);
        let zip_sha = sha256(&zip).expect("sha");
        let zip = url(&zip);
        let pins = local_pins(&zip, &zip_sha, None);
        let data = dir.join("3DS");

        let parts = pins.install(&data, &copy).expect("install");
        assert_eq!(parts, pins.parts(&data));
        assert_eq!(parts.moltenvk, None);
        assert_eq!(fs::read(&parts.core).expect("core"), CORE_BYTES);
        assert_mode(&parts.core, 0o755);
        assert_eq!(names(&data.join(CORE.folder)), [CORE.library]);
        assert_eq!(leftovers(&data), Vec::<String>::new());
        assert_eq!(pins.installed(&data), Some(parts.clone()));
        assert_eq!(pins.install(&data, &never_fetch).expect("again"), parts);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows: its second part is a tar, as MoltenVK is, which only macOS unpacks; installs_the_core_alone_where_vulkan_is_the_system_s covers Windows"
    )]
    fn installs_from_downloads_and_keeps_only_the_parts() {
        #[cfg(unix)]
        {
            let dir = temp_dir("local");
            let (zip, tar) = (core_zip(&dir), moltenvk_tar(&dir));
            let (zip_sha, tar_sha) = (sha256(&zip).expect("sha"), sha256(&tar).expect("sha"));
            let (zip, tar) = (url(&zip), url(&tar));
            let pins = local_pins(&zip, &zip_sha, Some((&tar, &tar_sha)));
            let data = dir.join("3DS");
            // An install the app quit during left this behind.
            write(&data.join(".install-1/core.zip"), b"half a download");

            let parts = pins.install(&data, &copy).expect("install");
            assert_eq!(parts, pins.parts(&data));
            let moltenvk = parts.moltenvk.clone().expect("MoltenVK");
            assert_eq!(fs::read(&parts.core).expect("core"), CORE_BYTES);
            assert_eq!(fs::read(&moltenvk).expect("MoltenVK"), MOLTENVK_BYTES);
            assert_mode(&parts.core, 0o755);
            assert_mode(&moltenvk, 0o755);
            let license = data.join("moltenvk-1.4.2/LICENSE");
            assert_eq!(fs::read(&license).expect("license"), LICENSE_BYTES);
            assert_eq!(
                names(&data.join("moltenvk-1.4.2")),
                ["LICENSE", "libMoltenVK.dylib"]
            );
            assert_eq!(names(&data.join(CORE.folder)), [CORE.library]);
            assert_eq!(leftovers(&data), Vec::<String>::new());

            // Installed, it downloads nothing more.
            assert_eq!(pins.install(&data, &never_fetch).expect("again"), parts);
            // Without its library, a part is installed again, and only it.
            fs::remove_file(&moltenvk).expect("remove");
            let core_only = Pins {
                core: Download {
                    url: "never fetched",
                    ..pins.core
                },
                ..pins
            };
            assert_eq!(core_only.install(&data, &copy).expect("reinstall"), parts);
            assert_eq!(fs::read(&moltenvk).expect("MoltenVK"), MOLTENVK_BYTES);
            fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn a_failed_install_leaves_nothing_behind() {
        let dir = temp_dir("failed");
        let zip_path = core_zip(&dir);
        let zip = url(&zip_path);
        let data = dir.join("3DS");

        let wrong = local_pins(&zip, CORE_MACOS_ARM64.sha256, None);
        let error = wrong
            .install(&data, &copy)
            .expect_err("the core does not match");
        assert!(matches!(error, InstallError::Digest { .. }), "{error}");
        assert!(!data.join(CORE.folder).exists());
        assert_eq!(leftovers(&data), Vec::<String>::new());
        assert_eq!(wrong.installed(&data), None);
        let error = wrong
            .install(&data, &|_, _| {
                Err("Could not resolve host: github.com".to_owned())
            })
            .expect_err("offline");
        assert_eq!(
            error.to_string(),
            "could not download the 3DS core: Could not resolve host: github.com"
        );

        // With MoltenVK, the core installs and MoltenVK does not.
        #[cfg(unix)]
        {
            let zip_sha = sha256(&zip_path).expect("sha");
            let tar = moltenvk_tar(&dir);
            let tar = url(&tar);
            let pins = local_pins(&zip, &zip_sha, Some((&tar, MOLTENVK.sha256)));
            let error = pins
                .install(&data, &copy)
                .expect_err("MoltenVK does not match");
            assert!(matches!(error, InstallError::Digest { .. }), "{error}");
            assert!(!data.join("moltenvk-1.4.2").exists());
            assert_eq!(leftovers(&data), Vec::<String>::new());
            assert_eq!(pins.installed(&data), None);

            let error = pins
                .install(&data, &|_, _| {
                    Err("Could not resolve host: github.com".to_owned())
                })
                .expect_err("offline");
            assert_eq!(
                error.to_string(),
                "could not download MoltenVK for the 3DS core: Could not resolve host: github.com"
            );
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_install_waits_for_the_first_and_uses_its_parts() {
        let Some(pins) = PINS else {
            return;
        };
        let data = temp_dir("lock");
        let first = lock(&data).expect("lock");
        thread::scope(|scope| {
            let second = scope.spawn(|| pins.install(&data, &never_fetch));
            thread::sleep(Duration::from_millis(100));
            assert!(!second.is_finished(), "the second install waits");
            let parts = pins.parts(&data);
            write(&parts.core, CORE_BYTES);
            if let Some(moltenvk) = &parts.moltenvk {
                write(moltenvk, MOLTENVK_BYTES);
            }
            drop(first);
            let installed = second.join().expect("no download").expect("install");
            assert_eq!(installed, parts);
        });
        fs::remove_dir_all(&data).ok();
    }

    /// Downloads every part for real: 67 MB on a Mac, 15 on Linux and 12
    /// on Windows.
    #[test]
    #[ignore = "downloads from GitHub"]
    fn installs_for_real() {
        let data = temp_dir("real");
        let started = Instant::now();
        let parts = install(&data).expect("install");
        eprintln!("installed in {:.1?}", started.elapsed());
        for library in std::iter::once(&parts.core).chain(&parts.moltenvk) {
            let bytes = fs::read(library).expect("library");
            let magic = u32::from_le_bytes(bytes[..4].try_into().expect("magic"));
            if cfg!(target_os = "macos") {
                // A Mach-O library, for one architecture or several.
                assert!(
                    matches!(magic, 0xFEED_FACF | 0xBEBA_FECA),
                    "{library:?}: {magic:#x}"
                );
            } else if cfg!(windows) {
                assert_eq!(&bytes[..2], b"MZ", "{library:?}: a Windows DLL");
            } else {
                assert_eq!(&bytes[..4], b"\x7FELF", "{library:?}: an ELF library");
            }
            assert_mode(library, 0o755);
        }
        assert_eq!(
            data.join("moltenvk-1.4.2/LICENSE").is_file(),
            cfg!(target_os = "macos")
        );
        assert_eq!(installed(&data), Some(parts.clone()));
        assert_eq!(leftovers(&data), Vec::<String>::new());

        // The core gets its fix, signed so that macOS still loads it.
        assert_eq!(ensure(&data).expect("ensure"), parts);
        #[cfg(target_os = "macos")]
        if let Some(fix) = pins().expect("pins").core.fix {
            assert!(!fix.wanted(&parts.core).expect("wanted"), "fixed");
            run("/usr/bin/codesign", &[Path::new("--verify"), &parts.core]);
        }
        assert_mode(&parts.core, 0o755);
        fs::remove_dir_all(&data).ok();
    }
}
