//! Installing an update on the Mac, as Sparkle does: the release's disk
//! image is downloaded and checked against the feed's SHA-256, its app is
//! copied out and checked, and on Restart to Update it takes the running
//! app's place, to be opened once this one has quit.
//!
//! The app copied out must be signed by the same Developer ID team as the
//! running app, under its identifier, must pass Gatekeeper (it is
//! notarized), and must be the version the feed names, so a feed can't
//! put back an older release. A running app that isn't a signed release
//! (an ad hoc build, as `install-local.sh` makes), that macOS runs from a
//! copy it moved aside (App Translocation), or that sits in a folder this
//! user can't write to, isn't replaced: the pill opens the download page.
//!
//! Everything here blocks.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use emu_os::fetch::{Allow, Download};
use emu_os::install::{Install, Pinned};

use super::{Package, Version};

/// How long the disk image may take to come.
const DOWNLOAD_SECONDS: u32 = 30 * 60;
/// How often the download is measured, for the pill's percentage.
const MEASURE_EVERY: Duration = Duration::from_millis(250);

/// The running app, which an update replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The app's bundle.
    bundle: PathBuf,
    /// Its identifier and its Developer ID team, as signed.
    identifier: String,
    team: String,
}

impl Target {
    /// The running app, if an update can replace it.
    ///
    /// # Errors
    ///
    /// Why it can't be: it isn't in an app bundle, isn't signed with a
    /// Developer ID, is translocated, or its folder can't be written to.
    pub fn find() -> Result<Self, String> {
        let exe = std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|error| format!("the app can't find itself: {error}"))?;
        let bundle = bundle_of(&exe).ok_or("the app isn't in an app bundle")?;
        if bundle.to_string_lossy().contains("/AppTranslocation/") {
            return Err("macOS runs the app from a copy; move it to Applications first".to_owned());
        }
        let output = run(Command::new("/usr/bin/codesign")
            .args(["--display", "--verbose=2"])
            .arg(&bundle))?;
        let (identifier, team) = signature(&String::from_utf8_lossy(&output.stderr))
            .ok_or("the app isn't signed with a Developer ID")?;
        let folder = bundle.parent().ok_or("the app has no folder")?;
        writable(folder).map_err(|error| {
            format!(
                "the app's folder, {}, can't be written to: {error}",
                folder.display()
            )
        })?;
        Ok(Self {
            bundle,
            identifier,
            team,
        })
    }

    /// Downloads `package`, the disk image of `version`, from where
    /// `allow` lets it come, and copies its app out and checks it,
    /// telling `percent` how much has come. Returns where the app waits.
    ///
    /// # Errors
    ///
    /// Why it isn't ready: it didn't come, isn't the file the feed pinned,
    /// or its app isn't one to install.
    pub fn prepare(
        &self,
        package: &Package,
        version: Version,
        allow: Allow,
        percent: &(dyn Fn(u8) + Sync),
    ) -> Result<PathBuf, String> {
        let folder = updates_folder()?;
        let install = Install::begin(&folder)
            .map_err(|error| format!("{} can't be used: {error}", folder.display()))?;
        let temp = install
            .temp()
            .map_err(|error| format!("{} can't be used: {error}", folder.display()))?;
        let image = temp.path().join("update.dmg");
        let pinned = Pinned {
            url: &package.url,
            sha256: &package.sha256,
            size: Some(package.size),
        };
        let fetch = |url: &str, to: &Path| {
            download(url, to, package.size, allow, percent).map_err(|error| error.to_string())
        };
        pinned
            .download(&image, &fetch)
            .map_err(|error| match error {
                emu_os::install::Error::Download(why) => format!("the download failed: {why}"),
                emu_os::install::Error::NotPinned(how) => {
                    eprintln!("ds-emulator: the update's download differs from the feed's: {how}");
                    "the download isn't the file that was published".to_owned()
                }
                emu_os::install::Error::Io(error) => format!("the download can't be read: {error}"),
            })?;
        percent(100);

        let mount = temp.path().join("mount");
        fs::create_dir(&mount).map_err(|error| error.to_string())?;
        run(Command::new("/usr/bin/hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-quiet"])
            .arg("-mountpoint")
            .arg(&mount)
            .arg(&image))?;
        let copied = copy_app_out(&mount, &folder);
        // Detached whatever the copy did; forced if something holds it.
        let detach = |force: bool| {
            let mut command = Command::new("/usr/bin/hdiutil");
            command.args(["detach", "-quiet"]);
            if force {
                command.arg("-force");
            }
            run(command.arg(&mount)).is_ok()
        };
        if !detach(false) {
            detach(true);
        }
        let staged = copied?;
        if let Err(why) = self.check(&staged, version) {
            fs::remove_dir_all(staged.parent().unwrap_or(&staged)).ok();
            return Err(why);
        }
        Ok(staged)
    }

    /// Checks that `app` is this app, signed by its team, accepted by
    /// Gatekeeper, and of `version`.
    fn check(&self, app: &Path, version: Version) -> Result<(), String> {
        let requirement = format!(
            "=anchor apple generic and identifier \"{}\" and certificate leaf[subject.OU] = \"{}\"",
            self.identifier, self.team
        );
        run(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(format!("-R{requirement}"))
            .arg(app))
        .map_err(|why| format!("the new app isn't signed as this one is ({why})"))?;
        run(Command::new("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute"])
            .arg(app))
        .map_err(|why| format!("Gatekeeper doesn't accept the new app ({why})"))?;
        let output = run(Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
            .arg(app.join("Contents/Info.plist")))?;
        let found = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if found != version.to_string() {
            return Err(format!("the new app is version {found}, not {version}"));
        }
        Ok(())
    }

    /// Puts the app at `staged` in this one's place, and has it opened
    /// once this process has ended. The app this one was is removed then
    /// too; until then it waits beside the new one.
    ///
    /// # Errors
    ///
    /// Why it couldn't be put in place; the running app is then where it
    /// was.
    pub fn install_and_relaunch(&self, staged: &Path) -> Result<(), String> {
        let previous = self.swap(staged)?;
        relaunch(&self.bundle, &previous)
            .map_err(|error| format!("the new app is installed but couldn't be opened: {error}"))
    }

    /// Puts the app at `staged` in this one's place, and returns where this
    /// one waits, beside it, to be removed.
    fn swap(&self, staged: &Path) -> Result<PathBuf, String> {
        let (incoming, previous) = beside(&self.bundle)?;
        for path in [&incoming, &previous] {
            remove(path)
                .map_err(|error| format!("{} can't be removed: {error}", path.display()))?;
        }
        // A rename if the cache is on the app's volume; else a copy.
        if fs::rename(staged, &incoming).is_err() {
            run(Command::new("/usr/bin/ditto").arg(staged).arg(&incoming))
                .map_err(|why| format!("the new app can't be put beside this one: {why}"))?;
        }
        if let Err(error) = fs::rename(&self.bundle, &previous) {
            remove(&incoming).ok();
            return Err(format!("this app can't be moved aside: {error}"));
        }
        if let Err(error) = fs::rename(&incoming, &self.bundle) {
            fs::rename(&previous, &self.bundle).ok();
            remove(&incoming).ok();
            return Err(format!("the new app can't be put in place: {error}"));
        }
        Ok(previous)
    }
}

