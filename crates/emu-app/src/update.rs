//! Updates, as T3 Code and Sparkle do them: the app asks imagoemulator.com
//! which version is newest, a pill in the library's header says when there
//! is a newer one, and on the Mac it downloads and installs it in place.
//!
//! - **The feed** is `update.json` beside the site's page
//!   ([`FEED`]), written by `site/publish.sh` when a version is published:
//!   the version, the download page, and each package's address, SHA-256
//!   and size.
//! - **When it is asked**: a few seconds after launch and every six hours
//!   while the app runs, unless Settings > Updates is off; and whenever the
//!   player chooses Check for Updates…. Builds made for debugging, test
//!   homes ([`emu_os::HOME_VARIABLE`]) and [`OFF_VARIABLE`] ask only when
//!   told to.
//! - **The pill** goes from Update Available to Downloading and then
//!   Restart to Update. Update Available can be dismissed until the next
//!   launch. Nothing is downloaded or installed until the player presses
//!   it, and nothing installs on quit.
//! - **On the Mac** the disk image is downloaded, checked against the
//!   feed's SHA-256, and its app copied out and checked: signed by the same
//!   Developer ID team as the running app, notarized, and the version the
//!   feed names (`update/mac.rs`). Restart to Update swaps it in for the
//!   running app and opens it once this one has quit. An app that isn't a
//!   signed release, or can't be replaced where it is, gets the download
//!   page instead.
//! - **On Windows and Linux** the pill opens the download page: the
//!   packages aren't signed yet, and a `.deb` needs the system's own
//!   installer.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use emu_os::fetch::{Allow, Download};
use gpui::{App, AppContext as _, Context, Entity, Global, PromptButton, PromptLevel, Task};
use serde::Deserialize;

use crate::settings::{self, Settings};

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as install;

/// Where the app asks which version is newest.
pub const FEED: &str = "https://imagoemulator.com/update.json";
/// Asks another feed instead: an `https://` address, or for a test a
/// stand-in server's `http://127.0.0.1:` one. Automatic checks run in any
/// build while it is set.
pub const FEED_VARIABLE: &str = "DS_EMULATOR_UPDATE_FEED";
/// Set to anything, turns automatic checks off, as T3 Code's
/// `T3CODE_DISABLE_AUTO_UPDATE` does.
pub const OFF_VARIABLE: &str = "DS_EMULATOR_DISABLE_AUTO_UPDATE";

/// How long after launch the first check waits, so it never slows the
/// window's first frames.
const FIRST_CHECK: Duration = Duration::from_secs(8);
/// How often the app checks while it runs.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// The most the feed may be, and how long asking for it may take.
const FEED_MAX_BYTES: u64 = 64 * 1024;
const FEED_SECONDS: u32 = 20;
/// The most a package may be: the disk image is about 10 MB.
const PACKAGE_MAX_BYTES: u64 = 512 << 20;

/// This system's package in the feed, if the app installs updates here.
const PACKAGE: Option<&str> = if cfg!(target_os = "macos") {
    Some("mac")
} else {
    None
};

/// A version, as `MAJOR.MINOR.PATCH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([u64; 3]);

impl Version {
    /// The version `text` names, if it is three numbers between dots.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut numbers = [0; 3];
        for number in &mut numbers {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *number = part.parse().ok()?;
        }
        parts.next().is_none().then_some(Self(numbers))
    }

    /// This app's version.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION"))
            .expect("the workspace's version is MAJOR.MINOR.PATCH")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch] = self.0;
        write!(f, "{major}.{minor}.{patch}")
    }
}

/// A file a release is installed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Where it is downloaded from.
    pub url: String,
    /// Its SHA-256, in lowercase hex.
    pub sha256: String,
    /// Its size in bytes.
    pub size: u64,
}

/// The newest version, as the feed tells of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version.
    pub version: Version,
    /// The page its downloads are on.
    pub page: String,
    /// What this system installs it from, if the app installs it here.
    pub package: Option<Package>,
}

#[derive(Deserialize)]
struct FeedFile {
    version: String,
    page: String,
    #[serde(default)]
    downloads: HashMap<String, PackageFile>,
}

