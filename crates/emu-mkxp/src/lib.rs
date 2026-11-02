//! RPG Maker games, played through mkxp-z.
//!
//! mkxp-z is an open-source player for RPG Maker XP, VX and VX Ace games.
//! A fan game made for Windows runs on a Mac in it: the game's folder holds
//! a player app (an mkxp-z build) and its settings, `mkxp.json`. This crate
//! runs a hidden copy of that player in a process of its own and makes it a
//! [`Core`]. Each frame it sends the buttons held and takes the picture
//! back, so the app paces, shows, records and controls the game as it does
//! a cartridge's. The player's sound comes back too.
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
pub mod link;
pub mod player;

use std::cell::RefCell;
use std::fs;
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

/// Why a game could not start.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The path is not a game folder with a player app.
    #[error("{} is not an RPG Maker game set up for mkxp-z: it needs a Game.ini and a player app", .0.display())]
    NotAGame(PathBuf),
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

/// The game folder `path` names: `path` itself, if it holds an RPG Maker
/// game (its `Game.ini`) and a player app, or the folder of the player app
/// `path` is.
pub fn game_folder(path: &Path) -> Option<PathBuf> {
    let folder = if is_app(path) { path.parent()? } else { path };
    let game = folder.join("Game.ini").is_file() && player_app(folder).is_some();
    game.then(|| folder.to_owned())
}

/// The player app in a game's folder: its first `.app`, by name, leaving
/// out hidden ones.
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
    /// Whether the next frame restarts the game.
    reset: bool,
    ended: RefCell<Option<String>>,
}

impl Mkxp {
    /// Starts the game that `path` names (see [`game_folder`]) in a hidden
    /// player, which [`player`] makes in `work_dir`, and waits for its
    /// first frame.
    ///
    /// Blocking: the game may take some seconds to start.
    ///
    /// # Errors
    ///
    /// Fails if `path` names no game, the player cannot be made or started,
    /// or the game does not draw its first frame within a minute.
    pub fn open(path: &Path, work_dir: &Path) -> Result<Self, Error> {
        let game = game_folder(path).ok_or_else(|| Error::NotAGame(path.to_owned()))?;
        let mut player = Player::prepare(&game, work_dir)?;
        let listener =
            link::listen(player.socket()).map_err(Error::io("listening for the game"))?;
        let sound = Capture::start(player.sound_pipe(), player.sound_settings())
            .map_err(Error::io("setting up the game's sound"))?;
        player.start()?;
        let started = link::accept(&listener, STARTUP_TIMEOUT).and_then(|stream| {
            stream.set_read_timeout(Some(FRAME_TIMEOUT))?;
            stream.set_write_timeout(Some(FRAME_TIMEOUT))?;
            let mut link = Link::new(stream);
            let hello = link.hello()?;
            Ok((link, hello))
        });
        player.forget_socket();
        let (link, hello) = started.map_err(|error| {
            let why = player.why_it_ended(&error);
            player.stop();
            Error::DidNotStart(why)
        })?;
        player.set_pid(hello.pid);
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

    #[test]
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
    fn frames_arrive_as_rgba_and_show_opaque() {
        let mut frame = Frame::new(ScreenSize::new(2, 1));
        rgba_into(
            &[0x12, 0x34, 0x56, 0x00, 0xFF, 0x00, 0x00, 0x80],
            &mut frame,
        );
        assert_eq!(frame.pixels(), [rgb(0x12, 0x34, 0x56), rgb(0xFF, 0, 0)]);
    }

    #[test]
    fn screen_lists_are_made_once_per_size() {
        let size = ScreenSize::new(512, 384);
        assert!(std::ptr::eq(screens_of(size), screens_of(size)));
        assert_eq!(
            screens_of(ScreenSize::new(640, 480)),
            [ScreenSize::new(640, 480)]
        );
    }
}
