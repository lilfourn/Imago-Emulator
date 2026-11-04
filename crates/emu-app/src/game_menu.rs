//! The game menu: a sheet that slides up over the paused game when the
//! player presses Escape. Four tiles: resume, cheats, restart and back to
//! the library, with the keys that play the game under them (see
//! [`key_guide`]). Cheats and restarting open pages of their own in the
//! sheet; restarting asks first, since it erases the game's progress.
//!
//! Progress needs no saving here: the main window keeps a checkpoint of
//! each game as the player leaves it, and picks it up when the game starts.
//!
//! Everything can be clicked. The arrow keys (or WASD) move, Return or
//! Space chooses and Escape goes back. The menu only shows what it is
//! given and says what was chosen, through [`MenuEvent`]; the main window
//! does the rest.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, Div, EventEmitter, KeyDownEvent,
    ScrollHandle, SharedString, Stateful, Window, div, ease_out_quint, prelude::*, px, rgb, rgba,
};

use crate::icons;
use crate::key_guide::{self, Control};
use crate::probe;
use crate::theme::{
    ACCENT, BACKGROUND, DANGER, INSET, LINE, MUTED as DIM, ON_ACCENT, TEXT, with_alpha,
};

/// The sheet: white, a touch see-through.
const SHEET: u32 = with_alpha(BACKGROUND, 0xF2);
const TILE: u32 = with_alpha(TEXT, 0x0A);
const TILE_HOVER: u32 = with_alpha(TEXT, 0x14);
const DANGER_FILL: u32 = DANGER;
/// The ring around whatever the keyboard is on, and how thick it is.
const FOCUS: u32 = with_alpha(ACCENT, 0xCC);
const RING: f32 = 2.0;

/// Widest the sheet gets, in logical pixels.
const MAX_WIDTH: f32 = 420.0;
/// Room around and inside the sheet, and between tiles.
const MARGIN: f32 = INSET;
const PADDING: f32 = 16.0;
const GAP: f32 = 8.0;
/// Height of a tile, a row and a button.
const TILE_HEIGHT: f32 = 80.0;
const ROW_HEIGHT: f32 = 40.0;
/// The narrowest a tile gets in a row of four; narrower, the tiles go
/// two to a row.
const MIN_TILE: f32 = 64.0;

/// What the player chose in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEvent {
    /// Close the menu and play on.
    Resume,
    /// Switch this cheat, by its place in the list, on or off.
    ToggleCheat(usize),
    /// Open the cheat file in a text editor.
    EditCheats,
    /// Erase the game's progress and start it from the beginning.
    StartOver,
    /// Stop the game, keeping its progress, and show the library.
    ExitToLibrary,
}

/// A cheat as the menu lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheatRow {
    /// The name from its `[name]` line.
    pub name: String,
    /// Whether it is on.
    pub enabled: bool,
}

/// The four tiles, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tile {
    Resume,
    Cheats,
    Restart,
    Library,
}

const TILES: [Tile; 4] = [Tile::Resume, Tile::Cheats, Tile::Restart, Tile::Library];

impl Tile {
    fn icon(self) -> &'static str {
        match self {
            Self::Resume => icons::PLAY,
            Self::Cheats => icons::CHEATS,
            Self::Restart => icons::RESTART,
            Self::Library => icons::LIBRARY,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Resume => "Resume",
            Self::Cheats => "Cheats",
            Self::Restart => "Restart",
            Self::Library => "Library",
        }
    }
}

/// What the sheet shows, and what the keyboard is on there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    /// The tiles, with this one chosen.
    Tiles(Tile),
    /// The cheats, with this one chosen; one past the last is Edit
    /// Cheats.
    Cheats(usize),
    /// Asking before restarting; whether Start Over rather than Cancel is
    /// chosen.
    Restart(bool),
}

/// Where `tile` is in [`TILES`].
fn tile_index(tile: Tile) -> usize {
    TILES.iter().position(|&t| t == tile).unwrap_or(0)
}

/// How many tiles go in a row across `width` logical pixels, and how wide
/// each is: all four, or two to a row when four would be too narrow.
fn tile_grid(width: f32) -> (usize, f32) {
    let side = |columns: usize| (width - GAP * (columns - 1) as f32) / columns as f32;
    let columns = if side(TILES.len()) >= MIN_TILE {
        TILES.len()
    } else {
        TILES.len() / 2
    };
    (columns, side(columns))
}

