//! The main window: draws the running game's screens and turns keyboard,
//! gamepad, mouse and file events into emulator commands.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use emu_core::{
    Button, Buttons, CoreInfo, Frame, Input, ScreenSize, System, TouchPoint, rgb as pack_rgb,
};
use emu_script::Draw;
use gpui::{
    BoxShadow, Context, Div, Entity, ExternalPaths, FocusHandle, KeyDownEvent, KeyUpEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, PathPromptOptions,
    RenderImage, SharedString, Subscription, Task, Window, WindowBounds, WindowHandle, div, img,
    linear_color_stop, linear_gradient, prelude::*, px, rgb, rgba,
};

use crate::ambient::{self, Backdrop};
use crate::app::{
    AddToLibrary, CloseGame, ConnectLinkCable, DebugNextFrame, DebugStep, DebugStepOver,
    EditCheats, FilterLcdGrid, FilterNone, FilterScale2x, FilterScale3x, FilterScanlines,
    InsertGbaCartridge, LayoutHorizontal, LayoutHybrid, LayoutSingle, LayoutVertical, LoadState,
    NextScreenGap, NextStateSlot, OpenRom, OpenSettings, PreviousStateSlot, Reset, Resolution3d1x,
    Resolution3d2x, Resolution3d3x, Resolution3d4x, RotateAnticlockwise, RotateClockwise,
    RunScript, SaveScreenshot, SaveState, ShowDebugger, StopScript, SwapScreens, SwitchPlayer,
    TOP_BAR, ToggleColorCorrection, ToggleFastForward, ToggleFullScreen, ToggleKeyGuide,
    ToggleMicrophone, TogglePause, ToggleRecording, ToggleSlowMotion, ToggleWindowControls,
};
use crate::app::{
    FastForward2x, FastForward3x, FastForward4x, FastForwardUnlimited, MenuChecks, ToggleFps,
};
use crate::audio::{self, AudioOutput};
use crate::bindings::{self, Bindings, Held, Shortcut};
use crate::cheats;
use crate::chords::{Chords, Effect};
use crate::color::Correction;
use crate::controls::{Controls, HeldState};
use crate::cursor::{self, Cursor};
use crate::debugger::{self, Snapshot};
use crate::debugger_window::DebuggerWindow;
use crate::emulator::{self, Command, Emulator, Event, Loaded};
use crate::filter::Filter;
use crate::game_menu::{CheatRow, GameMenu, MenuEvent};
use crate::gamepad::{self, PadEvent};
use crate::key_guide;
use crate::layout::{self, Arrangement, Layout, Placement, Rotation};
use crate::library::{self, Library};
use crate::library_view::{LibraryEvent, LibraryView};
use crate::meter::{self, Meter};
use crate::microphone::{self, Loudness, Microphone};
use crate::persist;
use crate::settings::{self, Settings, WindowPlace};
use crate::settings_window::SettingsWindow;
use crate::slots::{self, STATE_SLOTS, SlotPreview};
use crate::speed::{self, Pace, Speed, Speeds};
use crate::theme::BACKGROUND;
use crate::video;
use crate::window_chrome;

const NOTICE_DURATION: Duration = Duration::from_secs(3);

/// How often the speed readout measures.
const READOUT_INTERVAL: Duration = Duration::from_millis(500);

/// Where the top bar's shade starts, at the window's top edge: dark enough
/// for the close, minimise and zoom buttons to show on a white screen.
const TOP_BAR_SHADE: u32 = 0x0000_0066;

/// How long the window must stay put before its place is saved, so a
/// drag or resize writes the settings once rather than on every step.
const PLACE_SAVE_DELAY: Duration = Duration::from_millis(500);

/// The source name under which a held mouse button presses A, off the
/// touchscreen.
const MOUSE: &str = "mouse:left";

/// The save-state slot that holds each game's checkpoint: where the
/// player left it, picked up when the game next starts.
const CHECKPOINT: u8 = 0;

/// The root view of the main window.
pub struct EmulatorView {
    focus: FocusHandle,
    /// The bindings in force: the user's, with the active player's game's
    /// own laid over them.
    bindings: Bindings,
    layout: Layout,
    /// Whether a linked game shows its consoles side by side whatever
    /// `layout`, the user's own, says; until it stops or the user picks
    /// an arrangement.
    linked_side_by_side: bool,
    /// Warnings from startup kept for the game it opens, whose loading
    /// notices would hide them.
    startup_notice: Option<String>,
    /// Where the window is, to open it there next launch.
    window_place: Option<WindowPlace>,
    /// Saves the window's place once it has stopped moving.
    place_save: Option<Task<()>>,
    /// Where the layout is saved for next launch, if there is a config
    /// folder.
    settings_path: Option<PathBuf>,
    /// How fast fast-forward and slow motion run, from the settings file.
    speeds: Speeds,
    /// How many times its own resolution DS 3D is drawn at.
    resolution_3d: u32,
    /// The computer's microphone, while the user has it on: off at every
    /// launch.
    microphone: Option<(Microphone, Loudness)>,
    /// How loud the microphone was when the emulation thread was last
    /// told.
    heard: u8,
    /// Show GBC and GBA colours as their screens did.
    color_correction: bool,
    /// How the screens are filtered on their way to the window.
    filter: Filter,
    /// What fills the window around the screens.
    backdrop: Backdrop,
    /// Whether the speed readout shows in the window's corner.
    show_fps: bool,
    /// What the speed readout says, once it has measured.
    speed_readout: Option<SharedString>,
    /// Measures the speed for the readout, with the number from
    /// `sessions` of the game it counts.
    fps_meter: Option<(u64, Meter)>,
    /// Updates the readout while it shows.
    readout_task: Option<Task<()>>,
    /// How games made for the original Game Boy are shown, from the next
    /// game opened.
    dmg_colors: emu_core::DmgColors,
    /// Whether the window shows its close, minimise and zoom buttons and
    /// the bar they sit in.
    window_controls: bool,
    /// Whether the keys that play the game show along the foot of the
    /// window.
    key_guide: bool,
    /// Whether the buttons were last shown or hidden, once they have been.
    shown_controls: Option<bool>,
    /// The debugger window, once opened.
    debugger: Option<WindowHandle<DebuggerWindow>>,
    /// The settings window, once opened.
    settings_window: Option<WindowHandle<SettingsWindow>>,
    /// The games, shown while none runs.
    library: Entity<LibraryView>,
    /// The game menu, while it is open over the paused game.
    menu: Option<(Entity<GameMenu>, Subscription)>,
    /// Games started so far, which numbers each for the debugger.
    sessions: u64,
    session: Option<Session>,
    /// A script from the command line, to run with the first game.
    startup_script: Option<PathBuf>,
    loading: Option<Task<()>>,
    placements: Vec<Placement>,
    controls: Controls,
    /// Gamepad hotkey chords in progress.
    chords: Chords,
    /// The gamepad's touchscreen cursor, once a gamepad has used it.
    cursor: Option<Cursor>,
    /// What the emulation thread was last told is held.
    sent: HeldState,
    /// Where the emulation thread was last told the screen is touched.
    sent_touch: Option<TouchPoint>,
    /// While the mouse button is held down after a click on the
    /// touchscreen: which of `placements` was clicked, and where the mouse
    /// touches.
    stylus: Option<(usize, TouchPoint)>,
    /// The player keyboard and gamepads control, in multi-player sessions.
    active_player: usize,
    state_slot: u8,
    notice: Option<SharedString>,
    notice_timer: Option<Task<()>>,
    /// Screenshot of the selected save-state slot, shown with the notice.
    slot_image: Option<Arc<RenderImage>>,
    /// Images no longer shown, to remove from GPUI's atlas on next render.
    stale_images: Vec<Arc<RenderImage>>,
    slot_preview_task: Option<Task<()>>,
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
    emulator: Emulator,
    info: CoreInfo,
    /// Each cartridge's ROM: one per player, then any in a second slot.
    rom_paths: Vec<PathBuf>,
    /// Each player's ROM, which names their cheats and the states.
    player_rom_paths: Vec<PathBuf>,
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
    /// Per placement: the image drawn there and what it shows.
    images: Vec<Option<(ImageKey, Arc<RenderImage>)>>,
    /// The ambient backdrop, while it is shown.
    ambient: Option<Glow>,
    _events: Task<()>,
    /// Dropped after `emulator`, so the thread stops before the device.
    _audio: Option<AudioOutput>,
}

