//! Markdown answers as blocks the panel draws, parsed as they stream.
//!
//! [`Document::push`] appends a delta and parses again only from the line
//! the last top-level block starts on; the blocks before it are kept as
//! they were. While the last block's first line is still coming in, the
//! block before it is parsed again too, as the rest of that line can join
//! the two: `2` after a list becomes its next item once `.` arrives.
//!
//! While streaming, the last block's last paragraph, heading, table cell
//! or list item text is mended for display, as Imago does: an unclosed
//! `**bold` shows bold at once, and a link whose URL is still coming shows
//! as a [`Link::Pending`]. Code is never mended. [`Document::settle`]
//! parses the last block again as written, so a marker that never closed
//! shows as written.
//!
//! An image is never drawn: its alt text shows in its place, as text. Raw
//! HTML shows as written. A link reference definition links text before
//! it too, so a document with any is parsed whole once more when it
//! settles; while streaming, only text parsed with the definition links.

use std::ops::Range;

use pulldown_cmark::{
    Alignment, CodeBlockKind, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd,
};

mod mend;

/// A link's target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// A finished URL.
    Url(String),
    /// A link whose URL is still streaming: drawn as a link, not clickable.
    Pending,
}

/// How a run of text is styled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    /// `**bold**`
    pub bold: bool,
    /// `*italic*`
    pub italic: bool,
    /// `~~strike~~`
    pub strike: bool,
    /// `` `code` ``
    pub code: bool,
}

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The text shown. A hard line break is `\n`; a soft one a space.
    pub text: String,
    /// Its style.
    pub style: Style,
    /// Where it links, if it is link text.
    pub link: Option<Link>,
    /// The bytes of the document's source this text came from, for the
    /// fade-in of newly streamed text.
    pub range: Range<usize>,
}

/// A column's alignment in a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Not given.
    None,
    /// `:--`
    Left,
    /// `:-:`
    Center,
    /// `--:`
    Right,
}

/// One block of an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A paragraph.
    Paragraph(Vec<Span>),
    /// A heading, level 1 to 6.
    Heading {
        /// 1 to 6.
        level: u8,
        /// Its text.
        spans: Vec<Span>,
    },
    /// A list; `start` is the first number of an ordered list.
    List {
        /// `Some(n)` for an ordered list starting at n.
        start: Option<u64>,
        /// Each item's blocks.
        items: Vec<Vec<Block>>,
    },
    /// A block quote.
    Quote(Vec<Block>),
    /// Fenced or indented code.
    Code {
        /// The fence's info string's first word, if any.
        language: Option<String>,
        /// The code, without the fences.
        text: String,
    },
    /// A table.
    Table {
        /// Per column.
        aligns: Vec<Align>,
        /// The header row's cells.
        head: Vec<Vec<Span>>,
        /// The body rows' cells.
        rows: Vec<Vec<Vec<Span>>>,
    },
    /// A thematic break.
    Rule,
}

/// An answer's Markdown and its blocks, kept up to date as text streams in.
#[derive(Debug, Clone, Default)]
pub struct Document {
    source: String,
    blocks: Vec<Block>,
    /// Where each top-level block of the source as written starts. The
    /// blocks before the last are the first of `blocks`, one each; the
    /// last may be shown mended, as more or fewer blocks.
    starts: Vec<usize>,
    /// Whether the source defines link references.
    defines: bool,
    /// The bytes the last push or settle parsed.
    #[cfg(test)]
    parsed: usize,
}

impl Document {
    /// An empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// A finished document parsed whole, as a saved answer is.
    pub fn parse(source: &str) -> Self {
        let mut document = Self {
            source: source.to_owned(),
            ..Self::default()
        };
        document.reparse(0, false);
        document
    }

    /// Appends streamed text. Only the last top-level block is parsed
    /// again (and the one before it, while the last one's first line is
    /// unfinished), with hanging markers mended for display.
    pub fn push(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let from = self.push_from();
        self.source.push_str(delta);
        self.reparse(from, true);
    }

    /// The stream has ended: parses the last block as written, unmended.
    pub fn settle(&mut self) {
        let from = match self.starts.last() {
            Some(&last) if !self.defines => line_start(&self.source, last),
            _ => 0,
        };
        self.reparse(from, false);
    }

