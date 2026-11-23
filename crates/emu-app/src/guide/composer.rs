//! The Guide's composer: the field the player writes to the Guide in. Its
//! text wraps to the field's width and keeps the line breaks typed with
//! Shift-Return; the field grows a line at a time up to
//! [`ComposerStyle::max_height`], and its text scrolls after that.
//!
//! It is GPUI's own text input example made many-lined. Return sends and
//! Escape leaves, both as [`ComposerEvent`]s for the panel to act on, and
//! Up in an empty field asks for the last message back. The field's keys
//! are bound by [`bindings`] in the [`KEY_CONTEXT`] key context, so they
//! act only while it has focus, and an action it handles goes no further.
//! Typed characters are not actions: GPUI offers each key-down to the
//! key-down listeners of the views around the field before the input
//! method gets it, so a view that plays keys must skip them while the
//! field has focus.
//!
//! The editing itself, apart from any window, is [`Buffer`], where
//! offsets are UTF-8 bytes on character boundaries; the input method
//! counts in UTF-16, converted where it calls in.

use std::borrow::Cow;
use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Font, GlobalElementId, Hsla, InspectorElementId, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ScrollWheelEvent,
    SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
    actions, div, fill, point, prelude::*, px, relative, rgb, rgba, size,
};

use crate::probe;

actions!(
    guide_composer,
    [
        /// Sends the text.
        Submit,
        /// Starts a new line.
        Newline,
        /// Leaves the field.
        Cancel,
        /// Moves the caret a character left.
        MoveLeft,
        /// Moves the caret a character right.
        MoveRight,
        /// Moves the caret a row up, or asks for the last message back in
        /// an empty field.
        MoveUp,
        /// Moves the caret a row down.
        MoveDown,
        /// Moves the caret to the start of the word.
        MoveWordLeft,
        /// Moves the caret to the end of the word.
        MoveWordRight,
        /// Moves the caret to the start of its row.
        MoveToLineStart,
        /// Moves the caret to the end of its row.
        MoveToLineEnd,
        /// Moves the caret to the start of the text.
        MoveToStart,
        /// Moves the caret to the end of the text.
        MoveToEnd,
        /// Selects a character more to the left.
        SelectLeft,
        /// Selects a character more to the right.
        SelectRight,
        /// Selects up a row.
        SelectUp,
        /// Selects down a row.
        SelectDown,
        /// Selects to the start of the word.
        SelectWordLeft,
        /// Selects to the end of the word.
        SelectWordRight,
        /// Selects to the start of the row.
        SelectToLineStart,
        /// Selects to the end of the row.
        SelectToLineEnd,
        /// Selects to the start of the text.
        SelectToStart,
        /// Selects to the end of the text.
        SelectToEnd,
        /// Selects all the text.
        SelectAll,
        /// Deletes the character before the caret, or the selection.
        Backspace,
        /// Deletes the character after the caret, or the selection.
        Delete,
        /// Deletes back to the start of the word.
        DeleteWordLeft,
        /// Deletes on to the end of the word.
        DeleteWordRight,
        /// Deletes back to the start of the row.
        DeleteToLineStart,
        /// Deletes on to the end of the row.
        DeleteToLineEnd,
        /// Copies the selection.
        CopySelection,
        /// Copies the selection and deletes it.
        CutSelection,
        /// Puts the clipboard's text in place of the selection.
        Paste,
        /// Opens the system's emoji and symbols palette.
        ShowCharacterPalette,
    ]
);

/// The key context the composer's keys are bound in.
pub const KEY_CONTEXT: &str = "GuideComposer";

/// The caret's width, in logical pixels.
const CARET_WIDTH: f32 = 1.5;

/// The caret's height, as a share of the font size.
const CARET_HEIGHT: f32 = 1.3;

/// How wide a selected line break is drawn, as a share of the font size.
const NEWLINE_WIDTH: f32 = 0.35;

/// The composer's keys, bound in [`KEY_CONTEXT`] so they act only while
/// it has focus, for the app to add with `cx.bind_keys` at launch. Where
/// one is also an app shortcut (⌘⌫ removes a game from the library), the
/// field's wins while it has focus, as a deeper context's binding does.
///
/// Each system's own text keys: on macOS ⌥ moves by words and ⌘ to the
/// line's ends, with the Emacs keys a Mac's fields take; on Linux and
/// Windows Control moves by words and Control-Home and -End to the text's
/// ends, as their fields do. Copy, cut, paste and select all are
/// `secondary`, ⌘ on macOS and Control elsewhere.
pub fn bindings() -> Vec<KeyBinding> {
    let context = Some(KEY_CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("enter", Submit, context),
        KeyBinding::new("shift-enter", Newline, context),
        KeyBinding::new("alt-enter", Newline, context),
        KeyBinding::new("escape", Cancel, context),
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("home", MoveToLineStart, context),
        KeyBinding::new("end", MoveToLineEnd, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("shift-home", SelectToLineStart, context),
        KeyBinding::new("shift-end", SelectToLineEnd, context),
        KeyBinding::new("secondary-a", SelectAll, context),
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("secondary-c", CopySelection, context),
        KeyBinding::new("secondary-x", CutSelection, context),
        KeyBinding::new("secondary-v", Paste, context),
    ];
    if cfg!(target_os = "macos") {
        // ⌘ is written as itself here: these are macOS's keys alone.
        bindings.extend([
            KeyBinding::new("alt-left", MoveWordLeft, context),
            KeyBinding::new("alt-right", MoveWordRight, context),
            KeyBinding::new("cmd-left", MoveToLineStart, context),
            KeyBinding::new("cmd-right", MoveToLineEnd, context),
            KeyBinding::new("ctrl-a", MoveToLineStart, context),
            KeyBinding::new("ctrl-e", MoveToLineEnd, context),
            KeyBinding::new("cmd-up", MoveToStart, context),
            KeyBinding::new("cmd-down", MoveToEnd, context),
            KeyBinding::new("alt-shift-left", SelectWordLeft, context),
            KeyBinding::new("alt-shift-right", SelectWordRight, context),
            KeyBinding::new("cmd-shift-left", SelectToLineStart, context),
            KeyBinding::new("cmd-shift-right", SelectToLineEnd, context),
            KeyBinding::new("cmd-shift-up", SelectToStart, context),
            KeyBinding::new("cmd-shift-down", SelectToEnd, context),
            KeyBinding::new("ctrl-h", Backspace, context),
            KeyBinding::new("ctrl-d", Delete, context),
            KeyBinding::new("alt-backspace", DeleteWordLeft, context),
            KeyBinding::new("alt-delete", DeleteWordRight, context),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, context),
            KeyBinding::new("cmd-delete", DeleteToLineEnd, context),
            KeyBinding::new("ctrl-k", DeleteToLineEnd, context),
            KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, context),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-left", MoveWordLeft, context),
            KeyBinding::new("ctrl-right", MoveWordRight, context),
            KeyBinding::new("ctrl-home", MoveToStart, context),
            KeyBinding::new("ctrl-end", MoveToEnd, context),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, context),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, context),
            KeyBinding::new("ctrl-shift-home", SelectToStart, context),
            KeyBinding::new("ctrl-shift-end", SelectToEnd, context),
            KeyBinding::new("ctrl-backspace", DeleteWordLeft, context),
            KeyBinding::new("ctrl-delete", DeleteWordRight, context),
        ]);
    }
    bindings
}

