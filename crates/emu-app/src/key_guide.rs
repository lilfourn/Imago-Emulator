//! The key guide: a translucent island floating over the foot of the
//! window naming the keys that play the running game, read from the player's own bindings,
//! so it stays right when they rebind. Each key sits over the button it
//! presses, in small groups: the d-pad, the face buttons, the shoulders,
//! Start and Select, and the menu. It only shows; it takes no clicks.

use emu_core::{Button, System};
use gpui::{AnyElement, BoxShadow, div, hsla, point, prelude::*, px, rgb, rgba};

use crate::bindings::{Bindings, Held};
use crate::theme::{BACKGROUND, MUTED, TEXT, with_alpha};

/// Height of one line of keys, in logical pixels: a keycap over its label.
const LINE_HEIGHT: f32 = 36.0;
/// Room inside the island, above and below the lines and either side.
const PAD_Y: f32 = 7.0;
const PAD_X: f32 = 16.0;
/// Room between the island and the window's edges, and between it and
/// the screens above.
const INSET: f32 = 10.0;
const ABOVE: f32 = 6.0;
/// Space between groups, either side of their divider, and between the
/// keys of a group.
const GROUP_GAP: f32 = 12.0;
const KEY_GAP: f32 = 8.0;
/// A keycap's height, and its least width so single letters sit in
/// squares.
const CAP: f32 = 20.0;
/// Rough width of a character, for working out how many lines the groups
/// need.
const CHAR_WIDTH: f32 = 7.0;
/// Rounding of the island's corners.
const RADIUS: f32 = 16.0;

/// One key the guide names, over what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The key, as printed on the keyboard: "K", "W", "⏎".
    pub key: String,
    /// What it presses: "A", "↑", "Start".
    pub label: &'static str,
}

/// What the guide shows for a `system` game played with `bindings`, in
/// groups: the d-pad, the face buttons the system has, its shoulders,
/// Start and Select, and Escape for the menu. Unbound buttons are left
/// out, and so are empty groups.
pub fn groups(system: System, bindings: &Bindings) -> Vec<Vec<Entry>> {
    let entry = |button: Button, label: &'static str| {
        preferred(&bindings.keys_for(Held::Button(button))).map(|key| Entry {
            key: key_name(&key),
            label,
        })
    };
    let group = |buttons: &[(Button, &'static str)]| -> Vec<Entry> {
        buttons
            .iter()
            .filter_map(|&(button, label)| entry(button, label))
            .collect()
    };
    let mut face = vec![(Button::A, "A"), (Button::B, "B")];
    if system == System::Nds {
        face.extend([(Button::X, "X"), (Button::Y, "Y")]);
    }
    let shoulders: &[(Button, &str)] = if system == System::Gb {
        &[]
    } else {
        &[(Button::L, "L"), (Button::R, "R")]
    };
    let menu = vec![Entry {
        key: "esc".to_owned(),
        label: "Menu",
    }];
    [
        group(&[
            (Button::Up, "↑"),
            (Button::Left, "←"),
            (Button::Down, "↓"),
            (Button::Right, "→"),
        ]),
        group(&face),
        group(shoulders),
        group(&[(Button::Start, "Start"), (Button::Select, "Select")]),
        menu,
    ]
    .into_iter()
    .filter(|group| !group.is_empty())
    .collect()
}

/// The key to show of those bound: a letter or digit if there is one, as
/// easiest to find.
fn preferred(keys: &[String]) -> Option<String> {
    keys.iter()
        .find(|key| key.chars().count() == 1)
        .or_else(|| keys.first())
        .cloned()
}

/// `key`, as GPUI names it, as printed on a Mac keyboard.
fn key_name(key: &str) -> String {
    match key {
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "enter" => "⏎".to_owned(),
        "backspace" => "⌫".to_owned(),
        "space" => "␣".to_owned(),
        "tab" => "⇥".to_owned(),
        "escape" => "esc".to_owned(),
        other if other.chars().count() == 1 => other.to_uppercase(),
        other => other.to_owned(),
    }
}

/// About how wide `group` is drawn, in logical pixels.
fn width(group: &[Entry]) -> f32 {
    let columns: f32 = group
        .iter()
        .map(|entry| {
            let key = CHAR_WIDTH * entry.key.chars().count() as f32 + 10.0;
            let label = CHAR_WIDTH * entry.label.chars().count() as f32;
            key.max(label).max(CAP)
        })
        .sum();
    columns + KEY_GAP * group.len().saturating_sub(1) as f32
}

