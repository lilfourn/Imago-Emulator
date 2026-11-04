//! The store: free homebrew games to add to the library, from the catalogs
//! [`crate::store`] reads. It shows in the main window in the library's
//! place.
//!
//! A grid of the games, newest first, narrowed by what the user types and
//! by system and kind. Each game opens a page of its own with its
//! screenshots, what it is, who made it and its licence. Get downloads a
//! game into the games folder and adds it to the library with its cover,
//! in one go; Play plays a game the library has.
//!
//! Keyboard first, as the library is: typing searches, the arrow keys
//! choose, Return opens a game's page and there gets or plays it, Tab and
//! ⇧Tab change system, and Escape goes back. The view says what the user
//! did through [`StoreEvent`]; the library does the rest.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Context, EventEmitter, KeyDownEvent, ObjectFit,
    ScrollHandle, ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window, div, img,
    linear_color_stop, linear_gradient, prelude::*, px, rgb, rgba, uniform_list,
};

use crate::app::TOP_BAR;
use crate::archive;
use crate::boxart;
use crate::icons;
use crate::library::Game;
use crate::library_view::{HEADER_END, TRAFFIC_LIGHTS, read_game};
use crate::probe;
use crate::rom_info::Platform;
use crate::store::{self, Kind, Offer};
use crate::theme::{
    ACCENT, BACKGROUND, CHOSEN, HOVER, INSET, LINE, MUTED as DIM, ON_ACCENT, PANEL, TEXT,
    with_alpha,
};
use crate::ui::{self, Section, choice, fit, track};

/// The background lightens a touch towards the foot, as the library's does.
const FOOT: u32 = 0x0016_181B;
/// Space between the games.
const GAP: f32 = 12.0;
/// Room around a game's cover and words, inside its tile.
const TILE_PADDING: f32 = 6.0;
/// The narrowest a game's tile gets before the grid drops a column.
const MIN_TILE: f32 = 150.0;
/// A cover's height for its width: screenshots are 10:9 (Game Boy), 3:2
/// (GBA) or 4:3 (DS); each fits whole in a 4:3 frame.
const COVER_SHAPE: f32 = 0.75;
/// Line heights of a tile's title and its maker.
const TITLE_LINE: f32 = 18.0;
const MAKER_LINE: f32 = 16.0;
/// Height of the button on a tile.
const PILL: f32 = 22.0;
/// Height of the search field, and room around its magnifier, as the
/// library's.
const SEARCH_HEIGHT: f32 = 32.0;
const SEARCH_PADDING: f32 = 8.0;
/// Widest a game's page gets.
const PAGE_WIDTH: f32 = 640.0;
/// Tallest a game's screenshot gets on its page.
const SHOT_HEIGHT: f32 = 360.0;
/// How many pictures download at once.
const PICTURE_WORKERS: usize = 6;

/// The systems the filter offers, `None` being all of them.
const PLATFORMS: [Option<Platform>; 5] = [
    None,
    Some(Platform::Gb),
    Some(Platform::Gbc),
    Some(Platform::Gba),
    Some(Platform::Nds),
];

/// What the store asks of the library.
#[derive(Debug, Clone)]
pub enum StoreEvent {
    /// Leave the store.
    Close,
    /// Add these games, just downloaded, to the library.
    Added(Vec<Game>),
    /// Play the game at this path, which the library has.
    Play(PathBuf),
    /// Tell the user something.
    Notice(String),
}

/// A picture, by its URL: downloading, there, or not to be had.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Picture {
    Coming,
    Ready(PathBuf),
    Missing,
}

/// Pictures waiting to download, the latest asked for first: those just
/// scrolled into view, or the page just opened.
#[derive(Default)]
struct Queue {
    urls: Mutex<Vec<String>>,
    more: Condvar,
}

/// The store's view.
pub struct StoreView {
    /// Where catalogs and pictures are kept, if there is a cache folder.
    cache: Option<PathBuf>,
    /// Where games are installed, if there is a data folder.
    games: Option<PathBuf>,
    offers: Vec<Offer>,
    /// The places in `offers` of the games shown, in order.
    shown: Vec<usize>,
    /// While a catalog is read or fetched.
    loading: Option<Task<()>>,
    /// Why the catalog could not be fetched, if it could not.
    problem: Option<String>,
    query: String,
    /// Whether the search field has the caret.
    searching: bool,
    platform: Option<Platform>,
    kind: Kind,
    /// The chosen game, by [`Offer::id`]; the first shown if none is.
    selected: Option<String>,
    /// The game whose page shows, by its place in `offers`, and which of
    /// its screenshots.
    page: Option<(usize, usize)>,
    /// The games the library has, by id, and where each one's ROM is.
    owned: HashMap<String, PathBuf>,
    /// The library's games, to tell which of the store's it has.
    library: Vec<PathBuf>,
    /// The games being got, by id.
    getting: HashSet<String>,
    pictures: HashMap<String, Picture>,
    queue: Arc<Queue>,
    /// Where the picture downloaders send what they find, once started.
    found: async_channel::Sender<(String, Picture)>,
    downloading: bool,
    /// How many games fit across, and how wide each is, as last drawn.
    columns: usize,
    tile: f32,
    scroll: UniformListScrollHandle,
    page_scroll: ScrollHandle,
    _pictures: Task<()>,
}