/// How the composer looks: its colours and type, and how tall it grows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComposerStyle {
    /// The text's colour.
    pub text: Hsla,
    /// The colour of the hint shown while the field is empty.
    pub placeholder: Hsla,
    /// What selected text is drawn on.
    pub selection: Hsla,
    /// The caret's colour.
    pub caret: Hsla,
    /// The text's size.
    pub font_size: Pixels,
    /// How far apart the text's rows are.
    pub line_height: Pixels,
    /// The tallest the field grows before its text scrolls.
    pub max_height: Pixels,
}

impl Default for ComposerStyle {
    /// Near-white text on a dark panel.
    fn default() -> Self {
        Self {
            text: rgb(0x00ED_EDF0).into(),
            placeholder: rgb(0x008E_8EA0).into(),
            selection: rgba(0x3D7C_F066).into(),
            caret: rgb(0x00ED_EDF0).into(),
            font_size: px(14.5),
            line_height: px(22.0),
            max_height: px(160.0),
        }
    }
}

/// What the composer tells the panel it sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerEvent {
    /// Return was pressed: send the text. The field keeps it until
    /// cleared.
    Submit,
    /// Escape was pressed.
    Escape,
    /// Up was pressed in an empty field: bring back the last message.
    RecallLast,
    /// The text changed, by typing, pasting, the input method or a call.
    Changed,
}

/// The Guide's text field.
pub struct Composer {
    focus: FocusHandle,
    buffer: Buffer,
    placeholder: SharedString,
    style: ComposerStyle,
    /// Counts changes to the text, so rows drawn for older text are not
    /// used to move the caret.
    revision: u64,
    /// How far the text is scrolled up, in logical pixels.
    scroll: Pixels,
    /// Whether the next frame scrolls the caret into view.
    reveal_caret: bool,
    /// How far across the caret was when moving up or down began, so a
    /// short row passed through does not pull it left.
    goal_x: Option<Pixels>,
    /// Whether a mouse press in the field is selecting.
    selecting: bool,
    /// The text as last drawn.
    drawn: Option<Drawn>,
}

impl Composer {
    /// An empty field.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            buffer: Buffer::default(),
            placeholder: SharedString::default(),
            style: ComposerStyle::default(),
            revision: 0,
            scroll: Pixels::ZERO,
            reveal_caret: false,
            goal_x: None,
            selecting: false,
            drawn: None,
        }
    }

    /// The text, including any the input method is still composing.
    pub fn text(&self) -> &str {
        &self.buffer.text
    }

    /// Whether there is no text.
    pub fn is_empty(&self) -> bool {
        self.buffer.text.is_empty()
    }

    /// Replaces the text, with the caret at its end.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = normalize(text);
        let changed = self.buffer.text != text;
        self.buffer = Buffer {
            text: text.into_owned(),
            ..Buffer::default()
        };
        self.buffer.move_to(self.buffer.text.len());
        self.edited(changed, cx);
    }

    /// Empties the field.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
    }

    /// Sets the hint shown while the field is empty.
    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = text.into();
        cx.notify();
    }

    /// Sets how the field looks.
    pub fn set_style(&mut self, style: ComposerStyle, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    /// After the text or the caret was changed by an edit: tells the panel
    /// if the text changed, and shows the caret.
    fn edited(&mut self, changed: bool, cx: &mut Context<Self>) {
        if changed {
            self.revision += 1;
            cx.emit(ComposerEvent::Changed);
        }
        self.moved(cx);
    }

    /// After the caret or the selection moved: shows the caret.
    fn moved(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.reveal_caret = true;
        cx.notify();
    }

    fn move_caret(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.move_to(offset);
        self.moved(cx);
    }

    fn select_caret(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer.select_to(offset);
        self.moved(cx);
    }

    fn delete_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let changed = self.buffer.delete_to(offset);
        self.edited(changed, cx);
    }

    /// Where a move left starts: the caret, or the selection's start.
    fn left_of_selection(&self) -> usize {
        if self.buffer.selected.is_empty() {
            self.buffer.caret()
        } else {
            self.buffer.selected.start
        }
    }

    /// Where a move right starts: the caret, or the selection's end.
    fn right_of_selection(&self) -> usize {
        if self.buffer.selected.is_empty() {
            self.buffer.caret()
        } else {
            self.buffer.selected.end
        }
    }

    /// The text as last drawn, if it was drawn for the text as it is.
    fn fresh(&self) -> Option<&Drawn> {
        self.drawn
            .as_ref()
            .filter(|drawn| drawn.revision == self.revision && !drawn.rows.is_empty())
    }

    /// The start of the row `offset` is in, or of its line if the text
    /// has not been drawn since it changed.
    fn row_start(&self, offset: usize) -> usize {
        match self.fresh() {
            Some(drawn) => drawn.rows[row_of(&drawn.rows, offset)].start,
            None => self.buffer.line_start(offset),
        }
    }

    /// The end of the row `offset` is in, or of its line if the text has
    /// not been drawn since it changed.
    fn row_end(&self, offset: usize) -> usize {
        match self.fresh() {
            Some(drawn) => {
                caret_end(&drawn.rows[row_of(&drawn.rows, offset)], &self.buffer).max(offset)
            }
            None => self.buffer.line_end(offset),
        }
    }

    /// Where moving a row up or down from `from` lands, as near as it can
    /// to the column the moves began in; the start or the end of the text
    /// from the first or the last row.
    fn vertical(&mut self, from: usize, down: bool) -> usize {
        let edge = if down { self.buffer.text.len() } else { 0 };
        let Some(drawn) = self.fresh() else {
            let line = if down {
                self.buffer.line_down(from)
            } else {
                self.buffer.line_up(from)
            };
            return line.unwrap_or(edge);
        };
        let (row, x) = drawn.caret_at(from);
        let goal = self.goal_x.unwrap_or(x);
        let target = if down {
            Some(row + 1).filter(|&row| row < drawn.rows.len())
        } else {
            row.checked_sub(1)
        };
        let offset = target.map_or(edge, |row| drawn.offset_in_row(row, goal, &self.buffer));
        self.goal_x = Some(goal);
        offset
    }

    fn move_vertically(&mut self, down: bool, select: bool, cx: &mut Context<Self>) {
        if self.buffer.text.is_empty() {
            if !down && !select {
                cx.emit(ComposerEvent::RecallLast);
            }
            return;
        }
        let from = if select {
            self.buffer.caret()
        } else if down {
            self.right_of_selection()
        } else {
            self.left_of_selection()
        };
        let target = self.vertical(from, down);
        let goal = self.goal_x;
        if select {
            self.select_caret(target, cx);
        } else {
            self.move_caret(target, cx);
        }
        self.goal_x = goal;
    }

    fn delete_to_line_start(&mut self, cx: &mut Context<Self>) {
        let caret = self.buffer.caret();
        let start = self.row_start(caret);
        // At a row's start, joins it to the row above.
        let target = if start == caret {
            self.buffer.prev_cluster(caret)
        } else {
            start
        };
        self.delete_to(target, cx);
    }

    fn delete_to_line_end(&mut self, cx: &mut Context<Self>) {
        let caret = self.buffer.caret();
        let end = self.row_end(caret);
        let target = if end == caret {
            self.buffer.next_cluster(caret)
        } else {
            end
        };
        self.delete_to(target, cx);
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if !self.buffer.selected.is_empty() {
            let text = self.buffer.selected_text().to_owned();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if !self.buffer.selected.is_empty() {
            self.copy(cx);
            let changed = self.buffer.replace(self.buffer.selected.clone(), "");
            self.edited(changed, cx);
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let changed = self.buffer.insert(&text);
            self.edited(changed, cx);
        }
    }

    /// The offset nearest `position`, in window coordinates; past the
    /// first or the last row, the start or the end of the text.
    fn offset_at(&self, position: Point<Pixels>) -> usize {
        let Some(drawn) = self.drawn.as_ref().filter(|drawn| !drawn.rows.is_empty()) else {
            return 0;
        };
        if self.buffer.text.is_empty() {
            return 0;
        }
        let y = position.y - drawn.bounds.top() + self.scroll;
        if y < Pixels::ZERO {
            return 0;
        }
        let row = (y / drawn.line_height) as usize;
        if row >= drawn.rows.len() {
            return self.buffer.text.len();
        }
        let x = position.x - drawn.bounds.left();
        self.buffer.clamp(drawn.offset_in_row(row, x, &self.buffer))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        cx.stop_propagation();
        let offset = self.offset_at(event.position);
        self.selecting = true;
        match event.click_count {
            2 => {
                let word = self.buffer.word_at(offset);
                self.buffer.select(word);
            }
            count if count >= 3 => {
                let line = self.buffer.line_start(offset)..self.buffer.line_end(offset);
                self.buffer.select(line);
            }
            _ if event.modifiers.shift => self.buffer.select_to(offset),
            _ => self.buffer.move_to(offset),
        }
        self.moved(cx);
    }

    /// Selects to `position` while the mouse that pressed in the field is
    /// held, wherever it is in the window.
    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        let offset = self.offset_at(position);
        if offset != self.buffer.caret() {
            self.select_caret(offset, cx);
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drawn) = &self.drawn else {
            return;
        };
        let most = drawn.content_height - drawn.bounds.size.height;
        // Text that fits leaves the wheel to what is behind the field.
        if most <= Pixels::ZERO {
            return;
        }
        cx.stop_propagation();
        let delta = event.delta.pixel_delta(drawn.line_height);
        let scroll = (self.scroll - delta.y).clamp(Pixels::ZERO, most);
        if scroll != self.scroll {
            self.scroll = scroll;
            cx.notify();
        }
    }
}

