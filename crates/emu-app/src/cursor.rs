//! The gamepad's cursor on the touchscreen, for "touch at cursor".
//!
//! A stick moves the cursor and a button touches the screen where it
//! points, so DS games play without a mouse. The position is kept in
//! touchscreen pixels, so it stays on the same spot when the window or the
//! layout changes.

use emu_core::{ScreenSize, TouchPoint};

/// Touchscreen pixels the cursor crosses per second with the stick held
/// fully over.
pub const SPEED: f32 = 192.0;

/// A point on the touchscreen, in pixels from its top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    x: f32,
    y: f32,
}

impl Cursor {
    /// The middle of a `size` screen.
    pub fn centre(size: ScreenSize) -> Self {
        Self {
            x: f32::from(size.width / 2),
            y: f32::from(size.height / 2),
        }
    }

    /// Moves by `(dx, dy)` pixels, staying on a `size` screen.
    pub fn move_by(&mut self, (dx, dy): (f32, f32), size: ScreenSize) {
        let last = |length: u16| f32::from(length.saturating_sub(1));
        self.x = (self.x + dx).clamp(0.0, last(size.width));
        self.y = (self.y + dy).clamp(0.0, last(size.height));
    }

    /// The pixel under the cursor.
    pub fn pixel(self) -> TouchPoint {
        // The position is clamped to the screen, so it fits in u16.
        TouchPoint {
            x: self.x as u16,
            y: self.y as u16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DS: ScreenSize = ScreenSize::new(256, 192);

    #[test]
    fn starts_in_the_middle() {
        assert_eq!(Cursor::centre(DS).pixel(), TouchPoint { x: 128, y: 96 });
    }

    #[test]
    fn small_moves_add_up() {
        let mut cursor = Cursor::centre(DS);
        for _ in 0..4 {
            cursor.move_by((0.25, -0.5), DS);
        }
        assert_eq!(cursor.pixel(), TouchPoint { x: 129, y: 94 });
    }

    #[test]
    fn stays_on_the_screen() {
        let mut cursor = Cursor::centre(DS);
        cursor.move_by((1000.0, 1000.0), DS);
        assert_eq!(cursor.pixel(), TouchPoint { x: 255, y: 191 });
        cursor.move_by((-1000.0, -1000.0), DS);
        assert_eq!(cursor.pixel(), TouchPoint { x: 0, y: 0 });
    }
}
