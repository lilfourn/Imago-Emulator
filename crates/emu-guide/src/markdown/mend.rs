//! Closes the markers a streaming block leaves open, for display only: a
//! port of Imago's `close_hanging`, itself after streamdown's `remend`.
//!
//! While a block streams, an unclosed `**bold`, `*em`, `` `code ``,
//! `~~strike` or `[link](partial-url` parses as literal text, and the
//! closer's arrival restyles the run and reflows the line. Adding the
//! closers to the text that is shown keeps the style steady from the first
//! character after the opener. The blocks as written are left alone, so a
//! marker that never closes shows as written once the answer settles.
//!
//! What it mends:
//! - emphasis: `**a` → `**a**`, and `*a`, `_a`, `__a`, `~~a`; nested
//!   closers innermost first (`**a *b` → `**a *b***`); half-streamed
//!   closers completed (`**a*` → `**a**`);
//! - inline code's backticks;
//! - links and images: `[text](partial` and a bare `[text` become a link to
//!   [`PENDING`], so the text shows as a link at once and a half-written
//!   URL is never used;
//! - a last line of just `-`, `--`, `=` or `==` under text gets a
//!   zero-width space, so a list item on its way is not read as a setext
//!   underline.
//!
//! The scan is approximate on purpose, as streamdown's is: a misjudgment
//! lasts only until the next push or the settle. An intraword `**` closes
//! (`2**3` bolds the `3` for a moment), and an opener followed only by
//! spaces or markers stays literal until text follows it. Imago's skipping
//! of math spans is left out, as answers here are not parsed for math.
//!
//! One pass over the text, and nothing allocated when nothing hangs.

use std::ops::Range;

/// The URL a link still streaming points to while it is shown. The parse
/// of mended text turns it into [`super::Link::Pending`].
pub(super) const PENDING: &str = "emu-guide:pending-link";

/// A block's text with its open markers closed, and where each part of it
/// came from.
#[derive(Debug)]
pub(super) struct Mended {
    /// The text with its closers.
    pub text: String,
    /// Whether a zero-width space was added at the end, against a setext
    /// misreading.
    pub zwsp: bool,
    /// The parts of `text` in order.
    runs: Vec<Run>,
}

/// One part of a [`Mended`] text.
#[derive(Debug, Clone, Copy)]
struct Run {
    /// Where it starts in the mended text.
    at: usize,
    /// Where it starts in the original, or, if it was added, where it was
    /// added.
    from: usize,
    /// Whether it is the original's text, not added.
    written: bool,
}

impl Mended {
    fn new(capacity: usize) -> Self {
        Self {
            text: String::with_capacity(capacity),
            zwsp: false,
            runs: Vec::new(),
        }
    }

    /// `text` with its bytes in `part`, which this mended, replaced by
    /// this.
    pub fn within(self, text: &str, part: Range<usize>) -> Self {
        let mut whole = Self::new(text.len() + self.text.len() - part.len());
        whole.zwsp = self.zwsp;
        whole.written(text, 0..part.start);
        let at = whole.text.len();
        whole.runs.extend(self.runs.iter().map(|run| Run {
            at: at + run.at,
            from: part.start + run.from,
            written: run.written,
        }));
        whole.text.push_str(&self.text);
        whole.written(text, part.end..text.len());
        whole
    }

    /// Appends the original's bytes in `range`.
    fn written(&mut self, original: &str, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        self.runs.push(Run {
            at: self.text.len(),
            from: range.start,
            written: true,
        });
        self.text.push_str(&original[range]);
    }

    /// Appends `text`, which the original does not have, at its offset
    /// `from`.
    fn added(&mut self, text: &str, from: usize) {
        if text.is_empty() {
            return;
        }
        self.runs.push(Run {
            at: self.text.len(),
            from,
            written: false,
        });
        self.text.push_str(text);
    }

    /// Where offset `at` of the mended text is in the original. Added text
    /// is where it was added, so a range never reaches past the original.
    pub fn original(&self, at: usize) -> usize {
        match self.runs.iter().rev().find(|run| run.at <= at) {
            Some(run) if run.written => run.from + (at - run.at),
            Some(run) => run.from,
            None => at,
        }
    }
}

/// One unclosed emphasis delimiter run (`*`, `_` or `~~`).
struct OpenDelim {
    ch: char,
    len: usize,
    /// Char index just past the run: the order closers go in, and where
    /// content must follow for it to be closed.
    pos: usize,
}

