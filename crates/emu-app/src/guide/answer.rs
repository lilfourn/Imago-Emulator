//! Answers drawn as ChatGPT draws them: text straight on the panel, no
//! bubble, in the panel's colours, with links to the sites an answer drew
//! on shown as small source chips (PRD 6.7).

use std::ops::Range;
use std::time::Instant;

use emu_guide::markdown::{Align, Block, Link, Span};
use emu_guide::sources;
use gpui::{
    AnyElement, Context, ElementId, Font, FontStyle, FontWeight, Hsla, InteractiveText,
    SharedString, StrikethroughStyle, StyledText, TextRun, UnderlineStyle, div, prelude::*, px,
};

use super::palette::{Palette, hsla, mix};
use super::{GuidePanel, Veil};
use crate::icons;

/// Body text: 14.5 on 23.
pub const BODY: (f32, f32) = (14.5, 23.0);
/// Code: 12.5 on 18, in the monospaced face (Menlo on macOS).
const CODE: (f32, f32) = (12.5, 18.0);
/// Headings, by level.
const HEADINGS: [(f32, f32); 3] = [(17.0, 25.0), (15.0, 23.0), (14.5, 23.0)];
/// The thin space either side of a source chip's name.
const CHIP_PAD: char = '\u{2009}';

/// The colours answers are drawn in, from the panel's palette.
#[derive(Debug, Clone, Copy)]
pub struct Ink {
    /// Body text.
    pub text: Hsla,
    /// Secondary text.
    pub muted: Hsla,
    /// The least.
    pub faint: Hsla,
    /// List markers, quote rules, inline code's wash.
    pub accent: Hsla,
    /// Chips, code blocks, the question pill.
    pub raised: Hsla,
    /// Hairlines.
    pub hairline: Hsla,
    /// Errors.
    pub danger: Hsla,
    /// The panel.
    pub glass: Hsla,
}

impl Ink {
    /// The ink for `palette`.
    pub fn new(palette: &Palette) -> Self {
        Self {
            text: hsla(palette.text, 1.0),
            muted: hsla(palette.muted, 1.0),
            faint: hsla(palette.faint, 1.0),
            accent: hsla(palette.accent, 1.0),
            raised: hsla([1.0; 3], 0.075),
            hairline: hsla([1.0; 3], 0.08),
            danger: hsla(palette.danger, 1.0),
            glass: hsla(palette.glass, 1.0),
        }
    }

    /// Inline code's wash: the accent at 12% over the glass.
    fn code_wash(&self, palette_accent: Hsla) -> Hsla {
        let mut wash = palette_accent;
        wash.a = 0.14;
        wash
    }
}

/// How a block is being drawn: which turn and block it is, for ids, and
/// the fade over it, if any.
pub struct Drawing<'a> {
    /// The turn.
    pub turn: usize,
    /// The block's index in the answer.
    pub block: usize,
    /// The colours.
    pub ink: Ink,
    /// The panel's base font.
    pub font: Font,
    /// The fade over newly streamed text.
    pub veil: Option<&'a Veil>,
    /// Now, for the fade.
    pub now: Instant,
    /// Whether this block's code was just copied.
    pub copied: bool,
}

impl GuidePanel {
    /// One top-level block of an answer.
    pub(super) fn draw_block(
        &self,
        block: &Block,
        drawing: &Drawing,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut path = vec![drawing.block];
        self.block_element(block, drawing, &mut path, cx)
    }

