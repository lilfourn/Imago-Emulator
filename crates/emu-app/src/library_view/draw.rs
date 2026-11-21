//! How the library is drawn: its header and search field, the game last
//! played, the system filter, each game's tile and the tiles of games
//! being added, a game's menu, and the empty library; with the words and
//! measures they are drawn in.

// A part of the library's view, split off: it shares its names.
use super::*;

/// The background greys a touch towards the foot of the window, to
/// Imago Light's shell.
const FOOT: u32 = 0x00F6_F6F7;

/// Room left of the header for the close, minimise and zoom buttons and
/// a gap after them; off macOS, for the menu's button there instead
/// (`chrome::menu_button`).
pub const TRAFFIC_LIGHTS: f32 = if cfg!(target_os = "macos") {
    96.0
} else {
    48.0
};
/// Room right of the header's last button, which puts its icon's artwork
/// as far from the window's right side as the close button is from the
/// left.
pub const HEADER_END: f32 = 8.0;
/// Height of the update pill in the header.
const UPDATE_PILL: f32 = 26.0;
/// The narrowest header, besides the window's own buttons, whose update
/// pill has its words and a button to dismiss it; a narrower one shows the
/// pill's icon alone, so the header's other buttons stay in the window.
const ROOM_FOR_UPDATE_WORDS: f32 = 560.0;
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
/// The menu's surface: Imago Light's paper, lifted off the tiles it
/// floats over by its edge and shadow, so the theme's ink reads on it.
const MENU_BACKGROUND: u32 = TOP;
/// Height of an action in the menu, whose icon is as far in from the
/// row's side as from its top and bottom: as far in as everything else in
/// the menu.
const ACTION_HEIGHT: f32 = 32.0;
const MENU_INSET: f32 = (ACTION_HEIGHT - icons::SIZE) / 2.0;
/// Height of a line of the game's facts, and side of its picture.
const FACT_HEIGHT: f32 = 22.0;
const THUMBNAIL: f32 = 44.0;

/// Height of the progress bar on the tile of a game being added, and the
/// share of it the bar's moving piece fills while how far it has got is
/// not known.
const PROGRESS_BAR: f32 = 4.0;
const PROGRESS_PIECE: f32 = 0.3;

