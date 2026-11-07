//! Running other programs, such as `curl` for downloads, the same way on
//! every system.
//!
//! On Windows the app is a windowed program, and Windows opens a console
//! window for each console program it starts, unless told not to. Every
//! program the app runs goes through [`command`], so none flashes one up.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Command;
use std::thread;

/// `CREATE_NO_WINDOW`, from Windows's `WinBase.h`: the program gets no
/// console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A [`Command`] for `program`, as [`Command::new`] makes one, that on
/// Windows opens no console window.
///
/// A bare name is found as [`Command::new`] finds it: on `PATH`, and on
/// Windows first beside the app and in the system folder, so `curl` is the
/// `curl.exe` Windows 10 and later ship in System32.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    hide_console(&mut command);
    command
}

/// Tells Windows to open no console window for `command`. Elsewhere it
/// does nothing.
pub fn hide_console(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Why `program` didn't start, in words for the user: that it isn't
/// installed, if `error` says it wasn't found, or else that it didn't
/// start, and why.
pub fn why_not_started(program: &str, error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        format!("{program} isn't installed")
    } else {
        format!("{program} didn't start: {error}")
    }
}

/// Opens the text file at `path` in a text editor, without waiting for it.
///
/// On macOS that is the default text editor (`open -t`). On Linux it is
/// the app the desktop gives the file's type (`xdg-open`), a text editor
/// for a text file. On Windows it is Notepad: Windows has no app for a
/// type it doesn't know, such as a `.cht` cheat list, and would ask which
/// to use.
///
/// # Errors
///
/// If the program that opens it can't start.
pub fn open_in_text_editor(path: &Path) -> io::Result<()> {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("/usr/bin/open", &["-t"])
    } else if cfg!(windows) {
        ("notepad.exe", &[])
    } else {
        ("xdg-open", &[])
    };
    let mut child = command(program)
        .args(args)
        .arg(path)
        .spawn()
        .map_err(|error| io::Error::new(error.kind(), why_not_started(program, &error)))?;
    // Waited for, so it leaves no zombie once it ends.
    thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_that_is_not_there_is_said_to_be_not_installed() {
        let program = "ds-emulator-no-such-program";
        let error = command(program).output().expect_err("not there");
        assert_eq!(
            why_not_started(program, &error),
            "ds-emulator-no-such-program isn't installed"
        );
        let other = io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied");
        assert_eq!(
            why_not_started("curl", &other),
            "curl didn't start: Permission denied"
        );
    }

    #[test]
    fn a_command_runs_its_program() {
        // The test binary itself, asked only to list its tests.
        let output = command(std::env::current_exe().expect("test binary"))
            .arg("--list")
            .output()
            .expect("runs");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("a_command_runs_its_program"));
    }
}
