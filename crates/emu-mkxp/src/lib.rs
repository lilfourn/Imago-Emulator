//! RPG Maker games, played through mkxp-z.
//!
//! mkxp-z is an open-source player for RPG Maker XP, VX and VX Ace games.
//! A fan game made for Windows runs on a Mac in it: the game's folder holds
//! a player app (an mkxp-z build) and its settings, `mkxp.json`. On Linux
//! and Windows the player is mkxp-z's program for that system: one the game
//! ships, or one put once in the players' folder for every game
//! ([`SHARED_PLAYER`]). This crate runs a hidden copy of that player in a
//! process of its own and makes it a [`Core`]. Each frame it sends the
//! buttons held and takes the picture back, so the app paces, shows,
//! records and controls the game as it does a cartridge's. The player's
//! sound comes back too.
//!
//! It is the one core that is not deterministic: the game is a Ruby
//! program in another process. Its save states are the game's own saves,
//! which the game's scripts make and take through the bridge (see
//! [`link`]), so they work only where the game can save, and are too slow
//! for rewind.
//!
//! - [`player`] makes the hidden player and starts it;
//! - [`link`] is how this process and the bridge script (`bridge.rb`), which
//!   the player runs before the game, talk;
//! - [`audio`] reads the player's sound.

pub mod audio;
pub mod config;
#[cfg(target_os = "linux")]
mod display;
pub mod install;
pub mod link;
pub mod player;
pub mod run;

use std::cell::RefCell;
use std::fs;
#[cfg(not(target_os = "macos"))]
use std::fs::File;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use emu_core::{Core, CoreInfo, FrameRate, Input, ScreenSize, StateError, StereoSample, System};
use emu_core::{Frame, rgb};

use crate::audio::Capture;
use crate::link::{Hello, Link};
use crate::player::Player;
pub use crate::run::Run;

/// How long a game has to start: to draw its first frame.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one frame may take before the game counts as stuck, as when
/// the player shows an error that no one can see, and how long it may
/// leave a message unread.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// RPG Maker XP's screen and frame rate, which a game's core has until
/// its game says its own (see [`Mkxp::info`]).
pub const INFO: CoreInfo = CoreInfo {
    system: System::RpgMaker,
    screens: &[ScreenSize::new(640, 480)],
    touch_screen: None,
    players: 1,
    frame_rate: FrameRate::new(40, 1),
    audio_sample_rate: audio::SAMPLE_RATE,
    // The game's own save format.
    state_version: 1,
};

/// Why RPG Maker games can't play on this system, or `None` where they
/// can: on macOS, Linux and Windows.
pub const UNSUPPORTED: Option<&str> =
    if cfg!(any(target_os = "macos", target_os = "linux", windows)) {
        None
    } else {
        Some("RPG Maker games aren't supported on this system yet")
    };

/// Why a game could not start.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// RPG Maker games don't play on this system yet ([`UNSUPPORTED`]).
    #[error("{0}")]
    Unsupported(&'static str),
    /// The path is not a game folder with a player app.
    #[error("{} is not an RPG Maker game set up for mkxp-z: it needs a Game.ini and a player app", .0.display())]
    NotAGame(PathBuf),
    /// Off macOS: neither the game's folder nor the players' folder, this
    /// one, holds an mkxp-z for this system, and there is no build of it to
    /// download.
    #[error(
        "there is no mkxp-z build for {SYSTEM} ({}) to download: put one in {}, for every game, or in the game's folder",
        std::env::consts::ARCH,
        .0.display()
    )]
    NoPlayer(PathBuf),
    /// Off macOS: the game needed the hosted mkxp-z, and it could not be
    /// installed.
    #[error("{source}. An mkxp-z build for {SYSTEM} put in {} plays instead", .players.display())]
    Install {
        /// Why.
        #[source]
        source: install::InstallError,
        /// The players' folder.
        players: PathBuf,
    },
    /// On Linux: there is neither Xvfb nor an X display for the player to
    /// draw on.
    #[error(
        "RPG Maker games play on Linux through mkxp-z on an X display: install Xvfb (the xvfb package), so it plays unseen, or play in a session with an X display"
    )]
    NoDisplay,
    /// Preparing or starting the player failed.
    #[error("{what}: {source}")]
    Io {
        /// What was being done.
        what: String,
        /// What went wrong.
        #[source]
        source: io::Error,
    },
    /// The game's `mkxp.json` cannot be read.
    #[error("the game's mkxp.json: {0}")]
    Settings(String),
    /// The player started but the game did not.
    #[error("the game did not start: {0}")]
    DidNotStart(String),
}

