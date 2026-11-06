//! Guide: the chat about the game being played, in the margin beside the
//! screens ([PRD](../../../../docs/PRD-game-chat.md)).
//!
//! [`GuidePanel`] is its own entity, drawn by the game view as a cached
//! view, so game frames never lay it out and streaming text never lays out
//! the game. Answers stream from OpenRouter on a worker thread
//! (`emu_guide::chat`); the panel commits what arrived every 120 ms,
//! parses only the answer's last block again, and fades new text in.

mod answer;
pub mod composer;
pub mod palette;
mod pictures;
mod render;
pub mod shots;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use emu_core::Frame;
use emu_guide::api::Endpoints;
use emu_guide::chat::{self, ChatError, Effort, Finish, Message, Request, Stop};
use emu_guide::conversation::{Answer, Chat, ChatStore, Ending, Summary, Turn};
use emu_guide::game::GameContext;
use emu_guide::markdown::Document;
use emu_guide::pictures::{PictureStore, Wiki};
use emu_guide::{prompt, sources};
use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    ListAlignment, ListOffset, ListState, RenderImage, Subscription, Task, Window, px,
};

use crate::capture;
use crate::openrouter::{OpenRouter, Status};
use crate::video;
use composer::{Composer, ComposerEvent};
use palette::Palette;

/// The narrowest the panel gets.
pub const MIN_WIDTH: f32 = 300.0;
/// The widest the panel gets.
pub const MAX_WIDTH: f32 = 440.0;
/// The panel's width when the margin is too narrow and the screens make
/// room for it.
pub const COLUMN_WIDTH: f32 = 360.0;
/// Screenshots a question may carry.
pub const MOST_SHOTS: usize = 4;
/// How often streamed text is shown.
const COMMIT: Duration = Duration::from_millis(120);
/// How often the palette may follow the game.
pub const PALETTE_EVERY: Duration = Duration::from_secs(2);
/// How long the palette takes to ease to the game's new colours.
const PALETTE_EASE: Duration = Duration::from_millis(600);
/// How long a fresh screenshot keeps its accent edge.
const FRESH: Duration = Duration::from_millis(900);
/// How long the copy button shows its tick.
const COPIED: Duration = Duration::from_millis(1500);

/// The panel's width in a margin `margin` wide beside the screens, or
/// `None` if it doesn't fit and the screens must make room.
pub fn width_in(margin: f32, inset: f32) -> Option<f32> {
    (margin >= MIN_WIDTH + 2.0 * inset).then(|| (margin - inset).clamp(MIN_WIDTH, MAX_WIDTH))
}

/// The game Guide is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuideGame {
    /// Names the game's chats folder: its system and checksum.
    pub key: String,
    /// What the model is told.
    pub context: GameContext,
    /// Its system's colour, `0xRRGGBB`, before it draws anything.
    pub tint: u32,
}

/// What the panel asks of the game view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuideEvent {
    /// Close the panel.
    Close,
    /// Give the keys back to the game.
    Blur,
    /// Take a screenshot for the composer.
    TakeShot,
    /// Take a screenshot, then ask this starter question.
    AskWithShot(&'static str),
    /// Say this in the window's notice: why a file dialog didn't open.
    Notice(String),
}

/// A screenshot waiting in the composer.
struct Shot {
    /// For the model, scaled.
    frame: Frame,
    /// Drawn in the composer and on the sent question.
    image: Arc<RenderImage>,
    taken: Instant,
}

/// A picture shown large over the panel: a screenshot, or an answer's
/// picture with its caption, its wiki's name and its page.
#[derive(Clone)]
struct Enlarged {
    image: Arc<RenderImage>,
    caption: Option<(String, String, String)>,
}

impl Enlarged {
    fn shot(image: Arc<RenderImage>) -> Self {
        Self {
            image,
            caption: None,
        }
    }
}

/// One row of the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Row {
    /// A question, with its screenshots.
    Question(usize),
    /// The thinking line, and the activity under it when opened.
    Thought(usize),
    /// One top-level block of an answer.
    Block(usize, usize),
    /// What follows an answer: its actions, or why it stopped.
    Ending(usize),
}

