//! Which emulator inputs are held, combined from any number of keys and
//! gamepad controls.
//!
//! Each physical input is tracked by a source name such as `"key:x"` or
//! `"pad:<uuid>:South"`, so releasing one input never cancels another that
//! holds the same button.

use std::collections::HashMap;

use emu_core::Buttons;

use crate::bindings::Held;

/// The combined effect of everything held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeldState {
    /// Emulated buttons held.
    pub buttons: Buttons,
    /// Run unthrottled.
    pub fast_forward: bool,
    /// Step backwards through history.
    pub rewind: bool,
    /// Touch the screen at the gamepad cursor.
    pub touch: bool,
}

/// Held inputs by source.
#[derive(Debug, Default)]
pub struct Controls {
    pressed: HashMap<String, Held>,
    fast_forward_latched: bool,
}

impl Controls {
    /// Records `source` going down or up. A source with no `action` is
    /// ignored on press.
    pub fn set(&mut self, source: String, action: Option<Held>, down: bool) {
        match (down, action) {
            (true, Some(action)) => {
                self.pressed.insert(source, action);
            }
            (true, None) => {}
            (false, _) => {
                self.pressed.remove(&source);
            }
        }
    }

    /// Releases everything held, keeping a latched fast-forward.
    pub fn release_all(&mut self) {
        self.pressed.clear();
    }

    /// Turns latched fast-forward on or off; returns the new setting.
    pub fn toggle_fast_forward(&mut self) -> bool {
        self.fast_forward_latched = !self.fast_forward_latched;
        self.fast_forward_latched
    }

    /// The combined state.
    pub fn state(&self) -> HeldState {
        let mut state = HeldState {
            fast_forward: self.fast_forward_latched,
            ..HeldState::default()
        };
        for held in self.pressed.values() {
            match held {
                Held::Button(button) => state.buttons.set(*button, true),
                Held::FastForward => state.fast_forward = true,
                Held::Rewind => state.rewind = true,
                Held::Touch => state.touch = true,
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::Button;

    #[test]
    fn two_sources_hold_one_button_independently() {
        let mut controls = Controls::default();
        let a = Some(Held::Button(Button::A));
        controls.set("key:x".to_owned(), a, true);
        controls.set("pad:1:East".to_owned(), a, true);
        controls.set("key:x".to_owned(), a, false);
        assert!(controls.state().buttons.contains(Button::A));
        controls.set("pad:1:East".to_owned(), a, false);
        assert!(controls.state().buttons.is_empty());
    }

    #[test]
    fn unbound_presses_are_ignored_and_releases_always_count() {
        let mut controls = Controls::default();
        controls.set("key:p".to_owned(), None, true);
        assert_eq!(controls.state(), HeldState::default());
        controls.set("key:tab".to_owned(), Some(Held::FastForward), true);
        controls.set("key:tab".to_owned(), None, false);
        assert!(!controls.state().fast_forward);
    }

    #[test]
    fn touch_is_held_like_a_button() {
        let mut controls = Controls::default();
        controls.set("pad:1:RightThumb".to_owned(), Some(Held::Touch), true);
        controls.set("key:x".to_owned(), Some(Held::Button(Button::A)), true);
        let state = controls.state();
        assert!(state.touch && state.buttons.contains(Button::A));
        controls.set("pad:1:RightThumb".to_owned(), None, false);
        let state = controls.state();
        assert!(!state.touch && state.buttons.contains(Button::A));
    }

    #[test]
    fn latched_fast_forward_survives_release_all() {
        let mut controls = Controls::default();
        assert!(controls.toggle_fast_forward());
        controls.set("key:`".to_owned(), Some(Held::Rewind), true);
        controls.release_all();
        let state = controls.state();
        assert!(state.fast_forward);
        assert!(!state.rewind);
    }
}
