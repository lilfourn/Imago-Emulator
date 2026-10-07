//! Gamepad hotkeys: a modifier control, Select by default, held with
//! another control runs an app command (save state, pause, ...) instead
//! of pressing game buttons.
//!
//! The `[hotkeys]` table in the bindings decides which controls are
//! modifiers and what each other control does with one (see
//! `bindings.rs`). [`Chords`] turns each control's press or release into
//! [`Effect`]s, one controller at a time:
//!
//! - Pressing a modifier does nothing at first. Let go of again on its
//!   own, it reaches the game as a tap: pressed, then released, which the
//!   latch keeps on screen for a couple of frames (see `latch.rs`).
//! - While it is down, pressing a control bound in `[hotkeys]` runs that
//!   hotkey. The game sees neither that control nor the modifier, even
//!   after they are let go of, so Select+R1 saves a state without pressing
//!   Select or R.
//! - Pressing any other control while it is down, before a hotkey has run,
//!   hands the modifier to the game: the game sees it held from then until
//!   it is let go of, along with the other control, so a game's own
//!   Select+button combinations still work. Hotkeys are off until the
//!   modifier comes up.
//! - Without a modifier down, every control does what its bindings say.
//!
//! So a modifier held on its own for a while reaches the game only when let
//! go of. Bind the modifier to a control the game doesn't need held, or
//! leave `modifier` empty to turn hotkeys off.

use std::collections::HashMap;

use crate::bindings::{Held, Hotkey, Shortcut};

/// What a control's press or release does once hotkeys are accounted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Press or release the control as its bindings say.
    Pass {
        /// The control.
        control: String,
        /// Pressed or released.
        pressed: bool,
    },
    /// Hold `held` until the control is released, whatever its bindings
    /// say.
    Hold {
        /// The control.
        control: String,
        /// What it holds.
        held: Held,
    },
    /// Run an app command once.
    Run(Shortcut),
}

/// What a modifier held down has done so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    /// Nothing yet: let go of now, it is a tap.
    Waiting,
    /// It ran a hotkey, so the game never sees it.
    Chorded,
    /// The game saw it pressed, with another control.
    Passed,
}

/// The modifier a controller holds down.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Modifier {
    control: String,
    used: Use,
}

/// Hotkey chords in progress, per controller.
#[derive(Debug, Default)]
pub struct Chords {
    /// By controller UUID.
    modifiers: HashMap<String, Modifier>,
}

impl Chords {
    /// What `control` on `controller` going down or up does, given
    /// `hotkey`, which looks up a control's entry in the `[hotkeys]`
    /// table.
    pub fn event(
        &mut self,
        hotkey: impl Fn(&str) -> Option<Hotkey>,
        controller: &str,
        control: &str,
        pressed: bool,
    ) -> Vec<Effect> {
        let pass = |pressed| Effect::Pass {
            control: control.to_owned(),
            pressed,
        };
        if !pressed {
            let is_modifier = self
                .modifiers
                .get(controller)
                .is_some_and(|modifier| modifier.control == control);
            if !is_modifier {
                // A control whose press was taken by a hotkey never
                // reached the game; releasing it there does nothing.
                return vec![pass(false)];
            }
            return match self.modifiers.remove(controller).map(|m| m.used) {
                Some(Use::Waiting) => vec![pass(true), pass(false)],
                Some(Use::Chorded) => Vec::new(),
                Some(Use::Passed) | None => vec![pass(false)],
            };
        }
        let Some(modifier) = self.modifiers.get_mut(controller) else {
            if hotkey(control) == Some(Hotkey::Modifier) {
                self.modifiers.insert(
                    controller.to_owned(),
                    Modifier {
                        control: control.to_owned(),
                        used: Use::Waiting,
                    },
                );
                return Vec::new();
            }
            return vec![pass(true)];
        };
        match hotkey(control) {
            Some(Hotkey::Run(shortcut)) if modifier.used != Use::Passed => {
                modifier.used = Use::Chorded;
                vec![Effect::Run(shortcut)]
            }
            Some(Hotkey::Hold(held)) if modifier.used != Use::Passed => {
                modifier.used = Use::Chorded;
                vec![Effect::Hold {
                    control: control.to_owned(),
                    held,
                }]
            }
            _ => {
                let mut effects = Vec::with_capacity(2);
                if modifier.used == Use::Waiting {
                    modifier.used = Use::Passed;
                    effects.push(Effect::Pass {
                        control: modifier.control.clone(),
                        pressed: true,
                    });
                }
                effects.push(pass(true));
                effects
            }
        }
    }

