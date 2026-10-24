//! The library: what the main window shows while no game runs. Each game
//! the user has added or played, as box art over its title, narrowed to
//! what the user types.
//!
//! Keyboard first: typing searches, the arrow keys choose a game, Return
//! plays it and ⌘⌫ takes the game out of the library.
//! The view only shows and edits the library; it asks the main window,
//! through [`LibraryEvent`], to play a game, add games or open settings.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use emu_cores::rpg_maker;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, EventEmitter, KeyDownEvent, ObjectFit,
    ScrollHandle, SharedString, Task, Window, div, img, linear_color_stop, linear_gradient,
    prelude::*, px, rgb, rgba,
};

use crate::app::TOP_BAR;
use crate::app_icon;
use crate::archive;
use crate::boxart;
use crate::library::{self, Game, Library};
use crate::rom_info;
use crate::romhack::{self, MakeError};

use crate::theme::{ACCENT as CARET, BACKGROUND as TOP, MUTED as DIM, PANEL, TEXT};

/// The background lightens a touch towards the foot of the window.
const FOOT: u32 = 0x0016_181B;
/// Behind the chosen game.
const CHOSEN: u32 = 0xFFFF_FF1A;

/// Room left of the header for the close, minimise and zoom buttons and
/// a gap after them.
const TRAFFIC_LIGHTS: f32 = 96.0;
/// Space around the grid and between its games, in logical pixels.
const PADDING: f32 = 20.0;
const GAP: f32 = 12.0;
/// The narrowest a game's tile gets before the grid drops a column.
const MIN_TILE: f32 = 116.0;
/// Height of a line of a game's title.
const TITLE_LINE: f32 = 16.0;

/// What the library asks of the main window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryEvent {
    /// Play the game at this path.
    Play(PathBuf),
    /// Ask the user for games to add.
    AddGames,
    /// Open the settings window.
    OpenSettings,
    /// Tell the user something went wrong.
    Notice(String),
}

/// The library's view.
pub struct LibraryView {
    library: Library,
    /// Where the library is saved, if there is a config folder.
    path: Option<PathBuf>,
    /// Where box art is kept, if there is a cache folder.
    cache: Option<PathBuf>,
    /// Each game's box art or icon on disk, by ROM checksum.
    covers: HashMap<u32, PathBuf>,
    query: String,
    /// Whether the search field has the caret: clicked or typed into,
    /// until Escape leaves it.
    searching: bool,
    /// The chosen game, by its ROM's path, so it stays chosen as titles
    /// are looked up and the games sort anew. The first shown if none is.
    selected: Option<PathBuf>,
    /// How many games fit across, as last drawn.
    columns: usize,
    scroll: ScrollHandle,
    looking_up: Option<Task<()>>,
    /// Game databases parsed since launch.
    databases: Arc<boxart::Databases>,
    /// Games looked up since launch, found or not, so a failed lookup is
    /// not retried until next launch.
    tried: HashSet<PathBuf>,
}

impl EventEmitter<LibraryEvent> for LibraryView {}

