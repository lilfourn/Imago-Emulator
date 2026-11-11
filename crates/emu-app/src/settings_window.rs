//! The settings window, from ds-emulator > Settings…: the settings kept
//! between launches, each as a row of choices. Choosing one changes it in
//! the main window's [`EmulatorView`] as its menu item would, which saves
//! it.
//!
//! Keyboard first: the arrow keys (or WASD) move up and down the rows and
//! change the choice in the row the keyboard is on; Return or Space
//! presses the first button of a row of buttons; Escape closes the window.
//!
//! Under the settings, the Guide section connects an OpenRouter account
//! ([`OpenRouter`]): its buttons sit in a track as the choices do, with a
//! line under them saying how the connection stands.
//!
//! Last, the Supporter section ([`crate::supporter`]): a row of buttons to
//! become a supporter or paste a supporter's key, which Paste's shortcut
//! does too from anywhere in the window, and, once a key is in, the looks
//! it is thanked with. Nothing else in the app asks for support.

use emu_guide::chat::Effort;
use emu_supporter::{KeyError, Tier};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, KeyDownEvent, ScrollHandle,
    SharedString, Subscription, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions, div,
    prelude::*, px, rgb, rgba, size,
};

use crate::ambient::Backdrop;
use crate::app::{TOP_BAR, top_bar};
use crate::chrome::{self, Tone};
use crate::layout::{Arrangement, GAPS, HINGE_GAP, Rotation};
use crate::openrouter::{self, OpenRouter, Status, Usage};
use crate::picture::{Picture, RESOLUTIONS_3D};
use crate::probe;
use crate::settings::Settings;
use crate::supporter::{self, AppIcon, EnterError};
use crate::theme::{
    Accent, BACKGROUND, DANGER, LINE, MUTED as DIM, ON_ACCENT, PANEL, TEXT, accent, with_alpha,
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
/// The supporter's row's name, which names its parts too.
const KEY: &str = "Key";
/// The window's size, in logical pixels: wide enough for the widest row,
/// and tall enough for the rows everyone has.
const SIZE: (f32, f32) = (720.0, 800.0);
/// How much taller each of a supporter's rows makes it: a row and the gap
/// before it.
const LOOK_ROW: f32 = 43.0;
/// Room left above and below the window on a display too short for it,
/// for the system's bars; the rows then scroll.
const DISPLAY_MARGIN: f32 = 120.0;

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
    /// Why the key last pasted was not taken, until one is.
    key_problem: Option<String>,
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

/// A button in the supporter's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Support {
    Become,
    PasteKey,
    RemoveKey,
}

impl Support {
    fn label(self) -> &'static str {
        match self {
            Self::Become => "Become a Supporter",
            Self::PasteKey => "Paste Key",
            Self::RemoveKey => "Remove Key",
        }
    }

    /// Whether it is what to press, which the accent marks.
    fn primary(self) -> bool {
        self == Self::Become
    }
}

/// The supporter's row's buttons, with a key in or without.
fn support_buttons(supporter: bool) -> Vec<Support> {
    if supporter {
        vec![Support::RemoveKey]
    } else {
        vec![Support::Become, Support::PasteKey]
    }
}

/// The line under the supporter's row, and whether it tells of a problem:
/// why the key last pasted was not taken, or thanks to the supporter by
/// the name in their key (which may be empty) and its tier, or what
/// supporting is.
fn support_caption(supporter: Option<(&str, Tier)>, problem: Option<&str>) -> (String, bool) {
    // The first tier is what a supporter is; the others are named.
    let named = |tier: Tier| match tier {
        Tier::Supporter => String::new(),
        Tier::Patron => " You\u{2019}re a Patron.".to_owned(),
        Tier::Founder => " You\u{2019}re a Founder.".to_owned(),
    };
    match (problem, supporter) {
        (Some(problem), _) => (problem.to_owned(), true),
        (None, Some(("", tier))) => (
            format!("Thank you for supporting Imago Emulator.{}", named(tier)),
            false,
        ),
        (None, Some((name, tier))) => (format!("Thank you, {name}.{}", named(tier)), false),
        (None, None) if supporter::CHOOSES_ICON => (
            "Imago Emulator is free. Supporters get accent colours and Dock icons as thanks."
                .to_owned(),
            false,
        ),
        (None, None) => (
            "Imago Emulator is free. Supporters get accent colours as thanks.".to_owned(),
            false,
        ),
    }
}