// The field's focus handle comes only from here: an inherent
// `focus_handle(&self)` would hide this from `composer.focus_handle(cx)`.
impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<ComposerEvent> for Composer {}

impl EntityInputHandler for Composer {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range_utf16);
        actual_range.replace(self.buffer.range_to_utf16(&range));
        Some(self.buffer.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.buffer.selected),
            reversed: self.buffer.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked
            .as_ref()
            .map(|range| self.buffer.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.map_or_else(
            || self.buffer.target(),
            |range| self.buffer.range_from_utf16(&range),
        );
        let changed = self.buffer.replace(range, &normalize(text));
        self.edited(changed, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.map_or_else(
            || self.buffer.target(),
            |range| self.buffer.range_from_utf16(&range),
        );
        // The selection is counted within the new text.
        let selected = new_selected_range_utf16
            .map(|range| utf8_offset(new_text, range.start)..utf8_offset(new_text, range.end));
        let changed = self.buffer.replace_and_mark(range, new_text, selected);
        self.edited(changed, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let drawn = self.drawn.as_ref().filter(|drawn| !drawn.rows.is_empty())?;
        let range = self.buffer.range_from_utf16(&range_utf16);
        let (row, left) = drawn.caret_at(range.start);
        let end = range.end.min(drawn.rows[row].end).max(range.start);
        let right = drawn.x_of(&drawn.rows[row], end);
        let top = element_bounds.top() - self.scroll + drawn.line_height * row;
        Some(Bounds::from_corners(
            point(element_bounds.left() + left, top),
            point(element_bounds.left() + right, top + drawn.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let drawn = self.drawn.as_ref()?;
        if !drawn.bounds.contains(&position) {
            return None;
        }
        Some(self.buffer.to_utf16(self.offset_at(position)))
    }
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .relative()
            .w_full()
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(|_, _: &Submit, _, cx| cx.emit(ComposerEvent::Submit)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(ComposerEvent::Escape)))
            .on_action(cx.listener(|this, _: &Newline, _, cx| {
                let changed = this.buffer.insert("\n");
                this.edited(changed, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| {
                let target = if this.buffer.selected.is_empty() {
                    this.buffer.prev_cluster(this.buffer.caret())
                } else {
                    this.buffer.selected.start
                };
                this.move_caret(target, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| {
                let target = if this.buffer.selected.is_empty() {
                    this.buffer.next_cluster(this.buffer.caret())
                } else {
                    this.buffer.selected.end
                };
                this.move_caret(target, cx);
            }))
            .on_action(
                cx.listener(|this, _: &MoveUp, _, cx| this.move_vertically(false, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MoveDown, _, cx| this.move_vertically(true, false, cx)),
            )
            .on_action(cx.listener(|this, _: &MoveWordLeft, _, cx| {
                let target = this.buffer.prev_word(this.left_of_selection());
                this.move_caret(target, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveWordRight, _, cx| {
                let target = this.buffer.next_word(this.right_of_selection());
                this.move_caret(target, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveToLineStart, _, cx| {
                let target = this.row_start(this.left_of_selection());
                this.move_caret(target, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveToLineEnd, _, cx| {
                let target = this.row_end(this.right_of_selection());
                this.move_caret(target, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveToStart, _, cx| this.move_caret(0, cx)))
            .on_action(cx.listener(|this, _: &MoveToEnd, _, cx| {
                this.move_caret(this.buffer.text.len(), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                this.select_caret(this.buffer.prev_cluster(this.buffer.caret()), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.select_caret(this.buffer.next_cluster(this.buffer.caret()), cx);
            }))
            .on_action(
                cx.listener(|this, _: &SelectUp, _, cx| this.move_vertically(false, true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectDown, _, cx| this.move_vertically(true, true, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| {
                this.select_caret(this.buffer.prev_word(this.buffer.caret()), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| {
                this.select_caret(this.buffer.next_word(this.buffer.caret()), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectToLineStart, _, cx| {
                this.select_caret(this.row_start(this.buffer.caret()), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectToLineEnd, _, cx| {
                this.select_caret(this.row_end(this.buffer.caret()), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectToStart, _, cx| this.select_caret(0, cx)))
            .on_action(cx.listener(|this, _: &SelectToEnd, _, cx| {
                this.select_caret(this.buffer.text.len(), cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.buffer.select_all();
                this.moved(cx);
            }))
            .on_action(cx.listener(|this, _: &Backspace, _, cx| {
                this.delete_to(this.buffer.prev_cluster(this.buffer.caret()), cx);
            }))
            .on_action(cx.listener(|this, _: &Delete, _, cx| {
                this.delete_to(this.buffer.next_cluster(this.buffer.caret()), cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteWordLeft, _, cx| {
                this.delete_to(this.buffer.prev_word(this.buffer.caret()), cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteWordRight, _, cx| {
                this.delete_to(this.buffer.next_word(this.buffer.caret()), cx);
            }))
            .on_action(
                cx.listener(|this, _: &DeleteToLineStart, _, cx| this.delete_to_line_start(cx)),
            )
            .on_action(cx.listener(|this, _: &DeleteToLineEnd, _, cx| this.delete_to_line_end(cx)))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &CutSelection, _, cx| this.cut(cx)))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|_, _: &ShowCharacterPalette, window, _| {
                window.show_character_palette();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .child(Field {
                composer: cx.entity(),
            })
            .children(probe::part("field:guide"))
    }
}

/// The text being written and where its caret and selection are, apart
/// from any window. Offsets are UTF-8 bytes, always on character
/// boundaries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Buffer {
    text: String,
    /// What is selected; empty for a plain caret.
    selected: Range<usize>,
    /// Whether the caret is at the selection's start rather than its end.
    reversed: bool,
    /// The text the input method is still composing, if any.
    marked: Option<Range<usize>>,
}

impl Buffer {
    /// Where the caret is: the end of the selection that moves.
    fn caret(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    /// `offset` within the text and on a character boundary, moved back
    /// if it was inside a character.
    fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn clamp_range(&self, range: Range<usize>) -> Range<usize> {
        let start = self.clamp(range.start);
        start..self.clamp(range.end).max(start)
    }

    /// Puts the caret at `offset`, selecting nothing.
    fn move_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        self.selected = offset..offset;
        self.reversed = false;
    }

    /// Moves the caret to `offset`, selecting from where the selection
    /// began.
    fn select_to(&mut self, offset: usize) {
        let anchor = if self.reversed {
            self.selected.end
        } else {
            self.selected.start
        };
        self.set_selection(anchor, self.clamp(offset));
    }

    /// Selects `range`, with the caret at its end.
    fn select(&mut self, range: Range<usize>) {
        let range = self.clamp_range(range);
        self.set_selection(range.start, range.end);
    }

    fn select_all(&mut self) {
        self.set_selection(0, self.text.len());
    }

    fn set_selection(&mut self, anchor: usize, caret: usize) {
        self.reversed = caret < anchor;
        self.selected = anchor.min(caret)..anchor.max(caret);
    }

    fn selected_text(&self) -> &str {
        &self.text[self.selected.clone()]
    }

    /// What typing replaces: the text being composed, else the selection.
    fn target(&self) -> Range<usize> {
        self.marked.clone().unwrap_or_else(|| self.selected.clone())
    }

    /// Puts `new` in place of `range`, with the caret after it and nothing
    /// being composed. Whether the text changed.
    fn replace(&mut self, range: Range<usize>, new: &str) -> bool {
        let range = self.clamp_range(range);
        let changed = &self.text[range.clone()] != new;
        self.text.replace_range(range.clone(), new);
        self.move_to(range.start + new.len());
        self.marked = None;
        changed
    }

    /// Types or pastes `new` over the text being composed or the
    /// selection, its line ends made `\n`. Whether the text changed.
    fn insert(&mut self, new: &str) -> bool {
        self.replace(self.target(), &normalize(new))
    }

    /// Puts `new` in place of `range` as text the input method is still
    /// composing, selecting `selected` within it, or with the caret after
    /// it. Whether the text changed.
    fn replace_and_mark(
        &mut self,
        range: Range<usize>,
        new: &str,
        selected: Option<Range<usize>>,
    ) -> bool {
        let range = self.clamp_range(range);
        let changed = &self.text[range.clone()] != new;
        self.text.replace_range(range.clone(), new);
        let start = range.start;
        self.marked = (!new.is_empty()).then(|| start..start + new.len());
        let selected = selected.map_or(new.len()..new.len(), |selected| {
            let end = selected.end.min(new.len());
            selected.start.min(end)..end
        });
        self.selected = start + selected.start..start + selected.end;
        self.reversed = false;
        changed
    }

    /// Deletes the selection, or else from the caret to `offset`. Whether
    /// the text changed.
    fn delete_to(&mut self, offset: usize) -> bool {
        let range = if self.selected.is_empty() {
            let caret = self.caret();
            let offset = self.clamp(offset);
            caret.min(offset)..caret.max(offset)
        } else {
            self.selected.clone()
        };
        self.replace(range, "")
    }

    /// The start of the character before `offset`, as the reader sees
    /// characters.
    fn prev_cluster(&self, offset: usize) -> usize {
        cluster_starts(&self.text)
            .into_iter()
            .rev()
            .find(|&start| start < offset)
            .unwrap_or(0)
    }

    /// The end of the character after `offset`, as the reader sees
    /// characters.
    fn next_cluster(&self, offset: usize) -> usize {
        cluster_starts(&self.text)
            .into_iter()
            .find(|&start| start > offset)
            .unwrap_or(self.text.len())
    }

    /// Each character as the reader sees it: where it starts, and what it
    /// is for moving by word.
    fn clusters(&self) -> Vec<(usize, Kind)> {
        let mut clusters: Vec<(usize, Kind)> = cluster_starts(&self.text)
            .into_iter()
            .map(|start| {
                (
                    start,
                    self.text[start..].chars().next().map_or(Kind::Other, kind),
                )
            })
            .collect();
        // An apostrophe between letters is part of the word: "don't".
        for i in 1..clusters.len().saturating_sub(1) {
            let apostrophe = matches!(
                self.text[clusters[i].0..].chars().next(),
                Some('\'' | '\u{2019}')
            );
            if apostrophe && clusters[i - 1].1 == Kind::Word && clusters[i + 1].1 == Kind::Word {
                clusters[i].1 = Kind::Word;
            }
        }
        clusters
    }

    /// The start of the word before `offset`, past any spaces and
    /// punctuation, as ⌥← moves.
    fn prev_word(&self, offset: usize) -> usize {
        let clusters = self.clusters();
        let mut i = clusters.partition_point(|&(start, _)| start < offset);
        while i > 0 && clusters[i - 1].1 != Kind::Word {
            i -= 1;
        }
        while i > 0 && clusters[i - 1].1 == Kind::Word {
            i -= 1;
        }
        clusters.get(i).map_or(self.text.len(), |&(start, _)| start)
    }

    /// The end of the word after `offset`, past any spaces and
    /// punctuation, as ⌥→ moves.
    fn next_word(&self, offset: usize) -> usize {
        let clusters = self.clusters();
        let mut i = clusters.partition_point(|&(start, _)| start < offset);
        while i < clusters.len() && clusters[i].1 != Kind::Word {
            i += 1;
        }
        while i < clusters.len() && clusters[i].1 == Kind::Word {
            i += 1;
        }
        clusters.get(i).map_or(self.text.len(), |&(start, _)| start)
    }

    /// What a double click at `offset` selects: the word or the run of
    /// spaces there, or the one other character. At the end of a line, it
    /// is what comes before.
    fn word_at(&self, offset: usize) -> Range<usize> {
        let clusters = self.clusters();
        let end_of = |i: usize| clusters.get(i).map_or(self.text.len(), |&(start, _)| start);
        let Some(mut i) = clusters
            .partition_point(|&(start, _)| start <= offset)
            .checked_sub(1)
        else {
            return offset..offset;
        };
        if clusters[i].1 == Kind::Newline && i > 0 && clusters[i - 1].1 != Kind::Newline {
            i -= 1;
        }
        match clusters[i].1 {
            Kind::Newline => offset..offset,
            Kind::Other => clusters[i].0..end_of(i + 1),
            kind => {
                let mut first = i;
                while first > 0 && clusters[first - 1].1 == kind {
                    first -= 1;
                }
                let mut last = i;
                while last + 1 < clusters.len() && clusters[last + 1].1 == kind {
                    last += 1;
                }
                clusters[first].0..end_of(last + 1)
            }
        }
    }

    /// The start of the line `offset` is in.
    fn line_start(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);
        self.text[..offset].rfind('\n').map_or(0, |i| i + 1)
    }

    /// The end of the line `offset` is in, before its line break.
    fn line_end(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);
        self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |i| offset + i)
    }

    /// The same column in the line above `offset`, or as near as it has;
    /// none on the first line. For text not yet wrapped.
    fn line_up(&self, offset: usize) -> Option<usize> {
        let start = self.line_start(offset);
        let above = self.line_start(start.checked_sub(1)?);
        Some(self.at_column(above..start - 1, self.column(start, offset)))
    }

    /// The same column in the line below `offset`, or as near as it has;
    /// none on the last line. For text not yet wrapped.
    fn line_down(&self, offset: usize) -> Option<usize> {
        let end = self.line_end(offset);
        if end == self.text.len() {
            return None;
        }
        let below = end + 1;
        let column = self.column(self.line_start(offset), offset);
        Some(self.at_column(below..self.line_end(below), column))
    }

    /// How many characters `offset` is past `start`.
    fn column(&self, start: usize, offset: usize) -> usize {
        self.text[start..self.clamp(offset).max(start)]
            .chars()
            .count()
    }

    /// The offset `column` characters into `line`, or its end.
    fn at_column(&self, line: Range<usize>, column: usize) -> usize {
        self.text[line.clone()]
            .char_indices()
            .nth(column)
            .map_or(line.end, |(i, _)| line.start + i)
    }

    /// `offset` counted in UTF-16 code units, as the input method counts.
    fn to_utf16(&self, offset: usize) -> usize {
        self.text[..self.clamp(offset)].encode_utf16().count()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.to_utf16(range.start)..self.to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        let start = utf8_offset(&self.text, range.start);
        start..utf8_offset(&self.text, range.end).max(start)
    }
}

/// The UTF-8 offset in `text` of `offset` UTF-16 code units, rounded up
/// to a character boundary.
fn utf8_offset(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (i, c) in text.char_indices() {
        if units >= offset {
            return i;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// `text` with Windows (`\r\n`) and old Mac (`\r`) line ends made `\n`.
fn normalize(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// What a character is, for moving and selecting by word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Space,
    Newline,
    Other,
}

fn kind(c: char) -> Kind {
    if c == '\n' {
        Kind::Newline
    } else if c.is_whitespace() {
        Kind::Space
    } else if c.is_alphanumeric() || c == '_' {
        Kind::Word
    } else {
        Kind::Other
    }
}

/// The zero-width joiner, which joins emoji into one: 👨‍👩‍👧.
const ZWJ: char = '\u{200D}';

/// Where each character as the reader sees it starts in `text`: a
/// character with its accents, variation and skin-tone selectors, an
/// emoji joined to the next by [`ZWJ`], a flag's two letters. Close to
/// Unicode's grapheme clusters, enough for accents and emoji.
fn cluster_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut prev = None;
    // Regional indicators in a row before this character; flags pair them.
    let mut letters = 0usize;
    for (offset, c) in text.char_indices() {
        let joins = match prev {
            None | Some('\n') => false,
            Some(prev) => {
                c != '\n'
                    && (prev == ZWJ || extends(c) || (is_regional(c) && !letters.is_multiple_of(2)))
            }
        };
        letters = if is_regional(c) { letters + 1 } else { 0 };
        if !joins {
            starts.push(offset);
        }
        prev = Some(c);
    }
    starts
}

/// Whether `c` is drawn as part of the character before it: an accent, a
/// variation or skin-tone selector, a joiner or a tag.
fn extends(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0E31
            | 0x0E34..=0x0E3A
            | 0x0E47..=0x0E4E
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200C..=0x200D
            | 0x20D0..=0x20FF
            | 0x3099..=0x309A
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0x1F3FB..=0x1F3FF
            | 0xE0020..=0xE007F
            | 0xE0100..=0xE01EF
    )
}

/// Whether `c` is one of the letters flags are spelled with.
fn is_regional(c: char) -> bool {
    matches!(u32::from(c), 0x1F1E6..=0x1F1FF)
}

/// One row of the text as drawn: a line, or the part of one before or
/// after where it wraps.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Row {
    /// Which of the text's lines it is part of.
    line: usize,
    /// Where it starts in the text.
    start: usize,
    /// Where it ends: its line's end, or where the next row starts.
    end: usize,
    /// Whether it ends its line, rather than wrapping.
    last: bool,
    /// How far across its line, unwrapped, it starts.
    x0: Pixels,
}

/// The row `offset` is drawn in. At a wrap, that is the row after it.
fn row_of(rows: &[Row], offset: usize) -> usize {
    rows.partition_point(|row| row.start <= offset)
        .saturating_sub(1)
}

/// The furthest the caret goes in `row`: its end, or in a row that wraps,
/// before its last character, since the end is the next row's start.
fn caret_end(row: &Row, buffer: &Buffer) -> usize {
    if row.last {
        row.end
    } else {
        buffer.prev_cluster(row.end).max(row.start)
    }
}

/// The text as last drawn, to find offsets from points and back.
struct Drawn {
    /// The [`Composer::revision`] it was drawn for.
    revision: u64,
    /// The field, in window coordinates.
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    /// The text's lines, shaped and wrapped (or the placeholder's, for
    /// an empty field), with where each starts in the text.
    lines: Vec<(usize, WrappedLine)>,
    /// The text's rows, top to bottom.
    rows: Vec<Row>,
    /// How tall all the rows are.
    content_height: Pixels,
}

impl Drawn {
    /// How far across `row` `offset` is drawn.
    fn x_of(&self, row: &Row, offset: usize) -> Pixels {
        let Some((start, line)) = self.lines.get(row.line) else {
            return Pixels::ZERO;
        };
        line.unwrapped_layout
            .x_for_index(offset.saturating_sub(*start))
            - row.x0
    }

    /// The row `offset` is drawn in and how far across it.
    fn caret_at(&self, offset: usize) -> (usize, Pixels) {
        let index = row_of(&self.rows, offset);
        match self.rows.get(index) {
            Some(row) => (index, self.x_of(row, offset)),
            None => (0, Pixels::ZERO),
        }
    }

    /// The offset in row `index` nearest `x` across it.
    fn offset_in_row(&self, index: usize, x: Pixels, buffer: &Buffer) -> usize {
        let Some(row) = self.rows.get(index) else {
            return 0;
        };
        let Some((start, line)) = self.lines.get(row.line) else {
            return row.start;
        };
        let offset = start + line.unwrapped_layout.closest_index_for_x(row.x0 + x);
        offset.clamp(row.start, caret_end(row, buffer))
    }
}

/// The rows of `lines`, wrapped as shaped.
fn rows_of(lines: &[(usize, WrappedLine)]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(lines.len());
    for (index, (start, line)) in lines.iter().enumerate() {
        let layout = &line.unwrapped_layout;
        let mut row_start = 0;
        let mut x0 = Pixels::ZERO;
        for boundary in line.wrap_boundaries() {
            let glyph = &layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
            rows.push(Row {
                line: index,
                start: start + row_start,
                end: start + glyph.index,
                last: false,
                x0,
            });
            row_start = glyph.index;
            x0 = glyph.position.x;
        }
        rows.push(Row {
            line: index,
            start: start + row_start,
            end: start + line.len(),
            last: true,
            x0,
        });
    }
    rows
}

/// The runs to shape `len` bytes of text with, underlining `marked`.
fn text_runs(len: usize, font: Font, color: Hsla, marked: Option<Range<usize>>) -> Vec<TextRun> {
    let run = TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let Some(marked) = marked.filter(|marked| marked.start < marked.end && marked.end <= len)
    else {
        return vec![run];
    };
    let underlined = TextRun {
        len: marked.len(),
        underline: Some(UnderlineStyle {
            color: Some(color),
            thickness: px(1.0),
            wavy: false,
        }),
        ..run.clone()
    };
    [
        TextRun {
            len: marked.start,
            ..run.clone()
        },
        underlined,
        TextRun {
            len: len - marked.end,
            ..run
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

/// What selecting `selected` paints, a box a row, from `origin`, the
/// text's top left.
fn selection_quads(
    drawn: &Drawn,
    selected: &Range<usize>,
    origin: Point<Pixels>,
    color: Hsla,
    newline_width: Pixels,
) -> Vec<PaintQuad> {
    if selected.is_empty() {
        return Vec::new();
    }
    let height = drawn.line_height;
    drawn
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let start = selected.start.max(row.start);
            let end = selected.end.min(row.end);
            // A selection on past the line's end takes its line break.
            let past_end = row.last && selected.end > row.end;
            if start > end || (start == end && !past_end) {
                return None;
            }
            let left = drawn.x_of(row, start);
            let mut right = drawn.x_of(row, end);
            if past_end {
                right += newline_width;
            }
            let top = origin.y + height * index;
            Some(fill(
                Bounds::from_corners(
                    point(origin.x + left, top),
                    point(origin.x + right, top + height),
                ),
                color,
            ))
        })
        .collect()
}

/// The composer's text, caret and selection, drawn.
struct Field {
    composer: Entity<Composer>,
}

/// What [`Field`] paints, worked out before painting.
struct Painting {
    /// The text's top left: the field's, scrolled.
    origin: Point<Pixels>,
    line_height: Pixels,
    /// The lines and how far down each starts.
    lines: Vec<(Pixels, WrappedLine)>,
    selections: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl IntoElement for Field {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Field {
    type RequestLayoutState = ();
    type PrepaintState = Option<Painting>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    /// As wide as it is given, and as tall as its text wrapped to that,
    /// from one row to the most the style allows.
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let composer = self.composer.read(cx);
        let style = composer.style;
        let text: SharedString = if composer.buffer.text.is_empty() {
            composer.placeholder.clone()
        } else {
            composer.buffer.text.clone().into()
        };
        let font = window.text_style().font();
        let mut layout = Style::default();
        layout.size.width = relative(1.0).into();
        let id = window.request_measured_layout(layout, move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let runs = text_runs(text.len(), font.clone(), style.text, None);
            let rows = window
                .text_system()
                .shape_text(text.clone(), style.font_size, &runs, width, None)
                .map_or(1, |lines| {
                    lines
                        .iter()
                        .map(|line| line.wrap_boundaries().len() + 1)
                        .sum()
                });
            let height = (style.line_height * rows.max(1))
                .min(style.max_height)
                .max(style.line_height);
            size(width.unwrap_or_default(), height)
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let composer = self.composer.read(cx);
        let style = composer.style;
        let buffer = &composer.buffer;
        let empty = buffer.text.is_empty();
        let (text, color, marked): (SharedString, _, _) = if empty {
            (composer.placeholder.clone(), style.placeholder, None)
        } else {
            (
                buffer.text.clone().into(),
                style.text,
                buffer.marked.clone(),
            )
        };
        let runs = text_runs(text.len(), window.text_style().font(), color, marked);
        let shaped = window
            .text_system()
            .shape_text(text, style.font_size, &runs, Some(bounds.size.width), None)
            .ok()?;

        let line_height = style.line_height;
        let mut lines = Vec::with_capacity(shaped.len());
        let mut painted = Vec::with_capacity(shaped.len());
        let (mut start, mut top) = (0, Pixels::ZERO);
        for line in shaped {
            let len = line.len();
            painted.push((top, line.clone()));
            top += line_height * (line.wrap_boundaries().len() + 1);
            lines.push((start, line));
            start += len + 1;
        }
        let rows = if empty {
            vec![Row {
                line: 0,
                start: 0,
                end: 0,
                last: true,
                x0: Pixels::ZERO,
            }]
        } else {
            rows_of(&lines)
        };
        let drawn = Drawn {
            revision: composer.revision,
            bounds,
            line_height,
            lines,
            rows,
            content_height: top.max(line_height),
        };

        let (caret_row, caret_x) = drawn.caret_at(buffer.caret());
        let mut scroll = composer.scroll;
        if composer.reveal_caret {
            let caret_top = line_height * caret_row;
            if caret_top < scroll {
                scroll = caret_top;
            } else if caret_top + line_height > scroll + bounds.size.height {
                scroll = caret_top + line_height - bounds.size.height;
            }
        }
        let most = (drawn.content_height - bounds.size.height).max(Pixels::ZERO);
        let scroll = scroll.clamp(Pixels::ZERO, most);
        let origin = point(bounds.left(), bounds.top() - scroll);

        let focused = composer.focus.is_focused(window);
        // An inactive selection is drawn fainter, as macOS draws one.
        let selection = if focused {
            style.selection
        } else {
            style.selection.opacity(0.5)
        };
        let selections = selection_quads(
            &drawn,
            &buffer.selected,
            origin,
            selection,
            style.font_size * NEWLINE_WIDTH,
        );
        let caret = (focused && buffer.selected.is_empty()).then(|| {
            let height = (style.font_size * CARET_HEIGHT).min(line_height);
            let top = origin.y + line_height * caret_row + (line_height - height) * 0.5;
            fill(
                Bounds::new(
                    point(origin.x + caret_x, top),
                    size(px(CARET_WIDTH), height),
                ),
                style.caret,
            )
        });

        self.composer.update(cx, |composer, _| {
            composer.scroll = scroll;
            composer.reveal_caret = false;
            composer.drawn = Some(drawn);
        });
        Some(Painting {
            origin,
            line_height,
            lines: painted,
            selections,
            caret,
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        painting: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.composer.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.composer.clone()),
            cx,
        );
        if let Some(painting) = painting.take() {
            // Wide enough for the caret at either edge.
            let mask = Bounds::new(
                point(bounds.left() - px(CARET_WIDTH), bounds.top()),
                size(
                    bounds.size.width + px(CARET_WIDTH * 2.0),
                    bounds.size.height,
                ),
            );
            window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                for quad in painting.selections {
                    window.paint_quad(quad);
                }
                for (top, line) in &painting.lines {
                    let origin = point(painting.origin.x, painting.origin.y + *top);
                    let height = painting.line_height * (line.wrap_boundaries().len() + 1);
                    // Only what shows.
                    if origin.y + height < bounds.top() || origin.y > bounds.bottom() {
                        continue;
                    }
                    line.paint(
                        origin,
                        painting.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .ok();
                }
                if let Some(caret) = painting.caret {
                    window.paint_quad(caret);
                }
            });
        }
        // A drag that leaves the field still selects, and letting go
        // anywhere ends it.
        let composer = self.composer.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && event.pressed_button == Some(MouseButton::Left) {
                composer.update(cx, |composer, cx| composer.drag_to(event.position, cx));
            }
        });
        let composer = self.composer.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                composer.update(cx, |composer, _| composer.selecting = false);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::font;

    /// Each binding as its keys and action, sorted.
    fn written(bindings: &[KeyBinding]) -> Vec<String> {
        let mut written: Vec<String> = bindings
            .iter()
            .map(|binding| {
                let keys: Vec<String> = binding
                    .keystrokes()
                    .iter()
                    .map(|keystroke| keystroke.inner().unparse())
                    .collect();
                format!("{} {}", keys.join(" "), binding.action().name())
            })
            .collect();
        written.sort();
        written
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "checks macOS's own text keys (by design)"
    )]
    fn on_macos_the_text_keys_are_what_they_were() {
        let context = Some(KEY_CONTEXT);
        let before = [
            KeyBinding::new("enter", Submit, context),
            KeyBinding::new("shift-enter", Newline, context),
            KeyBinding::new("alt-enter", Newline, context),
            KeyBinding::new("escape", Cancel, context),
            KeyBinding::new("left", MoveLeft, context),
            KeyBinding::new("right", MoveRight, context),
            KeyBinding::new("up", MoveUp, context),
            KeyBinding::new("down", MoveDown, context),
            KeyBinding::new("alt-left", MoveWordLeft, context),
            KeyBinding::new("alt-right", MoveWordRight, context),
            KeyBinding::new("cmd-left", MoveToLineStart, context),
            KeyBinding::new("cmd-right", MoveToLineEnd, context),
            KeyBinding::new("home", MoveToLineStart, context),
            KeyBinding::new("end", MoveToLineEnd, context),
            KeyBinding::new("ctrl-a", MoveToLineStart, context),
            KeyBinding::new("ctrl-e", MoveToLineEnd, context),
            KeyBinding::new("cmd-up", MoveToStart, context),
            KeyBinding::new("cmd-down", MoveToEnd, context),
            KeyBinding::new("shift-left", SelectLeft, context),
            KeyBinding::new("shift-right", SelectRight, context),
            KeyBinding::new("shift-up", SelectUp, context),
            KeyBinding::new("shift-down", SelectDown, context),
            KeyBinding::new("alt-shift-left", SelectWordLeft, context),
            KeyBinding::new("alt-shift-right", SelectWordRight, context),
            KeyBinding::new("cmd-shift-left", SelectToLineStart, context),
            KeyBinding::new("cmd-shift-right", SelectToLineEnd, context),
            KeyBinding::new("shift-home", SelectToLineStart, context),
            KeyBinding::new("shift-end", SelectToLineEnd, context),
            KeyBinding::new("cmd-shift-up", SelectToStart, context),
            KeyBinding::new("cmd-shift-down", SelectToEnd, context),
            KeyBinding::new("cmd-a", SelectAll, context),
            KeyBinding::new("backspace", Backspace, context),
            KeyBinding::new("shift-backspace", Backspace, context),
            KeyBinding::new("ctrl-h", Backspace, context),
            KeyBinding::new("delete", Delete, context),
            KeyBinding::new("ctrl-d", Delete, context),
            KeyBinding::new("alt-backspace", DeleteWordLeft, context),
            KeyBinding::new("alt-delete", DeleteWordRight, context),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, context),
            KeyBinding::new("cmd-delete", DeleteToLineEnd, context),
            KeyBinding::new("ctrl-k", DeleteToLineEnd, context),
            KeyBinding::new("cmd-c", CopySelection, context),
            KeyBinding::new("cmd-x", CutSelection, context),
            KeyBinding::new("cmd-v", Paste, context),
            KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, context),
        ];
        assert_eq!(written(&bindings()), written(&before));
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "checks Linux's and Windows's text keys (by design)"
    )]
    fn off_macos_control_edits_text_as_linux_and_windows_fields_do() {
        let written = written(&bindings());
        let has = |keys: &str, action: &str| {
            written
                .iter()
                .any(|line| line.starts_with(&format!("{keys} ")) && line.ends_with(action))
        };
        assert!(has("ctrl-c", "CopySelection"));
        assert!(has("ctrl-v", "Paste"));
        assert!(has("ctrl-a", "SelectAll"));
        assert!(has("ctrl-left", "MoveWordLeft"));
        assert!(has("ctrl-shift-right", "SelectWordRight"));
        assert!(has("ctrl-backspace", "DeleteWordLeft"));
        assert!(has("ctrl-home", "MoveToStart"));
        assert!(
            !written
                .iter()
                .any(|line| line.contains("cmd-") || line.contains("super-")),
            "no binding takes the logo key"
        );
    }

    /// `text` with the caret at `caret`.
    fn buffer(text: &str, caret: usize) -> Buffer {
        let mut buffer = Buffer {
            text: text.to_owned(),
            ..Buffer::default()
        };
        buffer.move_to(caret);
        buffer
    }

    #[test]
    fn typing_goes_in_at_the_caret_and_moves_it_on() {
        let mut b = buffer("held", 2);
        assert!(b.insert("ll"));
        assert_eq!(b.text, "hellld");
        assert_eq!(b.caret(), 4);
        assert!(b.selected.is_empty());
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut b = buffer("hello world", 0);
        b.select(6..11);
        assert_eq!(b.selected_text(), "world");
        assert!(b.insert("there"));
        assert_eq!(b.text, "hello there");
        assert_eq!(b.caret(), 11);
    }

    #[test]
    fn backspace_and_delete_take_a_character_or_the_selection() {
        let mut b = buffer("abc", 2);
        assert!(b.delete_to(b.prev_cluster(b.caret())));
        assert_eq!((b.text.as_str(), b.caret()), ("ac", 1));
        assert!(b.delete_to(b.next_cluster(b.caret())));
        assert_eq!((b.text.as_str(), b.caret()), ("a", 1));
        // Nothing past either end.
        assert!(!b.delete_to(b.next_cluster(b.caret())));
        b.move_to(0);
        assert!(!b.delete_to(b.prev_cluster(0)));
        assert_eq!(b.text, "a");

        let mut b = buffer("one two", 0);
        b.select(1..5);
        assert!(b.delete_to(0));
        assert_eq!((b.text.as_str(), b.caret()), ("owo", 1));
    }

    #[test]
    fn selecting_past_the_anchor_turns_the_selection_round() {
        let mut b = buffer("abcdef", 3);
        b.select_to(5);
        assert_eq!((b.selected.clone(), b.caret()), (3..5, 5));
        b.select_to(1);
        assert_eq!((b.selected.clone(), b.caret()), (1..3, 1));
        b.select_to(3);
        assert!(b.selected.is_empty());
        b.select_all();
        assert_eq!(b.selected, 0..6);
        b.move_to(99);
        assert_eq!(b.caret(), 6);
    }

    #[test]
    fn word_moves_skip_spaces_and_punctuation() {
        let b = buffer("hello, world  foo_bar", 0);
        assert_eq!(b.next_word(0), 5);
        assert_eq!(b.next_word(5), 12);
        assert_eq!(b.next_word(12), 21);
        assert_eq!(b.next_word(21), 21);
        assert_eq!(b.prev_word(21), 14);
        assert_eq!(b.prev_word(14), 7);
        assert_eq!(b.prev_word(9), 7);
        assert_eq!(b.prev_word(7), 0);
        assert_eq!(b.prev_word(0), 0);
    }

    #[test]
    fn word_moves_cross_lines_and_keep_apostrophes() {
        let b = buffer("don't\nstop", 0);
        assert_eq!(b.next_word(0), 5);
        assert_eq!(b.next_word(5), 10);
        assert_eq!(b.prev_word(10), 6);
        assert_eq!(b.prev_word(6), 0);
        // A quote at a word's edge is not part of it.
        let b = buffer("'hi'", 0);
        assert_eq!(b.next_word(0), 3);
    }

    #[test]
    fn deleting_by_word() {
        let mut b = buffer("say hello there", 9);
        assert!(b.delete_to(b.prev_word(b.caret())));
        assert_eq!((b.text.as_str(), b.caret()), ("say  there", 4));
        assert!(b.delete_to(b.next_word(b.caret())));
        assert_eq!((b.text.as_str(), b.caret()), ("say ", 4));
    }

    #[test]
    fn a_double_click_selects_a_word_spaces_or_a_mark() {
        let b = buffer("one  two, three\n\nfour", 0);
        assert_eq!(b.word_at(1), 0..3);
        assert_eq!(b.word_at(3), 3..5);
        assert_eq!(b.word_at(5), 5..8);
        assert_eq!(b.word_at(8), 8..9);
        // The end of a line selects the word before it.
        assert_eq!(b.word_at(15), 10..15);
        // An empty line selects nothing.
        assert_eq!(b.word_at(16), 16..16);
        assert_eq!(b.word_at(21), 17..21);
        assert_eq!(buffer("", 0).word_at(0), 0..0);
    }

    #[test]
    fn lines_start_and_end_at_line_breaks() {
        let b = buffer("ab\n\ncde", 0);
        assert_eq!((b.line_start(1), b.line_end(1)), (0, 2));
        assert_eq!((b.line_start(2), b.line_end(2)), (0, 2));
        assert_eq!((b.line_start(3), b.line_end(3)), (3, 3));
        assert_eq!((b.line_start(6), b.line_end(6)), (4, 7));
    }

    #[test]
    fn moving_up_and_down_lines_keeps_the_column() {
        let b = buffer("abcd\nx\nwxyz", 0);
        assert_eq!(b.line_down(3), Some(6));
        assert_eq!(b.line_down(6), Some(8));
        assert_eq!(b.line_down(9), None);
        assert_eq!(b.line_up(9), Some(6));
        assert_eq!(b.line_up(5), Some(0));
        assert_eq!(b.line_up(2), None);
    }

    #[test]
    fn a_new_line_is_a_line_break_in_the_text() {
        let mut b = buffer("ab", 1);
        assert!(b.insert("\n"));
        assert_eq!((b.text.as_str(), b.caret()), ("a\nb", 2));
        assert_eq!(b.line_start(b.caret()), 2);
        // Backspace at a line's start joins it to the one above.
        assert!(b.delete_to(b.prev_cluster(b.caret())));
        assert_eq!((b.text.as_str(), b.caret()), ("ab", 1));
    }

    #[test]
    fn pasting_makes_every_line_end_a_line_break() {
        let mut b = buffer("", 0);
        assert!(b.insert("one\r\ntwo\rthree\nfour\t!"));
        assert_eq!(b.text, "one\ntwo\nthree\nfour\t!");
    }

    #[test]
    fn multibyte_characters_move_and_delete_whole() {
        // é is two bytes, 日 three, 👍🏽 a thumb and a skin tone.
        let mut b = buffer("é日👍🏽", 0);
        assert_eq!(b.next_cluster(0), 2);
        assert_eq!(b.next_cluster(2), 5);
        assert_eq!(b.next_cluster(5), 13);
        assert_eq!(b.prev_cluster(13), 5);
        b.move_to(13);
        assert!(b.delete_to(b.prev_cluster(b.caret())));
        assert_eq!(b.text, "é日");
        // An offset inside a character is moved back to its start.
        assert_eq!(b.clamp(4), 2);
        assert!(b.insert("x"));
        assert_eq!(b.text, "é日x");
    }

    #[test]
    fn accents_joined_emoji_and_flags_are_one_character_each() {
        let text = "e\u{301}👨\u{200D}👩\u{200D}👧🇫🇷🇩🇪a";
        let family = 1 + 2 + 4 + 3 + 4 + 3 + 4;
        assert_eq!(
            cluster_starts(text),
            vec![0, 3, family, family + 8, family + 16]
        );
        assert_eq!(cluster_starts("a\n\u{301}"), vec![0, 1, 2]);
    }

    #[test]
    fn utf16_offsets_round_trip() {
        let b = buffer("a😀b", 0);
        assert_eq!(b.to_utf16(1), 1);
        assert_eq!(b.to_utf16(5), 3);
        assert_eq!(b.to_utf16(6), 4);
        assert_eq!(b.range_from_utf16(&(1..3)), 1..5);
        assert_eq!(b.range_to_utf16(&(1..6)), 1..4);
        // Inside the surrogate pair rounds up past the emoji.
        assert_eq!(utf8_offset(&b.text, 2), 5);
        assert_eq!(utf8_offset(&b.text, 99), 6);
    }

    #[test]
    fn composing_marks_text_until_it_is_committed() {
        let mut b = buffer("ab", 1);
        // An input method starts composing at the caret...
        assert!(b.replace_and_mark(b.target(), "に", None));
        assert_eq!(b.text, "aにb");
        assert_eq!(b.marked, Some(1..4));
        assert_eq!(b.caret(), 4);
        // ...replaces what it composed, selecting within it...
        assert!(b.replace_and_mark(b.target(), "日本", Some(0..3)));
        assert_eq!(b.text, "a日本b");
        assert_eq!(b.marked, Some(1..7));
        assert_eq!(b.selected, 1..4);
        // ...and commits it.
        assert!(!b.replace(b.target(), "日本"));
        assert_eq!(b.marked, None);
        assert_eq!(b.caret(), 7);
        // Composing nothing leaves nothing marked.
        b.replace_and_mark(7..7, "", None);
        assert_eq!(b.marked, None);
    }

    #[test]
    fn the_caret_at_a_wrap_is_on_the_row_after() {
        let row = |line, start, end, last| Row {
            line,
            start,
            end,
            last,
            x0: px(0.0),
        };
        // "ab cd" wrapped before "cd", an empty line, then "ef".
        let rows = [
            row(0, 0, 3, false),
            row(0, 3, 5, true),
            row(1, 6, 6, true),
            row(2, 7, 9, true),
        ];
        let found: Vec<usize> = [0, 2, 3, 5, 6, 7, 9].map(|o| row_of(&rows, o)).into();
        assert_eq!(found, vec![0, 0, 1, 1, 2, 3, 3]);
        let b = buffer("ab cd\n\nef", 0);
        assert_eq!(caret_end(&rows[0], &b), 2);
        assert_eq!(caret_end(&rows[1], &b), 5);
        assert_eq!(caret_end(&rows[2], &b), 6);
    }

    #[test]
    fn composing_underlines_its_run() {
        let runs = text_runs(6, font(".SystemUIFont"), Hsla::default(), Some(2..4));
        let lens: Vec<(usize, bool)> = runs
            .iter()
            .map(|run| (run.len, run.underline.is_some()))
            .collect();
        assert_eq!(lens, vec![(2, false), (2, true), (2, false)]);
        assert_eq!(
            text_runs(6, font(".SystemUIFont"), Hsla::default(), Some(0..9)).len(),
            1
        );
    }
}
