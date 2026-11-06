//! Shortcuts and keys as each system writes them: `⌘⇧S` on macOS, as the
//! app always has, and `Ctrl+Shift+S` on Linux and Windows. Shortcuts are
//! written in GPUI's syntax, where `secondary` is ⌘ on macOS and Control
//! elsewhere, so one binding serves every system.

use gpui::{Action, Keystroke, Window};

/// Which way to write a shortcut: the system this build is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    /// Glyphs: `⌃⌥⌘⇧` and then the key, as the app's own words have
    /// always put them.
    Mac,
    /// Words joined with `+`: `Ctrl+Alt+Shift+S`, the logo key as `Win`.
    Windows,
    /// The same, with the logo key as `Super`.
    Linux,
}

const STYLE: Style = if cfg!(target_os = "macos") {
    Style::Mac
} else if cfg!(windows) {
    Style::Windows
} else {
    Style::Linux
};

/// The shortcut `binding`, in GPUI's syntax (`secondary-shift-s`), as this
/// system writes it. Several keystrokes, as in a chord, are written one
/// after another.
pub fn shortcut(binding: &str) -> String {
    binding
        .split_whitespace()
        .map(|source| match Keystroke::parse(source) {
            Ok(keystroke) => written(&keystroke, STYLE),
            Err(_) => source.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The shortcut that runs `action` where `window` has its focus, as this
/// system writes it, if it has one.
pub fn for_action(action: &dyn Action, window: &Window) -> Option<String> {
    let binding = window.highest_precedence_binding_for_action(action)?;
    let keystrokes: Vec<String> = binding
        .keystrokes()
        .iter()
        .map(|keystroke| {
            let mut shown = keystroke.inner().clone();
            shown.modifiers = *keystroke.modifiers();
            keystroke.key().clone_into(&mut shown.key);
            written(&shown, STYLE)
        })
        .collect();
    Some(keystrokes.join(" "))
}

/// A key as GPUI names it (`backspace`, `f11`, `a`), as this system's
/// keyboards print it: `⌫` on a Mac, `Backspace` elsewhere. The arrows
/// are arrows everywhere; only in a shortcut are they words off macOS.
pub fn key(name: &str) -> String {
    match name {
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        _ => key_name(name, STYLE),
    }
}

fn written(keystroke: &Keystroke, style: Style) -> String {
    let modifiers = keystroke.modifiers;
    let key = key_name(&keystroke.key, style);
    if style == Style::Mac {
        let mut text = String::new();
        for (held, glyph) in [
            (modifiers.function, "fn "),
            (modifiers.control, "⌃"),
            (modifiers.alt, "⌥"),
            (modifiers.platform, "⌘"),
            (modifiers.shift, "⇧"),
        ] {
            if held {
                text.push_str(glyph);
            }
        }
        text.push_str(&key);
        return text;
    }
    let logo = if style == Style::Windows {
        "Win"
    } else {
        "Super"
    };
    let mut words: Vec<&str> = [
        (modifiers.control, "Ctrl"),
        (modifiers.platform, logo),
        (modifiers.alt, "Alt"),
        (modifiers.shift, "Shift"),
        (modifiers.function, "Fn"),
    ]
    .into_iter()
    .filter(|(held, _)| *held)
    .map(|(_, word)| word)
    .collect();
    words.push(&key);
    words.join("+")
}

fn key_name(name: &str, style: Style) -> String {
    let mac = style == Style::Mac;
    let named = match name {
        "backspace" if mac => "⌫",
        "backspace" => "Backspace",
        "delete" if mac => "⌦",
        "delete" => "Delete",
        "enter" if mac => "⏎",
        "enter" => "Enter",
        "escape" if mac => "esc",
        "escape" => "Esc",
        "tab" if mac => "⇥",
        "tab" => "Tab",
        "space" => "Space",
        "left" if mac => "←",
        "left" => "Left",
        "right" if mac => "→",
        "right" => "Right",
        "up" if mac => "↑",
        "up" => "Up",
        "down" if mac => "↓",
        "down" => "Down",
        "home" if mac => "↖",
        "home" => "Home",
        "end" if mac => "↘",
        "end" => "End",
        "pageup" if mac => "⇞",
        "pageup" => "Page Up",
        "pagedown" if mac => "⇟",
        "pagedown" => "Page Down",
        "shift" if mac => "⇧",
        "shift" => "Shift",
        "control" if mac => "⌃",
        "control" => "Ctrl",
        "alt" if mac => "⌥",
        "alt" => "Alt",
        other => {
            // Function keys and letters in capitals; anything else as it is.
            let function = other.strip_prefix('f').is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            });
            return if function || other.chars().count() == 1 {
                other.to_uppercase()
            } else {
                other.to_owned()
            };
        }
    };
    named.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn each(source: &str, style: Style) -> String {
        written(&Keystroke::parse(source).expect("a keystroke"), style)
    }

    #[test]
    fn macos_keeps_the_glyphs_the_app_always_wrote() {
        // As in the notices, buttons and hints before Linux and Windows.
        assert_eq!(each("cmd-shift-f", Style::Mac), "⌘⇧F");
        assert_eq!(each("cmd-shift-a", Style::Mac), "⌘⇧A");
        assert_eq!(each("cmd-alt-s", Style::Mac), "⌥⌘S");
        assert_eq!(each("cmd-backspace", Style::Mac), "⌘⌫");
        assert_eq!(each("cmd-z", Style::Mac), "⌘Z");
        assert_eq!(each("ctrl-cmd-f", Style::Mac), "⌃⌘F");
        assert_eq!(each("cmd-alt-right", Style::Mac), "⌥⌘→");
        assert_eq!(each("cmd-,", Style::Mac), "⌘,");
    }

    #[test]
    fn linux_and_windows_write_words() {
        assert_eq!(each("ctrl-shift-s", Style::Linux), "Ctrl+Shift+S");
        assert_eq!(each("ctrl-alt-s", Style::Windows), "Ctrl+Alt+S");
        assert_eq!(each("f11", Style::Windows), "F11");
        assert_eq!(each("delete", Style::Linux), "Delete");
        assert_eq!(each("ctrl-shift-right", Style::Linux), "Ctrl+Shift+Right");
        assert_eq!(each("ctrl-]", Style::Windows), "Ctrl+]");
        assert_eq!(each("super-e", Style::Linux), "Super+E");
        assert_eq!(each("win-e", Style::Windows), "Win+E");
    }

    #[test]
    fn secondary_is_command_on_macos_and_control_elsewhere() {
        let expected = if cfg!(target_os = "macos") {
            "⌘O"
        } else {
            "Ctrl+O"
        };
        assert_eq!(shortcut("secondary-o"), expected);
    }

    #[test]
    fn keys_are_named_as_each_keyboard_prints_them() {
        assert_eq!(key_name("tab", Style::Mac), "⇥");
        assert_eq!(key_name("tab", Style::Windows), "Tab");
        assert_eq!(key_name("escape", Style::Mac), "esc");
        assert_eq!(key_name("escape", Style::Linux), "Esc");
        assert_eq!(key_name("enter", Style::Mac), "⏎");
        assert_eq!(key_name("f", Style::Linux), "F");
        assert_eq!(key_name("f10", Style::Linux), "F10");
        assert_eq!(key_name("[", Style::Mac), "[");
    }
}
