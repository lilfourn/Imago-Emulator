//! The library: what the main window shows while no game runs. The game
//! last played, large, to pick up where it was left; then each game the
//! user has added or played, as its box art over its title, narrowed to
//! one system and to what the user types.
//!
//! Every game has a picture: its box art or title screen if libretro has
//! one (see `boxart.rs`), or else the screen where it was left off, or a
//! plate in its system's colour with its icon or title.
//!
//! Keyboard first: typing searches, the arrow keys choose a game, Tab and
//! ⇧Tab change system, Return plays the chosen game, ⌘⌫ takes it out of
//! the library and ⌘Z puts it back. A click chooses a game and a double
//! click plays it. A right click, or ⌘I, opens the game's menu: how long
//! it has been played, its saves and size, and Play, Show in Finder and
//! Remove from Library.
//! The view only shows and edits the library; it asks the main window,
//! through [`LibraryEvent`], to play a game, add games or open settings.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use emu_cores::rpg_maker;
use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, Context, Entity, EventEmitter, ImageFormat,
    KeyDownEvent, LineFragment, MouseButton, MouseDownEvent, ObjectFit, Pixels, Point,
    ScrollHandle, SharedString, Stateful, Subscription, Task, Window, anchored, canvas, deferred,
    div, img, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, rgba,
};

use crate::app::TOP_BAR;
use crate::app_icon;
use crate::archive;
use crate::boxart;
use crate::icons;
use crate::leftovers;
use crate::library::{self, Game, Library};
use crate::persist;
use crate::probe;
use crate::randomizer;
use crate::rom_info::{self, Platform};
use crate::romhack::{self, MakeError};
use crate::slots::STATE_SLOTS;
use crate::store_view::{StoreEvent, StoreView, platform_name, pulse};
use crate::ui::{self, Section};

use crate::theme::{
    ACCENT as CARET, BACKGROUND as TOP, CHOSEN, DANGER, HOVER, INSET, LINE, MUTED as DIM,
    ON_ACCENT, PANEL, TEXT, tint, with_alpha,
};

/// The background lightens a touch towards the foot of the window.
const FOOT: u32 = 0x0016_181B;

/// Room left of the header for the close, minimise and zoom buttons and
/// a gap after them.
pub const TRAFFIC_LIGHTS: f32 = 96.0;
/// Room right of the header's last button, which puts its icon's artwork
/// as far from the window's right side as the close button is from the
/// left.
pub const HEADER_END: f32 = 8.0;
/// Space between the games, in logical pixels.
const GAP: f32 = 12.0;
/// Room around a game's picture and words, inside its tile, as in the
/// store.
const TILE_PADDING: f32 = 6.0;
/// The narrowest a game's tile gets before the grid drops a column.
const MIN_TILE: f32 = 120.0;
/// Line heights of a tile's title and the line under it.
const TITLE_LINE: f32 = 18.0;
const DETAIL_LINE: f32 = 16.0;
/// Corners of a picture, and of a plate standing in for one.
const ART_RADIUS: f32 = 8.0;
/// The tallest and shortest the last-played game's banner gets, and its
/// height for its width between.
const HERO_MAX: f32 = 232.0;
const HERO_MIN: f32 = 156.0;
const HERO_SHAPE: f32 = 0.44;
const HERO_RADIUS: f32 = 14.0;
/// The shortest window that shows the last-played game: one that has room
/// for games under it.
const ROOM_FOR_HERO: f32 = 3.0 * HERO_MIN;
/// The least room its words get beside its box art; with less, they get
/// the whole width.
const HERO_WORDS: f32 = 220.0;
/// Side of the play button over a chosen game's picture.
const PLAY_BUTTON: f32 = 30.0;
/// Height of the search field, and room around its magnifier. The
/// magnifier's artwork sits about as far inside its 14 pixels as the field's
/// edge is thick, so it is as far from the field's side as from its top and
/// bottom.
const SEARCH_HEIGHT: f32 = 32.0;
const SEARCH_PADDING: f32 = 8.0;
/// Size of the empty library's hint, and the widest it gets.
const HINT_SIZE: f32 = 14.0;
const HINT_WIDTH: f32 = 320.0;
/// A game's menu: its width, its edge, the room inside that and its
/// corners; its rows' corners share their centre.
const MENU_WIDTH: f32 = 264.0;
const MENU_BORDER: f32 = 1.0;
const MENU_PADDING: f32 = 6.0;
const MENU_RADIUS: f32 = 12.0;
const ROW_RADIUS: f32 = MENU_RADIUS - MENU_BORDER - MENU_PADDING;
/// The menu's surface, a step above the panels it floats over.
const MENU_BACKGROUND: u32 = 0x001E_2024;
/// Height of an action in the menu, whose icon is as far in from the
/// row's side as from its top and bottom: as far in as everything else in
/// the menu.
const ACTION_HEIGHT: f32 = 32.0;
const MENU_INSET: f32 = (ACTION_HEIGHT - icons::SIZE) / 2.0;
/// Height of a line of the game's facts, and side of its picture.
const FACT_HEIGHT: f32 = 22.0;
const THUMBNAIL: f32 = 44.0;
/// How long a game is being added before its tile shows, so one added at
/// once does not flash up; and how often the tiles are drawn anew after,
/// for their progress.
const ADDING_SHOWS_AFTER: Duration = Duration::from_millis(250);
const ADDING_REDRAW: Duration = Duration::from_millis(100);
/// Height of the progress bar on the tile of a game being added, and the
/// share of it the bar's moving piece fills while how far it has got is
/// not known.
const PROGRESS_BAR: f32 = 4.0;
const PROGRESS_PIECE: f32 = 0.3;

/// What the library asks of the main window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryEvent {
    /// Play the game at this path.
    Play(PathBuf),
    /// Ask the user for games to add.
    AddGames,
    /// Open the settings window.
    OpenSettings,
    /// Open the randomize window with the game at this path chosen.
    Randomize(PathBuf),
    /// Tell the user something went wrong.
    Notice(String),
}

/// What the library has to show a game by, besides its title.
#[derive(Debug, Clone, Default)]
struct Pictures {
    /// Its box art, or title screen, on disk.
    art: Option<PathBuf>,
    /// Its DS icon, or its RPG Maker player's icon, on disk.
    icon: Option<PathBuf>,
    /// The screen where it was left off, read in: GPUI keeps a picture
    /// loaded from a path for good, and this one changes each time the
    /// game is played.
    left_at: Option<Arc<gpui::Image>>,
    /// When it was last played.
    played: Option<SystemTime>,
}

/// A game's menu, opened by a right click on it: what the library knows
/// of the game, and what can be done with it.
struct GameMenu {
    /// The game's ROM file, or folder.
    path: PathBuf,
    /// Where it was opened, in the window.
    at: Point<Pixels>,
    /// What the game's files say, once read.
    facts: Option<Facts>,
    _reading: Task<()>,
}

/// What a game's files say of it, for its menu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Facts {
    /// Whether its ROM file, or folder, is still there.
    found: bool,
    /// How much room it takes.
    size: u64,
    /// When its battery save was last written, if it has one.
    saved: Option<SystemTime>,
    /// How many of its save-state slots hold a state.
    states: usize,
    /// Whether the randomizer can make a randomized copy of it.
    randomizable: bool,
}

/// A game being added: unpacked from an archive, made from a patch or
/// read from its ROM. It shows as a tile before the games, with how far it
/// has got, until it is in the library.
struct Adding {
    /// What it is added from: an archive, a patch, a ROM or an RPG Maker
    /// game's folder.
    path: PathBuf,
    /// What its tile is titled: the name of that file, or folder.
    name: String,
    progress: Arc<archive::Progress>,
    since: Instant,
}

impl Adding {
    fn new(path: &Path, progress: Arc<archive::Progress>) -> Self {
        let name = if path.is_dir() {
            path.file_name()
        } else {
            path.file_stem()
        };
        Self {
            path: path.to_owned(),
            name: name
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            progress,
            since: Instant::now(),
        }
    }

    /// How far it has got, from 0 to 1, while that is known: while its
    /// archive unpacks.
    fn fraction(&self) -> Option<f32> {
        let unpacking = archive::is_archive(&self.path);
        unpacking
            .then(|| self.progress.fraction())
            .flatten()
            .filter(|&fraction| fraction < 1.0)
    }

    /// What is being done: `"Unpacking · 42%"`, `"Patching…"`, `"Adding…"`.
    fn stage(&self) -> String {
        if let Some(fraction) = self.fraction() {
            // Rounded down, so it never says 100% before it is done.
            let percent = (fraction * 100.0) as u32;
            format!("Unpacking · {percent}%")
        } else if archive::is_archive(&self.path) && self.progress.fraction().is_none() {
            "Unpacking…".to_owned()
        } else if romhack::is_patch(&self.path) {
            "Patching…".to_owned()
        } else {
            "Adding…".to_owned()
        }
    }

    /// What is being done to it, with `more` games being added after it:
    /// `"Unpacking Game · 42% · 1 more"`, `"Adding Game…"`.
    fn status(&self, more: usize) -> String {
        let (stage, name) = (self.stage(), &self.name);
        let status = match stage.strip_prefix("Unpacking") {
            Some(rest) => format!("Unpacking {name}{rest}"),
            None => format!("{} {name}…", stage.trim_end_matches('…')),
        };
        match more {
            0 => status,
            more => format!("{status} · {more} more"),
        }
    }
}