impl Error {
    fn io(what: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let what = what.into();
        move |source| Self::Io { what, source }
    }
}

/// What the system is called, in messages.
const SYSTEM: &str = if cfg!(windows) {
    "Windows"
} else if cfg!(target_os = "linux") {
    "Linux"
} else if cfg!(target_os = "macos") {
    "macOS"
} else {
    "this system"
};

/// The app's work folder for RPG Maker games, which it gives
/// [`Mkxp::open`]: `RPG Maker` in its data folder, which holds each game's
/// hidden player and, off macOS, the players' folder ([`SHARED_PLAYER`]).
/// `None` if there is no data folder.
pub fn data_dir() -> Option<PathBuf> {
    Some(emu_os::data_dir()?.join("ds-emulator").join("RPG Maker"))
}

/// Whether opening the game `path` names with [`Mkxp::open`] and
/// `work_dir` downloads mkxp-z first: off macOS, when neither the game's
/// folder nor the players' folder has an mkxp-z for this system and the
/// hosted one is not installed yet. For the app to say so before it starts
/// ([`install::DOWNLOAD_MB`]).
pub fn downloads_player(path: &Path, work_dir: &Path) -> bool {
    #[cfg(not(target_os = "macos"))]
    {
        // The game's own last: on Windows its Game.exe may be read through.
        let players = work_dir.join(SHARED_PLAYER);
        install::PIN.is_some()
            && install::installed(&players).is_none()
            && mkxp_z_in(&players).is_none()
            && game_folder(path).is_some_and(|folder| mkxp_z(&played_game(&folder)).is_none())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (path, work_dir);
        false
    }
}

/// The players' folder, in the work folder [`Mkxp::open`] is given, off
/// macOS. A game without an mkxp-z of its own plays in the build the player
/// put there, as it comes (such as `mkxp-z.x86_64` on Linux or
/// `mkxp-z.x86_64.exe` on Windows), or else in the hosted build, which is
/// downloaded into a folder of its own there the first time it is needed
/// ([`install`]).
pub const SHARED_PLAYER: &str = "mkxp-z";

/// The game folder `path` names: `path` itself, if it holds an RPG Maker
/// game (its `Game.ini`) and a player app, or the folder of the player app
/// `path` is. The folder of a run of such a game ([`Run`]) is one too.
pub fn game_folder(path: &Path) -> Option<PathBuf> {
    let folder = if is_app(path) { path.parent()? } else { path };
    let holds_game =
        |folder: &Path| folder.join("Game.ini").is_file() && player_app(folder).is_some();
    let game = match Run::read(folder) {
        Some(run) => holds_game(&run.game),
        None => holds_game(folder),
    };
    game.then(|| folder.to_owned())
}

/// The folder of the game itself that the game folder `folder` plays:
/// `folder`, or for a run's folder, the folder of the game it is a run of.
#[must_use]
pub fn played_game(folder: &Path) -> PathBuf {
    Run::read(folder).map_or_else(|| folder.to_owned(), |run| run.game)
}

/// The settings of the game in `folder`, its `mkxp.json` as mkxp-z reads
/// it, if it has one that can be read.
#[must_use]
pub fn game_settings(folder: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(folder.join("mkxp.json")).ok()?;
    config::game_settings(&text)
        .ok()
        .map(serde_json::Value::Object)
}

/// The player app in a game's folder: on macOS its first `.app`, by name,
/// leaving out hidden ones. Elsewhere, the game's program: the mkxp-z the
/// folder holds for this system, or else its `Game.exe`, in whose place the
/// players' mkxp-z plays (see [`SHARED_PLAYER`]); its icon is the game's.
#[cfg(not(target_os = "macos"))]
pub fn player_app(folder: &Path) -> Option<PathBuf> {
    mkxp_z_in(folder).or_else(|| file_named(folder, "Game.exe"))
}

