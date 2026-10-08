//! The game menu: a sheet that slides up over the paused game when the
//! player presses Escape. Four tiles: resume, cheats, restart and back to
//! the library. Cheats and restarting open pages of their own in the
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
    SharedString, Stateful, Window, div, ease_out_quint, prelude::*, px, rgb, rgba,
};

use crate::theme::{ACCENT, LINE, MUTED as DIM, ON_ACCENT, TEXT, with_alpha};

/// The sheet: graphite, a touch see-through.
const SHEET: u32 = with_alpha(0x0013_1417, 0xF2);
const TILE: u32 = 0xFFFF_FF0D;
const TILE_HOVER: u32 = 0xFFFF_FF1A;
const DANGER: u32 = 0x00FF_6B6B;
const DANGER_FILL: u32 = 0x00E0_4848;
/// The ring around whatever the keyboard is on.
const FOCUS: u32 = 0xFFFF_FFCC;

/// Widest the sheet gets, in logical pixels.
const MAX_WIDTH: f32 = 420.0;
/// Room around and inside the sheet, and between tiles.
const MARGIN: f32 = 14.0;
const PADDING: f32 = 14.0;
const GAP: f32 = 8.0;

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
    fn glyph(self) -> &'static str {
        match self {
            Self::Resume => "▶",
            Self::Cheats => "✦",
            Self::Restart => "↺",
            Self::Library => "▦",
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
    /// Asking before restarting; whether Erase rather than Cancel is
    /// chosen.
    Restart(bool),
}