#[derive(Deserialize)]
struct PackageFile {
    url: String,
    sha256: String,
    size: u64,
}

/// The release the feed `bytes` tells of, with its package `package` (as
/// the feed names this system's), taking addresses `allow` lets the app
/// fetch.
///
/// # Errors
///
/// Why the feed isn't one the app can use: not the feed's JSON, a version
/// that isn't `MAJOR.MINOR.PATCH`, or a page or package that isn't to be
/// had safely.
pub fn parse_feed(bytes: &[u8], package: Option<&str>, allow: Allow) -> Result<Release, String> {
    let feed: FeedFile = serde_json::from_slice(bytes)
        .map_err(|error| format!("the feed is not readable: {error}"))?;
    let version = Version::parse(&feed.version)
        .ok_or_else(|| format!("the feed names no version: {:?}", feed.version))?;
    if !allow.takes(&feed.page) {
        return Err(format!(
            "the feed's page is not a web address: {:?}",
            feed.page
        ));
    }
    let package = match package.and_then(|name| feed.downloads.get(name)) {
        None => None,
        Some(file) => {
            let hex = file.sha256.len() == 64
                && file
                    .sha256
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
            if !allow.takes(&file.url) || !hex || file.size == 0 || file.size > PACKAGE_MAX_BYTES {
                return Err(format!(
                    "the feed's package is not one to install: {}",
                    file.url
                ));
            }
            Some(Package {
                url: file.url.clone(),
                sha256: file.sha256.clone(),
                size: file.size,
            })
        }
    };
    Ok(Release {
        version,
        page: feed.page,
        package,
    })
}

/// Where updates stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Nothing newer is known.
    Idle,
    /// A newer release; `installable` if the app can install it in place.
    Available {
        /// The release.
        release: Release,
        /// Whether pressing the pill installs it, rather than opening its
        /// page.
        installable: bool,
    },
    /// It is being downloaded and checked: `percent` of it has come.
    Downloading {
        /// The release.
        release: Release,
        /// How much of it has come.
        percent: u8,
    },
    /// It is checked and waiting, at `staged`, to replace the app.
    Ready {
        /// The release.
        release: Release,
        /// The new app.
        staged: PathBuf,
    },
    /// Installing it failed, for this reason: the pill opens its page.
    Failed {
        /// The release.
        release: Release,
        /// Why.
        why: String,
    },
}

/// What the library's header shows for updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pill {
    /// Its words.
    pub label: String,
    /// Whether pressing it does nothing yet: while downloading.
    pub busy: bool,
    /// Whether it can be dismissed until the next launch.
    pub dismissable: bool,
}

/// The app's updates, as an entity the library watches.
pub struct Updater {
    state: State,
    /// Whether the player dismissed the pill until the next launch.
    dismissed: bool,
    /// The feed asked, and where it and its packages may come from.
    feed: String,
    allow: Allow,
    /// Whether the check under way was asked for by the player, who is
    /// told what it found.
    manual: bool,
    checking: Option<Task<()>>,
    downloading: Option<Task<()>>,
    schedule: Option<Task<()>>,
}

/// The app's one [`Updater`].
struct Shared(Entity<Updater>);

impl Global for Shared {}

/// What the thread downloading a release says.
enum Progress {
    Percent(u8),
    Done(Result<PathBuf, String>),
}

