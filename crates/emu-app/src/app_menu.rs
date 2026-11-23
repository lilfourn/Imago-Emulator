//! The menu bar off macOS, where GPUI draws none: a menu that hangs from
//! the button in the top bar's left corner (or opens with F10), with the
//! menus as rows that open their items beside them, as a desktop's context
//! menus do. It is built from the menus the app gives GPUI
//! (`app::menus`, read back with `cx.get_menus()`), so it never differs
//! from macOS's menu bar: its submenus, separators, ticks (an item named
//! `✓ …`), shortcuts, and items dimmed when nothing in the window would
//! act on them, as macOS dims them.
//!
//! The keyboard works it as a menu bar does: ↑ and ↓ choose, → and Return
//! open a submenu, ← and Escape close one, Return runs an item, a letter
//! goes to the next item starting with it, and Escape or F10 at the top
//! closes the menu.

use gpui::{
    Action, App, Context, EventEmitter, FocusHandle, KeyDownEvent, MouseButton, OwnedMenu,
    OwnedMenuItem, Pixels, Point, ScrollHandle, SharedString, Window, div, prelude::*, px, rgb,
    rgba,
};

use crate::icons;
use crate::keys;
use crate::probe;
use crate::theme::{BACKGROUND, LINE, MUTED, TEXT, with_alpha};

/// A panel's edge, the room inside it, and its corners; its rows' corners
/// share their centre. As the library's game menu draws them.
const BORDER: f32 = 1.0;
const PADDING: f32 = 6.0;
const RADIUS: f32 = 12.0;
const ROW_RADIUS: f32 = RADIUS - BORDER - PADDING;
/// A row's height, and how far in from its sides its words sit.
const ROW: f32 = 28.0;
const ROW_INSET: f32 = 8.0;
/// A separator's height: a hairline with room above and below.
const RULE: f32 = 9.0;
/// The words' sizes: an item's, and its shortcut's.
const LABEL_SIZE: f32 = 13.0;
const KEYS_SIZE: f32 = 12.0;
/// The column the ticks stand in, the room between an item's name and
/// its shortcut, and a submenu's chevron.
const TICK_COLUMN: f32 = 22.0;
const KEYS_GAP: f32 = 28.0;
const CHEVRON: f32 = 14.0;
/// The narrowest panel, and how far a panel stays from the window's
/// sides.
const MIN_WIDTH: f32 = 168.0;
const MARGIN: f32 = 8.0;
/// How far a submenu tucks over the panel it opens from.
const OVERLAP: f32 = 4.0;

/// What the menu asks of the window it is in.
pub enum AppMenuEvent {
    /// Run this item's action; the menu is done.
    Run(Box<dyn Action>),
    /// Close the menu without running anything.
    Dismiss,
}

/// One row of a menu.
enum Entry {
    Separator,
    Submenu {
        name: SharedString,
        entries: Vec<Entry>,
    },
    Item {
        name: SharedString,
        /// Whether a tick shows before it, for an item of a set of
        /// choices (`Some(false)` for one not chosen); `None` for an
        /// item that is no choice.
        ticked: Option<bool>,
        action: Box<dyn Action>,
        enabled: bool,
        /// Its shortcut, as the system writes it.
        keys: Option<String>,
    },
}

impl Entry {
    fn chooseable(&self) -> bool {
        match self {
            Self::Separator => false,
            Self::Submenu { entries, .. } => !entries.is_empty(),
            Self::Item { enabled, .. } => *enabled,
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            Self::Separator => None,
            Self::Submenu { name, .. } | Self::Item { name, .. } => Some(name),
        }
    }

    fn height(&self) -> f32 {
        if matches!(self, Self::Separator) {
            RULE
        } else {
            ROW
        }
    }
}

/// The menu, open over the window.
pub struct AppMenu {
    focus: FocusHandle,
    /// The menus, each a row of the first panel.
    menus: Vec<Entry>,
    /// The submenus open, by their row in the panel before: the first
    /// panel's, then each one's after it.
    open: Vec<usize>,
    /// The row chosen in the last panel open, if one is.
    cursor: Option<usize>,
    /// Each panel's scrolling, for one taller than the window.
    scrolls: Vec<ScrollHandle>,
    /// Where the first panel's top left corner hangs, in the window.
    anchor: Point<Pixels>,
}

impl EventEmitter<AppMenuEvent> for AppMenu {}

