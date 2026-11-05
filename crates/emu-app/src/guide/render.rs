//! Drawing the panel: ChatGPT's structure (unboxed answers, the question
//! pill, one thinking line, source chips, a quiet action row and a rounded
//! composer) in the game's colours (PRD 6.7).

use std::time::{Duration, Instant};

use emu_guide::conversation::{Answer, Ending};
use emu_guide::sources;
use gpui::{
    Animation, AnimationExt, AnyElement, AppContext, BoxShadow, Context, FontWeight, IntoElement,
    MouseButton, ObjectFit, Render, Window, div, img, linear_color_stop, linear_gradient, list,
    point, prelude::*, px, relative,
};

use super::answer::{self, BODY, Drawing, Ink};
use super::composer;
use super::palette::hsla;
use super::{FRESH, GuideEvent, GuidePanel, MOST_SHOTS, Row};
use crate::icons;
use crate::openrouter::Status;
use crate::probe;

/// The header's height: the window's top bar, so the title sits on the
/// traffic lights' midline.
const HEADER: f32 = 40.0;
/// Side padding of the transcript.
const SIDE: f32 = 20.0;
/// How many lines of readable thinking show before the rest folds away.
const THINKING_LINES: usize = 24;

impl Render for GuidePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        for image in self.stale.drain(..) {
            window.drop_image(image).ok();
        }
        if self.ease_palette(now) {
            window.request_animation_frame();
        }
        if self
            .veil
            .as_ref()
            .is_some_and(|(_, veil)| !veil.settled(now))
        {
            window.request_animation_frame();
        }
        let ink = Ink::new(&self.palette);
        let mut glass = ink.glass;
        glass.a = 0.94;
        self.style_composer(&ink, cx);
        let connected = matches!(self.connection(cx), Status::Connected { .. });

        let body: AnyElement = if !connected {
            self.connect_card(&ink, cx)
        } else if self.chat.turns.is_empty() {
            self.empty_state(&ink, cx)
        } else {
            self.transcript(&ink, cx)
        };

        div()
            .id("guide-panel")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(glass)
            .border_l_1()
            .border_color(hsla([1.0; 3], 0.07))
            .text_color(ink.text)
            .text_size(px(BODY.0))
            .line_height(px(BODY.1))
            .occlude()
            // Clicks here are the panel's: never the game's A button.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(self.header(&ink, cx))
            .child(div().flex_1().min_h_0().relative().child(body))
            .when(connected, |panel| {
                panel.child(self.composer_area(&ink, now, cx))
            })
            .children(self.history_menu(&ink, cx))
            .children(self.sources_sheet(&ink, cx))
            .children(self.enlarged_shot(cx))
            .children(probe::part("panel:guide"))
    }
}

impl GuidePanel {
    /// Gives the composer the panel's colours, when they have changed.
    fn style_composer(&mut self, ink: &Ink, cx: &mut Context<Self>) {
        let mut selection = ink.accent;
        selection.a = 0.35;
        let style = composer::ComposerStyle {
            text: ink.text,
            placeholder: ink.faint,
            selection,
            caret: ink.text,
            ..composer::ComposerStyle::default()
        };
        if self.composer_style != Some(style) {
            self.composer_style = Some(style);
            self.composer
                .update(cx, |composer, cx| composer.set_style(style, cx));
        }
    }