/// The player app in a game's folder: on macOS its first `.app`, by name,
/// leaving out hidden ones. Elsewhere, the game's program: the mkxp-z the
/// folder holds for this system, or else its `Game.exe`, in whose place the
/// players' mkxp-z plays (see [`SHARED_PLAYER`]); its icon is the game's.
#[cfg(target_os = "macos")]
pub fn player_app(folder: &Path) -> Option<PathBuf> {
    let mut apps: Vec<PathBuf> = fs::read_dir(folder)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| is_app(path))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
        })
        .collect();
    apps.sort();
    apps.into_iter().next()
}

/// Whether `path` is a Mac app: a folder whose name ends in `.app`.
fn is_app(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        && path.is_dir()
}

/// The file in `folder` called `name`, in any case.
#[cfg(not(target_os = "macos"))]
fn file_named(folder: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(folder)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.is_file())
        .find(|path| {
            path.file_name()
                .is_some_and(|file| file.eq_ignore_ascii_case(name))
        })
}

/// The mkxp-z that plays the game in `game`: the one its folder holds for
/// this system, which on Windows may be its `Game.exe`.
#[cfg(not(target_os = "macos"))]
fn mkxp_z(game: &Path) -> Option<PathBuf> {
    let named = mkxp_z_in(game);
    #[cfg(windows)]
    let named = named.or_else(|| file_named(game, "Game.exe").filter(|exe| is_mkxp_z(exe)));
    named
}

/// The mkxp-z program for this system in `folder`, by its name: the first
/// file whose name starts with `mkxp-z` and that runs here, an x86-64 or
/// arm64 Linux program (such as `mkxp-z.x86_64`) for this processor, or a
/// Windows program (`mkxp-z.x86_64.exe`).
#[cfg(not(target_os = "macos"))]
fn mkxp_z_in(folder: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(folder)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .to_ascii_lowercase()
                    .starts_with("mkxp-z")
            })
        })
        .filter(|path| path.is_file() && runs_here(path))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Whether the program at `path` runs on this system and processor: an
/// ELF program for it on Linux, or a Windows program that Windows runs here
/// (with a name ending `.exe`).
#[cfg(not(target_os = "macos"))]
fn runs_here(path: &Path) -> bool {
    use std::io::Read as _;

    let mut header = [0; 0x400];
    let Ok(read) = File::open(path).and_then(|mut file| file.read(&mut header)) else {
        return false;
    };
    let header = &header[..read];
    let word = |at: usize| Some(u16::from_le_bytes([*header.get(at)?, *header.get(at + 1)?]));
    if cfg!(windows) {
        // MZ, then where the PE header is, its signature and its machine.
        let pe = header.get(0x3C..0x40).map_or(0, |at| {
            u32::from_le_bytes([at[0], at[1], at[2], at[3]]) as usize
        });
        let machine = word(pe + 4);
        let runs = match std::env::consts::ARCH {
            // x86 (WoW64) and x86-64 programs; arm64 Windows runs all three.
            "x86_64" => [Some(0x014C), Some(0x8664)].contains(&machine),
            "aarch64" => [Some(0x014C), Some(0x8664), Some(0xAA64)].contains(&machine),
            _ => machine == Some(0x014C),
        };
        header.starts_with(b"MZ")
            && header.get(pe..pe + 4) == Some(&b"PE\0\0"[..])
            && runs
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    } else {
        // A little-endian ELF program for this processor.
        let machine = match std::env::consts::ARCH {
            "x86_64" => Some(62),
            "aarch64" => Some(183),
            _ => None,
        };
        header.starts_with(b"\x7fELF") && header.get(5) == Some(&1) && word(18) == machine
    }
}

/// Whether the Windows program at `exe` is mkxp-z, as a `Game.exe` that
/// Pokémon Essentials' newer games ship may be: it names itself. RPG
/// Maker's own `Game.exe` is a small program that loads RGSS from a DLL.
#[cfg(windows)]
fn is_mkxp_z(exe: &Path) -> bool {
    use std::io::Read as _;

    const NAME: &[u8] = b"mkxp-z";
    let Ok(file) = File::open(exe) else {
        return false;
    };
    let mut reader = io::BufReader::new(file);
    let mut chunk = vec![0; 1 << 16];
    // The end of the last chunk, in case the name spans two.
    let mut kept = 0;
    loop {
        let read = match reader.read(&mut chunk[kept..]) {
            Ok(0) | Err(_) => return false,
            Ok(read) => read,
        };
        let end = kept + read;
        if chunk[..end]
            .windows(NAME.len())
            .any(|window| window == NAME)
        {
            return true;
        }
        kept = (NAME.len() - 1).min(end);
        chunk.copy_within(end - kept..end, 0);
    }
}