/// An answer streaming in.
struct Live {
    turn: usize,
    stop: Stop,
    started: Instant,
    /// The first text, which ends the thinking.
    first_text: Option<Instant>,
    /// OpenRouter said it was working: searching, while lookups are on.
    working: bool,
    /// Bytes of the answer handed to its document so far.
    committed: usize,
    last_commit: Instant,
    commit_soon: Option<Task<()>>,
    _pump: Task<()>,
}

/// The fade-in of newly streamed text.
#[derive(Debug, Clone, Default)]
pub(crate) struct Veil {
    /// Where each commit's text starts in the answer, and when it came.
    arrivals: Vec<(usize, Instant)>,
    /// How long a fade takes: three commit gaps, 120 to 400 ms.
    duration: Duration,
    /// The commit gap, smoothed.
    gap: f32,
}

impl Veil {
    /// How visible the text at byte `offset` is now, 0 to 1.
    pub(crate) fn alpha(&self, offset: usize, now: Instant) -> f32 {
        let Some((_, at)) = self
            .arrivals
            .iter()
            .rev()
            .find(|(start, _)| *start <= offset)
        else {
            return 1.0;
        };
        let p = (now.saturating_duration_since(*at).as_secs_f32()
            / self.duration.as_secs_f32().max(0.001))
        .clamp(0.0, 1.0);
        1.0 - (1.0 - p).powf(1.6)
    }

    /// Where the alpha changes in `range`: the arrivals inside it.
    pub(crate) fn cuts(&self, range: std::ops::Range<usize>) -> Vec<usize> {
        self.arrivals
            .iter()
            .map(|(start, _)| *start)
            .filter(|start| range.start < *start && *start < range.end)
            .collect()
    }

    fn settled(&self, now: Instant) -> bool {
        self.arrivals
            .last()
            .is_none_or(|(_, at)| now.saturating_duration_since(*at) >= self.duration)
    }

    fn arrive(&mut self, offset: usize, now: Instant) {
        if let Some((_, last)) = self.arrivals.last() {
            let gap = now.saturating_duration_since(*last).as_secs_f32() * 1000.0;
            self.gap = if self.gap == 0.0 {
                gap
            } else {
                self.gap * 0.7 + gap * 0.3
            };
        }
        let ms = (self.gap * 3.0).clamp(120.0, 400.0);
        self.duration = Duration::from_secs_f32(ms / 1000.0);
        // Older arrivals have long faded: forget them.
        self.arrivals
            .retain(|(_, at)| now.saturating_duration_since(*at) < Duration::from_secs(1));
        self.arrivals.push((offset, now));
    }
}

/// What the worker thread passes back.
enum Pumped {
    /// The screenshots are saved, under these names.
    Saved(Vec<String>),
    /// From the stream.
    Chat(chat::Event),
}

