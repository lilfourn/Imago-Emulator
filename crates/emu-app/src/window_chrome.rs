//! What GPUI leaves out of a window's chrome. On macOS, through AppKit:
//! showing or hiding the close, minimise and zoom buttons, the standard
//! About panel and the Dock icon, which do nothing elsewhere. And on every
//! OS, how often the display the window is on refreshes: from AppKit on
//! macOS, the monitor's display mode on Windows (`win32.rs`), RandR on
//! X11 (`x11.rs`) and the output's mode on Wayland (`wayland.rs`).

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(windows)]
mod win32;
#[cfg(target_os = "linux")]
mod x11;

use gpui::{App, Bounds, DisplayId, Pixels, Window};

/// Shows or hides `window`'s close, minimise and zoom buttons.
#[cfg(target_os = "macos")]
pub fn set_controls_visible(window: &Window, visible: bool) {
    use objc2_app_kit::NSWindowButton;

    let Some(native) = mac::native_window(window) else {
        return;
    };
    for kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = native.standardWindowButton(kind) {
            button.setHidden(!visible);
        }
    }
}

/// Shows or hides `window`'s close, minimise and zoom buttons.
#[cfg(not(target_os = "macos"))]
pub fn set_controls_visible(_window: &Window, _visible: bool) {}

/// Opens the app's standard About panel: its name, version and icon.
#[cfg(target_os = "macos")]
pub fn show_about_panel() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    if let Some(main_thread) = MainThreadMarker::new() {
        NSApplication::sharedApplication(main_thread).orderFrontStandardAboutPanel(None);
    }
}

/// Opens the app's standard About panel: its name, version and icon.
#[cfg(not(target_os = "macos"))]
pub fn show_about_panel() {}

/// Shows the image in `png`, a PNG file's bytes, as the app's Dock icon
/// while it runs. An app bundle's own icon takes over once there is one.
#[cfg(target_os = "macos")]
pub fn set_dock_icon(png: &[u8]) {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage, NSImageView};
    use objc2_foundation::NSData;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png)) else {
        return;
    };
    // The Dock tile draws a view; setting the icon image itself is not
    // possible without unsafe code.
    let view = NSImageView::imageViewWithImage(&image, main_thread);
    let tile = NSApplication::sharedApplication(main_thread).dockTile();
    tile.setContentView(Some(&view));
    tile.display();
}

/// Shows the image in `png` as the app's Dock icon while it runs.
#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon(_png: &[u8]) {}

/// Which display a window is on, as far as its refresh rate goes: while
/// this stays the same, so does [`refresh_interval`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayKey {
    /// The display GPUI says the window is on.
    display: Option<DisplayId>,
    /// Where the window is, under X11, where GPUI's one display spans
    /// every monitor; `None` elsewhere, where GPUI follows the window from
    /// one display to the next itself.
    bounds: Option<Bounds<Pixels>>,
}

/// Which display `window` is on, as far as its refresh rate goes.
pub fn display_key(window: &Window, cx: &App) -> DisplayKey {
    DisplayKey {
        display: window.display(cx).map(|display| display.id()),
        bounds: spans_monitors().then(|| window.bounds()),
    }
}

/// Whether GPUI's display is the whole screen, whatever monitors make it
/// up, as on X11.
#[cfg(target_os = "linux")]
fn spans_monitors() -> bool {
    gpui::guess_compositor() == "X11"
}

/// Whether GPUI's display is the whole screen, whatever monitors make it
/// up, as on X11.
#[cfg(not(target_os = "linux"))]
fn spans_monitors() -> bool {
    false
}

/// The interval between refreshes of the display `window` is mostly on:
/// 8.3 ms on a 120 Hz ProMotion display.
#[cfg(target_os = "macos")]
pub fn refresh_interval(window: &Window, _cx: &App) -> Option<std::time::Duration> {
    let fps = mac::native_window(window)?
        .screen()?
        .maximumFramesPerSecond();
    let fps = u32::try_from(fps).ok().filter(|fps| *fps > 0)?;
    Some(std::time::Duration::from_secs(1) / fps)
}

/// The interval between refreshes of the monitor `window` is mostly on:
/// 16.7 ms on a 60 Hz monitor.
#[cfg(windows)]
pub fn refresh_interval(window: &Window, _cx: &App) -> Option<std::time::Duration> {
    win32::refresh_rate(window).and_then(interval)
}

/// The interval between refreshes of the monitor `window` is mostly on,
/// under X11 or Wayland: 16.7 ms on a 60 Hz monitor.
#[cfg(target_os = "linux")]
pub fn refresh_interval(window: &Window, cx: &App) -> Option<std::time::Duration> {
    let rate = match gpui::guess_compositor() {
        "Wayland" => {
            wayland::refresh_rate(window.display(cx).and_then(|display| display.uuid().ok()))
        }
        "X11" => {
            // GPUI keeps the window's place in its own pixels.
            let bounds = window.bounds();
            let scale = window.scale_factor();
            let pixels = |value: Pixels| (f32::from(value) * scale).round();
            x11::refresh_rate(x11::Rect {
                x: pixels(bounds.origin.x) as i32,
                y: pixels(bounds.origin.y) as i32,
                width: pixels(bounds.size.width) as u32,
                height: pixels(bounds.size.height) as u32,
            })
        }
        _ => None,
    };
    rate.and_then(interval)
}

/// The interval between refreshes of the display `window` is on.
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub fn refresh_interval(_window: &Window, _cx: &App) -> Option<std::time::Duration> {
    None
}

/// The interval between refreshes `hz` times a second, if that is a rate a
/// display could have.
#[cfg(any(windows, target_os = "linux"))]
fn interval(hz: f64) -> Option<std::time::Duration> {
    (hz.is_finite() && (1.0..=1000.0).contains(&hz))
        .then(|| std::time::Duration::from_secs_f64(1.0 / hz))
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    use gpui::Window;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSWindow};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// The AppKit window `window` draws in. GPUI hands out only the view
    /// it draws into, so this finds the window holding that view, which
    /// needs no unsafe code.
    pub fn native_window(window: &Window) -> Option<Retained<NSWindow>> {
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        let view = handle.ns_view.as_ptr().cast_const();
        let main_thread = MainThreadMarker::new()?;
        NSApplication::sharedApplication(main_thread)
            .windows()
            .to_vec()
            .into_iter()
            .find(|candidate| {
                candidate.contentView().is_some_and(|content| {
                    content
                        .subviews()
                        .to_vec()
                        .iter()
                        .any(|subview| Retained::as_ptr(subview).cast::<c_void>() == view)
                })
            })
    }
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_rate_becomes_the_time_between_refreshes() {
        assert_eq!(interval(60.0), Some(Duration::from_secs_f64(1.0 / 60.0)));
        let ntsc = interval(60_000.0 / 1001.0).expect("59.94 Hz");
        assert_eq!(ntsc.as_micros(), 16_683);
        assert_eq!(interval(240.0).map(|step| step.as_micros()), Some(4_166));
        for nonsense in [0.0, -60.0, 0.5, 5000.0, f64::NAN, f64::INFINITY] {
            assert_eq!(interval(nonsense), None, "{nonsense}");
        }
    }
}
