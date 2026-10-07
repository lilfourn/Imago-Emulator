//! Keeps each press down long enough for the game to see it.
//!
//! Games read their buttons and touchscreen at most once a frame, some
//! less often. A press that comes and goes between two reads is never
//! seen, and a trackpad's tap to click can send its button-down and
//! button-up a few milliseconds apart, inside one frame. A
//! [`Latch`] shows each newly pressed button for at least
//! [`BUTTON_FRAMES`] frames and each new touch for at least
//! [`TOUCH_FRAMES`], putting off a release that comes sooner until then.
//! It counts emulated frames, not time, so the hold is the same at any
//! speed.

use emu_core::{Button, Input};

/// The fewest frames a newly pressed button is held: enough for a game
/// that reads its buttons every second frame, as one running at 30 frames
/// a second does. A key held down by hand lasts longer, so this changes
/// only taps too quick to have been seen. Some screens read less often
/// still (Pokemon White's opening movie checks A every sixth frame), but
/// holding every tap that long would turn quick taps into holds.
pub const BUTTON_FRAMES: u8 = 2;

/// The fewest frames a new touch is held. A DS game sees the touchscreen
/// through a sample its ARM7 takes and hands to the ARM9, and many DS
/// games run at 30 or 20 frames a second, so a touch should outlast a game
/// frame plus that handover. Three frames, 50 ms, is still shorter than a
/// quick stylus tap. (Pokemon White's naming keyboard sees even one.)
pub const TOUCH_FRAMES: u8 = 3;

/// One player's input: what they hold now, and what the game is shown.
#[derive(Debug, Clone, Default)]
pub struct Latch {
    /// What the player holds now.
    held: Input,
    /// What the game sees in the next frame.
    shown: Input,
    /// Per button, in [`Button::ALL`] order: frames it must still be shown.
    button_frames: [u8; Button::ALL.len()],
    /// Frames the touch must still be shown.
    touch_frames: u8,
}

impl Latch {
    /// Takes what the player holds now. A new press or touch, or a move
    /// of the touch, shows at once; a release waits out the minimum hold.
    pub fn set(&mut self, held: Input) {
        for button in Button::ALL {
            if held.buttons.contains(button) && !self.shown.buttons.contains(button) {
                self.button_frames[button as usize] = BUTTON_FRAMES;
            }
        }
        if held.touch.is_some() && self.shown.touch.is_none() {
            self.touch_frames = TOUCH_FRAMES;
        }
        self.held = held;
        self.show();
    }

    /// What the game should see in the next frame.
    pub fn input(&self) -> Input {
        self.shown
    }

    /// Counts one frame run with [`Latch::input`], releasing whatever has
    /// now been shown long enough.
    pub fn advance(&mut self) {
        for frames in &mut self.button_frames {
            *frames = frames.saturating_sub(1);
        }
        self.touch_frames = self.touch_frames.saturating_sub(1);
        self.show();
    }

    /// Drops releases still waiting, so the game sees just what is held
    /// now: for when the game jumps to another moment, where a press
    /// from before would be out of place.
    pub fn clear(&mut self) {
        self.button_frames = [0; Button::ALL.len()];
        self.touch_frames = 0;
        self.show();
    }

    /// Shows what is held, plus whatever is still within its minimum hold.
    fn show(&mut self) {
        let mut buttons = self.held.buttons;
        for button in Button::ALL {
            if self.button_frames[button as usize] > 0 {
                buttons.set(button, true);
            }
        }
        let touch = match self.held.touch {
            Some(point) => Some(point),
            // The pen stays where it was last shown.
            None if self.touch_frames > 0 => self.shown.touch,
            None => None,
        };
        self.shown = Input { buttons, touch };
    }
}

#[cfg(test)]
mod tests {
    use emu_core::{Buttons, TouchPoint};

    use super::*;

    const POINT: TouchPoint = TouchPoint { x: 128, y: 96 };

    fn touch(point: TouchPoint) -> Input {
        Input {
            touch: Some(point),
            ..Input::default()
        }
    }

    fn buttons(held: &[Button]) -> Input {
        Input {
            buttons: held.iter().copied().collect(),
            ..Input::default()
        }
    }

    /// What the game sees over the next `frames` frames.
    fn run(latch: &mut Latch, frames: usize) -> Vec<Input> {
        (0..frames)
            .map(|_| {
                let input = latch.input();
                latch.advance();
                input
            })
            .collect()
    }

    #[test]
    fn a_tap_inside_one_frame_touches_for_touch_frames() {
        let mut latch = Latch::default();
        latch.set(touch(POINT));
        latch.set(Input::default());
        let seen = run(&mut latch, 5);
        let touched = seen.iter().take_while(|input| input.touch == Some(POINT));
        assert_eq!(touched.count(), usize::from(TOUCH_FRAMES));
        assert!(
            seen[usize::from(TOUCH_FRAMES)..]
                .iter()
                .all(|i| i.touch.is_none())
        );
    }