/// The library's view.
pub struct LibraryView {
    library: Library,
    /// Where the library is saved, if there is a config folder.
    path: Option<PathBuf>,
    /// Where box art is kept, if there is a cache folder.
    cache: Option<PathBuf>,
    /// Each game's pictures and when it was played, by its path.
    pictures: HashMap<PathBuf, Pictures>,
    /// While [`Self::refresh`] reads them.
    refreshing: Option<Task<()>>,
    /// The system shown, `None` for all of them.
    platform: Option<Platform>,
    query: String,
    /// Whether the search field has the caret: clicked or typed into,
    /// until Escape leaves it.
    searching: bool,
    /// The chosen game, by its ROM's path, so it stays chosen as titles
    /// are looked up and the games sort anew. If none is, the game last
    /// played, or the first shown (see [`Self::chosen_at`]).
    selected: Option<PathBuf>,
    /// How many games fit across, as last drawn.
    columns: usize,
    scroll: ScrollHandle,
    /// How many of what scrolls come before the grid's first row, as last
    /// drawn: the last-played game, the system filter and the grid's title.
    lead: usize,
    looking_up: Option<Task<()>>,
    /// Game databases parsed since launch.
    databases: Arc<boxart::Databases>,
    /// Games looked up since launch, found or not, so a failed lookup is
    /// not retried until next launch.
    tried: HashSet<PathBuf>,
    /// The store, once opened: kept, with what it has fetched, while the
    /// library shows.
    store: Option<(Entity<StoreView>, Subscription)>,
    /// Whether the store shows in the library's place.
    store_open: bool,
    /// The game menu, while it is open.
    menu: Option<GameMenu>,
    /// The game last taken out of the library and where it was, for ⌘Z
    /// to put back.
    removed: Option<(usize, Game)>,
    /// Where the chosen game's tile was last drawn, if it was: where ⌘I
    /// opens its menu.
    chosen_tile: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The games being added, oldest first, by a key for
    /// [`Self::added`].
    adding: Vec<(usize, Adding)>,
    next_adding: usize,
    /// Draws the library anew while games are being added.
    adding_redraws: Option<Task<()>>,
    /// How many tiles of games being added came before the games, as last
    /// drawn.
    adding_shown: usize,
    /// Removes what adding games has under way when the app quits.
    _quit: Subscription,
}

impl EventEmitter<LibraryEvent> for LibraryView {}

impl LibraryView {
    /// A view of `library`, saved to `path`, which starts finding box art
    /// for games that have none, and removes what adding games left behind
    /// when an app quit or crashed during it.
    pub fn new(library: Library, path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let folders: Vec<PathBuf> = [archive::games_dir(), romhack::hacks_dir()]
            .into_iter()
            .flatten()
            .collect();
        cx.background_executor()
            .spawn(async move {
                let freed = leftovers::remove_left_behind(&folders);
                if freed > 0 {
                    eprintln!(
                        "ds-emulator: removed {} MB that unfinished unpacking left behind",
                        freed.div_ceil(1 << 20)
                    );
                }
            })
            .detach();
        let mut view = Self {
            library,
            path,
            cache: boxart::cache_dir(),
            pictures: HashMap::new(),
            refreshing: None,
            platform: None,
            query: String::new(),
            searching: false,
            selected: None,
            columns: 1,
            scroll: ScrollHandle::new(),
            lead: 0,
            looking_up: None,
            databases: Arc::default(),
            tried: HashSet::new(),
            store: None,
            store_open: false,
            menu: None,
            removed: None,
            chosen_tile: Rc::default(),
            adding: Vec::new(),
            next_adding: 0,
            adding_redraws: None,
            adding_shown: 0,
            _quit: cx.on_app_quit(|_, _| {
                leftovers::remove_own();
                async {}
            }),
        };
        // Box art and icons, in the cache, are read at once, so the games
        // don't open on plates that their pictures replace a moment later.
        // Where each was left, beside its ROM on what may be a slow disk,
        // comes after.
        let cache = view.cache.clone();
        view.pictures = view
            .library
            .games
            .iter()
            .map(|game| (game.path.clone(), cached_pictures(cache.as_deref(), game)))
            .collect();
        view.refresh(cx);
        view
    }

    /// Adds the games at `paths`, reading them off the UI thread: those in
    /// any archives among them once unpacked, and those that any ROM-hack
    /// patches among them make from games the library has. Games already
    /// in the library are left as they are, so playing one does not read
    /// its ROM again. Each shows as a tile while it is added.
    pub fn add(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.add_paths(paths, false, cx);
    }

