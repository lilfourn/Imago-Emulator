//! What the player holds, from the keyboard, gamepads and the mouse, and
//! what of it the emulation thread has been told.
//!
//! [`GameInput`] keeps the bindings in force, every input held and where
//! each key went when pressed, the gamepad's hotkey chords, its touch
//! cursor and the mouse's stylus. The window hands it each key, gamepad
//! and mouse event, with what it needs to know of the game ([`Stage`]),
//! and gets back the [`Command`]s that tell the emulation thread what
//! changed, in the order to send them. It knows nothing of the window, so
//! what it does with a key held across a menu, or let go of while the
//! window was behind another, is tested here without one.
//!
//! The rule it keeps: the game holds an input from its press to its
//! release, and when presses and releases stop reaching the game while
//! inputs may still be down (the game menu opens, the window loses focus,
//! ⌘ goes down), everything is let go of at once. An input still down
//! afterwards counts again only once pressed again.

use emu_core::{Button, Input, ScreenSize, TouchPoint};
use gpui::Modifiers;

use crate::bindings::{self, Bindings, Held, Shortcut};
use crate::chords::{Chords, Effect};
use crate::controls::{Controls, HeldState};
use crate::cursor::{self, Cursor};
use crate::emulator::Command;
use crate::key_routes::{KeyRoutes, Route};
use crate::layout::Rotation;
use crate::speed::{Pace, Speeds};
use crate::trace;

/// The source name under which a held mouse button presses A, off the
/// touchscreen.
const MOUSE: &str = "mouse:left";

/// What every keyboard key's source name starts with, before the key's
/// GPUI name.
const KEY: &str = "key:";

/// Shift's name in the bindings: held on its own, it comes only as a
/// change of modifiers.
const SHIFT: &str = "shift";

/// What the input needs to know of the game being played and of the
/// settings, as they are when an event comes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Stage {
    /// The touchscreen of the console being played, if it has one: its
    /// index among the screens and its size.
    pub(super) touchscreen: Option<(usize, ScreenSize)>,
    /// How fast fast-forward and slow motion run.
    pub(super) speeds: Speeds,
}

/// What a key going down came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pressed {
    /// A repeat of a key that was pressed for something else: it goes
    /// nowhere.
    Elsewhere,
    /// Typed into Guide's composer, which has it already.
    Guide,
    /// The library's, to search and choose with; `repeat` if the key was
    /// down already.
    Library {
        /// Whether the key was down already.
        repeat: bool,
    },
    /// The open game menu's; `repeat` if the key was down already.
    Menu {
        /// Whether the key was down already.
        repeat: bool,
    },
    /// Escape, in the game: it opens the game menu.
    OpensMenu,
    /// The game's: what to tell the emulation thread, which is nothing for
    /// a repeat, a shortcut's key or a key bound to nothing.
    Game(Vec<Command>),
}

/// Everything held for the game, and what the emulation thread was last
/// told of it.
#[derive(Debug, Default)]
pub(super) struct GameInput {
    /// The bindings in force: the user's, with the active player's game's
    /// own laid over them.
    bindings: Bindings,
    controls: Controls,
    /// Gamepad hotkey chords in progress.
    chords: Chords,
    /// Where each key held down went when pressed, which its repeats go
    /// to as well.
    key_routes: KeyRoutes,
    /// The gamepad's touchscreen cursor, once a gamepad has used it.
    cursor: Option<Cursor>,
    /// What the emulation thread was last told is held.
    sent: HeldState,
    /// Where the emulation thread was last told the screen is touched.
    sent_touch: Option<TouchPoint>,
    /// While the mouse button is held down after a click on the
    /// touchscreen: which of the window's placements was clicked, and
    /// where the mouse touches.
    stylus: Option<(usize, TouchPoint)>,
    /// The player keyboard and gamepads control, in multi-player sessions.
    player: usize,
    /// How loud the microphone was when the emulation thread was last
    /// told.
    heard: u8,
}