impl EventEmitter<StoreEvent> for StoreView {}

impl StoreView {
    /// The store, showing the catalogs as last fetched and fetching them
    /// anew if they are a day old.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (found, receiver) = async_channel::unbounded();
        let pictures = cx.spawn(async move |this, cx| {
            while let Ok((url, picture)) = receiver.recv().await {
                let updated = this.update(cx, |this, cx| {
                    this.pictures.insert(url, picture);
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
        });
        let mut view = Self {
            cache: boxart::cache_dir(),
            games: archive::games_dir(),
            offers: Vec::new(),
            shown: Vec::new(),
            loading: None,
            problem: None,
            query: String::new(),
            searching: false,
            platform: None,
            kind: Kind::Game,
            selected: None,
            page: None,
            owned: HashMap::new(),
            library: Vec::new(),
            getting: HashSet::new(),
            pictures: HashMap::new(),
            queue: Arc::default(),
            found,
            downloading: false,
            columns: 1,
            tile: MIN_TILE,
            scroll: UniformListScrollHandle::new(),
            page_scroll: ScrollHandle::new(),
            _pictures: pictures,
        };
        view.load(false, cx);
        view
    }

    /// Tells the store which games the library has, at these paths.
    pub fn set_library(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.library = paths;
        self.find_owned();
        cx.notify();
    }

    /// Reads the catalogs as last kept, then fetches them anew if they
    /// are old or `refetch` asks, off the UI thread.
    fn load(&mut self, refetch: bool, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            return;
        }
        let Some(cache) = self.cache.clone() else {
            self.problem = Some("There is no cache folder to keep the store in".to_owned());
            return;
        };
        self.problem = None;
        let (answers, receiver) = async_channel::unbounded();
        let spawned = thread::Builder::new()
            .name("store-catalog".to_owned())
            .spawn(move || {
                let kept = store::cached(&cache);
                let fresh = kept.fresh;
                if !kept.offers.is_empty() {
                    answers.send_blocking(Ok(kept)).ok();
                }
                if refetch || !fresh {
                    let fetched = store::fetch(&cache).map_err(|error| format!("{error:#}"));
                    answers.send_blocking(fetched).ok();
                }
            });
        if let Err(error) = spawned {
            self.problem = Some(format!("Cannot open the store: {error}"));
            return;
        }
        self.loading = Some(cx.spawn(async move |this, cx| {
            while let Ok(answer) = receiver.recv().await {
                let updated = this.update(cx, |this, cx| {
                    match answer {
                        Ok(catalog) => {
                            this.problem = None;
                            this.offers = catalog.offers;
                            this.page = None;
                            this.refilter();
                            this.find_owned();
                        }
                        Err(error) if this.offers.is_empty() => this.problem = Some(error),
                        Err(error) => {
                            cx.emit(StoreEvent::Notice(format!(
                                "Cannot update the store: {error}"
                            )));
                        }
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.loading = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Works out which games the library has: those installed in their
    /// store folders.
    fn find_owned(&mut self) {
        let Some(games) = &self.games else {
            return;
        };
        let folders: HashMap<PathBuf, &Offer> = self
            .offers
            .iter()
            .map(|offer| (store::install_dir(games, offer), offer))
            .collect();
        self.owned = self
            .library
            .iter()
            .filter_map(|path| {
                let offer = path.ancestors().find_map(|folder| folders.get(folder))?;
                Some((offer.id(), path.clone()))
            })
            .collect();
    }

    /// Works out which games show, after the search or a filter changed.
    fn refilter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.shown = self
            .offers
            .iter()
            .enumerate()
            .filter(|(_, offer)| {
                offer.kind == self.kind
                    && self
                        .platform
                        .is_none_or(|platform| offer.platform == platform)
                    && offer.matches(&query)
            })
            .map(|(index, _)| index)
            .collect();
    }

    /// Where the chosen game is among those shown.
    fn position(&self) -> usize {
        self.selected
            .as_ref()
            .and_then(|id| {
                self.shown
                    .iter()
                    .position(|&index| &self.offers[index].id() == id)
            })
            .unwrap_or(0)
    }

    fn choose(&mut self, at: usize) {
        self.selected = self.shown.get(at).map(|&index| self.offers[index].id());
        if let Some(row) = at.checked_div(self.columns) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
    }

    fn set_platform(&mut self, platform: Option<Platform>, cx: &mut Context<Self>) {
        self.platform = platform;
        self.selected = None;
        self.refilter();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.selected = None;
        self.refilter();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Opens the page of the game at `index` in `offers`.
    fn open_page(&mut self, index: usize, cx: &mut Context<Self>) {
        self.page = Some((index, 0));
        self.selected = Some(self.offers[index].id());
        self.page_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        let urls = self.offers[index].screenshots.clone();
        // The first shows first: asked for last.
        for url in urls.iter().rev() {
            self.want_picture(url);
        }
        cx.notify();
    }

    /// Shows the screenshot `step` along from the one showing, if there is
    /// one.
    fn turn_page(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some((index, shot)) = self.page else {
            return;
        };
        let count = self.offers[index].screenshots.len();
        let next = shot.saturating_add_signed(step);
        if next < count {
            self.page = Some((index, next));
            cx.notify();
        }
    }

    /// Gets the game at `index` in `offers`: downloads it and adds it to
    /// the library. Plays it if the library has it.
    fn get(&mut self, index: usize, cx: &mut Context<Self>) {
        let offer = self.offers[index].clone();
        let id = offer.id();
        if let Some(path) = self.owned.get(&id) {
            cx.emit(StoreEvent::Play(path.clone()));
            return;
        }
        let Some(games) = self.games.clone() else {
            cx.emit(StoreEvent::Notice(
                "There is no folder to keep games in".to_owned(),
            ));
            return;
        };
        if !self.getting.insert(id.clone()) {
            return;
        }
        let cache = self.cache.clone();
        let (answer, receiver) = async_channel::bounded(1);
        let spawned = thread::Builder::new()
            .name("store-get".to_owned())
            .spawn(move || {
                answer
                    .send_blocking(install(&offer, &games, cache.as_deref()))
                    .ok();
            });
        if let Err(error) = spawned {
            self.getting.remove(&id);
            cx.emit(StoreEvent::Notice(format!("Cannot get the game: {error}")));
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = receiver
                .recv()
                .await
                .unwrap_or_else(|_| Err("The download stopped".to_owned()));
            this.update(cx, |this, cx| {
                this.getting.remove(&id);
                match result {
                    Ok(games) => {
                        if let Some(game) = games.first() {
                            this.owned.insert(id, game.path.clone());
                            this.library
                                .extend(games.iter().map(|game| game.path.clone()));
                        }
                        cx.emit(StoreEvent::Added(games));
                    }
                    Err(error) => cx.emit(StoreEvent::Notice(error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Asks for the picture at `url`, unless it was asked for already,
    /// starting the downloaders the first time.
    fn want_picture(&mut self, url: &str) {
        let Some(cache) = &self.cache else {
            return;
        };
        if self.pictures.contains_key(url) {
            return;
        }
        self.pictures.insert(url.to_owned(), Picture::Coming);
        if !self.downloading {
            self.downloading = true;
            self.start_downloaders(cache.clone());
        }
        let mut urls = self
            .queue
            .urls
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        urls.push(url.to_owned());
        self.queue.more.notify_one();
    }

    /// Starts the threads that download pictures, latest asked for first,
    /// for as long as the view is there to take them.
    fn start_downloaders(&self, cache: PathBuf) {
        for number in 0..PICTURE_WORKERS {
            let queue = Arc::clone(&self.queue);
            let found = self.found.clone();
            let cache = cache.clone();
            thread::Builder::new()
                .name(format!("store-pictures-{number}"))
                .spawn(move || {
                    loop {
                        let url = {
                            let mut urls =
                                queue.urls.lock().unwrap_or_else(PoisonError::into_inner);
                            loop {
                                if let Some(url) = urls.pop() {
                                    break url;
                                }
                                urls = queue
                                    .more
                                    .wait(urls)
                                    .unwrap_or_else(PoisonError::into_inner);
                            }
                        };
                        let picture = match store::download_picture(&cache, &url) {
                            Ok(Some(path)) => Picture::Ready(path),
                            _ => Picture::Missing,
                        };
                        if found.send_blocking((url, picture)).is_err() {
                            return;
                        }
                    }
                })
                .ok();
        }
    }

    /// Handles a key while the store shows. Whether it was the store's.
    pub fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt {
            return false;
        }
        let key = keystroke.key.as_str();
        if let Some((index, _)) = self.page {
            match key {
                "escape" | "backspace" => self.page = None,
                "enter" if event.is_held => {}
                "enter" | "space" => self.get(index, cx),
                "left" => self.turn_page(-1, cx),
                "right" => self.turn_page(1, cx),
                _ => return false,
            }
            cx.notify();
            return true;
        }
        match key {
            "backspace" => {
                self.query.pop();
                self.selected = None;
                self.refilter();
            }
            "escape" if !self.query.is_empty() => {
                self.query.clear();
                self.selected = None;
                self.refilter();
            }
            "escape" if self.searching => self.searching = false,
            "escape" => cx.emit(StoreEvent::Close),
            "tab" => {
                let at = PLATFORMS
                    .iter()
                    .position(|&platform| platform == self.platform)
                    .unwrap_or(0);
                let next = if modifiers.shift {
                    (at + PLATFORMS.len() - 1) % PLATFORMS.len()
                } else {
                    (at + 1) % PLATFORMS.len()
                };
                self.set_platform(PLATFORMS[next], cx);
            }
            "enter" if event.is_held => {}
            "enter" => {
                if let Some(&index) = self.shown.get(self.position()) {
                    self.open_page(index, cx);
                }
            }
            key @ ("left" | "right" | "up" | "down") => {
                let at = moved(self.position(), self.shown.len(), self.columns, key);
                self.choose(at);
            }
            _ => match &keystroke.key_char {
                Some(text) if !text.is_empty() && text.chars().all(|c| !c.is_control()) => {
                    self.query.push_str(text);
                    self.searching = true;
                    self.selected = None;
                    self.refilter();
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                _ => return false,
            },
        }
        cx.notify();
        true
    }

    fn header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let refreshing = self.loading.is_some();
        div()
            .h(px(TOP_BAR))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(TRAFFIC_LIGHTS))
            // The refresh icon's artwork reaches further out in its box than
            // the library's add icon, which `HEADER_END` is set by.
            .pr(px(HEADER_END + 1.5))
            .gap_1()
            .children(probe::part("header"))
            // A game's page goes back to the grid; the tabs go between the
            // store and the library.
            .when(self.page.is_some(), |bar| {
                bar.child(
                    icons::button("back", icons::BACK)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.page = None;
                            cx.notify();
                        }))
                        .children(probe::part("button:back")),
                )
            })
            .child(ui::tabs([
                ui::tab(Section::Library, Section::Store, window)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(StoreEvent::Close))),
                ui::tab(Section::Store, Section::Store, window).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.page = None;
                        cx.notify();
                    },
                )),
            ]))
            .child(div().flex_1().flex().items_center().pl_2().gap_2().when(
                refreshing && !self.offers.is_empty(),
                |bar| {
                    bar.child(
                        div()
                            .text_xs()
                            .text_color(rgb(DIM))
                            .child("Updating…")
                            .with_animation(
                                "updating",
                                Animation::new(Duration::from_millis(1400)).repeat(),
                                |text, delta| text.opacity(0.45 + 0.55 * pulse(delta)),
                            ),
                    )
                },
            ))
            .child(
                icons::button("refresh", icons::REFRESH)
                    .on_click(cx.listener(|this, _, _, cx| this.load(true, cx)))
                    .children(probe::part("button:refresh")),
            )
            .into_any_element()
    }

    fn search_field(&self, cx: &mut Context<Self>) -> AnyElement {
        let placeholder = match self.offers.len() {
            0 => "Search: just type".to_owned(),
            count => format!("Search {} free games: just type", thousands(count)),
        };
        let text = if self.query.is_empty() {
            div().text_color(rgb(DIM)).child(placeholder)
        } else {
            div()
                .text_color(rgb(TEXT))
                .child(SharedString::from(self.query.clone()))
        };
        let caret = self.searching.then(|| {
            div().w(px(1.5)).h(px(16.0)).bg(rgb(ACCENT)).with_animation(
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
            .h(px(SEARCH_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px(px(SEARCH_PADDING))
            .rounded_lg()
            .bg(rgba(0xFFFF_FF12))
            .border_1()
            .border_color(rgba(if self.searching {
                with_alpha(ACCENT, 0xAA)
            } else {
                0
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

    /// The system and kind filters, and how many games show.
    fn filters(&self, cx: &mut Context<Self>) -> AnyElement {
        let platforms: Vec<_> = PLATFORMS
            .iter()
            .enumerate()
            .map(|(at, &platform)| {
                let label = platform.map_or("All", Platform::label);
                choice(("platform", at), label, self.platform == platform)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_platform(platform, cx)))
                    .children(probe::part(format!("segment:platform.{at}")))
            })
            .collect();
        let kinds: Vec<_> = Kind::ALL
            .iter()
            .enumerate()
            .map(|(at, &kind)| {
                choice(("kind", at), kind.plural(), self.kind == kind)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_kind(kind, cx)))
                    .children(probe::part(format!("segment:kind.{at}")))
            })
            .collect();
        let count = (!self.offers.is_empty()).then(|| {
            div()
                .text_xs()
                .text_color(rgb(DIM))
                .whitespace_nowrap()
                .child(match self.shown.len() {
                    1 => "1 shown".to_owned(),
                    count => format!("{} shown", thousands(count)),
                })
                .children(probe::part("count"))
        });
        div()
            .mx(px(INSET))
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_xs()
            .child(track("platform", platforms))
            .child(track("kind", kinds))
            .children(count)
            .into_any_element()
    }

    /// The rows of games in `rows`, for the grid.
    fn rows(
        &mut self,
        rows: Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let columns = self.columns.max(1);
        let chosen = self.position();
        // The pictures of the rows in view and the next, latest last, so
        // the top row's come first.
        let wanted: Vec<String> = rows
            .clone()
            .flat_map(|row| row * columns..((row + 2) * columns).min(self.shown.len()))
            .rev()
            .filter_map(|at| {
                let offer = &self.offers[self.shown[at]];
                offer.screenshots.first().cloned()
            })
            .collect();
        for url in wanted {
            self.want_picture(&url);
        }
        rows.map(|row| {
            let tiles: Vec<AnyElement> = (row * columns
                ..((row + 1) * columns).min(self.shown.len()))
                .map(|at| self.tile(at, at == chosen, window, cx))
                .collect();
            div()
                .flex()
                .gap(px(GAP))
                .px(px(INSET))
                .pb(px(GAP))
                .children(tiles)
                .into_any_element()
        })
        .collect()
    }

    /// The `at`th game shown, as a tile in the grid.
    fn tile(&self, at: usize, chosen: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let index = self.shown[at];
        let offer = &self.offers[index];
        let inner = self.tile - 2.0 * TILE_PADDING;
        let id = offer.id();
        let owned = self.owned.contains_key(&id);
        let getting = self.getting.contains(&id);
        let cover = cover(
            offer
                .screenshots
                .first()
                .and_then(|url| self.pictures.get(url)),
            offer.platform,
            inner,
            inner * COVER_SHAPE,
        );
        let badge = owned.then(|| {
            div()
                .absolute()
                .top(px(6.0))
                .right(px(6.0))
                .size(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgba(with_alpha(BACKGROUND, 0xCC)))
                .child(icons::icon(icons::CHECK, 16.0, TEXT))
        });
        let maker = if offer.developer.is_empty() {
            offer.source.name().to_owned()
        } else {
            offer.developer.clone()
        };
        // The button shows on the chosen game, under the pointer, and while
        // the game downloads.
        let button = self.tile_button(at, index, owned, getting, cx);
        let button = div()
            .absolute()
            .bottom(px(6.0))
            .right(px(6.0))
            .when(!chosen && !getting, |corner| {
                corner
                    .opacity(0.0)
                    .group_hover("tile", |corner| corner.opacity(1.0))
            })
            .child(button);
        div()
            .id(("offer", at))
            .group("tile")
            .w(px(self.tile))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(TILE_PADDING))
            .rounded_lg()
            .cursor_pointer()
            .when(chosen, |tile| tile.bg(rgba(CHOSEN)))
            .when(!chosen, |tile| tile.hover(|tile| tile.bg(rgba(HOVER))))
            .on_click(cx.listener(move |this, _, _, cx| this.open_page(index, cx)))
            .children(probe::part(format!("offer:{at}")))
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(inner * COVER_SHAPE))
                    .child(cover)
                    .children(badge)
                    .child(button),
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
                                .child(fit(
                                    &offer.title,
                                    inner,
                                    13.0,
                                    gpui::FontWeight::MEDIUM,
                                    window,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .h(px(MAKER_LINE))
                                .text_size(px(11.0))
                                .line_height(px(MAKER_LINE))
                                .text_color(rgb(DIM))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(fit(
                                    &format!("{} · {maker}", offer.platform.label()),
                                    inner,
                                    11.0,
                                    gpui::FontWeight::NORMAL,
                                    window,
                                    cx,
                                )),
                        ),
                ),
            )
            .into_any_element()
    }

    /// A tile's button: Get, Play, or a pulse while the game downloads.
    fn tile_button(
        &self,
        at: usize,
        index: usize,
        owned: bool,
        getting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (label, icon) = if owned {
            ("Play", icons::PLAY)
        } else {
            ("Get", icons::DOWNLOAD)
        };
        let pill = div()
            .id(("get", at))
            .flex_none()
            .h(px(PILL))
            .flex()
            .items_center()
            .gap(px(4.0))
            .pl(px(7.0))
            .pr(px(9.0))
            .rounded_full()
            .text_size(px(11.0))
            .font_weight(gpui::FontWeight::SEMIBOLD);
        if getting {
            return pill
                .bg(rgba(with_alpha(BACKGROUND, 0xE6)))
                .text_color(rgb(DIM))
                .pl(px(9.0))
                .child("Getting")
                .with_animation(
                    ("getting", at),
                    Animation::new(Duration::from_millis(1100)).repeat(),
                    |pill, delta| pill.opacity(0.4 + 0.6 * pulse(delta)),
                )
                .into_any_element();
        }
        pill.cursor_pointer()
            .bg(rgba(with_alpha(BACKGROUND, 0xE6)))
            .text_color(rgb(TEXT))
            .hover(|pill| pill.bg(rgb(ACCENT)).text_color(rgb(ON_ACCENT)))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.selected = Some(this.offers[index].id());
                this.get(index, cx);
            }))
            .child(icons::icon(icon, 12.0, TEXT))
            .child(label)
            .into_any_element()
    }

    /// The page of the game at `index` in `offers`, showing its `shot`th
    /// screenshot.
    fn page(
        &self,
        index: usize,
        shot: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let offer = &self.offers[index];
        let id = offer.id();
        let width = (f32::from(window.viewport_size().width) - 2.0 * INSET).min(PAGE_WIDTH);
        let shot_height = (width * COVER_SHAPE).min(SHOT_HEIGHT);
        let picture = offer
            .screenshots
            .get(shot)
            .and_then(|url| self.pictures.get(url));
        let shots = offer.screenshots.len();
        let arrows = (shots > 1).then(|| {
            let arrow = |id: &'static str, step: isize, enabled: bool, flip: bool| {
                div()
                    .id(id)
                    .size(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgba(with_alpha(BACKGROUND, 0xB0)))
                    .when(!enabled, |arrow| arrow.opacity(0.3))
                    .when(enabled, |arrow| {
                        arrow
                            .cursor_pointer()
                            .hover(|arrow| arrow.bg(rgba(with_alpha(BACKGROUND, 0xEE))))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.turn_page(step, cx)))
                    .child(if flip {
                        icons::icon(icons::BACK, 16.0, TEXT)
                            .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                std::f32::consts::PI,
                            )))
                            .into_any_element()
                    } else {
                        icons::icon(icons::BACK, 16.0, TEXT).into_any_element()
                    })
            };
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_between()
                .px(px(8.0))
                .child(arrow("shot-before", -1, shot > 0, false))
                .child(arrow("shot-after", 1, shot + 1 < shots, true))
        });
        let dots = (shots > 1).then(|| {
            div()
                .flex()
                .justify_center()
                .gap(px(6.0))
                .children((0..shots.min(12)).map(|at| {
                    div().size(px(6.0)).rounded_full().bg(rgba(if at == shot {
                        0xFFFF_FFCC
                    } else {
                        0xFFFF_FF33
                    }))
                }))
        });
        let owned = self.owned.contains_key(&id);
        let getting = self.getting.contains(&id);
        let action = {
            let (label, icon) = match (owned, getting) {
                (true, _) => ("Play", icons::PLAY),
                (false, true) => ("Getting…", icons::DOWNLOAD),
                (false, false) => ("Get", icons::DOWNLOAD),
            };
            let button = div()
                .id("get")
                .h(px(36.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .px(px(20.0))
                .rounded_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(icons::icon(
                    icon,
                    16.0,
                    if getting { DIM } else { ON_ACCENT },
                ))
                .child(label)
                .children(probe::part("button:get"));
            if getting {
                button
                    .bg(rgba(0xFFFF_FF14))
                    .text_color(rgb(DIM))
                    .with_animation(
                        "getting-page",
                        Animation::new(Duration::from_millis(1100)).repeat(),
                        |button, delta| button.opacity(0.5 + 0.5 * pulse(delta)),
                    )
                    .into_any_element()
            } else {
                button
                    .cursor_pointer()
                    .bg(rgb(ACCENT))
                    .text_color(rgb(ON_ACCENT))
                    .hover(|button| button.opacity(0.9))
                    .on_click(cx.listener(move |this, _, _, cx| this.get(index, cx)))
                    .into_any_element()
            }
        };
        let facts = [
            ("System", Some(platform_name(offer.platform).to_owned())),
            (
                "Made by",
                Some(offer.developer.clone()).filter(|maker| !maker.is_empty()),
            ),
            ("Licence", offer.license.clone()),
            ("From", Some(offer.source.name().to_owned())),
            (
                "Checked",
                offer.tested.then(|| "Starts in ds-emulator".to_owned()),
            ),
        ];
        let facts = facts
            .into_iter()
            .filter_map(|(name, value)| Some((name, value?)))
            .map(|(name, value)| {
                div()
                    .flex()
                    .gap_3()
                    .py(px(6.0))
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .text_sm()
                    .child(
                        div()
                            .w(px(80.0))
                            .flex_none()
                            .text_color(rgb(DIM))
                            .child(name),
                    )
                    .child(div().flex_1().min_w_0().text_color(rgb(TEXT)).child(value))
            });
        let website = offer.website.clone().map(|url| {
            div()
                .id("website")
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(rgb(DIM))
                .cursor_pointer()
                .hover(|link| link.text_color(rgb(TEXT)))
                .child(icons::icon(icons::GLOBE, 14.0, DIM))
                .child(div().truncate().child(url.clone()))
                .on_click(move |_, _, cx| cx.open_url(&url))
        });
        let tags = (!offer.tags.is_empty()).then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.0))
                .children(offer.tags.iter().map(|tag| {
                    div()
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded_full()
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(LINE))
                        .text_xs()
                        .text_color(rgb(DIM))
                        .child(tag.clone())
                }))
        });
        let paragraphs = offer
            .description
            .split("\n\n")
            .map(str::trim)
            .filter(|paragraph| !paragraph.is_empty())
            .map(|paragraph| {
                div()
                    .text_sm()
                    .line_height(px(20.0))
                    .text_color(rgb(0x00C9_CDD3))
                    .child(paragraph.replace('\n', " "))
            });
        div()
            .id("page")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.page_scroll)
            .child(
                div()
                    .mx_auto()
                    .w(px(width))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .pt(px(4.0))
                    .pb(px(INSET * 2.0))
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(shot_height))
                            .child(cover(picture, offer.platform, width, shot_height))
                            .children(arrows)
                            .children(probe::part("page.shot")),
                    )
                    .children(dots)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(TEXT))
                                            .child(offer.title.clone())
                                            .children(probe::part("page.title")),
                                    )
                                    .child(div().text_sm().text_color(rgb(DIM)).truncate().child(
                                        match offer.developer.as_str() {
                                            "" => platform_name(offer.platform).to_owned(),
                                            maker => format!(
                                                "{maker} · {}",
                                                platform_name(offer.platform)
                                            ),
                                        },
                                    )),
                            )
                            .child(action),
                    )
                    .children(tags)
                    .children(paragraphs)
                    .child(div().flex().flex_col().children(facts))
                    .children(website),
            )
            .into_any_element()
    }

    /// What shows while the store has no games: fetching, or why it
    /// cannot.
    fn waiting(&self, cx: &mut Context<Self>) -> AnyElement {
        let (title, hint) = match &self.problem {
            Some(problem) => ("Cannot reach the store", problem.clone()),
            None if self.loading.is_some() => (
                "Opening the store…",
                "Free homebrew from Homebrew Hub and Universal-DB".to_owned(),
            ),
            None => (
                "No games match",
                "Press Escape to clear the search, or try another system.".to_owned(),
            ),
        };
        let retry = self.problem.is_some().then(|| {
            div()
                .id("retry")
                .mt_2()
                .px(px(16.0))
                .h(px(32.0))
                .flex()
                .items_center()
                .rounded_lg()
                .cursor_pointer()
                .bg(rgb(ACCENT))
                .text_color(rgb(ON_ACCENT))
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .hover(|button| button.opacity(0.9))
                .child("Try Again")
                .on_click(cx.listener(|this, _, _, cx| this.load(true, cx)))
                .children(probe::part("button:retry"))
        });
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px(px(INSET))
            .child(
                div()
                    .text_xl()
                    .text_color(rgb(TEXT))
                    .child(title)
                    .children(probe::part("empty.title")),
            )
            .child(
                div()
                    .max_w(px(360.0))
                    .text_size(px(14.0))
                    .text_center()
                    .text_color(rgb(DIM))
                    .child(hint),
            )
            .children(retry)
            .into_any_element()
    }
}