/// The extensions of RPG Maker XP's, VX's and VX Ace's save files.
const SAVE_EXTENSIONS: [&str; 3] = ["rxdata", "rvdata", "rvdata2"];

/// Where a game keeps the saves it makes itself: the folder mkxp-z gives it
/// for its files, where Pokémon Essentials saves on a Mac, and the game's
/// own folder, where RPG Maker's own save screens put `Save1.rxdata` and
/// the like.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveFolders(Vec<PathBuf>);

impl SaveFolders {
    /// The saves in `folders`.
    pub fn new(folders: Vec<PathBuf>) -> Self {
        Self(folders)
    }

    /// The save files in the folders now, in order: those directly in each
    /// folder with a save file's extension.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self
            .0
            .iter()
            .filter_map(|folder| fs::read_dir(folder).ok())
            .flatten()
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| {
                path.extension().is_some_and(|extension| {
                    SAVE_EXTENSIONS
                        .iter()
                        .any(|save| extension.eq_ignore_ascii_case(save))
                })
            })
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        files.dedup();
        files
    }
}

/// The title in the `Game.ini` of the game in `folder`.
pub fn title(folder: &Path) -> Option<String> {
    let ini = fs::read(folder.join("Game.ini")).ok()?;
    String::from_utf8_lossy(&ini)
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("Title")
                .then(|| value.trim().to_owned())
        })
        .filter(|title| !title.is_empty())
}

/// An RPG Maker game running in its hidden player.
pub struct Mkxp {
    info: CoreInfo,
    player: Player,
    /// In cells, as saving a state, which leaves the machine as it is,
    /// still talks to the game.
    link: RefCell<Link>,
    screens: Vec<Frame>,
    /// The last frame's RGBA bytes, as the bridge sent them.
    pixels: Vec<u8>,
    sound: Capture,
    /// Where the game keeps the saves it makes itself.
    save_folders: SaveFolders,
    /// Whether the next frame restarts the game.
    reset: bool,
    ended: RefCell<Option<String>>,
}

impl Mkxp {
    /// Starts the game that `path` names (see [`game_folder`]) in a hidden
    /// player, which [`player`] makes in `work_dir`, and waits for its
    /// first frame.
    ///
    /// Blocking: the game may take some seconds to start, and off macOS the
    /// first game may download mkxp-z ([`downloads_player`]).
    ///
    /// # Errors
    ///
    /// Fails if RPG Maker games don't play on this system ([`UNSUPPORTED`]),
    /// `path` names no game, there is no mkxp-z for it and the hosted one
    /// cannot be downloaded ([`SHARED_PLAYER`]), the player cannot be made or
    /// started, or the game does not draw its first frame within a minute.
    pub fn open(path: &Path, work_dir: &Path) -> Result<Self, Error> {
        if let Some(why) = UNSUPPORTED {
            return Err(Error::Unsupported(why));
        }
        // The player runs in another folder, where a relative path would
        // name another place.
        let absolute = |path: &Path| {
            std::path::absolute(path).map_err(Error::io(format!("finding {}", path.display())))
        };
        let (path, work_dir) = (absolute(path)?, absolute(work_dir)?);
        let folder = game_folder(&path).ok_or_else(|| Error::NotAGame(path.clone()))?;
        // A run plays the game it is a run of, in a player of its own.
        let run = Run::read(&folder);
        let game = run
            .as_ref()
            .map_or_else(|| folder.clone(), |run| run.game.clone());
        let mut player = Player::prepare(
            &game,
            &work_dir,
            run.as_ref().map(|run| (folder.as_path(), run)),
        )?;
        let listener =
            link::listen(player.socket()).map_err(Error::io("listening for the game"))?;
        let sound = Capture::start(player.sound_pipe(), player.sound_settings())
            .map_err(Error::io("setting up the game's sound"))?;
        let link = listener
            .environment()
            .map_err(Error::io("listening for the game"))?;
        player.start(&link)?;
        let started = link::accept_while(&listener, STARTUP_TIMEOUT, || player.running()).and_then(
            |stream| {
                stream.set_read_timeout(Some(FRAME_TIMEOUT))?;
                stream.set_write_timeout(Some(FRAME_TIMEOUT))?;
                let mut link = Link::new(stream);
                let hello = link.hello()?;
                Ok((link, hello))
            },
        );
        player.forget_socket();
        let (link, hello) = started.map_err(|error| {
            let why = player.why_it_ended(&error);
            player.stop();
            Error::DidNotStart(why)
        })?;
        player.set_pid(hello.pid);
        let save_folders = SaveFolders(hello.save_folder.iter().cloned().chain([game]).collect());
        let size = ScreenSize::new(hello.width, hello.height);
        let mut core = Self {
            info: CoreInfo {
                screens: screens_of(size),
                frame_rate: FrameRate::new(u32::from(hello.frame_rate.max(1)), 1),
                ..INFO
            },
            player,
            link: RefCell::new(link),
            screens: vec![Frame::new(size)],
            pixels: vec![0; hello.frame_bytes()],
            sound,
            save_folders,
            reset: false,
            ended: RefCell::new(None),
        };
        core.read_frame();
        match core.ended.take() {
            Some(why) => Err(Error::DidNotStart(why)),
            None => Ok(core),
        }
    }