    /// The Markdown so far.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The blocks so far.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Where the next push parses from: the line the last block starts on,
    /// or, while that block's first line is unfinished, the line the block
    /// before it starts on.
    fn push_from(&self) -> usize {
        let start = match *self.starts.as_slice() {
            [] => 0,
            [.., last] if self.source[last..].contains('\n') => last,
            [.., before, _] => before,
            [only] => only,
        };
        line_start(&self.source, start)
    }

    /// Parses the source from `from`, a line where a top-level block
    /// starts, keeping the blocks before it.
    fn reparse(&mut self, from: usize, streaming: bool) {
        let kept = self.starts.partition_point(|&start| start < from);
        self.starts.truncate(kept);
        self.blocks.truncate(kept);
        let tail = parse_tail(&self.source, from, streaming);
        self.defines |= tail.defines;
        self.starts.extend(tail.starts);
        self.blocks.extend(tail.blocks);
        #[cfg(test)]
        {
            self.parsed = tail.parsed;
        }
    }
}

/// The source from some line on, parsed.
struct Tail {
    blocks: Vec<Block>,
    /// Where each top-level block starts, as written.
    starts: Vec<usize>,
    /// Whether it defines link references.
    defines: bool,
    /// The bytes parsed, mended text included.
    #[cfg_attr(not(test), allow(dead_code))]
    parsed: usize,
}

/// Parses `source` from `from` on. While `streaming`, the last top-level
/// block's last leaf is mended for display.
fn parse_tail(source: &str, from: usize, streaming: bool) -> Tail {
    let text = &source[from..];
    let (events, defines) = parse_events(text, &|at| from + at, streaming);
    let tops = tops(&events);
    let starts = tops.iter().map(|&(_, start)| start).collect();
    let mut tail = Tail {
        blocks: Vec::new(),
        starts,
        defines,
        parsed: text.len(),
    };
    if streaming
        && let Some(&(first, start)) = tops.last()
        && let Some(leaf) = last_leaf(&events[first..])
        && let Some(mended) = mend::close_hanging(&source[leaf.clone()])
    {
        // Mend only the last block's text, parsing it on its own: the
        // blocks before it are as written.
        let line = line_start(source, start);
        let mended = mended.within(&source[line..], leaf.start - line..leaf.end - line);
        tail.parsed += mended.text.len();
        tail.blocks = Builder::new(&events[..first], streaming).blocks();
        let (last, _) = parse_events(&mended.text, &|at| line + mended.original(at), streaming);
        let mut last = Builder::new(&last, streaming).blocks();
        if mended.zwsp {
            strip_zwsp(&mut last);
        }
        tail.blocks.extend(last);
        return tail;
    }
    tail.blocks = Builder::new(&events, streaming).blocks();
    tail
}

/// Events with their ranges in the document.
type Events<'a> = Vec<(Event<'a>, Range<usize>)>;

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH
}

/// Parses `text`, `at` turning each offset in it into one in the document.
/// Also says whether `text` defines link references. While `streaming`, a
/// reference link whose definition is on the last, unfinished line points
/// to [`mend::PENDING`], as its URL may be cut short.
fn parse_events<'a>(
    text: &'a str,
    at: &dyn Fn(usize) -> usize,
    streaming: bool,
) -> (Events<'a>, bool) {
    let mut parser = Parser::new_ext(text, options()).into_offset_iter();
    let mut events: Events<'a> = parser
        .by_ref()
        .map(|(event, range)| (event, at(range.start)..at(range.end)))
        .collect();
    let definitions = parser.reference_definitions();
    let defines = definitions.iter().next().is_some();
    if streaming && defines {
        for (event, _) in &mut events {
            if let Event::Start(Tag::Link {
                link_type: LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut,
                dest_url,
                id,
                ..
            }) = event
                && definitions
                    .get(id)
                    .is_some_and(|definition| !text[definition.span.end..].contains('\n'))
            {
                *dest_url = CowStr::Borrowed(mend::PENDING);
            }
        }
    }
    (events, defines)
}