/// Where pressing `key` on `page` goes, with `cheats` cheats and the tiles
/// `columns` to a row. Up and down only move between rows of tiles.
fn moved(page: Page, key: &str, cheats: usize, columns: usize) -> Page {
    let columns = columns.max(1);
    let last = TILES.len() - 1;
    match (page, key) {
        (Page::Tiles(tile), "left" | "a") => Page::Tiles(TILES[tile_index(tile).saturating_sub(1)]),
        (Page::Tiles(tile), "right" | "d") => Page::Tiles(TILES[(tile_index(tile) + 1).min(last)]),
        (Page::Tiles(tile), "up" | "w") => {
            let at = tile_index(tile);
            Page::Tiles(TILES[at.checked_sub(columns).unwrap_or(at)])
        }
        (Page::Tiles(tile), "down" | "s") => {
            let at = tile_index(tile);
            let below = Some(at + columns).filter(|&below| below <= last);
            Page::Tiles(TILES[below.unwrap_or(at)])
        }
        (Page::Cheats(at), "up" | "w") => Page::Cheats(at.saturating_sub(1)),
        // Edit Cheats sits one past the last cheat.
        (Page::Cheats(at), "down" | "s") => Page::Cheats((at + 1).min(cheats)),
        (Page::Cheats(_), "left" | "a") => Page::Tiles(Tile::Cheats),
        (Page::Restart(_), "left" | "a") => Page::Restart(false),
        (Page::Restart(_), "right" | "d") => Page::Restart(true),
        _ => page,
    }
}

/// Which of the items `page` lists the keyboard is on there, with
/// `cheats` cheats and the tiles `columns` to a row: the row of tiles, the
/// cheat (after the hint that there are none) or the buttons under the
/// question.
fn focused_item(page: Page, cheats: usize, columns: usize) -> usize {
    match page {
        Page::Tiles(tile) => tile_index(tile) / columns.max(1),
        Page::Cheats(at) => at + usize::from(cheats == 0),
        Page::Restart(_) => 1,
    }
}

/// The game menu's view.
pub struct GameMenu {
    /// The game's name, over the menu.
    title: SharedString,
    cheats: Vec<CheatRow>,
    /// The keys that play the game, under the tiles.
    controls: [Vec<Control>; 2],
    page: Page,
    /// How many tiles go in a row, as last drawn.
    columns: usize,
    /// Scrolls the page when it is taller than the window has room for.
    scroll: ScrollHandle,
}

impl EventEmitter<MenuEvent> for GameMenu {}

impl GameMenu {
    /// A menu for the game called `title`, with its `cheats` and the
    /// `controls` that play it (see [`key_guide::columns`]).
    pub fn new(
        title: impl Into<SharedString>,
        cheats: Vec<CheatRow>,
        controls: [Vec<Control>; 2],
    ) -> Self {
        Self {
            title: title.into(),
            cheats,
            controls,
            page: Page::Tiles(Tile::Resume),
            columns: TILES.len(),
            scroll: ScrollHandle::new(),
        }
    }

    /// Shows `cheats` in place of the list shown.
    pub fn set_cheats(&mut self, cheats: Vec<CheatRow>, cx: &mut Context<Self>) {
        self.cheats = cheats;
        if let Page::Cheats(at) = self.page {
            self.go(Page::Cheats(at.min(self.cheats.len())));
        }
        cx.notify();
    }

