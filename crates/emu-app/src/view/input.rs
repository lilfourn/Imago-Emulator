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

#[cfg(test)]
mod tests {
    use emu_core::Buttons;

    use super::*;
    use crate::speed::Speed;
    use crate::turbo;

    /// A controller, by its UUID, and another.
    const PAD: &str = "030000005e0400008e02000000007200";
    const OTHER_PAD: &str = "050000004c050000c405000000783f00";

    /// A DS game: its touchscreen is the second screen.
    fn ds() -> Stage {
        Stage {
            touchscreen: Some((1, ScreenSize::new(256, 192))),
            speeds: Speeds::default(),
        }
    }

    /// A game with no touchscreen.
    fn gba() -> Stage {
        Stage {
            touchscreen: None,
            ..ds()
        }
    }

    /// The first player told it holds `input`.
    fn told(input: Input) -> Command {
        Command::SetInput { player: 0, input }
    }

    /// The first player told it holds `buttons` and nothing else.
    fn holds(buttons: &[Button]) -> Command {
        told(Input {
            buttons: buttons.iter().copied().collect(),
            ..Input::default()
        })
    }

    /// The first player told the screen is touched at (`x`, `y`).
    fn touches(x: u16, y: u16) -> Command {
        told(Input {
            touch: Some(TouchPoint { x, y }),
            ..Input::default()
        })
    }

    /// The first player told its turbo `buttons`, at the default pace.
    fn turbo(buttons: &[Button]) -> Command {
        Command::SetTurbo {
            player: 0,
            buttons: buttons.iter().copied().collect(),
            frames: turbo::DEFAULT_FRAMES,
        }
    }

    /// What letting go of everything tells a first player who held
    /// buttons: the buttons gone, the release at once, and the input once
    /// more for the microphone.
    fn let_go() -> Vec<Command> {
        vec![holds(&[]), Command::ReleaseInput { player: 0 }, holds(&[])]
    }

    /// A key pressed afresh in the game, with nothing else held down.
    fn press(input: &mut GameInput, key: &str) -> Pressed {
        input.key_down(key, Modifiers::default(), false, Route::Game, false, &ds())
    }

    /// A key down again as it repeats, as GPUI says of keys that type:
    /// `route` and `game_menu` are where keys go by then.
    fn repeat(input: &mut GameInput, key: &str, route: Route, game_menu: bool) -> Pressed {
        input.key_down(key, Modifiers::default(), true, route, game_menu, &ds())
    }

    #[test]
    fn a_key_held_across_the_game_menu_counts_again_only_once_pressed_again() {
        let mut input = GameInput::default();
        assert_eq!(
            press(&mut input, "w"),
            Pressed::Game(vec![holds(&[Button::Up])])
        );
        // Escape opens the menu, which lets go of everything at once.
        assert_eq!(press(&mut input, "escape"), Pressed::OpensMenu);
        assert_eq!(input.release_all(&ds()), let_go());

        // W is still down and repeats: it is not the menu's.
        assert_eq!(
            repeat(&mut input, "w", Route::Menu, true),
            Pressed::Elsewhere
        );
        // Nor is Escape's own repeat, which GPUI passes on as a press: the
        // menu does not close under the key that opened it.
        let escape_again = |input: &mut GameInput, route, game_menu| {
            input.key_down(
                "escape",
                Modifiers::default(),
                false,
                route,
                game_menu,
                &ds(),
            )
        };
        assert_eq!(
            escape_again(&mut input, Route::Menu, true),
            Pressed::Elsewhere
        );
        assert_eq!(input.key_up("escape", &ds()), []);
        // Pressed again, it is the menu's, which closes; its repeats then
        // do not open the menu again.
        assert_eq!(
            escape_again(&mut input, Route::Menu, true),
            Pressed::Menu { repeat: false }
        );
        assert_eq!(
            escape_again(&mut input, Route::Game, false),
            Pressed::Elsewhere
        );
        assert_eq!(input.key_up("escape", &ds()), []);

        // Back in the game with W down all along: its repeats add
        // nothing, and the game is told nothing until it is pressed again.
        assert_eq!(
            repeat(&mut input, "w", Route::Game, false),
            Pressed::Game(Vec::new())
        );
        assert_eq!(input.key_up("w", &ds()), []);
        assert_eq!(
            press(&mut input, "w"),
            Pressed::Game(vec![holds(&[Button::Up])])
        );
    }

