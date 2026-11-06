//! The refresh rate of the monitor a window is on, under X11, from RandR.
//!
//! GPUI's X11 display is the whole screen, whatever monitors make it up,
//! and it won't hand out its window's ID (its `window_handle` panics). So
//! this asks the X server on a connection of its own: it finds the app's
//! window among the top-level windows by its process ID and size, takes
//! its place on the screen, and picks the RandR monitor (CRTC) holding the
//! most of it. The mode that monitor shows gives its rate: its pixel clock
//! over the pixels in a frame, blanking included.
//!
//! GPUI itself draws X11 windows on a timer at the rate of the first
//! monitor RandR lists, whichever monitor the window is on, so with two
//! monitors at different rates the window still draws at the first one's.

use std::sync::OnceLock;

use x11rb::connection::Connection as _;
use x11rb::protocol::randr::{ConnectionExt as _, ModeFlag, ModeInfo};
use x11rb::protocol::xproto::{self, AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

/// How far a window's size may be from GPUI's, in pixels, for it to be
/// taken as the app's window: GPUI rounds its size to its own pixels.
const SIZE_SLACK: u32 = 2;

/// A rectangle on the X screen, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// The left edge.
    pub x: i32,
    /// The top edge.
    pub y: i32,
    /// How wide it is.
    pub width: u32,
    /// How tall it is.
    pub height: u32,
}

impl Rect {
    /// How many pixels this and `other` share.
    fn overlap(self, other: Self) -> u64 {
        let span = |start: i32, length: u32, other_start: i32, other_length: u32| {
            let end = i64::from(start) + i64::from(length);
            let other_end = i64::from(other_start) + i64::from(other_length);
            let shared = end.min(other_end) - i64::from(start).max(i64::from(other_start));
            u64::try_from(shared).unwrap_or(0)
        };
        span(self.x, self.width, other.x, other.width)
            * span(self.y, self.height, other.y, other.height)
    }
}

/// A monitor showing part of the screen.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Monitor {
    area: Rect,
    /// Refreshes a second, if its mode says.
    hz: Option<f64>,
}

/// The app's own connection to the X server.
struct Server {
    connection: RustConnection,
    root: xproto::Window,
    client_list: xproto::Atom,
    wm_pid: xproto::Atom,
    active_window: xproto::Atom,
}

/// The connection, made the first time it is needed and kept.
fn server() -> Option<&'static Server> {
    static SERVER: OnceLock<Option<Server>> = OnceLock::new();
    SERVER
        .get_or_init(|| {
            let (connection, screen) = x11rb::connect(None).ok()?;
            let root = connection.setup().roots.get(screen)?.root;
            let atom =
                |name: &[u8]| Some(connection.intern_atom(false, name).ok()?.reply().ok()?.atom);
            Some(Server {
                client_list: atom(b"_NET_CLIENT_LIST")?,
                wm_pid: atom(b"_NET_WM_PID")?,
                active_window: atom(b"_NET_ACTIVE_WINDOW")?,
                connection,
                root,
            })
        })
        .as_ref()
}

/// How many times a second the monitor refreshes that holds the most of
/// the app's window, which GPUI has `gpui_bounds` big, at that place on
/// the screen as far as it knows. A window manager moves windows into
/// frames of its own, so the place is only trusted when the window can't
/// be found.
pub fn refresh_rate(gpui_bounds: Rect) -> Option<f64> {
    let server = server()?;
    let area = server
        .find_window(gpui_bounds.width, gpui_bounds.height)
        .and_then(|window| server.area(window))
        .unwrap_or(gpui_bounds);
    choose(&server.monitors()?, area)
}

/// The rate of the monitor holding the most of `area`, or of the first one
/// if none holds any of it, as GPUI's timer does.
fn choose(monitors: &[Monitor], area: Rect) -> Option<f64> {
    let mut best: Option<(u64, &Monitor)> = None;
    for monitor in monitors {
        let shared = monitor.area.overlap(area);
        if best.is_none_or(|(most, _)| shared > most) {
            best = Some((shared, monitor));
        }
    }
    best?.1.hz
}