impl Updater {
    /// The app's updates, made on first use. Nothing is asked until
    /// [`start`] or [`Updater::check`].
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(shared) = cx.try_global::<Shared>() {
            return shared.0.clone();
        }
        let (feed, allow) = match std::env::var(FEED_VARIABLE) {
            Ok(feed) if !feed.is_empty() => (feed, Allow::HttpsOrLoopback),
            _ => (FEED.to_owned(), Allow::Https),
        };
        let entity = cx.new(|_| Self {
            state: State::Idle,
            dismissed: false,
            feed,
            allow,
            manual: false,
            checking: None,
            downloading: None,
            schedule: None,
        });
        cx.set_global(Shared(entity.clone()));
        entity
    }

    /// What the library's header shows, if anything.
    pub fn pill(&self) -> Option<Pill> {
        pill(&self.state, self.dismissed)
    }

    /// Hides the pill until the next launch, if it may be hidden.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.pill().is_some_and(|pill| pill.dismissable) {
            self.dismissed = true;
            cx.notify();
        }
    }

    /// Asks the feed for the newest version. A `manual` check, from Check
    /// for Updates…, says what it found in a dialog; an automatic one only
    /// changes the pill.
    pub fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        // One asked for while an automatic check runs is answered by it.
        self.manual |= manual;
        if self.checking.is_some() {
            return;
        }
        let feed = self.feed.clone();
        let allow = self.allow;
        let asking = cx.background_executor().spawn(async move {
            let bytes = Download {
                url: &feed,
                allow,
                seconds: FEED_SECONDS,
                max_bytes: FEED_MAX_BYTES,
                patient: false,
            }
            .bytes()
            .map_err(|error| format!("couldn't reach imagoemulator.com: {error}"))?;
            let release = parse_feed(&bytes, PACKAGE, allow)?;
            let installable = release.package.is_some() && can_install();
            Ok::<_, String>((release, installable))
        });
        self.checking = Some(cx.spawn(async move |this, cx| {
            let found = asking.await;
            this.update(cx, |this, cx| {
                this.checking = None;
                let manual = std::mem::take(&mut this.manual);
                this.found(found, manual, cx);
            })
            .ok();
        }));
    }

    /// Takes in what a check found.
    fn found(
        &mut self,
        found: Result<(Release, bool), String>,
        manual: bool,
        cx: &mut Context<Self>,
    ) {
        let current = Version::current();
        match found {
            Err(why) => {
                eprintln!("ds-emulator: checking for updates: {why}");
                if manual {
                    tell(
                        cx,
                        "Couldn't check for updates",
                        format!("{}.", sentence(&why)),
                    );
                }
            }
            Ok((release, _)) if release.version <= current => {
                // A release taken back is no longer offered.
                if matches!(self.state, State::Available { .. } | State::Failed { .. }) {
                    self.state = State::Idle;
                }
                if manual {
                    tell(
                        cx,
                        "Imago Emulator is up to date",
                        format!("Version {current} is the newest."),
                    );
                }
            }
            Ok((release, installable)) => {
                let busy_with_it = match &self.state {
                    State::Downloading { release: busy, .. }
                    | State::Ready { release: busy, .. } => busy.version >= release.version,
                    _ => false,
                };
                if !busy_with_it {
                    self.state = State::Available {
                        release: release.clone(),
                        installable,
                    };
                }
                if manual {
                    self.dismissed = false;
                    self.offer(current, cx);
                }
            }
        }
        cx.notify();
    }

    /// Says, in a dialog, where the update stands, and asks whether to go
    /// on with it, as the pill would.
    fn offer(&mut self, current: Version, cx: &mut Context<Self>) {
        let (message, detail, button) = match &self.state {
            State::Available {
                release,
                installable,
            } => (
                format!("Imago Emulator {} is available", release.version),
                format!("You have version {current}."),
                if *installable { "Update" } else { "Download" },
            ),
            State::Downloading { release, .. } => {
                tell(
                    cx,
                    &format!("Imago Emulator {} is downloading", release.version),
                    "Restart to Update, in the library, installs it once it has come.",
                );
                return;
            }
            State::Ready { release, .. } => (
                format!("Imago Emulator {} is ready to install", release.version),
                "The app restarts to install it.".to_owned(),
                "Restart to Update",
            ),
            State::Idle | State::Failed { .. } => return,
        };
        let Some(window) = cx
            .active_window()
            .or_else(|| cx.windows().into_iter().next())
        else {
            return;
        };
        let answer = window
            .update(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Info,
                    &message,
                    Some(&detail),
                    &[button, "Later"],
                    cx,
                )
            })
            .ok();
        let Some(answer) = answer else {
            return;
        };
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| this.press(cx)).ok();
            }
        })
        .detach();
    }

    /// The pill was pressed: download the release, restart into it, or
    /// open its page.
    pub fn press(&mut self, cx: &mut Context<Self>) {
        match self.state.clone() {
            State::Available {
                release,
                installable: true,
            } => self.download(release, cx),
            State::Available { release, .. } | State::Failed { release, .. } => {
                crate::links::open_web_url(cx, &release.page);
            }
            State::Ready { release, staged } => self.restart(release, staged, cx),
            State::Idle | State::Downloading { .. } => {}
        }
    }

    /// Downloads `release` and checks it, on a thread of its own.
    fn download(&mut self, release: Release, cx: &mut Context<Self>) {
        let Some(package) = release.package.clone() else {
            return;
        };
        self.state = State::Downloading {
            release: release.clone(),
            percent: 0,
        };
        cx.notify();
        let (events, received) = async_channel::unbounded();
        let allow = self.allow;
        let version = release.version;
        let spawned = thread::Builder::new()
            .name("update-download".to_owned())
            .spawn(move || {
                let percent = |percent| {
                    events.try_send(Progress::Percent(percent)).ok();
                };
                let staged = prepare(&package, version, allow, &percent);
                events.try_send(Progress::Done(staged)).ok();
            });
        if let Err(error) = spawned {
            self.fail(release, format!("the download didn't start: {error}"), cx);
            return;
        }
        self.downloading = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = received.recv().await {
                let done = matches!(event, Progress::Done(_));
                let delivered = this.update(cx, |this, cx| this.progress(event, cx));
                if done || delivered.is_err() {
                    break;
                }
            }
        }));
    }

    /// Takes in what the downloading thread said.
    fn progress(&mut self, event: Progress, cx: &mut Context<Self>) {
        let State::Downloading { release, percent } = &mut self.state else {
            return;
        };
        match event {
            Progress::Percent(now) => {
                if *percent == now {
                    return;
                }
                *percent = now;
            }
            Progress::Done(Ok(staged)) => {
                self.state = State::Ready {
                    release: release.clone(),
                    staged,
                };
                self.downloading = None;
            }
            Progress::Done(Err(why)) => {
                let release = release.clone();
                self.downloading = None;
                self.fail(release, why, cx);
            }
        }
        cx.notify();
    }

    /// Replaces the app with the release staged at `staged`, opens it once
    /// this one has quit, and quits.
    fn restart(&mut self, release: Release, staged: PathBuf, cx: &mut Context<Self>) {
        match restart_into(&staged) {
            Ok(()) => cx.quit(),
            Err(why) => self.fail(release, why, cx),
        }
    }

    /// Installing `release` failed, for `why`: says so, and the pill
    /// offers its page.
    fn fail(&mut self, release: Release, why: String, cx: &mut Context<Self>) {
        eprintln!("ds-emulator: updating to {}: {why}", release.version);
        tell(
            cx,
            "The update couldn't be installed",
            format!(
                "{}. It can be downloaded from imagoemulator.com.",
                sentence(&why)
            ),
        );
        self.state = State::Failed { release, why };
        self.dismissed = false;
        cx.notify();
    }
}