/// Why a pasted key was not taken, for the line under the row.
fn key_problem(error: &EnterError) -> String {
    match error {
        EnterError::Key(KeyError::NotAKey) => {
            "Copy the key from your email first, then press Paste Key.".to_owned()
        }
        EnterError::Key(KeyError::Damaged) => {
            "Part of that key is missing. Copy all of it, then press Paste Key.".to_owned()
        }
        EnterError::Key(KeyError::Forged) => {
            "That key wasn\u{2019}t issued for Imago Emulator.".to_owned()
        }
        EnterError::Key(KeyError::Newer) => "That key needs a newer Imago Emulator.".to_owned(),
        EnterError::Key(KeyError::Revoked) => "That key is no longer valid.".to_owned(),
        EnterError::Io(error) => {
            format!("That key is good, but it couldn\u{2019}t be saved: {error}")
        }
    }
}

/// What pressing a button in a row of buttons does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    Account(Account),
    Support(Support),
}

/// A row of buttons where choices go, with a line under them.
struct Buttons {
    /// The heading over the row, if it starts a group.
    heading: Option<&'static str>,
    name: &'static str,
    /// Each button's label, whether the accent marks it, and what it does.
    buttons: Vec<(&'static str, bool, Press)>,
    caption: String,
    /// Whether the caption tells of a problem.
    problem: bool,
}

/// A row of the window.
enum Entry {
    Choices(Row),
    Buttons(Buttons),
}

impl Entry {
    fn name(&self) -> &'static str {
        match self {
            Self::Choices(row) => row.name,
            Self::Buttons(row) => row.name,
        }
    }

    /// What its choices or buttons are called, in order.
    fn labels(&self) -> Vec<&str> {
        match self {
            Self::Choices(row) => row.labels.iter().map(SharedString::as_ref).collect(),
            Self::Buttons(row) => row.buttons.iter().map(|&(label, ..)| label).collect(),
        }
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

/// The rows a supporter of `tier` gets, after the supporter's row, for
/// `settings`: the accent, and where there is a Dock, the icons the tier
/// is offered.
fn look_rows(settings: &Settings, tier: Tier) -> Vec<Row> {
    let mut rows = vec![Row::new(
        "Accent",
        Accent::ALL
            .into_iter()
            .map(|accent| (accent.label().into(), accent))
            .collect(),
        settings.accent,
        |settings, accent| settings.accent = accent,
    )];
    if supporter::CHOOSES_ICON {
        rows.push(Row::new(
            "App Icon",
            AppIcon::offered(tier)
                .into_iter()
                .map(|icon| (icon.label().into(), icon))
                .collect(),
            settings.app_icon,
            |settings, icon| settings.app_icon = icon,
        ));
    }
    rows
}

/// How tall the window is with `look_rows` of a supporter's rows, on a
/// display `display` tall, if known: tall enough for every row, or what
/// the display has room for.
fn window_height(display: Option<f32>, look_rows: usize) -> f32 {
    let wanted = SIZE.1 + LOOK_ROW * look_rows as f32;
    display.map_or(wanted, |display| {
        wanted.min((display - DISPLAY_MARGIN).max(320.0))
    })
}

/// How many of a supporter's rows the window shows: none without a key.
fn shown_look_rows(cx: &App) -> usize {
    supporter::tier(cx).map_or(0, |tier| look_rows(&Settings::default(), tier).len())
}

impl SettingsWindow {
    /// Opens a settings window for `main`, with the keyboard on the
    /// supporter's row if `supporter`.
    pub fn open(
        main: Entity<EmulatorView>,
        supporter: bool,
        cx: &mut App,
    ) -> anyhow::Result<WindowHandle<Self>> {
        let display = cx
            .primary_display()
            .map(|display| f32::from(display.bounds().size.height));
        let height = window_height(display, shown_look_rows(cx));
        let bounds = Bounds::centered(None, size(px(SIZE.0), px(height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(top_bar("Settings")),
            is_resizable: false,
            is_minimizable: false,
            app_id: Some("ds-emulator".to_owned()),
            window_decorations: chrome::window_decorations(),
            ..WindowOptions::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let mut settings = Self::new(main, window, cx);
                if supporter {
                    settings.focused = settings.key_row();
                    // Scrolled to once the rows have been laid out.
                    cx.on_next_frame(window, |this, _, cx| this.focus_supporter(cx));
                }
                settings
            })
        })
    }

    /// The window for `main`, redrawn whenever `main`'s settings change.
    pub fn new(main: Entity<EmulatorView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        crate::x11_redraw::ensure_redraws();
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
            key_problem: None,
            _subscriptions: subscriptions,
        }
    }

    /// Which row the supporter's is: after the settings' and OpenRouter's.
    fn key_row(&self) -> usize {
        rows(&self.settings).len() + 1
    }

