//! The layout probe, for `scripts/macos/check-layout.sh`. GPUI 0.2 gives
//! the accessibility tree no elements, so the checks read element frames
//! from here instead: with `DS_EMULATOR_PROBE` naming a folder, each window
//! writes its named parts' frames to `<window>.tsv` there whenever they
//! change. One line a part, `name x y width height`, in logical pixels from
//! the window's top left. Without the variable, nothing is drawn or
//! written.
//!
//! A window's root starts with [`begin`] and ends with [`end`]; a part is
//! a [`part`] child of the element it names, which it covers: a
//! [`bordered_part`] if the element has a border, which it covers too.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

use gpui::{Bounds, Pixels, SharedString, Styled, Window, canvas, prelude::*, px};

/// Where the frames go, if anywhere.
fn folder() -> Option<&'static PathBuf> {
    static FOLDER: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOLDER
        .get_or_init(|| std::env::var_os("DS_EMULATOR_PROBE").map(PathBuf::from))
        .as_ref()
}

type Frames = BTreeMap<SharedString, Bounds<Pixels>>;

thread_local! {
    /// Each window's parts, by window: those drawn in this frame, and
    /// those last written.
    static WINDOWS: RefCell<HashMap<u64, (Frames, Frames)>> = RefCell::default();
}

fn window_key(window: &Window) -> u64 {
    window.window_handle().window_id().as_u64()
}

/// Starts a window's frame: the first child of its root.
pub fn begin() -> Option<impl IntoElement> {
    folder()?;
    Some(
        canvas(
            |_, window, _| {
                WINDOWS.with_borrow_mut(|windows| {
                    windows.entry(window_key(window)).or_default().0.clear();
                });
            },
            |_, _, _, _| {},
        )
        .absolute(),
    )
}

/// Names the element it is a child of `name`.
pub fn part(name: impl Into<SharedString>) -> Option<impl IntoElement> {
    bordered_part(name, 0.0)
}

/// Names the element it is a child of, which has a `border` logical pixels
/// thick, `name`. A child is placed inside its parent's border.
pub fn bordered_part(name: impl Into<SharedString>, border: f32) -> Option<impl IntoElement> {
    folder()?;
    let name = name.into();
    Some(
        canvas(
            move |bounds, window, _| {
                WINDOWS.with_borrow_mut(|windows| {
                    let drawing = &mut windows.entry(window_key(window)).or_default().0;
                    drawing.insert(name, bounds);
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top(px(-border))
        .left(px(-border))
        .right(px(-border))
        .bottom(px(-border)),
    )
}

/// Ends a window's frame, writing its parts to `<file>.tsv` if they
/// changed: the last child of its root. Parts are recorded as they are
/// laid out, all before any is painted, when this writes.
pub fn end(file: &'static str) -> Option<impl IntoElement> {
    let path = folder()?.join(format!("{file}.tsv"));
    Some(
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let changed = WINDOWS.with_borrow_mut(|windows| {
                    let (drawing, written) = windows.entry(window_key(window)).or_default();
                    (drawing != written).then(|| {
                        written.clone_from(drawing);
                        tsv(drawing)
                    })
                });
                if let Some(text) = changed {
                    std::fs::write(&path, text).ok();
                }
            },
        )
        .absolute(),
    )
}

fn tsv(frames: &Frames) -> String {
    let mut text = String::new();
    for (name, bounds) in frames {
        let (origin, size) = (bounds.origin, bounds.size);
        writeln!(
            text,
            "{name}\t{}\t{}\t{}\t{}",
            f32::from(origin.x),
            f32::from(origin.y),
            f32::from(size.width),
            f32::from(size.height)
        )
        .ok();
    }
    text
}
