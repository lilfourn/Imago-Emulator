//! The randomize window, from File > Randomize Game…, a game's menu in
//! the library or the Randomizer page of a running game's menu: which of
//! the library's Pokémon games to randomize and how, then its randomized
//! run made with the Universal Pokémon Randomizer FVX (see `randomizer`),
//! which the game plays from then on while the run is switched on. The run
//! starts a new game, or goes on from a copy of the game's own save.
//!
//! Keyboard first: up and down (or W and S) choose the game, left and
//! right (or A and D) what changes, Tab whether it starts from the game's
//! save, ⌘V pastes a settings string from the randomizer's own window or a
//! guide, Return randomizes, then plays what was made, and Escape or ⌘W
//! closes the window.
//!
//! One game is made at a time. A run goes on if the window is closed, and
//! the main window says how it went.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, AnyWindowHandle, App, Bounds, Context, Entity,
    FocusHandle, KeyDownEvent, ScrollHandle, SharedString, WeakEntity, Window, WindowBounds,
    WindowHandle, WindowOptions, div, prelude::*, px, rgb, rgba, size,
};

use crate::app::{CloseGame, TOP_BAR, shortcut, top_bar};
use crate::chrome::{self, Tone};
use crate::icons;
use crate::library::Randomized;
use crate::library_view::{LibraryEvent, LibraryView};
use crate::probe;
use crate::randomizer::{self, Made, Preset};
use crate::romhack;
use crate::theme::{
    BACKGROUND, CHOSEN, HOVER, INSET, LINE, MUTED as DIM, ON_ACCENT, PANEL, TEXT, accent,
    with_alpha,
};
use crate::ui;
use crate::view::EmulatorView;

/// The window's size, in logical pixels.
const WIDTH: f32 = 520.0;
const HEIGHT: f32 = 560.0;
/// Size of the window's words.
const TEXT_SIZE: f32 = 14.0;
/// Height of a game's row in the list, and how many rows show before it
/// scrolls.
const ROW_HEIGHT: f32 = 36.0;
const ROWS_SHOWN: f32 = 4.0;
/// Corners of the list and of the rows in it, which share a centre.
const LIST_RADIUS: f32 = 10.0;
const LIST_PADDING: f32 = 4.0;
const ROW_RADIUS: f32 = LIST_RADIUS - LIST_PADDING - 1.0;
/// Height of the buttons at the foot, and the room the status beside them
/// keeps: two lines, so the foot stays put as it changes.
const BUTTON_HEIGHT: f32 = 32.0;
const STATUS_HEIGHT: f32 = 45.0;
/// Side of an icon in a button.
const ICON: f32 = 14.0;

/// Whether a game is being made, from any randomize window: one at a
/// time, as Java may take 4.6 GB for each.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Holds [`RUNNING`] while a game is being made, however that ends.
struct Running;

