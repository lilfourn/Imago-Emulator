//! The hidden player: a copy of the game's own player app that runs the
//! game unseen.
//!
//! mkxp-z takes `Contents/Game` inside its app, when there is one, as the
//! game's folder. The copy's holds only an `mkxp.json` (see [`config`]):
//! the game's own settings, pointed at the real folder, with the bridge
//! script loaded first. The game's folder stays as it is, and the game
//! keeps its saves where it always has.
//!
//! The copy is an agent app (`LSUIElement`), so it has no Dock icon or menu
//! bar, and opts out of App Nap, which would slow it once hidden. `open -j`
//! starts it hidden, and SDL is asked not to bring it to the front, so the
//! app keeps the keyboard.
//!
//! A game has one hidden player at a time. Its copy, socket and logs are
//! the game's, so a second player started beside the first, as a double
//! click while the game loads did, could take the first's link, and the
//! open left without one stopped both once it gave up.
//!
//! Everything here blocks.
//!
//! [`config`]: crate::config

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::{Error, config, player_app};

/// The script the player runs before the game's own, ds-emulator's end of
/// the link (see [`crate::link`]).
const BRIDGE: &str = include_str!("bridge.rb");
/// How long a player has to quit by itself once the link closes.
const QUIT_GRACE: Duration = Duration::from_secs(2);
/// What the copy's `Info.plist` changes, besides its bundle ID: no Dock
/// icon or menu bar, no App Nap, and a screen of the game's own size. On a
/// Retina screen mkxp-z's OpenGL drawing read back a quarter of the screen,
/// twice as large.
const PLIST_CHANGES: [(&str, &str, &str); 3] = [
    ("LSUIElement", "-bool", "true"),
    ("LSAppNapIsDisabled", "-bool", "true"),
    ("NSHighResolutionCapable", "-bool", "false"),
];

/// A game's hidden player.
#[derive(Debug)]
pub struct Player {
    app: PathBuf,
    socket: PathBuf,
    sound_pipe: PathBuf,
    sound_settings: PathBuf,
    /// What the player prints, and what it prints as errors.
    output: PathBuf,
    errors: PathBuf,
    pid: Option<u32>,
    /// Locked for as long as this is the game's player.
    _lock: File,
}