impl LibraryView {
    /// A view of `library`, saved to `path`, which starts finding box art
    /// for games that have none.
    pub fn new(library: Library, path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            library,
            path,
            cache: boxart::cache_dir(),
            covers: HashMap::new(),
            query: String::new(),
            searching: false,
            selected: None,
            columns: 1,
            scroll: ScrollHandle::new(),
            looking_up: None,
            databases: Arc::default(),
            tried: HashSet::new(),
        };
        view.refresh_covers();
        view.look_up(cx);
        view
    }

    /// Adds the games at `paths`, reading them off the UI thread: those in
    /// any archives among them once unpacked, and those that any ROM-hack
    /// patches among them make from games the library has. Games already
    /// in the library are left as they are, so playing one does not read
    /// its ROM again.
    pub fn add(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut seen = HashSet::new();
        let paths: Vec<PathBuf> = paths
            .iter()
            .map(|path| library::absolute(path))
            .filter(|path| !self.library.contains(path) && seen.insert(path.clone()))
            .collect();
        if paths.is_empty() {
            return;
        }
        let cache = self.cache.clone();
        let known = self.checksums();
        let read = cx.background_executor().spawn(async move {
            paths
                .iter()
                .flat_map(|path| read_games(path, cache.as_deref(), &known))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let games = read.await;
            this.update(cx, |this, cx| {
                for game in games {
                    match game {
                        Ok(game) => this.library.add(game),
                        Err(error) => cx.emit(LibraryEvent::Notice(error)),
                    }
                }
                this.save(cx);
                this.refresh_covers();
                this.look_up(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Each game's ROM checksum and path, for finding the game a patch is
    /// for.
    pub fn checksums(&self) -> Vec<(u32, PathBuf)> {
        let games = self.library.games.iter();
        games.map(|game| (game.crc32, game.path.clone())).collect()
    }

    /// The title the library shows for the game at `path`, if it has it.
    pub fn title_of(&self, path: &Path) -> Option<String> {
        let game = self.library.games.iter().find(|game| game.path == path)?;
        Some(game.title.clone())
    }

    /// Handles a key while the library shows. Whether it was the
    /// library's.
    pub fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        match keystroke.key.as_str() {
            "backspace" if modifiers.platform => self.remove_selected(cx),
            _ if modifiers.platform || modifiers.control => return false,
            // A new search chooses its first match.
            "backspace" => {
                self.query.pop();
                self.selected = None;
            }
            // The first Escape clears the search, the next leaves it.
            "escape" if self.query.is_empty() => self.searching = false,
            "escape" => {
                self.query.clear();
                self.selected = None;
            }
            // Return held down plays the game once, not again on every
            // repeat, which would restart its loading each time.
            "enter" if event.is_held => {}
            "enter" => {
                let shown = self.shown();
                if let Some(game) = shown.get(position(&shown, self.selected.as_deref())) {
                    cx.emit(LibraryEvent::Play(game.path.clone()));
                }
            }
            key @ ("left" | "right" | "up" | "down") => {
                let shown = self.shown();
                let at = position(&shown, self.selected.as_deref());
                let at = moved(at, shown.len(), self.columns, key);
                self.selected = shown.get(at).map(|game| game.path.clone());
                self.scroll.scroll_to_item(at);
            }
            _ => match &keystroke.key_char {
                Some(text) if text.chars().all(|c| !c.is_control()) => {
                    self.query.push_str(text);
                    self.searching = true;
                    self.selected = None;
                }
                _ => return false,
            },
        }
        cx.notify();
        true
    }

    /// The games shown, in order.
    fn shown(&self) -> Vec<&Game> {
        self.library.find(&self.query, None)
    }

    /// Takes the chosen game out of the library, choosing the game that
    /// takes its place.
    fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let shown = self.shown();
        let at = position(&shown, self.selected.as_deref());
        let Some(path) = shown.get(at).map(|game| game.path.clone()) else {
            return;
        };
        self.library.remove(&path);
        let shown = self.shown();
        let at = at.min(shown.len().saturating_sub(1));
        self.selected = shown.get(at).map(|game| game.path.clone());
        self.save(cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(error) = self.library.save(path) {
            let message = format!("Cannot save the library to {}: {error}", path.display());
            cx.emit(LibraryEvent::Notice(message));
        }
    }

    /// Finds each game's box art, or failing that its DS icon, on disk.
    fn refresh_covers(&mut self) {
        let Some(cache) = &self.cache else {
            return;
        };
        self.covers = self
            .library
            .games
            .iter()
            .filter_map(|game| Some((game.crc32, cover_on_disk(cache, game.crc32)?)))
            .collect();
    }

    /// Looks up the games not yet looked up, off the UI thread, unless
    /// that is already under way. Each game's title and box art show as
    /// soon as it is found.
    fn look_up(&mut self, cx: &mut Context<Self>) {
        let Some(cache) = self.cache.clone() else {
            return;
        };
        let pending: Vec<Game> = self
            .library
            .games
            .iter()
            .filter(|game| !game.looked_up && !self.tried.contains(&game.path))
            .cloned()
            .collect();
        if self.looking_up.is_some() || pending.is_empty() {
            return;
        }
        self.tried
            .extend(pending.iter().map(|game| game.path.clone()));
        let databases = Arc::clone(&self.databases);
        let (answers, receiver) = async_channel::unbounded();
        let spawned = thread::Builder::new()
            .name("box-art".to_owned())
            .spawn(move || {
                boxart::look_up_all(&cache, &pending, &databases, |game, name| {
                    answers
                        .send_blocking((game.path.clone(), game.crc32, name))
                        .ok();
                });
            });
        if let Err(error) = spawned {
            cx.emit(LibraryEvent::Notice(format!("No box art: {error}")));
            return;
        }
        self.looking_up = Some(cx.spawn(async move |this, cx| {
            let mut failure = None;
            while let Ok((path, crc32, name)) = receiver.recv().await {
                let answered = this.update(cx, |this, cx| this.answer(&path, crc32, name, cx));
                match answered {
                    Ok(error) => failure = failure.or(error),
                    Err(_) => return,
                }
            }
            this.update(cx, |this, cx| {
                this.looking_up = None;
                if let Some(error) = failure {
                    cx.emit(LibraryEvent::Notice(format!("No box art: {error}")));
                }
                this.save(cx);
                // Games added meanwhile.
                this.look_up(cx);
            })
            .ok();
        }));
    }

    /// Records the game database's answer for the game at `path`: its
    /// name, or why it could not be looked up, which this returns.
    fn answer(
        &mut self,
        path: &Path,
        crc32: u32,
        name: anyhow::Result<Option<String>>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let name = match name {
            Ok(name) => name,
            Err(error) => return Some(format!("{error:#}")),
        };
        let game = self
            .library
            .games
            .iter_mut()
            .find(|game| game.path == path && game.crc32 == crc32)?;
        game.found(name);
        if let Some(cover) = self
            .cache
            .as_deref()
            .and_then(|cache| cover_on_disk(cache, crc32))
        {
            self.covers.insert(crc32, cover);
        }
        cx.notify();
        None
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h(px(TOP_BAR))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(TRAFFIC_LIGHTS))
            .pr(px(PADDING))
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_color(rgb(TEXT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Library"),
            )
            .child(div().flex_1().flex().justify_end().gap_1().children([
                icon_button(
                    "settings",
                    "⚙",
                    cx.listener(|_, _, _, cx| {
                        cx.emit(LibraryEvent::OpenSettings);
                    }),
                ),
                icon_button(
                    "add",
                    "+",
                    cx.listener(|_, _, _, cx| {
                        cx.emit(LibraryEvent::AddGames);
                    }),
                ),
            ]))
            .into_any_element()
    }

    fn search_field(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = if self.query.is_empty() {
            div().text_color(rgb(DIM)).child("Search: just type")
        } else {
            div()
                .text_color(rgb(TEXT))
                .child(SharedString::from(self.query.clone()))
        };
        // The caret goes before the placeholder, as in a Mac search field.
        let caret = self.searching.then(|| {
            div().w(px(1.5)).h(px(16.0)).bg(rgb(CARET)).with_animation(
                "caret",
                Animation::new(Duration::from_millis(1060)).repeat(),
                |caret, delta| caret.opacity(if delta < 0.5 { 1.0 } else { 0.0 }),
            )
        });
        let (caret_before, caret_after) = if self.query.is_empty() {
            (caret, None)
        } else {
            (None, caret)
        };
        div()
            .id("search")
            .mx(px(PADDING))
            .mb(px(PADDING / 2.0))
            .h(px(30.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .rounded_lg()
            .bg(rgba(0xFFFF_FF12))
            .border_1()
            .border_color(rgba(if self.searching {
                CARET << 8 | 0xAA
            } else {
                0x0000_0000
            }))
            .cursor_text()
            .text_sm()
            .on_click(cx.listener(|this, _, _, cx| {
                this.searching = true;
                cx.notify();
            }))
            .child(div().text_color(rgb(DIM)).child("⌕"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(1.0))
                    .children(caret_before)
                    .child(text)
                    .children(caret_after),
            )
            .into_any_element()
    }

    fn tile(&self, index: usize, game: &Game, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.selected.as_ref() == Some(&game.path);
        let path = game.path.clone();
        let art = match self.covers.get(&game.crc32) {
            Some(cover) => img(cover.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .bg(rgb(PANEL))
                .text_2xl()
                .text_color(rgb(DIM))
                .child(game.platform.label())
                .into_any_element(),
        };
        div()
            .id(("game", index))
            .w(px(width))
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .rounded_lg()
            .cursor_pointer()
            .when(chosen, |tile| tile.bg(rgba(CHOSEN)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(path.clone());
                cx.emit(LibraryEvent::Play(path.clone()));
            }))
            .child(
                div()
                    .w_full()
                    .h(px(width - 8.0))
                    .flex()
                    .items_end()
                    .child(art),
            )
            .child(
                div()
                    .h(px(2.0 * TITLE_LINE))
                    .text_xs()
                    .line_height(px(TITLE_LINE))
                    .text_center()
                    .line_clamp(2)
                    .text_color(rgb(if chosen { 0x00FF_FFFF } else { TEXT }))
                    .child(SharedString::from(game.title.clone())),
            )
            .into_any_element()
    }

    fn empty(&self) -> AnyElement {
        let (title, hint) = if self.library.games.is_empty() {
            (
                "No games yet".to_owned(),
                "Add some with + or ⌘⇧A, or drop a .gb, .gbc, .gba or .nds file, a .zip, .7z or .rar of one, a ROM-hack patch, or an RPG Maker game's folder, here.".to_owned(),
            )
        } else {
            (
                "No games match".to_owned(),
                "Press Escape to clear the search.".to_owned(),
            )
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().text_xl().text_color(rgb(TEXT)).child(title))
            .child(div().text_sm().text_color(rgb(DIM)).child(hint))
            .into_any_element()
    }
}

impl gpui::Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        let (columns, tile) = grid(width - 2.0 * PADDING);
        self.columns = columns;
        let shown = self.shown();
        let at = position(&shown, self.selected.as_deref());
        let selected = shown.get(at).map(|game| game.path.clone());
        let games: Vec<Game> = shown.into_iter().cloned().collect();
        // The game drawn as chosen is the one chosen, wherever titles
        // looked up later sort it.
        self.selected = selected;

        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(linear_gradient(
                180.0,
                linear_color_stop(rgb(TOP), 0.3),
                linear_color_stop(rgb(FOOT), 1.0),
            ))
            .child(self.header(cx))
            .child(self.search_field(cx));
        if games.is_empty() {
            return root.child(self.empty());
        }
        let tiles: Vec<AnyElement> = games
            .iter()
            .enumerate()
            .map(|(index, game)| self.tile(index, game, tile, cx))
            .collect();
        root.child(
            div()
                .id("library")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .flex()
                .flex_wrap()
                .content_start()
                .gap(px(GAP))
                .px(px(PADDING))
                .pb(px(PADDING))
                .children(tiles),
        )
    }
}

/// A round button showing `glyph`.
fn icon_button(
    id: &'static str,
    glyph: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .size(px(26.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .text_color(rgb(TEXT))
        .hover(|button| button.bg(rgb(0x0033_3333)))
        .on_click(on_click)
        .child(glyph)
        .into_any_element()
}

/// The box art of the ROM with checksum `crc32` in `cache`, or failing
/// that, or if the art is not a picture, its DS icon.
fn cover_on_disk(cache: &Path, crc32: u32) -> Option<PathBuf> {
    let art = boxart::art_path(cache, crc32);
    let icon = boxart::icon_path(cache, crc32);
    if boxart::is_png(&art) {
        Some(art)
    } else {
        icon.exists().then_some(icon)
    }
}

/// The games at `path`: the ROM or RPG Maker game there, or each ROM in it
/// if it is an archive, unpacked into the games folder; and the game each
/// ROM-hack patch there makes from the game it names among `known`
/// (checksum and path) or the archive's ROMs.
fn read_games(
    path: &Path,
    cache: Option<&Path>,
    known: &[(u32, PathBuf)],
) -> Vec<Result<Game, String>> {
    let files = if archive::is_archive(path) {
        match archive::unpack(path) {
            Ok(files) => files,
            Err(error) => return vec![Err(error)],
        }
    } else {
        vec![path.to_owned()]
    };
    let (patches, roms): (Vec<PathBuf>, Vec<PathBuf>) =
        files.into_iter().partition(|file| romhack::is_patch(file));
    let mut games: Vec<Result<Game, String>> =
        roms.iter().map(|rom| read_game(rom, cache)).collect();
    let mut known = known.to_vec();
    known.extend(
        games
            .iter()
            .flatten()
            .map(|game| (game.crc32, game.path.clone())),
    );
    for patch in patches {
        games.push(match romhack::make(&patch, None, &known) {
            Ok(rom) => read_game(&rom, cache),
            Err(MakeError::NoBase(_)) => Err(format!(
                "To add {}, open it with ⌘O and choose the game it patches",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
            Err(MakeError::Failed(error)) => Err(error),
        });
    }
    games
}

/// The game in the ROM at `path`, or the RPG Maker game there, with its DS
/// icon or its player app's icon saved in `cache`.
fn read_game(path: &Path, cache: Option<&Path>) -> Result<Game, String> {
    if let Some(folder) = rpg_maker::game_folder(path) {
        let game = Game::rpg_maker(&folder, rpg_maker::title(&folder));
        let player = rpg_maker::player_app(&folder);
        if let (Some(cache), Some(player)) = (cache, player) {
            // Its only picture: no game database lists RPG Maker games.
            app_icon::save(&player, &boxart::icon_path(cache, game.crc32)).ok();
        }
        return Ok(game);
    }
    let rom = fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let info = rom_info::read(path, &rom)
        .ok_or_else(|| format!("{} is not a .gb, .gbc, .gba or .nds game", path.display()))?;
    if let (Some(cache), Some(icon)) = (cache, rom_info::nds_icon(&rom)) {
        // The icon is only a fallback for missing box art.
        boxart::save_icon(cache, info.crc32, &icon).ok();
    }
    let mut game = Game::new(path, &info);
    if romhack::is_hack(path) {
        game.made_from_patch();
    }
    Ok(game)
}

/// How many tiles fit across `width` logical pixels, and how wide each is
/// so that they fill it.
fn grid(width: f32) -> (usize, f32) {
    // Whole tiles only; `as` saturates.
    let columns = (((width + GAP) / (MIN_TILE + GAP)).floor() as usize).max(1);
    let tile = (width - GAP * (columns - 1) as f32) / columns as f32;
    (columns, tile.max(MIN_TILE / 2.0))
}

/// Where the game at `selected` is among `shown`, or the first if it is
/// not shown or none is chosen.
fn position(shown: &[&Game], selected: Option<&Path>) -> usize {
    selected
        .and_then(|selected| shown.iter().position(|game| game.path == selected))
        .unwrap_or(0)
}

/// The game chosen after pressing `key` with `selected` of `count` chosen
/// in a grid `columns` wide. Up and down stay put at the top and bottom.
fn moved(selected: usize, count: usize, columns: usize, key: &str) -> usize {
    let last = count.saturating_sub(1);
    match key {
        "left" => selected.saturating_sub(1),
        "right" => (selected + 1).min(last),
        "up" => selected.checked_sub(columns).unwrap_or(selected),
        "down" if selected + columns <= last => selected + columns,
        _ => selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_fills_the_width_with_whole_tiles() {
        let (columns, tile) = grid(450.0);
        assert_eq!(columns, 3);
        assert!((tile * 3.0 + GAP * 2.0 - 450.0).abs() < 0.01);
        assert_eq!(grid(4.0 * MIN_TILE + 3.0 * GAP).0, 4);
        assert_eq!(grid(50.0).0, 1);
    }

    #[test]
    fn arrows_move_through_the_grid() {
        // Seven games, three across:
        // 0 1 2
        // 3 4 5
        // 6
        assert_eq!(moved(4, 7, 3, "left"), 3);
        assert_eq!(moved(0, 7, 3, "left"), 0);
        assert_eq!(moved(6, 7, 3, "right"), 6);
        assert_eq!(moved(4, 7, 3, "up"), 1);
        assert_eq!(moved(1, 7, 3, "up"), 1);
        assert_eq!(moved(3, 7, 3, "down"), 6);
        assert_eq!(moved(4, 7, 3, "down"), 4);
        assert_eq!(moved(0, 0, 3, "right"), 0);
    }

    #[test]
    fn the_chosen_game_stays_chosen_as_titles_sort_anew() {
        let mut library = Library::default();
        for name in ["Advance Wars.gba", "Golden Sun.gba", "Metroid Fusion.gba"] {
            let info = rom_info::RomInfo {
                platform: rom_info::Platform::Gba,
                crc32: 1,
                serial: None,
                header_title: None,
                banner_title: None,
            };
            library.add(Game::new(Path::new(name), &info));
        }
        let chosen = Path::new("Golden Sun.gba");
        assert_eq!(position(&library.find("", None), Some(chosen)), 1);
        // Looked up, Advance Wars turns out to sort last.
        library.games[0].found(Some("Wars, The (USA)".to_owned()));
        assert_eq!(position(&library.find("", None), Some(chosen)), 0);
        assert_eq!(position(&library.find("", None), None), 0);
        let gone = Path::new("Removed.gba");
        assert_eq!(position(&library.find("", None), Some(gone)), 0);
        assert_eq!(position(&[], Some(chosen)), 0);
    }
}
