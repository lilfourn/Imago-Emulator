//! The key guide: the keys that play the running game, read from the
//! player's own bindings, so it stays right when they rebind. It shows in
//! the game menu, under its tiles, as a list in two columns: what a row
//! presses on the left, its keys on flat caps on the right. Moving and the
//! face buttons go in the first column; the shoulders, Start and Select in
//! the second. It only shows; it takes no clicks.

use emu_core::{Button, System};
use gpui::{AnyElement, Div, div, prelude::*, px, rgb, rgba};

use crate::bindings::{Bindings, Held};
use crate::probe;
use crate::theme::{LINE, MUTED, TEXT, with_alpha};

/// Height of a row.
const ROW_HEIGHT: f32 = 26.0;
/// A keycap's height, and its least width so single letters sit in
/// squares.
const CAP: f32 = 20.0;
/// Space between a row's keycaps, and between the two columns.
const CAP_GAP: f32 = 4.0;
const COLUMN_GAP: f32 = 24.0;
/// The narrowest the guide gets in two columns; narrower, they stack.
const TWO_COLUMNS: f32 = 300.0;

/// A row of the guide: what it presses, and the keys that press it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    /// What it presses: "Move", "A", "Start".
    pub label: &'static str,
    /// The keys, as printed on the keyboard: "K", or "W", "A", "S", "D"
    /// for moving.
    pub keys: Vec<String>,
}

/// The guide's two columns for a `system` game played with `bindings`:
/// moving and the face buttons the system has, then its shoulders, Start
/// and Select. Unbound buttons are left out.
pub fn columns(system: System, bindings: &Bindings) -> [Vec<Control>; 2] {
    let key = |button: Button| preferred(&bindings.keys_for(Held::Button(button)));
    let rows = |buttons: &[(Button, &'static str)]| -> Vec<Control> {
        buttons
            .iter()
            .filter_map(|&(button, label)| {
                key(button).map(|key| Control {
                    label,
                    keys: vec![key],
                })
            })
            .collect()
    };
    // Up, left, down, right: W A S D, as a keyboard has them.
    let moving: Vec<String> = [Button::Up, Button::Left, Button::Down, Button::Right]
        .into_iter()
        .filter_map(key)
        .collect();
    let mut face = vec![(Button::A, "A"), (Button::B, "B")];
    // RPG Maker games take the DS's buttons.
    if matches!(system, System::Nds | System::N3ds | System::RpgMaker) {
        face.extend([(Button::X, "X"), (Button::Y, "Y")]);
    }
    let shoulders: &[(Button, &str)] = match system {
        System::Gb => &[],
        System::Gba | System::Nds | System::RpgMaker => &[(Button::L, "L"), (Button::R, "R")],
        System::N3ds => &[
            (Button::L, "L"),
            (Button::R, "R"),
            (Button::Zl, "ZL"),
            (Button::Zr, "ZR"),
        ],
    };
    let first = (!moving.is_empty())
        .then_some(Control {
            label: "Move",
            keys: moving,
        })
        .into_iter()
        .chain(rows(&face))
        .collect();
    let mut second = rows(shoulders);
    second.extend(rows(&[
        (Button::Start, "Start"),
        (Button::Select, "Select"),
    ]));
    [first, second]
}

/// The key to show of those bound, as printed: a letter or digit if there
/// is one, as easiest to find.
fn preferred(keys: &[String]) -> Option<String> {
    keys.iter()
        .find(|key| key.chars().count() == 1)
        .or_else(|| keys.first())
        .map(|key| key_name(key))
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
        "space" => "Space".to_owned(),
        "tab" => "⇥".to_owned(),
        "escape" => "esc".to_owned(),
        other if other.chars().count() == 1 => other.to_uppercase(),
        other => other.to_owned(),
    }
}