/// Where pressing `key` on `page` goes, with `cheats` cheats.
fn moved(page: Page, key: &str, cheats: usize) -> Page {
    match (page, key) {
        (Page::Tiles(tile), "left" | "a" | "right" | "d") => {
            let at = TILES.iter().position(|&t| t == tile).unwrap_or(0);
            let at = if matches!(key, "left" | "a") {
                at.saturating_sub(1)
            } else {
                (at + 1).min(TILES.len() - 1)
            };
            Page::Tiles(TILES[at])
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

/// The game menu's view.
pub struct GameMenu {
    /// The game's name, over the menu.
    title: SharedString,
    cheats: Vec<CheatRow>,
    page: Page,
}

impl EventEmitter<MenuEvent> for GameMenu {}

impl GameMenu {
    /// A menu for the game called `title`, with its `cheats`.
    pub fn new(title: impl Into<SharedString>, cheats: Vec<CheatRow>) -> Self {
        Self {
            title: title.into(),
            cheats,
            page: Page::Tiles(Tile::Resume),
        }
    }

    /// Shows `cheats` in place of the list shown.
    pub fn set_cheats(&mut self, cheats: Vec<CheatRow>, cx: &mut Context<Self>) {
        self.cheats = cheats;
        if let Page::Cheats(at) = self.page {
            self.page = Page::Cheats(at.min(self.cheats.len()));
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
            key => self.page = moved(self.page, key, self.cheats.len()),
        }
        cx.notify();
    }

    /// Goes back a page, or closes the menu from the tiles.
    fn back(&mut self, cx: &mut Context<Self>) {
        self.page = match self.page {
            Page::Tiles(_) => {
                cx.emit(MenuEvent::Resume);
                return;
            }
            Page::Cheats(_) => Page::Tiles(Tile::Cheats),
            Page::Restart(_) => Page::Tiles(Tile::Restart),
        };
    }

    fn choose(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        match page {
            Page::Tiles(Tile::Resume) => cx.emit(MenuEvent::Resume),
            Page::Tiles(Tile::Cheats) => self.page = Page::Cheats(0),
            Page::Tiles(Tile::Restart) => self.page = Page::Restart(false),
            Page::Tiles(Tile::Library) => cx.emit(MenuEvent::ExitToLibrary),
            Page::Cheats(at) if at < self.cheats.len() => cx.emit(MenuEvent::ToggleCheat(at)),
            Page::Cheats(_) => cx.emit(MenuEvent::EditCheats),
            Page::Restart(true) => cx.emit(MenuEvent::StartOver),
            Page::Restart(false) => self.page = Page::Tiles(Tile::Restart),
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
            .border_2()
            .border_color(rgba(if self.page == page { FOCUS } else { 0 }))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.choose(page, cx)))
    }

    fn header(&self, subtitle: Option<&'static str>, cx: &mut Context<Self>) -> AnyElement {
        let back = subtitle.map(|_| {
            div()
                .id("back")
                .size(px(26.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(rgb(TEXT))
                .hover(|button| button.bg(rgba(TILE_HOVER)))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.back(cx);
                    cx.notify();
                }))
                .child("‹")
        });
        let title = div()
            .flex_1()
            .text_color(rgb(TEXT))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .line_clamp(1)
            .child(subtitle.map_or_else(|| self.title.clone(), SharedString::from));
        div()
            .flex()
            .items_center()
            .gap_2()
            .children(back)
            .child(title)
            .into_any_element()
    }

    fn tiles(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let side = (width - GAP * (TILES.len() - 1) as f32) / TILES.len() as f32;
        let tiles = TILES.map(|tile| {
            let resume = tile == Tile::Resume;
            self.part(Page::Tiles(tile), tile.label(), cx)
                .w(px(side))
                .h(px(side.min(84.0)))
                .rounded(px(14.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .bg(rgba(if resume {
                    with_alpha(ACCENT, 0xFF)
                } else {
                    TILE
                }))
                .when(resume, |tile| tile.hover(|tile| tile.opacity(0.9)))
                .when(!resume, |tile| tile.hover(|tile| tile.bg(rgba(TILE_HOVER))))
                .child(
                    div()
                        .text_xl()
                        .text_color(rgb(if tile == Tile::Restart { DANGER } else { TEXT }))
                        .child(tile.glyph()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(if resume { ON_ACCENT } else { DIM }))
                        .child(tile.label()),
                )
        });
        div().flex().gap(px(GAP)).children(tiles).into_any_element()
    }

    fn cheats(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<_> = self
            .cheats
            .iter()
            .enumerate()
            .map(|(at, cheat)| {
                self.part(Page::Cheats(at), ("cheat", at), cx)
                    .h(px(40.0))
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
                    .child(switch(cheat.enabled))
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
            .h(px(36.0))
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
            });
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .children(rows)
            .children(empty)
            .child(edit)
            .into_any_element()
    }

    fn restart(&self, cx: &mut Context<Self>) -> AnyElement {
        let cancel = self
            .part(Page::Restart(false), "cancel", cx)
            .flex_1()
            .h(px(40.0))
            .rounded(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(TILE_HOVER))
            .hover(|button| button.bg(rgba(0xFFFF_FF26)))
            .text_color(rgb(TEXT))
            .child("Cancel");
        let erase = self
            .part(Page::Restart(true), "erase", cx)
            .flex_1()
            .h(px(40.0))
            .rounded(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(DANGER_FILL))
            .hover(|button| button.opacity(0.9))
            .text_color(rgb(0x00FF_FFFF))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child("Erase & Restart");
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(DIM))
                    .child("This erases your saved progress in this game and starts it from the very beginning."),
            )
            .child(div().flex().gap(px(GAP)).child(cancel).child(erase))
            .into_any_element()
    }
}

impl gpui::Render for GameMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let sheet_width = (f32::from(viewport.width) - 2.0 * MARGIN).min(MAX_WIDTH);
        let inner = sheet_width - 2.0 * PADDING - 2.0;
        let (subtitle, body) = match self.page {
            Page::Tiles(_) => (None, self.tiles(inner, cx)),
            Page::Cheats(_) => (Some("Cheats"), self.cheats(cx)),
            Page::Restart(_) => (Some("Start over?"), self.restart(cx)),
        };
        let sheet = div()
            .id("game-menu")
            .w(px(sheet_width))
            .max_h(px(f32::from(viewport.height) - 2.0 * MARGIN))
            .overflow_y_scroll()
            .px(px(PADDING))
            .pt(px(8.0))
            .pb(px(PADDING))
            .rounded(px(22.0))
            .bg(rgba(SHEET))
            .border_1()
            .border_color(rgb(LINE))
            .shadow_lg()
            .text_sm()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                // The grabber, as on a sheet on a phone.
                div().w_full().flex().justify_center().child(
                    div()
                        .w(px(36.0))
                        .h(px(4.0))
                        .rounded_full()
                        .bg(rgba(0xFFFF_FF2E)),
                ),
            )
            .child(self.header(subtitle, cx))
            .child(body);
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
fn switch(on: bool) -> impl IntoElement {
    div()
        .w(px(36.0))
        .h(px(22.0))
        .p(px(3.0))
        .rounded_full()
        .flex()
        .when(on, |track| track.justify_end().bg(rgb(ACCENT)))
        .when(!on, |track| track.justify_start().bg(rgba(0xFFFF_FF26)))
        .child(div().size(px(16.0)).rounded_full().bg(rgb(0x00FF_FFFF)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_move_along_the_tiles() {
        assert_eq!(
            moved(Page::Tiles(Tile::Resume), "right", 0),
            Page::Tiles(Tile::Cheats)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Resume), "a", 0),
            Page::Tiles(Tile::Resume)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Library), "d", 0),
            Page::Tiles(Tile::Library)
        );
        assert_eq!(
            moved(Page::Tiles(Tile::Cheats), "down", 0),
            Page::Tiles(Tile::Cheats)
        );
    }

    #[test]
    fn the_cheats_page_ends_with_edit_cheats_and_goes_back_left() {
        assert_eq!(moved(Page::Cheats(0), "down", 2), Page::Cheats(1));
        assert_eq!(moved(Page::Cheats(1), "s", 2), Page::Cheats(2));
        assert_eq!(moved(Page::Cheats(2), "down", 2), Page::Cheats(2));
        assert_eq!(moved(Page::Cheats(0), "up", 2), Page::Cheats(0));
        assert_eq!(moved(Page::Cheats(1), "left", 2), Page::Tiles(Tile::Cheats));
    }

    #[test]
    fn restarting_starts_on_cancel() {
        assert_eq!(moved(Page::Restart(false), "right", 0), Page::Restart(true));
        assert_eq!(moved(Page::Restart(true), "left", 0), Page::Restart(false));
    }
}
