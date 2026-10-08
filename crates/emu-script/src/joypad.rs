//! Buttons a script presses or releases for the player.

use emu_core::{Button, Buttons};

/// Buttons a script presses or releases on one frame, over whatever the
/// player holds. Buttons it names neither way stay as the player has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Joypad {
    /// Held whatever the player does.
    pub press: Buttons,
    /// Let go whatever the player does.
    pub release: Buttons,
}

impl Joypad {
    /// The buttons held once this is laid over `held`.
    #[must_use]
    pub fn apply(self, held: Buttons) -> Buttons {
        Button::ALL
            .into_iter()
            .filter(|&button| {
                self.press.contains(button)
                    || (held.contains(button) && !self.release.contains(button))
            })
            .collect()
    }

    /// This with `later` laid over it: what `later` names, it decides.
    #[must_use]
    pub fn then(self, later: Self) -> Self {
        let named = |button: Button| later.press.contains(button) || later.release.contains(button);
        let keep = |set: Buttons, button: Button| set.contains(button) && !named(button);
        Self {
            press: Button::ALL
                .into_iter()
                .filter(|&button| later.press.contains(button) || keep(self.press, button))
                .collect(),
            release: Button::ALL
                .into_iter()
                .filter(|&button| later.release.contains(button) || keep(self.release, button))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buttons(list: &[Button]) -> Buttons {
        list.iter().copied().collect()
    }

    #[test]
    fn presses_add_and_releases_remove() {
        let pad = Joypad {
            press: buttons(&[Button::A]),
            release: buttons(&[Button::Start]),
        };
        let held = buttons(&[Button::Start, Button::Left]);
        assert_eq!(pad.apply(held), buttons(&[Button::A, Button::Left]));
    }

    #[test]
    fn later_calls_decide_the_buttons_they_name() {
        let first = Joypad {
            press: buttons(&[Button::A, Button::B]),
            release: buttons(&[Button::Up]),
        };
        let second = Joypad {
            press: buttons(&[Button::Up]),
            release: buttons(&[Button::A]),
        };
        let both = first.then(second);
        assert_eq!(both.press, buttons(&[Button::B, Button::Up]));
        assert_eq!(both.release, buttons(&[Button::A]));
    }
}
