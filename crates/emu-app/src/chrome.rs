//! The window chrome the app draws itself where the system draws less of
//! it than macOS does: close, minimise and maximise buttons, an area to
//! drag the window by, and edges to resize it from.
//!
//! - **macOS**: AppKit draws the traffic lights in the transparent title
//!   bar ([`crate::app::top_bar`]), so nothing here is drawn.
//! - **Windows**: the transparent title bar takes the caption away, so the
//!   app draws the caption buttons in its top bar. GPUI hit-tests them as
//!   the system's own (`window_control_area`): the buttons, Snap Layouts
//!   on the maximise button, dragging, double-clicking and the window menu
//!   all behave as in any Windows app.
//! - **Linux**: X11 window managers and most Wayland compositors (KDE's,
//!   wlroots') frame windows themselves. GNOME's and Weston's offer no
//!   frames to Wayland windows, so there the windows ask to draw their own
//!   ([`window_decorations`]) and get buttons, a drag area, resize edges
//!   and a hairline edge from here.
//!
//! `[window] controls = false` hides the buttons everywhere, as it hides
//! the traffic lights on macOS; the window can still be dragged.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, CursorStyle, Decorations, DispatchPhase, Div, ElementId, MouseButton,
    MouseMoveEvent, MouseUpEvent, ResizeEdge, Stateful, Window, WindowControlArea,
    WindowDecorations, canvas, div, prelude::*, px, rgb, rgba,
};

use crate::app::TOP_BAR;
use crate::icons;
use crate::theme::{LINE, TEXT, with_alpha};

/// Who frames a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// The system, buttons and all: macOS's traffic lights, or a Linux
    /// window manager's frame.
    System,
    /// Windows, with the caption hidden under the app's top bar: the app
    /// draws the caption buttons and the system hit-tests them.
    Caption,
    /// The app, on a Linux compositor that draws no frames. `resizable`
    /// unless the window is maximised, tiled or full screen, which leaves
    /// no edge to drag.
    Client {
        /// Whether the window has edges to resize it from.
        resizable: bool,
    },
}

/// What a window's top bar is drawn over, which picks the buttons'
/// colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// White paper: the library, the store and the app's other windows.
    Paper,
    /// A running game, under the top bar's shade.
    Stage,
}

/// Who frames `window`, a window opened with the app's transparent top
/// bar ([`crate::app::top_bar`]).
pub fn frame(window: &Window) -> Frame {
    if cfg!(target_os = "macos") {
        return Frame::System;
    }
    if cfg!(windows) {
        return Frame::Caption;
    }
    match window.window_decorations() {
        Decorations::Server => Frame::System,
        Decorations::Client { tiling } => Frame::Client {
            resizable: !tiling.is_tiled() && !window.is_maximized() && !window.is_fullscreen(),
        },
    }
}

/// How the app's windows ask to be framed: by the app on a Linux Wayland
/// compositor that offers no frames (GNOME's, Weston's), else by the
/// system. `DS_EMULATOR_DECORATIONS=client` or `server` overrides it,
/// on X11 too.
pub fn window_decorations() -> Option<WindowDecorations> {
    match std::env::var("DS_EMULATOR_DECORATIONS").as_deref() {
        Ok("client") => return Some(WindowDecorations::Client),
        Ok("server") => return Some(WindowDecorations::Server),
        _ => {}
    }
    #[cfg(target_os = "linux")]
    if wayland::without_frames() {
        return Some(WindowDecorations::Client);
    }
    None
}

/// Windows's caption buttons: as wide as Windows 11 draws them, and the
/// top bar's height.
const CAPTION_BUTTON: f32 = 46.0;
/// The round buttons of a window the app frames on Linux, as GNOME's,
/// the room between them, and from the last to the window's side.
const ROUND_BUTTON: f32 = 24.0;
const ROUND_GAP: f32 = 10.0;
const ROUND_END: f32 = 10.0;
/// How far in from a window's edges, and its corners, resizing starts.
const RESIZE_EDGE: f32 = 5.0;
const RESIZE_CORNER: f32 = 12.0;