impl AppMenu {
    /// The menu of `menus`, hanging at `anchor`, with each item's
    /// shortcut and whether anything in `window` acts on it as it stands.
    /// Opened from the keyboard, its first menu is chosen.
    pub fn new(
        menus: Vec<OwnedMenu>,
        anchor: Point<Pixels>,
        from_keyboard: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let menus: Vec<Entry> = menus
            .into_iter()
            .map(|menu| Entry::Submenu {
                name: menu.name,
                entries: entries(menu.items, window, cx),
            })
            .collect();
        let cursor = from_keyboard
            .then(|| next_chooseable(&menus, None, true))
            .flatten();
        Self {
            focus: cx.focus_handle(),
            menus,
            open: Vec::new(),
            cursor,
            scrolls: Vec::new(),
            anchor,
        }
    }

    /// Takes the keyboard, while it shows.
    pub fn focus(&self, window: &mut Window) {
        self.focus.focus(window);
    }

    /// The rows of panel `level`.
    fn panel(&self, level: usize) -> &[Entry] {
        let mut entries = self.menus.as_slice();
        for &row in &self.open[..level] {
            match entries.get(row) {
                Some(Entry::Submenu { entries: inner, .. }) => entries = inner,
                _ => return &[],
            }
        }
        entries
    }

    /// The pointer is on row `row` of panel `level`: choose it, and open
    /// it if it is a submenu, closing any open past it.
    fn hover(&mut self, level: usize, row: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.panel(level).get(row) else {
            return;
        };
        if !entry.chooseable() {
            return;
        }
        let submenu = matches!(entry, Entry::Submenu { .. });
        let changed = if submenu {
            self.open.len() != level + 1 || self.open[level] != row
        } else {
            self.open.len() != level || self.cursor != Some(row)
        };
        if !changed {
            return;
        }
        self.open.truncate(level);
        if submenu {
            self.open.push(row);
            self.cursor = None;
        } else {
            self.cursor = Some(row);
        }
        cx.notify();
    }