impl GameInput {
    /// The bindings in force.
    pub(super) fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// Puts `bindings` in force, for inputs pressed from now on.
    pub(super) fn set_bindings(&mut self, bindings: Bindings) {
        self.bindings = bindings;
    }

    /// The player keyboard and gamepads control.
    pub(super) fn player(&self) -> usize {
        self.player
    }

    /// The gamepad's touchscreen cursor, once a gamepad has used it.
    pub(super) fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    /// Whether the emulation thread was last told the gamepad cursor
    /// touches the screen.
    pub(super) fn touching(&self) -> bool {
        self.sent.touch
    }

    /// The pace the emulation thread was last told to run at.
    pub(super) fn pace(&self) -> Pace {
        self.sent.pace
    }

    /// While the mouse button is held down after a click on the
    /// touchscreen: which placement was clicked, and where the mouse
    /// touches.
    pub(super) fn stylus(&self) -> Option<(usize, TouchPoint)> {
        self.stylus
    }

    /// Whether fast-forward is latched on.
    pub(super) fn fast_forward_latched(&self) -> bool {
        self.controls.fast_forward_latched()
    }

    /// Whether slow motion is latched on.
    pub(super) fn slow_motion_latched(&self) -> bool {
        self.controls.slow_motion_latched()
    }

    /// What the window shows of the gamepad cursor: where it is, and
    /// whether it touches. Held inputs show nowhere else, so input events
    /// redraw only when this changes; a redraw holds up the events behind
    /// it.
    pub(super) fn cursor_look(&self) -> (Option<Cursor>, bool) {
        (self.cursor, self.sent.touch)
    }

    /// Latches fast-forward and slow motion on if they were left on last
    /// time, as the window opens: the first game is told as it starts.
    pub(super) fn latch(&mut self, fast_forward: bool, slow_motion: bool) {
        if fast_forward != self.controls.fast_forward_latched() {
            self.controls.toggle_fast_forward();
        }
        if slow_motion != self.controls.slow_motion_latched() {
            self.controls.toggle_slow_motion();
        }
    }

    /// A new game has started, which holds nothing yet and is played by
    /// its first player: forgets what the last game was told, so that
    /// [`GameInput::resend`] tells this one all that is held.
    pub(super) fn game_started(&mut self) {
        self.sent = HeldState::default();
        self.sent_touch = None;
        self.stylus = None;
        self.cursor = None;
        self.player = 0;
    }

    /// Tells the emulation thread whatever is held that it has not been
    /// told of.
    pub(super) fn resend(&mut self, stage: &Stage) -> Vec<Command> {
        let mut commands = Vec::new();
        self.sync(stage, &mut commands);
        commands
    }

    /// A key going down, or repeating if `held` or if it was down
    /// already, while keys go to `route`, with the game menu open over the
    /// game if `game_menu`. A repeat goes only where the press went:
    /// Return held to choose in the menu or the library never reaches what
    /// comes after, and W held to walk never types into Guide.
    pub(super) fn key_down(
        &mut self,
        key: &str,
        modifiers: Modifiers,
        held: bool,
        route: Route,
        game_menu: bool,
        stage: &Stage,
    ) -> Pressed {
        // A key held down repeats. GPUI passes on the repeats of keys that
        // type nothing (the arrows, Escape, Backspace) as fresh presses, so
        // a key already down is repeating whatever the event says.
        let pressed_to = self.key_routes.route_of(key);
        let repeat = held || pressed_to.is_some();
        if !repeat {
            // ⌘ keys are shortcuts, and macOS may never say they came up.
            if !modifiers.platform {
                self.key_routes.press(key, route);
            }
        } else if pressed_to != Some(route) {
            return Pressed::Elsewhere;
        }
        // Typing in Guide's composer: the composer's own, never the game's.
        if route == Route::Guide {
            return Pressed::Guide;
        }
        // Without a game, keys search and choose in the library.
        if route == Route::Library {
            return Pressed::Library { repeat };
        }
        // While the game menu is open, keys are the menu's.
        if game_menu {
            return Pressed::Menu { repeat };
        }
        // The game holds a key from its press to its release; repeats add
        // nothing. A key still down when the game stopped hearing it, as
        // the menu opened, counts again once pressed again.
        if repeat {
            return Pressed::Game(Vec::new());
        }
        let plain = !modifiers.platform && !modifiers.control && !modifiers.alt;
        if key == "escape" && plain {
            return Pressed::OpensMenu;
        }
        let mut commands = Vec::new();
        // Command and Control chords are menu shortcuts, not game input.
        if !modifiers.platform && !modifiers.control {
            let action = self.bindings.key_action(key);
            self.controls.set(format!("{KEY}{key}"), action, true);
            self.sync(stage, &mut commands);
        }
        Pressed::Game(commands)
    }