/// Windows 11's caption colours: the wash under the pointer and while
/// pressed, on paper and on the stage, and the close button's red.
const CAPTION_HOVER_PAPER: u32 = with_alpha(0x0000_0000, 0x0A);
const CAPTION_PRESS_PAPER: u32 = with_alpha(0x0000_0000, 0x06);
const CAPTION_HOVER_STAGE: u32 = with_alpha(0x00FF_FFFF, 0x1A);
const CAPTION_PRESS_STAGE: u32 = with_alpha(0x00FF_FFFF, 0x10);
const CLOSE_RED: u32 = 0x00C4_2B1C;
const CLOSE_RED_PRESSED: u32 = with_alpha(CLOSE_RED, 0xE6);
/// The buttons' glyphs over a game: near-white on the top bar's shade.
const ON_STAGE: u32 = 0x00F2_F2F4;

/// Which buttons a window has: minimise and maximise where the system
/// allows them, if the window can be `sizable` (minimised and resized),
/// and close.
struct Buttons {
    minimize: bool,
    maximize: bool,
}

fn buttons(window: &Window, sizable: bool) -> Buttons {
    let controls = window.window_controls();
    Buttons {
        minimize: sizable && controls.minimize,
        maximize: sizable && controls.maximize,
    }
}

/// How much of the right of `window`'s top bar its buttons take, for
/// what else is in the bar to keep clear of: nothing where the system
/// draws them. A window that is not `sizable` has only a close button.
pub fn trailing_room(window: &Window, sizable: bool) -> f32 {
    let Buttons { minimize, maximize } = buttons(window, sizable);
    let count = 1.0 + f32::from(u8::from(minimize)) + f32::from(u8::from(maximize));
    match frame(window) {
        Frame::System => 0.0,
        Frame::Caption => CAPTION_BUTTON * count,
        Frame::Client { .. } => ROUND_BUTTON * count + ROUND_GAP * (count - 1.0) + ROUND_END,
    }
}

/// The close, minimise and maximise buttons for the right of `window`'s
/// top bar, over `tone`, if the app draws them; the caller places them.
/// A window that is not `sizable` has only a close button.
pub fn buttons_for(window: &Window, tone: Tone, sizable: bool) -> Option<AnyElement> {
    let Buttons { minimize, maximize } = buttons(window, sizable);
    let restore = window.is_maximized();
    let size_glyph = if restore {
        icons::WINDOW_RESTORE
    } else {
        icons::WINDOW_MAXIMIZE
    };
    match frame(window) {
        Frame::System => None,
        Frame::Caption => Some(
            div()
                .flex()
                .h(px(TOP_BAR))
                .when(minimize, |row| {
                    row.child(caption_button(
                        "caption-minimize",
                        icons::WINDOW_MINIMIZE,
                        WindowControlArea::Min,
                        tone,
                    ))
                })
                .when(maximize, |row| {
                    row.child(caption_button(
                        "caption-maximize",
                        size_glyph,
                        WindowControlArea::Max,
                        tone,
                    ))
                })
                .child(caption_button(
                    "caption-close",
                    icons::WINDOW_CLOSE,
                    WindowControlArea::Close,
                    tone,
                ))
                .into_any_element(),
        ),
        Frame::Client { .. } => Some(
            div()
                .flex()
                .items_center()
                .h(px(TOP_BAR))
                .gap(px(ROUND_GAP))
                .pr(px(ROUND_END))
                .when(minimize, |row| {
                    row.child(
                        round_button("window-minimize", icons::WINDOW_MINIMIZE, tone)
                            .on_click(|_, window, _| window.minimize_window()),
                    )
                })
                .when(maximize, |row| {
                    row.child(
                        round_button("window-maximize", size_glyph, tone)
                            .on_click(|_, window, _| window.zoom_window()),
                    )
                })
                .child(
                    round_button("window-close", icons::WINDOW_CLOSE, tone)
                        .on_click(|_, window, _| window.remove_window()),
                )
                .into_any_element(),
        ),
    }
}