    fn header(&self, ink: &Ink, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .game
            .as_ref()
            .map_or_else(|| "Guide".to_owned(), |game| game.context.short_title());
        div()
            .h(px(HEADER))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.0))
            .pl(px(12.0))
            .pr(px(8.0))
            .child(
                div()
                    .id("guide-title")
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .min_w_0()
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(ink.raised))
                    .text_size(px(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(div().truncate().child(title))
                    .child(icons::icon(icons::CHEVRON_DOWN, 12.0, 0).text_color(ink.muted))
                    .on_click(cx.listener(|this, _, _, cx| this.show_history(cx)))
                    .children(probe::part("button:history")),
            )
            .child(div().flex_1())
            .child(
                panel_button("guide-new", icons::NEW_CHAT, ink)
                    .on_click(cx.listener(|this, _, window, cx| this.new_chat(window, cx)))
                    .children(probe::part("button:new-chat")),
            )
            .child(
                close_button("guide-close", ink)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(GuideEvent::Close)))
                    .children(probe::part("button:close-guide")),
            )
    }

    fn connect_card(&self, ink: &Ink, cx: &mut Context<Self>) -> AnyElement {
        let status = self.connection(cx);
        let mut card = div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(14.0))
            .px(px(28.0))
            .child(
                div()
                    .text_size(px(17.0))
                    .line_height(px(25.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Ask about the game you're playing"),
            )
            .child(div().text_color(ink.muted).child(
                "Guide answers questions about the game you're playing, with sources from the wikis players trust.",
            ));
        let (label, why) = match &status {
            Status::Connecting { returned } => {
                card = card.child(div().text_color(ink.muted).child(if *returned {
                    "Finishing connecting…"
                } else {
                    "Finish connecting in your browser."
                }));
                ("Cancel", None)
            }
            Status::Failed(why) => ("Try Again", Some(why.clone())),
            _ => ("Connect OpenRouter", None),
        };
        if let Some(why) = why {
            card = card.child(div().text_color(ink.danger).child(why));
        }
        let cancel = matches!(status, Status::Connecting { .. });
        card.child(
            div()
                .id("guide-connect")
                .h(px(36.0))
                .px(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(18.0))
                .cursor_pointer()
                .when(!cancel, |button| {
                    button
                        .bg(hsla([0.957; 3], 1.0))
                        .text_color(hsla([0.0; 3], 1.0))
                })
                .when(cancel, |button| button.bg(ink.raised))
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(14.0))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if cancel {
                        this.openrouter
                            .update(cx, |openrouter, cx| openrouter.cancel(cx));
                    } else {
                        this.connect(cx);
                    }
                }))
                .children(probe::part("button:connect")),
        )
        .child(
            div()
                .text_size(px(12.0))
                .line_height(px(17.0))
                .text_color(ink.faint)
                .child("Your questions, and any screenshots you add, go to OpenRouter."),
        )
        .into_any_element()
    }

    fn empty_state(&self, ink: &Ink, cx: &mut Context<Self>) -> AnyElement {
        let starters = self
            .game
            .as_ref()
            .map(|game| emu_guide::prompt::starters(&game.context))
            .unwrap_or_default();
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap(px(16.0))
            .px(px(SIDE))
            .child(
                div()
                    .text_size(px(19.0))
                    .line_height(px(26.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("What are you stuck on?"),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(8.0))
                    .children(starters.into_iter().enumerate().map(|(index, starter)| {
                        div()
                            .id(("guide-starter", index))
                            .px(px(12.0))
                            .py(px(6.0))
                            .rounded(px(16.0))
                            .border_1()
                            .border_color(ink.hairline)
                            .cursor_pointer()
                            .hover(|style| style.bg(ink.raised))
                            .text_size(px(13.5))
                            .line_height(px(19.0))
                            .text_color(ink.muted)
                            .child(starter)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(GuideEvent::AskWithShot(starter));
                            }))
                            .children(probe::part(format!("chip:starter.{index}")))
                    })),
            )
            .into_any_element()
    }

    fn transcript(&self, ink: &Ink, cx: &mut Context<Self>) -> AnyElement {
        let rows = list(
            self.list.clone(),
            cx.processor(|this, index: usize, window, cx| this.render_row(index, window, cx)),
        )
        .size_full()
        .pb(px(16.0));
        let mut fade_top = ink.glass;
        fade_top.a = 0.94;
        let mut fade_bottom = fade_top;
        fade_bottom.a = 0.0;
        div()
            .size_full()
            .relative()
            .child(rows)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(24.0))
                    .bg(linear_gradient(
                        180.0,
                        linear_color_stop(fade_top, 0.0),
                        linear_color_stop(fade_bottom, 1.0),
                    )),
            )
            .when(!self.pinned, |area| {
                area.child(
                    div()
                        .absolute()
                        .bottom(px(8.0))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("guide-jump")
                                .size(px(32.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(ink.glass)
                                .border_1()
                                .border_color(ink.hairline)
                                .cursor_pointer()
                                .child(icons::icon(icons::ARROW_DOWN, 16.0, 0).text_color(ink.text))
                                .on_click(cx.listener(|this, _, _, cx| this.jump_to_newest(cx))),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(index).copied() else {
            return div().into_any_element();
        };
        let ink = Ink::new(&self.palette);
        let font = window.text_style().font();
        match row {
            Row::Question(turn) => self.question_row(turn, &ink, cx),
            Row::Thought(turn) => self.thought_row(turn, &ink, &font, cx),
            Row::Block(turn, block) => {
                let Some(content) = self
                    .documents
                    .get(turn)
                    .and_then(|document| document.blocks().get(block))
                    .cloned()
                else {
                    return div().into_any_element();
                };
                let veil = self
                    .veil
                    .as_ref()
                    .filter(|(veiled, _)| *veiled == turn)
                    .map(|(_, veil)| veil.clone());
                let drawing = Drawing {
                    turn,
                    block,
                    ink,
                    font,
                    veil: veil.as_ref(),
                    now: Instant::now(),
                    copied: self
                        .copied
                        .is_some_and(|(copied, at, _)| copied == turn && at == Some(block)),
                };
                let element = self.draw_block(&content, &drawing, cx);
                div()
                    .px(px(SIDE))
                    .pt(px(if block == 0 { 2.0 } else { 10.0 }))
                    .child(element)
                    .into_any_element()
            }
            Row::Ending(turn) => self.ending_row(turn, &ink, cx),
        }
    }

    fn question_row(&self, turn: usize, ink: &Ink, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.chat.turns.get(turn) else {
            return div().into_any_element();
        };
        let mut pictures: Vec<AnyElement> = Vec::new();
        if let Some(images) = self.sent_images.get(&turn) {
            for (at, image) in images.iter().enumerate() {
                let image = image.clone();
                pictures.push(
                    thumbnail(("guide-sent", turn * 10 + at), ink)
                        .child(
                            img(image.clone())
                                .size_full()
                                .object_fit(ObjectFit::Contain),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.enlarged = Some(image.clone());
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
        } else if let Some(game) = &self.game {
            for (at, name) in entry.shots.iter().enumerate() {
                let path = self.store.shot_path(&game.key, &self.chat.id, name);
                pictures.push(
                    thumbnail(("guide-sent", turn * 10 + at), ink)
                        .child(img(path).size_full().object_fit(ObjectFit::Contain))
                        .into_any_element(),
                );
            }
        }
        div()
            .w_full()
            .px(px(SIDE))
            .pt(px(if turn == 0 { 12.0 } else { 22.0 }))
            .pb(px(10.0))
            .flex()
            .flex_col()
            .items_end()
            .gap(px(6.0))
            .when(!pictures.is_empty(), |row| {
                row.child(div().flex().gap(px(6.0)).children(pictures))
            })
            .child(
                div()
                    .max_w(relative(0.8))
                    .px(px(14.0))
                    .py(px(8.0))
                    .rounded(px(18.0))
                    .bg(ink.raised)
                    .child(entry.question.clone())
                    .children(probe::part(format!("pill:question.{turn}"))),
            )
            .into_any_element()
    }

    fn thought_row(
        &self,
        turn: usize,
        ink: &Ink,
        font: &gpui::Font,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(entry) = self.chat.turns.get(turn) else {
            return div().into_any_element();
        };
        let answer = &entry.answer;
        let live = self.live.as_ref().filter(|live| live.turn == turn);
        let waiting = live.is_some() && answer.text.is_empty();
        let line: AnyElement = if waiting {
            let label = if live.is_some_and(|live| live.working) && self.lookups {
                searching_label(&self.searched_sites(answer))
            } else {
                "Thinking".to_owned()
            };
            if self.reduce_motion {
                div().text_color(ink.muted).child(label).into_any_element()
            } else {
                let shimmer_ink = *ink;
                let shimmer_font = font.clone();
                div()
                    .text_color(ink.muted)
                    .with_animation(
                        ("guide-shimmer", turn),
                        Animation::new(Duration::from_millis(3400)).repeat(),
                        move |line, phase| {
                            line.child(answer::shimmer(&label, phase, &shimmer_ink, &shimmer_font))
                        },
                    )
                    .into_any_element()
            }
        } else {
            let secs = answer.thought_for.unwrap_or(0.0).round().max(1.0);
            let label = format!("Thought for {secs}s");
            div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .child(label)
                .child(
                    icons::icon(
                        if self.opened.contains(&turn) {
                            icons::CHEVRON_DOWN
                        } else {
                            icons::CHEVRON_RIGHT
                        },
                        12.0,
                        0,
                    )
                    .text_color(ink.muted),
                )
                .into_any_element()
        };
        let openable = !waiting;
        let mut row = div()
            .px(px(SIDE))
            .pb(px(6.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .id(("guide-thought", turn))
                    .text_size(px(13.5))
                    .line_height(px(20.0))
                    .text_color(ink.muted)
                    .when(openable, |line| line.cursor_pointer())
                    .child(line)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if openable {
                            if !this.opened.remove(&turn) {
                                this.opened.insert(turn);
                            }
                            this.after_change(Some(Row::Thought(turn)), cx);
                        }
                    }))
                    .children(probe::part(format!("line:thought.{turn}"))),
            );
        if self.opened.contains(&turn) && !waiting {
            row = row.child(self.activity(turn, answer, ink));
        }
        row.into_any_element()
    }

    /// The sites an answer drew on, by name, without repeats.
    fn searched_sites(&self, answer: &Answer) -> Vec<String> {
        let mut names: Vec<String> = answer
            .citations
            .iter()
            .map(|citation| sources::site_name(&citation.url))
            .collect();
        names.dedup();
        if names.is_empty()
            && let Some(game) = &self.game
        {
            names = sources::sources_for(&game.context)
                .into_iter()
                .take(2)
                .map(|source| source.name)
                .collect();
        }
        names
    }

    /// What happened while the model thought: searches, the pages it
    /// read, and any readable thinking.
    fn activity(&self, turn: usize, answer: &Answer, ink: &Ink) -> AnyElement {
        let step = |icon: &'static str, text: String| {
            div()
                .flex()
                .gap(px(8.0))
                .text_size(px(13.0))
                .line_height(px(19.0))
                .text_color(ink.muted)
                .child(
                    div()
                        .pt(px(2.0))
                        .child(icons::icon(icon, 15.0, 0).text_color(ink.faint)),
                )
                .child(div().flex_1().min_w_0().child(text))
        };
        let mut steps: Vec<AnyElement> = Vec::new();
        let searches = answer.usage.map_or(0, |usage| usage.searches);
        if searches > 0 || !answer.citations.is_empty() {
            let sites = self.searched_sites(answer).join(", ");
            steps.push(step(icons::MAGNIFIER, format!("Searched {sites}")).into_any_element());
        }
        for (at, citation) in answer.citations.iter().enumerate() {
            let url = citation.url.clone();
            let title = if citation.title.is_empty() {
                citation.url.clone()
            } else {
                citation.title.clone()
            };
            steps.push(
                step(
                    icons::PAGE,
                    format!("Read {title} · {}", sources::site_name(&citation.url)),
                )
                .id(("guide-read", turn * 100 + at))
                .cursor_pointer()
                .hover(|style| style.text_color(ink.text))
                .on_click(move |_, _, cx| cx.open_url(&url))
                .into_any_element(),
            );
        }
        if !answer.thinking.trim().is_empty() {
            let lines: Vec<&str> = answer.thinking.lines().collect();
            let mut shown = lines
                .iter()
                .take(THINKING_LINES)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if lines.len() > THINKING_LINES {
                shown.push_str(&format!("\n… {} more lines", lines.len() - THINKING_LINES));
            }
            steps.push(
                div()
                    .text_size(px(13.0))
                    .line_height(px(19.0))
                    .text_color(ink.faint)
                    .child(shown)
                    .into_any_element(),
            );
        }
        div()
            .ml(px(4.0))
            .pl(px(12.0))
            .border_l_1()
            .border_color(ink.hairline)
            .flex()
            .flex_col()
            .gap(px(6.0))
            .children(steps)
            .into_any_element()
    }

    fn ending_row(&self, turn: usize, ink: &Ink, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.chat.turns.get(turn) else {
            return div().into_any_element();
        };
        let answer = &entry.answer;
        let last = turn + 1 == self.chat.turns.len();
        let mut row = div()
            .px(px(SIDE))
            .pt(px(8.0))
            .flex()
            .flex_col()
            .gap(px(6.0));
        match &answer.ending {
            Ending::Failed(why) => {
                let refused = why.contains("connect again");
                row = row.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(10.0))
                        .bg({
                            let mut wash = ink.danger;
                            wash.a = 0.12;
                            wash
                        })
                        .text_size(px(13.5))
                        .line_height(px(19.0))
                        .text_color(ink.danger)
                        .child(div().flex_1().child(why.clone()))
                        .when(last && !refused, |chip| {
                            chip.child(
                                text_button("guide-retry", "Retry", ink)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.ask_again(turn, window, cx);
                                    }))
                                    .children(probe::part("button:retry")),
                            )
                        })
                        .when(refused, |chip| {
                            chip.child(
                                text_button("guide-reconnect", "Connect", ink)
                                    .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                            )
                        })
                        .children(probe::part(format!("chip:error.{turn}"))),
                );
                return row.into_any_element();
            }
            Ending::Stopped => {
                row = row.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(ink.faint)
                        .child("Stopped"),
                );
            }
            _ => {}
        }
        let copied = self
            .copied
            .is_some_and(|(copied, block, _)| copied == turn && block.is_none());
        let text = answer.text.clone();
        let meta = {
            let secs = answer.thought_for.unwrap_or(0.0);
            let cost = answer
                .usage
                .and_then(|usage| usage.cost)
                .map(|cost| format!(" · ${cost:.3}"))
                .unwrap_or_default();
            format!("{secs:.0}s{cost}")
        };
        let group = format!("guide-ending-{turn}");
        let mut actions = div()
            .group(group.clone())
            .flex()
            .items_center()
            .gap(px(2.0))
            .ml(px(-6.0))
            .child(
                icon_action(
                    ("guide-copy", turn),
                    if copied { icons::CHECK } else { icons::COPY },
                    ink,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy(text.clone(), turn, None, cx);
                })),
            );
        if last {
            actions = actions.child(
                icon_action(("guide-again", turn), icons::REFRESH, ink).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.ask_again(turn, window, cx);
                    },
                )),
            );
        }
        if !answer.citations.is_empty() {
            let mut sites: Vec<String> = answer
                .citations
                .iter()
                .map(|citation| sources::site_name(&citation.url))
                .collect();
            sites.dedup();
            actions = actions.child(
                div()
                    .id(("guide-sources", turn))
                    .ml(px(4.0))
                    .h(px(28.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(14.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(ink.raised))
                    .child(div().flex().children(sites.iter().take(3).enumerate().map(
                        |(at, site)| {
                            div()
                                .size(px(16.0))
                                .ml(px(if at == 0 { 0.0 } else { -5.0 }))
                                .rounded_full()
                                .border_1()
                                .border_color(ink.glass)
                                .bg(ink.accent)
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(9.0))
                                .line_height(px(10.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(ink.glass)
                                .child(
                                    site.chars()
                                        .next()
                                        .unwrap_or('·')
                                        .to_uppercase()
                                        .to_string(),
                                )
                        },
                    )))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(ink.muted)
                            .child("Sources"),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sheet = Some(turn);
                        cx.notify();
                    }))
                    .children(probe::part(format!("button:sources.{turn}"))),
            );
        }
        actions = actions.child(
            div()
                .ml(px(6.0))
                .text_size(px(12.0))
                .text_color(ink.faint)
                .opacity(0.0)
                .group_hover(group, |style| style.opacity(1.0))
                .child(meta),
        );
        row.child(actions).into_any_element()
    }

    fn composer_area(&self, ink: &Ink, now: Instant, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.live.is_some();
        let empty = self.composer.read(cx).is_empty() && self.shots.is_empty();
        let full = self.shots.len() >= MOST_SHOTS;
        let thumbs = self.shots.iter().enumerate().map(|(index, shot)| {
            let fresh = now.saturating_duration_since(shot.taken) < FRESH;
            let image = shot.image.clone();
            div()
                .relative()
                .child(
                    thumbnail(("guide-shot", index), ink)
                        .when(fresh, |thumb| thumb.border_color(ink.accent))
                        .child(
                            img(image.clone())
                                .size_full()
                                .object_fit(ObjectFit::Contain),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.enlarged = Some(image.clone());
                            cx.notify();
                        }))
                        .children(probe::part(format!("thumb:shot.{index}"))),
                )
                .child(
                    div()
                        .id(("guide-shot-remove", index))
                        .absolute()
                        .top(px(-6.0))
                        .right(px(-6.0))
                        .size(px(18.0))
                        .rounded_full()
                        .bg(hsla([0.957; 3], 1.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .child(
                            icons::icon(icons::ADD, 11.0, 0)
                                .text_color(hsla([0.0; 3], 1.0))
                                .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                    std::f32::consts::FRAC_PI_4,
                                ))),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_shot(index, cx))),
                )
        });
        let send_bg = if live || !empty {
            hsla([0.957; 3], 1.0)
        } else {
            hsla([1.0; 3], 0.18)
        };
        let send = div()
            .id("guide-send")
            .size(px(32.0))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(send_bg)
            .when(live || !empty, |button| button.cursor_pointer())
            .when(live, |button| {
                button
                    .child(
                        div()
                            .size(px(11.0))
                            .rounded(px(2.5))
                            .bg(hsla([0.0; 3], 1.0)),
                    )
                    .children(probe::part("button:stop"))
            })
            .when(!live, |button| {
                button
                    .child(icons::icon(icons::ARROW_UP, 17.0, 0).text_color(if empty {
                        hsla([0.0; 3], 0.45)
                    } else {
                        hsla([0.0; 3], 1.0)
                    }))
                    .children(probe::part("button:send"))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.live.is_some() {
                    this.stop(cx);
                } else {
                    this.send(None, window, cx);
                }
            }));
        let screenshot = div()
            .id("guide-screenshot")
            .h(px(32.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded(px(16.0))
            .text_size(px(13.5))
            .text_color(if full { ink.faint } else { ink.accent })
            .when(!full, |chip| {
                chip.cursor_pointer()
                    .hover(|style| style.bg(ink.raised))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(GuideEvent::TakeShot)))
            })
            .child(icons::icon(icons::CAMERA, 16.0, 0).text_color(if full {
                ink.faint
            } else {
                ink.accent
            }))
            .child("Screenshot")
            .tooltip(move |_, cx| {
                cx.new(|_| {
                    Tip(if full {
                        "Four screenshots at most"
                    } else {
                        "Screenshot the game  ⌥⌘S"
                    })
                })
                .into()
            })
            .children(probe::part("button:screenshot"));
        div()
            .flex_none()
            .px(px(12.0))
            .pb(px(8.0))
            .pt(px(4.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .rounded(px(24.0))
                    .bg(ink.raised)
                    .border_1()
                    .border_color(ink.hairline)
                    .shadow(vec![BoxShadow {
                        color: hsla([0.0; 3], 0.25),
                        offset: point(px(0.0), px(4.0)),
                        blur_radius: px(16.0),
                        spread_radius: px(0.0),
                    }])
                    .p(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .when(!self.shots.is_empty(), |composer| {
                        composer.child(
                            div()
                                .flex()
                                .gap(px(8.0))
                                .px(px(6.0))
                                .pt(px(6.0))
                                .children(thumbs),
                        )
                    })
                    .child(div().px(px(8.0)).pt(px(4.0)).child(self.composer.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .child(
                                panel_button("guide-add-picture", icons::ADD, ink)
                                    .size(px(32.0))
                                    .rounded_full()
                                    .on_click(cx.listener(|this, _, _, cx| this.add_picture(cx)))
                                    .children(probe::part("button:add-picture")),
                            )
                            .child(screenshot)
                            .child(div().flex_1())
                            .child(send),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .line_height(px(14.0))
                    .text_color(ink.faint)
                    .flex()
                    .justify_center()
                    .child("Guide can make mistakes. Answers cite fan wikis."),
            )
    }

    fn history_menu(&self, ink: &Ink, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chats = self.history.as_ref()?;
        let rows = chats.iter().map(|summary| {
            let open = summary.id.clone();
            let delete = summary.id.clone();
            let current = summary.id == self.chat.id;
            div()
                .id(gpui::ElementId::Name(
                    format!("guide-chat-{}", summary.id).into(),
                ))
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .cursor_pointer()
                .when(current, |row| row.bg(ink.raised))
                .hover(|style| style.bg(ink.raised))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(13.5))
                                .child(summary.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(ink.faint)
                                .child(started(summary.started)),
                        ),
                )
                .child(
                    panel_button(
                        gpui::ElementId::Name(format!("guide-delete-{}", summary.id).into()),
                        icons::TRASH,
                        ink,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.delete_chat(&delete, cx);
                    })),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_chat(&open, cx)))
        });
        let mut menu_bg = ink.glass;
        menu_bg.a = 0.98;
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("guide-history-scrim")
                        .absolute()
                        .inset_0()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.history = None;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("guide-history")
                        .absolute()
                        .top(px(HEADER))
                        .left(px(10.0))
                        .right(px(10.0))
                        .max_h(relative(0.6))
                        .overflow_y_scroll()
                        .p(px(6.0))
                        .rounded(px(14.0))
                        .bg(menu_bg)
                        .border_1()
                        .border_color(ink.hairline)
                        .shadow(vec![BoxShadow {
                            color: hsla([0.0; 3], 0.4),
                            offset: point(px(0.0), px(8.0)),
                            blur_radius: px(24.0),
                            spread_radius: px(0.0),
                        }])
                        .when(chats.is_empty(), |menu| {
                            menu.child(
                                div()
                                    .px(px(10.0))
                                    .py(px(8.0))
                                    .text_size(px(13.5))
                                    .text_color(ink.muted)
                                    .child("No earlier chats"),
                            )
                        })
                        .children(rows)
                        .children(probe::part("menu:history")),
                )
                .into_any_element(),
        )
    }

    fn sources_sheet(&self, ink: &Ink, cx: &mut Context<Self>) -> Option<AnyElement> {
        let turn = self.sheet?;
        let answer = &self.chat.turns.get(turn)?.answer;
        let rows = answer.citations.iter().enumerate().map(|(at, citation)| {
            let url = citation.url.clone();
            div()
                .id(("guide-source", at))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(10.0))
                .cursor_pointer()
                .hover(|style| style.bg(ink.raised))
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .text_color(ink.muted)
                        .child(sources::site_name(&citation.url)),
                )
                .child(
                    div()
                        .text_size(px(13.5))
                        .line_height(px(19.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if citation.title.is_empty() {
                            citation.url.clone()
                        } else {
                            citation.title.clone()
                        }),
                )
                .when(!citation.excerpt.is_empty(), |row| {
                    row.child(
                        div()
                            .text_size(px(12.5))
                            .line_height(px(18.0))
                            .text_color(ink.faint)
                            .truncate()
                            .child(citation.excerpt.clone()),
                    )
                })
                .on_click(move |_, _, cx| cx.open_url(&url))
                .children(probe::part(format!("chip:source.{at}")))
        });
        let mut sheet_bg = ink.glass;
        sheet_bg.a = 1.0;
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .justify_end()
                .child(
                    div()
                        .id("guide-sheet-scrim")
                        .absolute()
                        .inset_0()
                        .bg(hsla([0.0; 3], 0.35))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sheet = None;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("guide-sheet")
                        .relative()
                        .max_h(relative(0.72))
                        .overflow_y_scroll()
                        .rounded_t(px(18.0))
                        .bg(sheet_bg)
                        .border_t_1()
                        .border_color(ink.hairline)
                        .p(px(8.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .px(px(12.0))
                                .py(px(6.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Sources"),
                                )
                                .child(close_button("guide-sheet-close", ink).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.sheet = None;
                                        cx.notify();
                                    }),
                                )),
                        )
                        .children(rows)
                        .children(probe::part("sheet:sources")),
                )
                .into_any_element(),
        )
    }

    fn enlarged_shot(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let image = self.enlarged.clone()?;
        Some(
            div()
                .id("guide-enlarged")
                .absolute()
                .inset_0()
                .bg(hsla([0.0; 3], 0.85))
                .p(px(16.0))
                .cursor_pointer()
                .child(img(image).size_full().object_fit(ObjectFit::Contain))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.enlarged = None;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }
}