    /// Adds the games at `paths` that the user chose, as [`Self::add`]
    /// does, then chooses the first and says it was added.
    pub fn add_chosen(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.add_paths(paths, true, cx);
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>, chosen: bool, cx: &mut Context<Self>) {
        let mut seen = HashSet::new();
        // A file still being added is not added again, which would unpack
        // two copies into one file at once.
        let paths: Vec<PathBuf> = paths
            .iter()
            .map(|path| library::absolute(path))
            .filter(|path| {
                !self.library.contains(path)
                    && !self.adding.iter().any(|(_, adding)| adding.path == *path)
                    && seen.insert(path.clone())
            })
            .collect();
        for path in paths {
            // Only what the user chose shows as being added: a game played
            // or made for the first time is read at once, and the library
            // stays where it was.
            let (key, progress) = if chosen {
                let (key, progress) = self.start_adding(&path, cx);
                (Some(key), progress)
            } else {
                (None, Arc::default())
            };
            let cache = self.cache.clone();
            let known = self.checksums();
            let read = cx
                .background_executor()
                .spawn(async move { read_games(&path, cache.as_deref(), &known, &progress) });
            cx.spawn(async move |this, cx| {
                let games = read.await;
                this.update(cx, |this, cx| {
                    if let Some(key) = key {
                        this.added(key, cx);
                    }
                    let mut errors = Vec::new();
                    let mut titles = Vec::new();
                    for game in games {
                        match game {
                            Ok(game) => {
                                if titles.is_empty() && chosen {
                                    this.selected = Some(game.path.clone());
                                }
                                titles.push(game.title.clone());
                                this.library.add(game);
                            }
                            Err(error) => errors.push(error),
                        }
                    }
                    if chosen {
                        match titles.as_slice() {
                            [] => {}
                            [title] => cx.emit(LibraryEvent::Notice(format!(
                                "Added {title} to the library"
                            ))),
                            titles => cx.emit(LibraryEvent::Notice(format!(
                                "Added {} games to the library",
                                titles.len()
                            ))),
                        }
                        // Where the game now is, once drawn: the grid's rows
                        // come after what leads it.
                        if !titles.is_empty() {
                            this.scroll_to_chosen();
                        }
                    }
                    // After, so a game that could not be added is told.
                    for error in errors {
                        cx.emit(LibraryEvent::Notice(error));
                    }
                    this.save(cx);
                    this.refresh(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Shows the game at `path` as being added, until [`Self::added`] is
    /// called with the key returned; and the progress its unpacking is to
    /// tell, for its tile to show.
    pub fn start_adding(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> (usize, Arc<archive::Progress>) {
        let key = self.next_adding;
        self.next_adding += 1;
        let progress = Arc::<archive::Progress>::default();
        self.adding
            .push((key, Adding::new(path, Arc::clone(&progress))));
        if self.adding_redraws.is_none() {
            self.adding_redraws = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(ADDING_REDRAW).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
        // Up to the grid's first row, where its tile shows.
        self.scroll.scroll_to_item(self.lead);
        (key, progress)
    }

    /// Stops showing the game added under `key` as being added.
    pub fn added(&mut self, key: usize, cx: &mut Context<Self>) {
        self.adding.retain(|(adding, _)| *adding != key);
        if self.adding.is_empty() {
            self.adding_redraws = None;
        }
        cx.notify();
    }

    /// What is being added, for the main window to say over a game:
    /// `"Unpacking Game · 42%"`. `None` until it has been a moment.
    pub fn adding_status(&self) -> Option<String> {
        let mut shown = self.shown_adding();
        let (_, first) = shown.next()?;
        Some(first.status(shown.count()))
    }

    /// The games being added that have been for long enough to show, by
    /// their keys.
    fn shown_adding(&self) -> impl Iterator<Item = (usize, &Adding)> {
        let now = Instant::now();
        self.adding
            .iter()
            .filter(move |(_, adding)| now.duration_since(adding.since) >= ADDING_SHOWS_AFTER)
            .map(|(key, adding)| (*key, adding))
    }

    /// Scrolls the chosen game into view, by its row.
    fn scroll_to_chosen(&self) {
        let shown = self.shown();
        let at = self.chosen_at(&shown) + self.adding_shown;
        self.scroll
            .scroll_to_item(self.lead + at / self.columns.max(1));
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

    /// Shows the store in the library's place, opening it the first time.
    pub fn open_store(&mut self, cx: &mut Context<Self>) {
        let store = match &self.store {
            Some((store, _)) => store.clone(),
            None => {
                let store = cx.new(StoreView::new);
                let subscription = cx.subscribe(&store, Self::store_event);
                self.store = Some((store.clone(), subscription));
                store
            }
        };
        let paths = self
            .library
            .games
            .iter()
            .map(|game| game.path.clone())
            .collect();
        store.update(cx, |store, cx| store.set_library(paths, cx));
        self.store_open = true;
        self.menu = None;
        cx.notify();
    }

    fn store_event(&mut self, _: Entity<StoreView>, event: &StoreEvent, cx: &mut Context<Self>) {
        match event {
            StoreEvent::Close => {
                self.store_open = false;
                cx.notify();
            }
            StoreEvent::Added(games) => {
                for game in games {
                    self.library.add(game.clone());
                }
                self.selected = games.first().map(|game| game.path.clone());
                self.save(cx);
                self.refresh(cx);
                if let [game] = games.as_slice() {
                    cx.emit(LibraryEvent::Notice(format!(
                        "Added {} to the library",
                        game.title
                    )));
                } else {
                    let count = games.len();
                    cx.emit(LibraryEvent::Notice(format!(
                        "Added {count} games to the library"
                    )));
                }
                cx.notify();
            }
            StoreEvent::Play(path) => cx.emit(LibraryEvent::Play(path.clone())),
            StoreEvent::Notice(text) => cx.emit(LibraryEvent::Notice(text.clone())),
        }
    }

    /// Handles a key while the library shows. Whether it was the
    /// library's.
    pub fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.store_open
            && let Some((store, _)) = &self.store
        {
            return store.update(cx, |store, cx| store.key_down(event, cx));
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        // The game menu takes Escape, which closes it, and Return, which
        // plays its game. Any other key closes it and does what it does.
        if let Some(menu) = self.menu.take() {
            match keystroke.key.as_str() {
                "escape" => {}
                "enter" if event.is_held => {}
                "enter" => cx.emit(LibraryEvent::Play(menu.path)),
                _ => {
                    cx.notify();
                    return self.key_down(event, cx);
                }
            }
            cx.notify();
            return true;
        }
        match keystroke.key.as_str() {
            "z" if modifiers.platform && !modifiers.shift && self.removed.is_some() => {
                self.restore(cx);
            }
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
                if let Some(game) = shown.get(self.chosen_at(&shown)) {
                    cx.emit(LibraryEvent::Play(game.path.clone()));
                }
            }
            key @ ("left" | "right" | "up" | "down") => {
                let shown = self.shown();
                let at = self.chosen_at(&shown);
                let at = moved(at, shown.len(), self.columns, key);
                self.selected = shown.get(at).map(|game| game.path.clone());
                self.scroll_to_chosen();
            }
            // Tab and ⇧Tab go through the systems, as in the store.
            "tab" => {
                let choices = self.platform_choices();
                let Some(at) = choices.iter().position(|&choice| choice == self.platform) else {
                    return false;
                };
                let count = choices.len();
                let next = if modifiers.shift {
                    (at + count - 1) % count
                } else {
                    (at + 1) % count
                };
                self.set_platform(choices[next], cx);
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
        self.library.find(&self.query, self.platform)
    }

    /// Where the chosen game is among `shown`: the one chosen, or else the
    /// one last played unless the user is searching, or else the first.
    fn chosen_at(&self, shown: &[&Game]) -> usize {
        let chosen = self
            .selected
            .as_deref()
            .filter(|selected| shown.iter().any(|game| game.path == *selected));
        let recent = || {
            let recent = self.recent().filter(|_| self.query.is_empty())?;
            Some(recent.path.as_path())
        };
        position(shown, chosen.or_else(recent))
    }

    /// Takes the game whose menu is open, or else the chosen game, out of
    /// the library (⌘⌫).
    pub fn remove_chosen(&mut self, cx: &mut Context<Self>) {
        if self.store_open {
            return;
        }
        let path = match &self.menu {
            Some(menu) => Some(menu.path.clone()),
            None => {
                let shown = self.shown();
                shown
                    .get(self.chosen_at(&shown))
                    .map(|game| game.path.clone())
            }
        };
        if let Some(path) = path {
            self.remove(&path, cx);
        }
    }

    /// Takes the game at `path` out of the library, choosing the game that
    /// takes its place, and says how to put it back. Its files, and its
    /// saves beside them, stay where they are.
    fn remove(&mut self, path: &Path, cx: &mut Context<Self>) {
        let at = self.shown().iter().position(|game| game.path == path);
        let Some((place, game)) = self.library.remove(path) else {
            return;
        };
        if let Some(at) = at {
            let shown = self.shown();
            let at = at.min(shown.len().saturating_sub(1));
            self.selected = shown.get(at).map(|game| game.path.clone());
        }
        self.menu = None;
        self.save(cx);
        cx.emit(LibraryEvent::Notice(format!(
            "Removed {} from the library · ⌘Z puts it back",
            game.title
        )));
        self.removed = Some((place, game));
        cx.notify();
    }

    /// Puts the game last taken out back where it was, chosen.
    fn restore(&mut self, cx: &mut Context<Self>) {
        let Some((at, game)) = self.removed.take() else {
            return;
        };
        cx.emit(LibraryEvent::Notice(format!(
            "Put {} back in the library",
            game.title
        )));
        self.selected = Some(game.path.clone());
        self.library.restore(at, game);
        self.save(cx);
        self.refresh(cx);
        cx.notify();
    }

    /// Adds `time` played to each of the games at `paths` that the library
    /// has.
    pub fn add_play_time(&mut self, paths: &[PathBuf], time: Duration, cx: &mut Context<Self>) {
        let mut changed = false;
        for path in paths {
            changed |= self.library.add_play_time(path, time);
        }
        if changed {
            self.save(cx);
            cx.notify();
        }
    }

    /// Opens the menu of the game at `path` `at` that place in the window,
    /// choosing the game, and reads what its files say off the UI thread.
    fn open_menu(&mut self, path: PathBuf, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(game) = self.library.games.iter().find(|game| game.path == path) else {
            return;
        };
        let game = game.clone();
        let read = cx
            .background_executor()
            .spawn(async move { read_facts(&game) });
        let reading = cx.spawn({
            let path = path.clone();
            async move |this, cx| {
                let facts = read.await;
                this.update(cx, |this, cx| {
                    if let Some(menu) = &mut this.menu
                        && menu.path == path
                    {
                        menu.facts = Some(facts);
                        cx.notify();
                    }
                })
                .ok();
            }
        });
        self.selected = Some(path.clone());
        self.menu = Some(GameMenu {
            path,
            at,
            facts: None,
            _reading: reading,
        });
        cx.notify();
    }

    /// Opens the chosen game's menu over the middle of its picture, if
    /// its tile has been drawn, or closes the menu if it is open (⌘I).
    pub fn toggle_chosen_menu(&mut self, cx: &mut Context<Self>) {
        if self.store_open || self.menu.is_some() {
            self.close_menu(cx);
            return;
        }
        let shown = self.shown();
        let Some(path) = shown
            .get(self.chosen_at(&shown))
            .map(|game| game.path.clone())
        else {
            return;
        };
        let Some(tile) = self.chosen_tile.get() else {
            return;
        };
        let middle = tile.size.width / 2.0;
        self.open_menu(
            path,
            point(tile.origin.x + middle, tile.origin.y + middle),
            cx,
        );
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
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

    /// Reads each game's pictures, and when it was played, off the UI
    /// thread; then looks up the games that need it. Called as the library
    /// opens, as games are added and as a game is left.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let games = self.library.games.clone();
        let cache = self.cache.clone();
        let read = cx.background_executor().spawn(async move {
            games
                .iter()
                .map(|game| (game.path.clone(), pictures_on_disk(cache.as_deref(), game)))
                .collect::<HashMap<_, _>>()
        });
        self.refreshing = Some(cx.spawn(async move |this, cx| {
            let read = read.await;
            this.update(cx, |this, cx| {
                let recent = this.recent().map(|game| game.path.clone());
                let mut pictures = HashMap::new();
                for (path, mut fresh) in read {
                    if let Some(old) = this.pictures.remove(&path) {
                        // Found while the files were read.
                        fresh.art = fresh.art.or(old.art);
                        fresh.icon = fresh.icon.or(old.icon);
                        // GPUI keeps every picture it has drawn until told.
                        if let Some(old) = old.left_at
                            && fresh
                                .left_at
                                .as_ref()
                                .is_none_or(|new| new.bytes() != old.bytes())
                        {
                            gpui::ImageSource::Image(old).remove_asset(cx);
                        }
                    }
                    pictures.insert(path, fresh);
                }
                this.pictures = pictures;
                // A game played since, as one just left, is chosen.
                if this.recent().map(|game| &game.path) != recent.as_ref() {
                    this.selected = None;
                }
                this.refreshing = None;
                this.look_up(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Looks up, off the UI thread, unless that is already under way, the
    /// games not yet looked up and those whose box art the cache has lost
    /// or never had, once each a launch. Each game's title and picture
    /// show as soon as it is found.
    fn look_up(&mut self, cx: &mut Context<Self>) {
        let Some(cache) = self.cache.clone() else {
            return;
        };
        let mut pending: Vec<Game> = self
            .library
            .games
            .iter()
            .filter(|game| {
                let pictures = self.pictures.get(&game.path);
                let missing = pictures.is_none_or(|pictures| pictures.art.is_none());
                (!game.looked_up || missing) && !self.tried.contains(&game.path)
            })
            .cloned()
            .collect();
        // Games just added first, before those whose pictures went missing.
        pending.sort_by_key(|game| game.looked_up);
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
        let game = self
            .library
            .games
            .iter_mut()
            .find(|game| game.path == path && game.crc32 == crc32)?;
        let name = match name {
            Ok(name) => name,
            // Said only of a game being added: one whose picture the
            // cache lost keeps its plate quietly until the next launch.
            Err(error) => return (!game.looked_up).then(|| format!("{error:#}")),
        };
        game.found(name);
        if let Some(cache) = self.cache.as_deref() {
            let pictures = self.pictures.entry(path.to_owned()).or_default();
            let art = boxart::art_path(cache, crc32);
            pictures.art = boxart::is_png(&art).then_some(art);
            let icon = boxart::icon_path(cache, crc32);
            pictures.icon = icon.exists().then_some(icon);
        }
        cx.notify();
        None
    }

    fn header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h(px(TOP_BAR))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(TRAFFIC_LIGHTS))
            .pr(px(HEADER_END))
            .gap_1()
            .children(probe::part("header"))
            .child(ui::tabs([
                ui::tab(Section::Library, Section::Library, window),
                ui::tab(Section::Store, Section::Library, window)
                    .on_click(cx.listener(|this, _, _, cx| this.open_store(cx))),
            ]))
            .child(div().flex_1())
            .child(
                icons::button("settings", icons::SETTINGS)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::OpenSettings)))
                    .children(probe::part("button:settings")),
            )
            .child(
                icons::button("add", icons::ADD)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::AddGames)))
                    .children(probe::part("button:add")),
            )
            .into_any_element()
    }

    fn search_field(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = if self.query.is_empty() {
            let placeholder = match self.library.games.len() {
                0 => "Search: just type".to_owned(),
                1 => "Search 1 game: just type".to_owned(),
                count => format!("Search {count} games: just type"),
            };
            div().text_color(rgb(DIM)).child(placeholder)
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
            .mx(px(INSET))
            .mb(px(12.0))
            .h(px(SEARCH_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px(px(SEARCH_PADDING))
            .rounded_lg()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgba(if self.searching {
                with_alpha(CARET, 0xAA)
            } else {
                with_alpha(LINE, 0xFF)
            }))
            .cursor_text()
            .text_sm()
            .overflow_hidden()
            .on_click(cx.listener(|this, _, _, cx| {
                this.searching = true;
                cx.notify();
            }))
            .children(probe::bordered_part("field:search", 1.0))
            .child(
                div()
                    .flex_none()
                    .child(icons::icon(icons::MAGNIFIER, 14.0, DIM))
                    .children(probe::part("field:search.icon")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(1.0))
                    .whitespace_nowrap()
                    .children(caret_before)
                    .child(text)
                    .children(caret_after)
                    .children(probe::part(if self.query.is_empty() {
                        "field:search.placeholder"
                    } else {
                        "field:search.query"
                    })),
            )
            .into_any_element()
    }

    /// The game last played, if any has been.
    fn recent(&self) -> Option<&Game> {
        self.library
            .games
            .iter()
            .filter_map(|game| Some((self.pictures.get(&game.path)?.played?, game)))
            .max_by_key(|(played, _)| *played)
            .map(|(_, game)| game)
    }

    /// The game last played, `width` wide: where it was left, its title,
    /// when it was played and a button to pick it up again.
    fn hero(&self, game: &Game, width: f32, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let pictures = self.pictures.get(&game.path).cloned().unwrap_or_default();
        let height = (width * HERO_SHAPE).clamp(HERO_MIN, HERO_MAX);
        let (light, dark) = tint(game.platform);
        let backdrop = pictures.left_at.as_ref().map(|left_at| {
            img(left_at.clone())
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Cover)
        });
        // Without a picture of where it was left, its box art or icon
        // stands beside the words.
        let side = height - 2.0 * INSET;
        let roomy = width - 3.0 * INSET - side >= HERO_WORDS;
        let beside = (backdrop.is_none() && roomy)
            .then(|| pictures.art.clone().or_else(|| pictures.icon.clone()))
            .flatten()
            .map(|art| {
                div()
                    .absolute()
                    .top(px(INSET))
                    .right(px(INSET))
                    .size(px(side))
                    .child(
                        img(art)
                            .size_full()
                            .object_fit(ObjectFit::Contain)
                            .rounded(px(ART_RADIUS)),
                    )
            });
        let words = width - 2.0 * INSET - if beside.is_some() { side + INSET } else { 0.0 };
        let detail = match pictures.played {
            Some(played) => format!(
                "{} · Played {}",
                platform_name(game.platform),
                ago(played, SystemTime::now())
            ),
            None => platform_name(game.platform).to_owned(),
        };
        let path = game.path.clone();
        let resume = div()
            .id("resume")
            .flex_none()
            .h(px(32.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .pl(px(12.0))
            .pr(px(16.0))
            .rounded_full()
            .cursor_pointer()
            .bg(rgb(CARET))
            .text_color(rgb(ON_ACCENT))
            .text_size(px(13.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .hover(|button| button.opacity(0.9))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(path.clone());
                cx.emit(LibraryEvent::Play(path.clone()));
            }))
            .child(icons::icon(icons::PLAY, 14.0, ON_ACCENT))
            .child("Resume")
            .children(probe::part("button:resume"));
        let soft = |text: &str, size: f32, weight| ui::fit(text, words, size, weight, window, cx);
        let menu_path = game.path.clone();
        div()
            .id("hero")
            .relative()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.open_menu(menu_path.clone(), event.position, cx);
                }),
            )
            .mx(px(INSET))
            .mb(px(16.0))
            .h(px(height))
            .flex_none()
            .rounded(px(HERO_RADIUS))
            .overflow_hidden()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(rgb(light), 0.0),
                linear_color_stop(rgb(dark), 1.0),
            ))
            .children(backdrop)
            // Darker towards the foot and the left, under the words.
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.0,
                linear_color_stop(rgba(0x0000_0000), 0.2),
                linear_color_stop(rgba(0x0000_00E0), 1.0),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.0,
                linear_color_stop(rgba(0x0000_0080), 0.0),
                linear_color_stop(rgba(0x0000_0000), 0.7),
            )))
            .children(beside)
            .child(
                div()
                    .absolute()
                    .left(px(INSET))
                    .bottom(px(INSET))
                    .w(px(words))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgba(0xFFFF_FFB3))
                            .child("CONTINUE PLAYING"),
                    )
                    .child(
                        div()
                            .text_size(px(20.0))
                            .line_height(px(26.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0x00FF_FFFF))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(soft(&game.title, 20.0, gpui::FontWeight::SEMIBOLD))
                            .children(probe::part("hero.title")),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgba(0xFFFF_FFB3))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(soft(&detail, 12.0, gpui::FontWeight::NORMAL)),
                    )
                    .child(div().mt(px(12.0)).flex().child(resume)),
            )
            // A hairline over the picture, so a dark one keeps its edge.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(HERO_RADIUS))
                    .border_1()
                    .border_color(rgba(with_alpha(TEXT, 0x18))),
            )
            .child(corners(HERO_RADIUS, TOP))
            .children(probe::part("hero"))
            .into_any_element()
    }

