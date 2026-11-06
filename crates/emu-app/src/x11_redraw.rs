//! A workaround for GPUI 0.2.2 on X11, where a window sometimes draws its
//! first frame and never shows another: its redraw timer starts only when
//! the window's `MapNotify` event is handled (`x11/client.rs`,
//! `update_refresh_loop`), and a drawn frame is shown only after an
//! `Expose`. Both are sometimes missed: every time with no window manager,
//! and about one launch in eight under Openbox. The window then stays
//! black until it is unmapped and mapped again.
//!
//! The events are not lost but stranded: GPUI waits for replies while it
//! sets a window up, and events that arrive meanwhile are read into
//! x11rb's queue, where nothing wakes the event loop for them.
//!
//! [`ensure_redraws`] watches, on a connection of its own, for the app's
//! new window (one with the app's `_NET_WM_PID`, which GPUI always sets;
//! GPUI 0.2.2 gives no X11 window handle) to become viewable, then sends it
//! a `MapNotify` and an `Expose` of its own, a few times over its first
//! seconds, as one sent while GPUI is still busy would be stranded too.
//! GPUI handles them as the server's: the `MapNotify` starts the timer if
//! it had not started, and else does nothing; the `Expose` has the next
//! frame shown. Nothing is mapped, moved or drawn, so a window manager
//! sees nothing. On Wayland, macOS and Windows this does nothing.

/// Makes sure GPUI goes on redrawing the window the app is opening, on
/// X11. Call it as the window opens; it waits for the window off the main
/// thread.
pub fn ensure_redraws() {
    #[cfg(target_os = "linux")]
    if x11::in_use() {
        std::thread::Builder::new()
            .name("x11-map".to_owned())
            .spawn(|| {
                if let Err(error) = x11::announce_new_windows() {
                    eprintln!("ds-emulator: could not check the X11 window redraws: {error}");
                }
            })
            .ok();
    }
}

#[cfg(target_os = "linux")]
mod x11 {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, EXPOSE_EVENT, EventMask, ExposeEvent, MAP_NOTIFY_EVENT,
        MapNotifyEvent, MapState, Window,
    };

    /// How long to wait for a window to show, and how often to look.
    const PATIENCE: Duration = Duration::from_secs(20);
    const POLL: Duration = Duration::from_millis(50);
    /// The waits before each time a window is told it was mapped and
    /// exposed, once it shows: soon, and again in case GPUI was still busy.
    const NUDGES: [Duration; 5] = [
        Duration::from_millis(250),
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_millis(1000),
        Duration::from_millis(2000),
    ];

    /// The windows already told.
    static ANNOUNCED: Mutex<Option<HashSet<Window>>> = Mutex::new(None);

    /// Whether GPUI draws on X11: when `WAYLAND_DISPLAY` names no display
    /// and `DISPLAY` does, as GPUI chooses.
    pub fn in_use() -> bool {
        let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        !set("WAYLAND_DISPLAY") && set("DISPLAY")
    }

    /// Waits until one of the app's windows not yet told is viewable, then
    /// tells each such window that it was mapped and exposed.
    pub fn announce_new_windows() -> Result<(), String> {
        let (connection, screen) = x11rb::connect(None).map_err(|error| error.to_string())?;
        let root = connection.setup().roots[screen].root;
        let pid_atom = connection
            .intern_atom(false, b"_NET_WM_PID")
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())?
            .atom;
        let started = Instant::now();
        while started.elapsed() < PATIENCE {
            let mut ours = Vec::new();
            find_ours(&connection, root, pid_atom, std::process::id(), &mut ours);
            let new: Vec<Window> = {
                let mut announced = ANNOUNCED.lock().map_err(|error| error.to_string())?;
                let announced = announced.get_or_insert_with(HashSet::new);
                ours.into_iter()
                    .filter(|window| announced.insert(*window))
                    .collect()
            };
            if !new.is_empty() {
                for wait in NUDGES {
                    std::thread::sleep(wait);
                    for &window in &new {
                        nudge(&connection, window)?;
                    }
                    connection.flush().map_err(|error| error.to_string())?;
                }
                return Ok(());
            }
            std::thread::sleep(POLL);
        }
        Ok(())
    }

    /// Tells the clients listening to `window`, GPUI among them, that it was
    /// mapped and that all of it was exposed. Nothing if it is gone.
    fn nudge(connection: &impl Connection, window: Window) -> Result<(), String> {
        let Ok(Ok(geometry)) = connection.get_geometry(window).map(|cookie| cookie.reply()) else {
            return Ok(());
        };
        let mapped = MapNotifyEvent {
            response_type: MAP_NOTIFY_EVENT,
            sequence: 0,
            event: window,
            window,
            override_redirect: false,
        };
        let exposed = ExposeEvent {
            response_type: EXPOSE_EVENT,
            sequence: 0,
            window,
            x: 0,
            y: 0,
            width: geometry.width,
            height: geometry.height,
            count: 0,
        };
        connection
            .send_event(false, window, EventMask::STRUCTURE_NOTIFY, mapped)
            .map_err(|error| error.to_string())?;
        connection
            .send_event(false, window, EventMask::EXPOSURE, exposed)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Adds to `ours` the viewable windows under `parent` whose
    /// `_NET_WM_PID` is `pid`. A window manager's frame lies between the
    /// root and the window it frames, so the search goes down the tree. A
    /// window gone since its parent was read is passed over.
    fn find_ours(
        connection: &impl Connection,
        parent: Window,
        pid_atom: u32,
        pid: u32,
        ours: &mut Vec<Window>,
    ) {
        let Ok(Ok(tree)) = connection.query_tree(parent).map(|cookie| cookie.reply()) else {
            return;
        };
        for window in tree.children {
            let Ok(Ok(attributes)) = connection
                .get_window_attributes(window)
                .map(|cookie| cookie.reply())
            else {
                continue;
            };
            if attributes.map_state != MapState::VIEWABLE {
                continue;
            }
            let owner = connection
                .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .and_then(|reply| reply.value32().and_then(|mut values| values.next()));
            if owner == Some(pid) {
                ours.push(window);
            } else {
                find_ours(connection, window, pid_atom, pid, ours);
            }
        }
    }
}