    /// A click on row `row` of panel `level`: runs it, or opens it.
    fn press(&mut self, level: usize, row: usize, cx: &mut Context<Self>) {
        self.hover(level, row, cx);
        if let Some(Entry::Item {
            action,
            enabled: true,
            ..
        }) = self.panel(level).get(row)
        {
            cx.emit(AppMenuEvent::Run(action.boxed_clone()));
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Every key is the menu's while it is open; none reaches the game.
        cx.stop_propagation();
        let keystroke = &event.keystroke;
        let level = self.open.len();
        match keystroke.key.as_str() {
            "down" | "up" => {
                let forward = keystroke.key == "down";
                self.cursor = next_chooseable(self.panel(level), self.cursor, forward);
                self.scroll_to_cursor();
            }
            "home" => self.cursor = next_chooseable(self.panel(level), None, true),
            "end" => self.cursor = next_chooseable(self.panel(level), None, false),
            "right" | "enter" | "space" => {
                let Some(row) = self.cursor else {
                    return;
                };
                match self.panel(level).get(row) {
                    Some(Entry::Submenu { entries, .. }) => {
                        let first = next_chooseable(entries, None, true);
                        self.open.push(row);
                        self.cursor = first;
                    }
                    Some(Entry::Item {
                        action,
                        enabled: true,
                        ..
                    }) if keystroke.key != "right" => {
                        cx.emit(AppMenuEvent::Run(action.boxed_clone()));
                        return;
                    }
                    _ => return,
                }
            }
            "left" if level > 0 => self.cursor = self.open.pop(),
            "left" => return,
            "escape" if level > 0 => self.cursor = self.open.pop(),
            "escape" | "f10" => {
                cx.emit(AppMenuEvent::Dismiss);
                return;
            }
            _ => {
                // A letter goes to the next item starting with it.
                let Some(letter) = keystroke
                    .key_char
                    .as_deref()
                    .filter(|_| !keystroke.modifiers.secondary() && !keystroke.modifiers.alt)
                    .and_then(|text| text.chars().next())
                else {
                    return;
                };
                let entries = self.panel(level);
                let Some(found) = next_starting_with(entries, self.cursor, letter) else {
                    return;
                };
                self.cursor = Some(found);
                self.scroll_to_cursor();
            }
        }
        cx.notify();
    }

    fn scroll_to_cursor(&self) {
        if let (Some(row), Some(scroll)) = (self.cursor, self.scrolls.get(self.open.len())) {
            scroll.scroll_to_item(row);
        }
    }

    /// Panel `level`, at `left`, `top`, `width` wide and at most `tallest`
    /// tall, with the row `chosen` chosen.
    #[allow(clippy::too_many_arguments)]
    fn render_panel(
        &self,
        level: usize,
        left: f32,
        top: f32,
        width: f32,
        tallest: f32,
        chosen: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let entries = self.panel(level);
        let ticks = entries.iter().any(|entry| {
            matches!(
                entry,
                Entry::Item {
                    ticked: Some(_),
                    ..
                }
            )
        });
        let rows: Vec<gpui::AnyElement> = entries
            .iter()
            .enumerate()
            .map(|(row, entry)| row_element(level, row, entry, ticks, chosen == Some(row), cx))
            .collect();
        let scroll = self.scrolls.get(level).cloned().unwrap_or_default();
        div()
            .id(("app-menu-panel", level))
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .max_h(px(tallest))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .occlude()
            .flex()
            .flex_col()
            .p(px(PADDING))
            .rounded(px(RADIUS))
            .bg(rgb(BACKGROUND))
            .border(px(BORDER))
            .border_color(rgba(with_alpha(TEXT, 0x1F)))
            .shadow_lg()
            .text_color(rgb(TEXT))
            .text_size(px(LABEL_SIZE))
            .children(rows)
            .children(probe::bordered_part(format!("menu.panel:{level}"), BORDER))
    }
}

/// One row of panel `level`: a separator, a submenu with its chevron, or
/// an item with its tick and shortcut, dimmed if it can't be done.
fn row_element(
    level: usize,
    row: usize,
    entry: &Entry,
    ticks: bool,
    chosen: bool,
    cx: &mut Context<AppMenu>,
) -> gpui::AnyElement {
    let (name, ticked, keys, enabled, submenu) = match entry {
        Entry::Separator => {
            return div()
                .h(px(RULE))
                .flex_none()
                .flex()
                .items_center()
                .px(px(ROW_INSET))
                .child(div().h(px(1.0)).w_full().bg(rgb(LINE)))
                .into_any_element();
        }
        Entry::Submenu { name, entries } => (name, None, None, !entries.is_empty(), true),
        Entry::Item {
            name,
            ticked,
            keys,
            enabled,
            ..
        } => (name, *ticked, keys.as_deref(), *enabled, false),
    };
    let colour = if enabled { TEXT } else { MUTED };
    let tick = ticks.then(|| {
        div()
            .w(px(TICK_COLUMN))
            .flex_none()
            .children((ticked == Some(true)).then(|| icons::icon(icons::TICK, 14.0, colour)))
    });
    let id = ("app-menu-row", level * 1000 + row);
    div()
        .id(id)
        .h(px(ROW))
        .flex_none()
        .flex()
        .items_center()
        .px(px(ROW_INSET))
        .rounded(px(ROW_RADIUS))
        .text_color(rgb(colour))
        .when(chosen && enabled, |row| {
            row.bg(rgba(with_alpha(TEXT, 0x1A)))
        })
        .when(enabled, |element| {
            element
                .cursor_pointer()
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.hover(level, row, cx);
                    }
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.press(level, row, cx)))
        })
        .children(tick)
        .child(div().flex_1().min_w_0().truncate().child(name.clone()))
        .children(keys.map(|keys| {
            div()
                .flex_none()
                .pl(px(KEYS_GAP))
                .text_size(px(KEYS_SIZE))
                .text_color(rgb(MUTED))
                .child(keys.to_owned())
        }))
        .children(submenu.then(|| {
            div().flex_none().pl(px(KEYS_GAP / 2.0)).child(icons::icon(
                icons::CHEVRON_RIGHT,
                CHEVRON,
                MUTED,
            ))
        }))
        .children(probe::part(format!("menu.row:{name}")))
        .into_any_element()
}

