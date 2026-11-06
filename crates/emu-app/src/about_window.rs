//! About Imago Emulator, off macOS, where AppKit's standard About panel
//! isn't: the app's icon, name and version, its copyright and site, and
//! its license and open-source notices where the package ships them.
//! Escape or Close Game's shortcut closes it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, Bounds, Context, FocusHandle, Image, ImageFormat, KeyDownEvent, Window, WindowBounds,
    WindowOptions, div, img, prelude::*, px, rgb, size,
};

use crate::app::{APP_ICON, CloseGame, top_bar};
use crate::chrome::{self, Tone};
use crate::probe;
use crate::theme::{ACCENT, BACKGROUND, MUTED, TEXT};

/// The window's size, in logical pixels.
const WIDTH: f32 = 360.0;
const HEIGHT: f32 = 340.0;
/// The icon's side.
const ICON: f32 = 96.0;
/// The app's site, as macOS's About panel gives it.
const SITE: &str = "https://imagoemulator.com";
/// The license and the open-source notices, which the Linux and Windows
/// packages ship as `scripts/macos/release.sh` does for macOS.
const LICENSE: &str = "License.txt";
const NOTICES: &str = "Acknowledgements.html";

/// The About window's root view.
pub struct AboutWindow {
    focus: FocusHandle,
    icon: Arc<Image>,
    license: Option<PathBuf>,
    notices: Option<PathBuf>,
}

/// Brings the About window forward, opening it if it isn't open.
pub fn show(cx: &mut App) {
    let open = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<AboutWindow>());
    if let Some(window) = open
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(top_bar("About Imago Emulator")),
        is_resizable: false,
        is_minimizable: false,
        app_id: Some("ds-emulator".to_owned()),
        window_decorations: chrome::window_decorations(),
        ..WindowOptions::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            let focus = cx.focus_handle();
            focus.focus(window);
            crate::x11_redraw::ensure_redraws();
            AboutWindow {
                focus,
                icon: Arc::new(Image::from_bytes(ImageFormat::Png, APP_ICON.to_vec())),
                license: shipped(LICENSE),
                notices: shipped(NOTICES),
            }
        })
    });
    if let Err(error) = opened {
        eprintln!("ds-emulator: cannot open the About window: {error:#}");
    }
}

/// The file `name` the package shipped with the running app, if it did.
fn shipped(name: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    shipped_beside(&executable, name)
}

/// Where the packages put the file `name` for the app at `executable`:
/// beside it on Windows (installer and zip), and in `share/imago-emulator`
/// beside its `bin` on Linux (the `.deb` and AppImage alike).
fn shipped_beside(executable: &Path, name: &str) -> Option<PathBuf> {
    let folder = executable.parent()?;
    let shared = folder.join("..").join("share").join("imago-emulator");
    let found = [folder.join(name), shared.join(name)]
        .into_iter()
        .find(|path| path.is_file())?;
    // Without the `..`, but not on Windows, where that gives a `\\?\` path.
    if cfg!(windows) {
        return Some(found);
    }
    Some(std::fs::canonicalize(&found).unwrap_or(found))
}

/// A `file:` URL for the file at `path`, to open in the system's viewer:
/// spaces and the like escaped, Windows's backslashes turned.
fn file_url(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from(if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                url.push(char::from(byte));
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// The app's version, as its crate gives it.
fn version() -> String {
    format!("Version {}", env!("CARGO_PKG_VERSION"))
}

impl AboutWindow {
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, _: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            window.remove_window();
        }
    }
}

impl gpui::Render for AboutWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = chrome::header(
            div().h(px(crate::app::TOP_BAR)).flex_none(),
            false,
            window,
            cx,
        );
        let link = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .text_color(rgb(ACCENT))
                .cursor_pointer()
                .hover(|link| link.underline())
                .child(label)
        };
        let license = self.license.clone().map(|path| {
            link("license", "License")
                .on_click(move |_, _, cx| cx.open_url(&file_url(&path)))
                .children(probe::part("link:license"))
        });
        let notices = self.notices.clone().map(|path| {
            link("notices", "Open-Source Notices")
                .on_click(move |_, _, cx| cx.open_url(&file_url(&path)))
                .children(probe::part("link:notices"))
        });
        let files = (license.is_some() || notices.is_some()).then(|| {
            div()
                .flex()
                .gap(px(14.0))
                .children(license)
                .children(notices)
        });
        div()
            .track_focus(&self.focus)
            .key_context("About")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .on_key_down(cx.listener(Self::key_down))
            // Close Game's shortcut closes this window, as it does the
            // randomizer's.
            .on_action(cx.listener(|_, _: &CloseGame, window, _| window.remove_window()))
            .children(probe::begin())
            .child(header)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.0))
                    .pb(px(28.0))
                    .child(
                        div()
                            .mb(px(12.0))
                            .child(img(Arc::clone(&self.icon)).size(px(ICON)))
                            .children(probe::part("icon")),
                    )
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Imago Emulator"),
                    )
                    .child(div().text_color(rgb(MUTED)).child(version()))
                    .child(
                        div()
                            .mt(px(14.0))
                            .text_color(rgb(MUTED))
                            .child("© 2026 Luke Fournier"),
                    )
                    .child(
                        link("site", "imagoemulator.com")
                            .on_click(|_, _, cx| cx.open_url(SITE))
                            .children(probe::part("link:site")),
                    )
                    .children(files)
                    .child(
                        div()
                            .mt(px(14.0))
                            .text_size(px(11.5))
                            .text_color(rgb(MUTED))
                            .child("Not affiliated with Nintendo."),
                    ),
            )
            .children(chrome::window_edges(window, Tone::Paper, false))
            .children(probe::end("about"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_license_and_notices_are_found_where_each_package_puts_them() {
        let root = std::env::temp_dir().join(format!("emu-app-about-{}", std::process::id()));
        let real = |path: PathBuf| std::fs::canonicalize(path).expect("there");
        // Windows: beside the executable.
        let windows = root.join("windows");
        std::fs::create_dir_all(&windows).expect("folder");
        std::fs::write(windows.join(LICENSE), "terms").expect("license");
        let exe = windows.join("ds-emulator.exe");
        let found = shipped_beside(&exe, LICENSE).expect("found");
        assert_eq!(real(found), real(windows.join(LICENSE)));
        assert_eq!(shipped_beside(&exe, NOTICES), None);
        // Linux: in share/imago-emulator beside bin.
        let share = root.join("usr/share/imago-emulator");
        std::fs::create_dir_all(&share).expect("folder");
        std::fs::create_dir_all(root.join("usr/bin")).expect("folder");
        std::fs::write(share.join(NOTICES), "<html>").expect("notices");
        let found = shipped_beside(&root.join("usr/bin/ds-emulator"), NOTICES).expect("found");
        assert_eq!(real(found), real(share.join(NOTICES)));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn files_open_by_url_with_their_names_escaped() {
        assert_eq!(
            file_url(Path::new("/usr/bin/../share/imago-emulator/License.txt")),
            "file:///usr/bin/../share/imago-emulator/License.txt"
        );
        assert_eq!(
            file_url(Path::new(
                r"C:\Program Files\Imago Emulator\Acknowledgements.html"
            )),
            "file:///C:/Program%20Files/Imago%20Emulator/Acknowledgements.html"
        );
    }

    #[test]
    fn the_version_is_the_app_s() {
        assert_eq!(version(), format!("Version {}", env!("CARGO_PKG_VERSION")));
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