impl gpui::Render for StoreView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        let (columns, tile) = grid(width - 2.0 * INSET);
        self.columns = columns;
        self.tile = tile;
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(linear_gradient(
                180.0,
                linear_color_stop(rgb(BACKGROUND), 0.3),
                linear_color_stop(rgb(FOOT), 1.0),
            ))
            .child(self.header(window, cx));
        if let Some((index, shot)) = self.page {
            return root.child(self.page(index, shot, window, cx));
        }
        let root = root
            .child(self.search_field(cx))
            .child(div().h(px(10.0)).flex_none())
            .child(self.filters(cx))
            .child(div().h(px(12.0)).flex_none());
        if self.shown.is_empty() {
            return root.child(self.waiting(cx));
        }
        let rows = self.shown.len().div_ceil(columns);
        root.child(
            div()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "store",
                        rows,
                        cx.processor(|this, range: Range<usize>, window, cx| {
                            this.rows(range, window, cx)
                        }),
                    )
                    .size_full()
                    .track_scroll(self.scroll.clone()),
                )
                .children(probe::part("grid")),
        )
    }
}

/// Downloads `offer` into the games folder `games` and reads its games,
/// titled as the store titles it and with its cover as their box art.
fn install(offer: &Offer, games: &Path, cache: Option<&Path>) -> Result<Vec<Game>, String> {
    let paths = store::install(offer, games)?;
    let cover = cache.and_then(|cache| store::download_cover(cache, offer).ok().flatten());
    let mut added = Vec::new();
    for (at, path) in paths.iter().enumerate() {
        let mut game = read_game(path, cache)?;
        if at == 0 {
            game.title.clone_from(&offer.title);
        }
        // The store has named it, and no game database lists most
        // homebrew.
        game.looked_up = true;
        if let (Some(cache), Some(cover)) = (cache, &cover) {
            std::fs::read(cover)
                .and_then(|bytes| boxart::write(&boxart::art_path(cache, game.crc32), &bytes))
                .ok();
        }
        added.push(game);
    }
    Ok(added)
}