/// Each top-level block's first event, and where the block starts.
fn tops(events: &Events) -> Vec<(usize, usize)> {
    let mut depth = 0usize;
    let mut tops = Vec::new();
    for (index, (event, range)) in events.iter().enumerate() {
        match event {
            Event::Start(_) => {
                if depth == 0 {
                    tops.push((index, range.start));
                }
                depth += 1;
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Rule if depth == 0 => tops.push((index, range.start)),
            _ => {}
        }
    }
    tops
}

/// The source of the last leaf block in `events` (a paragraph, heading,
/// table cell or list item's text), where markers may hang. `None` when
/// the last leaf is code, HTML or a rule, or there is none.
fn last_leaf(events: &[(Event, Range<usize>)]) -> Option<Range<usize>> {
    // The leaf, and whether it holds inline text.
    let mut leaf: Option<(Range<usize>, bool)> = None;
    // Inside a paragraph, heading, cell, code or HTML block.
    let mut inside = false;
    // A tight list item's text, in no paragraph, is open.
    let mut bare = false;
    for (event, range) in events {
        match event {
            Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::TableCell) => {
                leaf = Some((range.clone(), true));
                inside = true;
            }
            Event::Start(Tag::CodeBlock(_) | Tag::HtmlBlock) => {
                leaf = Some((range.clone(), false));
                inside = true;
            }
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::TableCell
                | TagEnd::CodeBlock
                | TagEnd::HtmlBlock,
            ) => inside = false,
            Event::Rule => leaf = Some((range.clone(), false)),
            event if is_inline(event) => {
                if !inside {
                    match &mut leaf {
                        Some((text, true)) if bare => text.end = range.end,
                        _ => leaf = Some((range.clone(), true)),
                    }
                    bare = true;
                }
                continue;
            }
            _ => {}
        }
        bare = false;
    }
    leaf.and_then(|(range, inline)| inline.then_some(range))
}

/// Where the line holding `at` starts, if only spaces come before `at` on
/// it; else `at`. A block parsed on its own from there keeps its
/// indentation.
fn line_start(source: &str, at: usize) -> usize {
    let start = source[..at].rfind('\n').map_or(0, |newline| newline + 1);
    if source[start..at].bytes().all(|b| b == b' ' || b == b'\t') {
        start
    } else {
        at
    }
}

/// Whether an event belongs in a run of text.
fn is_inline(event: &Event) -> bool {
    match event {
        Event::Start(tag) => matches!(
            tag,
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
        ),
        Event::End(end) => matches!(
            end,
            TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image
        ),
        Event::Rule => false,
        _ => true,
    }
}

/// Drops the zero-width space a mend added from the end of the last text.
fn strip_zwsp(blocks: &mut [Block]) {
    if let Some(spans) = blocks.last_mut().and_then(last_spans)
        && let Some(span) = spans.last_mut()
        && let Some(text) = span.text.strip_suffix('\u{200B}')
    {
        span.text.truncate(text.len());
        if span.text.is_empty() {
            spans.pop();
        }
    }
}

/// The last run of text in a block.
fn last_spans(block: &mut Block) -> Option<&mut Vec<Span>> {
    match block {
        Block::Paragraph(spans) | Block::Heading { spans, .. } => Some(spans),
        Block::List { items, .. } => items.last_mut()?.last_mut().and_then(last_spans),
        Block::Quote(blocks) => blocks.last_mut().and_then(last_spans),
        Block::Table { head, rows, .. } => match rows.last_mut() {
            Some(row) => row.last_mut(),
            None => head.last_mut(),
        },
        Block::Code { .. } | Block::Rule => None,
    }
}

/// Builds blocks from events.
struct Builder<'a, 'e> {
    events: &'a [(Event<'e>, Range<usize>)],
    next: usize,
    /// Links to [`mend::PENDING`] are [`Link::Pending`].
    streaming: bool,
}

/// The styles and link open at a point in a run of text.
#[derive(Default)]
struct Inline {
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<Link>,
}

impl Inline {
    fn style(&self, code: bool) -> Style {
        Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code,
        }
    }
}

