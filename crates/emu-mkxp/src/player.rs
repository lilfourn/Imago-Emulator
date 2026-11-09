//! The hidden player: a copy of the game's own player that runs the game
//! unseen.
//!
//! On macOS, mkxp-z takes `Contents/Game` inside its app, when there is one,
//! as the game's folder. The copy's holds only an `mkxp.json` (see
//! [`config`]): the game's own settings, pointed at the real folder, with
//! the bridge script loaded first. The game's folder stays as it is, and
//! the game keeps its saves where it always has.
//!
//! The copy is an agent app (`LSUIElement`), so it has no Dock icon or menu
//! bar, and opts out of App Nap, which would slow it once hidden. `open -j`
//! starts it hidden, and SDL is asked not to bring it to the front, so the
//! app keeps the keyboard.
//!
//! On Linux and Windows the player is mkxp-z's program itself: the one the
//! game ships, or the one put in the players' folder for every game, or
//! else the hosted one, downloaded there the first time (see
//! [`crate::SHARED_PLAYER`] and [`crate::install`]). It is started directly, with its `mkxp.json`
//! in a folder of the game's own: on Linux, mkxp-z reads it from `SRCDIR`;
//! on Windows, only from beside the program, so the program and its
//! libraries are copied there. On Linux it draws on a private X display,
//! Xvfb, when there is one, where nothing shows; otherwise on the user's,
//! where the bridge hides its window as it starts, as it does on Windows.
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
#[cfg(not(target_os = "macos"))]
use std::process::{Child, Stdio};
#[cfg(not(target_os = "macos"))]
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;
#[cfg(not(target_os = "macos"))]
use std::time::Instant;

#[cfg(target_os = "linux")]
use crate::display::Display;
use crate::{Error, config};

/// The script the player runs before the game's own, ds-emulator's end of
/// the link (see [`crate::link`]).
const BRIDGE: &str = include_str!("bridge.rb");
/// How long a player has to quit by itself once the link closes.
const QUIT_GRACE: Duration = Duration::from_secs(2);
/// What the copy's `Info.plist` changes, besides its bundle ID: no Dock
/// icon or menu bar, no App Nap, and a screen of the game's own size. On a
/// Retina screen mkxp-z's OpenGL drawing read back a quarter of the screen,
/// twice as large.
#[cfg(target_os = "macos")]
const PLIST_CHANGES: [(&str, &str, &str); 3] = [
    ("LSUIElement", "-bool", "true"),
    ("LSAppNapIsDisabled", "-bool", "true"),
    ("NSHighResolutionCapable", "-bool", "false"),
];

/// A game's hidden player.
#[derive(Debug)]
pub struct Player {
    /// What is started: on macOS the copy of the player app, elsewhere
    /// mkxp-z's program.
    app: PathBuf,
    /// On Linux, the folder of the player's own files: its settings, logs
    /// and display's.
    #[cfg(target_os = "linux")]
    dir: PathBuf,
    socket: PathBuf,
    sound_pipe: PathBuf,
    sound_settings: PathBuf,
    /// What the player prints, and what it prints as errors.
    output: PathBuf,
    errors: PathBuf,
    pid: Option<u32>,
    /// The player's process, and on Linux its display, once started.
    #[cfg(not(target_os = "macos"))]
    running: Mutex<Option<Running>>,
    /// Locked for as long as this is the game's player.
    _lock: File,
}

/// A player started directly, off macOS.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
struct Running {
    process: Child,
    /// Where it draws, which goes once it does.
    #[cfg(target_os = "linux")]
    _display: Display,
}

