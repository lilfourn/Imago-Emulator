//! Which emulator inputs are held, combined from any number of keys and
//! gamepad controls.
//!
//! Each physical input is tracked by a source name such as `"key:x"` or
//! `"pad:<uuid>:South"`, so releasing one input never cancels another that
//! holds the same button.

use std::collections::HashMap;

use emu_core::Buttons;

use crate::bindings::Held;
use crate::speed::Pace;

/// The combined effect of everything held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeldState {
    /// Emulated buttons held.
    pub buttons: Buttons,
    /// Run faster or slower; fast-forward wins over slow motion.
    pub pace: Pace,
    /// Step backwards through history.
    pub rewind: bool,
    /// Touch the screen at the gamepad cursor.
    pub touch: bool,
    /// Blow into the microphone.
    pub blow: bool,
}

/// Held inputs by source.
#[derive(Debug, Default)]
pub struct Controls {
    pressed: HashMap<String, Held>,
    fast_forward_latched: bool,
    slow_motion_latched: bool,
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

    /// Releases everything held, keeping latched fast-forward and slow
    /// motion.
    pub fn release_all(&mut self) {
        self.pressed.clear();
    }

    /// Turns latched fast-forward on or off; returns the new setting.
    pub fn toggle_fast_forward(&mut self) -> bool {
        self.fast_forward_latched = !self.fast_forward_latched;
        self.fast_forward_latched
    }

    /// Turns latched slow motion on or off; returns the new setting.
    pub fn toggle_slow_motion(&mut self) -> bool {
        self.slow_motion_latched = !self.slow_motion_latched;
        self.slow_motion_latched
    }

    /// The combined state.
    pub fn state(&self) -> HeldState {
        let mut state = HeldState::default();
        let mut fast_forward = self.fast_forward_latched;
        let mut slow_motion = self.slow_motion_latched;
        for held in self.pressed.values() {
            match held {
                Held::Button(button) => state.buttons.set(*button, true),
                Held::FastForward => fast_forward = true,
                Held::SlowMotion => slow_motion = true,
                Held::Rewind => state.rewind = true,
                Held::Touch => state.touch = true,
                Held::Blow => state.blow = true,
            }
        }
        state.pace = if fast_forward {
            Pace::FastForward
        } else if slow_motion {
            Pace::SlowMotion
        } else {
            Pace::Normal
        };
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
        assert_eq!(controls.state().pace, Pace::Normal);
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
    fn blowing_is_held_like_a_button() {
        let mut controls = Controls::default();
        controls.set("key:b".to_owned(), Some(Held::Blow), true);
        assert!(controls.state().blow);
        controls.release_all();
        assert!(!controls.state().blow);
    }

    #[test]
    fn latched_fast_forward_survives_release_all() {
        let mut controls = Controls::default();
        assert!(controls.toggle_fast_forward());
        controls.set("key:`".to_owned(), Some(Held::Rewind), true);
        controls.release_all();
        let state = controls.state();
        assert_eq!(state.pace, Pace::FastForward);
        assert!(!state.rewind);
    }

    #[test]
    fn slow_motion_is_held_or_latched() {
        let mut controls = Controls::default();
        controls.set("key:1".to_owned(), Some(Held::SlowMotion), true);
        assert_eq!(controls.state().pace, Pace::SlowMotion);
        controls.set("key:1".to_owned(), None, false);
        assert_eq!(controls.state().pace, Pace::Normal);

        assert!(controls.toggle_slow_motion());
        controls.release_all();
        assert_eq!(controls.state().pace, Pace::SlowMotion);
        assert!(!controls.toggle_slow_motion());
        assert_eq!(controls.state().pace, Pace::Normal);
    }

    #[test]
    fn fast_forward_wins_over_slow_motion() {
        let mut controls = Controls::default();
        controls.toggle_slow_motion();
        controls.set("key:tab".to_owned(), Some(Held::FastForward), true);
        assert_eq!(controls.state().pace, Pace::FastForward);
        controls.set("key:tab".to_owned(), None, false);
        assert_eq!(controls.state().pace, Pace::SlowMotion);
    }
}