    /// Puts the keyboard on the supporter's row and scrolls to it.
    pub fn focus_supporter(&mut self, cx: &mut Context<Self>) {
        self.focused = self.key_row();
        self.scroll.scroll_to_item(self.focused);
        cx.notify();
    }

    /// The window's rows, top to bottom: the settings, OpenRouter's
    /// buttons, the supporter's, and with a key in, a supporter's looks.
    fn entries(&self, cx: &App) -> Vec<Entry> {
        let mut entries: Vec<Entry> = rows(&self.settings)
            .into_iter()
            .map(Entry::Choices)
            .collect();
        let status = self.openrouter.read(cx).status();
        let (caption, problem) = account_caption(status);
        entries.push(Entry::Buttons(Buttons {
            heading: None,
            name: ACCOUNT,
            buttons: account_buttons(status)
                .into_iter()
                .map(|button| (button.label(), button.primary(), Press::Account(button)))
                .collect(),
            caption,
            problem,
        }));
        let supporter = supporter::current(cx);
        let (caption, problem) = support_caption(
            supporter.map(|supporter| (supporter.name.as_str(), supporter.tier)),
            self.key_problem.as_deref(),
        );
        entries.push(Entry::Buttons(Buttons {
            heading: Some("Supporter"),
            name: KEY,
            buttons: support_buttons(supporter.is_some())
                .into_iter()
                .map(|button| (button.label(), button.primary(), Press::Support(button)))
                .collect(),
            caption,
            problem,
        }));
        if let Some(supporter) = supporter {
            let rows = look_rows(&self.settings, supporter.tier);
            entries.extend(rows.into_iter().map(Entry::Choices));
        }
        entries
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
        if let Some(Entry::Choices(row)) = self.entries(cx).get(index) {
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
                // Layout rounds the plate up to a whole point: the label
                // sits in the middle of it, not at its left.
                .text_center()
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
                    .border_color(rgba(if focused {
                        with_alpha(accent(), 0xAA)
                    } else {
                        0
                    }))
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

    /// Does what the button that does `press` says.
    fn press(&mut self, press: Press, window: &mut Window, cx: &mut Context<Self>) {
        match press {
            Press::Account(Account::Connect | Account::TryAgain) => {
                self.openrouter
                    .update(cx, |openrouter, cx| openrouter.connect(cx));
            }
            Press::Account(Account::Cancel) => self
                .openrouter
                .update(cx, |openrouter, cx| openrouter.cancel(cx)),
            Press::Account(Account::Disconnect) => {
                self.openrouter
                    .update(cx, |openrouter, cx| openrouter.disconnect(cx));
            }
            Press::Account(Account::ManageKeys) => {
                crate::links::open_web_url(cx, openrouter::KEYS_PAGE);
            }
            Press::Support(Support::Become) => {
                crate::links::open_web_url(cx, supporter::PAGE);
            }
            Press::Support(Support::PasteKey) => self.paste_key(window, cx),
            Press::Support(Support::RemoveKey) => {
                self.key_problem = supporter::remove(cx)
                    .err()
                    .map(|error| format!("The key couldn\u{2019}t be removed: {error}"));
                self.look_changed(window, cx);
            }
        }
    }

    /// Takes the clipboard's text as the supporter's key, if it is one.
    /// The clipboard is read here alone, when asked to.
    fn paste_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        self.key_problem = supporter::enter(&text, cx)
            .err()
            .map(|error| key_problem(&error));
        self.focused = self.key_row();
        self.look_changed(window, cx);
    }

    /// Shows the look the app has now that a key was put in or taken out,
    /// and makes room for a supporter's rows or takes it back.
    fn look_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(main) = self.main.upgrade() {
            main.update(cx, |main, cx| main.apply_look(cx));
        }
        let display = window
            .display(cx)
            .map(|display| f32::from(display.bounds().size.height));
        let height = window_height(display, shown_look_rows(cx));
        window.resize(size(px(SIZE.0), px(height)));
        cx.notify();
    }