    /// Where the game's sound also goes as it comes, on the player's clock
    /// rather than with each frame: a frame that loads a map can take a
    /// tenth of a second, while the music plays on.
    pub fn sound_tap(&self) -> audio::Tap {
        self.sound.tap()
    }

    /// Where the game keeps the saves it makes itself, which its save
    /// states, made from the game as it plays, leave alone.
    pub fn save_folders(&self) -> SaveFolders {
        self.save_folders.clone()
    }

    /// Reads the frame the bridge sends next into the screen.
    fn read_frame(&mut self) {
        match self.link.get_mut().frame(&mut self.pixels) {
            Ok(()) => rgba_into(&self.pixels, &mut self.screens[0]),
            Err(error) => self.lost(&error),
        }
    }

    /// Records that the link failed with `error`, which ends the game.
    fn lost(&self, error: &io::Error) {
        *self.ended.borrow_mut() = Some(self.player.why_it_ended(error));
    }
}

impl Core for Mkxp {
    fn info(&self) -> CoreInfo {
        self.info
    }

    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>) {
        if self.ended.get_mut().is_none() {
            let buttons = inputs.first().map_or(0, |input| input.buttons.bits());
            let flags = if mem::take(&mut self.reset) {
                link::RESET
            } else {
                0
            };
            match self.link.get_mut().step(buttons, flags) {
                Ok(()) => self.read_frame(),
                Err(error) => self.lost(&error),
            }
        }
        self.sound.drain(audio);
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        self.reset = true;
    }

    /// The game's save, as the game's scripts make it, or nothing where
    /// the game cannot save.
    fn save_state(&self, out: &mut Vec<u8>) {
        self.try_save_state(out).ok();
    }

    /// Restarts the game from `state`, one of its saves, at the next frame.
    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        if self.ended.get_mut().is_some() {
            return Err(StateError::Malformed("the game has stopped"));
        }
        self.link.get_mut().load(state).map_err(|error| {
            self.lost(&error);
            StateError::Malformed("the game stopped before it took the state")
        })
    }

    fn try_save_state(&self, out: &mut Vec<u8>) -> Result<(), String> {
        if let Some(why) = self.ended() {
            return Err(why);
        }
        let saved = self.link.borrow_mut().save();
        match saved {
            Ok(Ok(state)) => {
                out.extend(state);
                Ok(())
            }
            Ok(Err(why)) => Err(why),
            Err(error) => {
                self.lost(&error);
                Err(self.ended().unwrap_or_default())
            }
        }
    }

    fn saves_every_frame(&self) -> bool {
        false
    }

    fn ended(&self) -> Option<String> {
        self.ended.borrow().clone()
    }
}

impl Drop for Mkxp {
    fn drop(&mut self) {
        // The bridge quits the player when the link closes.
        self.link.get_mut().close();
        self.player.stop_soon();
    }
}

/// A screen list of `size` for [`CoreInfo::screens`], which wants it for
/// good: made once per size the games have, and kept.
fn screens_of(size: ScreenSize) -> &'static [ScreenSize] {
    static MADE: OnceLock<Mutex<Vec<&'static [ScreenSize]>>> = OnceLock::new();
    let mut made = MADE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(screens) = made.iter().find(|screens| screens[0] == size) {
        return screens;
    }
    let screens: &'static [ScreenSize] = Box::leak(Box::new([size]));
    made.push(screens);
    screens
}