    #[test]
    fn a_key_held_to_choose_goes_only_where_its_press_went() {
        let mut input = GameInput::default();
        // Return chooses a game in the library, and is still down as the
        // game starts: it never presses A there.
        let enter = |input: &mut GameInput, held, route| {
            input.key_down("enter", Modifiers::default(), held, route, false, &gba())
        };
        assert_eq!(
            enter(&mut input, false, Route::Library),
            Pressed::Library { repeat: false }
        );
        assert_eq!(
            enter(&mut input, true, Route::Library),
            Pressed::Library { repeat: true }
        );
        assert_eq!(enter(&mut input, true, Route::Game), Pressed::Elsewhere);
        assert_eq!(input.key_up("enter", &gba()), []);
        assert_eq!(
            enter(&mut input, false, Route::Game),
            Pressed::Game(vec![holds(&[Button::A])])
        );
        // W held to walk never types into Guide once its composer has the
        // caret, and what is typed there is not the game's.
        assert_eq!(
            press(&mut input, "w"),
            Pressed::Game(vec![holds(&[Button::A, Button::Up])])
        );
        assert_eq!(
            repeat(&mut input, "w", Route::Guide, false),
            Pressed::Elsewhere
        );
        assert_eq!(
            input.key_down(
                "j",
                Modifiers::default(),
                false,
                Route::Guide,
                false,
                &gba()
            ),
            Pressed::Guide
        );
    }

    #[test]
    fn focus_lost_with_keys_down_lets_go_of_them_and_forgets_them() {
        let mut input = GameInput::default();
        press(&mut input, "w");
        press(&mut input, "k");
        assert_eq!(
            press(&mut input, "left"),
            Pressed::Game(vec![holds(&[Button::Up, Button::A, Button::Left])])
        );
        // Another window comes to the front.
        assert_eq!(input.focus_lost(&ds()), let_go());

        // W was let go of meanwhile, and no key-up came. Pressed again, it
        // is a press, not a repeat of the one that was never let go of.
        assert_eq!(
            press(&mut input, "w"),
            Pressed::Game(vec![holds(&[Button::Up])])
        );
        // K is still down as focus comes back: its repeats are not
        // presses, so it counts once pressed again.
        assert_eq!(
            repeat(&mut input, "k", Route::Game, false),
            Pressed::Elsewhere
        );
        assert_eq!(input.key_up("k", &ds()), []);
        assert_eq!(
            press(&mut input, "k"),
            Pressed::Game(vec![holds(&[Button::Up, Button::A])])
        );
        // An arrow still down counts from its next repeat, which GPUI
        // passes on as a press.
        assert_eq!(
            press(&mut input, "left"),
            Pressed::Game(vec![holds(&[Button::Up, Button::A, Button::Left])])
        );
    }

    #[test]
    fn the_command_key_going_down_lets_go_of_every_key_but_nothing_else() {
        let mut input = GameInput::default();
        press(&mut input, "w");
        assert_eq!(
            input.pad_control(PAD, "East", true, &ds()).0,
            [holds(&[Button::Up, Button::A])]
        );
        // ⌘ goes down: macOS may never say W came up. The gamepad's A
        // stays held.
        assert_eq!(input.release_keys(&ds()), [holds(&[Button::A])]);
        assert_eq!(
            repeat(&mut input, "w", Route::Game, false),
            Pressed::Elsewhere
        );
        // A ⌘ shortcut's key is no game input, and is not taken for down:
        // S pressed after ⌘S is a press.
        let command = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert_eq!(
            input.key_down("s", command, false, Route::Game, false, &ds()),
            Pressed::Game(Vec::new())
        );
        assert_eq!(
            press(&mut input, "s"),
            Pressed::Game(vec![holds(&[Button::A, Button::Down])])
        );
        // Nor is a Control chord's key, nor Escape with ⌘ the menu's key.
        let control = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(
            input.key_down("k", control, false, Route::Game, false, &ds()),
            Pressed::Game(Vec::new())
        );
        assert_eq!(
            input.key_down("escape", command, false, Route::Game, false, &ds()),
            Pressed::Game(Vec::new())
        );
    }