/// "Searching Serebii, Bulbapedia"
fn searching_label(sites: &[String]) -> String {
    match sites {
        [] => "Searching".to_owned(),
        [one] => format!("Searching {one}"),
        [one, two, ..] => format!("Searching {one}, {two}"),
    }
}

/// When a chat started, as the chat list shows it.
fn started(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%-d %b, %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// A square icon button in the panel's colours.
fn panel_button(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    ink: &Ink,
) -> gpui::Stateful<gpui::Div> {
    icon_button(id, icons::icon(path, 16.0, 0).text_color(ink.muted), ink)
}

/// A ✕ button: the plus, turned.
fn close_button(id: impl Into<gpui::ElementId>, ink: &Ink) -> gpui::Stateful<gpui::Div> {
    let icon = icons::icon(icons::ADD, 16.0, 0)
        .text_color(ink.muted)
        .with_transformation(gpui::Transformation::rotate(gpui::radians(
            std::f32::consts::FRAC_PI_4,
        )));
    icon_button(id, icon, ink)
}

fn icon_button(
    id: impl Into<gpui::ElementId>,
    icon: gpui::Svg,
    ink: &Ink,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(|style| style.bg(ink.raised))
        .child(icon)
}

/// An action under an answer: a 16 pt icon in a 28 pt button.
fn icon_action(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    ink: &Ink,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(|style| style.bg(ink.raised))
        .child(icons::icon(path, 16.0, 0).text_color(ink.muted))
}

fn text_button(id: &'static str, label: &'static str, ink: &Ink) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(10.0))
        .py(px(3.0))
        .rounded(px(8.0))
        .cursor_pointer()
        .bg(ink.raised)
        .text_color(ink.text)
        .child(label)
}

/// A screenshot's frame: black, rounded, 52 pt tall.
fn thumbnail(id: impl Into<gpui::ElementId>, ink: &Ink) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(52.0))
        .w(px(52.0))
        .flex_none()
        .rounded(px(10.0))
        .overflow_hidden()
        .bg(hsla([0.0; 3], 1.0))
        .border(px(1.5))
        .border_color(ink.hairline)
        .cursor_pointer()
}

/// A tooltip's view.
struct Tip(&'static str);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(hsla([0.12; 3], 1.0))
            .text_size(px(12.0))
            .text_color(hsla([0.93; 3], 1.0))
            .child(self.0)
    }
}