/// What an image shows: a screen, enlarged by a whole factor and turned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageKey {
    screen: usize,
    factor: usize,
    rotation: Rotation,
}

/// The ambient backdrop: what it was last sampled from, its fade toward
/// that, and the image of where the fade has got to.
struct Glow {
    /// The placements and the frames' sequence number sampled.
    sampled: (Vec<Placement>, u64),
    fade: ambient::Fade,
    image: Arc<RenderImage>,
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
        let subscriptions = vec![
            cx.subscribe_in(&library, window, Self::library_event),
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    // Keys released while another window has focus never
                    // send key-up.
                    this.release_all();
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
            // place not yet saved is saved here.
            cx.on_release(|this, _| {
                if this.place_save.take().is_some()
                    && let Some(path) = &this.settings_path
                {
                    this.settings().save(path).ok();
                }
            }),
            // Stop the emulation thread on quit so the battery save is flushed.
            cx.on_app_quit(|this, cx| {
                if this.place_save.take().is_some() {
                    this.save_settings(cx);
                }
                this.save_checkpoint();
                this.session = None;
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
            bindings: Bindings::default(),
            layout: Layout::default(),
            linked_side_by_side: false,
            startup_notice: None,
            window_place: None,
            place_save: None,
            settings_path,
            speeds: Speeds::default(),
            resolution_3d: Settings::default().resolution_3d,
            microphone: None,
            heard: 0,
            color_correction: true,
            filter: Filter::default(),
            backdrop: Backdrop::default(),
            show_fps: false,
            speed_readout: None,
            fps_meter: None,
            readout_task: None,
            dmg_colors: Settings::default().dmg_colors,
            window_controls: Settings::default().window_controls,
            key_guide: Settings::default().key_guide,
            shown_controls: None,
            debugger: None,
            settings_window: None,
            library,
            menu: None,
            sessions: 0,
            session: None,
            startup_script: script,
            loading: None,
            placements: Vec::new(),
            controls: Controls::default(),
            chords: Chords::default(),
            cursor: None,
            sent: HeldState::default(),
            sent_touch: None,
            stylus: None,
            active_player: 0,
            state_slot: 1,
            notice: None,
            notice_timer: None,
            slot_image: None,
            stale_images: Vec::new(),
            slot_preview_task: None,
            _gamepad: gamepad_task,
            _subscriptions: subscriptions,
        };
        let mut warnings = Vec::new();
        match loaded_bindings {
            Some(Ok(bindings)) => view.bindings = bindings,
            Some(Err(error)) => warnings.push(format!("{error}; using default bindings")),
            None => {}
        }
        match loaded_settings {
            Some(Ok(settings)) => {
                view.layout = settings.layout;
                view.speeds = settings.speed;
                view.resolution_3d = settings.resolution_3d;
                view.filter = settings.filter;
                view.backdrop = settings.backdrop;
                view.show_fps = settings.show_fps;
                view.dmg_colors = settings.dmg_colors;
                view.window_controls = settings.window_controls;
                view.key_guide = settings.key_guide;
                view.color_correction = settings.color_correction;
                view.state_slot = settings.state_slot;
                view.window_place = settings.window_place;
                if settings.fast_forward_on {
                    view.controls.toggle_fast_forward();
                }
                if settings.slow_motion_on {
                    view.controls.toggle_slow_motion();
                }
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
        view.refresh_menus(cx);
        if view.show_fps {
            view.start_readout(cx);
        }
        if !roms.is_empty() {
            view.open(roms, window, cx);
        }
        view
    }

    /// Loads the ROMs off the UI thread, then starts them: one game, a DS
    /// game with a GBA game in its slot, or two games joined by a link
    /// cable.
    fn open(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        // Stopping writes the running game's battery saves. Reopening one
        // of its games (to link it or insert a cartridge) must read those,
        // not the copy on disk from up to a second ago.
        let saves: Vec<String> = paths.iter().map(|path| persist::backup_key(path)).collect();
        let reopening = self.session.as_ref().is_some_and(|session| {
            let mut roms = session.rom_paths.iter();
            roms.any(|rom| saves.contains(&persist::backup_key(rom)))
        });
        if reopening {
            self.save_checkpoint();
            self.stop(window);
        }
        let name = paths
            .iter()
            .map(|path| file_name(path))
            .collect::<Vec<_>>()
            .join(" + ");
        self.show_notice(format!("Loading {name}…"), cx);
        let dmg_colors = self.dmg_colors;
        let load = cx
            .background_executor()
            .spawn(async move { emulator::load(&paths, dmg_colors) });
        self.loading = Some(cx.spawn_in(window, async move |this, cx| {
            let loaded = load.await;
            this.update_in(cx, |this, window, cx| match loaded {
                Ok(loaded) => this.start(loaded, &name, window, cx),
                Err(error) => this.show_notice(format!("{error:#}"), cx),
            })
            .ok();
        }));
    }

    fn start(&mut self, loaded: Loaded, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Stop the previous game first so its battery save is flushed.
        self.save_checkpoint();
        self.stop(window);
        let info = loaded.info();
        let (audio_output, audio_sink) = match audio::open(info.audio_sample_rate) {
            Ok((output, sink)) => (Some(output), Some(sink)),
            Err(error) => {
                self.show_notice(format!("No sound: {error:#}"), cx);
                (None, None)
            }
        };
        let (events, receiver) = async_channel::unbounded();
        let rom_paths = loaded.rom_paths().to_vec();
        let played = rom_paths.clone();
        self.library
            .update(cx, |library, cx| library.add(played, cx));
        let player_rom_paths = loaded.player_rom_paths().to_vec();
        let emulator = Emulator::start(loaded, events, audio_sink);
        emulator.send(Command::SetRenderScale(self.resolution_3d));
        // Pick up where the player left off.
        if persist::state_path(&player_rom_paths, CHECKPOINT).exists() {
            emulator.send(Command::LoadState(CHECKPOINT));
        }
        let task = cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                if this.update(cx, |this, cx| this.handle(event, cx)).is_err() {
                    break;
                }
            }
        });
        self.session = Some(Session {
            emulator,
            info,
            rom_paths,
            player_rom_paths,
            frames: Vec::new(),
            sequence: 0,
            cheat_texts: vec![None; info.players.max(1)],
            bindings: Vec::new(),
            correction: Correction::for_system(info.system),
            recording: false,
            images: Vec::new(),
            ambient: None,
            _events: task,
            _audio: audio_output,
        });
        // The new emulator starts with nothing held; resend what is.
        self.sent = HeldState::default();
        self.sent_touch = None;
        self.stylus = None;
        self.cursor = None;
        self.active_player = 0;
        let bindings = self.reload_bindings();
        self.sync_held();
        self.sessions += 1;
        if info.players > 1 {
            // Linked consoles sit side by side; a DS's own two screens
            // keep the chosen layout.
            if consoles(&info) == 1 {
                self.linked_side_by_side = true;
            }
            window.set_window_title(&format!("{name} — {} link", info.system.name()));
        } else {
            window.set_window_title(&format!("{name} — {}", info.system.name()));
        }
        let running = format!("Running {name}");
        // Left on last time, so worth saying.
        let pace = if self.controls.fast_forward_latched() {
            Some("fast-forward on (⌘F)".to_owned())
        } else if self.controls.slow_motion_latched() {
            Some("slow motion on (⌘⇧F)".to_owned())
        } else {
            None
        };
        let notes = [
            Some(running),
            pace,
            bindings,
            self.reload_cheats(),
            self.startup_notice.take(),
        ];
        let notice: Vec<String> = notes.into_iter().flatten().collect();
        self.show_notice(notice.join(" · "), cx);
        if let Some(script) = self.startup_script.take() {
            self.run_script_file(script, cx);
        }
    }