impl<'a, 'e> Builder<'a, 'e> {
    fn new(events: &'a [(Event<'e>, Range<usize>)], streaming: bool) -> Self {
        Self {
            events,
            next: 0,
            streaming,
        }
    }

    fn peek(&self) -> Option<&'a (Event<'e>, Range<usize>)> {
        self.events.get(self.next)
    }

    fn bump(&mut self) -> Option<&'a (Event<'e>, Range<usize>)> {
        let event = self.events.get(self.next);
        self.next += 1;
        event
    }

    /// One block for each top-level start or rule, as [`tops`] counts them.
    fn blocks(mut self) -> Vec<Block> {
        let mut blocks = Vec::new();
        while let Some((event, _)) = self.bump() {
            match event {
                Event::Start(tag) => blocks.push(self.started(tag)),
                Event::Rule => blocks.push(Block::Rule),
                _ => {}
            }
        }
        blocks
    }

    /// The block whose start was just read, through its end.
    fn started(&mut self, tag: &'a Tag<'e>) -> Block {
        match tag {
            Tag::Paragraph => Block::Paragraph(self.leaf()),
            Tag::Heading { level, .. } => Block::Heading {
                level: *level as u8,
                spans: self.leaf(),
            },
            Tag::BlockQuote(_) => Block::Quote(self.children()),
            Tag::List(start) => Block::List {
                start: *start,
                items: self.items(),
            },
            Tag::CodeBlock(kind) => self.code(kind),
            Tag::Table(aligns) => self.table(aligns),
            Tag::HtmlBlock => {
                let mut spans = self.leaf();
                if let Some(last) = spans.last_mut() {
                    let kept = last.text.trim_end_matches(['\n', '\r']).len();
                    let cut = last.text.len() - kept;
                    last.text.truncate(kept);
                    last.range.end -= cut.min(last.range.len());
                    if last.text.is_empty() {
                        spans.pop();
                    }
                }
                Block::Paragraph(spans)
            }
            // The rest are off in `options` or never at block level.
            _ => Block::Paragraph(self.leaf()),
        }
    }

    /// The rest of a leaf block: its text, through its end.
    fn leaf(&mut self) -> Vec<Span> {
        let spans = self.spans();
        self.finish();
        spans
    }

    /// Skips through the end of the element whose start was just read.
    fn finish(&mut self) {
        let mut depth = 1usize;
        while let Some((event, _)) = self.bump() {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// The blocks in a quote or list item, through its end. Text outside a
    /// paragraph, as in a tight list's items, is a paragraph.
    fn children(&mut self) -> Vec<Block> {
        let mut blocks = Vec::new();
        while let Some((event, _)) = self.peek() {
            match event {
                event if is_inline(event) => blocks.push(Block::Paragraph(self.spans())),
                Event::Start(tag) => {
                    self.next += 1;
                    blocks.push(self.started(tag));
                }
                Event::Rule => {
                    self.next += 1;
                    blocks.push(Block::Rule);
                }
                _ => {
                    self.next += 1;
                    break;
                }
            }
        }
        blocks
    }

    /// A list's items, through its end.
    fn items(&mut self) -> Vec<Vec<Block>> {
        let mut items = Vec::new();
        while let Some((event, _)) = self.bump() {
            match event {
                Event::Start(Tag::Item) => items.push(self.children()),
                Event::Start(_) => self.finish(),
                Event::End(_) => break,
                _ => {}
            }
        }
        items
    }

    /// A code block, through its end.
    fn code(&mut self, kind: &CodeBlockKind) -> Block {
        let language = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_owned),
            CodeBlockKind::Indented => None,
        };
        let mut text = String::new();
        while let Some((event, _)) = self.bump() {
            match event {
                Event::Text(code) => text.push_str(code),
                Event::Start(_) => self.finish(),
                Event::End(_) => break,
                _ => {}
            }
        }
        if text.ends_with('\n') {
            text.pop();
        }
        Block::Code { language, text }
    }

    /// A table, through its end.
    fn table(&mut self, aligns: &[Alignment]) -> Block {
        let aligns = aligns
            .iter()
            .map(|align| match align {
                Alignment::None => Align::None,
                Alignment::Left => Align::Left,
                Alignment::Center => Align::Center,
                Alignment::Right => Align::Right,
            })
            .collect();
        let mut head = Vec::new();
        let mut rows = Vec::new();
        while let Some((event, _)) = self.bump() {
            match event {
                Event::Start(Tag::TableHead) => head = self.cells(),
                Event::Start(Tag::TableRow) => rows.push(self.cells()),
                Event::Start(_) => self.finish(),
                Event::End(_) => break,
                _ => {}
            }
        }
        Block::Table { aligns, head, rows }
    }

    /// A row's cells, through its end.
    fn cells(&mut self) -> Vec<Vec<Span>> {
        let mut cells = Vec::new();
        while let Some((event, _)) = self.bump() {
            match event {
                Event::Start(Tag::TableCell) => cells.push(self.leaf()),
                Event::Start(_) => self.finish(),
                Event::End(_) => break,
                _ => {}
            }
        }
        cells
    }

    /// The run of text from here, as spans. An image adds nothing of its
    /// own, so its alt text reads as text.
    fn spans(&mut self) -> Vec<Span> {
        let mut inline = Inline::default();
        let mut spans = Vec::new();
        while let Some((event, range)) = self.peek()
            && is_inline(event)
        {
            self.next += 1;
            let text = match event {
                Event::Start(tag) => {
                    match tag {
                        Tag::Emphasis => inline.italic += 1,
                        Tag::Strong => inline.bold += 1,
                        Tag::Strikethrough => inline.strike += 1,
                        Tag::Link {
                            link_type,
                            dest_url,
                            ..
                        } => inline.links.push(self.link(*link_type, dest_url)),
                        _ => {}
                    }
                    continue;
                }
                Event::End(end) => {
                    match end {
                        TagEnd::Emphasis => inline.italic = inline.italic.saturating_sub(1),
                        TagEnd::Strong => inline.bold = inline.bold.saturating_sub(1),
                        TagEnd::Strikethrough => inline.strike = inline.strike.saturating_sub(1),
                        TagEnd::Link => {
                            inline.links.pop();
                        }
                        _ => {}
                    }
                    continue;
                }
                Event::Code(code) => {
                    push(&mut spans, code, inline.style(true), &inline, range);
                    continue;
                }
                Event::Text(text)
                | Event::Html(text)
                | Event::InlineHtml(text)
                | Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::FootnoteReference(text) => text.as_ref(),
                Event::SoftBreak => " ",
                Event::HardBreak => "\n",
                Event::Rule | Event::TaskListMarker(_) => continue,
            };
            push(&mut spans, text, inline.style(false), &inline, range);
        }
        spans
    }

    fn link(&self, link_type: LinkType, url: &str) -> Link {
        match link_type {
            _ if self.streaming && url == mend::PENDING => Link::Pending,
            LinkType::Email => Link::Url(format!("mailto:{url}")),
            _ => Link::Url(url.to_owned()),
        }
    }
}