    /// The systems the library has games for, newest first.
    fn platforms(&self) -> Vec<Platform> {
        let mut platforms: Vec<Platform> = Vec::new();
        for game in &self.library.games {
            if !platforms.contains(&game.platform) {
                platforms.push(game.platform);
            }
        }
        const ORDER: [&str; 6] = ["3ds", "nds", "gba", "gbc", "gb", "rpgmaker"];
        platforms.sort_by_key(|platform| ORDER.iter().position(|tag| *tag == platform.tag()));
        platforms
    }

    /// The choices of system to show: all of them, then each the library
    /// has games for. None if it has games for only one.
    fn platform_choices(&self) -> Vec<Option<Platform>> {
        let platforms = self.platforms();
        if platforms.len() < 2 {
            return Vec::new();
        }
        std::iter::once(None)
            .chain(platforms.into_iter().map(Some))
            .collect()
    }

    /// Shows only `platform`'s games, or all for `None`, from the top.
    fn set_platform(&mut self, platform: Option<Platform>, cx: &mut Context<Self>) {
        self.platform = platform;
        self.selected = None;
        self.menu = None;
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// The system filter, if the library has games for more than one.
    fn filters(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let choices = self.platform_choices();
        if choices.is_empty() {
            return None;
        }
        let choices = choices
            .into_iter()
            .enumerate()
            .map(|(at, platform)| {
                let label = platform.map_or("All", Platform::label);
                ui::choice(("platform", at), label, self.platform == platform)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_platform(platform, cx)))
                    .children(probe::part(format!("segment:platform.{at}")))
            })
            .collect();
        Some(
            div()
                .mx(px(INSET))
                .mb(px(14.0))
                .flex_none()
                .flex()
                .text_xs()
                .child(ui::track("platform", choices))
                .into_any_element(),
        )
    }

    /// What the grid shows, over it, and how many.
    /// The grid's title, with how many games it shows and how many are
    /// being added: `12 games · 1 adding`.
    fn section_title(&self, count: usize, adding: usize) -> AnyElement {
        let title = if self.query.is_empty() {
            self.platform.map_or("All Games", platform_name)
        } else {
            "Results"
        };
        let games = match count {
            1 => "1 game".to_owned(),
            count => format!("{count} games"),
        };
        let count = match (count, adding) {
            (_, 0) => games,
            (0, adding) => format!("{adding} adding"),
            (_, adding) => format!("{games} · {adding} adding"),
        };
        div()
            .mx(px(INSET))
            .mb(px(8.0))
            .flex_none()
            .flex()
            .items_end()
            .gap_2()
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT))
                    .child(title)
                    .children(probe::part("section.title")),
            )
            .child(
                div()
                    .pb(px(1.0))
                    .text_size(px(12.0))
                    .text_color(rgb(DIM))
                    .child(count),
            )
            .into_any_element()
    }

    /// The tile of a game being added, the one added under `key`: in
    /// place of its picture, a plate with an arrow into a tray, breathing,
    /// and a bar of how far it has got, or a piece of one going to and fro
    /// while that is not known; under it, its file's name and what is
    /// being done.
    fn adding_tile(
        &self,
        key: usize,
        adding: &Adding,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let inner = width - 2.0 * TILE_PADDING;
        let track = inner - 2.0 * PLATE_PADDING;
        let bar = div()
            .absolute()
            .top_0()
            .bottom_0()
            .rounded_full()
            .bg(rgb(TEXT));
        let bar = match adding.fraction() {
            Some(fraction) => bar
                .left_0()
                .w(px((track * fraction).max(PROGRESS_BAR)))
                .into_any_element(),
            None => {
                let piece = track * PROGRESS_PIECE;
                bar.w(px(piece))
                    .with_animation(
                        ("adding.bar", key),
                        Animation::new(Duration::from_millis(1600)).repeat(),
                        move |bar, delta| {
                            let along = pulse(delta);
                            let eased = along * along * (3.0 - 2.0 * along);
                            bar.left(px((track - piece) * eased))
                        },
                    )
                    .into_any_element()
            }
        };
        let plate = div()
            .relative()
            .size_full()
            .rounded(px(ART_RADIUS))
            .overflow_hidden()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(rgb(PANEL), 0.0),
                linear_color_stop(rgb(TOP), 1.0),
            ))
            .border_1()
            .border_color(rgba(with_alpha(TEXT, 0x14)))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icons::icon(icons::DOWNLOAD, 24.0, DIM).with_animation(
                        ("adding.icon", key),
                        Animation::new(Duration::from_millis(1600)).repeat(),
                        |icon, delta| icon.opacity(0.45 + 0.55 * pulse(delta)),
                    )),
            )
            .child(
                div()
                    .absolute()
                    .left(px(PLATE_PADDING))
                    .right(px(PLATE_PADDING))
                    .bottom(px(PLATE_PADDING))
                    .h(px(PROGRESS_BAR))
                    .rounded_full()
                    .bg(rgba(with_alpha(TEXT, 0x1F)))
                    .child(bar),
            );
        let line =
            |text: &str, size: f32, weight| ui::fit(text, inner - 1.0, size, weight, window, cx);
        div()
            .w(px(width))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(TILE_PADDING))
            .children(probe::part(format!("adding:{key}")))
            .child(div().w_full().h(px(inner)).child(plate))
            .child(
                div().w_full().flex().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(TITLE_LINE))
                                .text_size(px(13.0))
                                .line_height(px(TITLE_LINE))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(rgb(TEXT))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(line(&adding.name, 13.0, gpui::FontWeight::MEDIUM)),
                        )
                        .child(
                            div()
                                .h(px(DETAIL_LINE))
                                .text_size(px(11.0))
                                .line_height(px(DETAIL_LINE))
                                .text_color(rgb(DIM))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(line(&adding.stage(), 11.0, gpui::FontWeight::NORMAL))
                                .children(probe::part(format!("adding.stage:{key}"))),
                        ),
                ),
            )
            .into_any_element()
    }

    fn tile(
        &self,
        index: usize,
        game: &Game,
        width: f32,
        chosen: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let inner = width - 2.0 * TILE_PADDING;
        let path = game.path.clone();
        let play = game.path.clone();
        let menu_path = game.path.clone();
        let art = cover(game, self.pictures.get(&game.path), inner, window, cx);
        // The play button shows on the chosen game and under the pointer.
        let button = div()
            .absolute()
            .bottom(px(8.0))
            .right(px(8.0))
            .when(!chosen, |corner| {
                corner
                    .opacity(0.0)
                    .group_hover("game", |corner| corner.opacity(1.0))
            })
            .child(
                div()
                    .id(("play", index))
                    .size(px(PLAY_BUTTON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .bg(rgb(CARET))
                    .shadow_md()
                    .hover(|button| button.opacity(0.9))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.selected = Some(play.clone());
                        cx.emit(LibraryEvent::Play(play.clone()));
                    }))
                    .child(icons::icon(icons::PLAY, 14.0, ON_ACCENT))
                    .children(chosen.then(|| probe::part("button:play")).flatten()),
            );
        // A point short, so an ellipsis stays inside the tile.
        let line =
            |text: &str, size: f32, weight| ui::fit(text, inner - 1.0, size, weight, window, cx);
        div()
            .id(("game", index))
            .group("game")
            .w(px(width))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(TILE_PADDING))
            .rounded_lg()
            .cursor_pointer()
            .when(chosen, |tile| tile.bg(rgba(CHOSEN)))
            .when(!chosen, |tile| tile.hover(|tile| tile.bg(rgba(HOVER))))
            // A click chooses the game; a second plays it. A right click,
            // or a Control-click, opens its menu.
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                this.selected = Some(path.clone());
                if event.click_count() >= 2 {
                    cx.emit(LibraryEvent::Play(path.clone()));
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.open_menu(menu_path.clone(), event.position, cx);
                }),
            )
            .children(probe::part(format!("tile:{index}")))
            .when(chosen, |tile| {
                let drawn = self.chosen_tile.clone();
                tile.child(
                    canvas(move |bounds, _, _| drawn.set(Some(bounds)), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
            })
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(inner))
                    .child(art)
                    .child(button)
                    .children(probe::part(format!("tile.art:{index}"))),
            )
            // In a row, as GPUI cuts text short to fit the width it is
            // measured at, and a column measures it before it has one.
            .child(
                div().w_full().flex().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(TITLE_LINE))
                                .text_size(px(13.0))
                                .line_height(px(TITLE_LINE))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(rgb(TEXT))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(line(&game.title, 13.0, gpui::FontWeight::MEDIUM))
                                .children(probe::part(format!("tile.title:{index}"))),
                        )
                        .child(
                            div()
                                .h(px(DETAIL_LINE))
                                .text_size(px(11.0))
                                .line_height(px(DETAIL_LINE))
                                .text_color(rgb(DIM))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(line(
                                    platform_name(game.platform),
                                    11.0,
                                    gpui::FontWeight::NORMAL,
                                )),
                        ),
                ),
            )
            .into_any_element()
    }

    /// The open game menu, over everything, and a shade under it that
    /// closes it on a click or a scroll anywhere else.
    fn menu_layer(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(menu) = &self.menu else {
            return Vec::new();
        };
        let at = menu.at;
        let facts = menu.facts.clone();
        let Some(game) = self
            .library
            .games
            .iter()
            .find(|game| game.path == menu.path)
        else {
            self.menu = None;
            return Vec::new();
        };
        let card = self.game_menu(&game.clone(), facts, window, cx);
        let close =
            |this: &mut Self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>| {
                this.close_menu(cx);
            };
        let shade = div()
            .id("menu-shade")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(close))
            .on_mouse_down(MouseButton::Right, cx.listener(close))
            .on_scroll_wheel(cx.listener(|this, _, _, cx| this.close_menu(cx)));
        // Its top left corner at the pointer, but inset from the window's
        // sides: GPUI keeps a margin only once the menu runs off the
        // window, so the margin goes around it, transparent.
        let margin = point(px(INSET), px(INSET));
        vec![
            deferred(shade).with_priority(1).into_any_element(),
            deferred(
                anchored()
                    .position(at - margin)
                    .snap_to_window()
                    .child(div().p(px(INSET)).child(card)),
            )
            .with_priority(2)
            .into_any_element(),
        ]
    }

    /// The menu of `game`, whose files say `facts` once read: its picture,
    /// title and system; when and how long it has been played, its saves
    /// and its size; then Play, Show in Finder, Randomize… for a Pokémon
    /// game the randomizer knows, and Remove from Library.
    fn game_menu(
        &self,
        game: &Game,
        facts: Option<Facts>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pictures = self.pictures.get(&game.path).cloned().unwrap_or_default();
        let missing = facts.as_ref().is_some_and(|facts| !facts.found);
        let inner = MENU_WIDTH - 2.0 * (MENU_BORDER + MENU_PADDING + MENU_INSET);

        let words = inner - THUMBNAIL - 10.0;
        let system = match &game.serial {
            Some(serial) => format!("{} · {serial}", platform_name(game.platform)),
            None => platform_name(game.platform).to_owned(),
        };
        let header = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .p(px(MENU_INSET))
            .child(thumbnail(game, &pictures, THUMBNAIL))
            .child(
                div()
                    .w(px(words))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .line_height(px(TITLE_LINE))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .whitespace_nowrap()
                            .child(ui::fit(
                                &game.title,
                                words - 1.0,
                                13.0,
                                gpui::FontWeight::SEMIBOLD,
                                window,
                                cx,
                            ))
                            .children(probe::part("menu.title")),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .line_height(px(DETAIL_LINE))
                            .text_color(rgb(DIM))
                            .whitespace_nowrap()
                            .child(ui::fit(
                                &system,
                                words - 1.0,
                                11.0,
                                gpui::FontWeight::NORMAL,
                                window,
                                cx,
                            )),
                    ),
            );

        let now = SystemTime::now();
        let read = |text: fn(&Facts) -> String| facts.as_ref().map_or("…".to_owned(), text);
        let mut lines = vec![
            (
                "Last played",
                pictures
                    .played
                    .map_or("Never".to_owned(), |played| ago(played, now)),
                TEXT,
            ),
            ("Play time", play_time_text(game.play_time), TEXT),
        ];
        // A 3DS game's saves are the core's, and an RPG Maker game keeps
        // its own.
        if !matches!(game.platform, Platform::N3ds | Platform::RpgMaker) {
            lines.push((
                "In-game save",
                read(|facts| {
                    facts
                        .saved
                        .map_or("None".to_owned(), |saved| ago(saved, SystemTime::now()))
                }),
                TEXT,
            ));
        }
        if game.platform != Platform::RpgMaker {
            lines.push((
                "Save states",
                read(|facts| match facts.states {
                    0 => "None".to_owned(),
                    states => format!("{states} of {STATE_SLOTS}"),
                }),
                TEXT,
            ));
        }
        if missing {
            lines.push(("File", "Missing".to_owned(), DANGER));
        } else {
            lines.push(("Size", read(|facts| size_text(facts.size)), TEXT));
        }
        let lines = div()
            .flex()
            .flex_col()
            .px(px(MENU_INSET))
            .py(px(4.0))
            .text_size(px(12.0))
            .children(lines.into_iter().map(|(name, value, colour)| {
                div()
                    .h(px(FACT_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_color(rgb(DIM)).child(name))
                    .child(div().text_color(rgb(colour)).child(value))
            }));

        let play_path = game.path.clone();
        let reveal_path = game.path.clone();
        let randomize_path = game.path.clone();
        let remove_path = game.path.clone();
        let randomizable = facts.as_ref().is_some_and(|facts| facts.randomizable);
        let play_label = if pictures.played.is_some() {
            "Resume"
        } else {
            "Play"
        };
        let play = menu_action(
            "menu-play",
            icons::PLAY,
            play_label,
            Some("↩"),
            TEXT,
            !missing,
        )
        .when(!missing, |row| {
            row.on_click(cx.listener(move |this, _, _, cx| {
                this.menu = None;
                this.selected = Some(play_path.clone());
                cx.emit(LibraryEvent::Play(play_path.clone()));
                cx.notify();
            }))
        })
        .children(probe::part("menu.action:play"));
        let reveal = menu_action(
            "menu-reveal",
            icons::FOLDER,
            "Show in Finder",
            None,
            TEXT,
            !missing,
        )
        .when(!missing, |row| {
            row.on_click(cx.listener(move |this, _, _, cx| {
                cx.reveal_path(&reveal_path);
                this.close_menu(cx);
            }))
        })
        .children(probe::part("menu.action:reveal"));
        let randomize = randomizable.then(|| {
            menu_action(
                "menu-randomize",
                icons::SHUFFLE,
                "Randomize…",
                None,
                TEXT,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.close_menu(cx);
                cx.emit(LibraryEvent::Randomize(randomize_path.clone()));
            }))
            .children(probe::part("menu.action:randomize"))
        });
        let remove = menu_action(
            "menu-remove",
            icons::TRASH,
            "Remove from Library",
            Some("⌘⌫"),
            DANGER,
            true,
        )
        .on_click(cx.listener(move |this, _, _, cx| this.remove(&remove_path, cx)))
        .children(probe::part("menu.action:remove"));

        let rule = || {
            div()
                .h(px(1.0))
                .mx(px(MENU_INSET))
                .my(px(4.0))
                .bg(rgb(LINE))
        };
        div()
            .id("game-menu")
            .occlude()
            .w(px(MENU_WIDTH))
            .flex()
            .flex_col()
            .p(px(MENU_PADDING))
            .rounded(px(MENU_RADIUS))
            .bg(rgb(MENU_BACKGROUND))
            .border(px(MENU_BORDER))
            .border_color(rgba(with_alpha(TEXT, 0x1F)))
            .shadow_lg()
            .text_color(rgb(TEXT))
            .child(header)
            .child(rule())
            .child(lines)
            .child(rule())
            .child(play)
            .child(reveal)
            .children(randomize)
            .child(rule())
            .child(remove)
            .children(probe::bordered_part("menu", MENU_BORDER))
            .into_any_element()
    }

    fn empty(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let none = self.library.games.is_empty();
        let (title, hint) = if none {
            (
                "No games yet".to_owned(),
                "Get free homebrew games from the store, add your own with + or ⌘⇧A, or drop a .gb, .gbc, .gba, .nds or .3ds file, a .zip, .7z or .rar of one, a ROM-hack patch, or an RPG Maker game's folder, here.".to_owned(),
            )
        } else {
            (
                "No games match".to_owned(),
                "Press Escape to clear the search.".to_owned(),
            )
        };
        let room = f32::from(window.viewport_size().width) - 2.0 * INSET;
        let hint = broken_lines(
            &hint,
            room.min(HINT_WIDTH),
            HINT_SIZE,
            gpui::FontWeight::NORMAL,
            usize::MAX,
            window,
            cx,
        );
        // Only in a window with room for it over the buttons and hint.
        let tall = f32::from(window.viewport_size().height) >= 400.0;
        let badge = tall.then(|| {
            div()
                .size(px(56.0))
                .mb_2()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(LINE))
                .child(icons::icon(
                    if none {
                        icons::LIBRARY
                    } else {
                        icons::MAGNIFIER
                    },
                    24.0,
                    DIM,
                ))
        });
        let buttons = none.then(|| {
            let button = |id: &'static str| {
                div()
                    .id(id)
                    .h(px(36.0))
                    .px(px(18.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_full()
                    .cursor_pointer()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .children(probe::part(format!("button:{id}")))
            };
            div()
                .my_1()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap_2()
                .child(
                    button("browse-store")
                        .bg(rgb(CARET))
                        .text_color(rgb(ON_ACCENT))
                        .hover(|button| button.opacity(0.9))
                        .on_click(cx.listener(|this, _, _, cx| this.open_store(cx)))
                        .child(icons::icon(icons::STORE, 16.0, ON_ACCENT))
                        .child("Browse the Store"),
                )
                // Only where there is room for both.
                .when(tall, |buttons| {
                    buttons.child(
                        button("add-games")
                            .bg(rgb(PANEL))
                            .border_1()
                            .border_color(rgb(LINE))
                            .text_color(rgb(TEXT))
                            .hover(|button| button.bg(rgb(LINE)))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::AddGames)))
                            .child(icons::icon(icons::ADD, 16.0, TEXT))
                            .child("Add Games"),
                    )
                })
        });
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px(px(INSET))
            .pb(px(TOP_BAR))
            .children(badge)
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT))
                    .child(title)
                    .children(probe::part("empty.title")),
            )
            // Before the hint, which a small window cuts short.
            .children(buttons)
            .child(
                div()
                    .text_size(px(HINT_SIZE))
                    .text_center()
                    .text_color(rgb(DIM))
                    .child(hint)
                    .children(probe::part("empty.hint")),
            )
            .into_any_element()
    }
}