/// What the header shows for `state`, unless `dismissed`.
fn pill(state: &State, dismissed: bool) -> Option<Pill> {
    let (label, busy, dismissable) = match state {
        State::Idle => return None,
        State::Available { .. } => ("Update Available".to_owned(), false, true),
        State::Downloading { percent, .. } => (format!("Downloading {percent}%"), true, false),
        State::Ready { .. } => ("Restart to Update".to_owned(), false, false),
        State::Failed { .. } => ("Download Update".to_owned(), false, true),
    };
    (!(dismissable && dismissed)).then_some(Pill {
        label,
        busy,
        dismissable,
    })
}

/// Shows `message` and `detail` in a dialog over the front window.
fn tell(cx: &mut App, message: &str, detail: impl Into<String>) {
    let Some(window) = cx
        .active_window()
        .or_else(|| cx.windows().into_iter().next())
    else {
        return;
    };
    let detail = detail.into();
    window
        .update(cx, |_, window, cx| {
            // Answered by its one button; nothing waits on it.
            // GPUI spells a plain "OK" as "Ok"; this one keeps its case.
            let ok = [PromptButton::Ok("OK".into())];
            drop(window.prompt(PromptLevel::Info, message, Some(&detail), &ok, cx));
        })
        .ok();
}

/// `why`, a reason the app gives in a sentence's middle, as a sentence's
/// start: its first letter a capital.
fn sentence(why: &str) -> String {
    let mut letters = why.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

/// Starts the automatic checks: soon after launch, then every few hours,
/// while Settings > Updates is on. Builds made for debugging and test
/// homes don't check unless [`FEED_VARIABLE`] names a feed, and none do
/// with [`OFF_VARIABLE`] set.
pub fn start(cx: &mut App) {
    let told = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let testing = cfg!(debug_assertions) || told(emu_os::HOME_VARIABLE);
    if told(OFF_VARIABLE) || (testing && !told(FEED_VARIABLE)) {
        return;
    }
    let updater = Updater::global(cx);
    cx.background_executor().spawn(async { tidy() }).detach();
    updater.update(cx, |this, cx| {
        this.schedule = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            loop {
                let on = settings::default_path()
                    .and_then(|path| Settings::load(&path).ok())
                    .is_none_or(|settings| settings.check_updates);
                if on && this.update(cx, |this, cx| this.check(false, cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(CHECK_EVERY).await;
            }
        }));
    });
}