/// Closes the markers left open in a streaming block's text. `None` when
/// nothing needs closing, which is most of the time.
pub(super) fn close_hanging(text: &str) -> Option<Mended> {
    let cs: Vec<(usize, char)> = text.char_indices().collect();
    let n = cs.len();
    let at = |i: usize| cs.get(i).map(|&(_, c)| c);

    let mut delims: Vec<OpenDelim> = Vec::new();
    // Char indices of unmatched `[`.
    let mut brackets: Vec<usize> = Vec::new();
    // An open inline code span: its backtick run's length, and the char
    // index its content starts at.
    let mut code: Option<(usize, usize)> = None;
    // Char index of the last character that justifies closing an opener:
    // not a space, not a bare marker.
    let mut last_content: Option<usize> = None;
    // Char index of the `]` of a `](…` whose URL runs off the end.
    let mut pending_url: Option<usize> = None;

    let mut i = 0;
    while i < n {
        let c = cs[i].1;
        if code.is_none() && c == '\\' {
            // An escaped character: both are literal, and the second still
            // counts as content.
            if i + 1 < n {
                last_content = Some(i + 1);
            }
            i += 2;
            continue;
        }
        if c == '`' {
            let run = run_len(&cs, i);
            match code {
                // A span closes only on a run of the opening length.
                Some((open, _)) if run == open => code = None,
                Some(_) => last_content = Some(i + run - 1),
                None => code = Some((run, i + run)),
            }
            i += run;
            continue;
        }
        if code.is_some() {
            last_content = Some(i);
            i += 1;
            continue;
        }
        match c {
            '*' | '_' | '~' => {
                let run = run_len(&cs, i);
                delim(&mut delims, &cs, c, run, i, &mut last_content);
                i += run;
            }
            '[' => {
                brackets.push(i);
                i += 1;
            }
            ']' => {
                if let Some(open) = brackets.pop() {
                    // Emphasis opened inside a finished `[…]` and not
                    // closed there stays literal, as the final parse will
                    // have it.
                    delims.retain(|d| d.pos < open);
                    if at(i + 1) == Some('(') {
                        // The URL, through its balanced `)`.
                        let mut j = i + 2;
                        let mut depth = 0usize;
                        loop {
                            match at(j) {
                                Some('(') => depth += 1,
                                Some(')') if depth == 0 => break,
                                Some(')') => depth -= 1,
                                Some(_) => {}
                                None => {
                                    pending_url = Some(i);
                                    break;
                                }
                            }
                            j += 1;
                        }
                        if pending_url.is_some() {
                            break;
                        }
                        last_content = Some(j);
                        i = j + 1;
                        continue;
                    }
                }
                last_content = Some(i);
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            _ => {
                last_content = Some(i);
                i += 1;
            }
        }
    }

    // The text ends inside a link's URL: keep the text, drop the URL.
    if let Some(close) = pending_url {
        let byte = cs[close].0;
        let tail = format!("]({PENDING})");
        let mut mended = Mended::new(byte + tail.len());
        mended.written(text, 0..byte);
        mended.added(&tail, byte);
        return Some(mended);
    }

    // Closers innermost first, by where they opened. An open `[` splits
    // them: delimiters opened inside the link text close before its
    // `](…)`, ones opened before it close after.
    let mut pending: Vec<(usize, String)> = Vec::new();
    if let Some((ticks, cpos)) = code
        && last_content.is_some_and(|lc| lc >= cpos)
    {
        pending.push((cpos, "`".repeat(ticks)));
    }
    for d in &delims {
        if last_content.is_some_and(|lc| lc >= d.pos) {
            pending.push((d.pos, d.ch.to_string().repeat(d.len)));
        }
    }
    if let Some(&open) = brackets.last()
        && last_content.is_some_and(|lc| lc > open)
    {
        pending.push((open, format!("]({PENDING})")));
    }
    pending.sort_by_key(|b| std::cmp::Reverse(b.0));
    let closers: String = pending.into_iter().map(|(_, s)| s).collect();

    // A last line of only `-`, `--`, `=` or `==` under text is a setext
    // underline to the parser, but nearly always a list item on its way. A
    // zero-width space breaks that reading, unseen, until the next
    // characters decide (streamdown's trick).
    let setext = setext_partial(text);

    if closers.is_empty() && !setext {
        return None;
    }
    let mut mended = Mended::new(text.len() + closers.len() + 3);
    if setext {
        // The closers belong to the text above the underline.
        match text.rfind('\n') {
            Some(nl) if !closers.is_empty() => {
                mended.written(text, 0..nl);
                mended.added(&closers, nl);
                mended.written(text, nl..text.len());
            }
            _ => mended.written(text, 0..text.len()),
        }
        mended.added("\u{200B}", text.len());
        mended.zwsp = true;
        return Some(mended);
    }
    // Before trailing spaces: a closer after a space does not close.
    let end = text.trim_end().len();
    mended.written(text, 0..end);
    mended.added(&closers, end);
    mended.written(text, end..text.len());
    Some(mended)
}

fn run_len(cs: &[(usize, char)], i: usize) -> usize {
    let c = cs[i].1;
    cs[i..].iter().take_while(|&&(_, x)| x == c).count()
}

/// Matches or opens one delimiter run. It closes against the innermost
/// opener of the same character (partly, for a half-streamed closer like
/// `**a*`); delimiters opened after that opener were inside the closed span
/// and stay literal, as the final parse has them.
fn delim(
    delims: &mut Vec<OpenDelim>,
    cs: &[(usize, char)],
    c: char,
    run: usize,
    i: usize,
    last_content: &mut Option<usize>,
) {
    let end = i + run;
    // Strikethrough is `~~` only; longer tilde runs are literal. A single
    // `~` may still be the first half of a closing `~~`, so it goes on to
    // be matched, but never opens.
    if c == '~' && run > 2 {
        *last_content = Some(end - 1);
        return;
    }
    let prev = i.checked_sub(1).map(|p| cs[p].1);
    let next = cs.get(end).map(|&(_, c)| c);
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    // An intraword `_` never delimits; an intraword single `*` is taken the
    // same way, so `2*3` does not flash italic.
    if word(prev) && word(next) && (c == '_' || (c == '*' && run == 1)) {
        *last_content = Some(end - 1);
        return;
    }
    let can_close = prev.is_some_and(|c| !c.is_whitespace());
    let can_open = next.is_some_and(|c| !c.is_whitespace());
    let mut rest = run;
    if can_close && let Some(k) = delims.iter().rposition(|d| d.ch == c) {
        let take = rest.min(delims[k].len);
        delims[k].len -= take;
        rest -= take;
        let keep = if delims[k].len == 0 { k } else { k + 1 };
        delims.truncate(keep);
    }
    if rest > 0 {
        // A lone `~` never opens; a `~` of length 1 is only ever what is
        // left of a half-streamed closer.
        if can_open && (c != '~' || rest == 2) {
            delims.push(OpenDelim {
                ch: c,
                len: rest,
                pos: end,
            });
        } else {
            *last_content = Some(end - 1);
        }
    }
}

/// Whether the last line is only one or two `-` or `=` (no trailing
/// space) under a line with text: the half-written setext underline.
fn setext_partial(text: &str) -> bool {
    let Some(nl) = text.rfind('\n') else {
        return false;
    };
    let last = &text[nl + 1..];
    let trimmed = last.trim_start();
    let underline =
        |c: char| !trimmed.is_empty() && trimmed.len() <= 2 && trimmed.chars().all(|x| x == c);
    (underline('-') || underline('='))
        && text[..nl]
            .lines()
            .last()
            .is_some_and(|l| !l.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn mends(input: &str, expected: &str) {
        let mended = close_hanging(input).map(|m| m.text);
        assert_eq!(mended.as_deref(), Some(expected), "{input:?}");
    }

    #[track_caller]
    fn stays(input: &str) {
        assert!(close_hanging(input).is_none(), "{input:?}");
    }

    #[test]
    fn balanced_text_needs_nothing() {
        stays("plain words, no markers");
        stays("a **b** and *c* and `d` and ~~e~~");
        stays("[docs](https://x.dev) done");
        stays("");
    }

    #[test]
    fn bold_and_italic_close() {
        mends("**bold", "**bold**");
        mends("some *em", "some *em*");
        mends("a __b", "a __b__");
        mends("a _b", "a _b_");
        mends("***both", "***both***");
    }

    #[test]
    fn half_streamed_closers_complete() {
        mends("**bold*", "**bold**");
        mends("__b_", "__b__");
        mends("~~gone~", "~~gone~~");
    }

    #[test]
    fn nested_closers_come_innermost_first() {
        mends("**a *b", "**a *b***");
        mends("*a **b", "*a **b***");
        mends("_a **b", "_a **b**_");
    }

    #[test]
    fn bare_openers_stay_literal_until_content() {
        stays("**");
        stays("text **");
        stays("text ** ");
        stays("*");
        stays("~~");
        stays("`");
    }

    #[test]
    fn closers_insert_before_trailing_whitespace() {
        // `**bold **` would only be left-flanking: it must be `**bold** `
        // to close.
        mends("**bold ", "**bold** ");
        mends("*em\n", "*em*\n");
    }

    #[test]
    fn intraword_and_escapes_are_literal() {
        stays("2*3 equals 6");
        stays("snake_case_name");
        stays("20~25 degrees");
        stays(r"\*not emphasis");
        stays(r"a \** b");
    }

    #[test]
    fn list_markers_are_not_openers() {
        stays("* item one");
        stays("- a\n* b");
    }

    #[test]
    fn strikethrough_closes() {
        mends("~~gone", "~~gone~~");
        stays("~single~x"); // a run of one is literal
    }

    #[test]
    fn inline_code_closes_and_shields_markers() {
        mends("`code", "`code`");
        mends("call `a ** b", "call `a ** b`");
        mends("``a`", "``a```"); // a shorter run is content, not a closer
        stays("`done` after");
    }

    #[test]
    fn links_mend_to_pending_sentinel() {
        mends("[docs](https://x.dev/lo", "[docs](emu-guide:pending-link)");
        mends("[docs](", "[docs](emu-guide:pending-link)");
        mends("see [do", "see [do](emu-guide:pending-link)");
        mends("![alt](https://x/i.p", "![alt](emu-guide:pending-link)");
        stays("see [");
        stays("[x] task-like"); // a finished bracket, no URL
    }

    #[test]
    fn link_urls_allow_nested_parens() {
        stays("[a](https://x.dev/(y)) done");
        mends("[a](https://x.dev/(y", "[a](emu-guide:pending-link)");
    }

    #[test]
    fn emphasis_inside_link_text_closes_inside() {
        mends("[**a", "[**a**](emu-guide:pending-link)");
        mends("**a [b", "**a [b](emu-guide:pending-link)**");
    }

    #[test]
    fn emphasis_unclosed_in_completed_bracket_is_dropped() {
        stays("[**a] done");
    }

    #[test]
    fn setext_partials_get_zero_width_space() {
        mends("para\n-", "para\n-\u{200B}");
        mends("para\n--", "para\n--\u{200B}");
        mends("para\n=", "para\n=\u{200B}");
        stays("para\n---"); // a real rule, or a settled setext heading
        stays("-"); // no line above
        stays("\n-"); // an empty line above
        mends("**b\n-", "**b**\n-\u{200B}"); // closers go above the underline
    }

    #[test]
    fn added_text_maps_to_where_it_was_added() {
        let text = "**b\n-";
        let mended = close_hanging(text).expect("hangs");
        assert_eq!(mended.text, "**b**\n-\u{200B}");
        // `**b` is the original's own text.
        assert_eq!(mended.original(2), 2);
        // The added `**` is where it went in, before the newline.
        assert_eq!(mended.original(3), 3);
        assert_eq!(mended.original(4), 3);
        // The underline line moved by two bytes.
        assert_eq!(mended.original(5), 3);
        assert_eq!(mended.original(6), 4);
        // The zero-width space, and the end, are at the original's end.
        assert_eq!(mended.original(7), text.len());
        assert_eq!(mended.original(mended.text.len()), text.len());

        let text = "see [docs](https://x.dev/lo";
        let mended = close_hanging(text).expect("hangs");
        assert_eq!(mended.original(mended.text.len()), "see [docs".len());
    }

    #[test]
    fn a_mended_part_maps_within_the_whole_text() {
        let text = "- a\n- **b\n";
        let part = 6..9;
        let mended = close_hanging(&text[part.clone()])
            .expect("hangs")
            .within(text, part);
        assert_eq!(mended.text, "- a\n- **b**\n");
        assert_eq!(mended.original(2), 2);
        assert_eq!(mended.original(8), 8);
        // The closer, and the newline after it.
        assert_eq!(mended.original(9), 9);
        assert_eq!(mended.original(11), 9);
        assert_eq!(mended.original(mended.text.len()), text.len());
    }
}