impl gpui::Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.store_open
            && let Some((store, _)) = &self.store
        {
            return div().size_full().child(store.clone());
        }
        let width = f32::from(window.viewport_size().width);
        let (columns, tile) = grid(width - 2.0 * INSET);
        self.columns = columns;
        // A system the library no longer has games for, or the only one
        // left, is no filter: show them all.
        if !self.platform_choices().contains(&self.platform) {
            self.platform = None;
        }
        let shown = self.shown();
        let at = self.chosen_at(&shown);
        let selected = shown.get(at).map(|game| game.path.clone());
        let games: Vec<Game> = shown.into_iter().cloned().collect();
        // The game drawn as chosen is the one chosen, wherever titles
        // looked up later sort it.
        self.selected = selected;
        self.chosen_tile.set(None);

        // Games being added lead the grid, but not search results.
        let adding: Vec<AnyElement> = if self.query.is_empty() {
            let shown: Vec<(usize, &Adding)> = self.shown_adding().collect();
            shown
                .into_iter()
                .map(|(key, adding)| self.adding_tile(key, adding, tile, window, cx))
                .collect()
        } else {
            Vec::new()
        };
        self.adding_shown = adding.len();

        let root = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(linear_gradient(
                180.0,
                linear_color_stop(rgb(TOP), 0.3),
                linear_color_stop(rgb(FOOT), 1.0),
            ))
            .child(self.header(window, cx))
            .child(self.search_field(cx));
        if games.is_empty() && adding.is_empty() {
            return root.child(self.empty(window, cx));
        }
        // The game last played leads, unless the user is looking for
        // something else or the window is too short to show more.
        let tall = f32::from(window.viewport_size().height) >= ROOM_FOR_HERO;
        let hero = (tall && self.query.is_empty() && self.platform.is_none())
            .then(|| self.recent().cloned())
            .flatten()
            .map(|game| self.hero(&game, width - 2.0 * INSET, window, cx));
        let mut lead: Vec<AnyElement> = hero.into_iter().collect();
        lead.extend(self.filters(cx));
        lead.push(self.section_title(games.len(), adding.len()));
        self.lead = lead.len();
        let mut tiles = adding;
        tiles.extend(
            games
                .iter()
                .enumerate()
                .map(|(index, game)| self.tile(index, game, tile, index == at, window, cx)),
        );
        let mut tiles = tiles.into_iter().peekable();
        let mut rows: Vec<AnyElement> = Vec::new();
        while tiles.peek().is_some() {
            rows.push(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(GAP))
                    .px(px(INSET))
                    .pb(px(GAP))
                    .children(tiles.by_ref().take(columns))
                    .into_any_element(),
            );
        }
        let menu = self.menu_layer(window, cx);
        root.child(
            div()
                .id("library")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .flex()
                .flex_col()
                .pb(px(INSET - GAP))
                .children(lead)
                .children(rows)
                .children(probe::part("grid")),
        )
        .children(menu)
    }
}

