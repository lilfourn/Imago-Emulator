//! The settings window, from ds-emulator > Settings…: the settings kept
//! between launches, each as a row of choices. Choosing one changes it in
//! the main window's [`EmulatorView`] as its menu item would, which saves
//! it.
//!
//! Keyboard first: the arrow keys (or WASD) move up and down the rows and
//! change the choice in the row the keyboard is on; Return or Space
//! presses the OpenRouter row's first button; Escape closes the window.
//!
//! Under the settings, the Guide section connects an OpenRouter account
//! ([`OpenRouter`]): its buttons sit in a track as the choices do, with a
//! line under them saying how the connection stands.

use emu_guide::chat::Effort;
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, KeyDownEvent, ScrollHandle,
    SharedString, Subscription, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, div,
    prelude::*, px, rgb, rgba, size,
};

use crate::ambient::Backdrop;
use crate::app::{TOP_BAR, top_bar};
use crate::layout::{Arrangement, GAPS, HINGE_GAP, Rotation};
use crate::openrouter::{self, OpenRouter, Status, Usage};
use crate::picture::{Picture, RESOLUTIONS_3D};
use crate::probe;
use crate::settings::Settings;
use crate::theme::{
    ACCENT, BACKGROUND, DANGER, LINE, MUTED as DIM, ON_ACCENT, PANEL, TEXT, with_alpha,
};
use crate::view::EmulatorView;

/// Size of the window's words, in logical pixels.
const TEXT_SIZE: f32 = 14.0;
/// Room between a row's name and its choices, either side of a choice's
/// label, and around the choices inside their 1-pixel edge.
const NAME_GAP: f32 = 12.0;
const CHOICE_PADDING: f32 = 12.0;
const TRACK_PADDING: f32 = 2.0;
/// Rounding of the choices' track, and of the chosen one inside it, so
/// the two corners share a centre.
const TRACK_RADIUS: f32 = 8.0;
const CHOICE_RADIUS: f32 = TRACK_RADIUS - TRACK_PADDING - 1.0;
/// The least room either side of the rows.
const MIN_SIDE: f32 = 24.0;
/// The OpenRouter row's name, which also names its parts for the layout
/// checks.
const ACCOUNT: &str = "OpenRouter";

/// The settings window's root view.
pub struct SettingsWindow {
    main: WeakEntity<EmulatorView>,
    /// The main window's settings, as last shown.
    settings: Settings,
    /// The row the keyboard is on.
    focused: usize,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// The app's OpenRouter connection, for Guide.
    openrouter: Entity<OpenRouter>,
    _subscriptions: Vec<Subscription>,
}

/// A row of choices, as shown for the settings it was made from.
struct Row {
    /// The heading over the row, if it starts a group.
    heading: Option<&'static str>,
    name: &'static str,
    /// What each choice is called, in order.
    labels: Vec<SharedString>,
    /// Which choice the settings hold, if one does.
    chosen: Option<usize>,
    choose: Choose,
}

/// Stores a row's choice, by its index, in the settings.
type Choose = Box<dyn Fn(&mut Settings, usize)>;

/// A button in the OpenRouter row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Account {
    Connect,
    TryAgain,
    Cancel,
    ManageKeys,
    Disconnect,
}

impl Account {
    fn label(self) -> &'static str {
        match self {
            Self::Connect => "Connect OpenRouter",
            Self::TryAgain => "Try Again",
            Self::Cancel => "Cancel",
            Self::ManageKeys => "Manage Keys",
            Self::Disconnect => "Disconnect",
        }
    }

    /// Whether it is what to press, which the accent marks.
    fn primary(self) -> bool {
        matches!(self, Self::Connect | Self::TryAgain)
    }
}

/// The OpenRouter row's buttons while the connection is `status`.
fn account_buttons(status: &Status) -> Vec<Account> {
    match status {
        Status::NotConnected => vec![Account::Connect],
        Status::Connecting { .. } => vec![Account::Cancel],
        Status::Connected {
            from_environment: true,
            ..
        } => vec![Account::ManageKeys],
        Status::Connected {
            usage: Usage::Refused,
            ..
        } => vec![Account::Connect, Account::Disconnect],
        Status::Connected { .. } => vec![Account::ManageKeys, Account::Disconnect],
        Status::Failed(_) => vec![Account::TryAgain],
    }
}