/// How many times a second `mode` refreshes.
fn mode_rate(mode: &ModeInfo) -> Option<f64> {
    let mut lines = f64::from(mode.vtotal);
    if mode.mode_flags.contains(ModeFlag::DOUBLE_SCAN) {
        lines *= 2.0;
    }
    if mode.mode_flags.contains(ModeFlag::INTERLACE) {
        lines /= 2.0;
    }
    let pixels = f64::from(mode.htotal) * lines;
    (mode.dot_clock > 0 && pixels > 0.0).then(|| f64::from(mode.dot_clock) / pixels)
}

impl Server {
    /// The app's top-level window `width` by `height` pixels: its own if
    /// it has only one, else the active one of them, else the first.
    fn find_window(&self, width: u32, height: u32) -> Option<xproto::Window> {
        let connection = &self.connection;
        // The window manager's list of top-level windows; without one, the
        // app's windows are the root's own children.
        let listed = self
            .windows(self.root, self.client_list)
            .filter(|list| !list.is_empty());
        let candidates = match listed {
            Some(list) => list,
            None => {
                connection
                    .query_tree(self.root)
                    .ok()?
                    .reply()
                    .ok()?
                    .children
            }
        };
        let pid = std::process::id();
        let asked: Vec<_> = candidates
            .into_iter()
            .map(|window| {
                let cookie =
                    connection.get_property(false, window, self.wm_pid, AtomEnum::CARDINAL, 0, 1);
                (window, cookie)
            })
            .collect();
        let ours: Vec<_> = asked
            .into_iter()
            .filter_map(|(window, cookie)| {
                let reply = cookie.ok()?.reply().ok()?;
                (reply.value32()?.next()? == pid).then_some(window)
            })
            .collect();
        let sized: Vec<_> = ours
            .iter()
            .copied()
            .filter(|window| {
                connection
                    .get_geometry(*window)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .is_some_and(|geometry| {
                        u32::from(geometry.width).abs_diff(width) <= SIZE_SLACK
                            && u32::from(geometry.height).abs_diff(height) <= SIZE_SLACK
                    })
            })
            .collect();
        let active = self
            .windows(self.root, self.active_window)
            .and_then(|list| list.first().copied());
        match sized.as_slice() {
            [] if ours.len() == 1 => ours.first().copied(),
            [] => None,
            [only] => Some(*only),
            several => active
                .filter(|active| several.contains(active))
                .or_else(|| several.first().copied()),
        }
    }

    /// The windows a property of `window` lists.
    fn windows(&self, window: xproto::Window, property: xproto::Atom) -> Option<Vec<u32>> {
        let reply = self
            .connection
            .get_property(false, window, property, AtomEnum::WINDOW, 0, u32::MAX / 4)
            .ok()?
            .reply()
            .ok()?;
        Some(reply.value32()?.collect())
    }

    /// Where `window` is on the screen.
    fn area(&self, window: xproto::Window) -> Option<Rect> {
        let connection = &self.connection;
        let geometry = connection.get_geometry(window).ok()?;
        let place = connection
            .translate_coordinates(window, self.root, 0, 0)
            .ok()?;
        let (geometry, place) = (geometry.reply().ok()?, place.reply().ok()?);
        Some(Rect {
            x: place.dst_x.into(),
            y: place.dst_y.into(),
            width: geometry.width.into(),
            height: geometry.height.into(),
        })
    }