    #[test]
    fn shift_holds_what_it_is_bound_to_and_a_key_it_renames_comes_up_as_itself() {
        let mut input = GameInput::default();
        let slow = Command::SetSpeed(Speeds::default().speed(Pace::SlowMotion));
        // `1` runs slow motion while held.
        assert_eq!(press(&mut input, "1"), Pressed::Game(vec![slow]));
        // Shift, which tilts gently, goes down for the game.
        let gently = told(Input {
            gentle: true,
            ..Input::default()
        });
        assert_eq!(input.shift(true, &ds()), [gently]);
        // `1` comes up under the name Shift gives it.
        assert_eq!(input.key_up("!", &ds()), [Command::SetSpeed(Speed::NORMAL)]);
        assert_eq!(input.shift(false, &ds()), [holds(&[])]);
        // Shift down while keys go elsewhere holds nothing.
        assert_eq!(input.shift(false, &ds()), []);
    }

    #[test]
    fn a_gamepad_unplugged_with_buttons_down_lets_go_of_them() {
        let mut input = GameInput::default();
        let fast = Command::SetSpeed(Speeds::default().speed(Pace::FastForward));
        assert_eq!(
            input.pad_control(PAD, "East", true, &ds()),
            (vec![holds(&[Button::A])], Vec::new())
        );
        assert_eq!(
            input.pad_control(PAD, "RightTrigger2", true, &ds()),
            (vec![fast], Vec::new())
        );
        // A second controller holds A too.
        assert_eq!(
            input.pad_control(OTHER_PAD, "East", true, &ds()),
            (Vec::new(), Vec::new())
        );
        // The first is unplugged: the gamepad thread sends a release for
        // each control it held. The other controller's A stays down.
        assert_eq!(
            input.pad_control(PAD, "East", false, &ds()),
            (Vec::new(), Vec::new())
        );
        assert_eq!(
            input.pad_control(PAD, "RightTrigger2", false, &ds()),
            (vec![Command::SetSpeed(Speed::NORMAL)], Vec::new())
        );
        // Then the other.
        assert_eq!(
            input.pad_control(OTHER_PAD, "East", false, &ds()),
            (vec![holds(&[])], Vec::new())
        );

        // Unplugged with only the hotkey modifier down, that reaches the
        // game as the tap letting go of it is.
        assert_eq!(
            input.pad_control(PAD, "Select", true, &ds()),
            (Vec::new(), Vec::new())
        );
        assert_eq!(
            input.pad_control(PAD, "Select", false, &ds()),
            (vec![holds(&[Button::Select]), holds(&[])], Vec::new())
        );

        // A controller unplugged under the game menu: everything was let
        // go of as the menu opened, so its releases change nothing.
        input.pad_control(PAD, "East", true, &ds());
        assert_eq!(input.release_all(&ds()), let_go());
        assert_eq!(
            input.pad_control(PAD, "East", false, &ds()),
            (Vec::new(), Vec::new())
        );
    }