/// The line under the OpenRouter row while the connection is `status`,
/// and whether it tells of a problem.
fn account_caption(status: &Status) -> (String, bool) {
    match status {
        Status::NotConnected => (
            "Guide asks OpenRouter's models about the game you're playing.".to_owned(),
            false,
        ),
        Status::Connecting { returned: false } => {
            ("Finish connecting in your browser.".to_owned(), false)
        }
        Status::Connecting { returned: true } => ("Finishing connecting\u{2026}".to_owned(), false),
        Status::Connected {
            usage: Usage::Refused,
            ..
        } => (
            "This key no longer works \u{2014} connect again.".to_owned(),
            true,
        ),
        Status::Connected {
            tail,
            from_environment,
            usage,
        } => {
            let mut words = if *from_environment {
                format!("Using OPENROUTER_API_KEY \u{2026}{tail}")
            } else {
                format!("Connected \u{b7} key \u{2026}{tail}")
            };
            if let Usage::Known(spent) = usage {
                words.push_str(" \u{b7} ");
                words.push_str(spent);
            }
            (words, false)
        }
        Status::Failed(reason) => (reason.clone(), true),
    }
}

impl Row {
    /// A row named `name` offering `choices`, each a label and the value
    /// `set` stores, with `current` chosen.
    fn new<T: Copy + PartialEq + 'static>(
        name: &'static str,
        choices: Vec<(SharedString, T)>,
        current: T,
        set: fn(&mut Settings, T),
    ) -> Self {
        let chosen = choices.iter().position(|&(_, value)| value == current);
        let (labels, values): (Vec<_>, Vec<_>) = choices.into_iter().unzip();
        Self {
            heading: None,
            name,
            labels,
            chosen,
            choose: Box::new(move |settings, at| {
                if let Some(&value) = values.get(at) {
                    set(settings, value);
                }
            }),
        }
    }

    /// The row with `heading` over it.
    fn under(mut self, heading: &'static str) -> Self {
        self.heading = Some(heading);
        self
    }
}

/// The rows, top to bottom, for `settings`.
fn rows(settings: &Settings) -> Vec<Row> {
    let layout = settings.layout;
    vec![
        Row::new(
            "Arrangement",
            vec![
                ("Stacked".into(), Arrangement::Vertical),
                ("Side by Side".into(), Arrangement::Horizontal),
                ("One Screen".into(), Arrangement::Single),
                ("Hybrid".into(), Arrangement::Hybrid),
            ],
            layout.arrangement,
            |settings, arrangement| settings.layout.arrangement = arrangement,
        )
        .under("Screens"),
        Row::new(
            "Order",
            vec![("Top First".into(), false), ("Swapped".into(), true)],
            layout.swapped,
            |settings, swapped| settings.layout.swapped = swapped,
        ),
        Row::new(
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
        ),
        Row::new(
            "Gap",
            GAPS.into_iter().map(|gap| (gap_label(gap), gap)).collect(),
            layout.gap,
            |settings, gap| settings.layout.gap = gap,
        ),
        Row::new(
            "Picture",
            Picture::ALL
                .into_iter()
                .map(|picture| (picture.label().into(), picture))
                .collect(),
            settings.picture,
            |settings, picture| settings.picture = picture,
        )
        .under("Video"),
        Row::new(
            "Backdrop",
            [Backdrop::Ambient, Backdrop::Solid]
                .into_iter()
                .map(|backdrop| (backdrop.label().into(), backdrop))
                .collect(),
            settings.backdrop,
            |settings, backdrop| settings.backdrop = backdrop,
        ),
        Row::new(
            "3D Resolution",
            RESOLUTIONS_3D
                .into_iter()
                .map(|resolution| (resolution.label().into(), resolution))
                .collect(),
            settings.resolution_3d,
            |settings, resolution| settings.resolution_3d = resolution,
        ),
        Row::new(
            "Window Controls",
            vec![("Shown".into(), true), ("Hidden".into(), false)],
            settings.window_controls,
            |settings, shown| settings.window_controls = shown,
        )
        .under("Window"),
        Row::new(
            "Thinking",
            vec![
                ("Off".into(), Effort::None),
                ("Low".into(), Effort::Low),
                ("Medium".into(), Effort::Medium),
                ("High".into(), Effort::High),
            ],
            settings.guide_thinking,
            |settings, effort| settings.guide_thinking = effort,
        )
        .under("Guide"),
        Row::new(
            "Look Things Up",
            vec![("On".into(), true), ("Off".into(), false)],
            settings.guide_lookups,
            |settings, on| settings.guide_lookups = on,
        ),
    ]
}

