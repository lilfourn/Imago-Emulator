//! Controls the library and the store share: the tabs in the top bar that
//! go between them, the segmented filters under their search fields, and
//! text cut short to fit.
//!
//! What is chosen is a white pill with dark type, as in Apple's own dark
//! apps; the accent is kept for what to press, such as Play and Get.

use gpui::{Div, SharedString, Stateful, Window, div, prelude::*, px, rgb};

use crate::icons;
use crate::probe;
use crate::theme::{BACKGROUND, LINE, MUTED, PANEL, TEXT};

/// The two places the main window shows while no game runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Library,
    Store,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Store => "Store",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Library => "tab-library",
            Self::Store => "tab-store",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Library => icons::LIBRARY,
            Self::Store => icons::STORE,
        }
    }
}

/// The narrowest window whose top bar has room for the tabs' names; a
/// narrower one shows their icons.
pub const ROOM_FOR_TAB_NAMES: f32 = 320.0;

/// Height of the tabs' track: an icon button's, so both sit on the top
/// bar's midline. Room inside it around each tab.
const TABS_HEIGHT: f32 = 28.0;
const TABS_PADDING: f32 = 2.0;

/// The tabs' track, holding a [`tab`] for each section.
pub fn tabs(tabs: [Stateful<Div>; 2]) -> Div {
    div()
        .flex_none()
        .h(px(TABS_HEIGHT))
        .flex()
        .items_center()
        .gap(px(TABS_PADDING))
        .p(px(TABS_PADDING))
        .rounded_full()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(LINE))
        .children(tabs)
        .children(probe::bordered_part("tabs", 1.0))
}

/// The tab for `section`, filled if it is the `chosen` one: its name, or
/// its icon in a window too narrow for names (see
/// [`ROOM_FOR_TAB_NAMES`]). The caller says what a click on it does.
pub fn tab(section: Section, chosen: Section, window: &Window) -> Stateful<Div> {
    let inner = TABS_HEIGHT - 2.0 * TABS_PADDING - 2.0;
    let named = f32::from(window.viewport_size().width) >= ROOM_FOR_TAB_NAMES;
    let colour = if section == chosen { BACKGROUND } else { MUTED };
    pill(div().id(section.id()), section == chosen)
        .h(px(inner))
        .px(px(if named { 12.0 } else { 8.0 }))
        .text_size(px(13.0))
        .when(named, |tab| tab.child(section.label()))
        .when(!named, |tab| {
            tab.child(icons::icon(section.icon(), 14.0, colour))
        })
        .children(probe::part(format!(
            "tab:{}",
            section.label().to_lowercase()
        )))
}

/// A segmented control called `name` holding `choices`.
pub fn track(name: &'static str, choices: Vec<Stateful<Div>>) -> Div {
    div()
        .flex()
        .gap(px(TRACK_PADDING))
        .p(px(TRACK_PADDING))
        .rounded(px(TRACK_RADIUS))
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(LINE))
        .children(choices)
        .children(probe::bordered_part(format!("track:{name}"), 1.0))
}

/// Room around and inside each choice of a [`track`].
const TRACK_PADDING: f32 = 2.0;
const TRACK_RADIUS: f32 = 8.0;
const CHOICE_RADIUS: f32 = TRACK_RADIUS - TRACK_PADDING - 1.0;
const CHOICE_PADDING: f32 = 10.0;

/// One choice in a [`track`], labelled `label`, filled if `chosen`.
pub fn choice(
    id: (&'static str, usize),
    label: impl Into<SharedString>,
    chosen: bool,
) -> Stateful<Div> {
    pill(div().id(id), chosen)
        .px(px(CHOICE_PADDING))
        .py(px(3.0))
        .rounded(px(CHOICE_RADIUS))
        .child(label.into())
}

/// A tab or choice: white with dark type if `chosen`, else muted type that
/// brightens under the pointer.
fn pill(element: Stateful<Div>, chosen: bool) -> Stateful<Div> {
    element
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .whitespace_nowrap()
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(chosen, |pill| {
            pill.bg(rgb(TEXT)).text_color(rgb(BACKGROUND))
        })
        .when(!chosen, |pill| {
            pill.cursor_pointer()
                .text_color(rgb(MUTED))
                .hover(|pill| pill.text_color(rgb(TEXT)).bg(rgb(LINE)))
        })
}

/// `text` cut short with an ellipsis to fit `width` logical pixels at
/// `size` and `weight`. GPUI's own truncation cuts text at the width it
/// was first measured at, which in a list is none.
pub fn fit(
    text: &str,
    width: f32,
    size: f32,
    weight: gpui::FontWeight,
    window: &Window,
    cx: &gpui::App,
) -> SharedString {
    let mut font = window.text_style().font();
    font.weight = weight;
    let mut wrapper = cx.text_system().line_wrapper(font, px(size));
    let line = wrapper.truncate_line(text.to_owned().into(), px(width), "…", &mut Vec::new());
    line.replace(" …", "…").into()
}