    /// Handles a key while the menu shows.
    pub fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.is_held && matches!(event.keystroke.key.as_str(), "escape" | "enter" | "space") {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.back(cx),
            "enter" | "space" => self.choose(self.page, cx),
            key => self.go(moved(self.page, key, self.cheats.len(), self.columns)),
        }
        cx.notify();
    }

    /// Puts the keyboard on `page`, scrolling what it is on into view.
    fn go(&mut self, page: Page) {
        self.page = page;
        let item = focused_item(page, self.cheats.len(), self.columns);
        self.scroll.scroll_to_item(item);
    }

    /// Goes back a page, or closes the menu from the tiles.
    fn back(&mut self, cx: &mut Context<Self>) {
        match self.page {
            Page::Tiles(_) => cx.emit(MenuEvent::Resume),
            Page::Cheats(_) => self.go(Page::Tiles(Tile::Cheats)),
            Page::Restart(_) => self.go(Page::Tiles(Tile::Restart)),
        }
    }

    fn choose(&mut self, page: Page, cx: &mut Context<Self>) {
        self.go(page);
        match page {
            Page::Tiles(Tile::Resume) => cx.emit(MenuEvent::Resume),
            Page::Tiles(Tile::Cheats) => self.go(Page::Cheats(0)),
            Page::Tiles(Tile::Restart) => self.go(Page::Restart(false)),
            Page::Tiles(Tile::Library) => cx.emit(MenuEvent::ExitToLibrary),
            Page::Cheats(at) if at < self.cheats.len() => cx.emit(MenuEvent::ToggleCheat(at)),
            Page::Cheats(_) => cx.emit(MenuEvent::EditCheats),
            Page::Restart(true) => cx.emit(MenuEvent::StartOver),
            Page::Restart(false) => self.go(Page::Tiles(Tile::Restart)),
        }
        cx.notify();
    }

    /// A clickable part of the sheet that `page` chooses, ringed while the
    /// keyboard is on it.
    fn part(
        &self,
        page: Page,
        id: impl Into<gpui::ElementId>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .border(px(RING))
            .border_color(rgba(if self.page == page { FOCUS } else { 0 }))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.choose(page, cx)))
    }

    fn header(&self, subtitle: Option<&'static str>, cx: &mut Context<Self>) -> AnyElement {
        let back = subtitle.map(|_| {
            icons::button("back", icons::BACK)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.back(cx);
                    cx.notify();
                }))
                .children(probe::part("button:back"))
        });
        let title = div()
            .flex_1()
            .text_color(rgb(TEXT))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .line_clamp(1)
            .child(subtitle.map_or_else(|| self.title.clone(), SharedString::from))
            .children(probe::part("menu.title"));
        // As tall as the back button with it or without.
        div()
            .flex_none()
            .h(px(icons::BUTTON))
            .flex()
            .items_center()
            .gap_1()
            .children(back)
            .child(title)
            .children(probe::part("menu.header"))
            .into_any_element()
    }

    /// `tile`, `side` logical pixels wide.
    fn tile(&self, tile: Tile, side: f32, cx: &mut Context<Self>) -> AnyElement {
        let resume = tile == Tile::Resume;
        let colour = if resume { ON_ACCENT } else { TEXT };
        self.part(Page::Tiles(tile), tile.label(), cx)
            .w(px(side))
            .h(px(TILE_HEIGHT))
            .rounded(px(14.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(rgba(if resume {
                with_alpha(ACCENT, 0xFF)
            } else {
                TILE
            }))
            .when(resume, |tile| tile.hover(|tile| tile.opacity(0.9)))
            .when(!resume, |tile| tile.hover(|tile| tile.bg(rgba(TILE_HOVER))))
            .child(
                div()
                    .flex_none()
                    .child(icons::icon(tile.icon(), 20.0, colour))
                    .children(probe::part(format!("tile.icon:{}", tile.label()))),
            )
            .child(
                // A line no taller than the letters, so the icon and the
                // label are centred together by what is drawn.
                div()
                    .text_xs()
                    .line_height(px(12.0))
                    .text_color(rgb(if resume { ON_ACCENT } else { DIM }))
                    .child(tile.label())
                    .children(probe::part(format!("tile.label:{}", tile.label()))),
            )
            .children(probe::bordered_part(format!("tile:{}", tile.label()), RING))
            .into_any_element()
    }

    /// The tiles, in rows of `columns`, each `side` wide, then the keys
    /// that play the game across `width`.
    fn tiles(
        &self,
        columns: usize,
        side: f32,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for row in TILES.chunks(columns.max(1)) {
            let tiles: Vec<AnyElement> =
                row.iter().map(|&tile| self.tile(tile, side, cx)).collect();
            rows.push(div().flex().gap(px(GAP)).children(tiles).into_any_element());
        }
        // After the rows, so `focused_item` counts them alone.
        if self.controls.iter().any(|column| !column.is_empty()) {
            rows.push(key_guide::guide(&self.controls, width));
        }
        rows
    }

    /// The cheats, each a row with its switch, then Edit Cheats; with a
    /// hint first if there are none.
    fn cheats(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let rows: Vec<_> = self
            .cheats
            .iter()
            .enumerate()
            .map(|(at, cheat)| {
                self.part(Page::Cheats(at), ("cheat", at), cx)
                    .h(px(ROW_HEIGHT))
                    .px_3()
                    .rounded(px(10.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(rgba(TILE))
                    .hover(|row| row.bg(rgba(TILE_HOVER)))
                    .child(
                        div()
                            .flex_1()
                            .text_color(rgb(TEXT))
                            .line_clamp(1)
                            .child(SharedString::from(cheat.name.clone())),
                    )
                    .child(switch(cheat.enabled).children(probe::part(format!("switch:{at}"))))
                    .children(probe::bordered_part(format!("row:cheat.{at}"), RING))
            })
            .collect();
        let empty = self.cheats.is_empty().then(|| {
            div()
                .py_2()
                .text_xs()
                .text_color(rgb(DIM))
                .child("No cheats for this game yet. Add codes to its cheat file.")
        });
        let edit = self
            .part(Page::Cheats(self.cheats.len()), "edit-cheats", cx)
            .h(px(ROW_HEIGHT))
            .rounded(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(TEXT))
            .bg(rgba(TILE))
            .hover(|row| row.bg(rgba(TILE_HOVER)))
            .child(if self.cheats.is_empty() {
                "Add Cheats…"
            } else {
                "Edit Cheats…"
            })
            .children(probe::bordered_part("button:edit-cheats", RING));
        // The order `focused_item` counts in.
        empty
            .map(IntoElement::into_any_element)
            .into_iter()
            .chain(rows.into_iter().map(IntoElement::into_any_element))
            .chain([edit.into_any_element()])
            .collect()
    }

    /// The question before restarting, then Cancel and Start Over.
    fn restart(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cancel = self
            .part(Page::Restart(false), "cancel", cx)
            .flex_1()
            .h(px(ROW_HEIGHT))
            .rounded(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(TILE_HOVER))
            .hover(|button| button.bg(rgba(with_alpha(TEXT, 0x1F))))
            .text_color(rgb(TEXT))
            .child("Cancel")
            .children(probe::bordered_part("button:cancel", RING));
        let erase = self
            .part(Page::Restart(true), "erase", cx)
            .flex_1()
            .h(px(ROW_HEIGHT))
            .rounded(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(DANGER_FILL))
            .hover(|button| button.opacity(0.9))
            .text_color(rgb(0x00FF_FFFF))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child("Start Over")
            .children(probe::bordered_part("button:erase", RING));
        // The order `focused_item` counts in.
        vec![
            div()
                .text_sm()
                .text_color(rgb(DIM))
                .child("This erases your saved progress in this game and starts it from the very beginning.")
                .into_any_element(),
            div()
                .flex()
                .gap(px(GAP))
                .child(cancel)
                .child(erase)
                .into_any_element(),
        ]
    }
}

impl gpui::Render for GameMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let sheet_width = (f32::from(viewport.width) - 2.0 * MARGIN).min(MAX_WIDTH);
        // Inside the sheet's 1-pixel edge.
        let inner = sheet_width - 2.0 * PADDING - 2.0;
        let (columns, side) = tile_grid(inner);
        self.columns = columns;
        let (subtitle, gap, items) = match self.page {
            Page::Tiles(_) => (None, GAP, self.tiles(columns, side, inner, cx)),
            Page::Cheats(_) => (Some("Cheats"), 6.0, self.cheats(cx)),
            Page::Restart(_) => (Some("Start over?"), 12.0, self.restart(cx)),
        };
        // Only the page scrolls, under its header, when the window is too
        // short for it.
        let body = div()
            .id("game-menu-body")
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(px(gap))
            .children(items);
        let sheet = div()
            .id("game-menu")
            .w(px(sheet_width))
            .max_h(px(f32::from(viewport.height) - 2.0 * MARGIN))
            .p(px(PADDING))
            .rounded(px(22.0))
            .bg(rgba(SHEET))
            .border_1()
            .border_color(rgb(LINE))
            .shadow_lg()
            .text_sm()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(self.header(subtitle, cx))
            .child(body)
            .children(probe::bordered_part("sheet", 1.0));
        // A dimmed backdrop that takes the clicks meant for the game.
        div()
            .id("game-menu-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x0000_0080))
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .pb(px(MARGIN))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(MenuEvent::Resume)))
            .child(
                // Clicks on the sheet itself stay on the sheet.
                div()
                    .id("game-menu-sheet")
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(sheet)
                    .with_animation(
                        "game-menu-slide",
                        Animation::new(Duration::from_millis(280)).with_easing(ease_out_quint()),
                        |sheet, delta| sheet.mt(px((1.0 - delta) * 140.0)).opacity(delta),
                    ),
            )
    }
}