    #[test]
    fn hotkey_chords_and_turbo_buttons_keep_out_of_each_other_s_way() {
        let mut input = GameInput::default();
        let bindings = Bindings::default()
            .overlay("[gamepad]\nturbo_a = [\"North\"]\n\n[hotkeys]\nturbo_b = [\"West\"]\n")
            .expect("valid bindings");
        input.set_bindings(bindings);
        let nothing = (Vec::new(), Vec::new());

        // Turbo A held, then Select+R1 saves a state: the game sees
        // neither Select nor R, and the turbo goes on.
        assert_eq!(
            input.pad_control(PAD, "North", true, &ds()),
            (vec![turbo(&[Button::A])], Vec::new())
        );
        assert_eq!(input.pad_control(PAD, "Select", true, &ds()), nothing);
        assert_eq!(
            input.pad_control(PAD, "RightTrigger", true, &ds()),
            (Vec::new(), vec![Shortcut::SaveState])
        );
        assert_eq!(
            input.pad_control(PAD, "RightTrigger", false, &ds()),
            nothing
        );
        // Select+West holds turbo B, a hotkey that is held, beside it.
        assert_eq!(
            input.pad_control(PAD, "West", true, &ds()),
            (vec![turbo(&[Button::A, Button::B])], Vec::new())
        );
        assert_eq!(
            input.pad_control(PAD, "West", false, &ds()),
            (vec![turbo(&[Button::A])], Vec::new())
        );
        assert_eq!(input.pad_control(PAD, "Select", false, &ds()), nothing);
        assert_eq!(
            input.pad_control(PAD, "North", false, &ds()),
            (vec![turbo(&[])], Vec::new())
        );

        // Select down, then a control that is no hotkey: Select is the
        // game's from then on, with the turbo, and hotkeys are off until
        // it comes up.
        assert_eq!(input.pad_control(PAD, "Select", true, &ds()), nothing);
        assert_eq!(
            input.pad_control(PAD, "North", true, &ds()),
            (
                vec![holds(&[Button::Select]), turbo(&[Button::A])],
                Vec::new()
            )
        );
        assert_eq!(
            input.pad_control(PAD, "RightTrigger", true, &ds()),
            (vec![holds(&[Button::Select, Button::R])], Vec::new())
        );
        // The menu opens: buttons and turbo are let go of at once, and
        // the chord in progress is forgotten with them.
        assert_eq!(
            input.release_all(&ds()),
            [
                holds(&[]),
                turbo(&[]),
                Command::ReleaseInput { player: 0 },
                holds(&[]),
            ]
        );
        assert_eq!(input.pad_control(PAD, "Select", false, &ds()), nothing);
        assert_eq!(
            input.pad_control(PAD, "RightTrigger", false, &ds()),
            nothing
        );

        // The modifier down on its own as the menu opens is forgotten too.
        // Still down after, it makes no hotkey of R1, which is the game's;
        // and let go of then, it is no tap.
        assert_eq!(input.pad_control(PAD, "Select", true, &ds()), nothing);
        assert_eq!(
            input.release_all(&ds()),
            [Command::ReleaseInput { player: 0 }, holds(&[])]
        );
        assert_eq!(
            input.pad_control(PAD, "RightTrigger", true, &ds()),
            (vec![holds(&[Button::R])], Vec::new())
        );
        assert_eq!(input.pad_control(PAD, "Select", false, &ds()), nothing);
    }

    #[test]
    fn the_mouse_touches_before_the_gamepad_cursor_does() {
        let mut input = GameInput::default();
        // A stick's click touches where the cursor is: the middle, at
        // first.
        assert_eq!(
            input.pad_control(PAD, "RightThumb", true, &ds()).0,
            [touches(128, 96)]
        );
        assert_eq!(
            input.cursor_look(),
            (Some(Cursor::centre(ScreenSize::new(256, 192))), true)
        );
        // Half a second of the stick held fully right, on upright
        // screens, and on screens turned a quarter clockwise.
        assert_eq!(
            input.move_cursor((0.5, 0.0), Rotation::Upright, &ds()),
            [touches(224, 96)]
        );
        assert_eq!(
            input.move_cursor((0.25, 0.0), Rotation::Clockwise, &ds()),
            [touches(224, 48)]
        );
        // The mouse presses on the touchscreen: the stylus is what
        // touches, and the cursor stays where it is, as it is touching.
        assert_eq!(
            input.mouse_down(Some((0, TouchPoint { x: 10, y: 20 })), &ds()),
            [touches(10, 20)]
        );
        assert_eq!(input.stylus(), Some((0, TouchPoint { x: 10, y: 20 })));
        assert_eq!(
            input.drag_stylus(Some(TouchPoint { x: 11, y: 20 }), &ds()),
            [touches(11, 20)]
        );
        assert_eq!(
            input.drag_stylus(Some(TouchPoint { x: 11, y: 20 }), &ds()),
            []
        );
        // The screen pressed on goes from the window: the stylus lifts,
        // and the cursor touches again.
        assert_eq!(input.drag_stylus(None, &ds()), [touches(224, 48)]);
        assert_eq!(input.stylus(), None);
        assert_eq!(
            input.drag_stylus(Some(TouchPoint { x: 1, y: 1 }), &ds()),
            []
        );
        assert_eq!(
            input.pad_control(PAD, "RightThumb", false, &ds()).0,
            [holds(&[])]
        );
        assert!(input.cursor().is_some(), "shown where it was left");

        // A click off the touchscreen presses A while held, and hides the
        // cursor, which is not touching.
        assert_eq!(input.mouse_down(None, &ds()), [holds(&[Button::A])]);
        assert_eq!(input.cursor(), None);
        assert_eq!(input.mouse_up(&ds()), [holds(&[])]);

        // A game with no touchscreen has nothing to touch or to point at.
        assert_eq!(input.pad_control(PAD, "RightThumb", true, &gba()).0, []);
        assert_eq!(input.move_cursor((0.5, 0.0), Rotation::Upright, &gba()), []);
        assert_eq!(input.cursor(), None);
    }