impl Player {
    /// Makes or brings up to date the hidden player of the game in `game`,
    /// in a folder of its own in `work_dir`, once the game has no other
    /// player: an earlier one is waited for until it is dropped.
    ///
    /// # Errors
    ///
    /// Fails if there is no player for the game, its settings cannot be
    /// read, or the copy cannot be made.
    pub fn prepare(game: &Path, work_dir: &Path) -> Result<Self, Error> {
        #[cfg(target_os = "macos")]
        let source = crate::player_app(game).ok_or_else(|| Error::NotAGame(game.to_owned()))?;
        let id = format!(
            "{:08X}",
            crc32fast::hash(game.as_os_str().as_encoded_bytes())
        );
        let dir = work_dir.join(&id);
        fs::create_dir_all(&dir).map_err(Error::io(format!("making {}", dir.display())))?;
        let lock_path = dir.join("player.lock");
        let lock = emu_os::lock::lock(&lock_path)
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
        #[cfg(target_os = "macos")]
        let app = {
            let app = dir.join("Player.app");
            copy_player(&source, &app, &id)?;
            let settings_path = app.join("Contents/Game/mkxp.json");
            if write_if_changed(&settings_path, &settings)
                .map_err(Error::io(format!("writing {}", settings_path.display())))?
            {
                sign(&app)?;
            }
            app
        };
        #[cfg(not(target_os = "macos"))]
        let app = {
            let source = player_program(game, &work_dir.join(crate::SHARED_PLAYER))?;
            let folder = dir.join("player");
            #[cfg(windows)]
            let app = copy_program(&source, &folder)?;
            #[cfg(not(windows))]
            let app = source;
            let settings_path = folder.join("mkxp.json");
            write_if_changed(&settings_path, &settings)
                .map_err(Error::io(format!("writing {}", settings_path.display())))?;
            app
        };
        // Sockets' paths are short, so these go in the temporary folder,
        // or on Linux in the session's own folder for such things, which
        // no other user can put a socket or a pipe of theirs in.
        let temp = emu_os::temp::runtime_dir()
            .map_err(Error::io("finding a folder for the game's link"))?;
        // Windows's pipes have names of their own, and this process's, as
        // two copies of the app each keep a player of their own.
        let sound_pipe = if cfg!(windows) {
            PathBuf::from(format!(r"\\.\pipe\ds-emulator-{id}-{}", std::process::id()))
        } else {
            temp.join(format!("ds-emulator-{id}.fifo"))
        };
        Ok(Self {
            app,
            socket: temp.join(format!("ds-emulator-{id}.sock")),
            sound_pipe,
            sound_settings: dir.join("alsoft.conf"),
            output: dir.join("player.log"),
            errors: dir.join("player-errors.log"),
            #[cfg(target_os = "linux")]
            dir,
            pid: None,
            #[cfg(not(target_os = "macos"))]
            running: Mutex::new(None),
            _lock: lock,
        })
    }

    /// Where the bridge connects, on macOS and Linux.
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