impl SettingsWindow {
    /// Opens a settings window for `main`.
    pub fn open(main: Entity<EmulatorView>, cx: &mut App) -> anyhow::Result<WindowHandle<Self>> {
        let bounds = Bounds::centered(None, size(px(720.0), px(640.0)), cx);
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

    /// The window for `main`, redrawn whenever `main`'s settings change.
    pub fn new(main: Entity<EmulatorView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let openrouter = OpenRouter::global(cx);
        // What the key has spent, freshly, each time settings open.
        openrouter.update(cx, |openrouter, cx| openrouter.check(cx));
        let subscriptions = vec![
            cx.observe(&openrouter, |_, _, cx| cx.notify()),
            // The main window redraws every frame; only its settings show
            // here.
            cx.observe(&main, |this, main, cx| {
                let settings = main.read(cx).shown_settings();
                if settings != this.settings {
                    this.settings = settings;
                    cx.notify();
                }
            }),
            // Settings go when the game's window does.
            cx.observe_release_in(&main, window, |_, _, window, _| window.remove_window()),
        ];
        Self {
            settings: main.read(cx).shown_settings(),
            main: main.downgrade(),
            focused: 0,
            focus,
            scroll: ScrollHandle::new(),
            openrouter,
            _subscriptions: subscriptions,
        }
    }

    /// Changes the main window's settings by `edit`.
    fn change(&self, edit: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let Some(main) = self.main.upgrade() else {
            return;
        };
        main.update(cx, |main, cx| {
            let mut settings = main.shown_settings();
            edit(&mut settings);
            main.apply_settings(settings, cx);
        });
    }

    /// Puts the keyboard on the `index`th row and picks its choice `at`.
    fn pick(&mut self, index: usize, at: usize, cx: &mut Context<Self>) {
        self.focused = index;
        if let Some(row) = rows(&self.settings).get(index) {
            self.change(|settings| (row.choose)(settings, at), cx);
        }
        cx.notify();
    }

    /// `row`, the `index`th, ringed if the keyboard is on it, with
    /// `name_width` for its name.
    fn row(&self, index: usize, row: Row, name_width: f32, cx: &mut Context<Self>) -> AnyElement {
        let focused = index == self.focused;
        let buttons = row.labels.into_iter().enumerate().map(|(at, label)| {
            let chosen = row.chosen == Some(at);
            div()
                .id((row.name, at))
                .px(px(CHOICE_PADDING))
                .py(px(3.0))
                .rounded(px(CHOICE_RADIUS))
                .cursor_pointer()
                // White, as the library's and the store's chosen filters are.
                .when(chosen, |button| {
                    button.bg(rgb(TEXT)).text_color(rgb(BACKGROUND))
                })
                .when(!chosen, |button| {
                    button.hover(|button| button.bg(rgb(LINE)))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.pick(index, at, cx)))
                .child(label)
                .children(probe::part(format!("segment:{}.{at}", row.name)))
        });
        let choices = div()
            .flex()
            .items_center()
            .gap(px(NAME_GAP))
            .child(
                div()
                    .w(px(name_width))
                    .flex_none()
                    .text_right()
                    .text_color(rgb(if focused { TEXT } else { DIM }))
                    .child(row.name)
                    .children(probe::part(format!("name:{}", row.name))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(TRACK_PADDING))
                    .p(px(TRACK_PADDING))
                    .rounded(px(TRACK_RADIUS))
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgba(if focused { with_alpha(ACCENT, 0xAA) } else { 0 }))
                    .children(buttons)
                    .children(probe::bordered_part(format!("track:{}", row.name), 1.0)),
            );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(row.heading.map(|heading| section(heading, name_width)))
            .child(choices)
            .into_any_element()
    }