    /// `row`, the `index`th, ringed if the keyboard is on it: its buttons
    /// where choices go, and its caption under them, at most `width` wide.
    fn button_row(
        &self,
        index: usize,
        row: Buttons,
        name_width: f32,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = index == self.focused;
        let name = row.name;
        let buttons = row
            .buttons
            .into_iter()
            .enumerate()
            .map(|(at, (label, primary, press))| {
                div()
                    .id((name, at))
                    .px(px(CHOICE_PADDING))
                    .py(px(3.0))
                    .rounded(px(CHOICE_RADIUS))
                    .text_center()
                    .cursor_pointer()
                    .when(primary, |this| {
                        this.bg(rgb(accent())).text_color(rgb(ON_ACCENT))
                    })
                    .when(!primary, |this| this.hover(|this| this.bg(rgb(LINE))))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focused = index;
                        this.press(press, window, cx);
                    }))
                    .child(label)
                    .children(probe::part(format!("segment:{name}.{at}")))
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
                    .child(name)
                    .children(probe::part(format!("name:{name}"))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(TRACK_PADDING))
                    .p(px(TRACK_PADDING))
                    .rounded(px(TRACK_RADIUS))
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgba(if focused {
                        with_alpha(accent(), 0xAA)
                    } else {
                        0
                    }))
                    .children(buttons)
                    .children(probe::bordered_part(format!("track:{name}"), 1.0)),
            );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(row.heading.map(|heading| section(heading, name_width)))
            .child(control)
            .child(
                div().pl(px(name_width + NAME_GAP)).child(
                    div()
                        .max_w(px(width))
                        .text_xs()
                        .text_color(rgb(if row.problem { DANGER } else { DIM }))
                        .child(row.caption)
                        .children(probe::part(format!("caption:{name}"))),
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
        let modifiers = keystroke.modifiers;
        // Paste, wherever the keyboard is: a key copied from its email.
        if pastes(&keystroke.key, modifiers) {
            self.paste_key(window, cx);
            self.scroll.scroll_to_item(self.focused);
            return;
        }
        // Other Command and Control chords are menu shortcuts.
        if modifiers.platform || modifiers.control || modifiers.alt {
            return;
        }
        let entries = self.entries(cx);
        if let Some(Entry::Buttons(row)) = entries.get(self.focused)
            && matches!(keystroke.key.as_str(), "enter" | "space")
        {
            if let Some(&(_, _, press)) = row.buttons.first() {
                self.press(press, window, cx);
            }
            return;
        }
        let focused = moved_row(self.focused, entries.len(), &keystroke.key);
        if focused != self.focused {
            self.focused = focused;
            self.scroll.scroll_to_item(focused);
            cx.notify();
            return;
        }
        let picked = match entries.get(focused) {
            Some(Entry::Choices(row)) => moved_choice(row.chosen, row.labels.len(), &keystroke.key),
            _ => None,
        };
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
        let entries = self.entries(cx);
        // A key taken out takes its rows with it.
        self.focused = self.focused.min(entries.len().saturating_sub(1));
        let widest = |width: &dyn Fn(&Entry) -> f32| entries.iter().map(width).fold(0.0, f32::max);
        let name_width = widest(&|entry| text_width(entry.name(), window)).ceil();
        let choices_width = widest(&|entry| track_width(entry.labels().into_iter(), window));
        let room = f32::from(window.viewport_size().width) - name_width - NAME_GAP - choices_width;
        let side = (room / 2.0).max(MIN_SIDE);
        let rows: Vec<AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                Entry::Choices(row) => self.row(index, row, name_width, cx),
                Entry::Buttons(row) => self.button_row(index, row, name_width, choices_width, cx),
            })
            .collect();
        let header = div()
            .h(px(TOP_BAR))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .text_color(rgb(DIM))
            .child("Settings")
            .children(probe::part("header.title"));
        let header = chrome::header(header, false, window, cx);
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
            .child(header)
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
            .children(chrome::window_edges(window, Tone::Paper, false))
            .children(probe::end("settings"))
    }
}

