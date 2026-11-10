//! The main window: draws the running game's screens and turns keyboard,
//! gamepad, mouse and file events into emulator commands.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use emu_core::{CoreInfo, Frame, ScreenSize, System, rgb as pack_rgb};
use emu_script::Draw;
use gpui::{
    Animation, AnimationExt, App, BoxShadow, Context, Div, Entity, ExternalPaths, FocusHandle,
    Font, FontFeatures, FontStyle, FontWeight, KeyDownEvent, KeyUpEvent, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, PathPromptOptions,
    RenderImage, SharedString, StyleRefinement, Subscription, Task, Window, WindowBounds,
    WindowHandle, div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb, rgba,
};

use crate::ambient::{self, Backdrop};
use crate::app::{
    AddGameFolders, OpenGameFolder, ScreenshotForGuide, ShowAppMenu, ToggleGuide, shortcut,
};
use crate::app::{
    AddToLibrary, CloseGame, ConnectLinkCable, DebugNextFrame, DebugStep, DebugStepOver,
    EditCheats, GameInfo, InsertGbaCartridge, LayoutHorizontal, LayoutHybrid, LayoutSingle,
    LayoutVertical, LoadState, NextScreenGap, NextStateSlot, OpenRom, OpenSettings, OpenStore,
    PictureAuto, PictureLcdGrid, PictureRoundedPixels, PictureScanlines, PictureSharpPixels,
    PictureSmooth, PreviousStateSlot, Randomize, RemoveFromLibrary, Reset, Resolution3d1x,
    Resolution3d2x, Resolution3d3x, Resolution3d4x, Resolution3d6x, Resolution3d8x,
    Resolution3dMatchDisplay, RotateAnticlockwise, RotateClockwise, RunScript, SaveScreenshot,
    SaveState, ShowDebugger, StopScript, SwapScreens, SwitchPlayer, TOP_BAR, ToggleColorCorrection,
    ToggleFastForward, ToggleFullScreen, ToggleMicrophone, TogglePause, ToggleRecording,
    ToggleSlowMotion, ToggleWindowControls,
};
use crate::app::{
    FastForward2x, FastForward3x, FastForward4x, FastForwardUnlimited, MenuChecks, ToggleFps,
};
use crate::app_menu::{AppMenu, AppMenuEvent};
use crate::archive;
use crate::audio::{self, AudioOutput};
use crate::bindings::{self, Bindings, Shortcut};
use crate::cheats;
use crate::chrome::{self, Tone};
use crate::color::Correction;
use crate::cursor::Cursor;
use crate::debugger::{self, Snapshot};
use crate::debugger_window::DebuggerWindow;
use crate::emulator::{self, CHECKPOINT, Command, Emulator, Event, Opened, OwnSaves};
use crate::filter::Filter;
use crate::fonts;
use crate::game_menu::{CheatRow, GameMenu, MenuEvent};
use crate::gamepad::{self, PadEvent};
use crate::gpu_upscale::{Bgra, Gpu, GpuDevice, GpuError, Method, Request};
use crate::guide::{self, GuideEvent, GuideGame, GuidePanel};
use crate::key_guide;
use crate::key_routes::Route;
use crate::keys;
use crate::layout::{self, Arrangement, Layout, Placement, Rotation};
use crate::library::{self, Library};
use crate::library_view::{LibraryEvent, LibraryView};
use crate::meter::{self, Meter};
use crate::microphone::{self, Loudness, Microphone};
use crate::persist::{self, GameFiles, GameLock, StateFiles, file_name};
use crate::picture::{self, Look, Picture, Resolution3d};
use crate::probe;
use crate::randomize_window::{self, RandomizeWindow};
use crate::romhack::{self, MakeError};
use crate::settings::{self, Settings, WindowPlace};
use crate::settings_window::SettingsWindow;
use crate::slots::{self, STATE_SLOTS, SlotPreview};
use crate::speed::{self, Pace, Speed};
use crate::store_view::pulse;
use crate::theme::{BACKGROUND, INSET, STAGE};
use crate::trace;
use crate::video;
use crate::window_chrome;

mod input;
mod opening;
mod screens;

use input::{GameInput, Pressed, Stage};
use opening::Game;
use screens::{Glow, ImageKey, Keeping, Upscaler, Upscaling};

const NOTICE_DURATION: Duration = Duration::from_secs(3);

/// How long a warning the player must not miss shows, such as a game
/// whose saves cannot be kept: long enough to read a sentence or two.
const WARNING_DURATION: Duration = Duration::from_secs(12);

/// How long the pill that opens Guide stays once the pointer rests.
const GUIDE_PILL_REST: Duration = Duration::from_secs(2);

/// The speed readout's grey: Apple's system grey, which reads on the
/// dark stage and on a light game alike.
const READOUT: u32 = 0x008E_8E93;
/// The height of the pill that opens Guide and of the recording mark,
/// which sit in the window's top right corner on the traffic lights'
/// midline.
const CORNER_PLATE: f32 = 24.0;
/// How often the speed readout measures.
const READOUT_INTERVAL: Duration = Duration::from_millis(500);

/// Where the top bar's shade starts, at the window's top edge: dark enough
/// for the close, minimise and zoom buttons to show on a white screen.
const TOP_BAR_SHADE: u32 = 0x0000_0066;

/// How long the window must stay put before its place is saved, so a
/// drag or resize writes the settings once rather than on every step.
const PLACE_SAVE_DELAY: Duration = Duration::from_millis(500);

/// The root view of the main window.
pub struct EmulatorView {
    focus: FocusHandle,
    /// Everything held for the game, from the keyboard, gamepads and the
    /// mouse, with the bindings in force and the player they control.
    input: GameInput,
    /// The settings in force, as the settings file keeps them: the
    /// layout, the speeds, the picture and the rest. Fast-forward and
    /// slow motion being left on are `controls`' to say, not this
    /// value's (see [`EmulatorView::settings`]).
    settings: Settings,
    /// Whether a linked game shows its consoles side by side whatever
    /// the settings' layout, the user's own, says; until it stops or the
    /// user picks an arrangement.
    linked_side_by_side: bool,
    /// Warnings from startup kept for the game it opens, whose loading
    /// notices would hide them.
    startup_notice: Option<String>,
    /// Saves the window's place once it has stopped moving.
    place_save: Option<Task<()>>,
    /// Where the layout is saved for next launch, if there is a config
    /// folder.
    settings_path: Option<PathBuf>,
    /// The computer's microphone, while the user has it on: off at every
    /// launch.
    microphone: Option<(Microphone, Loudness)>,
    /// The GPU half of the upscaler, started when a picture first needs it.
    upscaler: Upscaler,
    /// Draws again once the upscaler finishes screens it took too long
    /// over to wait for.
    upscale_wake: Option<Task<()>>,
    /// What the speed readout says, once it has measured.
    speed_readout: Option<SharedString>,
    /// Measures the speed for the readout, with the number from
    /// `sessions` of the game it counts.
    fps_meter: Option<(u64, Meter)>,
    /// Updates the readout while it shows.
    readout_task: Option<Task<()>>,
    /// Whether the buttons were last shown or hidden, once they have been.
    shown_controls: Option<bool>,
    /// The debugger window, once opened.
    debugger: Option<WindowHandle<DebuggerWindow>>,
    /// The settings window, once opened.
    settings_window: Option<WindowHandle<SettingsWindow>>,
    /// The randomize window, once opened.
    randomize_window: Option<WindowHandle<RandomizeWindow>>,
    /// Whether the randomize window is about to open, its games being read.
    opening_randomizer: bool,
    /// The games, shown while none runs.
    library: Entity<LibraryView>,
    /// The game menu, while it is open over the paused game.
    menu: Option<(Entity<GameMenu>, Subscription)>,
    /// Off macOS, the menu the window draws for the menu bar there is
    /// none of, while it is open, and what had the keyboard before it.
    app_menu: Option<(Entity<AppMenu>, Subscription, Option<FocusHandle>)>,
    /// Whether the pointer is in the top bar, which shows the menu's
    /// button off macOS while the window's buttons are hidden.
    title_hovered: bool,
    /// Games started so far, which numbers each for the debugger.
    sessions: u64,
    /// The game: none, loading, running or stopped (see `opening.rs`).
    game: Game<Session, Task<()>>,
    /// A script from the command line, to run with the first game.
    startup_script: Option<PathBuf>,
    placements: Vec<Placement>,
    notice: Option<SharedString>,
    /// Whether the notice shown is a warning, which shows for longer, and
    /// so does what joins it.
    notice_is_warning: bool,
    notice_timer: Option<Task<()>>,
    /// Screenshot of the selected save-state slot, shown with the notice.
    slot_image: Option<Arc<RenderImage>>,
    /// Images no longer shown, to remove from GPUI's atlas on next render.
    stale_images: Vec<Arc<RenderImage>>,
    slot_preview_task: Option<Task<()>>,
    /// Guide, the chat about the game, in the margin beside the screens.
    guide: Entity<GuidePanel>,
    /// Whether Guide's panel shows.
    guide_open: bool,
    /// While the panel is closed: when the pointer last moved in the
    /// empty margin, which shows the pill that opens it.
    guide_pill: Option<Instant>,
    /// Hides the pill once the pointer has rested.
    guide_pill_hide: Option<Task<()>>,
    /// Whether the pointer is on the pill, which keeps it and lights it.
    guide_pill_hovered: bool,
    _gamepad: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// What the debugger window needs to know about the running game.
pub struct DebugTarget {
    /// Counts the games started, so a new one can be told apart.
    pub session: u64,
    /// The game's system.
    pub system: System,
    /// The player the keyboard controls, whose CPUs the debugger shows.
    pub player: usize,
    /// The emulation thread's latest snapshot for the debugger.
    pub snapshot: Option<Arc<Snapshot>>,
}

/// A running game and the images last drawn for it.
struct Session {
    /// Passes the emulation thread's events to the view. Dropped before
    /// `emulator`, with `events`, so that what the thread says as it stops
    /// goes to stderr rather than nowhere; [`Session::finish`] shows it
    /// instead.
    _events: Task<()>,
    /// The events, for what is left of them as the game stops.
    events: async_channel::Receiver<Event>,
    emulator: Emulator,
    info: CoreInfo,
    /// Each cartridge's ROM: one per player, then any in a second slot.
    rom_paths: Vec<PathBuf>,
    /// Each player's ROM.
    player_rom_paths: Vec<PathBuf>,
    /// Each cartridge's own files, in `rom_paths`' order, named as the
    /// game started and kept so while it runs.
    files: Vec<GameFiles>,
    /// Where the session's save states are.
    states: StateFiles,
    /// Where the game keeps the saves it makes itself.
    own_saves: OwnSaves,
    frames: Vec<Frame>,
    sequence: u64,
    /// Per player: the cheat file's text when last sent to the emulator.
    cheat_texts: Vec<Option<String>>,
    /// Per player: the user's bindings with the game's own laid over them.
    bindings: Vec<Bindings>,
    /// The system's colour correction, if it has one.
    correction: Option<Correction>,
    /// Whether the emulation thread is recording a movie.
    recording: bool,
    /// Per placement: the image drawn there and what it shows. An image
    /// stays until the next is drawn, which the upscaler may finish a
    /// frame or so later.
    images: Vec<Option<(ImageKey, Arc<RenderImage>)>>,
    /// The screens on the upscaler's GPU, in the order sent.
    upscaling: Vec<Upscaling>,
    /// The 3D resolution last sent to the core.
    render_scale: u32,
    /// Match Display's check that the game keeps its speed: the most it
    /// may draw 3D at now, what it was set for (the scale matching the
    /// screens and the speed asked for; a change starts it again at that
    /// scale), and when the speed was last measured, with the frames run
    /// then.
    keeping: Keeping,
    /// The ambient backdrop, while it is shown.
    ambient: Option<Glow>,
    /// The display whose refresh the emulation thread was last told of,
    /// once it has been told.
    display: Option<window_chrome::DisplayKey>,
    /// When the window last took a new frame.
    taken_at: Option<Instant>,
    /// Redraws the backdrop's fade once frames stop coming.
    fade_wake: Option<Task<()>>,
    /// How long the game has been played since the library last heard.
    play: library::PlayClock,
    /// Keeps the session's games from another copy of the app. Dropped
    /// after `emulator`, once the thread has written the saves.
    _locks: Vec<GameLock>,
    /// Dropped after `emulator`, so the thread stops before the device.
    _audio: Option<AudioOutput>,
}

impl EmulatorView {
    /// Creates the view, loading `roms` straight away if there are any.
    /// `script` runs alongside the first game to start.
    pub fn new(
        roms: Vec<PathBuf>,
        script: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        crate::x11_redraw::ensure_redraws();

        let loaded_bindings = bindings::default_path().map(|path| Bindings::load(&path));
        let settings_path = settings::default_path();
        let loaded_settings = settings_path.as_deref().map(Settings::load);
        let library_path = library::default_path();
        let (games, library_error) = match library_path.as_deref().map(Library::load) {
            Some(Ok(games)) => (games, None),
            Some(Err(error)) => (Library::default(), Some(error)),
            None => (Library::default(), None),
        };
        // A library that cannot be read is left alone, not overwritten.
        let library_path = library_path.filter(|_| library_error.is_none());
        let library = cx.new(|cx| LibraryView::new(games, library_path, cx));
        let guide = cx.new(|cx| GuidePanel::new(window, cx));
        let composer_focus = guide.read(cx).composer_focus(cx);
        let subscriptions = vec![
            cx.subscribe_in(&guide, window, Self::guide_event),
            // The composer taking the caret lets go of every game key held,
            // as losing focus does: a held W stops walking.
            cx.on_focus_in(&composer_focus, window, |this, _, cx| {
                this.release_all();
                cx.notify();
            }),
            cx.subscribe_in(&library, window, Self::library_event),
            // Over a game, the library's games being added show in the
            // corner (see `adding_card`), drawn anew as the library is, even
            // while the game is paused.
            cx.observe(&library, |this, _, cx| {
                if this.game.session().is_some() {
                    // The library may have just read the game Guide is about.
                    let game = this.guide_game(cx);
                    this.guide.update(cx, |guide, cx| guide.set_game(game, cx));
                    cx.notify();
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    // Keys released while another window has focus never
                    // send key-up. Keys still down when focus comes back
                    // count once pressed again (the arrows may count from
                    // their next repeat, which GPUI passes on as a press).
                    let commands = this.input.focus_lost(&this.stage());
                    this.send_all(commands);
                } else {
                    // Back from editing a cheat file, perhaps.
                    if let Some(message) = this.reload_cheats() {
                        this.show_notice(message, cx);
                    }
                    this.refresh_menu_cheats(cx);
                }
            }),
            cx.observe_window_bounds(window, Self::remember_window_place),
            // Closing the window drops the view before quitting, so a
            // place not yet saved is saved here, and the game's checkpoint:
            // the emulation thread writes it, and the battery saves, before
            // it stops as the view goes.
            cx.on_release(|this, cx| {
                if this.place_save.take().is_some()
                    && let Some(path) = &this.settings_path
                {
                    this.settings().save(path).ok();
                }
                this.save_checkpoint();
                this.record_play_time(cx);
            }),
            // Stop the emulation thread on quit so the battery save is flushed.
            cx.on_app_quit(|this, cx| {
                if this.place_save.take().is_some() {
                    this.save_settings(cx);
                }
                this.save_checkpoint();
                this.record_play_time(cx);
                drop(this.game.end());
                async {}
            }),
        ];

        let (pad_events, pad_receiver) = async_channel::unbounded();
        if let Err(error) = gamepad::spawn(pad_events) {
            eprintln!("ds-emulator: cannot start gamepad thread: {error}");
        }
        let gamepad_task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = pad_receiver.recv().await {
                let delivered = this.update_in(cx, |this, window, cx| {
                    // Gamepads are global; only the active window listens,
                    // but releases always count.
                    let release = matches!(event, PadEvent::Control { pressed: false, .. });
                    if window.is_window_active() || release {
                        this.pad_event(&event, window, cx);
                    }
                });
                if delivered.is_err() {
                    break;
                }
            }
        });