/// Removes what an update left behind: an app downloaded and never
/// installed, and an app replaced that wasn't removed.
pub fn tidy() {
    if let Ok(folder) = updates_folder() {
        remove(&folder.join("ready")).ok();
    }
    if let Ok(target) = Target::find()
        && let Ok((incoming, previous)) = beside(&target.bundle)
    {
        remove(&incoming).ok();
        remove(&previous).ok();
    }
}

/// The app bundle the executable `exe` is in: `X.app/Contents/MacOS/exe`.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let contents = exe.parent()?.parent()?;
    let bundle = contents.parent()?;
    (exe.parent()?.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_owned())
}

/// The identifier and Developer ID team `codesign --display --verbose=2`
/// printed, if it names both: an ad hoc signature names no team.
fn signature(printed: &str) -> Option<(String, String)> {
    let field = |name: &str| {
        printed
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    let identifier = field("Identifier=")?;
    let team = field("TeamIdentifier=")?;
    let plain = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    };
    (plain(identifier) && team.len() == 10 && team.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .then(|| (identifier.to_owned(), team.to_owned()))
}

/// Where updates are downloaded and wait: `~/Library/Caches/ds-emulator/Update`.
fn updates_folder() -> Result<PathBuf, String> {
    emu_os::cache_dir()
        .map(|cache| cache.join("ds-emulator").join("Update"))
        .ok_or_else(|| "there is no home folder".to_owned())
}

/// The names beside `bundle` that a new app comes in under, and the old
/// one waits under, during an update: hidden, on the same volume.
fn beside(bundle: &Path) -> Result<(PathBuf, PathBuf), String> {
    let folder = bundle.parent().ok_or("the app has no folder")?;
    let name = bundle
        .file_name()
        .map(OsStr::to_string_lossy)
        .ok_or("the app has no name")?;
    Ok((
        folder.join(format!(".{name}.incoming")),
        folder.join(format!(".{name}.previous")),
    ))
}

/// Copies the one app on the disk image mounted at `mount` into a fresh
/// `ready` folder in `folder`, and returns where it went.
fn copy_app_out(mount: &Path, folder: &Path) -> Result<PathBuf, String> {
    let mut apps = fs::read_dir(mount)
        .map_err(|error| format!("the disk image can't be read: {error}"))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension() == Some(OsStr::new("app")) && path.is_dir());
    let (Some(app), None) = (apps.next(), apps.next()) else {
        return Err("the disk image doesn't hold one app".to_owned());
    };
    let ready = folder.join("ready");
    remove(&ready).map_err(|error| error.to_string())?;
    fs::create_dir_all(&ready).map_err(|error| error.to_string())?;
    let staged = ready.join(app.file_name().ok_or("the app has no name")?);
    // ditto keeps the signature, extended attributes and links as they are.
    run(Command::new("/usr/bin/ditto").arg(&app).arg(&staged))
        .map_err(|why| format!("the app can't be copied off the disk image: {why}"))?;
    Ok(staged)
}

