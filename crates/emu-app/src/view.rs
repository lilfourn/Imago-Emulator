//! The main window: draws the running game's screens and turns keyboard,
//! gamepad, mouse and file events into emulator commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use emu_core::{CoreInfo, Frame, Input, ScreenSize, System, TouchPoint, rgb as pack_rgb};
use gpui::{
    Context, ExternalPaths, FocusHandle, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, PathPromptOptions, Pixels, Point, RenderImage,
    SharedString, Subscription, Task, Window, div, img, prelude::*, px, rgb, rgba,
};

use crate::app::{
    ConnectLinkCable, LayoutHorizontal, LayoutSingle, LayoutVertical, LoadState, NextStateSlot,
    OpenRom, PreviousStateSlot, Reset, SaveState, SwapScreens, SwitchPlayer, ToggleColorCorrection,
    ToggleFastForward, TogglePause,
};
use crate::audio::{self, AudioOutput};
use crate::bindings::{self, Bindings};
use crate::color::Correction;
use crate::controls::{Controls, HeldState};
use crate::emulator::{self, Command, Emulator, Event, Loaded};
use crate::gamepad::{self, PadEvent};
use crate::layout::{self, Arrangement, Layout, Placement};
use crate::slots::{self, SlotPreview};
use crate::video;

const NOTICE_DURATION: Duration = Duration::from_secs(3);

/// Save-state slots, numbered from 1.
const STATE_SLOTS: u8 = 4;

/// The root view of the main window.
pub struct EmulatorView {
    focus: FocusHandle,
    bindings: Bindings,
    layout: Layout,
    /// Show GBC and GBA colours as their screens did.
    color_correction: bool,
    session: Option<Session>,
    loading: Option<Task<()>>,
    placements: Vec<Placement>,
    controls: Controls,
    touch: Option<TouchPoint>,
    /// What the emulation thread was last told is held.
    sent: HeldState,
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
    /// One ROM per player.
    rom_paths: Vec<PathBuf>,
    frames: Vec<Frame>,
    sequence: u64,
    /// The system's colour correction, if it has one.
    correction: Option<Correction>,
    /// Per screen: the pre-scale factor and the image built with it.
    images: Vec<Option<(usize, Arc<RenderImage>)>>,
    _events: Task<()>,
    /// Dropped after `emulator`, so the thread stops before the device.
    _audio: Option<AudioOutput>,
}

impl EmulatorView {
    /// Creates the view, loading `roms` straight away if there are any.
    pub fn new(roms: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);

