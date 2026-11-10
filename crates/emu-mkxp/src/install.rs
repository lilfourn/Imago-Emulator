//! Downloads mkxp-z at first use, off macOS: a pinned build, checked
//! against its pinned SHA-256.
//!
//! mkxp-z publishes no releases, only CI artifacts that expire and need a
//! GitHub login, so the builds come from Imago's own repository,
//! `lilfourn/imago-mkxp-z`. Its release holds upstream's artifacts as they
//! are, with their complete source beside them (decision 0008). This
//! downloads the artifact for this system and processor and keeps its
//! program and licence in a folder of the players' folder named for the
//! release: `mkxp-z/mkxp-z-37a04d1/mkxp-z.x86_64`, for example. A later pin
//! installs beside the earlier one.
//!
//! A game plays in its own mkxp-z if its folder has one, then in one the
//! player put in the players' folder, and only then in this one, which is
//! downloaded the first time it is needed. macOS games ship their player
//! app, so there is nothing to download there.
//!
//! An install downloads the artifact with curl into a temporary folder in
//! the players' folder, checks its size and digest, takes the program and
//! its licence out of it, and renames the folder into place, so the
//! release's folder is always complete. A lock file keeps two installs,
//! from two games opened together or the app and emu-headless, from
//! racing: the second waits for the first and then finds its build. The
//! lock, the temporary folder, the check and the rename are
//! [`emu_os::install`]'s, which the 3DS core and the randomizer install by
//! too.
//!
//! Everything here blocks.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use emu_os::install::{self as pinned, Install, Pinned};

/// Why mkxp-z could not be installed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// There is no hosted build for this system and processor.
    #[error("there is no mkxp-z build for {0} to download")]
    NoBuild(String),
    /// The download failed.
    #[error("could not download mkxp-z: {0}")]
    Download(String),
    /// The download was not the file pinned here, and was deleted.
    #[error("the download of mkxp-z, {file}, is not the file expected, so it was deleted ({why})")]
    NotPinned {
        /// The downloaded file's name.
        file: String,
        /// How it differs: its size or its SHA-256.
        why: String,
    },
    /// The download could not be unpacked, or lacked the program.
    #[error("could not unpack mkxp-z: {0}")]
    Unpack(String),
    /// A file could not be read or written.
    #[error("could not install mkxp-z: {0}")]
    Io(#[from] io::Error),
}

/// A hosted build of mkxp-z.
#[derive(Debug, Clone, Copy)]
pub struct Pin<'a> {
    /// Where it is downloaded from: a release asset of
    /// `lilfourn/imago-mkxp-z`, upstream's CI artifact as it is.
    pub url: &'a str,
    /// Its SHA-256, in lowercase hex: the digest GitHub recorded when
    /// upstream's CI uploaded it.
    pub sha256: &'a str,
    /// Its size in bytes.
    pub size: u64,
    /// The folder in the players' folder it is kept in, named for the
    /// release.
    pub folder: &'a str,
    /// The program's name in the artifact, and in that folder.
    pub program: &'a str,
}

/// The folder each pin of the release is kept in. The release is
/// `mkxp-z-37a04d1`: mkxp-z's commit `37a04d1`, built by upstream's CI run
/// 37110075912 on 2026-10-03.
const FOLDER: &str = "mkxp-z-37a04d1";

/// The build for Linux x86-64 (20.1 MB), made against Ubuntu 16.04, so for
/// glibc 2.23 or later.
const LINUX_X86_64: Pin<'static> = Pin {
    url: concat!(
        "https://github.com/lilfourn/imago-mkxp-z/releases/download/mkxp-z-37a04d1",
        "/mkxp-z.linux.ubuntu.xenial.x86_64.dev-37a04d1.zip"
    ),
    sha256: "8c700612e1d9e3f16b8d1426c2c9e1258f7bd97ba407d0aabceb87fe4e4a53f5",
    size: 20_093_482,
    folder: FOLDER,
    program: "mkxp-z.x86_64",
};

/// The build for Linux arm64 (18.9 MB), as for x86-64.
const LINUX_ARM64: Pin<'static> = Pin {
    url: concat!(
        "https://github.com/lilfourn/imago-mkxp-z/releases/download/mkxp-z-37a04d1",
        "/mkxp-z.linux.ubuntu.xenial.arm64.dev-37a04d1.zip"
    ),
    sha256: "3e0f3d6ed6486b672ed8988c180d2e6fd361622b5015cc245ce9c004ae5e3135",
    size: 18_850_554,
    folder: FOLDER,
    program: "mkxp-z.aarch64",
};