/// How tall the foot of the window the guide takes is, for `groups` in a
/// window `window_width` wide: the island, one line tall or as many as
/// the groups wrap onto, and the room around it.
pub fn height(groups: &[Vec<Entry>], window_width: f32) -> f32 {
    let room = (window_width - 2.0 * (INSET + PAD_X)).max(1.0);
    let mut lines = 1.0;
    let mut used = 0.0;
    for group in groups {
        let width = width(group);
        // Each group after the first brings a divider and space round it.
        let step = 2.0 * GROUP_GAP + 1.0 + width;
        if used > 0.0 && used + step > room {
            lines += 1.0;
            used = width;
        } else {
            used += if used > 0.0 { step } else { width };
        }
    }
    ABOVE + lines * LINE_HEIGHT + 2.0 * PAD_Y + INSET
}

/// The guide, in the foot of the window `height` tall: an island with
/// `groups` side by side, wrapping onto more lines if need be, through
/// which the backdrop shows.
pub fn guide(groups: &[Vec<Entry>], height: f32) -> AnyElement {
    let groups = groups.iter().enumerate().map(|(index, group)| {
        let divider = (index > 0).then(|| {
            div()
                .w(px(1.0))
                .h(px(22.0))
                .bg(rgba(with_alpha(TEXT, 0x1F)))
        });
        div()
            .flex()
            .items_center()
            .gap(px(GROUP_GAP))
            .children(divider)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(KEY_GAP))
                    .children(group.iter().map(key)),
            )
    });
    let island = div()
        .max_w_full()
        .px(px(PAD_X))
        .py(px(PAD_Y))
        .flex()
        .flex_wrap()
        .items_center()
        .justify_center()
        .gap_x(px(GROUP_GAP))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgba(with_alpha(TEXT, 0x17)))
        .bg(rgba(with_alpha(BACKGROUND, 0xA6)))
        .shadow(vec![BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.35),
            offset: point(px(0.0), px(4.0)),
            blur_radius: px(18.0),
            spread_radius: px(0.0),
        }])
        .children(groups);
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(height))
        .px(px(INSET))
        .pb(px(INSET))
        .flex()
        .items_end()
        .justify_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_xs()
        .line_height(px(14.0))
        .child(island)
        .into_any_element()
}

/// One key: its keycap over what it presses.
fn key(entry: &Entry) -> impl IntoElement {
    div()
        .h(px(LINE_HEIGHT))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(2.0))
        .child(
            div()
                .h(px(CAP))
                .min_w(px(CAP))
                .px(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .border_1()
                .border_color(rgba(with_alpha(TEXT, 0x24)))
                .bg(rgba(with_alpha(TEXT, 0x0F)))
                .text_color(rgb(TEXT))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(entry.key.clone()),
        )
        .child(
            div()
                .text_size(px(10.0))
                .line_height(px(12.0))
                .text_color(rgb(MUTED))
                .child(entry.label),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(groups: &[Vec<Entry>]) -> Vec<Vec<(&str, &str)>> {
        groups
            .iter()
            .map(|group| {
                group
                    .iter()
                    .map(|entry| (entry.key.as_str(), entry.label))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_default_keys_read_as_a_keyboard_prints_them() {
        let ds = groups(System::Nds, &Bindings::default());
        assert_eq!(
            labels(&ds),
            [
                vec![("W", "↑"), ("A", "←"), ("S", "↓"), ("D", "→")],
                vec![("K", "A"), ("J", "B"), ("I", "X"), ("U", "Y")],
                vec![("Q", "L"), ("E", "R")],
                vec![("␣", "Start"), ("⌫", "Select")],
                vec![("esc", "Menu")],
            ]
        );
    }

    #[test]
    fn each_system_shows_only_its_own_buttons() {
        let gb = groups(System::Gb, &Bindings::default());
        let gb = labels(&gb);
        assert_eq!(gb.len(), 4, "no shoulders on a Game Boy");
        assert_eq!(gb[1], [("K", "A"), ("J", "B")]);
        let gba = groups(System::Gba, &Bindings::default());
        let gba = labels(&gba);
        assert_eq!(gba[2], [("Q", "L"), ("E", "R")]);
        assert_eq!(gba[1].len(), 2, "no X or Y on a GBA");
    }

    #[test]
    fn the_island_fits_one_line_in_a_normal_window_and_wraps_when_narrow() {
        let ds = groups(System::Nds, &Bindings::default());
        let one_line = ABOVE + LINE_HEIGHT + 2.0 * PAD_Y + INSET;
        assert_eq!(height(&ds, 560.0), one_line);
        assert!(height(&ds, 200.0) > one_line);
    }

    #[test]
    fn keys_are_named_as_printed() {
        assert_eq!(key_name("enter"), "⏎");
        assert_eq!(key_name("up"), "↑");
        assert_eq!(key_name("k"), "K");
        assert_eq!(key_name("f1"), "f1");
        assert_eq!(
            preferred(&["up".to_owned(), "w".to_owned()]).as_deref(),
            Some("w")
        );
        assert_eq!(preferred(&[]), None);
    }
}