/// Downloads `url` into `to`, at most `size` bytes, telling `percent` how
/// much of it has come.
fn download(
    url: &str,
    to: &Path,
    size: u64,
    allow: Allow,
    percent: &(dyn Fn(u8) + Sync),
) -> Result<(), emu_os::fetch::Error> {
    let download = Download {
        url,
        allow,
        seconds: DOWNLOAD_SECONDS,
        max_bytes: size,
        patient: true,
    };
    thread::scope(|scope| {
        let downloading = scope.spawn(|| download.to_file(to));
        while !downloading.is_finished() {
            let have = fs::metadata(to).map_or(0, |file| file.len());
            // Never 100 until it has all come and been checked.
            percent(u8::try_from((have.saturating_mul(100) / size.max(1)).min(99)).unwrap_or(99));
            thread::sleep(MEASURE_EVERY);
        }
        downloading.join().unwrap_or_else(|_| {
            Err(emu_os::fetch::Error::Failed(
                "the download stopped".to_owned(),
            ))
        })
    })
}

/// Opens `bundle` once this process has ended, and removes `previous`
/// first: from a shell of its own, which outlives the app. A test home
/// ([`emu_os::HOME_VARIABLE`]) goes with it: `open` passes the new app
/// nothing of this one's environment otherwise.
fn relaunch(bundle: &Path, previous: &Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt as _;

    const SCRIPT: &str = r#"while /bin/kill -0 "$1" 2>/dev/null; do /bin/sleep 0.2; done
/bin/rm -rf "$2"
shift 2
exec /usr/bin/open "$@""#;
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", SCRIPT, "sh"])
        .arg(std::process::id().to_string())
        .arg(previous);
    if let Some(home) = std::env::var_os(emu_os::HOME_VARIABLE).filter(|home| !home.is_empty()) {
        let mut variable = std::ffi::OsString::from(format!("{}=", emu_os::HOME_VARIABLE));
        variable.push(home);
        command.arg("--env").arg(variable);
    }
    let mut child = command
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group, so nothing sent to the app's (a
        // terminal's hangup) reaches it.
        .process_group(0)
        .spawn()?;
    // Reaped if the app is still running when it ends; it won't be.
    thread::spawn(move || child.wait());
    Ok(())
}

/// Whether `folder` can be written to, by making and removing a folder in it.
fn writable(folder: &Path) -> io::Result<()> {
    let probe = folder.join(format!(".ds-emulator-update-{}", std::process::id()));
    fs::create_dir(&probe)?;
    fs::remove_dir(&probe)
}