/// The Guide panel.
pub struct GuidePanel {
    focus: FocusHandle,
    composer: Entity<Composer>,
    openrouter: Entity<OpenRouter>,
    store: ChatStore,
    game: Option<GuideGame>,
    chat: Chat,
    /// Each turn's answer, parsed.
    documents: Vec<Document>,
    rows: Vec<Row>,
    list: ListState,
    /// Whether the transcript follows the newest text.
    pinned: bool,
    live: Option<Live>,
    /// The veil over the turn it is fading, which outlasts the stream.
    veil: Option<(usize, Veil)>,
    shots: Vec<Shot>,
    /// Pictures of screenshots sent in this session, by turn, shown until
    /// the panel next loads the chat from its files.
    sent_images: HashMap<usize, Vec<Arc<RenderImage>>>,
    /// Images no longer shown, to drop from GPUI's atlas.
    stale: Vec<Arc<RenderImage>>,
    /// The palette shown, and where it is easing to and from.
    palette: Palette,
    palette_from: Palette,
    palette_to: Palette,
    palette_since: Option<Instant>,
    /// When the game's picture was last looked at.
    palette_seen: Option<Instant>,
    /// Turns whose thinking line is open.
    opened: HashSet<usize>,
    /// The turn whose sources are shown in the sheet.
    sheet: Option<usize>,
    /// A screenshot or picture shown large.
    enlarged: Option<Enlarged>,
    /// Where answers' pictures come from for this game, if anywhere.
    wiki: Option<Wiki>,
    /// Pictures fetched from the wiki, kept on disk.
    picture_store: PictureStore,
    /// Answers' pictures, by address.
    pictures: HashMap<String, pictures::Shown>,
    /// Bumped when the pictures are forgotten, so late ones are dropped.
    picture_generation: u64,
    /// The transcript's width when last laid out, which pictures fit.
    column: f32,
    /// The list of earlier chats, while open.
    history: Option<Vec<Summary>>,
    /// The turn whose answer, or code block, was just copied.
    copied: Option<(usize, Option<usize>, Instant)>,
    /// The last question asked, for ↑ in an empty composer.
    recall: Option<String>,
    /// How hard the model thinks.
    pub effort: Effort,
    /// Whether answers may look things up.
    pub lookups: bool,
    /// macOS's Reduce Motion: no fades or shimmer.
    reduce_motion: bool,
    /// The colours last given to the composer.
    composer_style: Option<composer::ComposerStyle>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<GuideEvent> for GuidePanel {}

impl Focusable for GuidePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GuidePanel {
    /// A panel with no game yet.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(Composer::new);
        let openrouter = OpenRouter::global(cx);
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, Self::composer_event),
            cx.observe(&openrouter, |_, _, cx| cx.notify()),
        ];
        let store = ChatStore::new(
            emu_os::data_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("ds-emulator")
                .join("Guide"),
        );
        let palette = Palette::from_tint(0x0057_5760);
        let list = ListState::new(0, ListAlignment::Top, px(600.0));
        let weak = cx.entity().downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            let at_end = event.visible_range.end >= event.count;
            if let Some(panel) = weak.upgrade() {
                panel.update(cx, |panel, cx| {
                    if panel.pinned != at_end {
                        panel.pinned = at_end;
                        cx.notify();
                    }
                });
            }
        });
        let this = Self {
            focus: cx.focus_handle(),
            composer,
            openrouter,
            store,
            game: None,
            chat: Chat::new(),
            documents: Vec::new(),
            rows: Vec::new(),
            list,
            pinned: true,
            live: None,
            veil: None,
            shots: Vec::new(),
            sent_images: HashMap::new(),
            stale: Vec::new(),
            palette,
            palette_from: palette,
            palette_to: palette,
            palette_since: None,
            palette_seen: None,
            opened: HashSet::new(),
            sheet: None,
            enlarged: None,
            wiki: None,
            picture_store: pictures::store(),
            pictures: HashMap::new(),
            picture_generation: 0,
            column: 0.0,
            history: None,
            copied: None,
            recall: None,
            effort: Effort::default(),
            lookups: true,
            reduce_motion: false,
            composer_style: None,
            _subscriptions: subscriptions,
        };
        // Read once: whether the Mac asks for less motion.
        let reading = cx.background_executor().spawn(async { reduce_motion() });
        cx.spawn(async move |this, cx| {
            let reduce = reading.await;
            this.update(cx, |this, cx| {
                this.reduce_motion = reduce;
                cx.notify();
            })
            .ok();
        })
        .detach();
        this
    }

    /// The composer's focus, which the game view watches to route keys.
    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }

    /// Puts the caret in the composer.
    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        let focus = self.composer.read(cx).focus_handle(cx);
        window.focus(&focus);
    }

    /// Whether the composer has the caret.
    pub fn composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer.read(cx).focus_handle(cx).is_focused(window)
    }

    /// The game being played, or none: loads its last chat.
    pub fn set_game(&mut self, game: Option<GuideGame>, cx: &mut Context<Self>) {
        if self.game.as_ref().map(|game| &game.key) == game.as_ref().map(|game| &game.key) {
            return;
        }
        self.stop(cx);
        self.discard_shots();
        let tint = game.as_ref().map(|game| game.tint);
        let chat = game
            .as_ref()
            .and_then(|game| self.store.latest(&game.key))
            .unwrap_or_default();
        self.wiki = game
            .as_ref()
            .and_then(|game| sources::picture_wiki(&game.context));
        self.game = game;
        self.load_chat(chat, cx);
        if let Some(tint) = tint {
            let palette = Palette::from_tint(tint);
            self.palette = palette;
            self.palette_from = palette;
            self.palette_to = palette;
            self.palette_since = None;
            self.palette_seen = None;
        }
        let placeholder = self.game.as_ref().map_or_else(
            || "Ask about this game".to_owned(),
            |game| format!("Ask about {}", game.context.short_title()),
        );
        self.composer
            .update(cx, |composer, cx| composer.set_placeholder(placeholder, cx));
        cx.notify();
    }

    /// The game's short title, for the margin's pill.
    pub fn short_title(&self) -> Option<String> {
        Some(self.game.as_ref()?.context.short_title())
    }

    fn load_chat(&mut self, chat: Chat, cx: &mut Context<Self>) {
        for images in std::mem::take(&mut self.sent_images).into_values() {
            self.stale.extend(images);
        }
        self.forget_pictures();
        self.enlarged = None;
        self.documents = chat
            .turns
            .iter()
            .map(|turn| Document::parse(&turn.answer.text))
            .collect();
        self.chat = chat;
        self.opened.clear();
        self.sheet = None;
        self.history = None;
        self.veil = None;
        self.pinned = true;
        self.rows = self.compute_rows();
        self.list.reset(self.rows.len());
        self.follow();
        cx.notify();
    }

    /// Whether it is time to look at the game's colours again.
    pub fn wants_palette(&self, now: Instant) -> bool {
        self.palette_seen
            .is_none_or(|seen| now.saturating_duration_since(seen) >= PALETTE_EVERY)
    }

    /// Follows the game's colours: `colors` is the ambient backdrop's
    /// picture. Called at most every [`PALETTE_EVERY`].
    pub fn see_picture(&mut self, colors: &[[f32; 3]], cx: &mut Context<Self>) {
        let now = Instant::now();
        self.palette_seen = Some(now);
        let Some(palette) = Palette::from_picture(colors) else {
            return;
        };
        if palette == self.palette_to {
            return;
        }
        self.palette_from = self.palette;
        self.palette_to = palette;
        self.palette_since = Some(now);
        cx.notify();
    }

    /// Brings the shown palette along its ease; true while still easing.
    fn ease_palette(&mut self, now: Instant) -> bool {
        let Some(since) = self.palette_since else {
            return false;
        };
        let t = now.saturating_duration_since(since).as_secs_f32() / PALETTE_EASE.as_secs_f32();
        if t >= 1.0 || self.reduce_motion {
            self.palette = self.palette_to;
            self.palette_since = None;
            return false;
        }
        let eased = 1.0 - (1.0 - t).powi(3);
        self.palette = self.palette_from.mix(&self.palette_to, eased);
        true
    }

    /// Screenshots waiting in the composer.
    pub fn shot_count(&self) -> usize {
        self.shots.len()
    }

    /// Adds a screenshot of `frames` (the console's screens, as drawn) to
    /// the composer, unless it has [`MOST_SHOTS`]. Returns how many wait.
    pub fn add_shot(&mut self, frames: &[Frame], players: usize, cx: &mut Context<Self>) -> usize {
        if self.shots.len() >= MOST_SHOTS || frames.is_empty() {
            return self.shots.len();
        }
        let shown = capture::compose(frames, players);
        let image = video::render_image(&shown, 1);
        self.shots.push(Shot {
            frame: shots::scale_for_model(&shown),
            image,
            taken: Instant::now(),
        });
        // The fresh edge fades with the next redraw after it.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FRESH).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        cx.notify();
        self.shots.len()
    }

    /// Adds a picture from a file, such as a photo of a paper map.
    fn add_picture(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match chosen.await {
                Ok(Ok(Some(paths))) => paths,
                // On Linux without a desktop portal, say so.
                Ok(Err(error)) => {
                    let said = format!("The file dialog didn't open: {error:#}");
                    this.update(cx, |_, cx| cx.emit(GuideEvent::Notice(said)))
                        .ok();
                    return;
                }
                Ok(Ok(None)) | Err(_) => return,
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let decoded = cx
                .background_executor()
                .spawn(async move { picture_frame(&path) })
                .await;
            this.update(cx, |this, cx| {
                if let Some(frame) = decoded {
                    this.add_shot(&[frame], 1, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn remove_shot(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.shots.len() {
            let shot = self.shots.remove(index);
            self.stale.push(shot.image);
            cx.notify();
        }
    }

    fn discard_shots(&mut self) {
        for shot in self.shots.drain(..) {
            self.stale.push(shot.image);
        }
    }

    fn composer_event(
        &mut self,
        _: &Entity<Composer>,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposerEvent::Submit => self.send(None, window, cx),
            ComposerEvent::Escape => cx.emit(GuideEvent::Blur),
            ComposerEvent::RecallLast => {
                if let Some(last) = self.recall.clone() {
                    self.composer
                        .update(cx, |composer, cx| composer.set_text(&last, cx));
                }
            }
            ComposerEvent::Changed => cx.notify(),
        }
    }

    /// Asks `question`, or what the composer holds, with the screenshots
    /// waiting.
    fn send(&mut self, question: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.live.is_some() {
            return;
        }
        let Some(game) = self.game.clone() else {
            return;
        };
        let typed = question.unwrap_or_else(|| self.composer.read(cx).text().trim().to_owned());
        if typed.is_empty() && self.shots.is_empty() {
            return;
        }
        let Some(key) = self.openrouter.read(cx).key() else {
            cx.notify();
            return;
        };
        let question = if typed.is_empty() {
            "What's happening on screen?".to_owned()
        } else {
            typed
        };
        // Every earlier turn's text goes, but only this question's
        // screenshots.
        let mut messages = Vec::new();
        for turn in &self.chat.turns {
            if turn.answer.text.trim().is_empty() {
                continue;
            }
            messages.push(Message::user(turn.question.clone(), Vec::new()));
            messages.push(Message::assistant(
                turn.answer.text.clone(),
                turn.answer.reasoning_details.clone(),
            ));
        }
        let shots: Vec<Shot> = self.shots.drain(..).collect();
        let frames: Vec<Frame> = shots.iter().map(|shot| shot.frame.clone()).collect();
        let turn = self.chat.turns.len();
        self.sent_images
            .insert(turn, shots.into_iter().map(|shot| shot.image).collect());
        self.chat.turns.push(Turn {
            question: question.clone(),
            shots: Vec::new(),
            answer: Answer::default(),
        });
        self.documents.push(Document::new());
        self.recall = Some(question.clone());
        self.composer.update(cx, |composer, cx| composer.clear(cx));
        let sources = sources::sources_for(&game.context);
        let system =
            prompt::system_prompt(&game.context, &sources, self.lookups, self.wiki.as_ref());
        let lookups = self.lookups.then(|| sources::domains(&sources));
        let session = format!("ds-emulator-{}", self.chat.id);
        let effort = self.effort;
        let store = self.store.clone();
        let chat_id = self.chat.id.clone();
        let stop = Stop::new();
        let worker_stop = stop.clone();
        let (sender, received) = async_channel::unbounded();
        let spawned = thread::Builder::new()
            .name("guide-answer".to_owned())
            .spawn(move || {
                let mut images = Vec::new();
                let mut names = Vec::new();
                for frame in &frames {
                    let png: Arc<[u8]> = Arc::from(capture::encode_png(frame).into_boxed_slice());
                    if let Ok(name) = store.save_shot(&game.key, &chat_id, &png) {
                        names.push(name);
                    }
                    images.push(png);
                }
                sender.send_blocking(Pumped::Saved(names)).ok();
                messages.push(Message::user(question, images));
                let request = Request {
                    model: chat::MODEL.to_owned(),
                    system,
                    messages,
                    effort,
                    lookups,
                    session,
                };
                chat::ask(
                    &Endpoints::openrouter(),
                    key.secret(),
                    &request,
                    &worker_stop,
                    &mut |event| {
                        sender.send_blocking(Pumped::Chat(event)).ok();
                    },
                );
            });
        if let Err(error) = spawned {
            self.chat.turns[turn].answer.ending = Ending::Failed(error.to_string());
            self.after_change(Some(Row::Question(turn)), cx);
            return;
        }
        let pump = cx.spawn(async move |this, cx| {
            while let Ok(pumped) = received.recv().await {
                if this.update(cx, |this, cx| this.pumped(pumped, cx)).is_err() {
                    break;
                }
            }
        });
        let now = Instant::now();
        self.live = Some(Live {
            turn,
            stop,
            started: now,
            first_text: None,
            working: false,
            committed: 0,
            last_commit: now,
            commit_soon: None,
            _pump: pump,
        });
        self.veil = Some((turn, Veil::default()));
        self.pinned = true;
        self.after_change(Some(Row::Question(turn)), cx);
        self.save(cx);
        self.focus_composer(window, cx);
    }

    /// Asks a starter question once a screenshot has been added.
    pub fn ask_starter(&mut self, question: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.send(Some(question.to_owned()), window, cx);
    }

    fn pumped(&mut self, pumped: Pumped, cx: &mut Context<Self>) {
        let Some(turn) = self.live.as_ref().map(|live| live.turn) else {
            return;
        };
        let answer = &mut self.chat.turns[turn].answer;
        match pumped {
            Pumped::Saved(names) => {
                self.chat.turns[turn].shots = names;
            }
            Pumped::Chat(event) => match event {
                chat::Event::Serving { model, .. } => answer.model = Some(model),
                chat::Event::Working => {
                    if let Some(live) = &mut self.live
                        && !live.working
                        && live.first_text.is_none()
                    {
                        live.working = true;
                        self.after_change(Some(Row::Thought(turn)), cx);
                    }
                }
                chat::Event::Reasoning(text) => {
                    answer.thinking.push_str(&text);
                    self.commit_soon(cx);
                }
                chat::Event::ReasoningDetails(details) => answer.reasoning_details.extend(details),
                chat::Event::Citation(citation) => {
                    answer.cite(citation);
                    self.commit_soon(cx);
                }
                chat::Event::Text(text) => {
                    let closes_block = text.contains("\n\n");
                    answer.text.push_str(&text);
                    if let Some(live) = &mut self.live
                        && live.first_text.is_none()
                    {
                        let now = Instant::now();
                        live.first_text = Some(now);
                        answer.thought_for =
                            Some(now.saturating_duration_since(live.started).as_secs_f32());
                    }
                    if closes_block {
                        self.commit(cx);
                    } else {
                        self.commit_soon(cx);
                    }
                }
                chat::Event::Usage(usage) => answer.usage = Some(usage),
                chat::Event::Finished(finish) => {
                    answer.ending = match finish {
                        Finish::Length if answer.text.trim().is_empty() => Ending::Failed(
                            "The model ran out of room while thinking — try less Thinking"
                                .to_owned(),
                        ),
                        Finish::ContentFilter => {
                            Ending::Failed("The model's provider stopped this answer".to_owned())
                        }
                        _ => Ending::Done,
                    };
                    self.finish(cx);
                }
                chat::Event::Failed(error) => {
                    if error == ChatError::KeyRefused {
                        self.openrouter
                            .update(cx, |openrouter, cx| openrouter.check(cx));
                    }
                    answer.ending = Ending::Failed(error.to_string());
                    self.finish(cx);
                }
                chat::Event::Stopped => {
                    answer.ending = Ending::Stopped;
                    self.finish(cx);
                }
            },
        }
    }

    /// Shows what streamed, at most [`COMMIT`] after it came.
    fn commit_soon(&mut self, cx: &mut Context<Self>) {
        let Some(live) = &mut self.live else {
            return;
        };
        if live.commit_soon.is_some() {
            return;
        }
        let wait = COMMIT.saturating_sub(live.last_commit.elapsed());
        live.commit_soon = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| this.commit(cx)).ok();
        }));
    }

    /// Hands the answer's new text to its document and redraws.
    fn commit(&mut self, cx: &mut Context<Self>) {
        let Some(turn) = self.live.as_ref().map(|live| live.turn) else {
            return;
        };
        let dirty = self.rows_dirty_from(turn);
        let Some(live) = &mut self.live else {
            return;
        };
        live.commit_soon = None;
        let now = Instant::now();
        live.last_commit = now;
        let text = &self.chat.turns[turn].answer.text;
        if live.committed < text.len() {
            let from = live.committed;
            self.documents[turn].push(&text[from..]);
            live.committed = text.len();
            if let Some((veil_turn, veil)) = &mut self.veil
                && *veil_turn == turn
                && !self.reduce_motion
            {
                veil.arrive(from, now);
            }
        }
        self.after_change(Some(dirty), cx);
    }

    /// The first row a commit may change: the answer's last block, or its
    /// thinking line before any.
    fn rows_dirty_from(&self, turn: usize) -> Row {
        let blocks = self.documents.get(turn).map_or(0, |doc| doc.blocks().len());
        if blocks > 0 {
            Row::Block(turn, blocks - 1)
        } else {
            Row::Thought(turn)
        }
    }

    /// The stream has ended, one way or another.
    fn finish(&mut self, cx: &mut Context<Self>) {
        self.commit(cx);
        let Some(live) = self.live.take() else {
            return;
        };
        let dirty = self.rows_dirty_from(live.turn);
        self.documents[live.turn].settle();
        self.after_change(Some(dirty), cx);
        self.save(cx);
    }

    /// Stops the answer under way.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(live) = &self.live {
            live.stop.stop();
        }
        // The worker reports Stopped; if the pump is gone, end here.
        if let Some(live) = &self.live
            && live.stop.is_stopped()
            && let Some(turn) = self.chat.turns.get_mut(live.turn)
            && turn.answer.ending == Ending::Streaming
        {
            turn.answer.ending = Ending::Stopped;
            self.finish(cx);
        }
    }

    /// Asks turn `turn`'s question again, in place of its answer, if it
    /// is the last.
    fn ask_again(&mut self, turn: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.live.is_some() || turn + 1 != self.chat.turns.len() {
            return;
        }
        let asked = self.chat.turns.pop().map(|turn| turn.question);
        self.documents.pop();
        if let Some(images) = self.sent_images.remove(&turn) {
            self.stale.extend(images);
        }
        self.rows = self.compute_rows();
        self.list.reset(self.rows.len());
        self.send(asked, window, cx);
    }

    fn copy(&mut self, text: String, turn: usize, block: Option<usize>, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some((turn, block, Instant::now()));
        let row = block.map_or(Row::Ending(turn), |block| Row::Block(turn, block));
        self.after_change(Some(row), cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED).await;
            this.update(cx, |this, cx| {
                this.copied = None;
                this.after_change(Some(row), cx);
            })
            .ok();
        })
        .detach();
    }

    /// Starts a new chat for this game.
    fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop(cx);
        self.load_chat(Chat::new(), cx);
        self.focus_composer(window, cx);
    }

    fn show_history(&mut self, cx: &mut Context<Self>) {
        self.history = match (&self.history, &self.game) {
            (None, Some(game)) => Some(self.store.list(&game.key)),
            _ => None,
        };
        cx.notify();
    }

    fn open_chat(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(game) = &self.game else {
            return;
        };
        if let Some(chat) = self.store.load(&game.key, id) {
            self.stop(cx);
            self.load_chat(chat, cx);
        }
    }

    fn delete_chat(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(game) = self.game.clone() else {
            return;
        };
        if id == self.chat.id {
            self.stop(cx);
            self.load_chat(Chat::new(), cx);
        }
        self.store.delete(&game.key, id).ok();
        self.history = Some(self.store.list(&game.key));
        cx.notify();
    }

    /// Writes the chat, off the UI thread.
    fn save(&self, cx: &mut Context<Self>) {
        let (Some(game), false) = (&self.game, self.chat.turns.is_empty()) else {
            return;
        };
        let store = self.store.clone();
        let key = game.key.clone();
        let chat = self.chat.clone();
        cx.background_executor()
            .spawn(async move {
                store.save(&key, &chat).ok();
            })
            .detach();
    }

    fn compute_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (turn, entry) in self.chat.turns.iter().enumerate() {
            rows.push(Row::Question(turn));
            if self.shows_thought(turn, &entry.answer) {
                rows.push(Row::Thought(turn));
            }
            let blocks = self.documents.get(turn).map_or(0, |doc| doc.blocks().len());
            rows.extend((0..blocks).map(|block| Row::Block(turn, block)));
            if entry.answer.ending != Ending::Streaming {
                rows.push(Row::Ending(turn));
            }
        }
        rows
    }

    /// Whether turn `turn` shows its thinking line: while waiting, and
    /// after if it thought, searched or read anything.
    fn shows_thought(&self, turn: usize, answer: &Answer) -> bool {
        let live = self.live.as_ref().is_some_and(|live| live.turn == turn);
        if live && answer.text.is_empty() {
            return true;
        }
        answer.thought_for.is_some_and(|secs| secs >= 1.0)
            || !answer.citations.is_empty()
            || !answer.thinking.is_empty()
    }

    /// Re-measures the rows from `dirty` on (or wherever they changed),
    /// keeps the newest in view while following, and redraws.
    fn after_change(&mut self, dirty: Option<Row>, cx: &mut Context<Self>) {
        let rows = self.compute_rows();
        let mut from = self
            .rows
            .iter()
            .zip(&rows)
            .position(|(old, new)| old != new)
            .unwrap_or(self.rows.len().min(rows.len()));
        if let Some(dirty) = dirty
            && let Some(at) = self.rows.iter().position(|row| *row == dirty)
        {
            from = from.min(at);
        }
        let old = self.rows.len();
        if from < old || rows.len() != old {
            self.list
                .splice(from..old, rows.len() - from.min(rows.len()));
        }
        self.rows = rows;
        self.follow();
        cx.notify();
    }

    /// Scrolls to the newest while pinned.
    fn follow(&self) {
        if self.pinned {
            self.list.scroll_to(ListOffset {
                item_ix: self.rows.len(),
                offset_in_item: px(0.0),
            });
        }
    }

    fn jump_to_newest(&mut self, cx: &mut Context<Self>) {
        self.pinned = true;
        self.follow();
        cx.notify();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        self.openrouter
            .update(cx, |openrouter, cx| openrouter.connect(cx));
    }

    fn connection(&self, cx: &App) -> Status {
        self.openrouter.read(cx).status().clone()
    }
}