    /// Does what the OpenRouter row's `button` says.
    fn press(&mut self, button: Account, cx: &mut Context<Self>) {
        match button {
            Account::Connect | Account::TryAgain => {
                self.openrouter
                    .update(cx, |openrouter, cx| openrouter.connect(cx));
            }
            Account::Cancel => self
                .openrouter
                .update(cx, |openrouter, cx| openrouter.cancel(cx)),
            Account::Disconnect => {
                self.openrouter
                    .update(cx, |openrouter, cx| openrouter.disconnect(cx));
            }
            Account::ManageKeys => cx.open_url(openrouter::KEYS_PAGE),
        }
    }

    /// The OpenRouter row, the `index`th, under the Guide heading: its
    /// buttons where choices go, and how the connection stands under
    /// them, at most `width` wide.
    fn account_row(
        &self,
        index: usize,
        name_width: f32,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = index == self.focused;
        let status = self.openrouter.read(cx).status().clone();
        let (caption, problem) = account_caption(&status);
        let buttons = account_buttons(&status)
            .into_iter()
            .enumerate()
            .map(|(at, button)| {
                div()
                    .id((ACCOUNT, at))
                    .px(px(CHOICE_PADDING))
                    .py(px(3.0))
                    .rounded(px(CHOICE_RADIUS))
                    .cursor_pointer()
                    .when(button.primary(), |this| {
                        this.bg(rgb(ACCENT)).text_color(rgb(ON_ACCENT))
                    })
                    .when(!button.primary(), |this| {
                        this.hover(|this| this.bg(rgb(LINE)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focused = index;
                        this.press(button, cx);
                    }))
                    .child(button.label())
                    .children(probe::part(format!("segment:{ACCOUNT}.{at}")))
            });
        let control = div()
            .flex()
            .items_center()
            .gap(px(NAME_GAP))
            .child(
                div()
                    .w(px(name_width))
                    .flex_none()
                    .text_right()
                    .text_color(rgb(if focused { TEXT } else { DIM }))
                    .child(ACCOUNT)
                    .children(probe::part(format!("name:{ACCOUNT}"))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(TRACK_PADDING))
                    .p(px(TRACK_PADDING))
                    .rounded(px(TRACK_RADIUS))
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgba(if focused { with_alpha(ACCENT, 0xAA) } else { 0 }))
                    .children(buttons)
                    .children(probe::bordered_part(format!("track:{ACCOUNT}"), 1.0)),
            );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(control)
            .child(
                div().pl(px(name_width + NAME_GAP)).child(
                    div()
                        .max_w(px(width))
                        .text_xs()
                        .text_color(rgb(if problem { DANGER } else { DIM }))
                        .child(caption)
                        .children(probe::part(format!("caption:{ACCOUNT}"))),
                ),
            )
            .into_any_element()
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" {
            window.remove_window();
            return;
        }
        // Command and Control chords are menu shortcuts.
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt {
            return;
        }
        let rows = rows(&self.settings);
        // The OpenRouter row comes after the settings' rows.
        let account = rows.len();
        if self.focused == account && matches!(keystroke.key.as_str(), "enter" | "space") {
            let status = self.openrouter.read(cx).status().clone();
            if let Some(&button) = account_buttons(&status).first() {
                self.press(button, cx);
            }
            return;
        }
        let focused = moved_row(self.focused, account + 1, &keystroke.key);
        if focused != self.focused {
            self.focused = focused;
            self.scroll.scroll_to_item(focused);
            cx.notify();
            return;
        }
        let picked = rows
            .get(focused)
            .and_then(|row| moved_choice(row.chosen, row.labels.len(), &keystroke.key));
        if let Some(at) = picked {
            self.scroll.scroll_to_item(focused);
            self.pick(focused, at, cx);
        }
    }
}