/// Copies `rgba`, bytes in R, G, B, A order, into `frame`, opaque.
fn rgba_into(rgba: &[u8], frame: &mut Frame) {
    for (pixel, bytes) in frame.pixels_mut().iter_mut().zip(rgba.chunks_exact(4)) {
        *pixel = rgb(bytes[0], bytes[1], bytes[2]);
    }
}

impl Hello {
    /// How many bytes each of the game's frames takes.
    fn frame_bytes(&self) -> usize {
        usize::from(self.width) * usize::from(self.height) * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-mkxp-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp folder");
        dir
    }

    /// The start of a program for this system and processor, enough for
    /// [`runs_here`], as `name` in `folder`, or its usual name.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn fake_program(folder: &Path, name: &str) -> PathBuf {
        let mut header = vec![0; 0x100];
        if cfg!(windows) {
            header[..2].copy_from_slice(b"MZ");
            header[0x3C] = 0x80;
            header[0x80..0x84].copy_from_slice(b"PE\0\0");
            let machine: u16 = if cfg!(target_arch = "aarch64") {
                0xAA64
            } else {
                0x8664
            };
            header[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        } else {
            header[..4].copy_from_slice(b"\x7fELF");
            header[4] = 2;
            header[5] = 1;
            let machine: u16 = if cfg!(target_arch = "aarch64") {
                183
            } else {
                62
            };
            header[18..20].copy_from_slice(&machine.to_le_bytes());
        }
        let path = folder.join(name);
        fs::write(&path, header).expect("program");
        path
    }

