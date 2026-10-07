//! The main window: draws the running game's screens and turns keyboard,
//! gamepad, mouse and file events into emulator commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use emu_core::{CoreInfo, Frame, Input, ScreenSize, System, TouchPoint, rgb as pack_rgb};
use gpui::{
    Context, ExternalPaths, FocusHandle, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, PathPromptOptions, Pixels, Point, RenderImage,
    ScrollWheelEvent, SharedString, Subscription, Task, Window, div, img, prelude::*, px, rgb,
    rgba,
};

use crate::app::{
    ConnectLinkCable, EditCheats, InsertGbaCartridge, LayoutHorizontal, LayoutHybrid, LayoutSingle,
    LayoutVertical, LoadState, NextScreenGap, NextStateSlot, OpenRom, PreviousStateSlot, Reset,
    RotateAnticlockwise, RotateClockwise, SaveState, SwapScreens, SwitchPlayer,
    ToggleColorCorrection, ToggleFastForward, ToggleFullScreen, ToggleMemoryViewer, TogglePause,
    ToggleSlowMotion,
};
use crate::audio::{self, AudioOutput};
use crate::bindings::{self, Bindings};
use crate::cheats;
use crate::color::Correction;
use crate::controls::{Controls, HeldState};
use crate::cursor::{self, Cursor};
use crate::emulator::{self, Command, Emulator, Event, Loaded, MemorySpan};
use crate::gamepad::{self, PadEvent};
use crate::layout::{self, Arrangement, Layout, Placement, Rotation};
use crate::memory_viewer::{self, BYTES_PER_ROW, Viewer};
use crate::persist;
use crate::settings::{self, Settings};
use crate::slots::{self, SlotPreview};
use crate::speed::Speeds;
use crate::video;

const NOTICE_DURATION: Duration = Duration::from_secs(3);

/// Save-state slots, numbered from 1.
const STATE_SLOTS: u8 = 4;

/// The root view of the main window.
pub struct EmulatorView {
    focus: FocusHandle,
    bindings: Bindings,
    layout: Layout,
    /// Where the layout is saved for next launch, if there is a config
    /// folder.
    settings_path: Option<PathBuf>,
    /// How fast fast-forward and slow motion run, from the settings file.
    speeds: Speeds,
    /// Show GBC and GBA colours as their screens did.
    color_correction: bool,
    /// Where the memory viewer is looking, while it is open.
    memory_viewer: Option<Viewer>,
    session: Option<Session>,
    loading: Option<Task<()>>,
    placements: Vec<Placement>,
    controls: Controls,
    /// Where the mouse touches the touchscreen, while its button is down.
    mouse_touch: Option<TouchPoint>,
    /// The gamepad's touchscreen cursor, once a gamepad has used it.
    cursor: Option<Cursor>,
    /// What the emulation thread was last told is held.
    sent: HeldState,
    /// Where the emulation thread was last told the screen is touched.
    sent_touch: Option<TouchPoint>,
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
    /// The system's colour correction, if it has one.
    correction: Option<Correction>,
    /// Per placement: the image drawn there and what it shows.
    images: Vec<Option<(ImageKey, Arc<RenderImage>)>>,
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

impl EmulatorView {
    /// Creates the view, loading `roms` straight away if there are any.
    pub fn new(roms: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);