/// A Windows caption button: a flat glyph that the system presses,
/// washed under the pointer, the close button red.
fn caption_button(
    id: &'static str,
    glyph: &'static str,
    area: WindowControlArea,
    tone: Tone,
) -> Stateful<Div> {
    let colour = match tone {
        Tone::Paper => TEXT,
        Tone::Stage => ON_STAGE,
    };
    let (hover, press) = match (area, tone) {
        (WindowControlArea::Close, _) => (rgb(CLOSE_RED), rgba(CLOSE_RED_PRESSED)),
        (_, Tone::Paper) => (rgba(CAPTION_HOVER_PAPER), rgba(CAPTION_PRESS_PAPER)),
        (_, Tone::Stage) => (rgba(CAPTION_HOVER_STAGE), rgba(CAPTION_PRESS_STAGE)),
    };
    let close = area == WindowControlArea::Close;
    // Not a click target of the app's: the system takes the press, so
    // nothing here handles the mouse. Occluding keeps the drag area and
    // the game behind it from counting it as theirs.
    div()
        .id(id)
        .w(px(CAPTION_BUTTON))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .window_control_area(area)
        .text_color(rgb(colour))
        .hover(move |button| {
            let button = button.bg(hover);
            if close {
                button.text_color(rgb(0x00FF_FFFF))
            } else {
                button
            }
        })
        .active(move |button| button.bg(press))
        .child(
            gpui::svg()
                .path(glyph)
                .size(px(icons::SIZE))
                .flex_none()
                .text_color(rgb(colour)),
        )
}

/// A round button as GNOME draws a window's, in a soft disc that firms
/// up under the pointer. The caller says what a click does.
fn round_button(id: &'static str, glyph: &'static str, tone: Tone) -> Stateful<Div> {
    let (colour, disc, hover, press) = match tone {
        Tone::Paper => (
            TEXT,
            with_alpha(TEXT, 0x10),
            with_alpha(TEXT, 0x1C),
            with_alpha(TEXT, 0x2A),
        ),
        Tone::Stage => (
            ON_STAGE,
            with_alpha(0x00FF_FFFF, 0x1A),
            with_alpha(0x00FF_FFFF, 0x2A),
            with_alpha(0x00FF_FFFF, 0x3A),
        ),
    };
    div()
        .id(id)
        .size(px(ROUND_BUTTON))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .occlude()
        .cursor_pointer()
        .bg(rgba(disc))
        .hover(move |button| button.bg(rgba(hover)))
        .active(move |button| button.bg(rgba(press)))
        // A press here is the button's, not the start of a drag.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icons::icon(glyph, 14.0, colour))
}

/// Where the main window's menu button sits off macOS, in the top bar's
/// left corner where the traffic lights are on macOS: its icon's artwork
/// about as far in from the side as the close button is on macOS, and on
/// the bar's midline.
pub const MENU_BUTTON_LEFT: f32 = 8.0;
pub const MENU_BUTTON_TOP: f32 = (TOP_BAR - icons::BUTTON) / 2.0;