    /// A key coming up.
    pub(super) fn key_up(&mut self, key: &str, stage: &Stage) -> Vec<Command> {
        // Shift going down or up while a key is down renames some keys: `1`
        // held as Shift goes down comes up as `!`. A key that went down
        // under this name is the one coming up, though, such as a keypad
        // `1`, which Shift leaves alone.
        let name = match bindings::shift_twin(key) {
            Some(twin)
                if self.key_routes.route_of(key).is_none()
                    && self.key_routes.route_of(twin).is_some() =>
            {
                twin
            }
            _ => key,
        };
        self.key_routes.release(name);
        self.controls.set(format!("{KEY}{name}"), None, false);
        let mut commands = Vec::new();
        self.sync(stage, &mut commands);
        commands
    }

    /// Shift going down for the game (`down`) or coming up: holds or lets
    /// go of whatever Shift is bound to, as it comes only as a change of
    /// modifiers, never as a key.
    pub(super) fn shift(&mut self, down: bool, stage: &Stage) -> Vec<Command> {
        let action = down.then(|| self.bindings.key_action(SHIFT)).flatten();
        self.controls.set(format!("{KEY}{SHIFT}"), action, down);
        let mut commands = Vec::new();
        self.sync(stage, &mut commands);
        commands
    }

    /// Lets go of every key held for the game, and forgets where each
    /// went: for when ⌘ goes down, as macOS sends no key-up for a key let
    /// go of while ⌘ is down, which would leave it held. A key still down
    /// counts again once pressed again.
    pub(super) fn release_keys(&mut self, stage: &Stage) -> Vec<Command> {
        self.key_routes.clear();
        self.controls.release_from(KEY);
        let mut commands = Vec::new();
        self.sync(stage, &mut commands);
        commands
    }

    /// `control` on `controller` going down or up. Returns what to tell
    /// the emulation thread, and the hotkeys' commands to run once that is
    /// sent (see `chords.rs`).
    pub(super) fn pad_control(
        &mut self,
        controller: &str,
        control: &str,
        pressed: bool,
        stage: &Stage,
    ) -> (Vec<Command>, Vec<Shortcut>) {
        let hotkey = |control: &str| self.bindings.hotkey(control);
        let effects = self.chords.event(hotkey, controller, control, pressed);
        let mut commands = Vec::new();
        let mut shortcuts = Vec::new();
        for effect in effects {
            match effect {
                Effect::Pass { control, pressed } => {
                    let action = self.bindings.pad_action(controller, &control);
                    let source = format!("pad:{controller}:{control}");
                    self.controls.set(source, action, pressed);
                    self.sync(stage, &mut commands);
                }
                Effect::Hold { control, held } => {
                    let source = format!("pad:{controller}:{control}");
                    self.controls.set(source, Some(held), true);
                    self.sync(stage, &mut commands);
                }
                Effect::Run(shortcut) => shortcuts.push(shortcut),
            }
        }
        (commands, shortcuts)
    }