        let loaded_bindings = bindings::default_path().map(|path| Bindings::load(&path));
        let settings_path = settings::default_path();
        let loaded_settings = settings_path.as_deref().map(Settings::load);
        let subscriptions = vec![
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    // Keys released while another window has focus never
                    // send key-up.
                    this.release_all();
                } else if let Some(message) = this.reload_cheats() {
                    // Back from editing a cheat file, perhaps.
                    this.show_notice(message, cx);
                }
            }),
            // Stop the emulation thread on quit so the battery save is flushed.
            cx.on_app_quit(|this, _| {
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
                        this.pad_event(&event, cx);
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
            settings_path,
            speeds: Speeds::default(),
            color_correction: true,
            memory_viewer: None,
            session: None,
            loading: None,
            placements: Vec::new(),
            controls: Controls::default(),
            mouse_touch: None,
            cursor: None,
            sent: HeldState::default(),
            sent_touch: None,
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
        match loaded_bindings {
            Some(Ok(bindings)) => view.bindings = bindings,
            Some(Err(error)) => view.show_notice(format!("{error}; using default bindings"), cx),
            None => {}
        }
        match loaded_settings {
            Some(Ok(settings)) => {
                view.layout = settings.layout;
                view.speeds = settings.speed;
            }
            Some(Err(error)) => view.show_notice(format!("{error}; using default settings"), cx),
            None => {}
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
            self.stop(window);
        }
        let name = paths
            .iter()
            .map(|path| file_name(path))
            .collect::<Vec<_>>()
            .join(" + ");
        self.show_notice(format!("Loading {name}…"), cx);
        let load = cx
            .background_executor()
            .spawn(async move { emulator::load(&paths) });
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
        let player_rom_paths = loaded.player_rom_paths().to_vec();
        let emulator = Emulator::start(loaded, events, audio_sink);
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
            correction: Correction::for_system(info.system),
            images: Vec::new(),
            _events: task,
            _audio: audio_output,
        });
        // The new emulator starts with nothing held; resend what is.
        self.sent = HeldState::default();
        self.sent_touch = None;
        self.mouse_touch = None;
        self.cursor = None;
        self.active_player = 0;
        self.sync_held();
        self.watch_memory();
        if info.players > 1 {
            self.layout.arrangement = Arrangement::Horizontal;
            window.set_window_title(&format!("{name} — {} link", info.system.name()));
        } else {
            window.set_window_title(&format!("{name} — {}", info.system.name()));
        }
        let running = format!("Running {name}");
        match self.reload_cheats() {
            Some(cheats) => self.show_notice(format!("{running} · {cheats}"), cx),
            None => self.show_notice(running, cx),
        }
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
        if let Some(session) = self.session.take() {
            for (_, image) in session.images.into_iter().flatten() {
                window.drop_image(image).ok();
            }
        }
    }

    fn handle(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Frame => cx.notify(),
            Event::Notice(text) => self.show_notice(text, cx),
        }
    }

    fn show_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
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
                microphone: if self.sent.blow { Input::LOUDEST } else { 0 },
            },
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
        self.send(Command::SetInput {
            player: self.active_player,
            input: Input::default(),
        });
        self.active_player = player;
        self.send_input();
        self.watch_memory();
        self.show_notice(format!("Controlling player {}", player + 1), cx);
    }

    /// Tells the emulation thread about any change in held inputs or in
    /// where the screen is touched. The mouse and the gamepad cursor touch
    /// independently; the mouse wins while both do.
    fn sync_held(&mut self) {
        let state = self.controls.state();
        let touch = match self.mouse_touch {
            Some(point) => Some(point),
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

    /// The running game's touchscreen: its index and size.
    fn touchscreen(&self) -> Option<(usize, ScreenSize)> {
        let info = &self.session.as_ref()?.info;
        let screen = info.touch_screen?;
        Some((screen, *info.screens.get(screen)?))
    }

    fn release_all(&mut self) {
        self.controls.release_all();
        self.mouse_touch = None;
        self.sync_held();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
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

    fn pad_event(&mut self, event: &PadEvent, cx: &mut Context<Self>) {
        match event {
            PadEvent::Control {
                controller,
                control,
                pressed,
            } => {
                let action = self.bindings.pad_action(controller, control);
                let source = format!("pad:{controller}:{control}");
                self.controls.set(source, action, *pressed);
                self.sync_held();
            }
            PadEvent::Pointer { dx, dy } => self.move_cursor((*dx, *dy)),
        }
        // Redraw the cursor, even when paused.
        cx.notify();
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
    }

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
    }

    fn change_state_slot(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.state_slot = if forward {
            self.state_slot % STATE_SLOTS + 1
        } else {
            (self.state_slot + STATE_SLOTS - 2) % STATE_SLOTS + 1
        };
        let slot = self.state_slot;
        self.show_notice(format!("State slot {slot}"), cx);
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
            let mut frame = Frame::new(ScreenSize::new(width, height));
            for (pixel, color) in frame.pixels_mut().iter_mut().zip(rgb.chunks_exact(3)) {
                *pixel = pack_rgb(color[0], color[1], color[2]);
            }
            let image = video::render_image(&frame, 2);
            self.stale_images.extend(self.slot_image.replace(image));
        }
    }

    /// Points the mouse's stylus at `position`, or lifts it if that is off
    /// every copy of the touchscreen.
    fn touch_at(&mut self, position: Point<Pixels>) {
        let point = (f32::from(position.x), f32::from(position.y));
        self.mouse_touch = self.touchscreen().and_then(|(screen, size)| {
            self.placements
                .iter()
                .filter(|placement| placement.screen == screen)
                .find_map(|placement| placement.pixel_at(size, point))
        });
        self.sync_held();
    }

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
        self.touch_at(event.position);
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if event.pressed_button == Some(MouseButton::Left) {
            self.touch_at(event.position);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.mouse_touch = None;
        self.sync_held();
    }

    /// Asks for a ROM file, then calls `then` with it.
    fn prompt_for_rom(
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

    fn open_rom(&mut self, _: &OpenRom, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_for_rom("Open", window, cx, |this, path, window, cx| {
            this.open(vec![path], window, cx);
        });
    }

    /// Restarts the running Game Boy game linked to a second one.
    fn connect_link_cable(
        &mut self,
        _: &ConnectLinkCable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let first = self.session.as_ref().and_then(|session| {
            let has_cable = matches!(session.info.system, System::Gb | System::Gba);
            let single = session.info.players == 1 && has_cable;
            single.then(|| session.rom_paths[0].clone())
        });
        let Some(first) = first else {
            self.show_notice(
                "Open a Game Boy or Game Boy Advance game first, then connect the cable",
                cx,
            );
            return;
        };
        self.prompt_for_rom("Connect", window, cx, move |this, second, window, cx| {
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
        self.prompt_for_rom("Insert", window, cx, move |this, cartridge, window, cx| {
            this.open(vec![game, cartridge], window, cx);
        });
    }

    fn drop_files(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = paths.paths().first() {
            self.open(vec![path.clone()], window, cx);
        }
    }

    fn arrange(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
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

    /// Switches to `layout` and saves it for next launch, noting any
    /// failure to save.
    fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        self.layout = layout;
        cx.notify();
        let Some(path) = &self.settings_path else {
            return;
        };
        let settings = Settings {
            layout,
            speed: self.speeds,
        };
        if let Err(error) = settings.save(path) {
            let message = format!("Cannot save settings to {}: {error}", path.display());
            self.show_notice(message, cx);
        }
    }

    fn toggle_memory_viewer(&mut self, cx: &mut Context<Self>) {
        self.memory_viewer = match self.memory_viewer {
            Some(_) => None,
            None => Some(Viewer::default()),
        };
        self.watch_memory();
        cx.notify();
    }

    /// Tells the emulation thread which memory the viewer shows, if any.
    fn watch_memory(&self) {
        let span = self
            .memory_viewer
            .zip(self.session.as_ref())
            .and_then(|(viewer, session)| {
                let region = memory_viewer::regions(session.info.system).get(viewer.region)?;
                let (start, length) = viewer.span(region);
                Some(MemorySpan {
                    player: self.active_player,
                    start,
                    length,
                })
            });
        self.send(Command::WatchMemory(span));
    }

    fn select_memory_region(&mut self, region: usize, cx: &mut Context<Self>) {
        if let Some(viewer) = &mut self.memory_viewer {
            viewer.select(region);
            self.watch_memory();
            cx.notify();
        }
    }

    fn scroll_memory(&mut self, rows: f32, cx: &mut Context<Self>) {
        let length = self.session.as_ref().and_then(|session| {
            let regions = memory_viewer::regions(session.info.system);
            Some(regions.get(self.memory_viewer?.region)?.length)
        });
        if let (Some(viewer), Some(length)) = (&mut self.memory_viewer, length) {
            let before = viewer.offset;
            viewer.scroll(rows, length);
            if viewer.offset != before {
                self.watch_memory();
                cx.notify();
            }
        }
    }

    /// The memory viewer overlay: region tabs over a hex dump.
    fn memory_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        const ROW_HEIGHT: f32 = 13.0;
        let viewer = self.memory_viewer?;
        let session = self.session.as_ref()?;
        let regions = memory_viewer::regions(session.info.system);
        let tabs = regions.iter().enumerate().map(|(index, region)| {
            let tab = div()
                .id(index)
                .px_1()
                .rounded_sm()
                .cursor_pointer()
                .child(region.name)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_memory_region(index, cx);
                }));
            if index == viewer.region {
                tab.bg(rgba(0xFFFF_FF33))
            } else {
                tab
            }
        });
        let rows: Vec<String> = match session.emulator.memory() {
            Some((span, bytes)) => (0u32..)
                .zip(bytes.chunks(BYTES_PER_ROW as usize))
                .map(|(row, chunk)| memory_viewer::hex_row(span.start + row * BYTES_PER_ROW, chunk))
                .collect(),
            None if regions.is_empty() => vec!["No memory to show for this system yet".to_owned()],
            None => Vec::new(),
        };
        Some(
            div()
                .id("memory-viewer")
                .absolute()
                .top(px(8.0))
                .left(px(8.0))
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(rgba(0x0000_00DD))
                .font_family("Menlo")
                // Small enough that a row fits the default window.
                .text_size(px(10.0))
                .line_height(px(ROW_HEIGHT))
                .whitespace_nowrap()
                .child(div().flex().gap_2().children(tabs))
                .child(div().children(rows))
                // Clicks here are for the viewer, not the screen below.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                    let delta = event.delta.pixel_delta(px(ROW_HEIGHT)).y;
                    this.scroll_memory(-f32::from(delta) / ROW_HEIGHT, cx);
                })),
        )
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
        let touching = self.sent.touch && self.mouse_touch.is_none();
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

    fn toggle_color_correction(&mut self, cx: &mut Context<Self>) {
        self.color_correction = !self.color_correction;
        if let Some(session) = &mut self.session {
            // Rebuild every screen with the new colours.
            let images = session.images.iter_mut().filter_map(Option::take);
            self.stale_images.extend(images.map(|(_, image)| image));
        }
        self.show_notice(
            if self.color_correction {
                "Colour correction on"
            } else {
                "Colour correction off"
            },
            cx,
        );
    }
}

