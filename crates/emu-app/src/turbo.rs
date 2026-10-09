//! Turbo: buttons pressed and released over and over while their turbo
//! input is held.
//!
//! Each turbo button is pressed for `frames` frames, released for as many,
//! and so on, counted in emulated frames on the emulation thread: the
//! pattern is the same at any speed and from one run to the next. It starts
//! with a press in the first frame after its turbo input goes down.
//!
//! A press is never cut short. A turbo input let go of during a press
//! finishes it, so a tap quicker than a frame still presses once and the
//! game never sees a press shorter than `frames`. Let go of between
//! presses, it stops at once.
//!
//! Turbo presses go round the [`Latch`](crate::latch::Latch): its minimum
//! hold would stretch a one-frame press to two and blur the pattern. A
//! button held normally and by turbo at once is simply held.

use emu_core::{Button, Buttons, Input};

/// Frames each turbo press lasts, and each gap between presses, unless the
/// bindings say otherwise: ten presses a second at 60 frames a second.
/// Games read their buttons once per game frame, and many run at 30 or 20
/// frames a second, so a press must outlast two or three emulated frames
/// for every one to be seen.
pub const DEFAULT_FRAMES: u8 = 3;

/// One player's turbo buttons.
#[derive(Debug, Clone)]
pub struct Turbo {
    /// Frames per press, and per gap; at least 1.
    frames: u8,
    /// Turbo inputs held now.
    held: Buttons,
    /// Per button, in [`Button::ALL`] order: while it is pulsing, how far
    /// it is through one press and one gap.
    phase: [Option<u16>; Button::ALL.len()],
}

impl Default for Turbo {
    fn default() -> Self {
        Self {
            frames: DEFAULT_FRAMES,
            held: Buttons::NONE,
            phase: [None; Button::ALL.len()],
        }
    }
}

impl Turbo {
    /// Takes the turbo inputs held now and how many frames each press and
    /// gap lasts. A newly held button presses in the next frame; one let
    /// go of finishes its press first.
    pub fn set(&mut self, held: Buttons, frames: u8) {
        self.frames = frames.max(1);
        for button in Button::ALL {
            let phase = &mut self.phase[button as usize];
            if held.contains(button) && phase.is_none() {
                *phase = Some(0);
            }
        }
        self.held = held;
        self.stop_released();
    }

    /// `input` with the turbo buttons pressed in the next frame added.
    pub fn apply(&self, mut input: Input) -> Input {
        for button in Button::ALL {
            let phase = self.phase[button as usize];
            if phase.is_some_and(|phase| self.pressed_at(phase)) {
                input.buttons.set(button, true);
            }
        }
        input
    }

    /// Counts one frame run with [`Turbo::apply`].
    pub fn advance(&mut self) {
        let period = 2 * u16::from(self.frames);
        for phase in self.phase.iter_mut().flatten() {
            *phase = (*phase + 1) % period;
        }
        self.stop_released();
    }

    /// Drops presses being finished and starts each held button again with
    /// a press: for when the game jumps to another moment.
    pub fn clear(&mut self) {
        for button in Button::ALL {
            self.phase[button as usize] = self.held.contains(button).then_some(0);
        }
    }

    /// Stops every button at once, cutting short a press under way: for
    /// when the player's turbo inputs stop reaching the game while they
    /// may still be down.
    pub fn release(&mut self) {
        self.held = Buttons::NONE;
        self.phase = [None; Button::ALL.len()];
    }

    /// Whether a button `phase` frames into its pattern is pressed.
    fn pressed_at(&self, phase: u16) -> bool {
        phase < u16::from(self.frames)
    }

