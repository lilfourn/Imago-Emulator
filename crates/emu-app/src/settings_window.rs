//! The settings window, from ds-emulator > Settings…: the settings kept
//! between launches, each as a row of choices. Choosing one changes it in
//! the main window's [`EmulatorView`] as its menu item would, which saves
//! it.

use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, KeyDownEvent, SharedString,
    Subscription, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::*,
    px, rgb, size,
};

use crate::ambient::Backdrop;
use crate::app::{TOP_BAR, top_bar};
use crate::filter::Filter;
use crate::layout::{Arrangement, GAPS, HINGE_GAP, Rotation};
use crate::settings::Settings;
use crate::theme::{ACCENT, BACKGROUND, MUTED as DIM, PANEL, TEXT};
use crate::view::EmulatorView;

/// Room for the names of the rows, in logical pixels.
const NAME_WIDTH: f32 = 120.0;

/// The settings window's root view.
pub struct SettingsWindow {
    main: WeakEntity<EmulatorView>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindow {
    /// Opens a settings window for `main`.
    pub fn open(main: Entity<EmulatorView>, cx: &mut App) -> anyhow::Result<WindowHandle<Self>> {
        let bounds = Bounds::centered(None, size(px(600.0), px(450.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(top_bar("Settings")),
            is_resizable: false,
            is_minimizable: false,
            app_id: Some("ds-emulator".to_owned()),
            ..WindowOptions::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| Self::new(main, window, cx))
        })
    }

    /// The window for `main`, redrawn whenever `main` is.
    pub fn new(main: Entity<EmulatorView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let subscriptions = vec![
            cx.observe(&main, |_, _, cx| cx.notify()),
            // Settings go when the game's window does.
            cx.observe_release_in(&main, window, |_, _, window, _| window.remove_window()),
        ];
        Self {
            main: main.downgrade(),
            focus,
            _subscriptions: subscriptions,
        }
    }

    /// Changes the main window's settings by `edit`.
    fn change(&self, edit: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let Some(main) = self.main.upgrade() else {
            return;
        };
        main.update(cx, |main, cx| {
            let mut settings = main.settings();
            edit(&mut settings);
            main.apply_settings(settings, cx);
        });
    }

    /// A row named `name` offering `choices`, each a label and the value
    /// `set` stores, with `current` marked.
    fn row<T: Copy + PartialEq + 'static>(
        &self,
        name: &'static str,
        choices: Vec<(SharedString, T)>,
        current: T,
        set: fn(&mut Settings, T),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let buttons = choices
            .into_iter()
            .enumerate()
            .map(|(index, (label, value))| {
                let chosen = value == current;
                div()
                    .id((name, index))
                    .px_3()
                    .py(px(3.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .when(chosen, |button| {
                        button.bg(rgb(ACCENT)).text_color(rgb(0x00FF_FFFF))
                    })
                    .when(!chosen, |button| {
                        button.hover(|button| button.bg(rgb(0x0033_3333)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change(|settings| set(settings, value), cx);
                    }))
                    .child(label)
            });
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(NAME_WIDTH))
                    .flex()
                    .justify_end()
                    .text_color(rgb(DIM))
                    .child(name),
            )
            .child(
                div()
                    .flex()
                    .gap(px(2.0))
                    .p(px(2.0))
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .children(buttons),
            )
            .into_any_element()
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, _: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            window.remove_window();
        }
    }
}

impl gpui::Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .track_focus(&self.focus)
            .key_context("Settings")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .text_sm()
            .on_key_down(cx.listener(Self::key_down))
            .child(
                div()
                    .h(px(TOP_BAR))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(DIM))
                    .child("Settings"),
            );
        let Some(main) = self.main.upgrade() else {
            return root;
        };
        let settings = main.read(cx).settings();
        let layout = settings.layout;
        let rows = vec![
            section("Screens"),
            self.row(
                "Arrangement",
                vec![
                    ("Stacked".into(), Arrangement::Vertical),
                    ("Side by Side".into(), Arrangement::Horizontal),
                    ("One Screen".into(), Arrangement::Single),
                    ("Hybrid".into(), Arrangement::Hybrid),
                ],
                layout.arrangement,
                |settings, arrangement| settings.layout.arrangement = arrangement,
                cx,
            ),
            self.row(
                "Order",
                vec![("Top First".into(), false), ("Swapped".into(), true)],
                layout.swapped,
                |settings, swapped| settings.layout.swapped = swapped,
                cx,
            ),
            self.row(
                "Rotation",
                [
                    Rotation::Upright,
                    Rotation::Clockwise,
                    Rotation::UpsideDown,
                    Rotation::Anticlockwise,
                ]
                .into_iter()
                .map(|rotation| (format!("{}°", rotation.degrees()).into(), rotation))
                .collect(),
                layout.rotation,
                |settings, rotation| settings.layout.rotation = rotation,
                cx,
            ),
            self.row(
                "Gap",
                GAPS.into_iter().map(|gap| (gap_label(gap), gap)).collect(),
                layout.gap,
                |settings, gap| settings.layout.gap = gap,
                cx,
            ),
            section("Video"),
            self.row(
                "Filter",
                [
                    Filter::None,
                    Filter::LcdGrid,
                    Filter::Scanlines,
                    Filter::Scale2x,
                    Filter::Scale3x,
                ]
                .into_iter()
                .map(|filter| (filter.label().into(), filter))
                .collect(),
                settings.filter,
                |settings, filter| settings.filter = filter,
                cx,
            ),
            self.row(
                "Backdrop",
                [Backdrop::Ambient, Backdrop::Solid]
                    .into_iter()
                    .map(|backdrop| (backdrop.label().into(), backdrop))
                    .collect(),
                settings.backdrop,
                |settings, backdrop| settings.backdrop = backdrop,
                cx,
            ),
            self.row(
                "DS 3D",
                (1..=4)
                    .map(|scale| (format!("{scale}×").into(), scale))
                    .collect(),
                settings.resolution_3d,
                |settings, scale| settings.resolution_3d = scale,
                cx,
            ),
            section("Window"),
            self.row(
                "Window Controls",
                vec![("Shown".into(), true), ("Hidden".into(), false)],
                settings.window_controls,
                |settings, shown| settings.window_controls = shown,
                cx,
            ),
            self.row(
                "Key Guide",
                vec![("Shown".into(), true), ("Hidden".into(), false)],
                settings.key_guide,
                |settings, shown| settings.key_guide = shown,
                cx,
            ),
        ];
        root.child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_2()
                .px_6()
                .pb_6()
                .children(rows),
        )
    }
}

/// A heading over a group of rows, lined up with the choices.
fn section(title: &'static str) -> AnyElement {
    div()
        .pt_2()
        .pl(px(NAME_WIDTH + 12.0))
        .text_xs()
        .text_color(rgb(DIM))
        .child(title)
        .into_any_element()
}

/// What a gap between screens is called.
fn gap_label(gap: f32) -> SharedString {
    if gap == 0.0 {
        "None".into()
    } else if gap == HINGE_GAP {
        "Hinge".into()
    } else {
        format!("{gap} px").into()
    }
}