impl gpui::Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The rows are centred as one form: the widest name, then the
        // widest row of choices.
        let rows = rows(&self.settings);
        let widest = |width: &dyn Fn(&Row) -> f32| rows.iter().map(width).fold(0.0, f32::max);
        let name_width = widest(&|row| text_width(row.name, window))
            .max(text_width(ACCOUNT, window))
            .ceil();
        let choices_width =
            widest(&|row| track_width(row.labels.iter().map(SharedString::as_ref), window));
        let account = account_buttons(self.openrouter.read(cx).status());
        let account_width = track_width(account.iter().map(|button| button.label()), window);
        let choices_width = choices_width.max(account_width);
        let room = f32::from(window.viewport_size().width) - name_width - NAME_GAP - choices_width;
        let side = (room / 2.0).max(MIN_SIDE);
        let count = rows.len();
        let mut rows: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| self.row(index, row, name_width, cx))
            .collect();
        rows.push(self.account_row(count, name_width, choices_width, cx));
        div()
            .track_focus(&self.focus)
            .key_context("Settings")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .text_size(px(TEXT_SIZE))
            .on_key_down(cx.listener(Self::key_down))
            .children(probe::begin())
            .child(
                div()
                    .h(px(TOP_BAR))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(DIM))
                    .child("Settings")
                    .children(probe::part("header.title")),
            )
            .child(
                // Scrolls if the rows are taller than the window.
                div()
                    .id("settings")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px(px(side))
                    .pb(px(MIN_SIDE))
                    .children(rows),
            )
            .children(probe::end("settings"))
    }
}

/// How wide `text` is drawn in the window's words, in logical pixels.
fn text_width(text: &str, window: &Window) -> f32 {
    let run = window.text_style().to_run(text.len());
    let line = window
        .text_system()
        .shape_line(text.to_owned().into(), px(TEXT_SIZE), &[run], None);
    f32::from(line.width)
}

/// How wide a track of buttons labelled `labels` is drawn: each label and
/// its padding, which layout rounds up to a whole point, the gaps between
/// them, and the track's padding and 1-pixel edge.
fn track_width<'a>(labels: impl Iterator<Item = &'a str>, window: &Window) -> f32 {
    let (count, buttons) = labels.fold((0.0, 0.0), |(count, width), label| {
        let button = (text_width(label, window) + 2.0 * CHOICE_PADDING).ceil();
        (count + 1.0, width + button)
    });
    buttons + (count + 1.0) * TRACK_PADDING + 2.0
}

