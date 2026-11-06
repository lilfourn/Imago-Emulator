//! About Imago Emulator, off macOS, where AppKit's standard About panel
//! isn't: the app's icon, name and version, its copyright and site, and
//! its open-source notices when the package ships them beside the app.
//! Escape or Close Game's shortcut closes it.

use std::path::PathBuf;
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
/// The open-source notices, as `scripts/macos/release.sh` writes them for
/// macOS: packages for Linux and Windows put them beside the app.
const NOTICES: &str = "Acknowledgements.html";

/// The About window's root view.
pub struct AboutWindow {
    focus: FocusHandle,
    icon: Arc<Image>,
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
                notices: notices(),
            }
        })
    });
    if let Err(error) = opened {
        eprintln!("ds-emulator: cannot open the About window: {error:#}");
    }
}

/// The open-source notices beside the app's executable, if the package
/// put them there.
fn notices() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let notices = executable.parent()?.join(NOTICES);
    notices.is_file().then_some(notices)
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
        let notices = self.notices.clone().map(|path| {
            link("notices", "Open-Source Notices")
                .on_click(move |_, _, cx| cx.open_url(&format!("file://{}", path.display())))
                .children(probe::part("link:notices"))
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
                    .children(notices)
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
    fn the_version_is_the_app_s() {
        assert_eq!(version(), format!("Version {}", env!("CARGO_PKG_VERSION")));
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