/// A picture file as a frame, for the composer.
fn picture_frame(path: &std::path::Path) -> Option<Frame> {
    let picture = image::open(path).ok()?.into_rgba8();
    let (width, height) = picture.dimensions();
    // Pictures larger than the model needs are shrunk on the way.
    let most = 1600u32;
    let picture = if width.max(height) > most {
        image::imageops::resize(
            &picture,
            width * most / width.max(height),
            height * most / width.max(height),
            image::imageops::FilterType::Triangle,
        )
    } else {
        picture
    };
    let (width, height) = picture.dimensions();
    let mut frame = Frame::new(emu_core::ScreenSize::new(
        u16::try_from(width).ok()?,
        u16::try_from(height).ok()?,
    ));
    for (pixel, rgba) in frame.pixels_mut().iter_mut().zip(picture.pixels()) {
        let [red, green, blue, _] = rgba.0;
        *pixel = emu_core::rgb(red, green, blue);
    }
    Some(frame)
}

/// Whether macOS's Reduce Motion is on.
fn reduce_motion() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "com.apple.universalaccess", "reduceMotion"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_fits_a_margin_or_asks_for_a_column() {
        assert_eq!(width_in(230.0, 16.0), None);
        assert_eq!(width_in(332.0, 16.0), Some(316.0));
        assert_eq!(width_in(490.0, 16.0), Some(440.0));
    }

    #[test]
    fn the_veil_fades_each_commit_in_on_its_own() {
        let start = Instant::now();
        let mut veil = Veil::default();
        veil.arrive(0, start);
        veil.arrive(10, start + Duration::from_millis(100));
        assert!(veil.duration >= Duration::from_millis(120));
        let at = start + Duration::from_millis(150);
        assert!(veil.alpha(5, at) > veil.alpha(12, at));
        assert_eq!(veil.alpha(5, start + Duration::from_secs(2)), 1.0);
        assert_eq!(veil.cuts(0..20), vec![10]);
        assert!(veil.settled(start + Duration::from_secs(2)));
        assert!(!veil.settled(at));
    }
}