impl Running {
    /// Starts a run, unless one is under way.
    fn start() -> Option<Self> {
        (!RUNNING.swap(true, Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

/// What the window, or the main window once it is closed, says of the
/// run just made for the game called `title`.
fn made_text(title: &str, seed: u32) -> String {
    format!("{title} is randomized, seed {seed}. Switch it off in its menu.")
}

/// A game in the library the randomizer can randomize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    path: PathBuf,
    title: String,
    generation: u8,
    /// The seed of its randomized run, if it has one.
    seed: Option<u32>,
    /// Whether it has a battery save of its own that holds something, for
    /// a run to go on from.
    saved: bool,
}

/// A game the library has, for [`candidates`]: its path, its title and
/// the seed of its randomized run, if it has one.
pub type LibraryGame = (PathBuf, String, Option<u32>);

/// The library's games, for [`candidates`].
pub fn library_games(library: &LibraryView) -> Vec<LibraryGame> {
    library
        .checksums()
        .into_iter()
        .filter_map(|(_, path)| {
            let game = library.game_of(&path)?;
            let seed = game.randomized.map(|run| run.seed);
            Some((path, game.title, seed))
        })
        .collect()
}

/// Which of `games` the randomizer can randomize, by generation and then
/// title. Each one's header says, and its folder whether it has a save, so
/// this blocks; run it off the UI thread.
pub fn candidates(games: Vec<LibraryGame>) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = games
        .into_iter()
        .filter_map(|(path, title, seed)| {
            let generation = randomizer::generation(&path)?;
            let saved = randomizer::has_save(&path);
            Some(Candidate {
                path,
                title,
                generation,
                seed,
                saved,
            })
        })
        .collect();
    candidates.sort_by_cached_key(|game| (game.generation, game.title.to_lowercase()));
    candidates
}

/// Where the game at `path` is among `candidates`, or the first if it
/// isn't there.
fn index_of(candidates: &[Candidate], path: Option<&PathBuf>) -> usize {
    path.and_then(|path| candidates.iter().position(|game| game.path == *path))
        .unwrap_or(0)
}

impl Candidate {
    /// Its system and generation, as its row says them, and the seed of
    /// its randomized run if it has one: `DS · Gen 4 · Seed 336933`.
    fn detail(&self) -> String {
        let system = match self.generation {
            1 => "GB",
            2 => "GBC",
            3 => "GBA",
            _ => "DS",
        };
        let detail = format!("{system} · Gen {}", self.generation);
        match self.seed {
            Some(seed) => format!("{detail} · Seed {seed}"),
            None => detail,
        }
    }
}

/// What changes: one of the presets, or settings the player pasted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Changes {
    Preset(Preset),
    /// A settings string, ready for FVX.
    Pasted(String),
}

/// Where the window is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Reading which of the library's games can be randomized.
    Reading,
    /// Choosing, with what went wrong last, if anything did.
    Choosing {
        problem: Option<String>,
    },
    /// Downloading the randomizer, the first time.
    Downloading,
    Randomizing,
    /// Made this run, which its game plays now, as this says.
    Made(Made, String),
}

/// The randomize window's root view.
pub struct RandomizeWindow {
    main: WeakEntity<EmulatorView>,
    /// The main window, which plays what is made.
    main_window: AnyWindowHandle,
    library: Entity<LibraryView>,
    candidates: Vec<Candidate>,
    /// The chosen game, by its index in `candidates`.
    chosen: usize,
    changes: Changes,
    /// The last settings pasted, kept while a preset is chosen.
    pasted: Option<String>,
    /// Whether the run goes on from a copy of the game's own save, for a
    /// game that has one, rather than starting a new game.
    with_save: bool,
    state: State,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

impl RandomizeWindow {
    /// Opens a randomize window over `main`, the view of `main_window`,
    /// offering `candidates`, the games in `library` it can randomize (see
    /// [`candidates`]), with `game` chosen if given.
    pub fn open(
        main: &Entity<EmulatorView>,
        main_window: AnyWindowHandle,
        library: Entity<LibraryView>,
        candidates: Vec<Candidate>,
        game: Option<PathBuf>,
        cx: &mut App,
    ) -> anyhow::Result<WindowHandle<Self>> {
        let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(top_bar("Randomize")),
            is_resizable: false,
            is_minimizable: false,
            app_id: Some("ds-emulator".to_owned()),
            window_decorations: chrome::window_decorations(),
            ..WindowOptions::default()
        };
        let main = main.clone();
        cx.open_window(options, move |window, cx| {
            cx.new(|cx| Self::new(&main, main_window, library, candidates, game, window, cx))
        })
    }

    fn new(
        main: &Entity<EmulatorView>,
        main_window: AnyWindowHandle,
        library: Entity<LibraryView>,
        candidates: Vec<Candidate>,
        game: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        crate::x11_redraw::ensure_redraws();
        // The window goes when the game's window does.
        cx.observe_release_in(main, window, |_, _, window, _| window.remove_window())
            .detach();
        let chosen = index_of(&candidates, game.as_ref());
        let scroll = ScrollHandle::new();
        scroll.scroll_to_item(chosen);
        Self {
            main: main.downgrade(),
            main_window,
            library,
            candidates,
            chosen,
            changes: Changes::Preset(Preset::Classic),
            pasted: None,
            with_save: false,
            state: State::Choosing {
                problem: unsupported(),
            },
            focus,
            scroll,
        }
    }