/// An on/off switch.
fn switch(on: bool) -> Div {
    div()
        .w(px(36.0))
        .h(px(22.0))
        .p(px(3.0))
        .rounded_full()
        .flex()
        .when(on, |track| track.justify_end().bg(rgb(ACCENT)))
        .when(!on, |track| {
            track.justify_start().bg(rgba(with_alpha(TEXT, 0x26)))
        })
        .child(div().size(px(16.0)).rounded_full().bg(rgb(0x00FF_FFFF)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_move_along_the_tiles() {
        assert_eq!(
            moved(Page::Tiles(Tile::Resume), "right", 0, 4),
            Page::Tiles(Tile::Cheats)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Resume), "a", 0, 4),
            Page::Tiles(Tile::Resume)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Library), "d", 0, 4),
            Page::Tiles(Tile::Library)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Cheats), "down", 0, 4),
            Page::Tiles(Tile::Cheats)
        );
    }

    #[test]
    fn two_rows_of_tiles_move_up_and_down_too() {
        // Resume  Cheats
        // Restart Library
        assert_eq!(
            moved(Page::Tiles(Tile::Cheats), "down", 0, 2),
            Page::Tiles(Tile::Library)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Restart), "w", 0, 2),
            Page::Tiles(Tile::Resume)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Resume), "up", 0, 2),
            Page::Tiles(Tile::Resume)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Library), "s", 0, 2),
            Page::Tiles(Tile::Library)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Cheats), "right", 0, 2),
            Page::Tiles(Tile::Restart)
        );
    }

    #[test]
    fn tiles_go_two_to_a_row_when_four_are_too_narrow() {
        let (columns, side) = tile_grid(390.0);
        assert_eq!(columns, 4);
        assert!((side * 4.0 + GAP * 3.0 - 390.0).abs() < 0.01);
        // The narrowest window: four would be about 43 pixels each.
        let (columns, side) = tile_grid(198.0);
        assert_eq!(columns, 2);
        assert!(side >= MIN_TILE);
        assert_eq!(tile_grid(4.0 * MIN_TILE + 3.0 * GAP).0, 4);
    }

    #[test]
    fn the_cheats_page_ends_with_edit_cheats_and_goes_back_left() {
        assert_eq!(moved(Page::Cheats(0), "down", 2, 4), Page::Cheats(1));
        assert_eq!(moved(Page::Cheats(1), "s", 2, 4), Page::Cheats(2));
        assert_eq!(moved(Page::Cheats(2), "down", 2, 4), Page::Cheats(2));
        assert_eq!(moved(Page::Cheats(0), "up", 2, 4), Page::Cheats(0));
        assert_eq!(
            moved(Page::Cheats(1), "left", 2, 4),
            Page::Tiles(Tile::Cheats)
        );
    }

    #[test]
    fn restarting_starts_on_cancel() {
        assert_eq!(
            moved(Page::Restart(false), "right", 0, 4),
            Page::Restart(true)
        );
        assert_eq!(
            moved(Page::Restart(true), "left", 0, 4),
            Page::Restart(false)
        );
    }

    #[test]
    fn the_item_in_view_is_what_the_keyboard_is_on() {
        assert_eq!(focused_item(Page::Tiles(Tile::Library), 0, 4), 0);
        assert_eq!(focused_item(Page::Tiles(Tile::Library), 0, 2), 1);
        assert_eq!(focused_item(Page::Tiles(Tile::Cheats), 0, 2), 0);
        assert_eq!(focused_item(Page::Cheats(3), 5, 4), 3);
        // Edit Cheats comes after the hint that there are none.
        assert_eq!(focused_item(Page::Cheats(0), 0, 4), 1);
        assert_eq!(focused_item(Page::Restart(true), 0, 4), 1);
    }
}