/// The button that opens the menu the window draws off macOS, over
/// `tone`, pressed while the menu is `open`. The caller says what a click
/// does.
pub fn menu_button(tone: Tone, open: bool) -> Stateful<Div> {
    let (colour, wash) = match tone {
        Tone::Paper => (TEXT, crate::theme::HOVER),
        Tone::Stage => (ON_STAGE, with_alpha(0x00FF_FFFF, 0x1F)),
    };
    div()
        .id("app-menu-button")
        .size(px(icons::BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .occlude()
        .cursor_pointer()
        .when(open, |button| button.bg(rgba(wash)))
        .hover(move |button| button.bg(rgba(wash)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icons::icon(icons::MENU, icons::SIZE, colour))
}

/// Makes `area`, a part of a top bar with nothing to press in it, drag
/// the window `window`, if the app frames it: as the system's own title
/// bar on Windows, and by hand on Linux, where a double click maximises
/// and a right click opens the compositor's window menu. `id` names the
/// area among the window's elements.
pub fn drag_area(
    area: Div,
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let id = id.into();
    match frame(window) {
        Frame::System => area.into_any_element(),
        Frame::Caption => area
            .id(id)
            .window_control_area(WindowControlArea::Drag)
            .into_any_element(),
        Frame::Client { .. } => {
            // Armed by a press in the area, so a drag that starts
            // elsewhere and crosses it moves nothing. A cell, as changing
            // it needs no redraw. The move is heard window-wide: the first
            // step of a quick drag may already be out of the bar.
            let armed = window
                .use_keyed_state(id.clone(), cx, |_, _| Rc::new(Cell::new(false)))
                .read(cx)
                .clone();
            let pressed = Rc::clone(&armed);
            let listener = canvas(
                |_, _, _| {},
                move |_, (), window, _| {
                    let moved = Rc::clone(&armed);
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
                        if phase == DispatchPhase::Bubble
                            && event.dragging()
                            && moved.replace(false)
                        {
                            window.start_window_move();
                        }
                    });
                    // Let go anywhere, even off the bar, disarms it.
                    let released = Rc::clone(&armed);
                    window.on_mouse_event(move |_: &MouseUpEvent, _, _, _| released.set(false));
                },
            )
            .absolute()
            .size_0();
            area.id(id)
                .child(listener)
                .on_mouse_down(MouseButton::Left, move |_, _, _| pressed.set(true))
                .on_click(|event, window, _| {
                    if event.click_count() == 2 {
                        window.zoom_window();
                    }
                })
                .on_mouse_down(MouseButton::Right, |event, window, cx| {
                    if window.window_controls().window_menu {
                        window.show_window_menu(event.position);
                        cx.stop_propagation();
                    }
                })
                .into_any_element()
        }
    }
}

/// Keeps a control in a top bar that drags its window a control: off
/// macOS, the drag area behind it must not take its presses, so it
/// stands in front of it for the pointer. On macOS it is left as it is.
pub fn over_drag_area<E: InteractiveElement>(element: E) -> E {
    if cfg!(target_os = "macos") {
        element
    } else {
        element.occlude()
    }
}

/// A hairline round the edge of `window`, and edges to resize it from if
/// it is `sizable`, over everything else in it, where the app frames the
/// window and it is not maximised or tiled; else nothing. `tone` is what
/// the edges are drawn over.
pub fn window_edges(window: &Window, tone: Tone, sizable: bool) -> Option<AnyElement> {
    let Frame::Client { resizable: true } = frame(window) else {
        return None;
    };
    let hairline = match tone {
        Tone::Paper => rgba(with_alpha(LINE, 0xFF)),
        Tone::Stage => rgba(with_alpha(0x00FF_FFFF, 0x14)),
    };
    let edge = |id: &'static str, edge: ResizeEdge, cursor: CursorStyle| {
        div()
            .id(id)
            .absolute()
            .occlude()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            })
    };
    let side = px(RESIZE_EDGE);
    let corner = px(RESIZE_CORNER);
    let frame = div().absolute().inset_0().border_1().border_color(hairline);
    if !sizable {
        return Some(frame.into_any_element());
    }
    Some(
        frame
            .child(
                edge("resize-top", ResizeEdge::Top, CursorStyle::ResizeUpDown)
                    .top_0()
                    .left(corner)
                    .right(corner)
                    .h(side),
            )
            .child(
                edge(
                    "resize-bottom",
                    ResizeEdge::Bottom,
                    CursorStyle::ResizeUpDown,
                )
                .bottom_0()
                .left(corner)
                .right(corner)
                .h(side),
            )
            .child(
                edge(
                    "resize-left",
                    ResizeEdge::Left,
                    CursorStyle::ResizeLeftRight,
                )
                .left_0()
                .top(corner)
                .bottom(corner)
                .w(side),
            )
            .child(
                edge(
                    "resize-right",
                    ResizeEdge::Right,
                    CursorStyle::ResizeLeftRight,
                )
                .right_0()
                .top(corner)
                .bottom(corner)
                .w(side),
            )
            .child(
                edge(
                    "resize-top-left",
                    ResizeEdge::TopLeft,
                    CursorStyle::ResizeUpLeftDownRight,
                )
                .top_0()
                .left_0()
                .size(corner),
            )
            .child(
                edge(
                    "resize-top-right",
                    ResizeEdge::TopRight,
                    CursorStyle::ResizeUpRightDownLeft,
                )
                .top_0()
                .right_0()
                .size(corner),
            )
            .child(
                edge(
                    "resize-bottom-left",
                    ResizeEdge::BottomLeft,
                    CursorStyle::ResizeUpRightDownLeft,
                )
                .bottom_0()
                .left_0()
                .size(corner),
            )
            .child(
                edge(
                    "resize-bottom-right",
                    ResizeEdge::BottomRight,
                    CursorStyle::ResizeUpLeftDownRight,
                )
                .bottom_0()
                .right_0()
                .size(corner),
            )
            .into_any_element(),
    )
}

