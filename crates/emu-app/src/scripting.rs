//! The Lua script running alongside a game (PRD UI-14), as the emulation
//! thread keeps it between frames: the script, the buttons it pressed for
//! the next frame, and how its log reaches the window without flooding it.

use std::time::{Duration, Instant};

use emu_core::Input;
use emu_script::{Joypad, Output, Script, ScriptError};

/// A script's log changes the notice at most this often, so a line
/// printed every frame stays readable.
const NOTICE_INTERVAL: Duration = Duration::from_millis(250);

/// A script and what it carries from one frame to the next.
pub struct Running {
    /// The script's file name, for notices.
    pub name: String,
    pub script: Script,
    /// Per player: what the script pressed and released for the frame
    /// under way.
    held: Vec<Joypad>,
    /// The same, asked for since, for the next frame.
    next: Vec<Joypad>,
    /// When its log last changed the notice.
    noticed: Option<Instant>,
    /// Its latest log line, not yet shown.
    unseen: Option<String>,
}

impl Running {
    pub fn new(name: String, script: Script) -> Self {
        Self {
            name,
            script,
            held: Vec::new(),
            next: Vec::new(),
            noticed: None,
            unseen: None,
        }
    }

    /// Starts a new frame with the buttons the script asked for last.
    /// A script started while a debugger holds a frame part-way presses
    /// nothing until the next.
    pub fn begin_frame(&mut self) {
        self.held.clone_from(&self.next);
    }

    /// `input`, what `player` holds, with the script's buttons for this
    /// frame laid over it.
    pub fn input(&self, player: usize, input: Input) -> Input {
        let pad = self.held.get(player).copied().unwrap_or_default();
        Input {
            buttons: pad.apply(input.buttons),
            ..input
        }
    }

    /// Keeps `output`'s buttons for the next frame, and prints its log
    /// lines to stdout. Returns the latest line as a notice, if one is due.
    pub fn take(&mut self, output: &Output) -> Option<String> {
        self.next.clone_from(&output.joypad);
        for line in &output.log {
            println!("{}: {line}", self.name);
        }
        if let Some(line) = output.log.last() {
            self.unseen = Some(format!("{}: {line}", self.name));
        }
        let due = self
            .noticed
            .is_none_or(|noticed| noticed.elapsed() >= NOTICE_INTERVAL);
        if due && self.unseen.is_some() {
            self.noticed = Some(Instant::now());
            return self.unseen.take();
        }
        None
    }
}

/// What to show when the script called `name` fails: its error, which
/// names the script and line where Lua knows them.
pub fn failure(name: &str, error: &ScriptError) -> String {
    let message = error.message();
    if message.starts_with(&format!("{name}:")) {
        format!("{message} · script stopped")
    } else {
        format!("{name}: {message} · script stopped")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> Running {
        Running::new(
            "test.lua".to_owned(),
            Script::new("test.lua", "").expect("compiles"),
        )
    }

    fn logging(lines: &[&str]) -> Output {
        Output {
            log: lines.iter().map(|line| (*line).to_owned()).collect(),
            ..Output::default()
        }
    }

    #[test]
    fn the_log_changes_the_notice_at_most_every_interval() {
        let mut running = running();
        assert_eq!(
            running.take(&logging(&["one", "two"])),
            Some("test.lua: two".to_owned())
        );
        assert_eq!(running.take(&logging(&["three"])), None, "too soon");
        assert_eq!(running.take(&Output::default()), None);
        running.noticed = Instant::now().checked_sub(NOTICE_INTERVAL);
        assert_eq!(
            running.take(&Output::default()),
            Some("test.lua: three".to_owned()),
            "shown later, though nothing new came"
        );
    }

    #[test]
    fn buttons_wait_for_the_next_frame() {
        let mut running = running();
        let a: emu_core::Buttons = [emu_core::Button::A].into_iter().collect();
        running.take(&Output {
            joypad: vec![Joypad {
                press: a,
                ..Joypad::default()
            }],
            ..Output::default()
        });
        assert_eq!(
            running.input(0, Input::default()).buttons,
            emu_core::Buttons::NONE
        );
        running.begin_frame();
        assert_eq!(running.input(0, Input::default()).buttons, a);
        running.take(&Output::default());
        running.begin_frame();
        assert_eq!(
            running.input(0, Input::default()).buttons,
            emu_core::Buttons::NONE
        );
    }

    #[test]
    fn failures_name_the_script_once() {
        let error = Script::new("hud.lua", "x = = 1")
            .err()
            .expect("a syntax error");
        assert_eq!(
            failure("hud.lua", &error),
            "hud.lua:1: unexpected symbol near '=' · script stopped"
        );
    }
}