    /// The monitors showing the screen, from RandR.
    fn monitors(&self) -> Option<Vec<Monitor>> {
        let connection = &self.connection;
        let resources = connection
            .randr_get_screen_resources_current(self.root)
            .ok()?
            .reply()
            .ok()?;
        let asked: Vec<_> = resources
            .crtcs
            .iter()
            .filter_map(|crtc| {
                connection
                    .randr_get_crtc_info(*crtc, resources.config_timestamp)
                    .ok()
            })
            .collect();
        let monitors = asked
            .into_iter()
            .filter_map(|cookie| {
                let crtc = cookie.reply().ok()?;
                // A CRTC that shows nothing has no mode.
                if crtc.mode == 0 || crtc.width == 0 || crtc.height == 0 {
                    return None;
                }
                let mode = resources.modes.iter().find(|mode| mode.id == crtc.mode);
                Some(Monitor {
                    area: Rect {
                        x: crtc.x.into(),
                        y: crtc.y.into(),
                        width: crtc.width.into(),
                        height: crtc.height.into(),
                    },
                    hz: mode.and_then(mode_rate),
                })
            })
            .collect();
        Some(monitors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn mode(dot_clock: u32, htotal: u16, vtotal: u16, flags: ModeFlag) -> ModeInfo {
        ModeInfo {
            id: 1,
            width: 1920,
            height: 1080,
            dot_clock,
            hsync_start: 0,
            hsync_end: 0,
            htotal,
            hskew: 0,
            vsync_start: 0,
            vsync_end: 0,
            vtotal,
            name_len: 0,
            mode_flags: flags,
        }
    }

    #[test]
    fn a_mode_refreshes_at_its_clock_over_its_pixels() {
        // CVT 1920x1080 at 60 Hz and 144 Hz, with reduced blanking.
        let sixty = mode_rate(&mode(138_500_000, 2080, 1111, ModeFlag::default()));
        assert!((sixty.expect("60 Hz") - 59.934).abs() < 0.001, "{sixty:?}");
        let fast = mode_rate(&mode(325_080_000, 2080, 1085, ModeFlag::default()));
        assert!((fast.expect("144 Hz") - 144.04).abs() < 0.01, "{fast:?}");
        let interlaced = mode_rate(&mode(74_250_000, 2200, 1125, ModeFlag::INTERLACE));
        assert!((interlaced.expect("1080i") - 60.0).abs() < 0.001);
        let doubled = mode_rate(&mode(25_175_000, 800, 525, ModeFlag::DOUBLE_SCAN));
        assert!((doubled.expect("double scan") - 29.97).abs() < 0.01);
        // Xvfb's own modes have no clock.
        assert_eq!(mode_rate(&mode(0, 0, 0, ModeFlag::default())), None);
    }

    #[test]
    fn the_monitor_holding_most_of_the_window_counts() {
        let monitors = [
            Monitor {
                area: rect(0, 0, 2560, 1440),
                hz: Some(60.0),
            },
            Monitor {
                area: rect(2560, 0, 1920, 1080),
                hz: Some(144.0),
            },
        ];
        assert_eq!(choose(&monitors, rect(100, 100, 800, 600)), Some(60.0));
        assert_eq!(choose(&monitors, rect(2900, 100, 800, 600)), Some(144.0));
        // Mostly on the second, across the edge.
        assert_eq!(choose(&monitors, rect(2400, 100, 800, 600)), Some(144.0));
        // Off every monitor: the first, as GPUI's own timer.
        assert_eq!(choose(&monitors, rect(-5000, 0, 800, 600)), Some(60.0));
        assert_eq!(choose(&[], rect(0, 0, 1, 1)), None);
    }

    #[test]
    fn rectangles_share_what_they_overlap() {
        let screen = rect(0, 0, 100, 50);
        assert_eq!(screen.overlap(rect(90, 40, 20, 20)), 100);
        assert_eq!(screen.overlap(rect(100, 0, 10, 10)), 0, "touching");
        assert_eq!(screen.overlap(rect(-10, -10, 5, 5)), 0);
        assert_eq!(screen.overlap(screen), 5000);
    }
}