/// A top bar for a window that has none of its own, where the app frames
/// it: `title` centred, the window's buttons, and the rest to drag it by.
/// Elsewhere the system's title bar does instead, and this is nothing.
pub fn title_bar(title: &'static str, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if !matches!(frame(window), Frame::Client { .. }) {
        return None;
    }
    let bar = div()
        .h(px(TOP_BAR))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(crate::theme::MUTED))
        .child(title);
    Some(header(bar, true, window, cx))
}

/// `bar`, the top bar of a window opened with the app's own
/// ([`crate::app::top_bar`]), with the window's buttons at its right where
/// the app draws them (only close if the window is not `sizable`), and the
/// rest of it to drag the window by. On macOS, `bar` as it is.
pub fn header(bar: Div, sizable: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    if frame(window) == Frame::System {
        return bar.into_any_element();
    }
    let buttons = buttons_for(window, Tone::Paper, sizable)
        .map(|buttons| div().absolute().top_0().right_0().child(buttons));
    drag_area(bar.relative().children(buttons), "header", window, cx)
}

/// Whether a Wayland compositor frames windows.
#[cfg(target_os = "linux")]
mod wayland {
    use std::sync::OnceLock;

    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, QueueHandle};

    /// The interfaces the compositor offers.
    #[derive(Default)]
    struct Globals(Vec<String>);

    impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
        fn event(
            globals: &mut Self,
            _: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { interface, .. } = event {
                globals.0.push(interface);
            }
        }
    }

    /// Whether GPUI will open its windows on a Wayland compositor that
    /// offers no server-side decorations (`zxdg_decoration_manager_v1`),
    /// so draws no frames. Asked once, over a connection of its own.
    pub fn without_frames() -> bool {
        static WITHOUT: OnceLock<bool> = OnceLock::new();
        *WITHOUT.get_or_init(|| {
            // GPUI picks Wayland whenever this is set and not empty.
            if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|display| display.is_empty()) {
                return false;
            }
            let Ok(connection) = Connection::connect_to_env() else {
                return false;
            };
            let mut queue = connection.new_event_queue();
            let _registry = connection.display().get_registry(&queue.handle(), ());
            let mut globals = Globals::default();
            if queue.roundtrip(&mut globals).is_err() {
                return false;
            }
            offers_no_frames(&globals.0)
        })
    }

    /// Whether a compositor offering `interfaces` leaves frames to its
    /// clients.
    pub(super) fn offers_no_frames(interfaces: &[String]) -> bool {
        !interfaces
            .iter()
            .any(|interface| interface == "zxdg_decoration_manager_v1")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_a_compositor_without_the_decoration_manager_leaves_frames_to_the_app() {
            let gnome = ["wl_compositor", "xdg_wm_base", "wl_seat"].map(String::from);
            assert!(offers_no_frames(&gnome));
            let kde =
                ["wl_compositor", "xdg_wm_base", "zxdg_decoration_manager_v1"].map(String::from);
            assert!(!offers_no_frames(&kde));
        }
    }
}