/// The box art and icon of `game` in `cache`.
fn cached_pictures(cache: Option<&Path>, game: &Game) -> Pictures {
    let art = cache
        .map(|cache| boxart::art_path(cache, game.crc32))
        .filter(|art| boxart::is_png(art));
    let icon = cache
        .map(|cache| boxart::icon_path(cache, game.crc32))
        .filter(|icon| icon.exists());
    Pictures {
        art,
        icon,
        ..Pictures::default()
    }
}

/// The pictures of `game` on disk, with `cache` holding its box art and
/// icon, and when it was last played.
fn pictures_on_disk(cache: Option<&Path>, game: &Game) -> Pictures {
    let Pictures { art, icon, .. } = cached_pictures(cache, game);
    let checkpoint = library::checkpoint_path(game);
    let played = fs::metadata(&checkpoint)
        .and_then(|metadata| metadata.modified())
        .ok();
    let left_at = played
        .and_then(|_| fs::read(persist::state_picture_path(&checkpoint)).ok())
        .map(|bytes| Arc::new(gpui::Image::from_bytes(ImageFormat::Png, bytes)));
    Pictures {
        art,
        icon,
        left_at,
        played,
    }
}

/// The picture of `game` filling a square `side` wide: its box art; or
/// else the screen where it was left off; or else a plate in its system's
/// colour with its icon, or its title.
fn cover(
    game: &Game,
    pictures: Option<&Pictures>,
    side: f32,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let pictures = pictures.cloned().unwrap_or_default();
    if let Some(art) = pictures.art {
        return img(art)
            .size_full()
            .object_fit(ObjectFit::Contain)
            .rounded(px(ART_RADIUS))
            .into_any_element();
    }
    if let Some(left_at) = pictures.left_at {
        return img(left_at)
            .size_full()
            .object_fit(ObjectFit::Contain)
            .rounded(px(ART_RADIUS))
            .into_any_element();
    }
    let (light, dark) = tint(game.platform);
    let middle = match pictures.icon {
        Some(icon) => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                img(icon)
                    .size(px(side * 0.46))
                    .object_fit(ObjectFit::Contain),
            ),
        None => {
            // Under the system's name, clear of the play button that shows
            // in the bottom corner of a chosen game.
            let title = broken_lines(
                &game.title,
                side - 2.0 * PLATE_PADDING,
                PLATE_TITLE,
                gpui::FontWeight::SEMIBOLD,
                3,
                window,
                cx,
            );
            div()
                .absolute()
                .left(px(PLATE_PADDING))
                .right(px(PLATE_PADDING))
                .top(px(PLATE_PADDING + PLATE_LABEL + 8.0))
                .text_size(px(PLATE_TITLE))
                .line_height(px(PLATE_TITLE + 4.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(0x00FF_FFFF))
                .child(title)
        }
    };
    div()
        .relative()
        .size_full()
        .rounded(px(ART_RADIUS))
        .overflow_hidden()
        .bg(linear_gradient(
            135.0,
            linear_color_stop(rgb(light), 0.0),
            linear_color_stop(rgb(dark), 1.0),
        ))
        .border_1()
        .border_color(rgba(with_alpha(TEXT, 0x14)))
        .child(middle)
        .child(
            div()
                .absolute()
                .top(px(PLATE_PADDING))
                .left(px(PLATE_PADDING))
                .h(px(PLATE_LABEL))
                .px(px(6.0))
                .flex()
                .items_center()
                .rounded(px(5.0))
                .bg(rgba(0x0000_0047))
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgba(0xFFFF_FFD9))
                .child(game.platform.label()),
        )
        .into_any_element()
}