    /// Reads the library's games again, as the window comes forward, with
    /// `game` chosen if given, or else the game that was. Not while a game
    /// is being made.
    pub fn refresh(&mut self, game: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let chosen = self.candidates.get(self.chosen);
        let wanted = game.or_else(|| chosen.map(|game| game.path.clone()));
        self.state = State::Reading;
        cx.notify();
        let games = library_games(self.library.read(cx));
        let read = cx
            .background_executor()
            .spawn(async move { candidates(games) });
        cx.spawn(async move |this, cx| {
            let candidates = read.await;
            this.update(cx, |this, cx| {
                this.chosen = index_of(&candidates, wanted.as_ref());
                this.candidates = candidates;
                this.scroll.scroll_to_item(this.chosen);
                this.state = State::Choosing {
                    problem: unsupported(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn busy(&self) -> bool {
        matches!(
            self.state,
            State::Reading | State::Downloading | State::Randomizing
        )
    }

    /// Chooses the game at `index`, and leaves what was made behind.
    fn choose_game(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.busy() || index >= self.candidates.len() {
            return;
        }
        self.chosen = index;
        self.scroll.scroll_to_item(index);
        self.state = State::Choosing { problem: None };
        cx.notify();
    }

    /// Chooses what changes, and leaves what was made behind.
    fn choose_changes(&mut self, changes: Changes, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.changes = changes;
        self.state = State::Choosing { problem: None };
        cx.notify();
    }

    /// Chooses whether the run goes on from the game's own save.
    fn choose_start(&mut self, with_save: bool, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.with_save = with_save;
        self.state = State::Choosing { problem: None };
        cx.notify();
    }

    /// Whether the chosen game has a save for its run to go on from.
    fn chosen_saved(&self) -> bool {
        let chosen = self.candidates.get(self.chosen);
        chosen.is_some_and(|game| game.saved)
    }

    /// The choices of what changes: the presets, then the pasted
    /// settings once there are some.
    fn change_choices(&self) -> Vec<Changes> {
        let presets = Preset::ALL.into_iter().map(Changes::Preset);
        presets
            .chain(self.pasted.clone().map(Changes::Pasted))
            .collect()
    }

    /// Takes a settings string from the clipboard, if it holds one, and
    /// chooses it; or says why it is not one.
    fn paste(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        match randomizer::pasted_settings(&text) {
            Ok(settings) => {
                self.pasted = Some(settings.clone());
                self.choose_changes(Changes::Pasted(settings), cx);
            }
            Err(problem) => {
                self.state = State::Choosing {
                    problem: Some(problem),
                };
                cx.notify();
            }
        }
    }

    /// Randomizes the chosen game: downloads the randomizer first if need
    /// be, then makes its randomized run, which the game plays from then
    /// on. A run it had before is left as it is if it was played, and
    /// removed if it never was. The work goes on if the window is closed.
    fn randomize(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        // Said at once, not after a moment of "Downloading…".
        if let Some(why) = randomizer::UNSUPPORTED {
            self.state = State::Choosing {
                problem: Some(why.to_owned()),
            };
            cx.notify();
            return;
        }
        let Some(game) = self.candidates.get(self.chosen).cloned() else {
            return;
        };
        let (Some(data), Some(hacks)) = (randomizer::data_dir(), romhack::hacks_dir()) else {
            self.state = State::Choosing {
                problem: Some("There is no folder to keep the randomizer in".to_owned()),
            };
            cx.notify();
            return;
        };
        let Some(running) = Running::start() else {
            self.state = State::Choosing {
                problem: Some("The randomizer is still making another game".to_owned()),
            };
            cx.notify();
            return;
        };
        let settings = match &self.changes {
            Changes::Preset(preset) => preset.settings().to_owned(),
            Changes::Pasted(settings) => settings.clone(),
        };
        let seed = randomizer::new_seed();
        let with_save = self.with_save && game.saved;
        self.state = if randomizer::installed(&data).is_some() {
            State::Randomizing
        } else {
            State::Downloading
        };
        cx.notify();
        let library = self.library.clone();
        let background = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let made = async {
                let randomizer = background
                    .spawn(async move { randomizer::ensure(&data) })
                    .await?;
                this.update(cx, |this, cx| {
                    this.state = State::Randomizing;
                    cx.notify();
                })
                .ok();
                let (base, title, settings) =
                    (game.path.clone(), game.title.clone(), settings.clone());
                background
                    .spawn(async move {
                        let made = randomizer::randomize(
                            &randomizer,
                            &base,
                            &title,
                            &settings,
                            seed,
                            &hacks,
                        )?;
                        // Its cheats and bindings, and its save if asked.
                        randomizer::carry_over(&base, &made.rom, with_save).map_err(|error| {
                            format!(
                                "Cannot copy the game's own files to its randomized run: {error}"
                            )
                        })?;
                        Ok::<Made, String>(made)
                    })
                    .await
            }
            .await;
            if let Ok(made) = &made {
                let run = Randomized {
                    image: made.rom.clone(),
                    seed,
                    settings: Some(settings),
                    on: true,
                };
                let before = library
                    .update(cx, |library, cx| {
                        let before = library.set_randomized(&game.path, run, cx);
                        // Taken out of the library meanwhile: a game of
                        // its own, as a hack is.
                        if before.is_err() {
                            library.add(vec![made.rom.clone()], cx);
                        }
                        before.ok().flatten()
                    })
                    .ok()
                    .flatten();
                // The run it had: gone if it was never played.
                if let Some(before) = before.filter(|before| before.image != made.rom) {
                    background
                        .spawn(async move { randomizer::remove_unplayed(&before.image) })
                        .detach();
                }
            }
            let said = match &made {
                Ok(_) => made_text(&game.title, seed),
                Err(problem) => problem.clone(),
            };
            let shown = this.update(cx, |this, cx| {
                this.state = match made {
                    Ok(made) => {
                        // Its row says the seed it has now.
                        let made_for = |candidate: &&mut Candidate| candidate.path == game.path;
                        if let Some(candidate) = this.candidates.iter_mut().find(made_for) {
                            candidate.seed = Some(seed);
                        }
                        State::Made(made, made_text(&game.title, seed))
                    }
                    Err(problem) => State::Choosing {
                        problem: Some(problem),
                    },
                };
                cx.notify();
            });
            // The window was closed while the game was made: the main
            // window says how it went.
            if shown.is_err() {
                library
                    .update(cx, |_, cx| cx.emit(LibraryEvent::Notice(said)))
                    .ok();
            }
            drop(running);
        })
        .detach();
    }

    /// Plays the game just made in the main window, and closes this one.
    fn play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (State::Made(made, _), Some(main)) = (&self.state, self.main.upgrade()) else {
            return;
        };
        let rom = made.rom.clone();
        self.main_window
            .update(cx, |_, main_window, cx| {
                main_window.activate_window();
                main.update(cx, |main, cx| main.play(rom, main_window, cx));
            })
            .ok();
        window.remove_window();
    }

    /// Opens the spoiler log of the game just made in the text editor.
    fn open_log(&self) {
        if let State::Made(Made { log: Some(log), .. }, _) = &self.state {
            emu_os::process::open_in_text_editor(log).ok();
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.secondary() && keystroke.key == "v" {
            self.paste(cx);
            return;
        }
        // Other Command and Control chords are menu shortcuts.
        if modifiers.platform || modifiers.control || modifiers.alt {
            return;
        }
        match keystroke.key.as_str() {
            // Return held through a quick run would play what it made.
            "enter" if event.is_held => {}
            "escape" => window.remove_window(),
            "enter" if matches!(self.state, State::Made(..)) => self.play(window, cx),
            "tab" if self.chosen_saved() => self.choose_start(!self.with_save, cx),
            "enter" => self.randomize(cx),
            "up" | "w" => self.choose_game(self.chosen.saturating_sub(1), cx),
            "down" | "s" => self.choose_game(self.chosen + 1, cx),
            key @ ("left" | "a" | "right" | "d") => {
                let choices = self.change_choices();
                let at = choices.iter().position(|changes| *changes == self.changes);
                let moved = match (key, at) {
                    ("left" | "a", Some(at)) => at.checked_sub(1),
                    (_, Some(at)) => Some(at + 1),
                    (_, None) => Some(0),
                };
                if let Some(changes) = moved.and_then(|at| choices.get(at)) {
                    self.choose_changes(changes.clone(), cx);
                }
            }
            _ => {}
        }
    }

    /// The list of games to choose from.
    fn games(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy();
        let rows = self.candidates.iter().enumerate().map(|(index, game)| {
            let chosen = index == self.chosen;
            div()
                .id(("game", index))
                .flex_none()
                .h(px(ROW_HEIGHT))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap_3()
                .rounded(px(ROW_RADIUS))
                .when(chosen, |row| row.bg(rgba(CHOSEN)))
                .when(!chosen && !busy, |row| {
                    row.cursor_pointer().hover(|row| row.bg(rgba(HOVER)))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.choose_game(index, cx)))
                .child(
                    // A dot marks the one chosen, as a radio button does.
                    div()
                        .flex_none()
                        .size(px(8.0))
                        .rounded_full()
                        .bg(rgba(if chosen { with_alpha(TEXT, 0xFF) } else { 0 }))
                        .border_1()
                        .border_color(rgb(if chosen { TEXT } else { DIM })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(rgb(if chosen { TEXT } else { DIM }))
                        .child(game.title.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(DIM))
                        .child(game.detail()),
                )
                .children(probe::part(format!("game:{index}")))
        });
        div()
            .id("games")
            .flex_none()
            .max_h(px(ROWS_SHOWN * ROW_HEIGHT + 2.0 * LIST_PADDING + 2.0))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .p(px(LIST_PADDING))
            .rounded(px(LIST_RADIUS))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(LINE))
            .children(rows)
            .children(probe::bordered_part("games", 1.0))
            .into_any_element()
    }

    /// The choices of what changes, and what the chosen one does.
    fn changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let choices = self
            .change_choices()
            .into_iter()
            .enumerate()
            .map(|(at, changes)| {
                let label = match &changes {
                    Changes::Preset(preset) => preset.label(),
                    Changes::Pasted(_) => "Pasted",
                };
                let chosen = changes == self.changes;
                ui::choice(("changes", at), label, chosen).on_click(
                    cx.listener(move |this, _, _, cx| this.choose_changes(changes.clone(), cx)),
                )
            })
            .collect();
        let description: SharedString = match &self.changes {
            Changes::Preset(preset) => preset.description().into(),
            Changes::Pasted(settings) => format!(
                "The settings pasted, {}. Whatever they say changes.",
                short(settings)
            )
            .into(),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().child(ui::track("changes", choices)))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(DIM))
                    .child(description)
                    .children(probe::part("description")),
            )
            .into_any_element()
    }

    /// Whether the run starts a new game or goes on from the chosen
    /// game's own save. A game with no save can only start a new one: the
    /// other choice is dimmed.
    fn start(&self, cx: &mut Context<Self>) -> AnyElement {
        let saved = self.chosen_saved();
        let choices = [("New Game", false), ("Copy My Save", true)]
            .into_iter()
            .enumerate()
            .map(|(at, (label, with_save))| {
                let chosen = with_save == (self.with_save && saved);
                let choice = ui::choice(("start", at), label, chosen);
                if with_save && !saved {
                    return choice.opacity(0.4).cursor_default();
                }
                choice.on_click(cx.listener(move |this, _, _, cx| this.choose_start(with_save, cx)))
            })
            .collect();
        div()
            .flex()
            .child(ui::track("start", choices))
            .into_any_element()
    }

    /// What is happening, or went wrong, at the foot of the window.
    fn status(&self) -> AnyElement {
        let (text, colour, pulsing): (SharedString, u32, bool) = match &self.state {
            State::Reading => ("Reading your games…".into(), DIM, true),
            State::Choosing {
                problem: Some(problem),
            } => (problem.clone().into(), TEXT, false),
            State::Choosing { problem: None } => {
                let chosen = self.candidates.get(self.chosen);
                let text = match chosen.and_then(|game| game.seed) {
                    Some(seed) => {
                        format!("Takes the place of seed {seed}, which is kept if it was played.")
                    }
                    None => {
                        "Your game keeps its own save. Trade evolutions happen by level.".to_owned()
                    }
                };
                (text.into(), DIM, false)
            }
            State::Downloading => (
                format!(
                    "Downloading the randomizer, FVX {} ({} MB)…",
                    randomizer::VERSION,
                    randomizer::DOWNLOAD_MB
                )
                .into(),
                DIM,
                true,
            ),
            State::Randomizing => ("Randomizing…".into(), DIM, true),
            State::Made(_, said) => (said.clone().into(), TEXT, false),
        };
        let words = div()
            .id("status")
            .flex_1()
            .min_w_0()
            .min_h(px(STATUS_HEIGHT))
            // A column, so the words, a block across it, wrap; centred in
            // the room kept for two lines.
            .flex()
            .flex_col()
            .justify_center()
            .text_sm()
            .text_color(rgb(colour))
            .child(div().line_clamp(2).text_ellipsis().child(text))
            .children(probe::part("status"));
        if pulsing {
            words
                .with_animation(
                    "working",
                    Animation::new(Duration::from_millis(1100)).repeat(),
                    |words, delta| words.opacity(0.45 + 0.55 * (1.0 - (2.0 * delta - 1.0).abs())),
                )
                .into_any_element()
        } else {
            words.into_any_element()
        }
    }

    /// The buttons at the foot: Randomize, or once made, Spoiler Log and
    /// Play.
    fn buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let made = match &self.state {
            State::Made(made, _) => Some(made),
            _ => None,
        };
        let ready = !self.busy() && !self.candidates.is_empty();
        let primary = |id: &'static str, label: &'static str, icon: Option<&'static str>| {
            div()
                .id(id)
                .flex_none()
                .h(px(BUTTON_HEIGHT))
                // As the library's Resume button.
                .pl(px(if icon.is_some() { 12.0 } else { 16.0 }))
                .pr(px(16.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .rounded_lg()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .bg(rgb(accent()))
                .text_color(rgb(ON_ACCENT))
                .children(icon.map(|icon| icons::icon(icon, ICON, ON_ACCENT)))
                .child(label)
                .children(probe::part(format!("button:{id}")))
        };
        let buttons = div().flex_none().flex().items_center().gap_2();
        if let Some(made) = made {
            let log = made.log.is_some().then(|| {
                div()
                    .id("log")
                    .flex_none()
                    .h(px(BUTTON_HEIGHT))
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .rounded_lg()
                    .cursor_pointer()
                    .text_sm()
                    .border_1()
                    .border_color(rgb(LINE))
                    .hover(|button| button.bg(rgba(HOVER)))
                    .child("Spoiler Log")
                    .on_click(cx.listener(|this, _, _, _| this.open_log()))
                    .children(probe::bordered_part("button:log", 1.0))
            });
            let play = primary("play", "Play", Some(icons::PLAY))
                .cursor_pointer()
                .hover(|button| button.opacity(0.9))
                .on_click(cx.listener(|this, _, window, cx| this.play(window, cx)));
            return buttons.children(log).child(play).into_any_element();
        }
        let chosen = self.candidates.get(self.chosen);
        let label = if chosen.is_some_and(|game| game.seed.is_some()) {
            "New Seed"
        } else {
            "Randomize"
        };
        let randomize = primary("randomize", label, None)
            .when(ready, |button| {
                button
                    .cursor_pointer()
                    .hover(|button| button.opacity(0.9))
                    .on_click(cx.listener(|this, _, _, cx| this.randomize(cx)))
            })
            .when(!ready, |button| button.opacity(0.4));
        buttons.child(randomize).into_any_element()
    }

    /// The foot of the window: the status and the buttons, and the keys
    /// under them, `hint`.
    fn foot(&self, hint: String, cx: &mut Context<Self>) -> AnyElement {
        let foot = div()
            .pt_4()
            .border_t_1()
            .border_color(rgb(LINE))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(self.status())
                    .child(self.buttons(cx)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(DIM))
                    .child(hint)
                    .children(probe::part("hint")),
            );
        // Its rule stops as far in from the sides as everything else.
        div()
            .flex_none()
            .mt_4()
            .px(px(INSET))
            .pb(px(INSET))
            .child(foot)
            .into_any_element()
    }

    /// What shows when the library has no game to randomize.
    fn no_games(&self) -> AnyElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px(px(INSET))
            .text_center()
            .child(
                div()
                    .text_color(rgb(TEXT))
                    .child("No Pokémon game to randomize"),
            )
            .child(
                // Two lines of its own: GPUI centres a line it wraps half a
                // space off.
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .text_sm()
                    .text_color(rgb(DIM))
                    .child("Add your own copy of a Pokémon game, from Red")
                    .child("to White 2, to the library, then come back.")
                    .children(probe::part("no-games")),
            )
            .into_any_element()
    }
}