/// Adds text to a run, joining it to the span before when they look the
/// same.
fn push(spans: &mut Vec<Span>, text: &str, style: Style, inline: &Inline, range: &Range<usize>) {
    if text.is_empty() {
        return;
    }
    let link = inline.links.last();
    if let Some(last) = spans.last_mut()
        && last.style == style
        && last.link.as_ref() == link
    {
        last.text.push_str(text);
        last.range.end = last.range.end.max(range.end);
        return;
    }
    spans.push(Span {
        text: text.to_owned(),
        style,
        link: link.cloned(),
        range: range.clone(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str, range: Range<usize>) -> Span {
        styled(text, Style::default(), range)
    }

    fn styled(text: &str, style: Style, range: Range<usize>) -> Span {
        Span {
            text: text.to_owned(),
            style,
            link: None,
            range,
        }
    }

    fn linked(text: &str, link: Link, range: Range<usize>) -> Span {
        Span {
            link: Some(link),
            ..plain(text, range)
        }
    }

    const BOLD: Style = Style {
        bold: true,
        italic: false,
        strike: false,
        code: false,
    };

    const ITALIC: Style = Style {
        bold: false,
        italic: true,
        strike: false,
        code: false,
    };

    fn parsed(source: &str) -> Vec<Block> {
        Document::parse(source).blocks().to_vec()
    }

    /// Every span in `blocks`, in order.
    fn all_spans(blocks: &[Block]) -> Vec<&Span> {
        fn walk<'b>(block: &'b Block, out: &mut Vec<&'b Span>) {
            match block {
                Block::Paragraph(spans) | Block::Heading { spans, .. } => out.extend(spans),
                Block::List { items, .. } => items.iter().flatten().for_each(|b| walk(b, out)),
                Block::Quote(blocks) => blocks.iter().for_each(|b| walk(b, out)),
                Block::Table { head, rows, .. } => {
                    out.extend(head.iter().flatten());
                    out.extend(rows.iter().flatten().flatten());
                }
                Block::Code { .. } | Block::Rule => {}
            }
        }
        let mut out = Vec::new();
        blocks.iter().for_each(|b| walk(b, &mut out));
        out
    }

    /// `text` in pieces of about `size` bytes, cut on char boundaries.
    fn chunks(text: &str, size: usize) -> Vec<&str> {
        let mut pieces = Vec::new();
        let mut at = 0;
        while at < text.len() {
            let mut end = (at + size).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            pieces.push(&text[at..end]);
            at = end;
        }
        pieces
    }

    #[test]
    fn paragraphs_carry_styles_breaks_and_absolute_ranges() {
        let source = "Use **Surf** and *Fly*, ~~Dig~~ `HM05`.\nSoft  \nhard";
        let strike = Style {
            strike: true,
            ..Style::default()
        };
        let code = Style {
            code: true,
            ..Style::default()
        };
        assert_eq!(
            parsed(source),
            [Block::Paragraph(vec![
                plain("Use ", 0..4),
                styled("Surf", BOLD, 6..10),
                plain(" and ", 12..17),
                styled("Fly", ITALIC, 18..21),
                plain(", ", 22..24),
                styled("Dig", strike, 26..29),
                plain(" ", 31..32),
                styled("HM05", code, 32..38),
                // One run across the soft and the hard break.
                plain(". Soft\nhard", 38..51),
            ])]
        );
    }

    #[test]
    fn headings_keep_their_level() {
        assert_eq!(
            parsed("# One\n\n### Three\n\nTwo\n---\n"),
            [
                Block::Heading {
                    level: 1,
                    spans: vec![plain("One", 2..5)],
                },
                Block::Heading {
                    level: 3,
                    spans: vec![plain("Three", 11..16)],
                },
                Block::Heading {
                    level: 2,
                    spans: vec![plain("Two", 18..21)],
                },
            ]
        );
    }

    #[test]
    fn lists_nest_and_tight_items_are_paragraphs() {
        assert_eq!(
            parsed("- a\n  - b\n- c\n\n3. x\n4. y\n"),
            [
                Block::List {
                    start: None,
                    items: vec![
                        vec![
                            Block::Paragraph(vec![plain("a", 2..3)]),
                            Block::List {
                                start: None,
                                items: vec![vec![Block::Paragraph(vec![plain("b", 8..9)])]],
                            },
                        ],
                        vec![Block::Paragraph(vec![plain("c", 12..13)])],
                    ],
                },
                Block::List {
                    start: Some(3),
                    items: vec![
                        vec![Block::Paragraph(vec![plain("x", 18..19)])],
                        vec![Block::Paragraph(vec![plain("y", 23..24)])],
                    ],
                },
            ]
        );
        // A loose list's items hold paragraphs of their own.
        assert_eq!(
            parsed("1. a\n\n2. b\n"),
            [Block::List {
                start: Some(1),
                items: vec![
                    vec![Block::Paragraph(vec![plain("a", 3..4)])],
                    vec![Block::Paragraph(vec![plain("b", 9..10)])],
                ],
            }]
        );
    }

    #[test]
    fn quotes_hold_blocks() {
        assert_eq!(
            parsed("> **Tip:** save first.\n>\n> - often\n"),
            [Block::Quote(vec![
                Block::Paragraph(vec![
                    styled("Tip:", BOLD, 4..8),
                    plain(" save first.", 10..22)
                ]),
                Block::List {
                    start: None,
                    items: vec![vec![Block::Paragraph(vec![plain("often", 29..34)])]],
                },
            ])]
        );
    }

    #[test]
    fn code_keeps_its_text_and_first_info_word() {
        assert_eq!(
            parsed("```rust ignore\nfn a() {}\n  **b**\n```\n\n    indented\n    more\n"),
            [
                Block::Code {
                    language: Some("rust".to_owned()),
                    text: "fn a() {}\n  **b**".to_owned(),
                },
                Block::Code {
                    language: None,
                    text: "indented\nmore".to_owned(),
                },
            ]
        );
        assert_eq!(
            parsed("~~~\nplain\n~~~"),
            [Block::Code {
                language: None,
                text: "plain".to_owned(),
            }]
        );
    }

    #[test]
    fn tables_have_aligns_a_head_and_rows() {
        let source = "| Pokémon | Type | Lv |\n|:--|:-:|--:|\n| Pikachu | **Electric** | 5 |\n";
        let at = |s: &str| {
            let start = source.find(s).unwrap();
            start..start + s.len()
        };
        assert_eq!(
            parsed(source),
            [Block::Table {
                aligns: vec![Align::Left, Align::Center, Align::Right],
                head: vec![
                    vec![plain("Pokémon", at("Pokémon"))],
                    vec![plain("Type", at("Type"))],
                    vec![plain("Lv", at("Lv"))],
                ],
                rows: vec![vec![
                    vec![plain("Pikachu", at("Pikachu"))],
                    vec![styled("Electric", BOLD, at("Electric"))],
                    vec![plain("5", at("5"))],
                ]],
            }]
        );
        assert_eq!(
            parsed("| a |\n|---|\n"),
            [Block::Table {
                aligns: vec![Align::None],
                head: vec![vec![plain("a", 2..3)]],
                rows: vec![],
            }]
        );
    }

    #[test]
    fn rules_are_blocks() {
        assert_eq!(
            parsed("a\n\n---\n\n***\nb"),
            [
                Block::Paragraph(vec![plain("a", 0..1)]),
                Block::Rule,
                Block::Rule,
                Block::Paragraph(vec![plain("b", 12..13)]),
            ]
        );
    }

    #[test]
    fn links_images_and_html() {
        let url = |u: &str| Link::Url(u.to_owned());
        assert_eq!(
            parsed("[Bulbapedia](https://bulbapedia.net) <hi@x.dev> <https://x.dev>"),
            [Block::Paragraph(vec![
                linked("Bulbapedia", url("https://bulbapedia.net"), 1..11),
                plain(" ", 36..37),
                linked("hi@x.dev", url("mailto:hi@x.dev"), 38..46),
                plain(" ", 47..48),
                linked("https://x.dev", url("https://x.dev"), 49..62),
            ])]
        );
        // An image is its alt text; raw HTML is text as written.
        assert_eq!(
            parsed("See ![a map](map.png) <b>here</b>"),
            [Block::Paragraph(vec![plain(
                "See a map <b>here</b>",
                0..33
            )])]
        );
        assert_eq!(
            parsed("<div>\nhi\n</div>\n\nafter"),
            [
                Block::Paragraph(vec![plain("<div>\nhi\n</div>", 0..15)]),
                Block::Paragraph(vec![plain("after", 17..22)]),
            ]
        );
        // Reference links, with the definition before or after.
        let linked_one = |source: &str, at: Range<usize>| {
            all_spans(&parsed(source))
                .into_iter()
                .any(|span| span.link == Some(url("https://x.dev/1")) && span.range == at)
        };
        assert!(linked_one("See [1].\n\n[1]: https://x.dev/1", 5..6));
        assert!(linked_one("[1]: https://x.dev/1\n\nSee [one][1].", 27..30));
    }

    #[test]
    fn hanging_markers_are_mended_while_streaming() {
        let mut doc = Document::new();
        doc.push("Use **Surf");
        assert_eq!(
            doc.blocks(),
            [Block::Paragraph(vec![
                plain("Use ", 0..4),
                styled("Surf", BOLD, 6..10)
            ])]
        );
        doc.settle();
        assert_eq!(
            doc.blocks(),
            [Block::Paragraph(vec![plain("Use **Surf", 0..10)])]
        );

        let mut doc = Document::new();
        doc.push("See [Bulbapedia](https://bulba");
        assert_eq!(
            doc.blocks(),
            [Block::Paragraph(vec![
                plain("See ", 0..4),
                linked("Bulbapedia", Link::Pending, 5..15),
            ])]
        );
        doc.push("pedia.net) now");
        assert_eq!(
            doc.blocks(),
            [Block::Paragraph(vec![
                plain("See ", 0..4),
                linked(
                    "Bulbapedia",
                    Link::Url("https://bulbapedia.net".to_owned()),
                    5..15
                ),
                plain(" now", 40..44),
            ])]
        );
    }

    #[test]
    fn code_is_never_mended() {
        let mut doc = Document::new();
        doc.push("```\nlet a = **b `c");
        assert_eq!(
            doc.blocks(),
            [Block::Code {
                language: None,
                text: "let a = **b `c".to_owned(),
            }]
        );
    }

    #[test]
    fn a_dash_under_text_stays_a_paragraph_while_streaming() {
        let mut doc = Document::new();
        doc.push("Steps\n-");
        assert_eq!(
            doc.blocks(),
            [Block::Paragraph(vec![plain("Steps -", 0..7)])]
        );
        doc.push("--\n");
        assert_eq!(
            doc.blocks(),
            [Block::Heading {
                level: 2,
                spans: vec![plain("Steps", 0..5)],
            }]
        );
    }

    #[test]
    fn only_the_last_leaf_is_mended() {
        // The first item's marker never closed; the second item is the one
        // streaming.
        let mut doc = Document::new();
        doc.push("- one *x\n- two **y");
        assert_eq!(
            doc.blocks(),
            [Block::List {
                start: None,
                items: vec![
                    vec![Block::Paragraph(vec![plain("one *x", 2..8)])],
                    vec![Block::Paragraph(vec![
                        plain("two ", 11..15),
                        styled("y", BOLD, 17..18)
                    ])],
                ],
            }]
        );
    }

    #[test]
    fn pushes_parse_only_the_tail() {
        let mut doc = Document::new();
        for n in 0..200 {
            doc.push(&format!(
                "Paragraph {n} with **bold** and a [link](https://x.dev/{n}).\n\n"
            ));
        }
        let last = "Paragraph 199 with **bold** and a [link](https://x.dev/199).\n\n";
        doc.push("Next");
        assert!(doc.parsed <= last.len() + 4 + 8, "{}", doc.parsed);
        doc.push(" one, **still");
        assert!(doc.parsed <= 2 * (last.len() + 32), "{}", doc.parsed);
        doc.push(" going\n");
        doc.push("more");
        assert!(doc.parsed <= 64, "{}", doc.parsed);
        doc.settle();
        assert_eq!(doc.blocks(), Document::parse(doc.source()).blocks());
    }

    /// Answers like the model's, each streamed at many chunk sizes.
    const ANSWERS: &[&str] = &[
        "Hi! **Bulbasaur** is a *Grass*/Poison type. See [Bulbapedia](https://bulbapedia.bulbagarden.net/wiki/Bulbasaur_(Pok%C3%A9mon)) for more.\n\nIt evolves at level `16` into ~~Charmander~~ **Ivysaur**, then *Venusaur* at [32](https://x.dev/32).\n",
        "To get Surf:\n\n- Beat **Misty** in [Cerulean City](https://x.dev/cerulean).\n- Go to the Safari Zone:\n  - find the *Secret House*\n  - talk to the man\n- Teach it to a **Water** type\n\nThat's it.",
        "Steps:\n\n1. Go north from **Pallet Town**.\n\n2. Talk to the professor.\n\n3. Pick a starter:\n   *Bulbasaur* is easiest.\n\n10. Ten is fine too.\n\nThen:\n1. a\n2. b\n",
        "| Pokémon | Type | Level |\n|:--|:-:|--:|\n| Bulbasaur | Grass | 5 |\n| Charmander | **Fire** | 5 |\n| Squirtle | [Water](https://x.dev/water) | 5 |\n\nPick one.",
        "Use this cheat:\n\n```lua\nprint(\"hi **there**\")\nlocal x = [1](2)\n```\n\nOr this one:\n\n    indented **code**\n\nDone.",
        "> **Tip:** save before the gym.\n> It *really* helps.\n\n## Gym 1\n\nBrock uses Rock types\n---\n\nAfter a rule, ![badge](https://x.dev/b.png) and <b>html</b>, a hard  \nbreak, é and 🦀.\n\n# Done",
        "para\n#b and > a\n#c\n\n> a quote\n#d still in it",
        "Read the guide [1] and the [map][m].\n\n[1]: https://x.dev/guide\n[m]: https://x.dev/map 'Map'\n",
    ];

    #[test]
    fn streaming_settles_to_the_whole_parse() {
        for answer in ANSWERS {
            let whole = parsed(answer);
            let urls: Vec<&String> = all_spans(&whole)
                .into_iter()
                .filter_map(|span| match &span.link {
                    Some(Link::Url(url)) => Some(url),
                    _ => None,
                })
                .collect();
            for size in [1, 2, 3, 7, 13, 64] {
                let mut doc = Document::new();
                for piece in chunks(answer, size) {
                    doc.push(piece);
                    let source = doc.source();
                    for span in all_spans(doc.blocks()) {
                        let Range { start, end } = span.range;
                        assert!(
                            start <= end && end <= source.len(),
                            "{span:?} in {source:?}"
                        );
                        assert!(
                            source.is_char_boundary(start) && source.is_char_boundary(end),
                            "{span:?} in {source:?}"
                        );
                        if let Some(Link::Url(url)) = &span.link {
                            // A finished URL: one the whole answer has, all
                            // of it here.
                            assert!(urls.contains(&url), "{url:?} in {source:?}");
                            let written = source.contains(&format!("({url})"))
                                || source.contains(&format!("]: {url}\n"))
                                || source.contains(&format!("]: {url} "));
                            assert!(written, "{url:?} in {source:?}");
                        }
                    }
                }
                assert_eq!(doc.source(), *answer);
                doc.settle();
                assert_eq!(doc.blocks(), whole, "{answer:?} in pieces of {size}");
            }
        }
    }

    #[test]
    fn empty_and_blank_documents() {
        assert_eq!(parsed(""), []);
        assert_eq!(parsed("\n\n  \n"), []);
        let mut doc = Document::new();
        doc.push("");
        doc.push("\n\n");
        doc.settle();
        assert!(doc.blocks().is_empty());
    }
}
