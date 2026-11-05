//! The app's icons: Solar (Linear) by 480 Design, CC BY 4.0, from
//! `assets/icons`. Each is drawn on a 24-unit grid with 1.5-unit strokes,
//! so icons of one size share one weight. GPUI loads them through
//! [`Icons`], the app's asset source.

use std::borrow::Cow;

use gpui::{
    AssetSource, Div, ElementId, InteractiveElement, ParentElement, SharedString, Stateful, Styled,
    Svg, div, px, rgb, rgba, svg,
};

use crate::theme::{HOVER, TEXT};

/// Adding games.
pub const ADD: &str = "icons/add.svg";
/// Back to the page before.
pub const BACK: &str = "icons/alt-arrow-left.svg";
/// A wand: cheats.
pub const CHEATS: &str = "icons/magic-stick.svg";
/// A folder: where a game's file is.
pub const FOLDER: &str = "icons/folder.svg";
/// A ticked circle: a game the library has.
pub const CHECK: &str = "icons/check-circle.svg";
/// An arrow into a tray: getting a game.
pub const DOWNLOAD: &str = "icons/download.svg";
/// A globe: a game's website.
pub const GLOBE: &str = "icons/global.svg";
/// A grid of four: the library.
pub const LIBRARY: &str = "icons/widget.svg";
pub const MAGNIFIER: &str = "icons/magnifer.svg";
pub const PLAY: &str = "icons/play.svg";
/// Two arrows chasing round: fetching anew.
pub const REFRESH: &str = "icons/refresh.svg";
/// An arrow coming round: starting over.
pub const RESTART: &str = "icons/restart.svg";
pub const SETTINGS: &str = "icons/settings.svg";
/// Two arrows crossing: randomizing a game.
pub const SHUFFLE: &str = "icons/shuffle.svg";
/// A shopfront: the store.
pub const STORE: &str = "icons/shop.svg";
/// A bin: taking a game out of the library.
pub const TRASH: &str = "icons/trash-bin-minimalistic.svg";
/// A chevron pointing right: something that opens.
pub const CHEVRON_RIGHT: &str = "icons/alt-arrow-right.svg";
/// A chevron pointing down: something open.
pub const CHEVRON_DOWN: &str = "icons/alt-arrow-down.svg";
/// An arrow up: sending a question.
pub const ARROW_UP: &str = "icons/arrow-up.svg";
/// An arrow down: back to the newest.
pub const ARROW_DOWN: &str = "icons/arrow-down.svg";
/// A camera: a screenshot of the game.
pub const CAMERA: &str = "icons/camera.svg";
/// Two sheets: copying.
pub const COPY: &str = "icons/copy.svg";
/// A thumb up: a good answer.
pub const LIKE: &str = "icons/like.svg";
/// A thumb down: a bad answer.
pub const DISLIKE: &str = "icons/dislike.svg";
/// A pen on a square: a new chat.
pub const NEW_CHAT: &str = "icons/pen-new-square.svg";
/// A clock going back: earlier chats.
pub const HISTORY: &str = "icons/history.svg";
/// A page of text: a page read.
pub const PAGE: &str = "icons/document-text.svg";
/// A picture with a plus: adding a picture from a file.
pub const ADD_PICTURE: &str = "icons/gallery-add.svg";

const FILES: [(&str, &[u8]); 28] = [
    (ADD, include_bytes!("../../../assets/icons/add.svg")),
    (
        BACK,
        include_bytes!("../../../assets/icons/alt-arrow-left.svg"),
    ),
    (
        CHEATS,
        include_bytes!("../../../assets/icons/magic-stick.svg"),
    ),
    (
        CHECK,
        include_bytes!("../../../assets/icons/check-circle.svg"),
    ),
    (
        DOWNLOAD,
        include_bytes!("../../../assets/icons/download.svg"),
    ),
    (FOLDER, include_bytes!("../../../assets/icons/folder.svg")),
    (GLOBE, include_bytes!("../../../assets/icons/global.svg")),
    (LIBRARY, include_bytes!("../../../assets/icons/widget.svg")),
    (
        MAGNIFIER,
        include_bytes!("../../../assets/icons/magnifer.svg"),
    ),
    (PLAY, include_bytes!("../../../assets/icons/play.svg")),
    (REFRESH, include_bytes!("../../../assets/icons/refresh.svg")),
    (RESTART, include_bytes!("../../../assets/icons/restart.svg")),
    (
        SETTINGS,
        include_bytes!("../../../assets/icons/settings.svg"),
    ),
    (SHUFFLE, include_bytes!("../../../assets/icons/shuffle.svg")),
    (STORE, include_bytes!("../../../assets/icons/shop.svg")),
    (
        TRASH,
        include_bytes!("../../../assets/icons/trash-bin-minimalistic.svg"),
    ),
    (
        CHEVRON_RIGHT,
        include_bytes!("../../../assets/icons/alt-arrow-right.svg"),
    ),
    (
        CHEVRON_DOWN,
        include_bytes!("../../../assets/icons/alt-arrow-down.svg"),
    ),
    (
        ARROW_UP,
        include_bytes!("../../../assets/icons/arrow-up.svg"),
    ),
    (
        ARROW_DOWN,
        include_bytes!("../../../assets/icons/arrow-down.svg"),
    ),
    (CAMERA, include_bytes!("../../../assets/icons/camera.svg")),
    (COPY, include_bytes!("../../../assets/icons/copy.svg")),
    (LIKE, include_bytes!("../../../assets/icons/like.svg")),
    (DISLIKE, include_bytes!("../../../assets/icons/dislike.svg")),
    (
        NEW_CHAT,
        include_bytes!("../../../assets/icons/pen-new-square.svg"),
    ),
    (HISTORY, include_bytes!("../../../assets/icons/history.svg")),
    (
        PAGE,
        include_bytes!("../../../assets/icons/document-text.svg"),
    ),
    (
        ADD_PICTURE,
        include_bytes!("../../../assets/icons/gallery-add.svg"),
    ),
];

/// The icons, as GPUI's asset source.
pub struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let file = FILES.iter().find(|(name, _)| *name == path);
        Ok(file.map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let names = FILES.iter().map(|(name, _)| *name);
        Ok(names
            .filter(|name| name.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

/// Side of an icon button, and of the icon on it, in logical pixels. The
/// icon sits 6 in, and its artwork 1 to 2 inside that.
pub const BUTTON: f32 = 28.0;
pub const SIZE: f32 = 16.0;

/// A button showing the icon at `path`, washed under the pointer. The
/// caller says what a click does.
pub fn button(id: impl Into<ElementId>, path: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(BUTTON))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(|button| button.bg(rgba(HOVER)))
        .child(icon(path, SIZE, TEXT))
}

/// The icon at `path`, `size` logical pixels square, in `colour`.
pub fn icon(path: &'static str, size: f32, colour: u32) -> Svg {
    svg()
        .path(path)
        .size(px(size))
        .flex_none()
        .text_color(rgb(colour))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads_and_draws_in_the_text_colour() {
        for (name, _) in FILES {
            let bytes = Icons.load(name).expect("loads").expect("is there");
            let svg = std::str::from_utf8(&bytes).expect("text");
            assert!(
                svg.contains("viewBox=\"0 0 24 24\""),
                "{name} is on the grid"
            );
            assert!(svg.contains("currentColor"), "{name} takes the colour");
        }
        assert_eq!(Icons.list("icons/").expect("lists").len(), FILES.len());
    }
}
