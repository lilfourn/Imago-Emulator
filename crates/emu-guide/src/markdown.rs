//! Markdown answers as blocks the panel draws, parsed as they stream.
//!
//! CONTRACT STUB — to be implemented (see the task brief). The public
//! types and signatures below are what the app's renderer uses; keep them.

use std::ops::Range;

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
}

impl Document {
    /// An empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// A finished document parsed whole, as a saved answer is.
    pub fn parse(source: &str) -> Self {
        let mut document = Self::new();
        document.push(source);
        document.settle();
        document
    }

    /// Appends streamed text. Only the last top-level block is parsed
    /// again, with hanging markers mended for display.
    pub fn push(&mut self, delta: &str) {
        self.source.push_str(delta);
        self.blocks = vec![Block::Paragraph(vec![Span {
            text: self.source.clone(),
            style: Style::default(),
            link: None,
            range: 0..self.source.len(),
        }])];
    }

    /// The stream has ended: parses the last block as written, unmended.
    pub fn settle(&mut self) {}

    /// The Markdown so far.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The blocks so far.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
}