impl Player {
    /// Makes or brings up to date the hidden player of the game in `game`,
    /// in a folder of its own in `work_dir`, once the game has no other
    /// player: an earlier one is waited for until it is dropped.
    ///
    /// # Errors
    ///
    /// Fails if the game has no player app, its settings cannot be read, or
    /// the copy cannot be made.
    pub fn prepare(game: &Path, work_dir: &Path) -> Result<Self, Error> {
        let source = player_app(game).ok_or_else(|| Error::NotAGame(game.to_owned()))?;
        let id = format!(
            "{:08X}",
            crc32fast::hash(game.as_os_str().as_encoded_bytes())
        );
        let dir = work_dir.join(&id);
        fs::create_dir_all(&dir).map_err(Error::io(format!("making {}", dir.display())))?;
        let lock_path = dir.join("player.lock");
        let lock = File::create(&lock_path)
            .and_then(|lock| lock.lock().map(|()| lock))
            .map_err(Error::io(format!("locking {}", lock_path.display())))?;
        let bridge = work_dir.join("bridge.rb");
        write_if_changed(&bridge, BRIDGE)
            .map_err(Error::io(format!("writing {}", bridge.display())))?;
        let game_settings = match fs::read_to_string(game.join("mkxp.json")) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::io("reading the game's mkxp.json")(error)),
        };
        let settings = config::player_settings(game_settings.as_deref(), game, &bridge)
            .map_err(Error::Settings)?;
        let app = dir.join("Player.app");
        copy_player(&source, &app, &id)?;
        let settings_path = app.join("Contents/Game/mkxp.json");
        if write_if_changed(&settings_path, &settings)
            .map_err(Error::io(format!("writing {}", settings_path.display())))?
        {
            sign(&app)?;
        }
        // Sockets' paths are short, so these go in the temporary folder.
        let temp = std::env::temp_dir();
        Ok(Self {
            app,
            socket: temp.join(format!("ds-emulator-{id}.sock")),
            sound_pipe: temp.join(format!("ds-emulator-{id}.fifo")),
            sound_settings: dir.join("alsoft.conf"),
            output: dir.join("player.log"),
            errors: dir.join("player-errors.log"),
            pid: None,
            _lock: lock,
        })
    }

    /// Where the bridge connects.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The named pipe the player's sound goes to.
    pub fn sound_pipe(&self) -> &Path {
        &self.sound_pipe
    }

    /// OpenAL Soft's settings for the player, which send its sound to
    /// [`Player::sound_pipe`].
    pub fn sound_settings(&self) -> &Path {
        &self.sound_settings
    }

    /// Starts the player, hidden.
    ///
    /// # Errors
    ///
    /// Fails if macOS cannot open it.
    pub fn start(&self) -> Result<(), Error> {
        fs::remove_file(&self.output).ok();
        fs::remove_file(&self.errors).ok();
        let env = |name: &str, path: &Path| format!("{name}={}", path.display());
        let output = Command::new("open")
            .args([
                "-n",
                "-j",
                "-g",
                "--env",
                "SDL_MAC_BACKGROUND_APP=1",
                "--env",
            ])
            .arg(env("DS_EMULATOR_LINK", &self.socket))
            .arg("--env")
            .arg(env("ALSOFT_CONF", &self.sound_settings))
            .arg("--stdout")
            .arg(&self.output)
            .arg("--stderr")
            .arg(&self.errors)
            .arg(&self.app)
            .output()
            .map_err(Error::io("running open"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::DidNotStart(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }

    /// Records the player's process ID, from the bridge's hello.
    pub fn set_pid(&mut self, pid: u32) {
        self.pid = Some(pid);
    }

    /// Removes the socket's file, once the bridge has connected or will not.
    pub fn forget_socket(&self) {
        fs::remove_file(&self.socket).ok();
    }

    /// Why the game ended, from how the link failed with `error` and the
    /// last error the player printed.
    pub fn why_it_ended(&self, error: &io::Error) -> String {
        let said = self.last_error();
        match (error.kind(), said) {
            (io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock, said) => {
                // A player that shows an error waits for someone to close
                // it, unseen.
                self.stop();
                match said {
                    Some(said) => format!("The game stopped responding: {said}"),
                    None => "The game stopped responding".to_owned(),
                }
            }
            (
                io::ErrorKind::UnexpectedEof
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset,
                Some(said),
            ) => format!("The game stopped: {said}"),
            (
                io::ErrorKind::UnexpectedEof
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset,
                None,
            ) => "The game quit".to_owned(),
            (_, _) => format!("The link to the game failed: {error}"),
        }
    }

    /// The last line the player printed as an error, leaving out Ruby's
    /// warnings and the Win32 stand-ins' notes.
    fn last_error(&self) -> Option<String> {
        let errors = fs::read_to_string(&self.errors).ok()?;
        errors
            .lines()
            .map(str::trim)
            .rev()
            .find(|line| !line.is_empty() && !line.contains("warning:") && !line.starts_with('['))
            .map(str::to_owned)
    }

    /// Stops the player now.
    pub fn stop(&self) {
        match self.pid {
            Some(pid) => {
                Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status()
                    .ok();
            }
            None => {
                Command::new("pkill")
                    .arg("-KILL")
                    .arg("-f")
                    .arg(self.app.join("Contents/MacOS"))
                    .status()
                    .ok();
            }
        }
    }

    /// Stops the player if it has not quit by itself in a moment, on a
    /// thread of its own.
    pub fn stop_soon(&self) {
        let Some(pid) = self.pid else {
            self.stop();
            return;
        };
        thread::spawn(move || {
            thread::sleep(QUIT_GRACE);
            let pid = pid.to_string();
            let running = Command::new("kill")
                .args(["-0", &pid])
                .output()
                .is_ok_and(|output| output.status.success());
            if running {
                Command::new("kill").args(["-KILL", &pid]).status().ok();
            }
        });
    }
}

/// Makes `app` a hidden copy of the player app `source`, unless it already
/// is one of the player as it is now.
fn copy_player(source: &Path, app: &Path, id: &str) -> Result<(), Error> {
    let stamp_path = app.with_extension("source");
    let modified = fs::metadata(source.join("Contents/Info.plist"))
        .and_then(|metadata| metadata.modified())
        .map_err(Error::io(format!("reading {}", source.display())))?;
    // The changes are in the stamp, so a copy made with others is made again.
    let stamp = format!("{}\n{modified:?}\n{PLIST_CHANGES:?}\n", source.display());
    if app.is_dir() && fs::read_to_string(&stamp_path).ok().as_deref() == Some(&stamp) {
        return Ok(());
    }
    let copy = app.with_extension("new");
    fs::remove_dir_all(&copy).ok();
    // A clone takes no space on APFS; elsewhere, a copy.
    let cloned = run(Command::new("cp").arg("-cR").arg(source).arg(&copy));
    if cloned.is_err() {
        fs::remove_dir_all(&copy).ok();
        run(Command::new("cp").arg("-R").arg(source).arg(&copy))?;
    }
    let plist = copy.join("Contents/Info.plist");
    let identifier = format!("local.ds-emulator.player.{id}");
    let bundle_id = ("CFBundleIdentifier", "-string", identifier.as_str());
    for (key, kind, value) in PLIST_CHANGES.into_iter().chain([bundle_id]) {
        run(Command::new("plutil")
            .args(["-replace", key, kind, value])
            .arg(&plist))?;
    }
    fs::create_dir_all(copy.join("Contents/Game"))
        .map_err(Error::io(format!("making {}", copy.display())))?;
    fs::remove_dir_all(app).ok();
    fs::rename(&copy, app).map_err(Error::io(format!("making {}", app.display())))?;
    fs::write(&stamp_path, stamp).map_err(Error::io(format!("writing {}", stamp_path.display())))
}

/// Signs `app` for this Mac alone, as its contents changed.
fn sign(app: &Path) -> Result<(), Error> {
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app))
}