/// Whether `key` with `modifiers` is Paste: ⌘V on macOS, Control-V
/// elsewhere.
fn pastes(key: &str, modifiers: gpui::Modifiers) -> bool {
    key == "v" && modifiers.secondary() && !modifiers.alt && !modifiers.shift
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

    #[test]
    fn the_supporters_row_offers_a_key_or_its_removal() {
        assert_eq!(support_buttons(false), [Support::Become, Support::PasteKey]);
        assert_eq!(support_buttons(true), [Support::RemoveKey]);
        // Only becoming one is what to press.
        assert!(Support::Become.primary());
        assert!(!Support::PasteKey.primary() && !Support::RemoveKey.primary());
    }

    #[test]
    fn the_supporters_row_thanks_a_supporter_and_says_why_a_key_was_refused() {
        let (words, problem) = support_caption(None, None);
        assert!(words.starts_with("Imago Emulator is free."), "{words}");
        assert_eq!(words.contains("Dock icons"), supporter::CHOOSES_ICON);
        assert!(!problem);
        assert_eq!(
            support_caption(Some(("Ada", Tier::Supporter)), None),
            ("Thank you, Ada.".to_owned(), false)
        );
        assert_eq!(
            support_caption(Some(("", Tier::Supporter)), None),
            ("Thank you for supporting Imago Emulator.".to_owned(), false)
        );
        // The higher tiers are named.
        assert_eq!(
            support_caption(Some(("Ada", Tier::Patron)), None).0,
            "Thank you, Ada. You\u{2019}re a Patron."
        );
        assert_eq!(
            support_caption(Some(("", Tier::Founder)), None).0,
            "Thank you for supporting Imago Emulator. You\u{2019}re a Founder."
        );
        // A key refused says so, whether or not another is in.
        for supporter in [None, Some(("Ada", Tier::Patron))] {
            assert_eq!(
                support_caption(supporter, Some("That key is no longer valid.")),
                ("That key is no longer valid.".to_owned(), true)
            );
        }
        let refused = [
            KeyError::NotAKey,
            KeyError::Damaged,
            KeyError::Forged,
            KeyError::Newer,
            KeyError::Revoked,
        ]
        .map(|error| key_problem(&EnterError::Key(error)));
        for (at, words) in refused.iter().enumerate() {
            assert!(!refused[at + 1..].contains(words), "{words}");
        }
        assert!(refused[0].contains("Paste Key"), "{}", refused[0]);
        let unsaved = key_problem(&EnterError::Io(std::io::Error::other("disk full")));
        assert!(unsaved.ends_with("disk full"), "{unsaved}");
    }

    #[test]
    fn a_supporters_rows_mark_and_store_the_look() {
        let mut settings = Settings::default();
        let rows = look_rows(&settings, Tier::Supporter);
        let names: Vec<_> = rows.iter().map(|row| row.name).collect();
        if supporter::CHOOSES_ICON {
            assert_eq!(names, ["Accent", "App Icon"]);
        } else {
            assert_eq!(names, ["Accent"]);
        }
        let accent = &rows[0];
        assert_eq!(accent.heading, None, "under the supporter's heading");
        assert_eq!(accent.labels.len(), Accent::ALL.len());
        assert_eq!(accent.chosen, Some(0), "blue by default");
        (accent.choose)(&mut settings, 4);
        assert_eq!(settings.accent, Accent::Violet);
        if let Some(icon) = rows.get(1) {
            assert_eq!(icon.chosen, Some(0), "the app's own by default");
            (icon.choose)(&mut settings, 1);
            assert_eq!(settings.app_icon, AppIcon::Night);
            // Each tier's row ends with the icon that is its own.
            assert_eq!(icon.labels.len(), 5);
            let patron = look_rows(&settings, Tier::Patron).remove(1);
            assert_eq!(patron.labels.last().map(AsRef::as_ref), Some("Ember"));
            let founder = look_rows(&settings, Tier::Founder).remove(1);
            assert_eq!(founder.labels.len(), 7);
            (founder.choose)(&mut settings, 6);
            assert_eq!(settings.app_icon, AppIcon::Founder);
            // A Founder's icon is none of a Supporter's choices.
            assert_eq!(look_rows(&settings, Tier::Supporter).remove(1).chosen, None);
        }
        // None of them is among the settings everyone has.
        for row in super::rows(&settings) {
            assert!(!names.contains(&row.name), "{}", row.name);
            assert_ne!(row.name, KEY);
        }
    }

    #[test]
    fn paste_is_the_systems_shortcut_and_nothing_else() {
        let secondary = if cfg!(target_os = "macos") {
            gpui::Modifiers::command()
        } else {
            gpui::Modifiers::control()
        };
        assert!(pastes("v", secondary));
        assert!(!pastes("v", gpui::Modifiers::none()));
        assert!(!pastes("c", secondary));
        let shifted = gpui::Modifiers {
            shift: true,
            ..secondary
        };
        assert!(!pastes("v", shifted));
    }

    #[test]
    fn the_window_fits_its_rows_and_is_no_taller_than_the_display_has_room_for() {
        assert_eq!(window_height(None, 0), 800.0);
        assert_eq!(window_height(Some(1080.0), 0), 800.0);
        // A supporter's rows make it taller, each by a row and its gap.
        assert_eq!(window_height(Some(1080.0), 2), 886.0);
        assert_eq!(window_height(Some(982.0), 2), 862.0);
        assert_eq!(window_height(Some(768.0), 0), 648.0);
        assert_eq!(window_height(Some(300.0), 2), 320.0);
    }

    /// The row called `name` for `settings`.
    fn row_named(settings: &Settings, name: &str) -> Row {
        rows(settings)
            .into_iter()
            .find(|row| row.name == name)
            .expect("a row by that name")
    }
}