    /// Asks for a Lua script, then runs it alongside the game.
    fn run_script(&mut self, _: &RunScript, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_none() {
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
        let session = self.session.as_mut()?;
        session.bindings = session
            .player_rom_paths
            .iter()
            .map(|rom| {
                let path = persist::bindings_path(rom);
                user.overlay_file(&path).unwrap_or_else(|error| {
                    problems.push(format!("{} not applied: {error}", file_name(&path)));
                    user.clone()
                })
            })
            .collect();
        self.bindings = session
            .bindings
            .get(self.active_player)
            .cloned()
            .unwrap_or(user);
        (!problems.is_empty()).then(|| problems.join(" · "))
    }

    /// Re-reads each player's cheat file and sends the ones that changed to
    /// the emulator. Returns a summary of the changes, if any.
    fn reload_cheats(&mut self) -> Option<String> {
        let session = self.session.as_mut()?;
        let players = session.player_rom_paths.len();
        let mut changes = Vec::new();
        for (player, rom) in session.player_rom_paths.iter().enumerate() {
            let text = cheats::read(&persist::cheats_path(rom));
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
        let Some(rom) = self.session.as_ref().and_then(|session| {
            let player = self.active_player.min(session.player_rom_paths.len() - 1);
            session.player_rom_paths.get(player)
        }) else {
            self.show_notice("Open a game first, then edit its cheats", cx);
            return;
        };
        let path = persist::cheats_path(rom);
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

    fn stop(&mut self, window: &mut Window) {
        // A preview still loading belongs to the game stopping.
        self.slot_preview_task = None;
        self.linked_side_by_side = false;
        if let Some(session) = self.session.take() {
            let images = session.images.into_iter().flatten();
            let ambient = session.ambient.map(|glow| glow.image);
            for image in images.map(|(_, image)| image).chain(ambient) {
                window.drop_image(image).ok();
            }
        }
    }

    fn handle(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Frame => {
                self.listen();
                cx.notify();
            }
            // Wakes the debugger window, which redraws with this view.
            Event::Debug => cx.notify(),
            Event::Notice(text) => {
                if let Some(text) = checkpoint_notice(text) {
                    self.show_notice(text, cx);
                }
            }
            Event::Recording(on) => {
                if let Some(session) = &mut self.session {
                    session.recording = on;
                }
                cx.notify();
            }
        }
    }

    /// Shows `text` for a few seconds in place of any notice shown, and
    /// of the save state screenshot shown with it.
    fn show_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
        // A screenshot belongs to the slot's notice; `show_slot_preview`
        // brings the new slot's, if it has one.
        self.stale_images.extend(self.slot_image.take());
        let timer = cx.background_executor().timer(NOTICE_DURATION);
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
        if let Some(session) = &self.session {
            session.emulator.send(command);
        }
    }

    fn send_input(&self) {
        self.send(Command::SetInput {
            player: self.active_player,
            input: Input {
                buttons: self.sent.buttons,
                touch: self.sent_touch,
                microphone: if self.sent.blow {
                    Input::LOUDEST
                } else {
                    self.heard
                },
            },
        });
    }

    fn send_turbo(&self) {
        self.send(Command::SetTurbo {
            player: self.active_player,
            buttons: self.sent.turbo,
            frames: self.bindings.turbo_frames(),
        });
    }

    /// Hands keyboard and gamepad control to `player`, releasing everything
    /// the previous player held.
    fn switch_player(&mut self, player: usize, cx: &mut Context<Self>) {
        let Some(players) = self.session.as_ref().map(|s| s.info.players) else {
            return;
        };
        if players < 2 || player == self.active_player || player >= players {
            return;
        }
        // The mouse touches the screen it was pressed on, not the next
        // player's.
        if self.stylus.take().is_some() {
            self.sent_touch = None;
        }
        self.send(Command::SetInput {
            player: self.active_player,
            input: Input::default(),
        });
        self.send(Command::SetTurbo {
            player: self.active_player,
            buttons: Buttons::NONE,
            frames: self.bindings.turbo_frames(),
        });
        self.active_player = player;
        // Keyboard and gamepads now play the other player's game.
        if let Some(bindings) = self
            .session
            .as_ref()
            .and_then(|session| session.bindings.get(player))
        {
            self.bindings = bindings.clone();
        }
        self.send_input();
        self.send_turbo();
        self.show_notice(format!("Controlling player {}", player + 1), cx);
    }

    /// Tells the emulation thread about any change in held inputs or in
    /// where the mouse or the gamepad cursor touches the screen. The mouse
    /// comes first.
    fn sync_held(&mut self) {
        let state = self.controls.state();
        let touch = match self.stylus {
            Some((_, at)) => Some(at),
            None => self.cursor_touch(state.touch),
        };
        if state.buttons != self.sent.buttons
            || touch != self.sent_touch
            || state.blow != self.sent.blow
        {
            self.sent.buttons = state.buttons;
            self.sent.blow = state.blow;
            self.sent_touch = touch;
            self.send_input();
        }
        if state.turbo != self.sent.turbo {
            self.sent.turbo = state.turbo;
            self.send_turbo();
        }
        if state.pace != self.sent.pace {
            self.send(Command::SetSpeed(self.speeds.speed(state.pace)));
        }
        if state.rewind != self.sent.rewind {
            self.send(Command::SetRewind(state.rewind));
        }
        self.sent = state;
    }

    /// Where the gamepad cursor touches, if its touch input is `held`. The
    /// cursor appears in the middle of the touchscreen if no gamepad has
    /// moved it yet.
    fn cursor_touch(&mut self, held: bool) -> Option<TouchPoint> {
        if !held {
            return None;
        }
        let (_, size) = self.touchscreen()?;
        Some(
            self.cursor
                .get_or_insert_with(|| Cursor::centre(size))
                .pixel(),
        )
    }

    /// The touchscreen of the console being played: its index and size.
    /// Each of two linked DSes has its own.
    fn touchscreen(&self) -> Option<(usize, ScreenSize)> {
        let info = &self.session.as_ref()?.info;
        let per_console = info.screens.len() / consoles(info);
        let screen = info.touch_screen? + self.active_player.min(consoles(info) - 1) * per_console;
        Some((screen, *info.screens.get(screen)?))
    }

    fn release_all(&mut self) {
        self.stylus = None;
        self.controls.release_all();
        self.chords.clear();
        self.sync_held();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Without a game, keys search and choose in the library.
        if self.session.is_none() {
            self.library
                .update(cx, |library, cx| library.key_down(event, cx));
            return;
        }
        // While the game menu is open, keys are the menu's.
        if let Some((menu, _)) = &self.menu {
            menu.clone().update(cx, |menu, cx| menu.key_down(event, cx));
            return;
        }
        let modifiers = event.keystroke.modifiers;
        let plain = !modifiers.platform && !modifiers.control && !modifiers.alt;
        if event.keystroke.key == "escape" && plain && !event.is_held {
            self.open_menu(window, cx);
            return;
        }
        // Command and Control chords are menu shortcuts, not game input.
        if !modifiers.platform && !modifiers.control {
            let key = &event.keystroke.key;
            let action = self.bindings.key_action(key);
            self.controls.set(format!("key:{key}"), action, true);
            self.sync_held();
            // Redraw the cursor if a key touches with it, even when paused.
            cx.notify();
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke.key;
        self.controls.set(format!("key:{key}"), None, false);
        self.sync_held();
        cx.notify();
    }

    fn pad_event(&mut self, event: &PadEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The game is paused under the menu; its controls wait.
        if self.menu.is_some() {
            return;
        }
        match event {
            PadEvent::Control {
                controller,
                control,
                pressed,
            } => {
                let hotkey = |control: &str| self.bindings.hotkey(control);
                let effects = self.chords.event(hotkey, controller, control, *pressed);
                for effect in effects {
                    self.pad_effect(controller, effect, window, cx);
                }
            }
            PadEvent::Pointer { dx, dy } => self.move_cursor((*dx, *dy)),
        }
        // Redraw the cursor, even when paused.
        cx.notify();
    }

    /// Carries out one effect of a control on `controller` going down or
    /// up (see `chords.rs`).
    fn pad_effect(
        &mut self,
        controller: &str,
        effect: Effect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match effect {
            Effect::Pass { control, pressed } => {
                let action = self.bindings.pad_action(controller, &control);
                let source = format!("pad:{controller}:{control}");
                self.controls.set(source, action, pressed);
                self.sync_held();
            }
            Effect::Hold { control, held } => {
                let source = format!("pad:{controller}:{control}");
                self.controls.set(source, Some(held), true);
                self.sync_held();
            }
            Effect::Run(shortcut) => {
                // Its handler updates this view, which is busy until this
                // event is done, so it runs just after.
                let action = shortcut_action(shortcut);
                let focus = self.focus.clone();
                window.defer(cx, move |window, cx| {
                    focus.dispatch_action(action.as_ref(), window, cx);
                });
            }
        }
    }

    /// Moves the gamepad cursor by `motion`, in the units of
    /// [`PadEvent::Pointer`] as seen on the window, whichever way the
    /// screens are turned.
    fn move_cursor(&mut self, motion: (f32, f32)) {
        let Some((_, size)) = self.touchscreen() else {
            return;
        };
        let (dx, dy) = self.layout.rotation.unturn_vector(motion);
        self.cursor
            .get_or_insert_with(|| Cursor::centre(size))
            .move_by((dx * cursor::SPEED, dy * cursor::SPEED), size);
        self.sync_held();
    }

    /// Turns latched fast-forward on or off, and saves that for next
    /// launch.
    fn toggle_fast_forward(&mut self, cx: &mut Context<Self>) {
        let on = self.controls.toggle_fast_forward();
        self.sync_held();
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
        let on = self.controls.toggle_slow_motion();
        self.sync_held();
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
        if self.window_place == Some(place) {
            return;
        }
        self.window_place = Some(place);
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
        self.state_slot = next_slot(self.state_slot, forward);
        let slot = self.state_slot;
        self.show_notice(format!("State slot {slot}"), cx);
        self.save_settings(cx);
        let Some(rom_paths) = self.session.as_ref().map(|s| s.player_rom_paths.clone()) else {
            return;
        };
        let preview = cx
            .background_executor()
            .spawn(async move { slots::preview(&rom_paths, slot) });
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
    /// anywhere else it presses A.
    fn mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // In a multi-player session, clicking a screen takes control of it.
        let point = (f32::from(event.position.x), f32::from(event.position.y));
        let clicked = self.session.as_ref().and_then(|session| {
            let placement = self.placements.iter().find(|p| p.rect.contains(point))?;
            Some(session.info.player_for_screen(placement.screen))
        });
        if let Some(player) = clicked {
            self.switch_player(player, cx);
        }
        // Back to the mouse: hide the gamepad cursor unless it is touching.
        if !self.sent.touch {
            self.cursor = None;
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
        if touched.is_some() {
            self.stylus = touched;
        } else {
            // Elsewhere, and in every other game, a click presses A for as
            // long as it is held.
            self.controls
                .set(MOUSE.to_owned(), Some(Held::Button(Button::A)), true);
        }
        self.sync_held();
        cx.notify();
    }

    /// Drags the stylus with the mouse while it touches. Past the
    /// touchscreen's edges it stays on the nearest edge, as a stylus slid
    /// off a DS would.
    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        let Some((index, at)) = self.stylus else {
            return;
        };
        let Some((screen, size)) = self.touchscreen() else {
            return;
        };
        let point = (f32::from(event.position.x), f32::from(event.position.y));
        // Where the screen is now, should the layout have changed.
        let moved = self
            .placements
            .get(index)
            .filter(|placement| placement.screen == screen)
            .and_then(|placement| placement.pixel_nearest(size, point));
        if moved.is_none() {
            // The screen pressed on is gone from the window.
            self.stylus = None;
            self.sync_held();
        } else if let Some(moved) = moved.filter(|moved| *moved != at) {
            self.stylus = Some((index, moved));
            self.sync_held();
        }
    }

    /// Lifts the stylus or releases A when the mouse button comes up, in
    /// the window or out of it.
    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.stylus = None;
        self.controls.set(MOUSE.to_owned(), None, false);
        self.sync_held();
    }

    /// Asks for a file, then calls `then` with it.
    fn prompt_for_file(
        &mut self,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| then(this, path, window, cx))
                .ok();
        })
        .detach();
    }

    /// Asks for games, then adds them to the library without playing them.
    fn add_to_library(&mut self, _: &AddToLibrary, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add".into()),
        });
        let library = self.library.clone();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            library
                .update(cx, |library, cx| library.add(paths, cx))
                .ok();
        })
        .detach();
    }

    /// Pauses the game and opens the game menu over it.
    fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        if self.menu.is_some() {
            return;
        }
        let rom = session.player_rom_paths[0].clone();
        let title = self
            .library
            .read(cx)
            .title_of(&rom)
            .unwrap_or_else(|| file_name(&rom));
        self.release_all();
        self.send_debug(debugger::Command::Pause);
        let cheats = self.menu_cheats();
        let menu = cx.new(|_| GameMenu::new(title, cheats));
        let subscription = cx.subscribe_in(&menu, window, Self::menu_event);
        self.menu = Some((menu, subscription));
        // The menu says progress is saved; make it so.
        self.save_checkpoint();
        cx.notify();
    }

    /// Closes the game menu, playing on if `resume`.
    fn close_menu(&mut self, resume: bool, cx: &mut Context<Self>) {
        if self.menu.take().is_none() {
            return;
        }
        if resume {
            self.send_debug(debugger::Command::Continue);
        }
        cx.notify();
    }

    fn menu_event(
        &mut self,
        _: &Entity<GameMenu>,
        event: &MenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match *event {
            MenuEvent::Resume => self.close_menu(true, cx),
            MenuEvent::ToggleCheat(index) => self.toggle_cheat(index, cx),
            MenuEvent::EditCheats => self.edit_cheats(&EditCheats, window, cx),
            MenuEvent::StartOver => self.start_over(window, cx),
            MenuEvent::ExitToLibrary => self.close_game(window, cx),
        }
    }

    /// Saves where the game is as its checkpoint, to pick up from when it
    /// next starts. The emulation thread finishes this before it stops.
    fn save_checkpoint(&self) {
        self.send(Command::SaveState(CHECKPOINT));
    }

    /// Erases the running game's progress, its checkpoint and battery
    /// saves, and starts it again. Each battery save is kept as
    /// `game.sav.bak` in case it was a mistake.
    fn start_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let roms = session.rom_paths.clone();
        let checkpoint = persist::state_path(&session.player_rom_paths, CHECKPOINT);
        self.close_menu(false, cx);
        // Stopping writes the battery saves, so they are set aside after.
        self.stop(window);
        let erased = remove_if_present(&checkpoint).and_then(|()| {
            roms.iter().try_for_each(|rom| {
                let save = persist::backup_path(rom);
                match fs::rename(&save, save.with_extension("sav.bak")) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                    _ => Ok(()),
                }
            })
        });
        if let Err(error) = erased {
            self.show_notice(format!("Cannot erase progress: {error}"), cx);
        }
        self.open(roms, window, cx);
    }

    /// The active player's cheat file, while a game runs.
    fn cheat_file(&self) -> Option<PathBuf> {
        let session = self.session.as_ref()?;
        let player = self.active_player.min(session.player_rom_paths.len() - 1);
        Some(persist::cheats_path(session.player_rom_paths.get(player)?))
    }

    /// The active player's cheats, as the game menu lists them.
    fn menu_cheats(&self) -> Vec<CheatRow> {
        let (Some(session), Some(path)) = (&self.session, self.cheat_file()) else {
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

    /// Stops the game, keeping its progress as its checkpoint, and goes
    /// back to the library.
    fn close_game(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        self.close_menu(false, cx);
        self.save_checkpoint();
        self.stop(window);
        window.set_window_title("ds-emulator");
        cx.notify();
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
            LibraryEvent::Notice(text) => self.show_notice(text.clone(), cx),
        }
    }

    fn open_rom(&mut self, _: &OpenRom, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_for_file("Open", window, cx, |this, path, window, cx| {
            this.open(vec![path], window, cx);
        });
    }

    /// Restarts the running game linked to a second one: by link cable
    /// for the Game Boy and Game Boy Advance, over local wireless for the
    /// DS.
    fn connect_link_cable(
        &mut self,
        _: &ConnectLinkCable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let first = self.session.as_ref().and_then(|session| {
            let single = session.info.players == 1;
            single.then(|| session.rom_paths[0].clone())
        });
        let Some(first) = first else {
            self.show_notice("Open a game first, then connect a second console", cx);
            return;
        };
        self.prompt_for_file("Connect", window, cx, move |this, second, window, cx| {
            this.open(vec![first, second], window, cx);
        });
    }

    /// Restarts the running DS game with a GBA game in its GBA slot,
    /// replacing any already there.
    fn insert_gba_cartridge(
        &mut self,
        _: &InsertGbaCartridge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let game = self
            .session
            .as_ref()
            .filter(|session| has_gba_slot(session.info.system))
            .map(|session| session.rom_paths[0].clone());
        let Some(game) = game else {
            self.show_notice("Open a DS game first, then insert a GBA cartridge", cx);
            return;
        };
        self.prompt_for_file("Insert", window, cx, move |this, cartridge, window, cx| {
            this.open(vec![game, cartridge], window, cx);
        });
    }

    fn drop_files(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = paths.paths().first() {
            self.open(vec![path.clone()], window, cx);
        }
    }

    fn arrange(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        // Chosen over a linked game's side by side, so shown at once.
        self.linked_side_by_side = false;
        self.set_layout(
            Layout {
                arrangement,
                ..self.layout
            },
            cx,
        );
    }

    fn swap_screens(&mut self, cx: &mut Context<Self>) {
        self.set_layout(
            Layout {
                swapped: !self.layout.swapped,
                ..self.layout
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
                ..self.layout
            },
            cx,
        );
    }

    fn change_gap(&mut self, cx: &mut Context<Self>) {
        let gap = layout::next_gap(self.layout.gap);
        let message = if gap <= 0.0 {
            "No gap between screens".to_owned()
        } else if gap == layout::HINGE_GAP {
            format!("Screen gap {gap} px, as across the DS hinge")
        } else {
            format!("Screen gap {gap} px")
        };
        self.show_notice(message, cx);
        self.set_layout(Layout { gap, ..self.layout }, cx);
    }

    /// Switches to `layout` and saves it for next launch.
    fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        self.layout = layout;
        cx.notify();
        self.save_settings(cx);
    }

    /// Draws DS 3D at `scale` times its own resolution and saves that for
    /// next launch.
    fn set_resolution_3d(&mut self, scale: u32, cx: &mut Context<Self>) {
        self.resolution_3d = scale;
        self.send(Command::SetRenderScale(scale));
        let message = match (scale, self.session.as_ref().map(|s| s.info.system)) {
            (1, _) => "3D at the native resolution".to_owned(),
            (_, Some(System::Nds) | None) => format!("3D at {scale}× resolution"),
            (_, Some(_)) => format!("3D at {scale}× resolution, for DS games"),
        };
        self.show_notice(message, cx);
        self.save_settings(cx);
    }

    /// Starts or stops listening to the computer's microphone for DS games.
    fn toggle_microphone(&mut self, cx: &mut Context<Self>) {
        if self.microphone.take().is_some() {
            self.heard = 0;
            self.send_input();
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
        if heard != self.heard {
            self.heard = heard;
            self.send_input();
        }
    }

    /// The settings in force.
    pub fn settings(&self) -> Settings {
        Settings {
            layout: self.layout,
            speed: self.speeds,
            fast_forward_on: self.controls.fast_forward_latched(),
            slow_motion_on: self.controls.slow_motion_latched(),
            resolution_3d: self.resolution_3d,
            filter: self.filter,
            backdrop: self.backdrop,
            show_fps: self.show_fps,
            color_correction: self.color_correction,
            state_slot: self.state_slot,
            dmg_colors: self.dmg_colors,
            window_controls: self.window_controls,
            key_guide: self.key_guide,
            window_place: self.window_place,
        }
    }

    /// Switches to `settings`, as the menu items for each would, and saves
    /// them for next launch. The speeds, which only the settings file sets,
    /// stay as they are.
    pub fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        if settings.layout != self.settings().layout {
            self.set_layout(settings.layout, cx);
        }
        if settings.resolution_3d != self.resolution_3d {
            self.set_resolution_3d(settings.resolution_3d, cx);
        }
        if settings.filter != self.filter {
            self.set_filter(settings.filter, cx);
        }
        if settings.backdrop != self.backdrop {
            self.set_backdrop(settings.backdrop, cx);
        }
        if settings.window_controls != self.window_controls {
            self.set_window_controls(settings.window_controls, cx);
        }
        if settings.key_guide != self.key_guide {
            self.set_key_guide(settings.key_guide, cx);
        }
    }

    /// Fills the window around the screens with `backdrop` from now on,
    /// and saves that for next launch.
    fn set_backdrop(&mut self, backdrop: Backdrop, cx: &mut Context<Self>) {
        self.backdrop = backdrop;
        if let Some(session) = &mut self.session {
            self.stale_images
                .extend(session.ambient.take().map(|glow| glow.image));
        }
        cx.notify();
        self.save_settings(cx);
    }

    /// Shows or hides the window's close, minimise and zoom buttons and the
    /// bar they sit in, and saves that for next launch.
    fn set_window_controls(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.window_controls = shown;
        cx.notify();
        self.save_settings(cx);
    }

    /// Shows or hides the key guide, and saves that for next launch.
    fn set_key_guide(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.key_guide = shown;
        self.refresh_menus(cx);
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
        self.speeds.fast_forward = speed;
        if self.sent.pace == Pace::FastForward {
            self.send(Command::SetSpeed(speed));
        }
        self.refresh_menus(cx);
        self.show_notice(format!("Fast-forward speed: {}", speed.label()), cx);
        self.save_settings(cx);
    }

    /// Shows or hides the speed readout, and saves that for next launch.
    fn toggle_fps(&mut self, cx: &mut Context<Self>) {
        self.show_fps = !self.show_fps;
        if self.show_fps {
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
        let Some(session) = &self.session else {
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
        cx.set_menus(crate::app::menus(MenuChecks {
            fast_forward: self.speeds.fast_forward,
            show_fps: self.show_fps,
            key_guide: self.key_guide,
        }));
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
        let session = self.session.as_ref()?;
        Some(DebugTarget {
            session: self.sessions,
            system: session.info.system,
            player: self.active_player,
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
        let (Some(cursor), Some((screen, size))) = (self.cursor, self.touchscreen()) else {
            return Vec::new();
        };
        // Centre the mark on the pixel it touches.
        let pixel = cursor.pixel();
        let centre = (f32::from(pixel.x) + 0.5, f32::from(pixel.y) + 0.5);
        let touching = self.sent.touch;
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
    /// in how frames become images.
    fn rebuild_images(&mut self) {
        if let Some(session) = &mut self.session {
            let images = session.images.iter_mut().filter_map(Option::take);
            self.stale_images.extend(images.map(|(_, image)| image));
            self.stale_images
                .extend(session.ambient.take().map(|glow| glow.image));
        }
    }

    /// Shows games made for the original Game Boy with `colors` from the
    /// next one opened, and saves that for next launch. A game already
    /// running keeps its console until it is opened again.
    fn set_dmg_colors(&mut self, colors: emu_core::DmgColors, cx: &mut Context<Self>) {
        self.dmg_colors = colors;
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

    /// Filters the screens with `filter` from now on, and saves that for
    /// next launch.
    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.rebuild_images();
        self.show_notice(format!("Filter: {}", filter.label()), cx);
        self.save_settings(cx);
    }

    /// Turns colour correction on or off, and saves that for next launch.
    fn toggle_color_correction(&mut self, cx: &mut Context<Self>) {
        self.color_correction = !self.color_correction;
        self.rebuild_images();
        self.show_notice(
            if self.color_correction {
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
    /// Takes the emulation thread's latest frames, if they are new,
    /// retiring every screen image drawn from the last ones. The ambient
    /// backdrop fades over to them instead.
    fn take_frames(&mut self, window: &mut Window) {
        let Some(sequence) = self.emulator.take_latest(self.sequence, &mut self.frames) else {
            return;
        };
        self.sequence = sequence;
        for (_, image) in self.images.iter_mut().filter_map(Option::take) {
            window.drop_image(image).ok();
        }
    }

    /// Brings the images up to date with how each placement draws its
    /// screen. Each frame is colour corrected, turned and filtered, then
    /// enlarged by the whole factor left.
    fn refresh_images(
        &mut self,
        placements: &[Placement],
        scale_factor: f32,
        color_correction: bool,
        filter: Filter,
        window: &mut Window,
    ) {
        // One image per placement, so the layout decides how many.
        if self.images.len() > placements.len() {
            for (_, image) in self.images.drain(placements.len()..).flatten() {
                window.drop_image(image).ok();
            }
        }
        self.images.resize(placements.len(), None);
        let correction = self.correction.as_ref().filter(|_| color_correction);
        for (slot, placement) in self.images.iter_mut().zip(placements) {
            let Some(frame) = self.frames.get(placement.screen) else {
                continue;
            };
            let shown = placement.rotation.turn_size(frame.size());
            let key = ImageKey {
                screen: placement.screen,
                factor: video::prescale_factor(shown.width, placement.rect.width, scale_factor),
                rotation: placement.rotation,
            };
            if slot.as_ref().is_some_and(|(built, _)| *built == key) {
                continue;
            }
            if let Some((_, stale)) = slot.take() {
                window.drop_image(stale).ok();
            }
            // Frames drawn at a higher 3D resolution have several pixels
            // to each console pixel.
            let native = self.info.screens.get(placement.screen);
            let cell = native.map_or(1, |native| frame.size().width / native.width.max(1));
            let corrected = correction.map(|correction| correction.frame(frame));
            let frame = corrected.as_ref().unwrap_or(frame);
            let turned =
                (key.rotation != Rotation::Upright).then(|| video::turn(frame, key.rotation));
            let frame = turned.as_ref().unwrap_or(frame);
            let (filtered, factor) =
                filter.apply(frame, key.factor, usize::from(cell), key.rotation);
            *slot = Some((key, video::render_image(&filtered, factor)));
        }
    }

    /// Brings the ambient backdrop up to date with the screens as
    /// `placements` show them, fading toward their colours. Whether it is
    /// still fading.
    fn refresh_ambient(
        &mut self,
        placements: &[Placement],
        color_correction: bool,
        window: &mut Window,
    ) -> bool {
        let now = Instant::now();
        let current = self
            .ambient
            .as_ref()
            .is_some_and(|glow| glow.sampled.1 == self.sequence && glow.sampled.0 == placements);
        if !current {
            let correction = self.correction.as_ref().filter(|_| color_correction);
            let Some(target) = ambient::picture(&self.frames, placements, correction) else {
                if let Some(glow) = self.ambient.take() {
                    window.drop_image(glow.image).ok();
                }
                return false;
            };
            let sampled = (placements.to_vec(), self.sequence);
            let Some(glow) = &mut self.ambient else {
                let fade = ambient::Fade::new(target, now);
                let image = video::render_image(&fade.shown().frame(ambient::BORDER), 1);
                self.ambient = Some(Glow {
                    sampled,
                    fade,
                    image,
                });
                return false;
            };
            glow.sampled = sampled;
            glow.fade.aim(target);
        }
        let Some(glow) = &mut self.ambient else {
            return false;
        };
        let changed = !current || !glow.fade.settled();
        let moving = glow.fade.step(now);
        if changed {
            let image = video::render_image(&glow.fade.shown().frame(ambient::BORDER), 1);
            window
                .drop_image(std::mem::replace(&mut glow.image, image))
                .ok();
        }
        moving
    }
}

impl Render for EmulatorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Full screen shows its own bar on hover, buttons and all.
        let fullscreen = window.is_fullscreen();
        let show_controls = self.window_controls || fullscreen;
        if self.shown_controls != Some(show_controls) {
            window_chrome::set_controls_visible(window, show_controls);
            self.shown_controls = Some(show_controls);
        }
        // The key guide floats in a strip at the foot, and the screens fit
        // above it; the backdrop fills the whole window, under it too.
        let guide = self
            .session
            .as_ref()
            .filter(|_| self.key_guide && !fullscreen)
            .map(|session| key_guide::groups(session.info.system, &self.bindings));
        let viewport = window.viewport_size();
        let footer = guide.as_ref().map_or(0.0, |guide| {
            key_guide::height(guide, f32::from(viewport.width))
        });
        let window_size = (f32::from(viewport.width), f32::from(viewport.height));
        let viewport = (
            f32::from(viewport.width),
            (f32::from(viewport.height) - footer).max(0.0),
        );
        let scale_factor = window.scale_factor();

        let mut root = div()
            .track_focus(&self.focus)
            .key_context("Emulator")
            .size_full()
            .relative()
            .text_color(rgb(0x00DD_DDDD))
            .on_action(cx.listener(Self::open_rom))
            .on_action(cx.listener(Self::add_to_library))
            .on_action(cx.listener(|this, _: &CloseGame, window, cx| this.close_game(window, cx)))
            .on_action(cx.listener(Self::connect_link_cable))
            .on_action(cx.listener(Self::edit_cheats))
            .on_action(cx.listener(Self::run_script))
            .on_action(cx.listener(|this, _: &StopScript, _, _| this.send(Command::StopScript)))
            .on_action(cx.listener(|this, _: &SwitchPlayer, _, cx| {
                let players = this.session.as_ref().map_or(1, |s| s.info.players);
                this.switch_player((this.active_player + 1) % players.max(1), cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePause, _, _| this.send(Command::TogglePause)))
            .on_action(cx.listener(|this, _: &Reset, _, _| this.send(Command::Reset)))
            .on_action(cx.listener(|this, _: &SaveState, _, _| {
                this.send(Command::SaveState(this.state_slot));
            }))
            .on_action(cx.listener(|this, _: &LoadState, _, _| {
                this.send(Command::LoadState(this.state_slot));
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
                this.rotate(this.layout.rotation.clockwise(), cx);
            }))
            .on_action(cx.listener(|this, _: &RotateAnticlockwise, _, cx| {
                this.rotate(this.layout.rotation.anticlockwise(), cx);
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
            .on_action(cx.listener(|this, _: &ToggleWindowControls, _, cx| {
                this.set_window_controls(!this.window_controls, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleKeyGuide, _, cx| {
                this.set_key_guide(!this.key_guide, cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d1x, _, cx| {
                this.set_resolution_3d(1, cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d2x, _, cx| {
                this.set_resolution_3d(2, cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d3x, _, cx| {
                this.set_resolution_3d(3, cx);
            }))
            .on_action(cx.listener(|this, _: &Resolution3d4x, _, cx| {
                this.set_resolution_3d(4, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleMicrophone, _, cx| {
                this.toggle_microphone(cx);
            }))
            .on_action(cx.listener(|this, _: &SaveScreenshot, _, _| {
                this.send(Command::SaveScreenshot);
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
            .on_action(cx.listener(|this, _: &FilterNone, _, cx| {
                this.set_filter(Filter::None, cx);
            }))
            .on_action(cx.listener(|this, _: &FilterLcdGrid, _, cx| {
                this.set_filter(Filter::LcdGrid, cx);
            }))
            .on_action(cx.listener(|this, _: &FilterScanlines, _, cx| {
                this.set_filter(Filter::Scanlines, cx);
            }))
            .on_action(cx.listener(|this, _: &FilterScale2x, _, cx| {
                this.set_filter(Filter::Scale2x, cx);
            }))
            .on_action(cx.listener(|this, _: &FilterScale3x, _, cx| {
                this.set_filter(Filter::Scale3x, cx);
            }))
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| {
                window.toggle_fullscreen();
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            // macOS reports a left button let go with Control held as the
            // right one.
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_drop(cx.listener(Self::drop_files));
        // Listening only while a DS runs leaves the menu item disabled
        // otherwise.
        let system = self.session.as_ref().map(|session| session.info.system);
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
        // glows.
        root = root.bg(rgb(if fullscreen { 0x0000_0000 } else { BACKGROUND }));
        if let Some(session) = self.session.as_mut() {
            let consoles = consoles(&session.info);
            let shown = if self.linked_side_by_side {
                Layout {
                    arrangement: Arrangement::Horizontal,
                    ..self.layout
                }
            } else {
                self.layout
            };
            self.placements = shown.place_consoles(session.info.screens, consoles, viewport);
            session.take_frames(window);
            session.refresh_images(
                &self.placements,
                scale_factor,
                self.color_correction,
                self.filter,
                window,
            );
            if self.backdrop == Backdrop::Ambient {
                // Paused, no new frames come to draw the rest of the fade.
                if session.refresh_ambient(&self.placements, self.color_correction, window) {
                    window.request_animation_frame();
                }
                if let Some(glow) = &session.ambient {
                    root = root.child(ambient_layer(glow, window_size));
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
        root = root.children(self.cursor_marks());
        if self.window_controls && !fullscreen {
            root = root.child(top_bar_shade());
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.recording)
        {
            root = root.child(recording_mark());
        }
        if self.show_fps
            && let Some(readout) = &self.speed_readout
        {
            root = root.child(speed_readout(readout.clone(), footer));
        }

        if let Some(guide) = &guide {
            root = root.child(key_guide::guide(guide, footer));
        }
        if let Some((menu, _)) = &self.menu {
            root = root.child(menu.clone());
        }

        // A preview only lives as long as its notice.
        if self.notice.is_none() {
            self.stale_images.extend(self.slot_image.take());
        }
        for image in self.stale_images.drain(..) {
            window.drop_image(image).ok();
        }
        if let Some(notice) = &self.notice {
            let card = div().absolute().left(px(12.0));
            // Over the game menu, notices go to the top, clear of it.
            let card = if self.menu.is_some() {
                card.top(px(TOP_BAR + 4.0))
            } else {
                card.bottom(px(12.0 + footer))
            };
            let mut card = card
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgba(0x0000_00CC))
                .text_sm();
            if let Some(image) = &self.slot_image {
                card = card.child(
                    img(Arc::clone(image))
                        .w(px(160.0))
                        .object_fit(ObjectFit::Contain),
                );
            }
            root = root.child(card.child(notice.clone()));
        }
        root
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
                    .font_family("Menlo")
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

/// A red dot and "REC" in the window's corner while a recording is under
/// way. The recording is of the core's frames, so it never shows this.
fn recording_mark() -> impl IntoElement {
    div()
        .absolute()
        .top(px(10.0))
        .right(px(10.0))
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .rounded_md()
        .bg(rgba(0x0000_00AA))
        .text_xs()
        .child(div().size(px(8.0)).rounded_full().bg(rgb(0x00E5_3935)))
        .child("REC")
}

/// What to tell the player of the emulation thread's notice `text`:
/// nothing of the checkpoint being saved, which happens unasked, and
/// loading it in words of its own.
fn checkpoint_notice(text: String) -> Option<String> {
    if text == format!("Saved state {CHECKPOINT}") {
        None
    } else if text == format!("Loaded state {CHECKPOINT}") {
        Some("Picked up where you left off".to_owned())
    } else {
        Some(text)
    }
}

/// Removes the file at `path`, if there is one.
fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
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

/// The speed readout, small in the window's bottom right corner, above a
/// key guide `footer` logical pixels tall.
fn speed_readout(text: SharedString, footer: f32) -> impl IntoElement {
    div()
        .absolute()
        .bottom(px(8.0 + footer))
        .right(px(8.0))
        .px_1()
        .rounded_sm()
        .bg(rgba(0x0000_0099))
        .text_xs()
        .font_family("Menlo")
        .child(text)
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

/// Whether `system` takes GBA games in a second slot.
fn has_gba_slot(system: System) -> bool {
    emu_cores::second_slot(system) == Some(System::Gba)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