/// The build for Windows x64 (20.5 MB), cross-compiled with MinGW-w64,
/// which Windows on ARM runs as well.
const WINDOWS_X64: Pin<'static> = Pin {
    url: concat!(
        "https://github.com/lilfourn/imago-mkxp-z/releases/download/mkxp-z-37a04d1",
        "/mkxp-z.windows.cross.x86_64.dev-37a04d1.zip"
    ),
    sha256: "f9438d765f608cd99186eabf92c3be9d7b2eda0f93d83d51ca45bfe9d53c3b59",
    size: 20_497_967,
    folder: FOLDER,
    program: "mkxp-z.x86_64.exe",
};

/// This system's hosted build, if there is one.
pub const PIN: Option<Pin<'static>> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some(LINUX_X86_64)
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    Some(LINUX_ARM64)
} else if cfg!(all(
    windows,
    any(target_arch = "x86_64", target_arch = "aarch64")
)) {
    Some(WINDOWS_X64)
} else {
    None
};

/// About how many megabytes an install downloads, for the app to say so
/// before it starts: 20 on Linux x86-64 and Windows, 19 on Linux arm64.
pub const DOWNLOAD_MB: u32 = match PIN {
    Some(pin) => ((pin.size + 500_000) / 1_000_000) as u32,
    None => 0,
};

/// The licence the artifacts hold, kept beside the program.
const LICENSE: &str = "LICENSE.txt";
/// How long the download may take in all, in seconds.
const DOWNLOAD_SECONDS: u32 = 1800;
/// The most the download may be: three times the biggest build.
const MAX_DOWNLOAD: u64 = 64 << 20;

/// The hosted mkxp-z installed in `players`, the players' folder, if it is.
#[must_use]
pub fn installed(players: &Path) -> Option<PathBuf> {
    PIN.and_then(|pin| pin.installed(players))
}

/// The hosted mkxp-z in `players`, the players' folder, downloading and
/// installing it first if it is not there.
///
/// # Errors
///
/// Fails if there is no build for this system, the download fails or is
/// not the file pinned, or a file cannot be written.
pub fn ensure(players: &Path) -> Result<PathBuf, InstallError> {
    let pin = PIN.ok_or_else(|| {
        InstallError::NoBuild(format!("{} ({})", crate::SYSTEM, std::env::consts::ARCH))
    })?;
    pin.ensure(players, &curl)
}

pub use emu_os::install::Fetch;

impl Pin<'_> {
    /// Where the program is, or would be, in `players`.
    fn program_path(&self, players: &Path) -> PathBuf {
        players.join(self.folder).join(self.program)
    }

    /// The program in `players`, if it is there.
    fn installed(&self, players: &Path) -> Option<PathBuf> {
        let program = self.program_path(players);
        program.is_file().then_some(program)
    }

    /// The program in `players`, installing it first with `fetch` if it is
    /// not there, once no other install is under way.
    ///
    /// # Errors
    ///
    /// As [`ensure`].
    pub fn ensure(&self, players: &Path, fetch: Fetch<'_>) -> Result<PathBuf, InstallError> {
        if let Some(program) = self.installed(players) {
            return Ok(program);
        }
        let install = Install::begin(players)?;
        // Another install may have finished while this one waited.
        if let Some(program) = self.installed(players) {
            return Ok(program);
        }
        let temp = install.temp()?;
        self.install(players, temp.path(), fetch)
    }

    /// Downloads into `temp`, then puts the program and its licence in
    /// their folder in `players`. Nothing is unpacked from a download that
    /// is not the one pinned, in size and in SHA-256: it is deleted.
    fn install(
        &self,
        players: &Path,
        temp: &Path,
        fetch: Fetch<'_>,
    ) -> Result<PathBuf, InstallError> {
        let pinned = Pinned {
            url: self.url,
            sha256: self.sha256,
            size: Some(self.size),
        };
        let file = pinned.file_name();
        let archive = temp.join(file);
        pinned
            .download(&archive, fetch)
            .map_err(|error| match error {
                pinned::Error::Download(why) => InstallError::Download(why),
                pinned::Error::NotPinned(differs) => InstallError::NotPinned {
                    file: file.to_owned(),
                    why: differs.to_string(),
                },
                pinned::Error::Io(error) => InstallError::Io(error),
            })?;
        let unpacked = temp.join(self.folder);
        fs::create_dir(&unpacked)?;
        unpack(&archive, &unpacked, self.program)?;
        pinned::place(&unpacked, &players.join(self.folder))?;
        Ok(self.program_path(players))
    }
}