    #[test]
    fn a_new_game_is_told_all_that_is_held_and_left_on() {
        let mut input = GameInput::default();
        // Fast-forward was left on last time.
        input.latch(true, false);
        assert!(input.fast_forward_latched() && !input.slow_motion_latched());
        let fast = Command::SetSpeed(Speeds::default().speed(Pace::FastForward));
        input.game_started();
        assert_eq!(input.resend(&ds()), std::slice::from_ref(&fast));
        assert_eq!(input.pace(), Pace::FastForward);
        assert_eq!(input.resend(&ds()), [], "told once");

        // The next game starts with W held and a turbo key down: it is
        // told of them, and of the speed again.
        input.set_bindings(
            Bindings::default()
                .overlay("[keys]\nturbo_b = [\"l\"]\n")
                .expect("valid bindings"),
        );
        press(&mut input, "w");
        press(&mut input, "l");
        input.game_started();
        assert_eq!(
            input.resend(&ds()),
            [holds(&[Button::Up]), turbo(&[Button::B]), fast]
        );
        // Turned off, and on again, with the notice's answer.
        assert_eq!(
            input.toggle_fast_forward(&ds()),
            (false, vec![Command::SetSpeed(Speed::NORMAL)])
        );
        let slow = Command::SetSpeed(Speeds::default().speed(Pace::SlowMotion));
        assert_eq!(input.toggle_slow_motion(&ds()), (true, vec![slow]));
    }

    #[test]
    fn the_next_player_starts_with_nothing_held_under_its_own_bindings() {
        let mut input = GameInput::default();
        press(&mut input, "w");
        let theirs = Bindings::default()
            .overlay("[turbo]\nframes = 5\n")
            .expect("valid bindings");
        let empty = Input::default();
        assert_eq!(
            input.switch_player(1, Some(theirs), &ds()),
            [
                holds(&[]),
                Command::ReleaseInput { player: 0 },
                Command::SetInput {
                    player: 1,
                    input: empty
                },
                Command::SetTurbo {
                    player: 1,
                    buttons: Buttons::NONE,
                    frames: 5
                },
            ]
        );
        assert_eq!(input.player(), 1);
        assert_eq!(input.bindings().turbo_frames(), 5);
        // W was pressed for the first player: it counts for the second
        // once pressed again.
        assert_eq!(
            repeat(&mut input, "w", Route::Game, false),
            Pressed::Game(Vec::new())
        );
        assert_eq!(input.key_up("w", &ds()), []);
        let up: Buttons = [Button::Up].into_iter().collect();
        assert_eq!(
            press(&mut input, "w"),
            Pressed::Game(vec![Command::SetInput {
                player: 1,
                input: Input {
                    buttons: up,
                    ..empty
                }
            }])
        );
        // A player with no bindings of its own keeps those in force.
        input.switch_player(0, None, &ds());
        assert_eq!(input.bindings().turbo_frames(), 5);
    }

    #[test]
    fn the_microphone_is_heard_with_whatever_is_held() {
        let mut input = GameInput::default();
        let hears = |microphone| {
            told(Input {
                microphone,
                ..Input::default()
            })
        };
        assert_eq!(input.hear(40), Some(hears(40)));
        assert_eq!(input.hear(40), None, "no change");
        // B blows into it, as loud as it reads, whatever it hears.
        assert_eq!(
            press(&mut input, "b"),
            Pressed::Game(vec![hears(Input::LOUDEST)])
        );
        assert_eq!(input.key_up("b", &ds()), [hears(40)]);
        // Everything let go of: the microphone goes on being heard.
        assert_eq!(
            input.release_all(&ds()),
            [Command::ReleaseInput { player: 0 }, hears(40)]
        );
        assert_eq!(input.stop_hearing(), hears(0));
    }
}