    /// Moves the gamepad cursor by `motion`, in the units of
    /// [`PadEvent::Pointer`](crate::gamepad::PadEvent::Pointer) as seen on
    /// the window, whichever way `rotation` turns the screens.
    pub(super) fn move_cursor(
        &mut self,
        motion: (f32, f32),
        rotation: Rotation,
        stage: &Stage,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        let Some((_, size)) = stage.touchscreen else {
            return commands;
        };
        let (dx, dy) = rotation.unturn_vector(motion);
        self.cursor
            .get_or_insert_with(|| Cursor::centre(size))
            .move_by((dx * cursor::SPEED, dy * cursor::SPEED), size);
        self.sync(stage, &mut commands);
        commands
    }

    /// Turns latched fast-forward on or off. Returns whether it is now on.
    pub(super) fn toggle_fast_forward(&mut self, stage: &Stage) -> (bool, Vec<Command>) {
        let on = self.controls.toggle_fast_forward();
        let mut commands = Vec::new();
        self.sync(stage, &mut commands);
        (on, commands)
    }

    /// Turns latched slow motion on or off. Returns whether it is now on.
    pub(super) fn toggle_slow_motion(&mut self, stage: &Stage) -> (bool, Vec<Command>) {
        let on = self.controls.toggle_slow_motion();
        let mut commands = Vec::new();
        self.sync(stage, &mut commands);
        (on, commands)
    }

    /// The mouse button going down: on the touchscreen at `touched`, a
    /// placement of the window's and the pixel in it, where it is the
    /// stylus touching there; or elsewhere, where it presses A for as long
    /// as it is held. Back to the mouse, the gamepad cursor hides unless
    /// it is touching.
    pub(super) fn mouse_down(
        &mut self,
        touched: Option<(usize, TouchPoint)>,
        stage: &Stage,
    ) -> Vec<Command> {
        if !self.sent.touch {
            self.cursor = None;
        }
        if touched.is_some() {
            self.stylus = touched;
        } else {
            self.controls
                .set(MOUSE.to_owned(), Some(Held::Button(Button::A)), true);
        }
        let mut commands = Vec::new();
        self.sync(stage, &mut commands);
        commands
    }

    /// The mouse moving while the stylus touches: to `moved` on the
    /// touchscreen, or off it if the screen pressed on is gone from the
    /// window (`None`), which lifts the stylus.
    pub(super) fn drag_stylus(&mut self, moved: Option<TouchPoint>, stage: &Stage) -> Vec<Command> {
        let mut commands = Vec::new();
        let Some((index, at)) = self.stylus else {
            return commands;
        };
        match moved {
            None => {
                self.stylus = None;
                self.sync(stage, &mut commands);
            }
            Some(moved) if moved != at => {
                self.stylus = Some((index, moved));
                self.sync(stage, &mut commands);
            }
            Some(_) => {}
        }
        commands
    }

    /// The mouse button coming up, in the window or out of it: lifts the
    /// stylus or releases A.
    pub(super) fn mouse_up(&mut self, stage: &Stage) -> Vec<Command> {
        self.stylus = None;
        self.controls.set(MOUSE.to_owned(), None, false);
        let mut commands = Vec::new();
        self.sync(stage, &mut commands);
        commands
    }

    /// The microphone hearing `level`: tells the emulation thread, if that
    /// is a change.
    pub(super) fn hear(&mut self, level: u8) -> Option<Command> {
        (level != self.heard).then(|| {
            self.heard = level;
            self.input_command()
        })
    }

    /// The microphone going off: the game hears nothing from now on.
    pub(super) fn stop_hearing(&mut self) -> Command {
        self.heard = 0;
        self.input_command()
    }

    /// Hands keyboard and gamepad control to `player`, with that player's
    /// game's `bindings` in force if it has any. The previous player lets
    /// go of everything at once, and the new one starts with nothing held:
    /// what is down now was pressed under the previous player's bindings,
    /// so it counts again only once pressed again.
    pub(super) fn switch_player(
        &mut self,
        player: usize,
        bindings: Option<Bindings>,
        stage: &Stage,
    ) -> Vec<Command> {
        let mut commands = Vec::new();
        // The mouse touches the screen it was pressed on, not the next
        // player's, so it lets go too.
        self.release_player_into(stage, &mut commands);
        self.player = player;
        // Keyboard and gamepads now play the other player's game.
        if let Some(bindings) = bindings {
            self.bindings = bindings;
        }
        commands.push(self.input_command());
        commands.push(self.turbo_command());
        commands
    }

