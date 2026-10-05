//! The main window: draws the running game's screens and turns keyboard,
//! mouse and file events into emulator commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use emu_core::{Buttons, CoreInfo, Frame, Input, TouchPoint};
use gpui::{
    Context, ExternalPaths, FocusHandle, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, PathPromptOptions, Pixels, Point, RenderImage,
    SharedString, Subscription, Task, Window, div, img, prelude::*, px, rgb, rgba,
};

use crate::app::{
    LayoutHorizontal, LayoutSingle, LayoutVertical, LoadState, OpenRom, Reset, SaveState,
    SwapScreens, TogglePause,
};
use crate::audio::{self, AudioOutput};
use crate::bindings::{self, Bindings, Held};
use crate::emulator::{self, Command, Emulator, Event, Loaded};
use crate::layout::{self, Arrangement, Layout, Placement};
use crate::video;

const NOTICE_DURATION: Duration = Duration::from_secs(3);

/// The root view of the main window.
pub struct EmulatorView {
    focus: FocusHandle,
    bindings: Bindings,
    layout: Layout,
    session: Option<Session>,
    loading: Option<Task<()>>,
    placements: Vec<Placement>,
    buttons: Buttons,
    touch: Option<TouchPoint>,
    notice: Option<SharedString>,
    notice_timer: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// A running game and the images last drawn for it.
struct Session {
    emulator: Emulator,
    info: CoreInfo,
    frames: Vec<Frame>,
    sequence: u64,
    /// Per screen: the pre-scale factor and the image built with it.
    images: Vec<Option<(usize, Arc<RenderImage>)>>,
    _events: Task<()>,
    /// Dropped after `emulator`, so the thread stops before the device.
    _audio: Option<AudioOutput>,
}

impl EmulatorView {
    /// Creates the view, loading `rom` straight away if given.
    pub fn new(rom: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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

        let mut view = Self {
            focus,
            bindings: Bindings::default(),
            layout: Layout::default(),
            session: None,
            loading: None,
            placements: Vec::new(),
            buttons: Buttons::NONE,
            touch: None,
            notice: None,
            notice_timer: None,
            _subscriptions: subscriptions,
        };
        match loaded_bindings {
            Some(Ok(bindings)) => view.bindings = bindings,
            Some(Err(error)) => view.show_notice(format!("{error}; using default keys"), cx),
            None => {}
        }
        if let Some(rom) = rom {
            view.open(rom, window, cx);
        }
        view
    }

    /// Loads `path` off the UI thread, then starts it.
    fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = file_name(&path);
        self.show_notice(format!("Loading {name}…"), cx);
        let load = cx
            .background_executor()
            .spawn(async move { emulator::load(&path) });
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
            frames: Vec::new(),
            sequence: 0,
            images: Vec::new(),
            _events: task,
            _audio: audio_output,
        });
        self.buttons = Buttons::NONE;
        self.touch = None;
        window.set_window_title(&format!("{name} — {}", info.system.name()));
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
        self.send(Command::SetInput(Input {
            buttons: self.buttons,
            touch: self.touch,
        }));
    }

    fn release_all(&mut self) {
        self.buttons = Buttons::NONE;
        self.touch = None;
        self.send_input();
        self.send(Command::SetFastForward(false));
    }

    fn set_held(&mut self, key: &str, held: bool) {
        match self.bindings.action(key) {
            Some(Held::Button(button)) => {
                self.buttons.set(button, held);
                self.send_input();
            }
            Some(Held::FastForward) => self.send(Command::SetFastForward(held)),
            None => {}
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, _: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        // Command and Control chords are menu shortcuts, not game input.
        if !modifiers.platform && !modifiers.control {
            self.set_held(&event.keystroke.key, true);
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.set_held(&event.keystroke.key, false);
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

    fn mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, _: &mut Context<Self>) {
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

    fn open_rom(&mut self, _: &OpenRom, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.open(path, window, cx))
                .ok();
        })
        .detach();
    }

    fn drop_files(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = paths.paths().first() {
            self.open(path.clone(), window, cx);
        }
    }

    fn arrange(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        self.layout.arrangement = arrangement;
        cx.notify();
    }
}

impl Session {
    /// Brings the images up to date with the latest frame and the size each
    /// screen is drawn at.
    fn refresh_images(&mut self, placements: &[Placement], scale_factor: f32, window: &mut Window) {
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
            *slot = Some((factor, video::render_image(frame, factor)));
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
            .on_action(cx.listener(|this, _: &TogglePause, _, _| this.send(Command::TogglePause)))
            .on_action(cx.listener(|this, _: &Reset, _, _| this.send(Command::Reset)))
            .on_action(cx.listener(|this, _: &SaveState, _, _| this.send(Command::SaveState)))
            .on_action(cx.listener(|this, _: &LoadState, _, _| this.send(Command::LoadState)))
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
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_drop(cx.listener(Self::drop_files));

        if let Some(session) = self.session.as_mut() {
            self.placements = self.layout.place(session.info.screens, viewport);
            session.refresh_images(&self.placements, scale_factor, window);
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

        if let Some(notice) = &self.notice {
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(12.0))
                    .left(px(12.0))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgba(0x0000_00CC))
                    .text_sm()
                    .child(notice.clone()),
            );
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
