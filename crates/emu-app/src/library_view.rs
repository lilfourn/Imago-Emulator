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
//! Remove from Library. On Linux and Windows those are Delete, Ctrl+Z and
//! Ctrl+I, and Show in Files or Show in File Explorer.
//! The view only shows and edits the library; it asks the main window,
//! through [`LibraryEvent`], to play a game, add games or open settings.
//!
//! This file is the view's state and what changes it: the games, what is
//! chosen and shown, the keys, adding and removing games, and looking them
//! up. How it is drawn is `library_view/draw.rs`, and what it reads from
//! disk, off the UI thread, is `library_view/files.rs`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use emu_cores::rpg_maker;
use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, Context, Entity, EventEmitter, ImageFormat,
    KeyDownEvent, LineFragment, MouseButton, MouseDownEvent, ObjectFit, Pixels, Point,
    ScrollHandle, SharedString, Stateful, Subscription, Task, Window, anchored, canvas, deferred,
    div, img, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, rgba,
};

use crate::app::{TOP_BAR, shortcut};
use crate::app_icon;
use crate::archive;
use crate::boxart;
use crate::chrome;
use crate::icons;
use crate::keys;
use crate::leftovers;
use crate::library::{self, Game, Library};
use crate::persist;
use crate::probe;
use crate::randomizer;
use crate::rom_info::{self, Platform};
use crate::romhack::{self, MakeError};
use crate::slots::{LAST_FILE_SLOT, STATE_SLOTS};
use crate::store_view::{StoreEvent, StoreView, platform_name, pulse};
use crate::ui::{self, Section};

use crate::theme::{
    ACCENT as CARET, BACKGROUND as TOP, CHOSEN, DANGER, HOVER, INSET, LINE, MUTED as DIM,
    ON_ACCENT, PANEL, TEXT, tint, with_alpha,
};

mod draw;
mod files;

pub use draw::{HEADER_END, TRAFFIC_LIGHTS};
pub use files::read_game;
use files::{cached_pictures, pictures_on_disk, read_facts, read_games};

/// How long a game is being added before its tile shows, so one added at
/// once does not flash up; and how often the tiles are drawn anew after,
/// for their progress.
const ADDING_SHOWS_AFTER: Duration = Duration::from_millis(250);
const ADDING_REDRAW: Duration = Duration::from_millis(100);

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
/// of the game, and what can be done with it. Not the menu over a running
/// game, which is `game_menu::GameMenu`.
struct ContextMenu {
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
    /// How many of the slots only a script saves to hold a state.
    script_states: usize,
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
    /// The games the library file held when this copy of the app last read
    /// or wrote it, by path: one it holds beyond these, another copy
    /// added.
    on_disk: HashSet<PathBuf>,
    /// The games the player has been told the file cannot hold, by path,
    /// so each is said once.
    told_left_out: HashSet<PathBuf>,
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
    menu: Option<ContextMenu>,
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
            on_disk: library.paths(),
            told_left_out: HashSet::new(),
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

    /// The library's record of the game at `path`, if it has it.
    pub fn game_of(&self, path: &Path) -> Option<library::Game> {
        self.library
            .games
            .iter()
            .find(|game| game.path == path)
            .cloned()
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
            "z" if modifiers.secondary() && !modifiers.shift && self.removed.is_some() => {
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
                let at = ui::moved(at, shown.len(), self.columns, key);
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
            "Removed {} from the library · {} puts it back",
            game.title,
            keys::shortcut(shortcut::PUT_BACK)
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
        self.menu = Some(ContextMenu {
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

    /// Writes the library to its file, keeping the games another copy of
    /// the app added to it meanwhile, which then show here too.
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = &self.path else {
            return;
        };
        match self.library.save_keeping(path, &mut self.on_disk) {
            Ok(0) => {}
            Ok(_) => {
                self.refresh(cx);
                cx.notify();
            }
            Err(error) => {
                let message = format!("Cannot save the library to {}: {error}", path.display());
                cx.emit(LibraryEvent::Notice(message));
            }
        }
        for notice in left_out_notices(&self.library, &mut self.told_left_out) {
            cx.emit(LibraryEvent::Notice(notice));
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

    /// The game last played, if any has been.
    fn recent(&self) -> Option<&Game> {
        self.library
            .games
            .iter()
            .filter_map(|game| Some((self.pictures.get(&game.path)?.played?, game)))
            .max_by_key(|(played, _)| *played)
            .map(|(_, game)| game)
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
}

/// What to tell the player of the games in `library` that its file cannot
/// hold, which `told` does not have yet: each is said once, and `told`
/// then has it.
fn left_out_notices(library: &Library, told: &mut HashSet<PathBuf>) -> Vec<String> {
    library
        .left_out()
        .filter(|game| told.insert(game.path.clone()))
        .map(|game| {
            format!(
                "{} plays, but will not be in the library next time: its file's name is not valid Unicode. Rename the file to keep it.",
                game.title
            )
        })
        .collect()
}

/// Where the game at `selected` is among `shown`, or the first if it is
/// not shown or none is chosen.
fn position(shown: &[&Game], selected: Option<&Path>) -> usize {
    selected
        .and_then(|selected| shown.iter().position(|game| game.path == selected))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_game_the_library_file_cannot_hold_is_said_once() {
        use std::os::unix::ffi::OsStringExt as _;

        let odd = PathBuf::from(std::ffi::OsString::from_vec(
            b"/games/Pok\xE9mon Fan Game".to_vec(),
        ));
        let mut library = Library::default();
        library.add(Game::rpg_maker(Path::new("/games/Uranium"), None));
        let mut told = HashSet::new();
        assert!(left_out_notices(&library, &mut told).is_empty());
        library.add(Game::rpg_maker(&odd, Some("Fan Game".to_owned())));
        assert_eq!(
            left_out_notices(&library, &mut told),
            [
                "Fan Game plays, but will not be in the library next time: its file's name is not valid Unicode. Rename the file to keep it."
            ]
        );
        // Every later save says nothing more of it.
        assert!(left_out_notices(&library, &mut told).is_empty());
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