/// What spills past the rounded corners of the box it is in, `radius`
/// round, covered over in `colour`, the colour behind the box: GPUI clips
/// what is in a box to its bounds, not its corners. A band round the box
/// whose inner edge is the box's rounded edge.
fn corners(radius: f32, colour: u32) -> gpui::Div {
    const BAND: f32 = 8.0;
    div()
        .absolute()
        .top(px(-BAND))
        .left(px(-BAND))
        .right(px(-BAND))
        .bottom(px(-BAND))
        .rounded(px(radius + BAND))
        .border_8()
        .border_color(rgb(colour))
}

/// Room inside a plate standing in for a picture, and the size of the
/// title on it.
const PLATE_PADDING: f32 = 10.0;
const PLATE_TITLE: f32 = 14.0;
/// Height of the system's name in a plate's corner.
const PLATE_LABEL: f32 = 18.0;

/// How long before `now` `then` was, in words: `"just now"`, `"5 min
/// ago"`, `"yesterday"`.
fn ago(then: SystemTime, now: SystemTime) -> String {
    let seconds = now.duration_since(then).map_or(0, |gone| gone.as_secs());
    let (minute, hour, day) = (60, 60 * 60, 24 * 60 * 60);
    match seconds {
        s if s < minute => "just now".to_owned(),
        s if s < hour => format!("{} min ago", s / minute),
        s if s < 2 * hour => "1 hour ago".to_owned(),
        s if s < day => format!("{} hours ago", s / hour),
        s if s < 2 * day => "yesterday".to_owned(),
        s if s < 7 * day => format!("{} days ago", s / day),
        s if s < 14 * day => "last week".to_owned(),
        s if s < 60 * day => format!("{} weeks ago", s / (7 * day)),
        s if s < 365 * day => format!("{} months ago", s / (30 * day)),
        _ => "over a year ago".to_owned(),
    }
}

/// The games at `path`: the ROM or RPG Maker game there, or each ROM in it
/// if it is an archive, unpacked into the games folder; and the game each
/// ROM-hack patch there makes from the game it names among `known`
/// (checksum and path) or the archive's ROMs. Tells `progress` how far an
/// archive's unpacking has got.
fn read_games(
    path: &Path,
    cache: Option<&Path>,
    known: &[(u32, PathBuf)],
    progress: &archive::Progress,
) -> Vec<Result<Game, String>> {
    let files = if archive::is_archive(path) {
        match archive::unpack(path, progress) {
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
/// icon or its player app's icon saved in `cache`. A 3DS game's headers
/// are read without reading the rest of its file.
pub fn read_game(path: &Path, cache: Option<&Path>) -> Result<Game, String> {
    let cannot_read = |error: std::io::Error| format!("Cannot read {}: {error}", path.display());
    if let Some(folder) = rpg_maker::game_folder(path) {
        let game = Game::rpg_maker(&folder, rpg_maker::title(&folder));
        let player = rpg_maker::player_app(&folder);
        if let (Some(cache), Some(player)) = (cache, player) {
            // Its only picture: no game database lists RPG Maker games.
            app_icon::save(&player, &boxart::icon_path(cache, game.crc32)).ok();
        }
        return Ok(game);
    }
    if emu_3ds::is_game(path) {
        return Ok(Game::new(
            path,
            &rom_info::read_3ds(path).map_err(cannot_read)?,
        ));
    }
    let rom = fs::read(path).map_err(cannot_read)?;
    let info = rom_info::read(path, &rom).ok_or_else(|| {
        format!(
            "{} is not a .gb, .gbc, .gba, .nds or .3ds game",
            path.display()
        )
    })?;
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

/// `text` broken into lines no wider than `width` logical pixels at
/// `font_size` and `weight`, at most `max_lines` of them, the last cut
/// short with an ellipsis if need be. GPUI centres a line it wraps by its
/// width up to the next line, which takes in the space it broke at and
/// leaves the line half a space off centre; lines broken here are each
/// centred on their own.
fn broken_lines(
    text: &str,
    width: f32,
    font_size: f32,
    weight: gpui::FontWeight,
    max_lines: usize,
    window: &Window,
    cx: &App,
) -> SharedString {
    let mut font = window.text_style().font();
    font.weight = weight;
    let mut wrapper = cx.text_system().line_wrapper(font, px(font_size));
    let mut lines: Vec<String> = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        if lines.len() + 1 == max_lines {
            let last =
                wrapper.truncate_line(rest.to_owned().into(), px(width), "…", &mut Vec::new());
            lines.push(last.replace(" …", "…"));
            break;
        }
        let fragments = [LineFragment::text(rest)];
        let end = wrapper
            .wrap_line(&fragments, px(width))
            .next()
            .map(|boundary| boundary.ix)
            .filter(|&end| end > 0)
            .unwrap_or(rest.len());
        let (line, after) = rest.split_at(end);
        lines.push(line.trim_end().to_owned());
        rest = after.trim_start();
    }
    lines.join("\n").into()
}

/// How many tiles fit across `width` logical pixels, and how wide each is
/// so that they fill it.
/// An action in a game's menu: its icon, its label and the keys that do
/// it too, in `colour`, or dimmed if it can't be done. The caller says
/// what a click does.
fn menu_action(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    keys: Option<&'static str>,
    colour: u32,
    enabled: bool,
) -> Stateful<gpui::Div> {
    let colour = if enabled { colour } else { DIM };
    div()
        .id(id)
        .h(px(ACTION_HEIGHT))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(MENU_INSET))
        .rounded(px(ROW_RADIUS))
        .text_size(px(13.0))
        .text_color(rgb(colour))
        .when(enabled, |row| {
            row.cursor_pointer()
                .hover(move |row| row.bg(rgba(with_alpha(colour, 0x1A))))
        })
        .child(icons::icon(icon, icons::SIZE, colour))
        .child(div().flex_1().child(label))
        .children(keys.map(|keys| div().text_size(px(12.0)).text_color(rgb(DIM)).child(keys)))
}

/// `game`'s picture, `side` square, atop its menu: its box art, or the
/// screen where it was left off, or its icon or system's label on a plate
/// in its system's colour.
fn thumbnail(game: &Game, pictures: &Pictures, side: f32) -> AnyElement {
    let frame = div().size(px(side)).flex_none();
    if let Some(art) = &pictures.art {
        return frame
            .child(
                img(art.clone())
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .rounded(px(ROW_RADIUS)),
            )
            .into_any_element();
    }
    if let Some(left_at) = &pictures.left_at {
        return frame
            .child(
                img(left_at.clone())
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .rounded(px(ROW_RADIUS)),
            )
            .into_any_element();
    }
    let (light, dark) = tint(game.platform);
    let middle = match &pictures.icon {
        Some(icon) => img(icon.clone())
            .size(px(side * 0.6))
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
        None => div()
            .text_size(px(10.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgba(0xFFFF_FFD9))
            .child(game.platform.label())
            .into_any_element(),
    };
    frame
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(ROW_RADIUS))
        .bg(linear_gradient(
            135.0,
            linear_color_stop(rgb(light), 0.0),
            linear_color_stop(rgb(dark), 1.0),
        ))
        .border_1()
        .border_color(rgba(with_alpha(TEXT, 0x14)))
        .child(middle)
        .into_any_element()
}

/// What `game`'s files say of it: whether its ROM or folder is there, its
/// size, when its battery save was written and how many save states it
/// has.
fn read_facts(game: &Game) -> Facts {
    let Ok(metadata) = fs::metadata(&game.path) else {
        return Facts::default();
    };
    let size = if metadata.is_dir() {
        folder_size(&game.path)
    } else {
        metadata.len()
    };
    if game.platform == Platform::RpgMaker {
        return Facts {
            found: true,
            size,
            ..Facts::default()
        };
    }
    let files = persist::GameFiles::of(&game.path);
    let saved = fs::metadata(files.backup())
        .and_then(|metadata| metadata.modified())
        .ok();
    let states = (1..=STATE_SLOTS)
        .filter(|&slot| files.state(slot, false).is_file())
        .count();
    Facts {
        found: true,
        size,
        saved,
        states,
        randomizable: randomizer::generation(&game.path).is_some(),
    }
}

/// How much room the files in `folder` take, not following links.
fn folder_size(folder: &Path) -> u64 {
    let mut total = 0;
    let mut folders = vec![folder.to_owned()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(entry.path()),
                Ok(kind) if kind.is_file() => {
                    total += entry.metadata().map_or(0, |metadata| metadata.len());
                }
                _ => {}
            }
        }
    }
    total
}