/// `settings` shortened to its start and end, to tell pasted settings
/// apart by: `321AAgE…OIo=`.
fn short(settings: &str) -> String {
    let chars: Vec<char> = settings.chars().collect();
    if chars.len() <= 14 {
        return settings.to_owned();
    }
    let head: String = chars[..7].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// Why the randomizer can't run here, said as the window opens rather than
/// after a moment of downloading; `None` where it can.
fn unsupported() -> Option<String> {
    randomizer::UNSUPPORTED.map(str::to_owned)
}

/// The keys the window takes, once a game is `made` or before: ↩ and ⌘V
/// on macOS, Enter and Ctrl+V elsewhere. Tab is among them for a game
/// with a save of its own (`saved`).
fn hint(made: bool, saved: bool) -> String {
    let enter = if cfg!(target_os = "macos") {
        "↩".to_owned()
    } else {
        crate::keys::key("enter")
    };
    if made {
        format!("{enter} play · choose again to make another")
    } else {
        let paste = crate::keys::shortcut(shortcut::PASTE);
        let start = if saved { " · ⇥ start" } else { "" };
        format!("↑↓ game · ←→ what changes{start} · {paste} paste settings · {enter} randomize")
    }
}

/// A heading over a part of the window.
fn heading(title: &'static str) -> AnyElement {
    div()
        .text_xs()
        .text_color(rgb(DIM))
        .child(title)
        .children(probe::part(format!("heading:{title}")))
        .into_any_element()
}

impl gpui::Render for RandomizeWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // With no game to randomize, the window only says so: no status,
        // button or keys.
        let has_games = self.state == State::Reading || !self.candidates.is_empty();
        let body = if has_games {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_2()
                .px(px(INSET))
                .child(heading("Game"))
                .child(self.games(cx))
                .child(div().h(px(12.0)))
                .child(heading("What Changes"))
                .child(self.changes(cx))
                .child(div().h(px(12.0)))
                .child(heading("Start"))
                .child(self.start(cx))
                .into_any_element()
        } else {
            self.no_games()
        };
        let hint = hint(matches!(self.state, State::Made(..)), self.chosen_saved());
        let header = div()
            .h(px(TOP_BAR))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .text_color(rgb(DIM))
            .child("Randomize")
            .children(probe::part("header.title"));
        let header = chrome::header(header, false, window, cx);
        div()
            .track_focus(&self.focus)
            .key_context("Randomize")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .text_size(px(TEXT_SIZE))
            .on_key_down(cx.listener(Self::key_down))
            // ⌘W, which closes a game in the main window, comes here as
            // Close Game, ahead of any binding for this window alone.
            .on_action(cx.listener(|_, _: &CloseGame, window, _| window.remove_window()))
            .children(probe::begin())
            .child(header)
            .child(body)
            .when(has_games, |window| window.child(self.foot(hint, cx)))
            .children(chrome::window_edges(window, Tone::Paper, false))
            .children(probe::end("randomize"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_settings_are_shown_by_their_ends() {
        assert_eq!(
            short("321AAgEBQL///////8EAAIeJAIBFAAAFABABAABGAAAAAAAAAAAMgQyAQAICTIAAAAyOIo="),
            "321AAgE…OIo="
        );
        assert_eq!(short("428AAAA"), "428AAAA");
    }

    #[test]
    fn the_keys_are_named_as_each_system_names_them() {
        let (enter, paste) = if cfg!(target_os = "macos") {
            ("↩", "⌘V")
        } else {
            ("Enter", "Ctrl+V")
        };
        assert_eq!(
            hint(true, true),
            format!("{enter} play · choose again to make another")
        );
        assert_eq!(
            hint(false, false),
            format!("↑↓ game · ←→ what changes · {paste} paste settings · {enter} randomize")
        );
        // A game with a save of its own can start its run from it.
        assert_eq!(
            hint(false, true),
            format!(
                "↑↓ game · ←→ what changes · ⇥ start · {paste} paste settings · {enter} randomize"
            )
        );
    }

    #[test]
    fn the_games_offered_are_those_the_randomizer_knows_by_generation() {
        let dir = std::env::temp_dir().join(format!("emu-app-candidates-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let rom = |name: &str, at: usize, code: &[u8]| {
            let mut bytes = vec![0; 0x150];
            bytes[at..at + code.len()].copy_from_slice(code);
            let path = dir.join(name);
            std::fs::write(&path, bytes).expect("rom");
            path
        };
        let platinum = rom("Platinum.nds", 0x0C, b"CPUE");
        let emerald = rom("Emerald.gba", 0xAC, b"BPEE");
        let crystal = rom("Crystal.gbc", 0x134, b"PM_CRYSTAL");
        let golden_sun = rom("Golden Sun.gba", 0xAC, b"AGSE");
        // Emerald has a save beside it, and Platinum a randomized run.
        std::fs::write(dir.join("Emerald.sav"), [7; 8]).expect("save");
        // Crystal's is blank: it was never saved in.
        std::fs::write(dir.join("Crystal.sav"), [0xFF; 8]).expect("save");
        let games = vec![
            (platinum.clone(), "Platinum".to_owned(), Some(336_933)),
            (golden_sun, "Golden Sun".to_owned(), None),
            (dir.join("Gone.nds"), "Gone".to_owned(), None),
            (emerald.clone(), "Emerald".to_owned(), None),
            (crystal.clone(), "Crystal".to_owned(), None),
        ];
        let offered = candidates(games);
        let saved: Vec<bool> = offered.iter().map(|game| game.saved).collect();
        assert_eq!(
            saved,
            [false, true, false],
            "which has a save to go on from"
        );
        assert_eq!(offered[2].seed, Some(336_933));
        let paths: Vec<&PathBuf> = offered.iter().map(|game| &game.path).collect();
        assert_eq!(paths, [&crystal, &emerald, &platinum], "by generation");
        assert_eq!(index_of(&offered, Some(&platinum)), 2);
        assert_eq!(index_of(&offered, Some(&dir.join("Gone.nds"))), 0);
        assert_eq!(index_of(&offered, None), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rows_say_each_games_system_and_generation() {
        let game = |generation| Candidate {
            path: PathBuf::from("game"),
            title: "Pokémon".to_owned(),
            generation,
            seed: None,
            saved: false,
        };
        assert_eq!(game(1).detail(), "GB · Gen 1");
        assert_eq!(game(2).detail(), "GBC · Gen 2");
        assert_eq!(game(3).detail(), "GBA · Gen 3");
        assert_eq!(game(5).detail(), "DS · Gen 5");
        let randomized = Candidate {
            seed: Some(336_933),
            ..game(4)
        };
        assert_eq!(randomized.detail(), "DS · Gen 4 · Seed 336933");
        assert_eq!(
            made_text("Pokemon: Platinum Version", 336_933),
            "Pokemon: Platinum Version is randomized, seed 336933. Switch it off in its menu."
        );
    }
}
