//! Where the hidden player draws on Linux: an X display of its own.
//!
//! Linux has no hidden apps, and the mkxp-z builds have no SDL video driver
//! that draws without a window (`offscreen`). So the player draws on a
//! private Xvfb, a display in memory that no one sees, where its window
//! never shows and never takes the keyboard. Mesa draws there on the CPU,
//! which for RPG Maker's screens costs about what the GPU's driver does:
//! each of mkxp-z's draw calls has a fixed cost either way.
//!
//! Xvfb picks a free display number and says it (`-displayfd`), and only
//! clients with the cookie this writes (`-auth`) may connect, so no other
//! program can watch the game or type into it.
//!
//! Without Xvfb, the player draws on the user's own X display, through
//! XWayland on Wayland, and the bridge hides its window as it starts: it
//! shows for a moment. mkxp-z is always run on X, as its Wayland driver
//! has not been tried.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::Error;

/// How long Xvfb has to start.
const XVFB_TIMEOUT: Duration = Duration::from_secs(10);

/// The X display a player draws on.
#[derive(Debug)]
pub(crate) struct Display {
    /// Its name, as `DISPLAY` has it.
    name: String,
    /// A private display's server and cookie, or none for the user's.
    private: Option<(Child, PathBuf)>,
}

impl Display {
    /// A private display, with its files in `dir`, if Xvfb is installed;
    /// otherwise the user's.
    ///
    /// # Errors
    ///
    /// Fails if Xvfb fails to start, or there is neither Xvfb nor an X
    /// display.
    pub(crate) fn start(dir: &Path) -> Result<Self, Error> {
        match start_xvfb(dir) {
            Ok(display) => Ok(display),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match std::env::var("DISPLAY") {
                    Ok(name) if !name.is_empty() => Ok(Self {
                        name,
                        private: None,
                    }),
                    _ => Err(Error::NoDisplay),
                }
            }
            Err(error) => Err(Error::io("starting Xvfb, the player's display")(error)),
        }
    }

    /// Has `command`, the player, draw here: on X, hiding its window as it
    /// starts if this is the user's display.
    pub(crate) fn apply(&self, command: &mut Command) {
        command
            .env("DISPLAY", &self.name)
            .env("SDL_VIDEODRIVER", "x11")
            .env_remove("WAYLAND_DISPLAY");
        match &self.private {
            Some((_, cookie)) => command.env("XAUTHORITY", cookie),
            None => command.env("DS_EMULATOR_HIDE_WINDOW", "1"),
        };
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if let Some((xvfb, cookie)) = &mut self.private {
            xvfb.kill().ok();
            xvfb.wait().ok();
            fs::remove_file(cookie).ok();
        }
    }
}

/// Starts Xvfb with a new cookie, its files in `dir`.
///
/// # Errors
///
/// Fails with [`io::ErrorKind::NotFound`] if Xvfb is not installed, and
/// otherwise with what it said if it does not start.
fn start_xvfb(dir: &Path) -> io::Result<Display> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let cookie = dir.join("display.xauth");
    fs::remove_file(&cookie).ok();
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&cookie)
        .and_then(|mut file| io::Write::write_all(&mut file, &xauthority(&random)))?;
    let log_path = dir.join("display.log");
    let mut xvfb = Command::new("Xvfb")
        .args(["-displayfd", "1", "-nolisten", "tcp"])
        .args(["-screen", "0", "1024x768x24", "-auth"])
        .arg(&cookie)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(File::create(&log_path)?)
        .spawn()?;
    // Xvfb writes its display's number once it takes clients. The pipe is
    // read to the end, as Xvfb would stop if it wrote to a closed one.
    let (number, said) = mpsc::channel();
    if let Some(stdout) = xvfb.stdout.take() {
        thread::spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            number.send(lines.next()).ok();
            lines.for_each(drop);
        });
    }
    let number = said
        .recv_timeout(XVFB_TIMEOUT)
        .ok()
        .flatten()
        .and_then(Result::ok)
        .and_then(|line| line.trim().parse::<u32>().ok());
    match number {
        Some(number) => Ok(Display {
            name: format!(":{number}"),
            private: Some((xvfb, cookie)),
        }),
        None => {
            xvfb.kill().ok();
            xvfb.wait().ok();
            fs::remove_file(&cookie).ok();
            let log = fs::read_to_string(&log_path).unwrap_or_default();
            let said = log.lines().rev().find(|line| !line.trim().is_empty());
            Err(io::Error::other(match said {
                Some(said) => format!("Xvfb did not start: {}", said.trim()),
                None => "Xvfb did not start".to_owned(),
            }))
        }
    }
}

/// An `.Xauthority` file's one entry: `cookie`, an MIT-MAGIC-COOKIE-1, for
/// any host's every display, as the X server reads it with `-auth` and Xlib
/// with `XAUTHORITY`. Big-endian: the address family (`FamilyWild`), then
/// the address, display number, method name and cookie, each with its size.
fn xauthority(cookie: &[u8; 16]) -> Vec<u8> {
    const FAMILY_WILD: u16 = 0xFFFF;
    let mut entry = FAMILY_WILD.to_be_bytes().to_vec();
    for field in [&b""[..], b"", b"MIT-MAGIC-COOKIE-1", cookie] {
        let size = u16::try_from(field.len()).unwrap_or(u16::MAX);
        entry.extend(size.to_be_bytes());
        entry.extend(field);
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cookie_is_for_every_display_in_xauthoritys_format() {
        let cookie: [u8; 16] = std::array::from_fn(|i| i as u8);
        let mut expected = vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 18];
        expected.extend(b"MIT-MAGIC-COOKIE-1");
        expected.extend([0, 16]);
        expected.extend(cookie);
        assert_eq!(xauthority(&cookie), expected);
    }
}