/// A picture of a game filling `width` by `height`, or while it comes or
/// if there is none, a plate naming its system.
fn cover(picture: Option<&Picture>, platform: Platform, width: f32, height: f32) -> AnyElement {
    let plate = div()
        .w(px(width))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(LINE));
    match picture {
        Some(Picture::Ready(path)) => plate
            .child(
                img(path.clone())
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .rounded(px(5.0)),
            )
            .into_any_element(),
        _ => plate
            .text_size(px(13.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(DIM))
            .child(platform.label())
            .into_any_element(),
    }
}

/// A system's full name, for a game's page.
pub fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Gb => "Game Boy",
        Platform::Gbc => "Game Boy Color",
        Platform::Gba => "Game Boy Advance",
        Platform::Nds => "Nintendo DS",
        Platform::N3ds => "Nintendo 3DS",
        Platform::RpgMaker => "RPG Maker",
    }
}

/// `count` with its thousands split by commas: `1,182`.
fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Up and down once each time `delta` goes from 0 to 1, from 0 to 1 and
/// back.
fn pulse(delta: f32) -> f32 {
    1.0 - (2.0 * delta - 1.0).abs()
}

/// How many tiles fit across `width` logical pixels, and how wide each is
/// so that they fill it.
fn grid(width: f32) -> (usize, f32) {
    // Whole tiles only; `as` saturates.
    let columns = (((width + GAP) / (MIN_TILE + GAP)).floor() as usize).max(1);
    let tile = (width - GAP * (columns - 1) as f32) / columns as f32;
    (columns, tile.max(MIN_TILE / 2.0))
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
    fn counts_split_their_thousands() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1182), "1,182");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn the_grid_fills_the_width_with_whole_tiles() {
        let (columns, tile) = grid(4.0 * MIN_TILE + 3.0 * GAP);
        assert_eq!(columns, 4);
        assert!((tile - MIN_TILE).abs() < 0.01);
        assert_eq!(grid(100.0).0, 1);
    }

    #[test]
    fn arrows_move_through_the_grid() {
        assert_eq!(moved(4, 7, 3, "left"), 3);
        assert_eq!(moved(6, 7, 3, "right"), 6);
        assert_eq!(moved(4, 7, 3, "up"), 1);
        assert_eq!(moved(1, 7, 3, "up"), 1);
        assert_eq!(moved(3, 7, 3, "down"), 6);
        assert_eq!(moved(4, 7, 3, "down"), 4);
    }

    #[test]
    fn a_pulse_rises_and_falls() {
        assert!(pulse(0.0).abs() < f32::EPSILON);
        assert!((pulse(0.5) - 1.0).abs() < f32::EPSILON);
        assert!(pulse(1.0).abs() < f32::EPSILON);
    }
}