    fn block_element(
        &self,
        block: &Block,
        drawing: &Drawing,
        path: &mut Vec<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ink = drawing.ink;
        match block {
            Block::Paragraph(spans) => text_block(spans, BODY, false, drawing, path, cx),
            Block::Pictures(pictures) => self.pictures_element(pictures, drawing, path, cx),
            Block::Heading { level, spans } => {
                let size = HEADINGS[usize::from(level.saturating_sub(1)).min(2)];
                text_block(spans, size, true, drawing, path, cx)
            }
            Block::List { start, items } => {
                let rows = items.iter().enumerate().map(|(index, item)| {
                    let marker = match start {
                        Some(first) => format!("{}.", first + index as u64),
                        None => "•".to_owned(),
                    };
                    path.push(index);
                    let content = item
                        .iter()
                        .enumerate()
                        .map(|(at, inner)| {
                            path.push(at);
                            let element = self.block_element(inner, drawing, path, cx);
                            path.pop();
                            element
                        })
                        .collect::<Vec<_>>();
                    path.pop();
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(if start.is_some() { 18.0 } else { 10.0 }))
                                .text_size(px(BODY.0))
                                .line_height(px(BODY.1))
                                .text_color(if start.is_some() {
                                    ink.muted
                                } else {
                                    ink.accent
                                })
                                .child(marker),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .children(content),
                        )
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .children(rows.collect::<Vec<_>>())
                    .into_any_element()
            }
            Block::Quote(blocks) => {
                let content = blocks
                    .iter()
                    .enumerate()
                    .map(|(at, inner)| {
                        path.push(at);
                        let element = self.block_element(inner, drawing, path, cx);
                        path.pop();
                        element
                    })
                    .collect::<Vec<_>>();
                div()
                    .border_l_2()
                    .border_color(ink.accent)
                    .pl(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .text_color(ink.muted)
                    .children(content)
                    .into_any_element()
            }
            Block::Code { language, text } => {
                self.code_block(language.as_deref(), text, drawing, cx)
            }
            Block::Table { aligns, head, rows } => table(aligns, head, rows, drawing, path),
            Block::Rule => div()
                .h(px(1.0))
                .my(px(6.0))
                .bg(ink.hairline)
                .into_any_element(),
        }
    }

    fn code_block(
        &self,
        language: Option<&str>,
        text: &str,
        drawing: &Drawing,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ink = drawing.ink;
        let (turn, block) = (drawing.turn, drawing.block);
        let code = text.trim_end_matches('\n').to_owned();
        let copied = drawing.copied;
        div()
            .rounded(px(10.0))
            .bg(ink.raised)
            .border_1()
            .border_color(ink.hairline)
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.0))
                    .pt(px(4.0))
                    .text_size(px(11.0))
                    .text_color(ink.faint)
                    .child(language.unwrap_or_default().to_owned())
                    .child(
                        div()
                            .id(("guide-code-copy", turn * 1000 + block))
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(|style| style.bg(ink.raised))
                            .child(icons::icon(icons::COPY, 13.0, 0).text_color(ink.muted))
                            .child(if copied { "Copied" } else { "Copy" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy(code.clone(), turn, Some(block), cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id(("guide-code", turn * 1000 + block))
                    .overflow_x_scroll()
                    .px(px(12.0))
                    .pb(px(10.0))
                    .pt(px(2.0))
                    .font_family(crate::fonts::MONO)
                    .text_size(px(CODE.0))
                    .line_height(px(CODE.1))
                    .text_color(ink.text)
                    .child(
                        div()
                            .whitespace_nowrap()
                            .child(text.trim_end_matches('\n').to_owned()),
                    ),
            )
            .into_any_element()
    }
}

/// A paragraph or heading: its spans as one run of text.
fn text_block(
    spans: &[Span],
    (size, line): (f32, f32),
    heading: bool,
    drawing: &Drawing,
    path: &[usize],
    _: &mut Context<GuidePanel>,
) -> AnyElement {
    let mut font = drawing.font.clone();
    if heading {
        font.weight = FontWeight::SEMIBOLD;
    }
    let inline = inline(spans, &drawing.ink, &font, drawing.veil, drawing.now);
    let id = ElementId::Name(
        format!(
            "guide-text-{}-{}",
            drawing.turn,
            path.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(".")
        )
        .into(),
    );
    div()
        .text_size(px(size))
        .line_height(px(line))
        .child(clickable(id, inline))
        .into_any_element()
}

/// Text with its runs, and the links in it.
pub struct Inline {
    text: SharedString,
    runs: Vec<TextRun>,
    links: Vec<(Range<usize>, String)>,
}

/// The text, its links opening in the browser.
fn clickable(id: ElementId, inline: Inline) -> AnyElement {
    let styled = StyledText::new(inline.text).with_runs(inline.runs);
    if inline.links.is_empty() {
        return styled.into_any_element();
    }
    let (ranges, urls): (Vec<_>, Vec<_>) = inline.links.into_iter().unzip();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |index, _, cx| {
            if let Some(url) = urls.get(index) {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

/// Whether a link reads as a source chip: its text is the name of the
/// site it points to, as the prompt asks the model to cite.
fn is_chip(span: &Span) -> Option<&str> {
    let Some(Link::Url(url)) = &span.link else {
        return None;
    };
    let text = span.text.trim();
    let site = sources::site_name(url);
    let same = text.eq_ignore_ascii_case(&site)
        || text.trim_end_matches(" +1").eq_ignore_ascii_case(&site)
        || (text.chars().count() <= 24 && site.to_lowercase().contains(&text.to_lowercase()));
    same.then_some(url.as_str())
}

/// `spans` as text runs in `ink`, fading in under `veil`.
pub fn inline(spans: &[Span], ink: &Ink, base: &Font, veil: Option<&Veil>, now: Instant) -> Inline {
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut links = Vec::new();
    for span in spans {
        let chip = is_chip(span);
        let mut font = base.clone();
        if span.style.bold {
            font.weight = FontWeight::SEMIBOLD;
        }
        if span.style.italic {
            font.style = FontStyle::Italic;
        }
        if span.style.code {
            font.family = crate::fonts::MONO.into();
        }
        let (color, background) = if chip.is_some() {
            (ink.muted, Some(ink.raised))
        } else if span.style.code {
            (ink.text, Some(ink.code_wash(ink.accent)))
        } else {
            (ink.text, None)
        };
        let underline = match (&span.link, chip) {
            (Some(_), None) => Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(ink.faint),
                wavy: false,
            }),
            _ => None,
        };
        let strikethrough = span.style.strike.then_some(StrikethroughStyle {
            thickness: px(1.0),
            color: Some(ink.text),
        });
        let shown = if chip.is_some() {
            format!("{CHIP_PAD}{}{CHIP_PAD}", span.text.trim())
        } else {
            span.text.clone()
        };
        let start = text.len();
        // Where the fade changes inside the span, mapped from the source
        // to the shown text.
        let mut cuts: Vec<usize> = veil
            .map(|veil| {
                veil.cuts(span.range.clone())
                    .into_iter()
                    .map(|cut| floor_boundary(&shown, cut - span.range.start))
                    .filter(|cut| *cut > 0 && *cut < shown.len())
                    .collect()
            })
            .unwrap_or_default();
        cuts.dedup();
        let mut piece_start = 0;
        for piece_end in cuts.into_iter().chain([shown.len()]) {
            if piece_end <= piece_start {
                continue;
            }
            let alpha = veil.map_or(1.0, |veil| veil.alpha(span.range.start + piece_start, now));
            let fade = |mut colour: Hsla| {
                colour.a *= alpha;
                colour
            };
            runs.push(TextRun {
                len: piece_end - piece_start,
                font: font.clone(),
                color: fade(color),
                background_color: background.map(fade),
                underline: underline.map(|mut underline| {
                    underline.color = underline.color.map(fade);
                    underline
                }),
                strikethrough,
            });
            piece_start = piece_end;
        }
        text.push_str(&shown);
        if let Some(Link::Url(url)) = &span.link {
            links.push((start..text.len(), url.clone()));
        }
    }
    if runs.is_empty() {
        runs.push(TextRun {
            len: 0,
            font: base.clone(),
            color: ink.text,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    Inline {
        text: text.into(),
        runs,
        links,
    }
}

/// The largest char boundary in `text` at or before `at`.
fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// A table, as flat hairline rows, scrolling sideways in the narrow
/// panel. Columns are as wide as their longest cell, within bounds.
fn table(
    aligns: &[Align],
    head: &[Vec<Span>],
    rows: &[Vec<Vec<Span>>],
    drawing: &Drawing,
    path: &[usize],
) -> AnyElement {
    let ink = drawing.ink;
    let columns = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let length = |cell: &[Span]| {
        cell.iter()
            .map(|span| span.text.chars().count())
            .sum::<usize>()
    };
    let widths: Vec<f32> = (0..columns)
        .map(|column| {
            let longest = std::iter::once(head.get(column).map_or(0, |cell| length(cell)))
                .chain(
                    rows.iter()
                        .map(|row| row.get(column).map_or(0, |cell| length(cell))),
                )
                .max()
                .unwrap_or(0);
            (longest as f32 * 8.4 + 24.0).clamp(64.0, 220.0)
        })
        .collect();
    let id_base = format!(
        "guide-table-{}-{}",
        drawing.turn,
        path.iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(".")
    );
    let row_element = |cells: &[Vec<Span>], header: bool, row: usize| {
        let mut font = drawing.font.clone();
        if header {
            font.weight = FontWeight::SEMIBOLD;
        }
        div()
            .flex()
            .border_b_1()
            .border_color(ink.hairline)
            .children((0..columns).map(|column| {
                let spans = cells.get(column).map_or(&[][..], Vec::as_slice);
                let inline = inline(spans, &ink, &font, drawing.veil, drawing.now);
                let cell = div()
                    .w(px(widths[column]))
                    .flex_none()
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_size(px(13.5))
                    .line_height(px(19.0))
                    .text_color(ink.text);
                let cell = match aligns.get(column) {
                    Some(Align::Right) => cell.flex().justify_end(),
                    Some(Align::Center) => cell.flex().justify_center(),
                    _ => cell,
                };
                cell.child(clickable(
                    ElementId::Name(format!("{id_base}-{row}-{column}").into()),
                    inline,
                ))
            }))
    };
    div()
        .id(ElementId::Name(id_base.clone().into()))
        .overflow_x_scroll()
        .w_full()
        .child(
            div()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(ink.hairline)
                .child(row_element(head, true, 0))
                .children(
                    rows.iter()
                        .enumerate()
                        .map(|(index, row)| row_element(row, false, index + 1)),
                ),
        )
        .into_any_element()
}

/// A shimmering line of muted text: a brighter band sweeps across it,
/// `phase` (0 to 1) of the way, as ChatGPT's working line does.
pub fn shimmer(text: &str, phase: f32, ink: &Ink, font: &Font) -> StyledText {
    let count = text.chars().count().max(1) as f32;
    let centre = phase * 1.6 - 0.3;
    let runs = text
        .char_indices()
        .enumerate()
        .map(|(index, (_, c))| {
            let x = (index as f32 + 0.5) / count;
            let d = (x - centre) / 0.16;
            let glow = (-d * d).exp();
            let color = mix_hsla(ink.muted, ink.text, glow);
            TextRun {
                len: c.len_utf8(),
                font: font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }
        })
        .collect();
    StyledText::new(SharedString::from(text.to_owned())).with_runs(runs)
}

fn mix_hsla(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let a = gpui::Rgba::from(a);
    let b = gpui::Rgba::from(b);
    let mixed = mix([a.r, a.g, a.b], [b.r, b.g, b.b], t);
    hsla(mixed, a.a + (b.a - a.a) * t)
}