impl LibraryView {
    fn header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h(px(TOP_BAR))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(TRAFFIC_LIGHTS))
            // Clear of the window's buttons, where the app draws them.
            .pr(px(HEADER_END + chrome::trailing_room(window, true)))
            .gap_1()
            .children(probe::part("header"))
            .child(chrome::over_drag_area(ui::tabs([
                ui::tab(Section::Library, Section::Library, window),
                ui::tab(Section::Store, Section::Library, window)
                    .on_click(cx.listener(|this, _, _, cx| this.open_store(cx))),
            ])))
            .child(div().flex_1())
            .children(self.update_pill(window, cx))
            .child(chrome::over_drag_area(
                icons::button("settings", icons::SETTINGS)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::OpenSettings)))
                    .children(probe::part("button:settings")),
            ))
            .child(chrome::over_drag_area(
                icons::button("add", icons::ADD)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(LibraryEvent::AddGames)))
                    .children(probe::part("button:add")),
            ))
            .into_any_element()
    }

    /// The pill that says a newer version is out, and downloads it,
    /// restarts into it or opens its page; with a button to put an offer
    /// away until the next launch.
    fn update_pill(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pill = self.updater.read(cx).pill()?;
        let room = f32::from(window.viewport_size().width) - chrome::trailing_room(window, true);
        let worded = room >= ROOM_FOR_UPDATE_WORDS;
        let updater = self.updater.clone();
        let button = div()
            .id("update")
            .flex_none()
            .h(px(UPDATE_PILL))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .when(worded, |button| button.pl(px(10.0)).pr(px(12.0)))
            .when(!worded, |button| button.w(px(UPDATE_PILL)))
            .rounded_full()
            .bg(rgb(accent()))
            .text_color(rgb(ON_ACCENT))
            .text_size(px(12.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .whitespace_nowrap()
            .when(pill.busy, |button| button.opacity(0.8))
            .when(!pill.busy, |button| {
                button
                    .cursor_pointer()
                    .hover(|button| button.opacity(0.9))
                    .on_click(move |_, _, cx| updater.update(cx, |updater, cx| updater.press(cx)))
            })
            .child(icons::icon(icons::DOWNLOAD, 14.0, ON_ACCENT))
            .when(worded, |button| button.child(pill.label))
            .children(probe::part("button:update"));
        let updater = self.updater.clone();
        let dismiss = (pill.dismissable && worded).then(|| {
            chrome::over_drag_area(
                icons::button("dismiss-update", icons::WINDOW_CLOSE)
                    .on_click(move |_, _, cx| updater.update(cx, |updater, cx| updater.dismiss(cx)))
                    .children(probe::part("button:dismiss-update")),
            )
        });
        Some(
            div()
                .flex()
                .items_center()
                .mr_1()
                .child(chrome::over_drag_area(button))
                .children(dismiss)
                .into_any_element(),
        )
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
            div()
                .w(px(1.5))
                .h(px(16.0))
                .bg(rgb(accent()))
                .with_animation(
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
                with_alpha(accent(), 0xAA)
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
                system_line(game),
                ago(played, SystemTime::now())
            ),
            None => system_line(game),
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
            .bg(rgb(accent()))
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
                    .bg(rgb(accent()))
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
                                .child(line(&system_line(game), 11.0, gpui::FontWeight::NORMAL)),
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
    /// and its size; then Play, Show in Finder and Remove from Library. A
    /// Pokémon game the randomizer knows has Randomize… too, or once it
    /// has a randomized run, Randomized, which switches the run on and
    /// off, and New Seed….
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
            Some(serial) => format!("{} · {serial}", system_line(game)),
            None => system_line(game),
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
                read(|facts| states_line(facts.states, facts.script_states)),
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
            Some(return_key()),
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
            SHOW_IN_FILES,
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
        let run = game.randomized.as_ref();
        // The switch: which of the two the game plays, said as it is now.
        let switch = run.map(|run| {
            let (path, on) = (game.path.clone(), !run.on);
            let now = if run.on { "On" } else { "Off" };
            menu_action(
                "menu-randomized",
                icons::SHUFFLE,
                "Randomized",
                Some(now.to_owned()),
                TEXT,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                let at = this.menu.as_ref().map(|menu| menu.at);
                this.switch_randomized(&path, on, cx);
                // Its saves and states are the other file's now.
                if let Some(at) = at {
                    this.open_menu(path.clone(), at, cx);
                }
            }))
            .children(probe::part("menu.action:randomized"))
        });
        let randomize = (randomizable || run.is_some()).then(|| {
            let label = if run.is_some() {
                "New Seed…"
            } else {
                "Randomize…"
            };
            let icon = if run.is_some() {
                icons::REFRESH
            } else {
                icons::SHUFFLE
            };
            menu_action("menu-randomize", icon, label, None, TEXT, true)
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
            Some(keys::shortcut(shortcut::REMOVE_FROM_LIBRARY)),
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
            .children(switch)
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
                format!(
                    "Get free homebrew games from the store, add your own with + or {}, or drop a .gb, .gbc, .gba, .nds or .3ds file, a .zip, .7z or .rar of one, a ROM-hack patch, or an RPG Maker game's folder, here.",
                    keys::shortcut(shortcut::ADD_TO_LIBRARY)
                ),
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
            if none {
                return brand_mark(56.0).mb_2();
            }
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
                .child(icons::icon(icons::MAGNIFIER, 24.0, DIM))
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
                        .bg(rgb(accent()))
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

/// The app's icon, `side` across, for the empty library: `assets/icon.svg`
/// made with `resvg -w 168 -h 168 assets/icon.svg assets/icon.png`.
fn brand_mark(side: f32) -> gpui::Div {
    static ICON: LazyLock<Arc<gpui::Image>> = LazyLock::new(|| {
        let png = include_bytes!("../../../../assets/icon.png");
        Arc::new(gpui::Image::from_bytes(ImageFormat::Png, png.to_vec()))
    });
    div().size(px(side)).child(img(ICON.clone()).size_full())
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
    keys: Option<String>,
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

/// `game`'s system, and that it is randomized while its randomized run
/// is what it plays: `Nintendo DS · Randomized`.
fn system_line(game: &Game) -> String {
    let system = platform_name(game.platform);
    match &game.randomized {
        Some(run) if run.on => format!("{system} · Randomized"),
        _ => system.to_owned(),
    }
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

/// How many save states a game has, for its menu: `states` in the slots
/// the player chooses, and `script_states` in those only a script saves
/// to.
pub(super) fn states_line(states: usize, script_states: usize) -> String {
    match (states, script_states) {
        (0, 0) => "None".to_owned(),
        (states, 0) => format!("{states} of {STATE_SLOTS}"),
        (0, scripts) => format!("{scripts} from scripts"),
        (states, scripts) => format!("{states} of {STATE_SLOTS}, and {scripts} from scripts"),
    }
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

/// The game menu's item that shows a game's file where the system keeps
/// files: the Finder, File Explorer, or the desktop's file manager on
/// Linux, which GPUI opens at the game's folder.
const SHOW_IN_FILES: &str = if cfg!(target_os = "macos") {
    "Show in Finder"
} else if cfg!(windows) {
    "Show in File Explorer"
} else {
    "Show in Files"
};

/// The Return key as the game menu writes it beside Play: `↩` on macOS,
/// `Enter` elsewhere.
fn return_key() -> String {
    if cfg!(target_os = "macos") {
        "↩".to_owned()
    } else {
        keys::key("enter")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_game_menus_words_read_on_its_surface() {
        use crate::guide::palette::contrast;
        let rgb =
            |colour: u32| [16, 8, 0].map(|shift| f32::from((colour >> shift & 0xFF) as u8) / 255.0);
        for (name, colour) in [("text", TEXT), ("dimmed", DIM), ("danger", DANGER)] {
            let ratio = contrast(rgb(colour), rgb(MENU_BACKGROUND));
            assert!(ratio >= 4.5, "{name} on the menu: {ratio:.2}:1");
        }
    }

    #[test]
    fn the_grid_fills_the_width_with_whole_tiles() {
        let (columns, tile) = grid(450.0);
        assert_eq!(columns, 3);
        assert!((tile * 3.0 + GAP * 2.0 - 450.0).abs() < 0.01);
        assert_eq!(grid(4.0 * MIN_TILE + 3.0 * GAP).0, 4);
        assert_eq!(grid(50.0).0, 1);
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
}