impl Render for AppMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let (room_x, room_y) = (f32::from(viewport.width), f32::from(viewport.height));
        let levels = self.open.len() + 1;
        self.scrolls.resize_with(levels, ScrollHandle::new);
        self.scrolls.truncate(levels);
        let tallest = (room_y - 2.0 * MARGIN).max(ROW);
        let mut panels = Vec::with_capacity(levels);
        let mut left = f32::from(self.anchor.x);
        let mut top = f32::from(self.anchor.y);
        let mut before: Option<(f32, f32)> = None;
        for level in 0..levels {
            let entries = self.panel(level);
            let width = panel_width(entries, window).min(room_x - 2.0 * MARGIN);
            let height = panel_height(entries).min(tallest);
            if let Some((parent_left, parent_width)) = before {
                // Beside the panel it opens from, or else on its other
                // side, and level with the row it opens from.
                let right = parent_left + parent_width - OVERLAP;
                left = if right + width <= room_x - MARGIN {
                    right
                } else {
                    (parent_left - width + OVERLAP).max(MARGIN)
                };
                let parent = self.panel(level - 1);
                let row = self.open[level - 1];
                let scrolled = self
                    .scrolls
                    .get(level - 1)
                    .map_or(0.0, |scroll| f32::from(scroll.offset().y));
                top += row_top(parent, row) + scrolled;
            }
            top = top.min(room_y - MARGIN - height).max(MARGIN);
            let chosen = if level + 1 < levels {
                Some(self.open[level])
            } else {
                self.cursor
            };
            panels.push(self.render_panel(level, left, top, width, tallest, chosen, cx));
            before = Some((left, width));
        }
        div()
            .id("app-menu")
            .track_focus(&self.focus)
            .key_context("AppMenu")
            .absolute()
            .inset_0()
            .on_key_down(cx.listener(Self::key_down))
            .child(
                // A press anywhere else closes the menu, and does nothing
                // else: never the game's A button.
                div()
                    .id("app-menu-backdrop")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(AppMenuEvent::Dismiss);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(AppMenuEvent::Dismiss);
                        }),
                    ),
            )
            .children(panels)
    }
}

/// `items`, a menu's items as GPUI keeps them, as rows: each item with its
/// tick, its shortcut where `window` has its focus, and whether anything
/// there acts on it. The Services menu, which is macOS's own, is left out.
fn entries(items: Vec<OwnedMenuItem>, window: &mut Window, cx: &mut App) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            OwnedMenuItem::Separator => {
                // Never two together, nor one first.
                if entries
                    .last()
                    .is_some_and(|last| !matches!(last, Entry::Separator))
                {
                    entries.push(Entry::Separator);
                }
            }
            OwnedMenuItem::Submenu(menu) => entries.push(Entry::Submenu {
                name: menu.name,
                entries: entries_of(menu.items, window, cx),
            }),
            OwnedMenuItem::SystemMenu(_) => {}
            OwnedMenuItem::Action { name, action, .. } => {
                let (ticked, name) = tick(&name);
                // Acted on along the focus's path in this window, or by
                // the app: as macOS enables menu items.
                let enabled = window.is_action_available(action.as_ref(), cx)
                    || cx.is_action_available(action.as_ref());
                let keys = keys::for_action(action.as_ref(), window);
                entries.push(Entry::Item {
                    name: name.to_owned().into(),
                    ticked,
                    action,
                    enabled,
                    keys,
                });
            }
        }
    }
    if matches!(entries.last(), Some(Entry::Separator)) {
        entries.pop();
    }
    entries
}

fn entries_of(items: Vec<OwnedMenuItem>, window: &mut Window, cx: &mut App) -> Vec<Entry> {
    entries(items, window, cx)
}

/// Whether the item named `name` is a ticked choice, one not ticked, or
/// no choice, and its name without the mark: `app::menus` names a choice
/// `✓ name` or, not chosen, `\u{2003}name`, as macOS's menus need.
fn tick(name: &str) -> (Option<bool>, &str) {
    if let Some(rest) = name.strip_prefix("✓ ") {
        (Some(true), rest)
    } else if let Some(rest) = name.strip_prefix('\u{2003}') {
        (Some(false), rest)
    } else {
        (None, name)
    }
}

/// The next row of `entries` after `from` (or the first, or with
/// `forward` false the one before, or the last) that can be chosen,
/// going round.
fn next_chooseable(entries: &[Entry], from: Option<usize>, forward: bool) -> Option<usize> {
    let count = entries.len();
    if count == 0 {
        return None;
    }
    let start = match (from, forward) {
        (Some(at), true) => at + 1,
        (Some(at), false) => at + count - 1,
        (None, true) => 0,
        (None, false) => count - 1,
    };
    (0..count)
        .map(|step| {
            if forward {
                (start + step) % count
            } else {
                (start + count - step) % count
            }
        })
        .find(|&row| entries[row].chooseable())
}