/// A heading over a group of rows, lined up with the choices after names
/// `name_width` wide.
fn section(title: &'static str, name_width: f32) -> AnyElement {
    div()
        .pt_2()
        .pl(px(name_width + NAME_GAP))
        .text_xs()
        .text_color(rgb(DIM))
        .child(title)
        .children(probe::part(format!("heading:{title}")))
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

/// The row the keyboard is on after pressing `key` on row `focused` of
/// `rows`: up and down (or W and S) move, stopping at the ends.
fn moved_row(focused: usize, rows: usize, key: &str) -> usize {
    match key {
        "up" | "w" => focused.saturating_sub(1),
        "down" | "s" => (focused + 1).min(rows.saturating_sub(1)),
        _ => focused,
    }
}

/// The choice pressing `key` picks in a row of `choices` with `chosen`
/// picked, if it picks one: left and right (or A and D) move, stopping at
/// the ends. With none picked, right picks the first and left the last.
fn moved_choice(chosen: Option<usize>, choices: usize, key: &str) -> Option<usize> {
    let last = choices.checked_sub(1)?;
    let at = match (key, chosen) {
        ("left" | "a", Some(at)) => at.saturating_sub(1),
        ("left" | "a", None) => last,
        ("right" | "d", Some(at)) => (at + 1).min(last),
        ("right" | "d", None) => 0,
        _ => return None,
    };
    Some(at).filter(|&at| chosen != Some(at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::Resolution3d;

    #[test]
    fn up_and_down_move_between_rows_and_stop_at_the_ends() {
        assert_eq!(moved_row(0, 9, "down"), 1);
        assert_eq!(moved_row(1, 9, "s"), 2);
        assert_eq!(moved_row(8, 9, "down"), 8);
        assert_eq!(moved_row(3, 9, "up"), 2);
        assert_eq!(moved_row(0, 9, "w"), 0);
        assert_eq!(moved_row(4, 9, "left"), 4);
        assert_eq!(moved_row(0, 0, "down"), 0);
    }

    #[test]
    fn left_and_right_pick_the_next_choice_and_stop_at_the_ends() {
        assert_eq!(moved_choice(Some(1), 4, "right"), Some(2));
        assert_eq!(moved_choice(Some(1), 4, "a"), Some(0));
        assert_eq!(moved_choice(Some(0), 4, "left"), None);
        assert_eq!(moved_choice(Some(3), 4, "d"), None);
        assert_eq!(moved_choice(Some(1), 4, "up"), None);
        assert_eq!(moved_choice(None, 4, "right"), Some(0));
        assert_eq!(moved_choice(None, 4, "left"), Some(3));
        assert_eq!(moved_choice(None, 0, "right"), None);
    }

    #[test]
    fn rows_mark_what_the_settings_hold_and_store_what_is_picked() {
        let mut settings = Settings::default();
        let rows = rows(&settings);
        let arrangement = &rows[0];
        assert_eq!(arrangement.heading, Some("Screens"));
        assert_eq!(arrangement.chosen, Some(0), "stacked by default");
        (arrangement.choose)(&mut settings, 1);
        assert_eq!(settings.layout.arrangement, Arrangement::Horizontal);
        // A 3D scale the row does not offer leaves none of it chosen.
        assert_eq!(row_named(&settings, "3D Resolution").chosen, Some(0));
        settings.resolution_3d = Resolution3d::Times(5);
        assert_eq!(row_named(&settings, "3D Resolution").chosen, None);
        settings.resolution_3d = Resolution3d::Times(2);
        assert_eq!(row_named(&settings, "3D Resolution").chosen, Some(2));
    }

    #[test]
    fn the_openrouter_row_offers_what_the_connection_allows() {
        let connected = |from_environment, usage| Status::Connected {
            tail: "1234".to_owned(),
            from_environment,
            usage,
        };
        assert_eq!(account_buttons(&Status::NotConnected), [Account::Connect]);
        assert_eq!(
            account_buttons(&Status::Connecting { returned: false }),
            [Account::Cancel]
        );
        assert_eq!(
            account_buttons(&connected(false, Usage::Unknown)),
            [Account::ManageKeys, Account::Disconnect]
        );
        // A key from the environment can't be forgotten from here.
        assert_eq!(
            account_buttons(&connected(true, Usage::Unknown)),
            [Account::ManageKeys]
        );
        assert_eq!(
            account_buttons(&connected(false, Usage::Refused)),
            [Account::Connect, Account::Disconnect]
        );
        assert_eq!(
            account_buttons(&Status::Failed("x".to_owned())),
            [Account::TryAgain]
        );
        assert!(Account::Connect.primary() && Account::TryAgain.primary());
        assert!(!Account::Disconnect.primary());
    }

    #[test]
    fn the_openrouter_row_says_how_the_connection_stands() {
        assert_eq!(
            account_caption(&Status::Connected {
                tail: "1234".to_owned(),
                from_environment: false,
                usage: Usage::Known("$0.42 used".to_owned()),
            }),
            ("Connected · key …1234 · $0.42 used".to_owned(), false)
        );
        assert_eq!(
            account_caption(&Status::Connected {
                tail: "abcd".to_owned(),
                from_environment: true,
                usage: Usage::Unknown,
            }),
            ("Using OPENROUTER_API_KEY …abcd".to_owned(), false)
        );
        assert!(
            account_caption(&Status::Connected {
                tail: "abcd".to_owned(),
                from_environment: false,
                usage: Usage::Refused,
            })
            .1
        );
        assert_eq!(
            account_caption(&Status::Failed(
                "Access was declined on OpenRouter.".to_owned()
            )),
            ("Access was declined on OpenRouter.".to_owned(), true)
        );
        assert_eq!(
            account_caption(&Status::Connecting { returned: false }).0,
            "Finish connecting in your browser."
        );
    }

    /// The row called `name` for `settings`.
    fn row_named(settings: &Settings, name: &str) -> Row {
        rows(settings)
            .into_iter()
            .find(|row| row.name == name)
            .expect("a row by that name")
    }
}