/// Runs `command`, failing with what it said if it fails.
fn run(command: &mut Command) -> Result<(), Error> {
    let what = format!("running {}", command.get_program().to_string_lossy());
    let output = command.output().map_err(Error::io(what.clone()))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(Error::Io {
        what,
        source: io::Error::other(said),
    })
}

/// Writes `text` to `path` unless it holds it already. Whether it wrote.
fn write_if_changed(path: &Path, text: &str) -> io::Result<bool> {
    if fs::read_to_string(path).ok().as_deref() == Some(text) {
        return Ok(false);
    }
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    fs::write(path, text)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player_with_errors(errors: &str) -> (PathBuf, Player) {
        let dir = crate::tests::temp_dir(&format!("errors-{}", errors.len()));
        fs::write(dir.join("player-errors.log"), errors).expect("log");
        let player = Player {
            app: dir.join("Player.app"),
            socket: dir.join("link.sock"),
            sound_pipe: dir.join("sound.fifo"),
            sound_settings: dir.join("alsoft.conf"),
            output: dir.join("player.log"),
            errors: dir.join("player-errors.log"),
            // Never signalled: these tests end without a timeout.
            pid: None,
            _lock: File::create(dir.join("player.lock")).expect("lock"),
        };
        (dir, player)
    }

    /// A game folder in `dir` whose player app is a stand-in, enough for
    /// [`Player::prepare`] to copy and sign.
    fn fake_game(dir: &Path) -> PathBuf {
        let game = dir.join("Game");
        let contents = game.join("Game.app/Contents");
        fs::create_dir_all(contents.join("MacOS")).expect("app");
        fs::write(
            contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleExecutable</key><string>game</string></dict></plist>
"#,
        )
        .expect("Info.plist");
        let executable = contents.join("MacOS/game");
        fs::write(&executable, "#!/bin/sh\n").expect("executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("mode");
        }
        game
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "Linux, Windows: the hidden player is a Mac app, changed with plutil and signed with codesign; needs mkxp-z's Linux and Windows players (RPG Maker stream)"
    )]
    fn a_game_has_one_player_at_a_time() {
        use std::sync::mpsc;

        let dir = crate::tests::temp_dir("one-player");
        let game = fake_game(&dir);
        let work = dir.join("work");
        let first = Player::prepare(&game, &work).expect("first player");
        let (prepared, second) = mpsc::channel();
        let waiting = {
            let (game, work) = (game.clone(), work.clone());
            thread::spawn(move || prepared.send(Player::prepare(&game, &work).is_ok()))
        };
        assert!(
            second.recv_timeout(Duration::from_millis(500)).is_err(),
            "the second player waits for the first"
        );
        drop(first);
        assert_eq!(second.recv_timeout(Duration::from_secs(10)), Ok(true));
        waiting.join().expect("thread").expect("sent");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_closed_link_is_a_quit_or_the_last_error_printed() {
        let (dir, player) = player_with_errors("");
        let closed = io::Error::from(io::ErrorKind::UnexpectedEof);
        assert_eq!(player.why_it_ended(&closed), "The game quit");
        let (other, player) = player_with_errors(
            "Scripts:35: warning: regular expression has ']' without escape\n[Win32API] [Kernel32.dll:LoadLibrary]\nScript 'Main' line 12: NoMethodError occurred.\n\n",
        );
        assert_eq!(
            player.why_it_ended(&closed),
            "The game stopped: Script 'Main' line 12: NoMethodError occurred."
        );
        for dir in [dir, other] {
            fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn settings_are_rewritten_only_when_they_change() {
        let dir = crate::tests::temp_dir("settings");
        let path = dir.join("Contents/Game/mkxp.json");
        assert!(write_if_changed(&path, "{}").expect("written"));
        assert!(!write_if_changed(&path, "{}").expect("kept"));
        assert!(write_if_changed(&path, "{\"a\": 1}").expect("rewritten"));
        fs::remove_dir_all(&dir).ok();
    }
}