/// The next row after `from` that can be chosen and whose name starts
/// with `letter`, going round.
fn next_starting_with(entries: &[Entry], from: Option<usize>, letter: char) -> Option<usize> {
    let count = entries.len();
    let start = from.map_or(0, |at| at + 1);
    let letter = letter.to_lowercase().collect::<String>();
    (0..count).map(|step| (start + step) % count).find(|&row| {
        let entry = &entries[row];
        entry.chooseable()
            && entry
                .name()
                .is_some_and(|name| name.to_lowercase().starts_with(&letter))
    })
}

/// Where row `row` of a panel of `entries` starts, from the panel's top.
fn row_top(entries: &[Entry], row: usize) -> f32 {
    entries[..row.min(entries.len())]
        .iter()
        .map(Entry::height)
        .sum()
}

/// How tall a panel of `entries` is drawn.
fn panel_height(entries: &[Entry]) -> f32 {
    entries.iter().map(Entry::height).sum::<f32>() + 2.0 * (PADDING + BORDER)
}

/// How wide a panel of `entries` is drawn: its widest row's name and
/// shortcut, or chevron, and its ticks' column, in its room.
fn panel_width(entries: &[Entry], window: &Window) -> f32 {
    let ticks = entries.iter().any(|entry| {
        matches!(
            entry,
            Entry::Item {
                ticked: Some(_),
                ..
            }
        )
    });
    let widest = entries
        .iter()
        .map(|entry| match entry {
            Entry::Separator => 0.0,
            Entry::Submenu { name, .. } => {
                text_width(name, LABEL_SIZE, window) + KEYS_GAP / 2.0 + CHEVRON
            }
            Entry::Item { name, keys, .. } => {
                text_width(name, LABEL_SIZE, window)
                    + keys
                        .as_deref()
                        .map_or(0.0, |keys| KEYS_GAP + text_width(keys, KEYS_SIZE, window))
            }
        })
        .fold(0.0, f32::max);
    let tick = if ticks { TICK_COLUMN } else { 0.0 };
    (widest + tick + 2.0 * (ROW_INSET + PADDING + BORDER) + 2.0)
        .ceil()
        .max(MIN_WIDTH)
}

/// How wide `text` is drawn at `size` in the window's words.
fn text_width(text: &str, size: f32, window: &Window) -> f32 {
    let run = window.text_style().to_run(text.len());
    let line = window
        .text_system()
        .shape_line(text.to_owned().into(), px(size), &[run], None);
    f32::from(line.width)
}

#[cfg(test)]
mod tests {
    use super::*;

    gpui::actions!(app_menu_tests, [Nothing]);

    fn item(name: &str, enabled: bool) -> Entry {
        let (ticked, name) = tick(name);
        Entry::Item {
            name: name.to_owned().into(),
            ticked,
            action: Box::new(Nothing),
            enabled,
            keys: None,
        }
    }

    #[test]
    fn ticks_come_from_the_names_macos_menus_show() {
        assert_eq!(tick("✓ 4×"), (Some(true), "4×"));
        assert_eq!(tick("\u{2003}2×"), (Some(false), "2×"));
        assert_eq!(tick("Reset"), (None, "Reset"));
    }

    #[test]
    fn the_keys_skip_separators_and_dimmed_items_and_go_round() {
        let entries = vec![
            item("Open", true),
            Entry::Separator,
            item("Insert", false),
            item("Edit", true),
        ];
        assert_eq!(next_chooseable(&entries, None, true), Some(0));
        assert_eq!(next_chooseable(&entries, Some(0), true), Some(3));
        assert_eq!(next_chooseable(&entries, Some(3), true), Some(0));
        assert_eq!(next_chooseable(&entries, Some(0), false), Some(3));
        assert_eq!(next_chooseable(&entries, None, false), Some(3));
        assert_eq!(next_chooseable(&[], None, true), None);
    }

    #[test]
    fn a_letter_goes_to_the_next_item_starting_with_it() {
        let entries = vec![
            item("Save State", true),
            item("Slow Motion", true),
            item("Reset", true),
            item("Swap", false),
        ];
        assert_eq!(next_starting_with(&entries, None, 's'), Some(0));
        assert_eq!(next_starting_with(&entries, Some(0), 'S'), Some(1));
        assert_eq!(next_starting_with(&entries, Some(1), 's'), Some(0));
        assert_eq!(next_starting_with(&entries, None, 'x'), None);
    }
}