    /// A stand-in for mkxp-z for this system in `folder`, by the name its
    /// builds have.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn fake_mkxp_z(folder: &Path) -> PathBuf {
        let name = format!(
            "mkxp-z.{}{}",
            std::env::consts::ARCH,
            std::env::consts::EXE_SUFFIX
        );
        fake_program(folder, &name)
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_game_folder_has_a_game_ini_and_a_player_app() {
        let dir = temp_dir("folder");
        let game = dir.join("My Game");
        fs::create_dir_all(game.join("My Game.app/Contents")).expect("player");
        fs::create_dir_all(game.join(".hidden.app")).expect("hidden app");
        assert_eq!(game_folder(&game), None, "no Game.ini yet");
        fs::write(
            game.join("Game.ini"),
            "[Game]\r\nLibrary=RGSS104E.dll\r\nTitle=My Game\r\n",
        )
        .expect("Game.ini");
        assert_eq!(game_folder(&game), Some(game.clone()));
        assert_eq!(game_folder(&game.join("My Game.app")), Some(game.clone()));
        assert_eq!(player_app(&game), Some(game.join("My Game.app")));
        assert_eq!(title(&game).as_deref(), Some("My Game"));
        fs::remove_dir_all(game.join("My Game.app")).expect("removed");
        assert_eq!(game_folder(&game), None, "no player app");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_game_folder_has_a_game_ini_and_a_program() {
        let dir = temp_dir("folder");
        let game = dir.join("My Game");
        fs::create_dir_all(game.join("My Game.app/Contents")).expect("a Mac player");
        assert_eq!(game_folder(&game), None, "no Game.ini yet");
        fs::write(
            game.join("Game.ini"),
            "[Game]\r\nLibrary=RGSS104E.dll\r\nTitle=My Game\r\n",
        )
        .expect("Game.ini");
        assert_eq!(game_folder(&game), None, "a Mac app plays only on a Mac");
        assert_eq!(title(&game).as_deref(), Some("My Game"));
        // RPG Maker's own Game.exe: the shared mkxp-z plays the game.
        let exe = fake_program(&game, "GAME.EXE");
        assert_eq!(game_folder(&game), Some(game.clone()));
        assert_eq!(player_app(&game), Some(exe.clone()));
        assert_eq!(
            mkxp_z(&game),
            None,
            "RPG Maker's own Game.exe is not mkxp-z"
        );
        // A build for another system or processor does not play here.
        let other = if cfg!(windows) {
            "mkxp-z.aarch64"
        } else {
            "mkxp-z.exe"
        };
        fs::write(game.join(other), b"\x7fELF").expect("another system's");
        assert_eq!(mkxp_z(&game), None);
        let mkxp = fake_mkxp_z(&game);
        assert_eq!(player_app(&game), Some(mkxp.clone()));
        assert_eq!(mkxp_z(&game), Some(mkxp));
        let shared = dir.join("work").join(SHARED_PLAYER);
        fs::create_dir_all(&shared).expect("players' folder");
        assert_eq!(mkxp_z_in(&shared), None);
        let theirs = fake_mkxp_z(&shared);
        assert_eq!(mkxp_z_in(&shared), Some(theirs));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_game_plays_in_its_own_mkxp_z_then_the_players_then_the_hosted_one() {
        let dir = temp_dir("lookup");
        let game = dir.join("Game");
        fs::create_dir_all(&game).expect("game");
        fs::write(game.join("Game.ini"), "[Game]\r\nTitle=Game\r\n").expect("Game.ini");
        fake_program(&game, "Game.exe");
        let players = dir.join("work").join(SHARED_PLAYER);
        let pin = install::PIN.expect("a hosted build for this system");
        // The hosted build, as an earlier game installed it.
        let hosted = players.join(pin.folder);
        fs::create_dir_all(&hosted).expect("hosted folder");
        let hosted = fake_program(&hosted, pin.program);
        assert!(!downloads_player(&game, &dir.join("work")));
        assert_eq!(player::player_program(&game, &players).ok(), Some(hosted));
        let theirs = fake_mkxp_z(&players);
        assert_eq!(player::player_program(&game, &players).ok(), Some(theirs));
        let own = fake_mkxp_z(&game);
        assert_eq!(player::player_program(&game, &players).ok(), Some(own));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn without_a_player_the_game_says_where_one_goes() {
        let players = Path::new("/home/me/.local/share/ds-emulator/RPG Maker/mkxp-z");
        let system = if cfg!(windows) { "Windows" } else { "Linux" };
        let arch = std::env::consts::ARCH;
        assert_eq!(
            Error::NoPlayer(players.to_owned()).to_string(),
            format!(
                "there is no mkxp-z build for {system} ({arch}) to download: put one in {}, for every game, or in the game's folder",
                players.display()
            )
        );
        let failed = Error::Install {
            source: install::InstallError::Download(
                "curl, which downloads it, is not installed".to_owned(),
            ),
            players: players.to_owned(),
        };
        assert_eq!(
            failed.to_string(),
            format!(
                "could not download mkxp-z: curl, which downloads it, is not installed. An mkxp-z build for {system} put in {} plays instead",
                players.display()
            )
        );
    }

    #[test]
    fn save_files_are_those_with_rpg_makers_extensions_in_either_folder() {
        let dir = temp_dir("saves");
        let (data, game) = (dir.join("data"), dir.join("game"));
        for folder in [&data, &game, &game.join("Data")] {
            fs::create_dir_all(folder).expect("folder");
        }
        for file in [
            data.join("Game.rxdata"),
            data.join("errorlog.txt"),
            data.join("Game.rxdata.bak"),
            game.join("Save1.RXDATA"),
            game.join("Save01.rvdata2"),
            game.join("Game.ini"),
            game.join("Data/Map001.rxdata"),
        ] {
            fs::write(file, b"x").expect("file");
        }
        fs::create_dir_all(game.join("Saves.rvdata")).expect("a folder, not a save");
        let folders = SaveFolders(vec![data.clone(), game.clone(), dir.join("gone")]);
        assert_eq!(
            folders.files(),
            [
                data.join("Game.rxdata"),
                game.join("Save01.rvdata2"),
                game.join("Save1.RXDATA"),
            ]
        );
        assert_eq!(SaveFolders::default().files(), Vec::<PathBuf>::new());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frames_arrive_as_rgba_and_show_opaque() {
        let mut frame = Frame::new(ScreenSize::new(2, 1));
        rgba_into(
            &[0x12, 0x34, 0x56, 0x00, 0xFF, 0x00, 0x00, 0x80],
            &mut frame,
        );
        assert_eq!(frame.pixels(), [rgb(0x12, 0x34, 0x56), rgb(0xFF, 0, 0)]);
    }

    #[test]
    fn a_game_s_screen_list_is_its_own_size() {
        let (small, large) = (ScreenSize::new(512, 384), ScreenSize::new(640, 480));
        assert_eq!(screens_of(small), [small]);
        assert_eq!(screens_of(large), [large]);
        assert_eq!(screens_of(small), [small], "not the last size made");
    }
}
