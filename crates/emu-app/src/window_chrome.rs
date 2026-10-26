//! What GPUI leaves out of a macOS window's chrome, done through AppKit:
//! showing or hiding the close, minimise and zoom buttons, the standard
//! About panel, the Dock icon, and how often the window's display
//! refreshes. Elsewhere these do nothing.

use gpui::Window;

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

/// The interval between refreshes of the display `window` is mostly on:
/// 8.3 ms on a 120 Hz ProMotion display.
#[cfg(target_os = "macos")]
pub fn refresh_interval(window: &Window) -> Option<std::time::Duration> {
    let fps = mac::native_window(window)?
        .screen()?
        .maximumFramesPerSecond();
    let fps = u32::try_from(fps).ok().filter(|fps| *fps > 0)?;
    Some(std::time::Duration::from_secs(1) / fps)
}

/// The interval between refreshes of the display `window` is mostly on.
#[cfg(not(target_os = "macos"))]
pub fn refresh_interval(_window: &Window) -> Option<std::time::Duration> {
    None
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