impl Session {
    /// Brings the images up to date with the latest frame and with how
    /// each placement draws its screen.
    fn refresh_images(
        &mut self,
        placements: &[Placement],
        scale_factor: f32,
        color_correction: bool,
        window: &mut Window,
    ) {
        if let Some(sequence) = self.emulator.take_latest(self.sequence, &mut self.frames) {
            self.sequence = sequence;
            for (_, image) in self.images.iter_mut().filter_map(Option::take) {
                window.drop_image(image).ok();
            }
        }
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
            let corrected = correction.map(|correction| correction.frame(frame));
            let frame = corrected.as_ref().unwrap_or(frame);
            let turned =
                (key.rotation != Rotation::Upright).then(|| video::turn(frame, key.rotation));
            let frame = turned.as_ref().unwrap_or(frame);
            *slot = Some((key, video::render_image(frame, key.factor)));
        }
    }
}

impl Render for EmulatorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let viewport = (f32::from(viewport.width), f32::from(viewport.height));
        let scale_factor = window.scale_factor();

        let mut root = div()
            .track_focus(&self.focus)
            .key_context("Emulator")
            .size_full()
            .relative()
            // Nothing but the screens on black in full screen.
            .bg(rgb(if window.is_fullscreen() {
                0x0000_0000
            } else {
                0x0011_1111
            }))
            .text_color(rgb(0x00DD_DDDD))
            .on_action(cx.listener(Self::open_rom))
            .on_action(cx.listener(Self::connect_link_cable))
            .on_action(cx.listener(Self::edit_cheats))
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
            .on_action(cx.listener(|this, _: &ToggleMemoryViewer, _, cx| {
                this.toggle_memory_viewer(cx);
            }))
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| {
                window.toggle_fullscreen();
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_drop(cx.listener(Self::drop_files));
        // Listening only while a DS runs leaves the menu item disabled
        // otherwise.
        let system = self.session.as_ref().map(|session| session.info.system);
        if system.is_some_and(has_gba_slot) {
            root = root.on_action(cx.listener(Self::insert_gba_cartridge));
        }

        if let Some(session) = self.session.as_mut() {
            self.placements = self.layout.place(session.info.screens, viewport);
            session.refresh_images(
                &self.placements,
                scale_factor,
                self.color_correction,
                window,
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
        } else {
            self.placements.clear();
            root = root.child(welcome());
        }
        root = root.children(self.cursor_marks());

        // A preview only lives as long as its notice.
        if self.notice.is_none() {
            self.stale_images.extend(self.slot_image.take());
        }
        for image in self.stale_images.drain(..) {
            window.drop_image(image).ok();
        }
        if let Some(panel) = self.memory_panel(cx) {
            root = root.child(panel);
        }
        if let Some(notice) = &self.notice {
            let mut card = div()
                .absolute()
                .bottom(px(12.0))
                .left(px(12.0))
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

fn welcome() -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().text_xl().child("ds-emulator"))
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x0088_8888))
                .child("Open a ROM with ⌘O, or drop a .gb, .gbc, .gba or .nds file here."),
        )
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