    /// Forgets every modifier held down, as when the window loses focus
    /// and everything is released.
    pub fn clear(&mut self) {
        self.modifiers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Select is the modifier; R1 saves, Start pauses, L2 rewinds.
    fn hotkey(control: &str) -> Option<Hotkey> {
        match control {
            "Select" => Some(Hotkey::Modifier),
            "RightTrigger" => Some(Hotkey::Run(Shortcut::SaveState)),
            "Start" => Some(Hotkey::Run(Shortcut::Pause)),
            "LeftTrigger2" => Some(Hotkey::Hold(Held::Rewind)),
            _ => None,
        }
    }

    fn pass(control: &str, pressed: bool) -> Effect {
        Effect::Pass {
            control: control.to_owned(),
            pressed,
        }
    }

    /// Feeds `events` from one controller, collecting every effect.
    fn play(chords: &mut Chords, events: &[(&str, bool)]) -> Vec<Effect> {
        events
            .iter()
            .flat_map(|(control, pressed)| chords.event(hotkey, "pad", control, *pressed))
            .collect()
    }

    #[test]
    fn a_chord_runs_its_hotkey_and_the_game_sees_neither_control() {
        let mut chords = Chords::default();
        let effects = play(
            &mut chords,
            &[
                ("Select", true),
                ("RightTrigger", true),
                ("RightTrigger", false),
                ("Select", false),
            ],
        );
        assert_eq!(
            effects,
            [
                Effect::Run(Shortcut::SaveState),
                // Releasing a control the game never saw pressed is
                // harmless.
                pass("RightTrigger", false),
            ]
        );
    }

    #[test]
    fn letting_go_of_the_modifier_first_still_hides_both() {
        let mut chords = Chords::default();
        let effects = play(
            &mut chords,
            &[
                ("Select", true),
                ("Start", true),
                ("Select", false),
                ("Start", false),
            ],
        );
        assert_eq!(
            effects,
            [Effect::Run(Shortcut::Pause), pass("Start", false)]
        );
    }

    #[test]
    fn the_modifier_alone_reaches_the_game_as_a_tap_on_release() {
        let mut chords = Chords::default();
        assert_eq!(play(&mut chords, &[("Select", true)]), []);
        assert_eq!(
            play(&mut chords, &[("Select", false)]),
            [pass("Select", true), pass("Select", false)]
        );
    }

    #[test]
    fn another_control_hands_the_modifier_to_the_game() {
        let mut chords = Chords::default();
        let effects = play(
            &mut chords,
            &[
                ("Select", true),
                ("East", true),
                // Hotkeys are off now: R1 is the game's R.
                ("RightTrigger", true),
                ("Select", false),
            ],
        );
        assert_eq!(
            effects,
            [
                pass("Select", true),
                pass("East", true),
                pass("RightTrigger", true),
                pass("Select", false),
            ]
        );
    }

    #[test]
    fn hotkeys_run_one_after_another_while_the_modifier_is_held() {
        let mut chords = Chords::default();
        let effects = play(
            &mut chords,
            &[
                ("Select", true),
                ("RightTrigger", true),
                ("Start", true),
                // An ordinary control after a chord reaches the game, but
                // the modifier stays hidden.
                ("East", true),
                ("Select", false),
            ],
        );
        assert_eq!(
            effects,
            [
                Effect::Run(Shortcut::SaveState),
                Effect::Run(Shortcut::Pause),
                pass("East", true),
            ]
        );
    }

    #[test]
    fn a_held_hotkey_holds_until_its_control_is_released() {
        let mut chords = Chords::default();
        let effects = play(
            &mut chords,
            &[
                ("Select", true),
                ("LeftTrigger2", true),
                ("Select", false),
                ("LeftTrigger2", false),
            ],
        );
        assert_eq!(
            effects,
            [
                Effect::Hold {
                    control: "LeftTrigger2".to_owned(),
                    held: Held::Rewind,
                },
                pass("LeftTrigger2", false),
            ]
        );
    }

    #[test]
    fn without_the_modifier_hotkey_controls_are_game_buttons() {
        let mut chords = Chords::default();
        assert_eq!(
            play(&mut chords, &[("Start", true), ("Start", false)]),
            [pass("Start", true), pass("Start", false)]
        );
        // Held before the modifier went down, R1 stays the game's.
        let effects = play(
            &mut chords,
            &[("RightTrigger", true), ("Select", true), ("Select", false)],
        );
        assert_eq!(
            effects,
            [
                pass("RightTrigger", true),
                pass("Select", true),
                pass("Select", false)
            ]
        );
    }

    #[test]
    fn controllers_chord_independently() {
        let mut chords = Chords::default();
        assert_eq!(chords.event(hotkey, "one", "Select", true), []);
        assert_eq!(
            chords.event(hotkey, "two", "RightTrigger", true),
            [pass("RightTrigger", true)]
        );
        assert_eq!(
            chords.event(hotkey, "one", "RightTrigger", true),
            [Effect::Run(Shortcut::SaveState)]
        );
    }

    #[test]
    fn clear_forgets_a_modifier_held_down() {
        let mut chords = Chords::default();
        play(&mut chords, &[("Select", true)]);
        chords.clear();
        // Everything was released already; this release changes nothing,
        // and is no tap.
        assert_eq!(
            play(&mut chords, &[("Select", false)]),
            [pass("Select", false)]
        );
    }

    #[test]
    fn no_modifier_means_no_hotkeys() {
        let mut chords = Chords::default();
        let none = |_: &str| None;
        let effects: Vec<Effect> = [("Select", true), ("RightTrigger", true)]
            .into_iter()
            .flat_map(|(control, pressed)| chords.event(none, "pad", control, pressed))
            .collect();
        assert_eq!(effects, [pass("Select", true), pass("RightTrigger", true)]);
    }
}