        let mut view = Self {
            focus,
            input: GameInput::default(),
            settings: Settings::default(),
            linked_side_by_side: false,
            startup_notice: None,
            place_save: None,
            settings_path,
            microphone: None,
            upscaler: Upscaler::Idle,
            upscale_wake: None,
            speed_readout: None,
            fps_meter: None,
            readout_task: None,
            shown_controls: None,
            debugger: None,
            settings_window: None,
            randomize_window: None,
            opening_randomizer: false,
            library,
            menu: None,
            app_menu: None,
            title_hovered: false,
            sessions: 0,
            game: Game::Idle,
            startup_script: script,
            placements: Vec::new(),
            notice: None,
            notice_is_warning: false,
            notice_timer: None,
            slot_image: None,
            stale_images: Vec::new(),
            slot_preview_task: None,
            guide,
            guide_open: false,
            guide_pill: None,
            guide_pill_hide: None,
            guide_pill_hovered: false,
            _gamepad: gamepad_task,
            _subscriptions: subscriptions,
        };
        let mut warnings = Vec::new();
        match loaded_bindings {
            Some(Ok(bindings)) => view.input.set_bindings(bindings),
            Some(Err(error)) => warnings.push(format!("{error}; using default bindings")),
            None => {}
        }
        match loaded_settings {
            Some(Ok(settings)) => {
                view.settings = settings;
                view.input
                    .latch(settings.fast_forward_on, settings.slow_motion_on);
            }
            Some(Err(error)) => {
                // A file that cannot be read is left for the user to fix,
                // not overwritten.
                view.settings_path = None;
                warnings.push(format!("{error}; using default settings, not saved"));
            }
            None => {}
        }
        if let Some(error) = library_error {
            warnings.push(format!("{error}; the library is not saved"));
        }
        if !warnings.is_empty() {
            let warning = warnings.join(" · ");
            if roms.is_empty() {
                view.show_notice(warning, cx);
            } else {
                view.startup_notice = Some(warning);
            }
        }
        view.tell_guide_settings(cx);
        view.refresh_menus(cx);
        if view.settings.show_fps {
            view.start_readout(cx);
        }
        view.start_upscaler(cx);
        if !roms.is_empty() {
            view.open(roms, window, cx);
        }
        view
    }

    /// Asks for a Lua script, then runs it alongside the game.
    fn run_script(&mut self, _: &RunScript, window: &mut Window, cx: &mut Context<Self>) {
        if self.game.session().is_none() {
            self.show_notice("Open a game first, then run a script", cx);
            return;
        }
        self.prompt_for_file("Run", window, cx, |this, path, _, cx| {
            this.run_script_file(path, cx);
        });
    }

    /// Reads the script at `path` off the UI thread and hands it to the
    /// emulation thread, which runs it.
    fn run_script_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let read = cx.background_executor().spawn({
            let path = path.clone();
            async move { fs::read_to_string(path) }
        });
        cx.spawn(async move |this, cx| {
            let source = read.await;
            this.update(cx, |this, cx| match source {
                Ok(source) => this.send(Command::RunScript {
                    name: file_name(&path),
                    source,
                }),
                Err(error) => {
                    this.show_notice(format!("Cannot read {}: {error}", path.display()), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-reads the user's bindings file and lays each player's game
    /// bindings file over it, putting the active player's in force.
    /// Returns what went wrong, if anything.
    fn reload_bindings(&mut self) -> Option<String> {
        let mut problems = Vec::new();
        let user = match bindings::default_path().map(|path| Bindings::load(&path)) {
            Some(Ok(bindings)) => bindings,
            Some(Err(error)) => {
                problems.push(format!("{error}; using default bindings"));
                Bindings::default()
            }
            None => Bindings::default(),
        };
        let session = self.game.session_mut()?;
        session.bindings = session
            .player_files()
            .iter()
            .map(|files| {
                let path = files.bindings();
                user.overlay_file(&path).unwrap_or_else(|error| {
                    problems.push(format!("{} not applied: {error}", file_name(&path)));
                    user.clone()
                })
            })
            .collect();
        let in_force = session
            .bindings
            .get(self.input.player())
            .cloned()
            .unwrap_or(user);
        self.input.set_bindings(in_force);
        (!problems.is_empty()).then(|| problems.join(" · "))
    }

    /// Re-reads each player's cheat file and sends the ones that changed to
    /// the emulator. Returns a summary of the changes, if any.
    fn reload_cheats(&mut self) -> Option<String> {
        let session = self.game.session_mut()?;
        let paths: Vec<PathBuf> = session
            .player_files()
            .iter()
            .map(GameFiles::cheats)
            .collect();
        let players = paths.len();
        let mut changes = Vec::new();
        for (player, path) in paths.iter().enumerate() {
            let text = cheats::read(path);
            if text == session.cheat_texts[player] {
                continue;
            }
            let whose = if players > 1 {
                format!("Player {} cheats", player + 1)
            } else {
                "Cheats".to_owned()
            };
            match emu_cheats::parse(session.info.system, text.as_deref().unwrap_or_default()) {
                Ok(list) => {
                    let on = list.iter().filter(|cheat| cheat.enabled).count();
                    changes.push(format!("{whose}: {on} on"));
                    session.emulator.send(Command::SetCheats {
                        player,
                        cheats: list,
                    });
                }
                Err(error) => changes.push(format!("{whose} not applied: {error}")),
            }
            session.cheat_texts[player] = text;
        }
        (!changes.is_empty()).then(|| changes.join(" · "))
    }

    /// Opens the active player's cheat file in the text editor, creating it
    /// from a template first if needed.
    fn edit_cheats(&mut self, _: &EditCheats, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.cheat_file() else {
            self.show_notice("Open a game first, then edit its cheats", cx);
            return;
        };
        let opened = cheats::create_if_missing(&path).and_then(|()| cheats::open_in_editor(&path));
        match opened {
            Ok(()) => self.show_notice(
                format!(
                    "Editing {}; save it and switch back to apply",
                    file_name(&path)
                ),
                cx,
            ),
            Err(error) => self.show_notice(format!("Cannot open {}: {error}", path.display()), cx),
        }
    }

    /// Adds the time the running game has been played to each player's
    /// game in the library.
    fn record_play_time(&mut self, cx: &mut App) {
        let Some(session) = self.game.session_mut() else {
            return;
        };
        let played = session.play.take();
        if played.is_zero() {
            return;
        }
        let paths = session.player_rom_paths.clone();
        self.library
            .update(cx, |library, cx| library.add_play_time(&paths, played, cx));
    }

    fn handle(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Frame => {
                if let Some(session) = self.game.session_mut() {
                    session.play.frame(Instant::now());
                }
                self.listen();
                cx.notify();
            }
            // Wakes the debugger window, which redraws with this view.
            Event::Debug => cx.notify(),
            event @ (Event::StateSaved(_)
            | Event::StateLoaded(_)
            | Event::Failed(_)
            | Event::Notice(_)) => {
                if let Some(text) = notice_of(&event) {
                    self.show_notice(text.into_owned(), cx);
                }
            }
            Event::Warning(text) => self.show_warning(text, cx),
            // Joins the game's starting notice, which may hold warnings.
            Event::PickedUp(loaded) => {
                let own_save = self.game.session().is_some_and(|session| {
                    matches!(session.info.system, System::RpgMaker | System::N3ds)
                });
                let picked = if loaded {
                    "Picked up where you left off"
                } else if own_save {
                    "The game's own save changed after you left off, so the game starts at its title"
                } else {
                    "The battery save changed after you left off, so the game starts from it"
                };
                // A warning it joins still shows as long as a warning.
                match &self.notice {
                    Some(shown) if self.notice_is_warning => {
                        self.show_warning(format!("{shown} · {picked}"), cx);
                    }
                    Some(shown) => self.show_notice(format!("{shown} · {picked}"), cx),
                    None => self.show_notice(picked, cx),
                }
            }
            Event::Recording(on) => {
                if let Some(session) = self.game.session_mut() {
                    session.recording = on;
                }
                cx.notify();
            }
            // Kept up until another notice, as no more frames come.
            Event::Stopped(text) => {
                self.game.halted();
                self.show_notice(text, cx);
                self.notice_timer = None;
                self.library.update(cx, |library, cx| library.refresh(cx));
            }
        }
    }

    /// Shows `text` for a few seconds in place of any notice shown, and
    /// of the save state screenshot shown with it.
    fn show_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_for(text.into(), false, cx);
    }

    /// Shows `text` as [`Self::show_notice`] does, but for long enough to
    /// read a sentence or two: for what the player must not miss.
    fn show_warning(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_for(text.into(), true, cx);
    }

    fn show_for(&mut self, text: SharedString, warning: bool, cx: &mut Context<Self>) {
        self.notice = Some(text);
        self.notice_is_warning = warning;
        // A screenshot belongs to the slot's notice; `show_slot_preview`
        // brings the new slot's, if it has one.
        self.stale_images.extend(self.slot_image.take());
        let timer = cx.background_executor().timer(if warning {
            WARNING_DURATION
        } else {
            NOTICE_DURATION
        });
        self.notice_timer = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| {
                this.notice = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn send(&self, command: Command) {
        if let Some(session) = self.game.session() {
            session.emulator.send(command);
        }
    }

    /// Sends the emulation thread each of `commands`, in order.
    fn send_all(&self, commands: Vec<Command>) {
        for command in commands {
            self.send(command);
        }
    }

    /// Sends the emulation thread what an input event came to, and draws
    /// the window again if the gamepad cursor no longer looks as it
    /// `looked` before the event, even when paused. Held inputs show
    /// nowhere else.
    fn obey(
        &mut self,
        commands: Vec<Command>,
        looked: (Option<Cursor>, bool),
        cx: &mut Context<Self>,
    ) {
        self.send_all(commands);
        if self.input.cursor_look() != looked {
            cx.notify();
        }
    }

    /// What the input needs to know of the game and the settings now.
    fn stage(&self) -> Stage {
        Stage {
            touchscreen: self.touchscreen(),
            speeds: self.settings.speed,
        }
    }

    /// Hands keyboard and gamepad control to `player` (see
    /// [`GameInput::switch_player`]).
    ///
    /// Not while the game menu is open: it is the active player's game's
    /// menu, its cheats and Start Over that player's, so the player stays
    /// until it closes.
    fn switch_player(&mut self, player: usize, cx: &mut Context<Self>) {
        let Some(players) = self.game.session().map(|s| s.info.players) else {
            return;
        };
        if players < 2 || player == self.input.player() || player >= players || self.menu.is_some()
        {
            return;
        }
        let bindings = self
            .game
            .session()
            .and_then(|session| session.bindings.get(player))
            .cloned();
        let commands = self.input.switch_player(player, bindings, &self.stage());
        self.send_all(commands);
        self.show_notice(format!("Controlling player {}", player + 1), cx);
    }

    /// The touchscreen of the console being played: its index and size.
    /// Each of two linked DSes has its own.
    fn touchscreen(&self) -> Option<(usize, ScreenSize)> {
        let info = &self.game.session()?.info;
        let per_console = info.screens.len() / consoles(info);
        let screen = info.touch_screen? + self.input.player().min(consoles(info) - 1) * per_console;
        Some((screen, *info.screens.get(screen)?))
    }

    /// Whether a touchscreen is drawn up into the top bar, where a press
    /// must touch it rather than drag the window.
    fn touchscreen_in_top_bar(&self) -> bool {
        let Some(info) = self.game.session().map(|session| &session.info) else {
            return false;
        };
        let Some(touch) = info.touch_screen else {
            return false;
        };
        let per_console = (info.screens.len() / consoles(info)).max(1);
        self.placements
            .iter()
            .any(|placement| placement.screen % per_console == touch && placement.rect.y < TOP_BAR)
    }

    /// Lets go of everything held, at once: for when keys and buttons stop
    /// reaching the game while they may still be down, as when the game
    /// menu opens (see [`GameInput::release_all`]).
    fn release_all(&mut self) {
        let commands = self.input.release_all(&self.stage());
        self.send_all(commands);
    }

    /// Where keys go now.
    fn key_route(&self, window: &Window, cx: &App) -> Route {
        if self.game.session().is_none() {
            Route::Library
        } else if self.menu.is_some() || self.app_menu.is_some() {
            Route::Menu
        } else if self.guide_open && self.guide.read(cx).composer_focused(window, cx) {
            Route::Guide
        } else {
            Route::Game
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let route = self.key_route(window, cx);
        let looked = self.input.cursor_look();
        let pressed = self.input.key_down(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers,
            event.is_held,
            route,
            self.menu.is_some(),
            &self.stage(),
        );
        // What the library and the menu are told of the key: a key already
        // down is repeating whatever the event says.
        let passed_on = |repeat: bool| KeyDownEvent {
            keystroke: event.keystroke.clone(),
            is_held: repeat,
        };
        match pressed {
            Pressed::Elsewhere => {
                if route == Route::Guide {
                    cx.stop_propagation();
                }
            }
            // The composer's own.
            Pressed::Guide => {}
            Pressed::Library { repeat } => {
                let event = passed_on(repeat);
                self.library
                    .update(cx, |library, cx| library.key_down(&event, cx));
            }
            Pressed::Menu { repeat } => {
                if let Some((menu, _)) = &self.menu {
                    let event = passed_on(repeat);
                    menu.clone()
                        .update(cx, |menu, cx| menu.key_down(&event, cx));
                }
            }
            Pressed::OpensMenu => self.open_menu(window, cx),
            // Redraws the cursor if a key touches with it.
            Pressed::Game(commands) => self.obey(commands, looked, cx),
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let looked = self.input.cursor_look();
        let commands = self
            .input
            .key_up(event.keystroke.key.as_str(), &self.stage());
        self.obey(commands, looked, cx);
    }

    /// Holds or lets go of whatever Shift is bound to, as it comes only as
    /// a change of modifiers, never as a key; Shift is the game's while it
    /// plays, as a key would be. Lets go of every key held for the game
    /// when ⌘ goes down (see [`GameInput::release_keys`]).
    fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shift = event.modifiers.shift && self.key_route(window, cx) == Route::Game;
        let commands = self.input.shift(shift, &self.stage());
        self.send_all(commands);
        if !event.modifiers.platform {
            return;
        }
        let looked = self.input.cursor_look();
        let commands = self.input.release_keys(&self.stage());
        self.obey(commands, looked, cx);
    }

    fn pad_event(&mut self, event: &PadEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The game is paused under the menu; its controls wait.
        if self.menu.is_some() {
            return;
        }
        let looked = self.input.cursor_look();
        let stage = self.stage();
        let commands = match event {
            PadEvent::Control {
                controller,
                control,
                pressed,
            } => {
                let (commands, shortcuts) = self
                    .input
                    .pad_control(controller, control, *pressed, &stage);
                for shortcut in shortcuts {
                    // Its handler updates this view, which is busy until
                    // this event is done, so it runs just after.
                    let action = shortcut_action(shortcut);
                    let focus = self.focus.clone();
                    window.defer(cx, move |window, cx| {
                        focus.dispatch_action(action.as_ref(), window, cx);
                    });
                }
                commands
            }
            PadEvent::Pointer { dx, dy } => {
                let rotation = self.settings.layout.rotation;
                self.input.move_cursor((*dx, *dy), rotation, &stage)
            }
        };
        // Redraws the cursor, even when paused.
        self.obey(commands, looked, cx);
    }

    /// Turns latched fast-forward on or off, and saves that for next
    /// launch.
    fn toggle_fast_forward(&mut self, cx: &mut Context<Self>) {
        let (on, commands) = self.input.toggle_fast_forward(&self.stage());
        self.send_all(commands);
        self.show_notice(
            if on {
                "Fast-forward on"
            } else {
                "Fast-forward off"
            },
            cx,
        );
        self.save_settings(cx);
    }

    /// Turns latched slow motion on or off, and saves that for next
    /// launch.
    fn toggle_slow_motion(&mut self, cx: &mut Context<Self>) {
        let (on, commands) = self.input.toggle_slow_motion(&self.stage());
        self.send_all(commands);
        self.show_notice(
            if on {
                "Slow motion on"
            } else {
                "Slow motion off"
            },
            cx,
        );
        self.save_settings(cx);
    }

    /// Notes where the window now is and, once it has settled there,
    /// saves that for next launch.
    fn remember_window_place(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (bounds, fullscreen) = match window.window_bounds() {
            WindowBounds::Fullscreen(bounds) => (bounds, true),
            WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds) => (bounds, false),
        };
        let place = WindowPlace {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
            fullscreen,
            display: window
                .display(cx)
                .and_then(|display| display.uuid().ok())
                .map(|uuid| uuid.as_u128()),
        };
        if self.settings.window_place == Some(place) {
            return;
        }
        self.settings.window_place = Some(place);
        let timer = cx.background_executor().timer(PLACE_SAVE_DELAY);
        self.place_save = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| {
                this.place_save = None;
                this.save_settings(cx);
            })
            .ok();
        }));
    }

    /// Moves to the next or previous save-state slot, and saves that for
    /// next launch.
    fn change_state_slot(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.settings.state_slot = next_slot(self.settings.state_slot, forward);
        let slot = self.settings.state_slot;
        self.show_notice(format!("State slot {slot}"), cx);
        self.save_settings(cx);
        // The file Load State would read.
        let Some(state) = self.game.session().map(|s| s.states.to_read(slot)) else {
            return;
        };
        let preview = cx
            .background_executor()
            .spawn(async move { slots::preview(&state) });
        self.slot_preview_task = Some(cx.spawn(async move |this, cx| {
            let preview = preview.await;
            this.update(cx, |this, cx| this.show_slot_preview(slot, preview, cx))
                .ok();
        }));
    }

    fn show_slot_preview(
        &mut self,
        slot: u8,
        preview: Option<SlotPreview>,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = preview else {
            self.show_notice(format!("State slot {slot} · empty"), cx);
            return;
        };
        let age = preview.saved.elapsed().unwrap_or_default();
        self.show_notice(
            format!("State slot {slot} · saved {}", slots::describe_age(age)),
            cx,
        );
        if let Some((width, height, rgb)) = preview.thumbnail {
            let image = slot_image(width, height, &rgb);
            self.stale_images.extend(self.slot_image.replace(image));
        }
    }

    /// A click. On the touchscreen it is the stylus touching there, so the
    /// game moves its selection there and presses it at once, as on a DS;
    /// anywhere else it presses A. The click that brings the window to the
    /// front only does that.
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The panel keeps its own clicks, so this one is outside it: the
        // keys go back to the game.
        if self.guide.read(cx).composer_focused(window, cx) {
            window.focus(&self.focus);
        }
        if event.first_mouse {
            return;
        }
        // In a multi-player session, clicking a screen takes control of it.
        let point = (f32::from(event.position.x), f32::from(event.position.y));
        let clicked = self.game.session().and_then(|session| {
            let placement = self.placements.iter().find(|p| p.rect.contains(point))?;
            Some(session.info.player_for_screen(placement.screen))
        });
        if let Some(player) = clicked {
            self.switch_player(player, cx);
        }
        // The touchscreen as the window shows it, possibly more than once.
        let touched = self.touchscreen().and_then(|(screen, size)| {
            let (index, placement) = self
                .placements
                .iter()
                .enumerate()
                .find(|(_, p)| p.screen == screen && p.rect.contains(point))?;
            Some((index, placement.pixel_at(size, point)?))
        });
        // Back to the mouse, which touches there, or elsewhere, and in
        // every other game, presses A for as long as it is held.
        let commands = self.input.mouse_down(touched, &self.stage());
        self.send_all(commands);
        cx.notify();
    }

    /// Drags the stylus with the mouse while it touches. Past the
    /// touchscreen's edges it stays on the nearest edge, as a stylus slid
    /// off a DS would.
    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.show_guide_pill(event, window, cx);
        let Some((index, _)) = self.input.stylus() else {
            return;
        };
        let Some((screen, size)) = self.touchscreen() else {
            return;
        };
        let point = (f32::from(event.position.x), f32::from(event.position.y));
        // Where the screen is now, should the layout have changed: nowhere
        // if the screen pressed on is gone from the window.
        let moved = self
            .placements
            .get(index)
            .filter(|placement| placement.screen == screen)
            .and_then(|placement| placement.pixel_nearest(size, point));
        let commands = self.input.drag_stylus(moved, &self.stage());
        self.send_all(commands);
    }

    /// Lifts the stylus or releases A when the mouse button comes up, in
    /// the window or out of it.
    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let commands = self.input.mouse_up(&self.stage());
        self.send_all(commands);
    }

    /// Asks for a game, then calls `then` with it: a file, or on macOS a
    /// file or a folder, as an RPG Maker game is.
    fn prompt_for_file(
        &mut self,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.prompt_for(Choose::Games, prompt, window, cx, then);
    }

    /// Asks for `choose`, then calls `then` with the first chosen. Says
    /// why if the dialog can't open, as on Linux without a desktop portal.
    fn prompt_for(
        &mut self,
        choose: Choose,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let paths = cx.prompt_for_paths(choose.options(false, prompt));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(chosen) = paths.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| match chosen {
                Ok(Some(paths)) => {
                    if let Some(path) = paths.into_iter().next() {
                        then(this, path, window, cx);
                    }
                }
                Ok(None) => {}
                Err(error) => this.show_notice(dialog_failed(&error), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Shows the store in the library's place, unless a game is running.
    fn open_store(&mut self, cx: &mut Context<Self>) {
        if self.game.session().is_some() {
            let close = keys::shortcut(shortcut::CLOSE_GAME);
            self.show_notice(format!("Close the game with {close} to open the store"), cx);
            return;
        }
        self.library
            .update(cx, |library, cx| library.open_store(cx));
    }

    /// Asks for games, then adds them to the library without playing them.
    fn add_to_library(&mut self, _: &AddToLibrary, window: &mut Window, cx: &mut Context<Self>) {
        self.add_chosen(Choose::Games, window, cx);
    }

    /// Asks for `choose`, any number, then adds them to the library.
    fn add_chosen(&mut self, choose: Choose, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(choose.options(true, "Add"));
        let library = self.library.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(chosen) = paths.await else {
                return;
            };
            match chosen {
                Ok(Some(paths)) => {
                    library
                        .update(cx, |library, cx| library.add_chosen(paths, cx))
                        .ok();
                }
                Ok(None) => {}
                Err(error) => {
                    this.update(cx, |this, cx| this.show_notice(dialog_failed(&error), cx))
                        .ok();
                }
            }
        })
        .detach();
    }

    /// What Guide is told about the game the keyboard plays.
    fn guide_game(&self, cx: &App) -> Option<GuideGame> {
        let session = self.game.session()?;
        let player = self
            .input
            .player()
            .min(session.player_rom_paths.len().saturating_sub(1));
        let rom = session.player_rom_paths.get(player)?;
        let game = self.library.read(cx).game_of(rom);
        Some(guide_info(rom, session.info.system, game))
    }

    /// ⌘J: opens Guide with the caret in its composer, or closes it if
    /// the caret is there already.
    fn toggle_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.game.session().is_none() {
            let guide = keys::shortcut(shortcut::GUIDE);
            self.show_notice(
                format!("Open a game, then ask Guide about it ({guide})"),
                cx,
            );
            return;
        }
        if self.guide_open && self.guide.read(cx).composer_focused(window, cx) {
            self.close_guide(window, cx);
        } else {
            self.open_guide(window, cx);
        }
    }

    fn open_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(cx);
        let game = self.guide_game(cx);
        self.guide.update(cx, |guide, cx| {
            guide.set_game(game, cx);
            guide.focus_composer(window, cx);
        });
        self.guide_open = true;
        self.guide_pill = None;
        self.guide_pill_hovered = false;
        self.release_all();
        cx.notify();
    }

    fn close_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.guide_open = false;
        window.focus(&self.focus);
        cx.notify();
    }

    /// ⌥⌘S and the Screenshot button: the screens as the console drew
    /// them, into Guide's composer, whether the panel shows or not.
    fn screenshot_for_guide(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.game.session() else {
            return;
        };
        if session.frames.is_empty() {
            return;
        }
        let players = session.info.players;
        let before = self.guide.read(cx).shot_count();
        let count = self
            .guide
            .update(cx, |guide, cx| guide.add_shot(&session.frames, players, cx));
        if count == before {
            self.show_notice(
                format!("Guide takes {} screenshots a question", guide::MOST_SHOTS),
                cx,
            );
        } else if !self.guide_open {
            self.show_notice(format!("Screenshot added to Guide ({count})"), cx);
        }
    }

    fn guide_event(
        &mut self,
        _: &Entity<GuidePanel>,
        event: &GuideEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            GuideEvent::Close => self.close_guide(window, cx),
            GuideEvent::Blur => {
                window.focus(&self.focus);
                cx.notify();
            }
            GuideEvent::TakeShot => self.screenshot_for_guide(cx),
            GuideEvent::Notice(text) => self.show_notice(text.clone(), cx),
            &GuideEvent::AskWithShot(question) => {
                self.screenshot_for_guide(cx);
                self.guide
                    .update(cx, |guide, cx| guide.ask_starter(question, window, cx));
            }
        }
    }

    /// Shows the pill that opens Guide while the pointer moves in the
    /// empty margin beside the screens, and hides it once it rests.
    fn show_guide_pill(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.guide_open || self.game.session().is_none() || self.menu.is_some() {
            return;
        }
        let x = f32::from(event.position.x);
        let right = self
            .placements
            .iter()
            .map(|placement| placement.rect.x + placement.rect.width)
            .fold(0.0, f32::max);
        if right <= 0.0 || x < right + INSET {
            return;
        }
        let shown = self.guide_pill.is_some();
        self.guide_pill = Some(Instant::now());
        if !shown {
            // A pill taken away under the pointer never heard it leave.
            self.guide_pill_hovered = false;
            cx.notify();
        }
        self.hide_guide_pill_later(cx);
    }

    /// Hides the pill after [`GUIDE_PILL_REST`], unless the pointer is on
    /// it then.
    fn hide_guide_pill_later(&mut self, cx: &mut Context<Self>) {
        self.guide_pill_hide = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GUIDE_PILL_REST).await;
            this.update(cx, |this, cx| {
                if !this.guide_pill_hovered {
                    this.guide_pill = None;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// "Ask about Omega Ruby ⌘J", quiet grey words in the window's top
    /// right corner that light up under the pointer.
    fn guide_pill_element(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let title = self
            .guide
            .read(cx)
            .short_title()
            .or_else(|| self.guide_game(cx).map(|game| game.context.short_title()))?;
        let quiet = |alpha: u8| rgba(crate::theme::with_alpha(0x00FF_FFFF, alpha));
        // Lit by state, not a hover style: text takes its colour when laid
        // out, before a hover style applies.
        let lit = self.guide_pill_hovered;
        // Off macOS it stands in the bar that drags the window.
        let pill = chrome::over_drag_area(
            div()
                .id("guide-pill")
                .flex()
                .items_center()
                .gap(px(6.0))
                .h(px(CORNER_PLATE))
                .px(px(CORNER_PLATE / 3.0))
                .rounded_full()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(if lit { rgb(0x00ED_EDF0) } else { rgb(READOUT) })
                .when(lit, |pill| pill.bg(quiet(0x14)))
                .cursor_pointer()
                .child(format!("Ask about {title}"))
                .child(
                    div()
                        .text_color(quiet(if lit { 0x8C } else { 0x59 }))
                        .child(keys::shortcut(shortcut::GUIDE)),
                )
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    this.guide_pill_hovered = *hovered;
                    if !*hovered {
                        this.hide_guide_pill_later(cx);
                    }
                    cx.notify();
                }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, window, cx| this.open_guide(window, cx)))
                .children(probe::part("pill:guide")),
        );
        Some(pill.with_animation(
            "guide-pill-in",
            Animation::new(Duration::from_millis(160)),
            |pill, delta| pill.opacity(delta),
        ))
    }

    /// Pauses the game and opens the game menu over it.
    fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.game.session() else {
            return;
        };
        if self.menu.is_some() {
            return;
        }
        // The active player's game, which Start Over erases.
        let player = self.input.player().min(session.player_rom_paths.len() - 1);
        let rom = session.player_rom_paths[player].clone();
        let title = self
            .library
            .read(cx)
            .title_of(&rom)
            .unwrap_or_else(|| file_name(&rom));
        let controls = key_guide::columns(session.info.system, self.input.bindings());
        // Let go at once, before pausing, so no quick tap waits under the
        // menu to reach the game when it resumes.
        self.release_all();
        self.send_debug(debugger::Command::Pause);
        let cheats = self.menu_cheats();
        let menu = cx.new(|_| GameMenu::new(title, cheats, controls));
        let subscription = cx.subscribe_in(&menu, window, Self::menu_event);
        self.menu = Some((menu, subscription));
        // The menu says progress is saved; make it so.
        self.save_checkpoint();
        cx.notify();
    }

    /// Closes the game menu, if open, and plays on.
    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_none() {
            return;
        }
        self.send_debug(debugger::Command::Continue);
        cx.notify();
    }

    /// Closes the game menu, if open, and plays on, returning whether it
    /// was open: a shortcut that acts on the game (pause, reset, load
    /// state) ends the menu's pause, so the game never runs or changes
    /// unseen under it.
    fn leave_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.menu.is_some();
        self.close_menu(cx);
        open
    }

    fn menu_event(
        &mut self,
        _: &Entity<GameMenu>,
        event: &MenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match *event {
            MenuEvent::Resume => self.close_menu(cx),
            MenuEvent::ToggleCheat(index) => self.toggle_cheat(index, cx),
            MenuEvent::EditCheats => self.edit_cheats(&EditCheats, window, cx),
            MenuEvent::StartOver => self.start_over(window, cx),
            MenuEvent::ExitToLibrary => self.close_game(window, cx),
        }
    }

    /// Saves where the game is as its checkpoint, to pick up from when it
    /// next starts, with the battery saves it goes with. The emulation
    /// thread finishes this before it stops.
    fn save_checkpoint(&self) {
        self.send(Command::SaveCheckpoint(CHECKPOINT));
    }

    /// The active player's cheat file, while a game runs.
    fn cheat_file(&self) -> Option<PathBuf> {
        let session = self.game.session()?;
        Some(session.player_files_of(self.input.player()).cheats())
    }

    /// The active player's cheats, as the game menu lists them.
    fn menu_cheats(&self) -> Vec<CheatRow> {
        let (Some(session), Some(path)) = (self.game.session(), self.cheat_file()) else {
            return Vec::new();
        };
        let text = cheats::read(&path).unwrap_or_default();
        emu_cheats::parse(session.info.system, &text)
            .map(|list| {
                list.iter()
                    .map(|cheat| CheatRow {
                        name: cheat.name.clone(),
                        enabled: cheat.enabled,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Shows the cheat file's cheats in the game menu, if it is open.
    fn refresh_menu_cheats(&mut self, cx: &mut Context<Self>) {
        let Some((menu, _)) = &self.menu else {
            return;
        };
        let cheats = self.menu_cheats();
        menu.clone()
            .update(cx, |menu, cx| menu.set_cheats(cheats, cx));
    }

    /// Switches the active player's cheat `index` on or off, in its file,
    /// and applies the file.
    fn toggle_cheat(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(path) = self.cheat_file() else {
            return;
        };
        let listed = self.menu_cheats();
        let Some(cheat) = listed.get(index) else {
            return;
        };
        let text = cheats::read(&path).unwrap_or_default();
        match cheats::set_enabled(&text, listed.len(), index, !cheat.enabled) {
            Some(toggled) => {
                if let Err(error) = persist::write_atomic(&path, toggled.as_bytes()) {
                    self.show_notice(format!("Cannot save {}: {error}", path.display()), cx);
                    return;
                }
            }
            None => {
                let message = "Give these codes a [name] line to switch them here";
                self.show_notice(message, cx);
                return;
            }
        }
        if let Some(message) = self.reload_cheats() {
            self.show_notice(message, cx);
        }
        self.refresh_menu_cheats(cx);
    }

    fn library_event(
        &mut self,
        _: &Entity<LibraryView>,
        event: &LibraryEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            LibraryEvent::Play(path) => self.open(vec![path.clone()], window, cx),
            LibraryEvent::AddGames => self.add_to_library(&AddToLibrary, window, cx),
            LibraryEvent::OpenSettings => self.show_settings(cx),
            LibraryEvent::Randomize(path) => self.show_randomizer(Some(path.clone()), window, cx),
            LibraryEvent::Notice(text) => self.show_notice(text.clone(), cx),
        }
    }

    fn open_rom(&mut self, _: &OpenRom, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_for_file("Open", window, cx, |this, path, window, cx| {
            this.open(vec![path], window, cx);
        });
    }

    /// Off macOS, where Open ROM… takes only files: opens an RPG Maker
    /// game from its folder.
    fn open_game_folder(
        &mut self,
        _: &OpenGameFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_for(
            Choose::Folders,
            "Open",
            window,
            cx,
            |this, path, window, cx| {
                this.open(vec![path], window, cx);
            },
        );
    }

    /// Off macOS: asks for RPG Maker games' folders and adds them to the
    /// library.
    fn add_game_folders(
        &mut self,
        _: &AddGameFolders,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_chosen(Choose::Folders, window, cx);
    }

    /// Opens the menu the window draws off macOS, or closes it if it is
    /// open: from its button, or with F10 from the keyboard, which chooses
    /// its first menu.
    fn toggle_app_menu(
        &mut self,
        from_keyboard: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.app_menu.is_some() {
            self.close_app_menu(window, cx);
            return;
        }
        if cfg!(target_os = "macos") {
            return;
        }
        let menus = cx.get_menus().unwrap_or_else(|| {
            crate::app::menus(self.menu_checks())
                .into_iter()
                .map(gpui::Menu::owned)
                .collect()
        });
        let anchor = gpui::point(px(chrome::MENU_BUTTON_LEFT), px(TOP_BAR - 4.0));
        let menu = cx.new(|cx| AppMenu::new(menus, anchor, from_keyboard, window, cx));
        let subscription = cx.subscribe_in(&menu, window, Self::app_menu_event);
        let before = window.focused(cx);
        menu.read(cx).focus(window);
        self.app_menu = Some((menu, subscription, before));
        cx.notify();
    }

    /// Closes the menu the window draws, giving the keyboard back to what
    /// had it.
    fn close_app_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, _, before)) = self.app_menu.take() {
            before.unwrap_or_else(|| self.focus.clone()).focus(window);
            cx.notify();
        }
    }

    fn app_menu_event(
        &mut self,
        _: &Entity<AppMenu>,
        event: &AppMenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_app_menu(window, cx);
        if let AppMenuEvent::Run(action) = event {
            window.dispatch_action(action.boxed_clone(), cx);
        }
    }

    /// The layout the window shows: the user's, but with a linked game's
    /// consoles side by side until the user picks an arrangement.
    fn shown_layout(&self) -> Layout {
        if self.linked_side_by_side {
            Layout {
                arrangement: Arrangement::Horizontal,
                ..self.settings.layout
            }
        } else {
            self.settings.layout
        }
    }

    fn arrange(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        // Chosen over a linked game's side by side, so shown at once.
        self.linked_side_by_side = false;
        self.set_layout(
            Layout {
                arrangement,
                ..self.settings.layout
            },
            cx,
        );
    }

    fn swap_screens(&mut self, cx: &mut Context<Self>) {
        self.set_layout(
            Layout {
                swapped: !self.settings.layout.swapped,
                ..self.settings.layout
            },
            cx,
        );
    }

    fn rotate(&mut self, rotation: Rotation, cx: &mut Context<Self>) {
        self.show_notice(
            match rotation {
                Rotation::Upright => "Screens upright".to_owned(),
                _ => format!("Screens turned {}° clockwise", rotation.degrees()),
            },
            cx,
        );
        self.set_layout(
            Layout {
                rotation,
                ..self.settings.layout
            },
            cx,
        );
    }

    fn change_gap(&mut self, cx: &mut Context<Self>) {
        let gap = layout::next_gap(self.settings.layout.gap);
        let message = if gap <= 0.0 {
            "No gap between screens".to_owned()
        } else if gap == layout::HINGE_GAP {
            format!("Screen gap {gap} px, as across the DS hinge")
        } else {
            format!("Screen gap {gap} px")
        };
        self.show_notice(message, cx);
        self.set_layout(
            Layout {
                gap,
                ..self.settings.layout
            },
            cx,
        );
    }

    /// Switches to `layout` and saves it for next launch.
    fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        self.settings.layout = layout;
        cx.notify();
        self.save_settings(cx);
    }

    /// Draws DS and 3DS 3D at `resolution` from now on, and saves that for
    /// next launch. The next draw sends the core the scale it comes to.
    fn set_resolution_3d(&mut self, resolution: Resolution3d, cx: &mut Context<Self>) {
        self.settings.resolution_3d = resolution;
        let system = self.game.session().map(|session| session.info.system);
        let three_d = matches!(system, Some(System::Nds | System::N3ds) | None);
        let mut message = match (resolution, system) {
            (Resolution3d::MatchDisplay, Some(system @ (System::Nds | System::N3ds))) => format!(
                "3D resolution matches the display, up to {}×",
                picture::match_display_cap(system)
            ),
            (Resolution3d::MatchDisplay, _) => format!(
                "3D resolution matches the display, up to {}× for DS games and {}× for 3DS games",
                picture::match_display_cap(System::Nds),
                picture::match_display_cap(System::N3ds)
            ),
            (Resolution3d::Times(1), _) => "3D at the native resolution".to_owned(),
            (Resolution3d::Times(scale), _) => format!("3D at {scale}× resolution"),
        };
        if !three_d && resolution != Resolution3d::MatchDisplay {
            message.push_str(", for DS and 3DS games");
        }
        self.show_notice(message, cx);
        self.save_settings(cx);
        cx.notify();
    }

    /// Starts or stops listening to the computer's microphone for DS games.
    fn toggle_microphone(&mut self, cx: &mut Context<Self>) {
        if self.microphone.take().is_some() {
            let command = self.input.stop_hearing();
            self.send(command);
            self.show_notice("Microphone off", cx);
            return;
        }
        match microphone::open() {
            Ok(microphone) => {
                self.microphone = Some(microphone);
                self.show_notice("Microphone on: DS games hear how loud it is", cx);
            }
            Err(error) => self.show_notice(format!("{error:#}"), cx),
        }
    }

    /// Passes on how loud the microphone has been since the last frame,
    /// while it is on, as the active player's microphone input.
    fn listen(&mut self) {
        let Some((_, loudness)) = &self.microphone else {
            return;
        };
        let heard = loudness.take();
        if let Some(command) = self.input.hear(heard) {
            self.send(command);
        }
    }

    /// The settings in force, to save or to show: the ones held, with
    /// fast-forward and slow motion as `controls` has them latched now.
    pub fn settings(&self) -> Settings {
        Settings {
            fast_forward_on: self.input.fast_forward_latched(),
            slow_motion_on: self.input.slow_motion_latched(),
            ..self.settings
        }
    }

    /// The settings as the window shows them, for the settings window: a
    /// linked game's consoles side by side, while they are.
    pub fn shown_settings(&self) -> Settings {
        Settings {
            layout: self.shown_layout(),
            ..self.settings()
        }
    }

    /// Switches to `settings`, as the menu items for each would, and saves
    /// them for next launch. The speeds, which only the settings file sets,
    /// stay as they are. Its layout is a change to the one shown (see
    /// [`EmulatorView::shown_settings`]).
    pub fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let shown = self.shown_layout();
        if settings.layout != shown {
            // An arrangement picked ends a linked game's side by side, as
            // from the menu; any other change keeps it, and keeps the
            // user's own arrangement for after.
            let arrangement = if settings.layout.arrangement == shown.arrangement {
                self.settings.layout.arrangement
            } else {
                self.linked_side_by_side = false;
                settings.layout.arrangement
            };
            self.set_layout(
                Layout {
                    arrangement,
                    ..settings.layout
                },
                cx,
            );
        }
        if settings.resolution_3d != self.settings.resolution_3d {
            self.set_resolution_3d(settings.resolution_3d, cx);
        }
        if settings.picture != self.settings.picture {
            self.set_picture(settings.picture, cx);
        }
        if settings.backdrop != self.settings.backdrop {
            self.set_backdrop(settings.backdrop, cx);
        }
        if settings.window_controls != self.settings.window_controls {
            self.set_window_controls(settings.window_controls, cx);
        }
        if (settings.guide_thinking, settings.guide_lookups)
            != (self.settings.guide_thinking, self.settings.guide_lookups)
        {
            self.settings.guide_thinking = settings.guide_thinking;
            self.settings.guide_lookups = settings.guide_lookups;
            self.tell_guide_settings(cx);
            self.save_settings(cx);
        }
    }

    /// Hands Guide's settings to its panel.
    fn tell_guide_settings(&self, cx: &mut App) {
        let (effort, lookups) = (self.settings.guide_thinking, self.settings.guide_lookups);
        self.guide.update(cx, |guide, _| {
            guide.effort = effort;
            guide.lookups = lookups;
        });
    }

    /// Fills the window around the screens with `backdrop` from now on,
    /// and saves that for next launch.
    fn set_backdrop(&mut self, backdrop: Backdrop, cx: &mut Context<Self>) {
        self.settings.backdrop = backdrop;
        if let Some(session) = self.game.session_mut() {
            self.stale_images
                .extend(session.ambient.take().map(|glow| glow.image));
        }
        cx.notify();
        self.save_settings(cx);
    }

    /// Shows or hides the window's close, minimise and zoom buttons and the
    /// bar they sit in, and saves that for next launch.
    fn set_window_controls(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.settings.window_controls = shown;
        cx.notify();
        self.save_settings(cx);
    }

    /// Writes the settings for next launch, noting any failure.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        let Some(path) = &self.settings_path else {
            return;
        };
        let settings = self.settings();
        if let Err(error) = settings.save(path) {
            let message = format!("Cannot save settings to {}: {error}", path.display());
            self.show_notice(message, cx);
        }
    }

    /// Runs fast-forward at `speed` from now on, at once if it is on,
    /// and saves that for next launch.
    fn set_fast_forward(&mut self, speed: Speed, cx: &mut Context<Self>) {
        self.settings.speed.fast_forward = speed;
        if self.input.pace() == Pace::FastForward {
            self.send(Command::SetSpeed(speed));
        }
        self.refresh_menus(cx);
        self.show_notice(format!("Fast-forward speed: {}", speed.label()), cx);
        self.save_settings(cx);
    }

    /// Shows or hides the speed readout, and saves that for next launch.
    fn toggle_fps(&mut self, cx: &mut Context<Self>) {
        self.settings.show_fps = !self.settings.show_fps;
        if self.settings.show_fps {
            self.start_readout(cx);
        } else {
            self.readout_task = None;
            self.speed_readout = None;
            self.fps_meter = None;
        }
        self.refresh_menus(cx);
        cx.notify();
        self.save_settings(cx);
    }

    /// Measures the speed for the readout every [`READOUT_INTERVAL`], until
    /// it is hidden.
    fn start_readout(&mut self, cx: &mut Context<Self>) {
        self.readout_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(READOUT_INTERVAL).await;
                if this.update(cx, |this, cx| this.update_readout(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Measures the speed since the last time, for the readout.
    fn update_readout(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.game.session() else {
            self.speed_readout = None;
            return;
        };
        let now = Instant::now();
        let frames = session.emulator.frames_shown();
        self.speed_readout = match &mut self.fps_meter {
            Some((game, meter)) if *game == self.sessions => {
                let fps = meter.sample(now, frames);
                Some(meter::readout(fps, session.info.frame_rate.hz()).into())
            }
            _ => {
                // A new game: measure from here.
                self.fps_meter = Some((self.sessions, Meter::new(now, frames)));
                None
            }
        };
        cx.notify();
    }

    /// Rebuilds the menus to check what is chosen now.
    fn refresh_menus(&self, cx: &mut Context<Self>) {
        cx.set_menus(crate::app::menus(self.menu_checks()));
    }

    /// What the menus show checked now.
    fn menu_checks(&self) -> MenuChecks {
        MenuChecks {
            fast_forward: self.settings.speed.fast_forward,
            show_fps: self.settings.show_fps,
        }
    }

    /// Opens the debugger window, or brings it to the front.
    fn show_debugger(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.debugger
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        // The new window draws at once and reads this view, so open it
        // once this update is over.
        let main = cx.entity();
        cx.defer(move |cx| {
            let opened = DebuggerWindow::open(main.clone(), cx);
            main.update(cx, |this, cx| match opened {
                Ok(handle) => this.debugger = Some(handle),
                Err(error) => this.show_notice(format!("Cannot open the debugger: {error:#}"), cx),
            });
        });
    }

    /// Opens the settings window, or brings it to the front.
    fn show_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.settings_window
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        // The new window draws at once and reads this view, so open it
        // once this update is over.
        let main = cx.entity();
        cx.defer(move |cx| {
            let opened = SettingsWindow::open(main.clone(), cx);
            main.update(cx, |this, cx| match opened {
                Ok(handle) => this.settings_window = Some(handle),
                Err(error) => this.show_notice(format!("Cannot open settings: {error:#}"), cx),
            });
        });
    }

    /// Opens the randomize window, or brings it to the front with the
    /// library's games read anew, with `game` chosen if given. A new window
    /// opens once the games it offers are read, off the UI thread, so it
    /// opens showing them.
    fn show_randomizer(&mut self, game: Option<PathBuf>, window: &Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.randomize_window
            && handle
                .update(cx, |randomizer, window, cx| {
                    window.activate_window();
                    randomizer.refresh(game.clone(), cx);
                })
                .is_ok()
        {
            return;
        }
        if self.opening_randomizer {
            return;
        }
        self.opening_randomizer = true;
        let games = randomize_window::library_games(self.library.read(cx));
        let read = cx
            .background_executor()
            .spawn(async move { randomize_window::candidates(games) });
        let main_window = window.window_handle();
        let library = self.library.clone();
        cx.spawn(async move |this, cx| {
            let candidates = read.await;
            let Some(main) = this.upgrade() else {
                return;
            };
            cx.update(|cx| {
                let opened =
                    RandomizeWindow::open(&main, main_window, library, candidates, game, cx);
                main.update(cx, |this, cx| {
                    this.opening_randomizer = false;
                    match opened {
                        Ok(handle) => this.randomize_window = Some(handle),
                        Err(error) => {
                            this.show_notice(format!("Cannot open the randomizer: {error:#}"), cx);
                        }
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// Plays the game at `path`, as a click in the library would.
    pub fn play(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open(vec![path], window, cx);
    }

    /// Sends a step for the CPU the debugger window shows, if it is open.
    fn step_in_debugger(&mut self, step: fn(usize) -> debugger::Command, cx: &mut Context<Self>) {
        let cpu = self
            .debugger
            .and_then(|handle| handle.read(cx).ok().map(DebuggerWindow::cpu));
        match cpu {
            Some(cpu) => self.send_debug(step(cpu)),
            None => self.debugger = None,
        }
    }

    /// What the debugger window needs to know about the running game.
    pub fn debug_target(&self) -> Option<DebugTarget> {
        let session = self.game.session()?;
        Some(DebugTarget {
            session: self.sessions,
            system: session.info.system,
            player: self.input.player(),
            snapshot: session.emulator.debug_snapshot(),
        })
    }

    /// Sends a debugger request to the running game, if there is one.
    pub fn send_debug(&self, command: debugger::Command) {
        self.send(Command::Debug(command));
    }

    /// The gamepad cursor, drawn over every copy of the touchscreen and
    /// filled in while it touches.
    fn cursor_marks(&self) -> Vec<impl IntoElement + use<>> {
        const RADIUS: f32 = 6.0;
        let (Some(cursor), Some((screen, size))) = (self.input.cursor(), self.touchscreen()) else {
            return Vec::new();
        };
        // Centre the mark on the pixel it touches.
        let pixel = cursor.pixel();
        let centre = (f32::from(pixel.x) + 0.5, f32::from(pixel.y) + 0.5);
        let touching = self.input.touching();
        self.placements
            .iter()
            .filter(|placement| placement.screen == screen)
            .map(|placement| {
                let (x, y) = placement.window_point(size, centre);
                div()
                    .absolute()
                    .left(px(x - RADIUS))
                    .top(px(y - RADIUS))
                    .size(px(RADIUS * 2.0))
                    .rounded_full()
                    .border_2()
                    .border_color(rgb(0x00FF_FFFF))
                    .bg(if touching {
                        rgba(0xFFFF_FFAA)
                    } else {
                        rgba(0x0000_0066)
                    })
            })
            .collect()
    }

    /// Rebuilds every screen's image at the next render, after a change
    /// in how frames become images. The images shown stay until the new
    /// ones replace them, which on a busy GPU can take a frame or two;
    /// a batch already on the GPU was made the old way, so it is dropped.
    fn rebuild_images(&mut self) {
        if let Some(session) = self.game.session_mut() {
            for (key, _) in session.images.iter_mut().flatten() {
                *key = key.stale();
            }
            session.upscaling.clear();
            self.stale_images
                .extend(session.ambient.take().map(|glow| glow.image));
        }
    }

    /// Shows games made for the original Game Boy with `colors` from the
    /// next one opened, and saves that for next launch. A game already
    /// running keeps its console until it is opened again.
    fn set_dmg_colors(&mut self, colors: emu_core::DmgColors, cx: &mut Context<Self>) {
        self.settings.dmg_colors = colors;
        let shown = match colors {
            emu_core::DmgColors::Grey => "in grey, as on a Game Boy",
            _ => "in colour, as on a Game Boy Color",
        };
        self.show_notice(
            format!("Original Game Boy games: {shown}, from the next game opened"),
            cx,
        );
        self.save_settings(cx);
    }

    /// Shows the screens as `picture` from now on, and saves that for next
    /// launch.
    fn set_picture(&mut self, picture: Picture, cx: &mut Context<Self>) {
        self.settings.picture = picture;
        if let Some(session) = self.game.session() {
            session
                .emulator
                .send(Command::SetTextureUpscaling(picture.texture_upscaling()));
        }
        self.start_upscaler(cx);
        self.rebuild_images();
        self.show_notice(format!("Picture: {}", picture.label()), cx);
        self.save_settings(cx);
    }

    /// Starts the upscaler's GPU on a background thread, if the picture
    /// upscales and it has not been started. Compiling its shaders takes
    /// about 130 ms, which the first upscaled frame would otherwise wait
    /// for; screens are drawn as sharp pixels until it is ready.
    fn start_upscaler(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.upscaler, Upscaler::Idle) || !self.settings.picture.upscales() {
            return;
        }
        let started = cx.background_executor().spawn(async { GpuDevice::start() });
        cx.spawn(async move |this, cx| {
            let started = started.await;
            this.update(cx, |view, cx| {
                view.upscaler = match started {
                    Ok(device) => Upscaler::Ready(Gpu::new(device)),
                    Err(error) => Upscaler::Off(Some(error)),
                };
                // Screens drawn sharp meanwhile are drawn again.
                view.rebuild_images();
                cx.notify();
            })
            .ok();
        })
        .detach();
        self.upscaler = Upscaler::Starting;
    }

    /// Turns colour correction on or off, and saves that for next launch.
    fn toggle_color_correction(&mut self, cx: &mut Context<Self>) {
        self.settings.color_correction = !self.settings.color_correction;
        self.rebuild_images();
        self.show_notice(
            if self.settings.color_correction {
                "Colour correction on"
            } else {
                "Colour correction off"
            },
            cx,
        );
        self.save_settings(cx);
    }
}

impl Session {
    /// Each player's game's own files.
    fn player_files(&self) -> &[GameFiles] {
        &self.files[..self.player_rom_paths.len()]
    }

    /// The own files of `player`'s game, or of the last player's if there
    /// are fewer.
    fn player_files_of(&self, player: usize) -> &GameFiles {
        &self.files[player.min(self.player_rom_paths.len() - 1)]
    }

    /// Stops the emulation thread, which first finishes what it was asked,
    /// such as saving the checkpoint, and writes the battery saves.
    /// Returns what it said meanwhile that was not yet shown, as it would
    /// have been ([`said_by`]).
    fn finish(self) -> Vec<String> {
        let Self {
            emulator,
            events,
            _audio: audio,
            ..
        } = self;
        drop(emulator);
        // The thread has stopped, so the device can go.
        drop(audio);
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| said_by(&event).map(Cow::into_owned))
            .collect()
    }
}

impl Render for EmulatorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        trace::event("render", &[]);
        // Full screen shows its own bar on hover, buttons and all.
        let fullscreen = window.is_fullscreen();
        let show_controls = self.settings.window_controls || fullscreen;
        if self.shown_controls != Some(show_controls) {
            window_chrome::set_controls_visible(window, show_controls);
            self.shown_controls = Some(show_controls);
        }
        let viewport = window.viewport_size();
        let viewport = (f32::from(viewport.width), f32::from(viewport.height));
        let scale_factor = window.scale_factor();

        let mut root = div()
            .track_focus(&self.focus)
            .key_context("Emulator")
            .size_full()
            .relative()
            .text_color(rgb(0x00DD_DDDD))
            .on_action(cx.listener(Self::open_rom))
            .on_action(cx.listener(Self::add_to_library))
            .on_action(cx.listener(|this, _: &OpenStore, _, cx| this.open_store(cx)))
            // The library's own: nothing while a game runs.
            .on_action(cx.listener(|this, _: &GameInfo, _, cx| {
                if this.game.session().is_none() {
                    this.library
                        .update(cx, |library, cx| library.toggle_chosen_menu(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &RemoveFromLibrary, _, cx| {
                if this.game.session().is_none() && this.app_menu.is_none() {
                    this.library
                        .update(cx, |library, cx| library.remove_chosen(cx));
                } else if !cfg!(target_os = "macos") {
                    // Off macOS it is Delete alone, which is the game's
                    // while one runs, and the menu's while it is open.
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CloseGame, window, cx| this.close_game(window, cx)))
            .on_action(cx.listener(Self::connect_link_cable))
            .on_action(cx.listener(Self::edit_cheats))
            .on_action(cx.listener(Self::run_script))
            .on_action(cx.listener(|this, _: &StopScript, _, _| this.send(Command::StopScript)))
            .on_action(cx.listener(|this, _: &SwitchPlayer, _, cx| {
                let players = this.game.session().map_or(1, |s| s.info.players);
                this.switch_player((this.input.player() + 1) % players.max(1), cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePause, _, cx| {
                // Under the game menu the game is paused already: resume.
                if !this.leave_menu(cx) {
                    this.send(Command::TogglePause);
                }
            }))
            .on_action(cx.listener(|this, _: &Reset, _, cx| {
                this.leave_menu(cx);
                this.send(Command::Reset);
            }))
            .on_action(cx.listener(|this, _: &SaveState, _, _| {
                this.send(Command::SaveState(this.settings.state_slot));
            }))
            .on_action(cx.listener(|this, _: &LoadState, _, cx| {
                this.leave_menu(cx);
                this.send(Command::LoadState(this.settings.state_slot));
            }))
            .on_action(
                cx.listener(|this, _: &NextStateSlot, _, cx| this.change_state_slot(true, cx)),
            )
            .on_action(cx.listener(|this, _: &PreviousStateSlot, _, cx| {
                this.change_state_slot(false, cx);
            }))
            .on_action(
                cx.listener(|this, _: &ToggleFastForward, _, cx| this.toggle_fast_forward(cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSlowMotion, _, cx| this.toggle_slow_motion(cx)))
            .on_action(cx.listener(|this, _: &LayoutVertical, _, cx| {
                this.arrange(Arrangement::Vertical, cx);
            }))
            .on_action(cx.listener(|this, _: &LayoutHorizontal, _, cx| {
                this.arrange(Arrangement::Horizontal, cx);
            }))
            .on_action(cx.listener(|this, _: &LayoutSingle, _, cx| {
                this.arrange(Arrangement::Single, cx);
            }))
            .on_action(cx.listener(|this, _: &LayoutHybrid, _, cx| {
                this.arrange(Arrangement::Hybrid, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapScreens, _, cx| this.swap_screens(cx)))
            .on_action(cx.listener(|this, _: &RotateClockwise, _, cx| {
                this.rotate(this.settings.layout.rotation.clockwise(), cx);
            }))
            .on_action(cx.listener(|this, _: &RotateAnticlockwise, _, cx| {
                this.rotate(this.settings.layout.rotation.anticlockwise(), cx);
            }))
            .on_action(cx.listener(|this, _: &NextScreenGap, _, cx| this.change_gap(cx)))
            .on_action(cx.listener(|this, _: &ToggleColorCorrection, _, cx| {
                this.toggle_color_correction(cx);
            }))
            .on_action(cx.listener(|this, _: &ShowDebugger, _, cx| this.show_debugger(cx)))
            .on_action(cx.listener(|this, _: &FastForward2x, _, cx| {
                this.set_fast_forward(speed::FAST_FORWARD_CHOICES[0], cx);
            }))
            .on_action(cx.listener(|this, _: &FastForward3x, _, cx| {
                this.set_fast_forward(speed::FAST_FORWARD_CHOICES[1], cx);
            }))
            .on_action(cx.listener(|this, _: &FastForward4x, _, cx| {
                this.set_fast_forward(speed::FAST_FORWARD_CHOICES[2], cx);
            }))
            .on_action(cx.listener(|this, _: &FastForwardUnlimited, _, cx| {
                this.set_fast_forward(speed::FAST_FORWARD_CHOICES[3], cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFps, _, cx| this.toggle_fps(cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| this.show_settings(cx)))
            .on_action(cx.listener(|this, _: &Randomize, window, cx| {
                this.show_randomizer(None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleWindowControls, _, cx| {
                this.set_window_controls(!this.settings.window_controls, cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3dMatchDisplay, _, cx| {
                this.set_resolution_3d(Resolution3d::MatchDisplay, cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d1x, _, cx| {
                this.set_resolution_3d(Resolution3d::Times(1), cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d2x, _, cx| {
                this.set_resolution_3d(Resolution3d::Times(2), cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d3x, _, cx| {
                this.set_resolution_3d(Resolution3d::Times(3), cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d4x, _, cx| {
                this.set_resolution_3d(Resolution3d::Times(4), cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d6x, _, cx| {
                this.set_resolution_3d(Resolution3d::Times(6), cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d8x, _, cx| {
                this.set_resolution_3d(Resolution3d::Times(8), cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleMicrophone, _, cx| {
                this.toggle_microphone(cx);
            }))
            .on_action(cx.listener(|this, _: &SaveScreenshot, _, _| {
                this.send(Command::SaveScreenshot);
            }))
            .on_action(cx.listener(|this, _: &ToggleGuide, window, cx| {
                this.toggle_guide(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ScreenshotForGuide, _, cx| {
                this.screenshot_for_guide(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRecording, _, _| {
                this.send(Command::ToggleRecording);
            }))
            .on_action(cx.listener(|this, _: &crate::app::DmgColorsGrey, _, cx| {
                this.set_dmg_colors(emu_core::DmgColors::Grey, cx);
            }))
            .on_action(
                cx.listener(|this, _: &crate::app::DmgColorsGameBoyColor, _, cx| {
                    this.set_dmg_colors(emu_core::DmgColors::ByTitle, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &PictureAuto, _, cx| {
                this.set_picture(Picture::Auto, cx);
            }))
            .on_action(cx.listener(|this, _: &PictureSharpPixels, _, cx| {
                this.set_picture(Picture::SharpPixels, cx);
            }))
            .on_action(cx.listener(|this, _: &PictureRoundedPixels, _, cx| {
                this.set_picture(Picture::RoundedPixels, cx);
            }))
            .on_action(cx.listener(|this, _: &PictureSmooth, _, cx| {
                this.set_picture(Picture::Smooth, cx);
            }))
            .on_action(cx.listener(|this, _: &PictureLcdGrid, _, cx| {
                this.set_picture(Picture::LcdGrid, cx);
            }))
            .on_action(cx.listener(|this, _: &PictureScanlines, _, cx| {
                this.set_picture(Picture::Scanlines, cx);
            }))
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| {
                window.toggle_fullscreen();
            }))
            .on_action(cx.listener(Self::open_game_folder))
            .on_action(cx.listener(Self::add_game_folders))
            .on_action(cx.listener(|this, _: &ShowAppMenu, window, cx| {
                this.toggle_app_menu(true, window, cx);
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            // macOS reports a left button let go with Control held as the
            // right one.
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_drop(cx.listener(Self::drop_files))
            .children(probe::begin());
        // Listening only while a DS runs leaves the menu item disabled
        // otherwise.
        let system = self.game.session().map(|session| session.info.system);
        if system.is_some_and(has_gba_slot) {
            root = root.on_action(cx.listener(Self::insert_gba_cartridge));
        }
        // The debugger's step keys work from here too while it is open.
        if self.debugger.is_some() {
            root = root
                .on_action(cx.listener(|this, _: &DebugStep, _, cx| {
                    this.step_in_debugger(debugger::Command::Step, cx);
                }))
                .on_action(cx.listener(|this, _: &DebugStepOver, _, cx| {
                    this.step_in_debugger(debugger::Command::StepOver, cx);
                }))
                .on_action(cx.listener(|this, _: &DebugNextFrame, _, cx| {
                    this.step_in_debugger(|_| debugger::Command::NextFrame, cx);
                }));
        }

        // Black around the screens in full screen, unless the backdrop
        // glows; the dark stage around them in a window; the library's
        // white without a game.
        root = root.bg(rgb(if fullscreen {
            0x0000_0000
        } else if self.game.session().is_some() {
            STAGE
        } else {
            BACKGROUND
        }));
        // Off macOS, the top bar drags the window where the app frames
        // it, under everything drawn there (`chrome.rs`).
        let framed = !cfg!(target_os = "macos") && chrome::frame(window) != chrome::Frame::System;
        if framed && !fullscreen && !self.touchscreen_in_top_bar() {
            let strip = div().absolute().top_0().left_0().right_0().h(px(TOP_BAR));
            root = root.child(chrome::drag_area(strip, "drag-area", window, cx));
        }
        let shown = self.shown_layout();
        let mut upscaler_failed = None;
        let mut guide_width = None;
        if let Some(session) = self.game.session_mut() {
            let consoles = consoles(&session.info);
            // Guide takes the margin beside the screens, or a column the
            // screens make room for when the margin is too narrow.
            let (stage, width) = guide_stage(
                self.guide_open,
                &shown,
                session.info.screens,
                consoles,
                viewport,
            );
            guide_width = width;
            // On whole device pixels, so upscaled screens draw one to one.
            self.placements = shown
                .place_consoles(session.info.screens, consoles, stage)
                .into_iter()
                .map(|placement| Placement {
                    rect: placement.rect.snapped(scale_factor),
                    ..placement
                })
                .collect();
            // 3D as large as the screens are now shown, for Match Display,
            // as long as the game keeps the speed asked for.
            let mut scale = render_scale(
                self.settings.resolution_3d,
                &session.info,
                &self.placements,
                scale_factor,
            );
            if self.settings.resolution_3d == Resolution3d::MatchDisplay {
                scale = session.keep_speed(scale, self.settings.speed.speed(self.input.pace()));
            }
            if scale != session.render_scale {
                trace::event("scale", &[u64::from(scale)]);
                session.render_scale = scale;
                session.emulator.send(Command::SetRenderScale(scale));
                // Measure the speed afresh at the new scale, once it has
                // settled.
                session.keeping.measured = None;
                session.keeping.settling = true;
            }
            // Frames keep in step with the refreshes of the display the
            // window is on.
            let display = window_chrome::display_key(window, cx);
            if session.display != Some(display) {
                session.display = Some(display);
                let refresh = window_chrome::refresh_interval(window, cx);
                session.emulator.send(Command::SetRefresh(refresh));
            }
            let refreshing = Instant::now();
            let seen = session.sequence;
            session.take_frames();
            if session.sequence != seen {
                trace::event("take", &[session.sequence]);
            }
            let upscaling;
            (upscaler_failed, upscaling) = session.refresh_images(
                &self.placements,
                scale_factor,
                self.settings.color_correction,
                self.settings.picture,
                &mut self.upscaler,
                window,
            );
            if upscaling && self.upscale_wake.is_none() {
                self.upscale_wake = Some(cx.spawn(async move |this, cx| {
                    // Draws again once the GPU has finished the screens it
                    // has, checking every millisecond.
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(1))
                            .await;
                        let done = this
                            .update(cx, |view, cx| {
                                let done = !view.upscaler.busy() || view.upscaler.finished();
                                if done {
                                    view.upscale_wake = None;
                                    cx.notify();
                                }
                                done
                            })
                            .unwrap_or(true);
                        if done {
                            break;
                        }
                    }
                }));
            }
            if session.sequence != seen {
                let made = trace::micros(refreshing.elapsed());
                trace::event("drawn", &[session.sequence, made]);
            }
            if self.settings.backdrop == Backdrop::Ambient {
                if session.refresh_ambient(&self.placements, self.settings.color_correction, window)
                {
                    // New frames draw the fade on as they come, so the
                    // window redraws no more often than the game. Once they
                    // stop, as when paused, it draws the rest itself.
                    let wait = session.info.frame_rate.frame_duration() * 2;
                    if session.taken_at.is_some_and(|at| at.elapsed() < wait) {
                        session.fade_wake = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(wait).await;
                            this.update(cx, |_, cx| cx.notify()).ok();
                        }));
                    } else {
                        window.request_animation_frame();
                    }
                }
                if let Some(glow) = &session.ambient {
                    root = root.child(ambient_layer(glow, viewport));
                }
            }
            // Guide's colours follow the game's, at most every two seconds.
            if guide_width.is_some() && self.guide.read(cx).wants_palette(Instant::now()) {
                let colors = match &session.ambient {
                    Some(glow) => Some(glow.fade.shown().colors().to_vec()),
                    None => {
                        let correction = session
                            .correction
                            .as_ref()
                            .filter(|_| self.settings.color_correction);
                        ambient::picture(&session.frames, &self.placements, correction)
                            .map(|picture| picture.colors().to_vec())
                    }
                };
                if let Some(colors) = colors.filter(|colors| !colors.is_empty()) {
                    self.guide
                        .update(cx, |guide, cx| guide.see_picture(&colors, cx));
                }
            }
            root = root.children(
                ambient::shadows(&self.placements)
                    .into_iter()
                    .map(screen_shadow),
            );
            for (placement, image) in self.placements.iter().zip(&session.images) {
                let Some((_, image)) = image else {
                    continue;
                };
                let rect = placement.rect;
                root = root.child(
                    img(Arc::clone(image))
                        .absolute()
                        .left(px(rect.x))
                        .top(px(rect.y))
                        .w(px(rect.width))
                        .h(px(rect.height))
                        .object_fit(ObjectFit::Fill),
                );
            }
            let overlay = session.emulator.overlay();
            root = root.children(overlay_layers(
                &overlay,
                &self.placements,
                session.info.screens,
            ));
        } else {
            self.placements.clear();
            root = root.child(self.library.clone());
        }
        // Off macOS the upscaler has no GPU half yet and the CPU's draws
        // the pictures, as it is meant to: nothing to say.
        let expected =
            !cfg!(target_os = "macos") && matches!(upscaler_failed, Some(GpuError::NoDevice));
        if let Some(error) = upscaler_failed.filter(|_| !expected) {
            self.show_notice(format!("Upscaling is off: {error}"), cx);
        }
        root = root.children(self.cursor_marks());
        // Off macOS the menu's button and the window's own (where the app
        // draws them) stand in the top bar as the traffic lights do on
        // macOS, hidden with them, and shown again as the pointer comes
        // into the bar; in full screen only the menu's.
        let off_mac = !cfg!(target_os = "macos");
        let controls_shown = self.settings.window_controls && !fullscreen;
        let chrome_shown =
            off_mac && (controls_shown || self.title_hovered || self.app_menu.is_some());
        let shaded = if off_mac {
            chrome_shown
        } else {
            controls_shown
        };
        if shaded && self.game.session().is_some() {
            root = root.child(top_bar_shade());
        }
        // Guide, drawn as its own cached view: game frames reuse its last
        // layout and paint, and its streaming redraws only itself.
        if let Some(width) = guide_width {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(width))
                    .child(
                        gpui::AnyView::from(self.guide.clone())
                            .cached(StyleRefinement::default().size_full()),
                    ),
            );
        }
        // The top right corner, on the traffic lights' midline and clear
        // of Guide's panel: the pill that opens Guide, then the recording
        // mark.
        let pill = (guide_width.is_none()
            && self.game.session().is_some()
            && self.guide_pill.is_some()
            && self.menu.is_none())
        .then(|| self.guide_pill_element(cx))
        .flatten();
        let recording = self
            .game
            .session()
            .is_some_and(|session| session.recording)
            .then(recording_mark);
        if pill.is_some() || recording.is_some() {
            // As far in from the right as down from the top, for the
            // plates and, with their padding, for the words in them; clear
            // of the window's buttons where the app draws them.
            let corner = TOP_BAR / 2.0 - CORNER_PLATE / 2.0;
            let buttons = if guide_width.is_none() && chrome_shown && !fullscreen {
                chrome::trailing_room(window, true)
            } else {
                0.0
            };
            root = root.child(
                div()
                    .absolute()
                    .top(px(corner))
                    .right(px(guide_width.unwrap_or(0.0) + corner + buttons))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .children(pill)
                    .children(recording),
            );
        }
        if let Some((menu, _)) = &self.menu {
            root = root.child(menu.clone());
            // The game menu's backdrop takes every press; its top bar still
            // drags the window where the app frames it.
            if framed && !fullscreen {
                let strip = div().absolute().top_0().left_0().right_0().h(px(TOP_BAR));
                root = root.child(chrome::drag_area(strip, "menu-drag-area", window, cx));
            }
        }
        if off_mac {
            let tone = if self.game.session().is_some() {
                Tone::Stage
            } else {
                Tone::Paper
            };
            if chrome_shown {
                root = root.child(
                    div()
                        .absolute()
                        .top(px(chrome::MENU_BUTTON_TOP))
                        .left(px(chrome::MENU_BUTTON_LEFT))
                        .child(
                            chrome::menu_button(tone, self.app_menu.is_some())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_app_menu(false, window, cx);
                                }))
                                .children(probe::part("button:menu")),
                        ),
                );
                if !fullscreen && let Some(buttons) = chrome::buttons_for(window, tone, true) {
                    root = root.child(div().absolute().top_0().right_0().child(buttons));
                }
            }
            // While they are hidden, the pointer coming into the top bar
            // shows them: the bar notes it, without taking any presses.
            if !controls_shown {
                root = root.child(
                    div()
                        .id("title-reveal")
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(TOP_BAR))
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if this.title_hovered != *hovered {
                                this.title_hovered = *hovered;
                                cx.notify();
                            }
                        })),
                );
            }
            root = root.children(chrome::window_edges(window, tone, true));
            if let Some((menu, _, _)) = &self.app_menu {
                root = root.child(menu.clone());
            }
        }

        // A preview only lives as long as its notice.
        if self.notice.is_none() {
            self.stale_images.extend(self.slot_image.take());
        }
        for image in self.stale_images.drain(..) {
            window.drop_image(image).ok();
        }
        // Over a game, which hides the library's tiles of games being
        // added, what is being added shows over any notice.
        let adding = self
            .game
            .session()
            .and_then(|_| self.library.read(cx).adding_status())
            .map(adding_card);
        // Only over a game: the last game's reading would otherwise stay
        // over the library, as the readout's timer stops with the game.
        // Not under the game menu, where the game is paused.
        let speed = self
            .speed_readout
            .clone()
            .filter(|_| {
                self.settings.show_fps && self.game.session().is_some() && self.menu.is_none()
            })
            .map(speed_readout);
        let notice = self.notice.as_ref().map(|notice| {
            let mut card = overlay_plate(div()).flex_col().gap_1();
            if let Some(image) = &self.slot_image {
                card = card.child(
                    img(Arc::clone(image))
                        .w(px(160.0))
                        .object_fit(ObjectFit::Contain),
                );
            }
            card.child(notice.clone()).children(probe::part("notice"))
        });
        // Down the left side's foot, clear of Guide in the right margin:
        // what is being added, the notice, then the speed readout.
        if adding.is_some() || notice.is_some() || speed.is_some() {
            let cards = div()
                .absolute()
                .left(px(INSET))
                .flex()
                .flex_col()
                .items_start()
                .gap(px(6.0));
            // Off macOS, where a notice may carry a system's error, a long
            // one wraps short of the right inset and of Guide.
            let cards = if off_mac {
                let room = viewport.0 - 2.0 * INSET - guide_width.unwrap_or(0.0);
                cards.max_w(px(room.max(0.0)))
            } else {
                cards
            };
            // Over the game menu, notices go to the top, clear of it.
            let cards = if self.menu.is_some() {
                cards.top(px(TOP_BAR + 4.0))
            } else {
                cards.bottom(px(INSET))
            };
            root = root.child(cards.children(adding).children(notice).children(speed));
        }
        root.children(probe::end("main"))
    }
}

/// What a script drew, over every placement of the screens it drew on,
/// each clipped to its screen.
fn overlay_layers(draws: &[Draw], placements: &[Placement], screens: &[ScreenSize]) -> Vec<Div> {
    if draws.is_empty() {
        return Vec::new();
    }
    placements
        .iter()
        .filter_map(|placement| {
            let size = *screens.get(placement.screen)?;
            let mut shapes = draws
                .iter()
                .filter(|draw| draw.screen() == placement.screen)
                .peekable();
            shapes.peek()?;
            let rect = placement.rect;
            let layer = div()
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.width))
                .h(px(rect.height))
                .overflow_hidden();
            Some(layer.children(shapes.flat_map(|draw| shape(draw, placement, size))))
        })
        .collect()
}

/// One drawn shape as elements placed inside `placement`'s layer. Text
/// stays upright on turned screens, starting from where its corner lands.
fn shape(draw: &Draw, placement: &Placement, size: ScreenSize) -> Vec<Div> {
    let rect = placement.rect;
    // Window pixels per screen pixel.
    let scale = rect.width / f32::from(placement.rotation.turn_size(size).width.max(1));
    let at = |x: f32, y: f32| {
        let (left, top) = placement.window_point(size, (x, y));
        (left - rect.x, top - rect.y)
    };
    match draw {
        Draw::Rect {
            x,
            y,
            width,
            height,
            fill,
            outline,
            ..
        } => {
            let (x, y) = (*x as f32, *y as f32);
            let a = at(x, y);
            let b = at(x + *width as f32, y + *height as f32);
            let mut element = div()
                .absolute()
                .left(px(a.0.min(b.0)))
                .top(px(a.1.min(b.1)))
                .w(px((a.0 - b.0).abs()))
                .h(px((a.1 - b.1).abs()));
            if fill.is_visible() {
                element = element.bg(rgba(fill.rgba()));
            }
            if outline.is_visible() {
                element = element
                    .border(px(scale.max(1.0)))
                    .border_color(rgba(outline.rgba()));
            }
            vec![element]
        }
        Draw::Text {
            x, y, text, color, ..
        } => {
            if !color.is_visible() {
                return Vec::new();
            }
            let (left, top) = at(*x as f32, *y as f32);
            let height = Draw::TEXT_HEIGHT as f32 * scale;
            // A dark copy underneath keeps light text readable on light
            // screens, as emulators' script text has been.
            let shadow = (scale / 2.0).max(1.0);
            let shadow_alpha = u32::from(color.alpha()) * 3 / 4;
            let line = |offset: f32, color: u32| {
                div()
                    .absolute()
                    .left(px(left + offset))
                    .top(px(top + offset))
                    .whitespace_nowrap()
                    .font_family(fonts::MONO)
                    .text_size(px(height * 0.9))
                    .line_height(px(height))
                    .text_color(rgba(color))
                    .child(SharedString::from(text.clone()))
            };
            vec![line(shadow, shadow_alpha), line(0.0, color.rgba())]
        }
    }
}

/// The ambient backdrop stretched over the `viewport`, behind everything,
/// with its border drawn just past the edges, out of sight.
fn ambient_layer(glow: &Glow, viewport: (f32, f32)) -> impl IntoElement {
    let size = glow.fade.shown().size();
    let border = f32::from(ambient::BORDER);
    let reach = (
        border * viewport.0 / f32::from(size.width),
        border * viewport.1 / f32::from(size.height),
    );
    div()
        .absolute()
        .left_0()
        .top_0()
        .w(px(viewport.0))
        .h(px(viewport.1))
        .overflow_hidden()
        .child(
            img(Arc::clone(&glow.image))
                .absolute()
                .left(px(-reach.0))
                .top(px(-reach.1))
                .w(px(viewport.0 + 2.0 * reach.0))
                .h(px(viewport.1 + 2.0 * reach.1))
                .object_fit(ObjectFit::Fill),
        )
}

/// A soft shadow under screens covering `rect`, lifting them off the
/// backdrop.
fn screen_shadow(rect: layout::Rect) -> impl IntoElement {
    div()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.width))
        .h(px(rect.height))
        .shadow(vec![BoxShadow {
            color: gpui::hsla(0.0, 0.0, 0.0, 0.6),
            offset: gpui::point(px(0.0), px(8.0)),
            blur_radius: px(40.0),
            spread_radius: px(0.0),
        }])
}

/// The top bar: a shade over the top of the screens, darkest at the
/// window's edge and gone at the bar's foot, so the game shows through
/// behind the close, minimise and zoom buttons.
fn top_bar_shade() -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(TOP_BAR))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgba(TOP_BAR_SHADE), 0.0),
            linear_color_stop(rgba(TOP_BAR_SHADE & 0xFFFF_FF00), 1.0),
        ))
}

/// The plate under words over the game: the notice and the recording
/// mark. Its 12-pixel words on 16-pixel lines sit evenly in it.
fn overlay_plate(plate: Div) -> Div {
    plate
        .flex()
        .px_2()
        .py_1()
        .rounded(px(8.0))
        .bg(rgba(0x0000_00CC))
        .text_xs()
        .line_height(px(16.0))
}

/// What the library is adding, said over a game: `Unpacking Game · 42%`,
/// its words breathing while it is under way.
fn adding_card(status: String) -> Div {
    overlay_plate(div())
        .child(div().child(status).with_animation(
            "adding",
            Animation::new(Duration::from_millis(1600)).repeat(),
            |words, delta| words.opacity(0.45 + 0.55 * pulse(delta)),
        ))
        .children(probe::part("adding"))
}

/// A red dot and "REC" in the window's corner, on the traffic lights'
/// midline, while a recording is under way. The recording is of the core's
/// frames, so it never shows this.
fn recording_mark() -> Div {
    overlay_plate(div())
        .items_center()
        .gap_1()
        .child(div().size(px(8.0)).rounded_full().bg(rgb(0x00E5_3935)))
        .child("REC")
        .children(probe::part("rec"))
}

/// What to tell the player briefly of the emulation thread's `event`, if
/// anything: its own words ([`Event::notice`]), but nothing of the
/// checkpoint being saved, which happens unasked, and loading it in words
/// of its own.
fn notice_of(event: &Event) -> Option<Cow<'_, str>> {
    match event {
        Event::StateSaved(CHECKPOINT) => None,
        Event::StateLoaded(CHECKPOINT) => Some("Picked up where you left off".into()),
        event => event.notice(),
    }
}

/// What the emulation thread's `event` tells the player, however it is
/// shown: a notice as [`notice_of`] words it, a warning, or why the game
/// stopped.
fn said_by(event: &Event) -> Option<Cow<'_, str>> {
    match event {
        Event::Warning(text) | Event::Stopped(text) => Some(text.as_str().into()),
        event => notice_of(event),
    }
}

/// The save-state slot after `slot`, or before it unless `forward`,
/// wrapping round.
fn next_slot(slot: u8, forward: bool) -> u8 {
    if forward {
        slot % STATE_SLOTS + 1
    } else {
        (slot + STATE_SLOTS - 2) % STATE_SLOTS + 1
    }
}

/// A save state's `width`×`height` RGB screenshot as an image, twice its
/// size.
fn slot_image(width: u16, height: u16, rgb: &[u8]) -> Arc<RenderImage> {
    let mut frame = Frame::new(ScreenSize::new(width, height));
    for (pixel, color) in frame.pixels_mut().iter_mut().zip(rgb.chunks_exact(3)) {
        *pixel = pack_rgb(color[0], color[1], color[2]);
    }
    video::render_image(&frame, 2)
}

/// The speed readout at the foot of the window's left side: grey words
/// on nothing, in figures of one width so they hold still as they change.
fn speed_readout(text: SharedString) -> Div {
    static FIGURES: LazyLock<FontFeatures> =
        LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));
    div()
        .font(Font {
            family: fonts::UI.into(),
            features: FIGURES.clone(),
            fallbacks: None,
            weight: FontWeight::MEDIUM,
            style: FontStyle::Normal,
        })
        .text_xs()
        .line_height(px(16.0))
        .text_color(rgb(READOUT))
        .child(text)
        .children(probe::part("speed"))
}

/// The menu action a gamepad hotkey runs.
fn shortcut_action(shortcut: Shortcut) -> Box<dyn gpui::Action> {
    match shortcut {
        Shortcut::SaveState => Box::new(SaveState),
        Shortcut::LoadState => Box::new(LoadState),
        Shortcut::NextSlot => Box::new(NextStateSlot),
        Shortcut::PreviousSlot => Box::new(PreviousStateSlot),
        Shortcut::Pause => Box::new(TogglePause),
        Shortcut::Reset => Box::new(Reset),
        Shortcut::ToggleFastForward => Box::new(ToggleFastForward),
        Shortcut::ToggleSlowMotion => Box::new(ToggleSlowMotion),
        Shortcut::SwapScreens => Box::new(SwapScreens),
        Shortcut::SwitchPlayer => Box::new(SwitchPlayer),
        Shortcut::FullScreen => Box::new(ToggleFullScreen),
    }
}

/// The 3D resolution to draw `info`'s game at: the one chosen, or for
/// Match Display, as large as `placements` show its screens on a display
/// of `scale_factor` device pixels to a logical one. Systems without 3D
/// stay at 1.
fn render_scale(
    resolution: Resolution3d,
    info: &CoreInfo,
    placements: &[Placement],
    scale_factor: f32,
) -> u32 {
    match resolution {
        Resolution3d::Times(scale) => scale,
        Resolution3d::MatchDisplay if matches!(info.system, System::Nds | System::N3ds) => {
            picture::matched_resolution(
                placements.iter().filter_map(|placement| {
                    let native = info.screens.get(placement.screen)?;
                    let rect = placement.rect;
                    let target = picture::device_size(rect.width, rect.height, scale_factor);
                    Some((target, placement.rotation.turn_size(*native)))
                }),
                picture::match_display_cap(info.system),
            )
        }
        Resolution3d::MatchDisplay => 1,
    }
}

/// How many consoles a session shows side by side, each laid out on its
/// own: one, or each player's for linked systems with several screens
/// each (two DSes). Single-screen consoles share one layout instead.
fn consoles(info: &CoreInfo) -> usize {
    let players = info.players.max(1);
    if info.screens.len() > players {
        players
    } else {
        1
    }
}

/// What a file dialog asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choose {
    /// Games: their files, and on macOS their folders too, as an RPG
    /// Maker game is one. Linux's and Windows's dialogs can't take both at
    /// once: asked for folders, they take only folders.
    Games,
    /// RPG Maker games' folders, which off macOS get a dialog of their own.
    Folders,
}

impl Choose {
    fn options(self, multiple: bool, prompt: &'static str) -> PathPromptOptions {
        let (files, directories) = match self {
            Self::Games => (true, cfg!(target_os = "macos")),
            Self::Folders => (false, true),
        };
        PathPromptOptions {
            files,
            directories,
            multiple,
            prompt: Some(prompt.into()),
        }
    }
}

/// What to say when a file dialog can't open: on Linux, when no desktop
/// portal runs to show one (GPUI asks xdg-desktop-portal over D-Bus).
pub fn dialog_failed(error: &anyhow::Error) -> String {
    let said = format!("{error:#}");
    let no_portal = ["xdg-desktop-portal", "ZBus", "org.freedesktop"]
        .iter()
        .any(|sign| said.contains(sign));
    if no_portal {
        "No file dialog: it needs the desktop portal (xdg-desktop-portal), which isn't running. Drop games on the window instead.".to_owned()
    } else {
        format!("The file dialog didn't open: {said}")
    }
}

/// Whether `system` takes GBA games in a second slot.
fn has_gba_slot(system: System) -> bool {
    emu_cores::second_slot(system) == Some(System::Gba)
}

/// Where the screens go with Guide `open`: the window as it is, with the
/// panel in the margin beside them, or less a column for the panel when the
/// margin is too narrow. Also the panel's width, while it is open.
fn guide_stage(
    open: bool,
    layout: &Layout,
    screens: &[ScreenSize],
    consoles: usize,
    viewport: (f32, f32),
) -> ((f32, f32), Option<f32>) {
    if !open {
        return (viewport, None);
    }
    let natural = layout.place_consoles(screens, consoles, viewport);
    let right = natural
        .iter()
        .map(|placement| placement.rect.x + placement.rect.width)
        .fold(0.0, f32::max);
    if let Some(width) = guide::width_in(viewport.0 - right, INSET) {
        return (viewport, Some(width));
    }
    let width = guide::COLUMN_WIDTH
        .min(viewport.0 * 0.6)
        .max(guide::MIN_WIDTH.min(viewport.0));
    (((viewport.0 - width).max(1.0), viewport.1), Some(width))
}

/// What Guide is told about the game at `rom`, for `system`, from the
/// library's record of it if it has one.
fn guide_info(rom: &Path, system: System, game: Option<library::Game>) -> GuideGame {
    use crate::rom_info::Platform as Shelf;
    use emu_guide::game::{GameContext, Kind, Platform};
    let stem = rom
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let shelf = game.as_ref().map(|game| game.platform);
    let platform = match (shelf, system) {
        (Some(Shelf::Gb), _) => Platform::GameBoy,
        (Some(Shelf::Gbc), _) => Platform::GameBoyColor,
        (Some(Shelf::Gba), _) | (None, System::Gba) => Platform::GameBoyAdvance,
        (Some(Shelf::Nds), _) | (None, System::Nds) => Platform::Ds,
        (Some(Shelf::N3ds), _) | (None, System::N3ds) => Platform::ThreeDs,
        (Some(Shelf::RpgMaker), _) | (None, System::RpgMaker) => Platform::RpgMaker,
        (None, _) => Platform::GameBoy,
    };
    let title = game.as_ref().map_or_else(
        || crate::game_db::display_title(&stem),
        |game| game.title.clone(),
    );
    let release = game.as_ref().and_then(|game| game.name.clone());
    let kind = if platform == Platform::RpgMaker {
        Kind::FanGame
    } else if let Some(at) = stem.find(" (Random ") {
        Kind::Randomized {
            base: stem[..at].to_owned(),
        }
    } else if release.is_some() {
        Kind::Retail
    } else {
        Kind::Unlisted
    };
    GuideGame {
        // By its system and file, so it is the same from the moment it
        // starts, before the library has read it.
        key: format!(
            "{}-{:08x}",
            system.name().to_lowercase().replace(' ', "-"),
            crc32fast::hash(rom.as_os_str().as_encoded_bytes())
        ),
        tint: theme_tint(platform),
        context: GameContext {
            title,
            release,
            platform,
            serial: game.as_ref().and_then(|game| game.serial.clone()),
            kind,
            play_time: game.map(|game| game.play_time),
        },
    }
}

/// A system's colour, as the library's plates use it, for Guide before a
/// game draws anything.
fn theme_tint(platform: emu_guide::game::Platform) -> u32 {
    use crate::rom_info::Platform as Shelf;
    use emu_guide::game::Platform;
    let shelf = match platform {
        Platform::GameBoy => Shelf::Gb,
        Platform::GameBoyColor => Shelf::Gbc,
        Platform::GameBoyAdvance => Shelf::Gba,
        Platform::Ds => Shelf::Nds,
        Platform::ThreeDs => Shelf::N3ds,
        Platform::RpgMaker => Shelf::RpgMaker,
    };
    crate::theme::tint(shelf).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_guide::game::{Kind, Platform};

    const DS: [ScreenSize; 2] = [ScreenSize::new(256, 192), ScreenSize::new(256, 192)];

    #[test]
    fn a_state_saved_or_loaded_is_told_by_its_slot_not_by_its_words() {
        let told = |event: Event| notice_of(&event).map(Cow::into_owned);
        // The slots the player saves to and loads from, in the words they
        // have always had.
        for slot in 1..=slots::LAST_FILE_SLOT {
            assert_eq!(
                told(Event::StateSaved(slot)),
                Some(format!("Saved state {slot}"))
            );
            assert_eq!(
                told(Event::StateLoaded(slot)),
                Some(format!("Loaded state {slot}"))
            );
        }
        // The checkpoint is saved unasked, and picked up in its own words.
        assert_eq!(told(Event::StateSaved(CHECKPOINT)), None);
        assert_eq!(
            told(Event::StateLoaded(CHECKPOINT)).as_deref(),
            Some("Picked up where you left off")
        );
        // Words are only ever shown: a script that says what the
        // checkpoint's notice once said is not taken for it.
        let said = format!("Saved state {CHECKPOINT}");
        assert_eq!(told(Event::Notice(said.clone())), Some(said.clone()));
        assert_eq!(told(Event::Failed(said.clone())), Some(said));
        // Warnings and the rest are not notices.
        assert_eq!(told(Event::Warning("kept".to_owned())), None);
        assert_eq!(told(Event::Stopped("stopped".to_owned())), None);
        assert_eq!(told(Event::PickedUp(true)), None);
        assert_eq!(told(Event::Frame), None);
        // What a game said as it stopped is worded the same way, with its
        // warnings and why it stopped.
        let left = |event: Event| said_by(&event).map(Cow::into_owned);
        assert_eq!(left(Event::StateSaved(CHECKPOINT)), None);
        assert_eq!(
            left(Event::StateLoaded(CHECKPOINT)).as_deref(),
            Some("Picked up where you left off")
        );
        assert_eq!(left(Event::StateSaved(2)).as_deref(), Some("Saved state 2"));
        assert_eq!(
            left(Event::Warning("kept".to_owned())).as_deref(),
            Some("kept")
        );
        assert_eq!(
            left(Event::Stopped("stopped".to_owned())).as_deref(),
            Some("stopped")
        );
        assert_eq!(left(Event::Recording(false)), None);
    }

    #[test]
    fn guide_sits_in_a_wide_margin_and_makes_room_in_a_narrow_one() {
        let layout = Layout::default();
        assert_eq!(
            guide_stage(false, &layout, &DS, 1, (1360.0, 860.0)),
            ((1360.0, 860.0), None)
        );
        // Stacked screens 860 tall are about 573 wide: margins of almost
        // 400 each, room enough.
        let (stage, width) = guide_stage(true, &layout, &DS, 1, (1360.0, 860.0));
        assert_eq!(stage, (1360.0, 860.0));
        assert!(width.is_some_and(|width| (guide::MIN_WIDTH..=guide::MAX_WIDTH).contains(&width)));
        // A narrow window: the screens move over for a column.
        let (stage, width) = guide_stage(true, &layout, &DS, 1, (800.0, 860.0));
        assert_eq!(width, Some(guide::COLUMN_WIDTH));
        assert_eq!(stage, (800.0 - guide::COLUMN_WIDTH, 860.0));
    }

    #[test]
    fn a_file_dialog_without_a_desktop_portal_says_so_plainly() {
        let missing = anyhow::anyhow!(
            "Couldn't open file picker due to missing xdg-desktop-portal implementation."
        );
        assert!(dialog_failed(&missing).starts_with("No file dialog: it needs the desktop portal"));
        let no_bus =
            anyhow::anyhow!("ZBus Error: Failed to connect to address `unix:path=/run/user/0/bus`");
        assert_eq!(dialog_failed(&no_bus), dialog_failed(&missing));
        let other = anyhow::anyhow!("the dialog was refused");
        assert_eq!(
            dialog_failed(&other),
            "The file dialog didn't open: the dialog was refused"
        );
    }

    #[test]
    fn guide_is_told_what_kind_of_copy_is_played() {
        let retail = library::Game {
            path: PathBuf::from("/games/Pokemon - Platinum Version (USA) (Rev 1).nds"),
            platform: crate::rom_info::Platform::Nds,
            crc32: 0x69D6_28E8,
            serial: Some("CPUE".to_owned()),
            title: "Pokemon: Platinum Version".to_owned(),
            name: Some("Pokemon - Platinum Version (USA) (Rev 1)".to_owned()),
            looked_up: true,
            play_time: Duration::from_secs(60),
        };
        let info = guide_info(&retail.path.clone(), System::Nds, Some(retail));
        assert_eq!(info.context.kind, Kind::Retail);
        assert_eq!(info.context.platform, Platform::Ds);
        assert_eq!(info.context.short_title(), "Platinum");
        let unread = guide_info(
            Path::new("/games/Pokemon - Platinum Version (USA) (Rev 1).nds"),
            System::Nds,
            None,
        );
        assert_eq!(info.key, unread.key);

        let random = Path::new("/games/Pokemon Emerald (Random 42).gba");
        let info = guide_info(random, System::Gba, None);
        assert_eq!(
            info.context.kind,
            Kind::Randomized {
                base: "Pokemon Emerald".to_owned()
            }
        );
        let hack = guide_info(Path::new("/games/Some Hack.gba"), System::Gba, None);
        assert_eq!(hack.context.kind, Kind::Unlisted);
        let fan = guide_info(Path::new("/games/Insurgence"), System::RpgMaker, None);
        assert_eq!(fan.context.kind, Kind::FanGame);
        // The same file is the same game, before and after the library
        // reads it.
        assert_eq!(
            guide_info(random, System::Gba, None).key,
            guide_info(random, System::Gba, None).key
        );
    }
}