/// Whether the app can install a release in place here.
#[cfg(target_os = "macos")]
fn can_install() -> bool {
    match install::Target::find() {
        Ok(_) => true,
        Err(why) => {
            eprintln!("ds-emulator: updates open the download page: {why}");
            false
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn can_install() -> bool {
    false
}

/// Downloads `package`, a release of `version`, and readies it to replace
/// the app, telling `percent` how much has come.
#[cfg(target_os = "macos")]
fn prepare(
    package: &Package,
    version: Version,
    allow: Allow,
    percent: &(dyn Fn(u8) + Sync),
) -> Result<PathBuf, String> {
    install::Target::find()?.prepare(package, version, allow, percent)
}

#[cfg(not(target_os = "macos"))]
fn prepare(_: &Package, _: Version, _: Allow, _: &(dyn Fn(u8) + Sync)) -> Result<PathBuf, String> {
    Err("updates aren't installed by the app on this system".to_owned())
}

/// Puts the app staged at `staged` in this one's place, to open once this
/// one has quit.
#[cfg(target_os = "macos")]
fn restart_into(staged: &std::path::Path) -> Result<(), String> {
    install::Target::find()?.install_and_relaunch(staged)
}

#[cfg(not(target_os = "macos"))]
fn restart_into(_: &std::path::Path) -> Result<(), String> {
    Err("updates aren't installed by the app on this system".to_owned())
}

/// Removes what an update left behind: a download never installed, or an
/// app replaced whose removal didn't finish.
#[cfg(target_os = "macos")]
fn tidy() {
    install::tidy();
}

#[cfg(not(target_os = "macos"))]
fn tidy() {}

#[cfg(test)]
mod tests {
    use super::*;

    const SUM: &str = "f658c80e6c85f11ab0e3153cbb7d3a5f6874d241178b231c755eddc9e7a96317";

    fn feed(version: &str, mac_url: &str, sha256: &str, size: u64) -> String {
        serde_json::json!({
            "version": version,
            "page": "https://imagoemulator.com/#download",
            "downloads": {
                "mac": { "url": mac_url, "sha256": sha256, "size": size },
                "windows": {
                    "url": "https://imagoemulator.com/downloads/Imago-Emulator-0.2.0-windows-x86_64.exe",
                    "sha256": SUM,
                    "size": 6_000_000
                }
            }
        })
        .to_string()
    }

    #[test]
    fn versions_are_three_numbers_and_compare_as_numbers() {
        assert_eq!(Version::parse("0.1.1"), Some(Version([0, 1, 1])));
        assert!(Version::parse("0.1.10") > Version::parse("0.1.9"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "1..3",
            "v1.2.3",
            "1.2.3-beta",
            " 1.2.3",
            "1.2.+3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_feed_gives_the_version_the_page_and_this_systems_package() {
        let url = "https://imagoemulator.com/downloads/Imago-Emulator-0.2.0.dmg";
        let text = feed("0.2.0", url, SUM, 10_400_000);
        let release = parse_feed(text.as_bytes(), Some("mac"), Allow::Https).expect("feed");
        assert_eq!(release.version, Version([0, 2, 0]));
        assert_eq!(release.page, "https://imagoemulator.com/#download");
        assert_eq!(
            release.package,
            Some(Package {
                url: url.to_owned(),
                sha256: SUM.to_owned(),
                size: 10_400_000
            })
        );
        // A system the app doesn't install on takes no package, and one
        // the feed leaves out has none.
        let release = parse_feed(text.as_bytes(), None, Allow::Https).expect("feed");
        assert_eq!(release.package, None);
        let release = parse_feed(text.as_bytes(), Some("linux"), Allow::Https).expect("feed");
        assert_eq!(release.package, None);
    }

    #[test]
    fn a_feed_that_could_install_something_else_is_refused() {
        let url = "https://imagoemulator.com/downloads/Imago-Emulator-0.2.0.dmg";
        for (text, why) in [
            (
                feed("0.2.0", "http://imagoemulator.com/x.dmg", SUM, 1),
                "plain http",
            ),
            (feed("0.2.0", "file:///tmp/x.dmg", SUM, 1), "a file"),
            (
                feed("0.2.0", url, &SUM.to_uppercase(), 1),
                "uppercase digest",
            ),
            (feed("0.2.0", url, &SUM[1..], 1), "short digest"),
            (feed("0.2.0", url, SUM, 0), "empty"),
            (feed("0.2.0", url, SUM, PACKAGE_MAX_BYTES + 1), "too big"),
            (feed("0.2", url, SUM, 1), "two-part version"),
            ("{}".to_owned(), "empty feed"),
            ("<html>".to_owned(), "not JSON"),
        ] {
            assert!(
                parse_feed(text.as_bytes(), Some("mac"), Allow::Https).is_err(),
                "{why}"
            );
        }
        let text = feed("0.2.0", url, SUM, 1)
            .replace("https://imagoemulator.com/#download", "javascript:alert(1)");
        assert!(parse_feed(text.as_bytes(), Some("mac"), Allow::Https).is_err());
        // A stand-in server on this computer only where tests allow it.
        let local = feed("0.2.0", "http://127.0.0.1:8080/x.dmg", SUM, 1);
        assert!(parse_feed(local.as_bytes(), Some("mac"), Allow::Https).is_err());
        assert!(parse_feed(local.as_bytes(), Some("mac"), Allow::HttpsOrLoopback).is_ok());
    }

    #[test]
    fn reasons_start_sentences_with_a_capital() {
        assert_eq!(sentence("the download failed"), "The download failed");
        assert_eq!(sentence("éclair"), "Éclair");
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn the_pill_follows_the_update_and_only_an_offer_is_dismissed() {
        let release = Release {
            version: Version([0, 2, 0]),
            page: "https://imagoemulator.com/#download".to_owned(),
            package: None,
        };
        let shown = |state: &State, dismissed| {
            pill(state, dismissed).map(|pill| (pill.label, pill.busy, pill.dismissable))
        };
        assert_eq!(shown(&State::Idle, false), None);
        let available = State::Available {
            release: release.clone(),
            installable: true,
        };
        assert_eq!(
            shown(&available, false),
            Some(("Update Available".into(), false, true))
        );
        assert_eq!(shown(&available, true), None);
        let downloading = State::Downloading {
            release: release.clone(),
            percent: 42,
        };
        assert_eq!(
            shown(&downloading, true),
            Some(("Downloading 42%".into(), true, false))
        );
        let ready = State::Ready {
            release: release.clone(),
            staged: PathBuf::from("/tmp/Imago Emulator.app"),
        };
        assert_eq!(
            shown(&ready, true),
            Some(("Restart to Update".into(), false, false))
        );
        let failed = State::Failed {
            release,
            why: "no".into(),
        };
        assert_eq!(
            shown(&failed, false),
            Some(("Download Update".into(), false, true))
        );
        assert_eq!(shown(&failed, true), None);
    }
}