    #[test]
    fn a_quick_key_tap_is_held_for_button_frames() {
        let mut latch = Latch::default();
        latch.set(buttons(&[Button::A]));
        latch.set(Input::default());
        let seen = run(&mut latch, 4);
        let held = seen
            .iter()
            .filter(|input| input.buttons.contains(Button::A));
        assert_eq!(held.count(), usize::from(BUTTON_FRAMES));
        assert_eq!(seen.last().map(|input| input.buttons), Some(Buttons::NONE));
    }

    #[test]
    fn a_release_after_the_minimum_applies_at_once() {
        let mut latch = Latch::default();
        latch.set(Input {
            buttons: [Button::Up].into_iter().collect(),
            touch: Some(POINT),
        });
        run(&mut latch, 10);
        latch.set(Input::default());
        assert_eq!(latch.input(), Input::default());
    }

    #[test]
    fn a_release_part_way_through_waits_only_for_the_rest() {
        let mut latch = Latch::default();
        latch.set(touch(POINT));
        run(&mut latch, 1);
        latch.set(Input::default());
        let touched = run(&mut latch, 5)
            .iter()
            .filter(|input| input.touch.is_some())
            .count();
        assert_eq!(touched, usize::from(TOUCH_FRAMES) - 1);
    }

    #[test]
    fn pressing_again_while_a_release_waits_keeps_it_held() {
        let mut latch = Latch::default();
        latch.set(buttons(&[Button::B]));
        latch.set(Input::default());
        latch.set(buttons(&[Button::B]));
        assert!(
            run(&mut latch, 10)
                .iter()
                .all(|i| i.buttons.contains(Button::B))
        );
        latch.set(Input::default());
        assert_eq!(latch.input(), Input::default());
    }

    #[test]
    fn a_press_after_the_hold_ends_is_a_new_press() {
        let mut latch = Latch::default();
        latch.set(buttons(&[Button::A]));
        latch.set(Input::default());
        run(&mut latch, usize::from(BUTTON_FRAMES));
        assert!(latch.input().buttons.is_empty());
        latch.set(buttons(&[Button::A]));
        latch.set(Input::default());
        let held = run(&mut latch, 4)
            .iter()
            .filter(|input| input.buttons.contains(Button::A))
            .count();
        assert_eq!(held, usize::from(BUTTON_FRAMES));
    }

    #[test]
    fn a_drag_moves_at_once_and_a_lift_stays_where_it_was() {
        let mut latch = Latch::default();
        let moved = TouchPoint { x: 10, y: 20 };
        latch.set(touch(POINT));
        latch.set(touch(moved));
        assert_eq!(latch.input().touch, Some(moved));
        latch.set(Input::default());
        let seen = run(&mut latch, usize::from(TOUCH_FRAMES));
        assert!(seen.iter().all(|input| input.touch == Some(moved)));
        assert_eq!(latch.input().touch, None);
    }

    #[test]
    fn keys_held_while_tapping_stay_held() {
        let mut latch = Latch::default();
        let left = buttons(&[Button::Left]);
        latch.set(left);
        run(&mut latch, 10);
        latch.set(Input {
            touch: Some(POINT),
            ..left
        });
        latch.set(left);
        let seen = run(&mut latch, 5);
        assert!(seen.iter().all(|input| input.buttons == left.buttons));
        let touched = seen.iter().filter(|input| input.touch.is_some()).count();
        assert_eq!(touched, usize::from(TOUCH_FRAMES));
        latch.set(Input::default());
        assert_eq!(latch.input(), Input::default());
    }

    #[test]
    fn a_key_tap_while_touching_leaves_the_touch_alone() {
        let mut latch = Latch::default();
        latch.set(touch(POINT));
        run(&mut latch, 10);
        latch.set(Input {
            buttons: [Button::A].into_iter().collect(),
            touch: Some(POINT),
        });
        latch.set(touch(POINT));
        let seen = run(&mut latch, 4);
        assert!(seen.iter().all(|input| input.touch == Some(POINT)));
        let held = seen
            .iter()
            .filter(|i| i.buttons.contains(Button::A))
            .count();
        assert_eq!(held, usize::from(BUTTON_FRAMES));
        latch.set(Input::default());
        assert_eq!(latch.input().touch, None);
    }

    #[test]
    fn each_button_keeps_its_own_hold() {
        let mut latch = Latch::default();
        latch.set(buttons(&[Button::A]));
        latch.set(Input::default());
        run(&mut latch, 1);
        latch.set(buttons(&[Button::B]));
        latch.set(Input::default());
        let seen = run(&mut latch, 3);
        let a = seen.iter().map(|i| i.buttons.contains(Button::A));
        let b = seen.iter().map(|i| i.buttons.contains(Button::B));
        assert_eq!(a.collect::<Vec<_>>(), [true, false, false]);
        assert_eq!(b.collect::<Vec<_>>(), [true, true, false]);
    }

    #[test]
    fn clear_drops_waiting_releases_but_not_held_input() {
        let mut latch = Latch::default();
        latch.set(buttons(&[Button::Down]));
        run(&mut latch, 10);
        latch.set(Input {
            buttons: [Button::Down, Button::A].into_iter().collect(),
            touch: Some(POINT),
        });
        latch.set(buttons(&[Button::Down]));
        latch.clear();
        assert_eq!(latch.input(), buttons(&[Button::Down]));
    }
}