    /// Starts the player, hidden, with `link` in its environment: what the
    /// bridge needs to connect (see [`crate::link::Listener::environment`]).
    ///
    /// # Errors
    ///
    /// Fails if macOS cannot open it, or elsewhere, if it cannot be started
    /// or has nowhere to draw.
    #[cfg(target_os = "macos")]
    pub fn start(&mut self, link: &[(&str, String)]) -> Result<(), Error> {
        fs::remove_file(&self.output).ok();
        fs::remove_file(&self.errors).ok();
        let env = |name: &str, path: &Path| format!("{name}={}", path.display());
        let mut open = Command::new("/usr/bin/open");
        open.args(["-n", "-j", "-g", "--env", "SDL_MAC_BACKGROUND_APP=1"]);
        for (name, value) in link {
            open.arg("--env").arg(format!("{name}={value}"));
        }
        let output = open
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

    /// Starts the player, hidden, with `link` in its environment: what the
    /// bridge needs to connect (see [`crate::link::Listener::environment`]).
    ///
    /// # Errors
    ///
    /// Fails if macOS cannot open it, or elsewhere, if it cannot be started
    /// or has nowhere to draw.
    #[cfg(not(target_os = "macos"))]
    pub fn start(&mut self, link: &[(&str, String)]) -> Result<(), Error> {
        let log = |path: &Path| {
            fs::remove_file(path).ok();
            // Appended to, as the bridge on Windows opens the error log
            // again for Ruby's own errors.
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(Error::io(format!("making {}", path.display())))
        };
        let mut command = Command::new(&self.app);
        command
            .envs(link.iter().map(|(name, value)| (name, value)))
            .env("ALSOFT_CONF", &self.sound_settings)
            .stdin(Stdio::null())
            .stdout(log(&self.output)?)
            .stderr(log(&self.errors)?);
        #[cfg(target_os = "linux")]
        let display = {
            command.env("SRCDIR", self.dir.join("player"));
            let display = Display::start(&self.dir)?;
            display.apply(&mut command);
            display
        };
        #[cfg(windows)]
        {
            // Shown for a moment before the bridge hides it, the window
            // leaves the keyboard where it is. mkxp-z sends Ruby's errors
            // nowhere on Windows, so the bridge sends them to the log.
            command
                .env("SDL_WINDOW_NO_ACTIVATION_WHEN_SHOWN", "1")
                .env("DS_EMULATOR_HIDE_WINDOW", "1")
                .env("DS_EMULATOR_ERRORS", &self.errors);
        }
        let process = command
            .spawn()
            .map_err(Error::io(format!("starting {}", self.app.display())))?;
        #[cfg(windows)]
        windows_process::unthrottle(&process);
        self.pid = Some(process.id());
        *self.running.lock().unwrap_or_else(PoisonError::into_inner) = Some(Running {
            process,
            #[cfg(target_os = "linux")]
            _display: display,
        });
        Ok(())
    }

    /// Whether the player may still be running: on macOS, where `open`
    /// starts it apart, always.
    pub fn running(&self) -> bool {
        #[cfg(not(target_os = "macos"))]
        if let Some(running) = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            return matches!(running.process.try_wait(), Ok(None));
        }
        true
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
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted,
                Some(said),
            ) => format!("The game stopped: {said}"),
            (
                io::ErrorKind::UnexpectedEof
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted,
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
    #[cfg(target_os = "macos")]
    pub fn stop(&self) {
        match self.pid {
            Some(pid) => {
                Command::new("/bin/kill")
                    .args(["-KILL", &pid.to_string()])
                    .status()
                    .ok();
            }
            None => {
                Command::new("/usr/bin/pkill")
                    .arg("-KILL")
                    .arg("-f")
                    .arg(self.app.join("Contents/MacOS"))
                    .status()
                    .ok();
            }
        }
    }

    /// Stops the player now.
    #[cfg(not(target_os = "macos"))]
    pub fn stop(&self) {
        if let Some(mut running) = self.take_running() {
            running.process.kill().ok();
            running.process.wait().ok();
        }
    }

    /// Stops the player if it has not quit by itself in a moment, on a
    /// thread of its own.
    #[cfg(target_os = "macos")]
    pub fn stop_soon(&self) {
        let Some(pid) = self.pid else {
            self.stop();
            return;
        };
        thread::spawn(move || {
            thread::sleep(QUIT_GRACE);
            let pid = pid.to_string();
            let running = Command::new("/bin/kill")
                .args(["-0", &pid])
                .output()
                .is_ok_and(|output| output.status.success());
            if running {
                Command::new("/bin/kill")
                    .args(["-KILL", &pid])
                    .status()
                    .ok();
            }
        });
    }

    /// Stops the player if it has not quit by itself in a moment, on a
    /// thread of its own.
    #[cfg(not(target_os = "macos"))]
    pub fn stop_soon(&self) {
        let Some(mut running) = self.take_running() else {
            return;
        };
        thread::spawn(move || {
            let deadline = Instant::now() + QUIT_GRACE;
            while Instant::now() < deadline {
                match running.process.try_wait() {
                    Ok(None) => thread::sleep(Duration::from_millis(20)),
                    _ => return,
                }
            }
            running.process.kill().ok();
            running.process.wait().ok();
        });
    }

    /// The player's process, if it was started and not yet stopped.
    #[cfg(not(target_os = "macos"))]
    fn take_running(&self) -> Option<Running> {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// A player left running stops with it, off macOS, where it is this
/// process's child.
#[cfg(not(target_os = "macos"))]
impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Makes `app` a hidden copy of the player app `source`, unless it already
/// is one of the player as it is now.
#[cfg(target_os = "macos")]
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
    let cloned = run(Command::new("/bin/cp").arg("-cR").arg(source).arg(&copy));
    if cloned.is_err() {
        fs::remove_dir_all(&copy).ok();
        run(Command::new("/bin/cp").arg("-R").arg(source).arg(&copy))?;
    }
    let plist = copy.join("Contents/Info.plist");
    let identifier = format!("local.ds-emulator.player.{id}");
    let bundle_id = ("CFBundleIdentifier", "-string", identifier.as_str());
    for (key, kind, value) in PLIST_CHANGES.into_iter().chain([bundle_id]) {
        run(Command::new("/usr/bin/plutil")
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
#[cfg(target_os = "macos")]
fn sign(app: &Path) -> Result<(), Error> {
    run(Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(app))
}

/// The mkxp-z that plays the game in `game`, off macOS: the game's own,
/// then one the player put in `players`, the players' folder, then the
/// hosted one there, downloaded if it is not yet.
#[cfg(not(target_os = "macos"))]
pub(crate) fn player_program(game: &Path, players: &Path) -> Result<PathBuf, Error> {
    if let Some(program) = crate::mkxp_z(game).or_else(|| crate::mkxp_z_in(players)) {
        return Ok(program);
    }
    crate::install::ensure(players).map_err(|source| match source {
        crate::install::InstallError::NoBuild(_) => Error::NoPlayer(players.to_owned()),
        source => Error::Install {
            source,
            players: players.to_owned(),
        },
    })
}

/// What Windows does to the player's process, which it would otherwise
/// treat as a background program: it has no window in sight.
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_process {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::System::Threading::{
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
        ProcessPowerThrottling, SetProcessInformation,
    };

    /// Keeps Windows from throttling `player`, as the app keeps itself
    /// (emu-app's `qos.rs`): Windows 11 slows a process with no window in
    /// sight to its efficiency cores (EcoQoS) and stops honouring its timer
    /// period, which would hold back the sound's writes as Game Mode did on
    /// macOS. Each on its own, as Windows 10 knows only the first. The
    /// bridge raises the frame thread and asks for millisecond timers from
    /// inside, as only the player can.
    pub(super) fn unthrottle(player: &Child) {
        for control in [
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        ] {
            // A control bit with its state bit clear turns that throttling
            // off.
            let state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: control,
                StateMask: 0,
            };
            // SAFETY: the handle is the child's, open with all access while
            // `player` lives, and the pointer and size are those of `state`,
            // a PROCESS_POWER_THROTTLING_STATE, which the call only reads.
            // A refusal leaves the process as it was.
            unsafe {
                SetProcessInformation(
                    player.as_raw_handle(),
                    ProcessPowerThrottling,
                    (&raw const state).cast(),
                    size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                );
            }
        }
    }
}

/// Copies the program `source` and the libraries beside it into `folder`,
/// where its `mkxp.json` goes, unless they are already copies of them as
/// they are now. Hard links where they can be, as the program is large.
/// What it copied the program to.
#[cfg(windows)]
fn copy_program(source: &Path, folder: &Path) -> Result<PathBuf, Error> {
    let from = source.parent().unwrap_or(Path::new("."));
    let name = source.file_name().unwrap_or_default();
    let program = folder.join(name);
    let stamp_path = folder.join("player.source");
    let metadata =
        fs::metadata(source).map_err(Error::io(format!("reading {}", source.display())))?;
    let stamp = format!(
        "{}\n{:?}\n{}\n",
        source.display(),
        metadata.modified().ok(),
        metadata.len()
    );
    if program.is_file() && fs::read_to_string(&stamp_path).ok().as_deref() == Some(&stamp) {
        return Ok(program);
    }
    let copying = |what: &Path| Error::io(format!("copying {}", what.display()));
    fs::remove_dir_all(folder).ok();
    fs::create_dir_all(folder).map_err(Error::io(format!("making {}", folder.display())))?;
    let libraries = fs::read_dir(from)
        .map_err(copying(from))?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
                && path.is_file()
        });
    for file in [source.to_owned()].into_iter().chain(libraries) {
        let to = folder.join(file.file_name().unwrap_or_default());
        fs::hard_link(&file, &to)
            .or_else(|_| fs::copy(&file, &to).map(|_| ()))
            .map_err(copying(&file))?;
    }
    fs::write(&stamp_path, stamp)
        .map_err(Error::io(format!("writing {}", stamp_path.display())))?;
    Ok(program)
}

/// Runs `command`, one of macOS's own tools by its full path, failing with
/// what it said if it fails.
#[cfg(target_os = "macos")]
fn run(command: &mut Command) -> Result<(), Error> {
    // By its name alone, as it was always said: "running cp".
    let program = Path::new(command.get_program());
    let name = program.file_name().unwrap_or(program.as_os_str());
    let what = format!("running {}", name.to_string_lossy());
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
            #[cfg(target_os = "linux")]
            dir: dir.clone(),
            socket: dir.join("link.sock"),
            sound_pipe: dir.join("sound.fifo"),
            sound_settings: dir.join("alsoft.conf"),
            output: dir.join("player.log"),
            errors: dir.join("player-errors.log"),
            // Never signalled: these tests end without a timeout.
            pid: None,
            #[cfg(not(target_os = "macos"))]
            running: Mutex::new(None),
            _lock: File::create(dir.join("player.lock")).expect("lock"),
        };
        (dir, player)
    }