/// The guide for the game menu, `width` logical pixels wide: a heading,
/// a line over it, and `columns` side by side, or one over the other when
/// two would be too narrow.
pub fn guide(columns: &[Vec<Control>; 2], width: f32) -> AnyElement {
    let column = |rows: &[Control]| div().flex().flex_col().children(rows.iter().map(row));
    let [first, second] = columns;
    let body = if width >= TWO_COLUMNS {
        div()
            .flex()
            .items_start()
            .gap(px(COLUMN_GAP))
            .child(column(first).flex_1().min_w_0())
            .child(column(second).flex_1().min_w_0())
    } else {
        div()
            .flex()
            .flex_col()
            .child(column(first))
            .child(column(second))
    };
    div()
        .mt(px(4.0))
        .pt(px(12.0))
        .border_t_1()
        .border_color(rgb(LINE))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .text_xs()
                .line_height(px(16.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(MUTED))
                .child("Controls"),
        )
        .child(body)
        .children(probe::part("controls"))
        .into_any_element()
}

/// One row: what it presses, then its keys at the far side.
fn row(control: &Control) -> Div {
    div()
        .h(px(ROW_HEIGHT))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .line_clamp(1)
                .text_color(rgb(MUTED))
                .child(control.label),
        )
        .child(div().flex_none().flex().gap(px(CAP_GAP)).children(
            control.keys.iter().enumerate().map(|(at, key)| {
                cap(key).children(probe::part(format!("controls.cap:{}.{at}", control.label)))
            }),
        ))
        .children(probe::part(format!("controls.row:{}", control.label)))
}

/// A flat keycap printed with `key`.
fn cap(key: &str) -> Div {
    div()
        .h(px(CAP))
        .min_w(px(CAP))
        .px(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .bg(rgba(with_alpha(TEXT, 0x14)))
        .text_size(px(11.0))
        .line_height(px(CAP))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(TEXT))
        .child(key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(columns: &[Vec<Control>; 2]) -> [Vec<(&str, String)>; 2] {
        columns.each_ref().map(|rows| {
            rows.iter()
                .map(|control| (control.label, control.keys.join(" ")))
                .collect()
        })
    }

    fn row(label: &'static str, keys: &str) -> (&'static str, String) {
        (label, keys.to_owned())
    }

    #[test]
    fn the_default_keys_read_as_a_keyboard_prints_them() {
        let ds = columns(System::Nds, &Bindings::default());
        assert_eq!(
            labels(&ds),
            [
                vec![
                    row("Move", "W A S D"),
                    row("A", "K"),
                    row("B", "J"),
                    row("X", "I"),
                    row("Y", "U"),
                ],
                vec![
                    row("L", "Q"),
                    row("R", "E"),
                    row("Start", "Space"),
                    row("Select", "⌫"),
                ],
            ]
        );
    }

    #[test]
    fn each_system_shows_only_its_own_buttons() {
        let gb = columns(System::Gb, &Bindings::default());
        let [first, second] = labels(&gb);
        assert_eq!(
            first,
            [row("Move", "W A S D"), row("A", "K"), row("B", "J")]
        );
        assert_eq!(
            second,
            [row("Start", "Space"), row("Select", "⌫")],
            "no shoulders on a Game Boy"
        );
        let gba = columns(System::Gba, &Bindings::default());
        let [first, second] = labels(&gba);
        assert_eq!(first.len(), 3, "no X or Y on a GBA");
        assert_eq!(second[..2], [row("L", "Q"), row("R", "E")]);
        let ds = columns(System::Nds, &Bindings::default());
        assert_eq!(ds[1].len(), 4, "no ZL or ZR on a DS");
    }

    #[test]
    fn a_3ds_game_shows_the_ds_s_buttons_and_zl_and_zr() {
        let n3ds = columns(System::N3ds, &Bindings::default());
        let [first, second] = labels(&n3ds);
        assert_eq!(first.len(), 5);
        assert_eq!(
            second,
            [
                row("L", "Q"),
                row("R", "E"),
                row("ZL", "Z"),
                row("ZR", "C"),
                row("Start", "Space"),
                row("Select", "⌫"),
            ]
        );
    }

    #[test]
    fn keys_are_named_as_printed() {
        assert_eq!(key_name("enter"), "⏎");
        assert_eq!(key_name("up"), "↑");
        assert_eq!(key_name("k"), "K");
        assert_eq!(key_name("f1"), "f1");
        assert_eq!(
            preferred(&["up".to_owned(), "w".to_owned()]).as_deref(),
            Some("W")
        );
        assert_eq!(preferred(&["up".to_owned()]).as_deref(), Some("↑"));
        assert_eq!(preferred(&[]), None);
    }
}