        let loaded_bindings = bindings::default_path().map(|path| Bindings::load(&path));
        let subscriptions = vec![
            // Keys released while another window has focus never send key-up.
            cx.observe_window_activation(window, |this, window, _| {
                if !window.is_window_active() {
                    this.release_all();
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
                let delivered = this.update_in(cx, |this, window, _| {
                    // Gamepads are global; only the active window listens.
                    if window.is_window_active() || !event.pressed {
                        this.pad_event(&event);
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
            color_correction: true,
            session: None,
            loading: None,
            placements: Vec::new(),
            controls: Controls::default(),
            touch: None,
            sent: HeldState::default(),
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
        if !roms.is_empty() {
            view.open(roms, window, cx);
        }
        view
    }

    /// Loads one ROM per player off the UI thread, then starts them; two
    /// ROMs are joined by a link cable.
    fn open(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
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
            frames: Vec::new(),
            sequence: 0,
            correction: Correction::for_system(info.system),
            images: Vec::new(),
            _events: task,
            _audio: audio_output,
        });
        // The new emulator starts with nothing held; resend what is.
        self.sent = HeldState::default();
        self.touch = None;
        self.active_player = 0;
        self.sync_held();
        if info.players > 1 {
            self.layout.arrangement = Arrangement::Horizontal;
            window.set_window_title(&format!("{name} — {} link", info.system.name()));
        } else {
            window.set_window_title(&format!("{name} — {}", info.system.name()));
        }
        self.show_notice(format!("Running {name}"), cx);
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
                touch: self.touch,
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
        self.show_notice(format!("Controlling player {}", player + 1), cx);
    }

    /// Tells the emulation thread about any change in held inputs.
    fn sync_held(&mut self) {
        let state = self.controls.state();
        if state.buttons != self.sent.buttons {
            self.sent.buttons = state.buttons;
            self.send_input();
        }
        if state.fast_forward != self.sent.fast_forward {
            self.send(Command::SetFastForward(state.fast_forward));
        }
        if state.rewind != self.sent.rewind {
            self.send(Command::SetRewind(state.rewind));
        }
        self.sent = state;
    }

    fn release_all(&mut self) {
        self.controls.release_all();
        self.touch = None;
        self.send_input();
        self.sync_held();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, _: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        // Command and Control chords are menu shortcuts, not game input.
        if !modifiers.platform && !modifiers.control {
            let key = &event.keystroke.key;
            let action = self.bindings.key_action(key);
            self.controls.set(format!("key:{key}"), action, true);
            self.sync_held();
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let key = &event.keystroke.key;
        self.controls.set(format!("key:{key}"), None, false);
        self.sync_held();
    }

    fn pad_event(&mut self, event: &PadEvent) {
        let action = self.bindings.pad_action(&event.controller, &event.control);
        let source = format!("pad:{}:{}", event.controller, event.control);
        self.controls.set(source, action, event.pressed);
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

    fn change_state_slot(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.state_slot = if forward {
            self.state_slot % STATE_SLOTS + 1
        } else {
            (self.state_slot + STATE_SLOTS - 2) % STATE_SLOTS + 1
        };
        let slot = self.state_slot;
        self.show_notice(format!("State slot {slot}"), cx);
        let Some(rom_paths) = self.session.as_ref().map(|s| s.rom_paths.clone()) else {
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

    /// Points the stylus at `position`, or lifts it if that is off the
    /// touchscreen.
    fn touch_at(&mut self, position: Point<Pixels>) {
        let touch = self.session.as_ref().and_then(|session| {
            let screen = session.info.touch_screen?;
            let size = *session.info.screens.get(screen)?;
            let placement = self.placements.iter().find(|p| p.screen == screen)?;
            let point = (f32::from(position.x), f32::from(position.y));
            layout::pixel_at(placement.rect, size, point)
        });
        if touch != self.touch {
            self.touch = touch;
            self.send_input();
        }
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
        self.touch_at(event.position);
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if event.pressed_button == Some(MouseButton::Left) {
            self.touch_at(event.position);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if self.touch.take().is_some() {
            self.send_input();
        }
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
            let single_game_boy = session.info.players == 1 && session.info.system == System::Gb;
            single_game_boy.then(|| session.rom_paths[0].clone())
        });
        let Some(first) = first else {
            self.show_notice("Open a Game Boy game first, then connect the cable", cx);
            return;
        };
        self.prompt_for_rom("Connect", window, cx, move |this, second, window, cx| {
            this.open(vec![first, second], window, cx);
        });
    }

    fn drop_files(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = paths.paths().first() {
            self.open(vec![path.clone()], window, cx);
        }
    }

    fn arrange(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        self.layout.arrangement = arrangement;
        cx.notify();
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
    /// Brings the images up to date with the latest frame and the size each
    /// screen is drawn at.
    fn refresh_images(
        &mut self,
        placements: &[Placement],
        scale_factor: f32,
        color_correction: bool,
        window: &mut Window,
    ) {
        if let Some(sequence) = self.emulator.take_latest(self.sequence, &mut self.frames) {
            self.sequence = sequence;
            self.images.resize(self.frames.len(), None);
            for (_, image) in self.images.iter_mut().filter_map(Option::take) {
                window.drop_image(image).ok();
            }
        }
        for placement in placements {
            let Some(frame) = self.frames.get(placement.screen) else {
                continue;
            };
            let factor =
                video::prescale_factor(frame.size().width, placement.rect.width, scale_factor);
            let slot = &mut self.images[placement.screen];
            if slot.as_ref().is_some_and(|(built, _)| *built == factor) {
                continue;
            }
            if let Some((_, stale)) = slot.take() {
                window.drop_image(stale).ok();
            }
            let image = match self.correction.as_ref().filter(|_| color_correction) {
                Some(correction) => video::render_image(&correction.frame(frame), factor),
                None => video::render_image(frame, factor),
            };
            *slot = Some((factor, image));
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
            .bg(rgb(0x0011_1111))
            .text_color(rgb(0x00DD_DDDD))
            .on_action(cx.listener(Self::open_rom))
            .on_action(cx.listener(Self::connect_link_cable))
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
            .on_action(cx.listener(|this, _: &LayoutVertical, _, cx| {
                this.arrange(Arrangement::Vertical, cx);
            }))
            .on_action(cx.listener(|this, _: &LayoutHorizontal, _, cx| {
                this.arrange(Arrangement::Horizontal, cx);
            }))
            .on_action(cx.listener(|this, _: &LayoutSingle, _, cx| {
                this.arrange(Arrangement::Single, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapScreens, _, cx| {
                this.layout.swapped = !this.layout.swapped;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleColorCorrection, _, cx| {
                this.toggle_color_correction(cx);
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_drop(cx.listener(Self::drop_files));

        if let Some(session) = self.session.as_mut() {
            self.placements = self.layout.place(session.info.screens, viewport);
            session.refresh_images(
                &self.placements,
                scale_factor,
                self.color_correction,
                window,
            );
            for placement in &self.placements {
                let Some(Some((_, image))) = session.images.get(placement.screen) else {
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

        // A preview only lives as long as its notice.
        if self.notice.is_none() {
            self.stale_images.extend(self.slot_image.take());
        }
        for image in self.stale_images.drain(..) {
            window.drop_image(image).ok();
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

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