    /// A game folder in `dir` whose player app is a stand-in, enough for
    /// [`Player::prepare`] to copy and sign.
    #[cfg(target_os = "macos")]
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

    /// A game folder in `dir` with a stand-in for mkxp-z, enough for
    /// [`Player::prepare`] to find and, on Windows, copy.
    #[cfg(not(target_os = "macos"))]
    fn fake_game(dir: &Path) -> PathBuf {
        let game = dir.join("Game");
        fs::create_dir_all(&game).expect("game");
        crate::tests::fake_mkxp_z(&game);
        game
    }

    #[test]
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
    fn a_game_s_player_lock_is_opened_as_it_is_not_emptied() {
        let dir = crate::tests::temp_dir("player-lock");
        let game = fake_game(&dir);
        let work = dir.join("work");
        let id = format!(
            "{:08X}",
            crc32fast::hash(game.as_os_str().as_encoded_bytes())
        );
        let lock = work.join(&id).join("player.lock");
        fs::create_dir_all(work.join(&id)).expect("the game's folder");
        // Windows may refuse to empty a file another process holds locked,
        // so a player must not: what is in the file shows whether it did.
        fs::write(&lock, b"held by another player").expect("lock");
        let player = Player::prepare(&game, &work).expect("player");
        assert_eq!(fs::read(&lock).expect("lock"), b"held by another player");
        drop(player);
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