/// Removes the file or folder at `path`, if there is one.
fn remove(path: &Path) -> io::Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
    };
    match removed {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Runs `command` to its end, and returns what it printed if it succeeded.
fn run(command: &mut Command) -> Result<Output, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| emu_os::process::why_not_started(&program, &error))?;
    if output.status.success() {
        Ok(output)
    } else {
        let said = String::from_utf8_lossy(&output.stderr);
        let said = said.trim();
        let name = Path::new(&program)
            .file_name()
            .map_or(program.clone(), |name| name.to_string_lossy().into_owned());
        Err(if said.is_empty() {
            format!("{name} failed ({})", output.status)
        } else {
            format!("{name}: {said}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_found_from_its_executable() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Imago Emulator.app/Contents/MacOS/ds-emulator"
            )),
            Some(PathBuf::from("/Applications/Imago Emulator.app"))
        );
        for loose in [
            "/Users/me/target/release/ds-emulator",
            "/Applications/Imago Emulator/Contents/MacOS/ds-emulator",
            "/Applications/Imago Emulator.app/Contents/Resources/ds-emulator",
        ] {
            assert_eq!(bundle_of(Path::new(loose)), None, "{loose}");
        }
    }

    #[test]
    fn only_a_developer_id_signature_names_a_team() {
        let release = "Executable=/Applications/Imago Emulator.app/Contents/MacOS/ds-emulator\n\
            Identifier=com.lilfourn.ds-emulator\nFormat=app bundle with Mach-O thin (arm64)\n\
            Authority=Developer ID Application: Someone (AB12CD34EF)\nTeamIdentifier=AB12CD34EF\n";
        assert_eq!(
            signature(release),
            Some((
                "com.lilfourn.ds-emulator".to_owned(),
                "AB12CD34EF".to_owned()
            ))
        );
        let ad_hoc =
            "Identifier=com.lilfourn.ds-emulator\nSignature=adhoc\nTeamIdentifier=not set\n";
        assert_eq!(signature(ad_hoc), None);
        // Nothing that could change the requirement it goes into.
        let quoted = "Identifier=x\" or anchor trusted\nTeamIdentifier=AB12CD34EF\n";
        assert_eq!(signature(quoted), None);
        assert_eq!(signature("Identifier=x\n"), None);
    }

    #[test]
    fn the_new_app_comes_in_and_the_old_one_waits_beside_it() {
        let (incoming, previous) =
            beside(Path::new("/Applications/Imago Emulator.app")).expect("names");
        assert_eq!(
            incoming,
            PathBuf::from("/Applications/.Imago Emulator.app.incoming")
        );
        assert_eq!(
            previous,
            PathBuf::from("/Applications/.Imago Emulator.app.previous")
        );
    }

    /// A folder of its own for `test`, empty.
    fn scratch(test: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ds-emulator-update-{test}-{}", std::process::id()));
        remove(&root).ok();
        root
    }

    /// A folder standing in for an app of `version`.
    fn fake_app(at: &Path, version: &str) {
        fs::create_dir_all(at.join("Contents")).expect("folders");
        fs::write(at.join("Contents/version"), version).expect("file");
    }

    fn version_of(app: &Path) -> String {
        fs::read_to_string(app.join("Contents/version")).expect("version")
    }

    fn target(bundle: &Path) -> Target {
        Target {
            bundle: bundle.to_owned(),
            identifier: "com.lilfourn.ds-emulator".to_owned(),
            team: "AB12CD34EF".to_owned(),
        }
    }

    #[test]
    fn the_new_app_takes_the_old_ones_place_and_the_old_one_waits_beside_it() {
        let root = scratch("swap");
        let bundle = root.join("Apps/Imago Emulator.app");
        let staged = root.join("Cache/ready/Imago Emulator.app");
        fake_app(&bundle, "old");
        fake_app(&staged, "new");
        // What an update that stopped halfway left beside it goes first.
        let (incoming, _) = beside(&bundle).expect("names");
        fake_app(&incoming, "half");
        let previous = target(&bundle).swap(&staged).expect("swapped");
        assert_eq!(version_of(&bundle), "new");
        assert_eq!(version_of(&previous), "old");
        assert!(!staged.exists() && !incoming.exists());
        remove(&root).ok();
    }

    #[test]
    fn an_app_that_cant_be_moved_aside_stays_where_it_was() {
        let root = scratch("stays");
        let bundle = root.join("Apps/Imago Emulator.app");
        let staged = root.join("Cache/ready/Imago Emulator.app");
        fake_app(&staged, "new");
        // No app where the running one should be: moving it aside fails.
        fs::create_dir_all(root.join("Apps")).expect("folder");
        assert!(target(&bundle).swap(&staged).is_err());
        let (incoming, previous) = beside(&bundle).expect("names");
        assert!(!bundle.exists() && !incoming.exists() && !previous.exists());
        remove(&root).ok();
    }
}