    /// Stops each button no longer held that is between presses.
    fn stop_released(&mut self) {
        for button in Button::ALL {
            let phase = self.phase[button as usize];
            let between_presses = phase.is_some_and(|phase| !self.pressed_at(phase));
            if !self.held.contains(button) && between_presses {
                self.phase[button as usize] = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(buttons: &[Button]) -> Buttons {
        buttons.iter().copied().collect()
    }

    /// Whether the game sees `button` in each of the next `frames` frames,
    /// on top of `base`.
    fn run(turbo: &mut Turbo, base: Input, button: Button, frames: usize) -> Vec<bool> {
        (0..frames)
            .map(|_| {
                let pressed = turbo.apply(base).buttons.contains(button);
                turbo.advance();
                pressed
            })
            .collect()
    }

    #[test]
    fn a_held_turbo_button_alternates_frames_pressed_and_released() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::A]), 2);
        assert_eq!(
            run(&mut turbo, Input::default(), Button::A, 9),
            [true, true, false, false, true, true, false, false, true]
        );
    }

    #[test]
    fn one_frame_turbo_is_not_stretched_by_a_minimum_hold() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::B]), 1);
        assert_eq!(
            run(&mut turbo, Input::default(), Button::B, 6),
            [true, false, true, false, true, false]
        );
    }

    #[test]
    fn the_default_presses_ten_times_a_second_at_sixty_frames() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::A]), DEFAULT_FRAMES);
        let seen = run(&mut turbo, Input::default(), Button::A, 60);
        let presses =
            seen.windows(2).filter(|pair| pair[1] && !pair[0]).count() + usize::from(seen[0]);
        assert_eq!(presses, 10);
    }

    #[test]
    fn a_tap_quicker_than_a_frame_presses_once_in_full() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::A]), 3);
        turbo.set(Buttons::NONE, 3);
        assert_eq!(
            run(&mut turbo, Input::default(), Button::A, 8),
            [true, true, true, false, false, false, false, false]
        );
    }

    #[test]
    fn letting_go_mid_press_finishes_it_and_between_presses_stops_at_once() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::A]), 3);
        run(&mut turbo, Input::default(), Button::A, 1);
        turbo.set(Buttons::NONE, 3);
        assert_eq!(
            run(&mut turbo, Input::default(), Button::A, 4),
            [true, true, false, false]
        );

        turbo.set(held(&[Button::A]), 3);
        run(&mut turbo, Input::default(), Button::A, 4);
        turbo.set(Buttons::NONE, 3);
        assert_eq!(run(&mut turbo, Input::default(), Button::A, 3), [false; 3]);
    }

    #[test]
    fn holding_again_while_finishing_keeps_the_rhythm() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::X]), 2);
        turbo.set(Buttons::NONE, 2);
        turbo.set(held(&[Button::X]), 2);
        assert_eq!(
            run(&mut turbo, Input::default(), Button::X, 6),
            [true, true, false, false, true, true]
        );
    }

    #[test]
    fn buttons_held_normally_stay_held() {
        let mut turbo = Turbo::default();
        let base = Input {
            buttons: held(&[Button::A, Button::Left]),
            ..Input::default()
        };
        turbo.set(held(&[Button::A, Button::B]), 1);
        let seen: Vec<Input> = (0..4)
            .map(|_| {
                let input = turbo.apply(base);
                turbo.advance();
                input
            })
            .collect();
        assert!(seen.iter().all(|input| input.buttons.contains(Button::A)));
        assert!(
            seen.iter()
                .all(|input| input.buttons.contains(Button::Left))
        );
        let b: Vec<bool> = seen.iter().map(|i| i.buttons.contains(Button::B)).collect();
        assert_eq!(b, [true, false, true, false]);
    }

    #[test]
    fn each_button_keeps_its_own_rhythm() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::A]), 2);
        run(&mut turbo, Input::default(), Button::A, 1);
        turbo.set(held(&[Button::A, Button::B]), 2);
        let seen: Vec<Input> = (0..4)
            .map(|_| {
                let input = turbo.apply(Input::default());
                turbo.advance();
                input
            })
            .collect();
        let a: Vec<bool> = seen.iter().map(|i| i.buttons.contains(Button::A)).collect();
        let b: Vec<bool> = seen.iter().map(|i| i.buttons.contains(Button::B)).collect();
        assert_eq!(a, [true, false, false, true]);
        assert_eq!(b, [true, true, false, false]);
    }

    #[test]
    fn clear_restarts_held_buttons_and_drops_finishing_ones() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::A, Button::B]), 2);
        run(&mut turbo, Input::default(), Button::A, 1);
        // B finishes its press; A is part way through its own.
        turbo.set(held(&[Button::A]), 2);
        turbo.clear();
        let input = turbo.apply(Input::default());
        assert!(input.buttons.contains(Button::A), "starts with a press");
        assert!(!input.buttons.contains(Button::B));
    }

    #[test]
    fn release_stops_held_and_finishing_presses_at_once() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::A, Button::B]), 3);
        // B finishes its press; A is still held.
        turbo.set(held(&[Button::A]), 3);
        turbo.release();
        assert_eq!(turbo.apply(Input::default()).buttons, Buttons::NONE);
        turbo.advance();
        assert_eq!(turbo.apply(Input::default()).buttons, Buttons::NONE);
    }

    #[test]
    fn zero_frames_counts_as_one() {
        let mut turbo = Turbo::default();
        turbo.set(held(&[Button::A]), 0);
        assert_eq!(
            run(&mut turbo, Input::default(), Button::A, 4),
            [true, false, true, false]
        );
    }
}