/// `time` played, as the game menu says it.
fn play_time_text(time: Duration) -> String {
    let minutes = time.as_secs() / 60;
    match (minutes / 60, minutes % 60) {
        _ if time.is_zero() => "None yet".to_owned(),
        (0, 0) => "Under a minute".to_owned(),
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// `bytes` as Finder says sizes, in thousands.
fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut size = bytes as f64 / 1000.0;
    let mut unit = 0;
    while size >= 999.5 && unit + 1 < UNITS.len() {
        size /= 1000.0;
        unit += 1;
    }
    if size < 9.95 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.0} {}", UNITS[unit])
    }
}

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
    fn a_3ds_game_is_read_from_its_headers_and_titled_by_its_file() {
        let dir = std::env::temp_dir().join(format!("emu-app-read-3ds-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("Cool_Homebrew.3dsx");
        let mut file = vec![0; 0x20];
        file[0..4].copy_from_slice(b"3DSX");
        file[4..6].copy_from_slice(&0x20_u16.to_le_bytes());
        fs::write(&path, &file).expect("written");

        let game = read_game(&path, None).expect("listed");
        assert_eq!(game.platform, rom_info::Platform::N3ds);
        assert_eq!(game.title, "Cool Homebrew");
        assert_eq!(game.path, path);
        fs::remove_dir_all(&dir).ok();
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

    #[test]
    fn a_games_pictures_and_last_play_are_read_from_disk() {
        let dir = std::env::temp_dir().join(format!("emu-app-pictures-{}", std::process::id()));
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).expect("temp dir");
        let info = rom_info::RomInfo {
            platform: rom_info::Platform::Gba,
            crc32: 0x1234,
            serial: None,
            header_title: None,
            banner_title: None,
        };
        let game = Game::new(&dir.join("Golden Sun.gba"), &info);

        // Never played, nor looked up.
        let pictures = pictures_on_disk(Some(&cache), &game);
        assert!(pictures.art.is_none() && pictures.icon.is_none());
        assert!(pictures.played.is_none() && pictures.left_at.is_none());

        let png = b"\x89PNG\r\n\x1a\n";
        boxart::write(&boxart::art_path(&cache, 0x1234), png).expect("art");
        boxart::write(&boxart::icon_path(&cache, 0x1234), png).expect("icon");
        let checkpoint = library::checkpoint_path(&game);
        fs::write(&checkpoint, b"state").expect("checkpoint");
        fs::write(persist::state_picture_path(&checkpoint), png).expect("picture");
        let pictures = pictures_on_disk(Some(&cache), &game);
        assert_eq!(pictures.art, Some(boxart::art_path(&cache, 0x1234)));
        assert_eq!(pictures.icon, Some(boxart::icon_path(&cache, 0x1234)));
        assert!(pictures.played.is_some());
        assert_eq!(
            pictures.left_at.map(|picture| picture.bytes().len()),
            Some(8)
        );

        // Without a cache folder, the game still has where it was left.
        let pictures = pictures_on_disk(None, &game);
        assert!(pictures.art.is_none() && pictures.left_at.is_some());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn how_long_ago_reads_as_words() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(400 * 24 * 60 * 60);
        let ago_by = |seconds: u64| ago(now - Duration::from_secs(seconds), now);
        assert_eq!(ago_by(5), "just now");
        assert_eq!(ago_by(5 * 60), "5 min ago");
        assert_eq!(ago_by(90 * 60), "1 hour ago");
        assert_eq!(ago_by(3 * 60 * 60), "3 hours ago");
        assert_eq!(ago_by(30 * 60 * 60), "yesterday");
        assert_eq!(ago_by(3 * 24 * 60 * 60), "3 days ago");
        assert_eq!(ago_by(10 * 24 * 60 * 60), "last week");
        assert_eq!(ago_by(21 * 24 * 60 * 60), "3 weeks ago");
        assert_eq!(ago_by(90 * 24 * 60 * 60), "3 months ago");
        assert_eq!(ago_by(380 * 24 * 60 * 60), "over a year ago");
        // A clock set back since reads as just now.
        assert_eq!(ago(now + Duration::from_secs(60), now), "just now");
    }

    #[test]
    fn play_time_and_sizes_read_as_words() {
        let played = |seconds| play_time_text(Duration::from_secs(seconds));
        assert_eq!(played(0), "None yet");
        assert_eq!(played(59), "Under a minute");
        assert_eq!(played(45 * 60 + 30), "45 min");
        assert_eq!(played(2 * 60 * 60), "2 h");
        assert_eq!(played(3 * 60 * 60 + 12 * 60), "3 h 12 min");
        assert_eq!(size_text(512), "512 bytes");
        assert_eq!(size_text(32_768), "33 KB");
        assert_eq!(size_text(8_388_608), "8.4 MB");
        assert_eq!(size_text(999_999), "1.0 MB");
        assert_eq!(size_text(268_435_456), "268 MB");
        assert_eq!(size_text(4_294_967_296), "4.3 GB");
    }

    #[test]
    fn a_games_facts_are_read_from_its_files() {
        let dir = std::env::temp_dir().join(format!("emu-app-facts-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let info = |platform| rom_info::RomInfo {
            platform,
            crc32: 1,
            serial: None,
            header_title: None,
            banner_title: None,
        };
        let rom = dir.join("Golden Sun.gba");
        let game = Game::new(&rom, &info(Platform::Gba));
        assert_eq!(read_facts(&game), Facts::default(), "a missing ROM");

        fs::write(&rom, [0; 2048]).expect("rom");
        let facts = read_facts(&game);
        assert!(facts.found);
        assert_eq!((facts.size, facts.saved, facts.states), (2048, None, 0));

        fs::write(dir.join("Golden Sun.sav"), [0; 8]).expect("save");
        fs::write(dir.join("Golden Sun.ss1"), [0; 8]).expect("state");
        fs::write(dir.join("Golden Sun.ss3"), [0; 8]).expect("state");
        // The checkpoint is no slot.
        fs::write(dir.join("Golden Sun.ss0"), [0; 8]).expect("checkpoint");
        let facts = read_facts(&game);
        assert!(facts.saved.is_some());
        assert_eq!(facts.states, 2);
        assert!(!facts.randomizable, "not a Pokémon game");

        // FireRed's game code says the randomizer knows it.
        let mut fire_red = vec![0; 2048];
        fire_red[0xAC..0xB0].copy_from_slice(b"BPRE");
        let rom = dir.join("FireRed.gba");
        fs::write(&rom, fire_red).expect("rom");
        assert!(read_facts(&Game::new(&rom, &info(Platform::Gba))).randomizable);

        // An RPG Maker game is its folder, all of it.
        let folder = dir.join("Fan Game");
        fs::create_dir_all(folder.join("Data")).expect("folder");
        fs::write(folder.join("Game.ini"), [0; 100]).expect("ini");
        fs::write(folder.join("Data/Map001.rxdata"), [0; 900]).expect("map");
        let facts = read_facts(&Game::rpg_maker(&folder, None));
        assert!(facts.found);
        assert_eq!(facts.size, 1000);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_game_being_added_says_how_far_it_has_got() {
        let adding = |path: &str, progress: archive::Progress| {
            Adding::new(Path::new(path), Arc::new(progress))
        };
        let archive = "/Downloads/Omega Ruby (USA).7z";
        // Before the archive's size is known, and while it unpacks: the
        // share is rounded down, so it never reads 100% too soon.
        let listing = adding(archive, archive::Progress::default());
        assert_eq!(listing.stage(), "Unpacking…");
        assert_eq!(listing.fraction(), None);
        let unpacking = adding(archive, archive::Progress::at(999, 1000));
        assert_eq!(unpacking.name, "Omega Ruby (USA)");
        assert_eq!(unpacking.stage(), "Unpacking · 99%");
        assert_eq!(unpacking.fraction(), Some(0.999));
        assert_eq!(unpacking.status(0), "Unpacking Omega Ruby (USA) · 99%");
        assert_eq!(listing.status(2), "Unpacking Omega Ruby (USA)… · 2 more");
        // Unpacked, the game is read into the library.
        let reading = adding(archive, archive::Progress::at(1000, 1000));
        assert_eq!(reading.stage(), "Adding…");
        assert_eq!(reading.fraction(), None, "the bar goes to and fro");
        assert_eq!(reading.status(0), "Adding Omega Ruby (USA)…");

        let patch = adding("/Downloads/Hack.bps", archive::Progress::default());
        assert_eq!(patch.status(0), "Patching Hack…");
        let rom = adding("/Downloads/Tetris.gb", archive::Progress::default());
        assert_eq!(rom.status(0), "Adding Tetris…");
    }
}