/// Takes `program` and the licence out of the zip `archive` into `into`,
/// the program able to run.
fn unpack(archive: &Path, into: &Path, program: &str) -> Result<(), InstallError> {
    let unreadable = |error: zip::result::ZipError| InstallError::Unpack(error.to_string());
    let mut zip = zip::ZipArchive::new(File::open(archive)?).map_err(unreadable)?;
    for name in [program, LICENSE] {
        let mut entry = match zip.by_name(name) {
            Ok(entry) => entry,
            // The licence is kept if the artifact has one.
            Err(zip::result::ZipError::FileNotFound) if name == LICENSE => continue,
            Err(zip::result::ZipError::FileNotFound) => {
                return Err(InstallError::Unpack(format!("the download has no {name}")));
            }
            Err(error) => return Err(unreadable(error)),
        };
        let to = into.join(name);
        io::copy(&mut entry, &mut File::create(&to)?)?;
        #[cfg(unix)]
        if name == program {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&to, fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Downloads `url` into the file `to` with curl, as [`emu_os::fetch`] does
/// a big file: from an `https://` address only, giving up on one stalled
/// for a minute and trying again twice after a passing failure.
fn curl(url: &str, to: &Path) -> Result<(), String> {
    let download = emu_os::fetch::Download {
        url,
        allow: emu_os::fetch::Allow::or_loopback_if(cfg!(test)),
        seconds: DOWNLOAD_SECONDS,
        max_bytes: MAX_DOWNLOAD,
        patient: true,
    };
    download.to_file(to).map_err(|error| match error {
        emu_os::fetch::Error::NotStarted(error) => match error.kind() {
            io::ErrorKind::NotFound => "curl, which downloads it, is not installed".to_owned(),
            _ => format!("could not run curl: {error}"),
        },
        error => error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Write;

    use super::*;

    const PROGRAM: &[u8] = b"not really mkxp-z";

    /// An artifact as upstream's CI makes one: the program, its licence,
    /// its settings and Ruby's library.
    fn artifact(program: &str) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in [
            ("LICENSE.txt", &b"GNU GENERAL PUBLIC LICENSE"[..]),
            ("mkxp.json", b"{}"),
            (program, PROGRAM),
            ("stdlib/socket.rb", b"# socket"),
        ] {
            zip.start_file(name, stored).expect("entry");
            zip.write_all(bytes).expect("written");
        }
        zip.finish().expect("zip").into_inner()
    }

    fn digest(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        format!("{:x}", sha2::Sha256::digest(bytes))
    }

    #[test]
    fn the_program_and_its_licence_are_installed_once() {
        let dir = crate::tests::temp_dir("install");
        let players = dir.join("mkxp-z");
        let bytes = artifact("mkxp-z.x86_64");
        let sha256 = digest(&bytes);
        let pin = Pin {
            url: "https://example.com/release/mkxp-z.linux.zip",
            sha256: &sha256,
            size: bytes.len() as u64,
            folder: "mkxp-z-test",
            program: "mkxp-z.x86_64",
        };
        let fetched = Cell::new(0);
        let fetch = |url: &str, to: &Path| {
            assert_eq!(url, pin.url);
            fetched.set(fetched.get() + 1);
            fs::write(to, &bytes).map_err(|error| error.to_string())
        };
        assert_eq!(pin.installed(&players), None);
        // An install cut short is cleared.
        fs::create_dir_all(players.join(".install-1")).expect("leftover");
        let program = pin.ensure(&players, &fetch).expect("installed");
        assert_eq!(program, players.join("mkxp-z-test/mkxp-z.x86_64"));
        assert_eq!(fs::read(&program).expect("program"), PROGRAM);
        assert!(players.join("mkxp-z-test/LICENSE.txt").is_file());
        assert!(
            !players.join("mkxp-z-test/stdlib").exists(),
            "only what plays"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&program)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "it runs");
        }
        assert!(!players.join(".install-1").exists());
        assert_eq!(pin.ensure(&players, &fetch).expect("there"), program);
        assert_eq!(fetched.get(), 1, "downloaded once");
        let leftovers: Vec<_> = fs::read_dir(&players)
            .expect("players")
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(leftovers.len(), 2, "the build and the lock: {leftovers:?}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_download_that_is_not_the_one_pinned_installs_nothing() {
        let dir = crate::tests::temp_dir("install-wrong");
        let players = dir.join("mkxp-z");
        let bytes = artifact("mkxp-z.x86_64");
        let sha256 = digest(&bytes);
        let pin = Pin {
            url: "https://example.com/mkxp-z.zip",
            sha256: &sha256,
            size: bytes.len() as u64,
            folder: "mkxp-z-test",
            program: "mkxp-z.x86_64",
        };
        let mut other = bytes.clone();
        *other.last_mut().expect("bytes") ^= 1;
        for (download, says) in [(other, "its SHA-256 is"), (b"short".to_vec(), "bytes, not")] {
            let fetch = |_: &str, to: &Path| fs::write(to, &download).map_err(|e| e.to_string());
            let error = pin.ensure(&players, &fetch).expect_err("refused");
            assert!(error.to_string().contains(says), "{error}");
            assert_eq!(pin.installed(&players), None);
        }
        let failing =
            |_: &str, _: &Path| Err("the server answered with HTTP status 404".to_owned());
        let error = pin.ensure(&players, &failing).expect_err("failed");
        assert_eq!(
            error.to_string(),
            "could not download mkxp-z: the server answered with HTTP status 404"
        );
        let wrong_program = Pin {
            program: "mkxp-z.aarch64",
            ..pin
        };
        let fetch = |_: &str, to: &Path| fs::write(to, &bytes).map_err(|e| e.to_string());
        let error = wrong_program
            .ensure(&players, &fetch)
            .expect_err("no program");
        assert_eq!(
            error.to_string(),
            "could not unpack mkxp-z: the download has no mkxp-z.aarch64"
        );
        assert_eq!(
            fs::read_dir(&players).expect("players").count(),
            1,
            "the lock only"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_install_lock_is_opened_as_it_is_not_emptied() {
        let dir = crate::tests::temp_dir("install-lock");
        let players = dir.join("mkxp-z");
        fs::create_dir_all(&players).expect("players");
        // Windows may refuse to empty a file another process holds locked,
        // so an install must not: what is in the file shows whether it did.
        fs::write(players.join("install.lock"), b"held by another install").expect("lock");
        let bytes = artifact("mkxp-z.x86_64");
        let sha256 = digest(&bytes);
        let pin = Pin {
            url: "https://example.com/mkxp-z.zip",
            sha256: &sha256,
            size: bytes.len() as u64,
            folder: "mkxp-z-test",
            program: "mkxp-z.x86_64",
        };
        let fetch = |_: &str, to: &Path| fs::write(to, &bytes).map_err(|e| e.to_string());
        pin.ensure(&players, &fetch).expect("installed");
        assert_eq!(
            fs::read(players.join("install.lock")).expect("lock"),
            b"held by another install"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_download_names_the_app_as_every_download_does() {
        use std::io::Read as _;
        use std::net::TcpListener;

        // A stand-in server on this computer that answers with the user
        // agent it was sent.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        let url = format!(
            "http://{}/mkxp-z.zip",
            listener.local_addr().expect("address")
        );
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut head = Vec::new();
                let mut chunk = [0; 1024];
                while !head.windows(4).any(|end| end == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(read) if read > 0 => head.extend_from_slice(&chunk[..read]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let agent = head
                    .lines()
                    .find_map(|line| line.strip_prefix("User-Agent: "))
                    .unwrap_or_default();
                let answer = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{agent}",
                    agent.len()
                );
                stream.write_all(answer.as_bytes()).ok();
            }
        });
        let dir = crate::tests::temp_dir("install-agent");
        let to = dir.join("mkxp-z.zip");
        curl(&url, &to).expect("downloaded");
        assert_eq!(
            fs::read_to_string(&to).expect("downloaded"),
            format!(
                "ImagoEmulator/{} (+https://imagoemulator.com)",
                env!("CARGO_PKG_VERSION")
            )
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn each_system_has_the_build_for_it() {
        let expected = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(("mkxp-z.x86_64", 20))
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(("mkxp-z.aarch64", 19))
        } else if cfg!(windows) {
            Some(("mkxp-z.x86_64.exe", 20))
        } else {
            None
        };
        assert_eq!(PIN.map(|pin| (pin.program, DOWNLOAD_MB)), expected);
        for pin in [LINUX_X86_64, LINUX_ARM64, WINDOWS_X64] {
            let release =
                "https://github.com/lilfourn/imago-mkxp-z/releases/download/mkxp-z-37a04d1/";
            assert!(pin.url.starts_with(release), "{}", pin.url);
            assert!(pin.url.ends_with(".dev-37a04d1.zip"), "{}", pin.url);
            assert_eq!(pin.sha256.len(), 64);
            assert_eq!(pin.folder, "mkxp-z-37a04d1");
        }
    }

    /// Downloads and checks this system's real build. Run with
    /// `cargo test -p emu-mkxp -- --ignored downloads_for_real`.
    #[test]
    #[cfg(not(target_os = "macos"))]
    #[ignore = "downloads about 20 MB from GitHub"]
    fn downloads_for_real() {
        let dir = crate::tests::temp_dir("install-real");
        let Some(pin) = PIN else {
            return;
        };
        let program = ensure(&dir).expect("installed");
        assert_eq!(program, dir.join(pin.folder).join(pin.program));
        assert!(crate::mkxp_z_in(program.parent().expect("folder")).is_some());
        fs::remove_dir_all(&dir).ok();
    }
}