    /// Lets go of everything held, at once: for when keys and buttons stop
    /// reaching the game while they may still be down, as when the game
    /// menu opens. A key still down counts again only once pressed again,
    /// as its repeats are not presses.
    pub(super) fn release_all(&mut self, stage: &Stage) -> Vec<Command> {
        let mut commands = Vec::new();
        self.release_player_into(stage, &mut commands);
        // The microphone goes on being heard.
        commands.push(self.input_command());
        commands
    }

    /// The window losing focus: lets go of everything, as
    /// [`GameInput::release_all`] does, and forgets every key down. Keys
    /// released while another window has focus never send key-up, and
    /// keys still down when focus comes back count once pressed again
    /// (the arrows may count from their next repeat, which GPUI passes on
    /// as a press).
    pub(super) fn focus_lost(&mut self, stage: &Stage) -> Vec<Command> {
        self.key_routes.clear();
        self.release_all(stage)
    }

    /// Forgets everything held, and has the active player let go of it in
    /// the game at once, microphone and all: quick taps and turbo presses
    /// are cut short rather than held for the game to see.
    fn release_player_into(&mut self, stage: &Stage, commands: &mut Vec<Command>) {
        self.stylus = None;
        self.controls.release_all();
        self.chords.clear();
        self.sync(stage, commands);
        commands.push(Command::ReleaseInput {
            player: self.player,
        });
    }

    /// Adds to `commands` what tells the emulation thread about any change
    /// in held inputs or in where the mouse or the gamepad cursor touches
    /// the screen. The mouse comes first.
    fn sync(&mut self, stage: &Stage, commands: &mut Vec<Command>) {
        let state = self.controls.state();
        let touch = match self.stylus {
            Some((_, at)) => Some(at),
            None => self.cursor_touch(state.touch, stage),
        };
        if state.buttons != self.sent.buttons
            || touch != self.sent_touch
            || state.blow != self.sent.blow
            || state.gentle != self.sent.gentle
        {
            self.sent.buttons = state.buttons;
            self.sent.blow = state.blow;
            self.sent.gentle = state.gentle;
            self.sent_touch = touch;
            commands.push(self.input_command());
        }
        if state.turbo != self.sent.turbo {
            self.sent.turbo = state.turbo;
            commands.push(self.turbo_command());
        }
        if state.pace != self.sent.pace {
            commands.push(Command::SetSpeed(stage.speeds.speed(state.pace)));
        }
        if state.rewind != self.sent.rewind {
            commands.push(Command::SetRewind(state.rewind));
        }
        self.sent = state;
    }

    /// Where the gamepad cursor touches, if its touch input is `held`. The
    /// cursor appears in the middle of the touchscreen if no gamepad has
    /// moved it yet.
    fn cursor_touch(&mut self, held: bool, stage: &Stage) -> Option<TouchPoint> {
        if !held {
            return None;
        }
        let (_, size) = stage.touchscreen?;
        Some(
            self.cursor
                .get_or_insert_with(|| Cursor::centre(size))
                .pixel(),
        )
    }

    /// What tells the emulation thread the active player's input as it was
    /// last sent, with the microphone as it is heard now.
    fn input_command(&self) -> Command {
        trace::event("send", &[u64::from(self.sent.buttons.bits())]);
        Command::SetInput {
            player: self.player,
            input: Input {
                buttons: self.sent.buttons,
                touch: self.sent_touch,
                microphone: if self.sent.blow {
                    Input::LOUDEST
                } else {
                    self.heard
                },
                gentle: self.sent.gentle,
            },
        }
    }

    /// What tells the emulation thread the active player's turbo buttons
    /// as they were last sent.
    fn turbo_command(&self) -> Command {
        Command::SetTurbo {
            player: self.player,
            buttons: self.sent.turbo,
            frames: self.bindings.turbo_frames(),
        }
    }
}
