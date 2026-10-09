//! The joypad register P1 (Pan Docs, "Joypad Input").

use emu_core::{Button, Buttons};
use serde::{Deserialize, Serialize};

use crate::interrupts;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Joypad {
    /// Bits 4 (directions) and 5 (actions) as written; 0 selects.
    select: u8,
    /// Held action buttons: A, B, Select, Start in bits 0 to 3.
    actions: u8,
    /// Held directions: Right, Left, Up, Down in bits 0 to 3.
    directions: u8,
}

impl Joypad {
    /// Bits 0 to 3 as the CPU reads them: 0 means pressed.
    fn lines(&self) -> u8 {
        let mut lines = 0x0F;
        if self.select & 0x20 == 0 {
            lines &= !self.actions;
        }
        if self.select & 0x10 == 0 {
            lines &= !self.directions;
        }
        lines
    }

    pub(crate) fn read(&self) -> u8 {
        0xC0 | self.select | self.lines()
    }

    /// Writes the select bits, requesting the joypad interrupt if
    /// selecting a group with a button held pulled a line low.
    pub(crate) fn write(&mut self, value: u8, interrupt_flag: &mut u8) {
        let before = self.lines();
        self.select = value & 0x30;
        self.interrupt_on_fall(before, interrupt_flag);
    }

    /// Updates the held buttons, requesting the joypad interrupt if a
    /// selected line went low.
    pub(crate) fn set_buttons(&mut self, buttons: Buttons, interrupt_flag: &mut u8) {
        let before = self.lines();
        let bit = |button, mask| if buttons.contains(button) { mask } else { 0 };
        self.actions =
            bit(Button::A, 1) | bit(Button::B, 2) | bit(Button::Select, 4) | bit(Button::Start, 8);
        self.directions = bit(Button::Right, 1)
            | bit(Button::Left, 2)
            | bit(Button::Up, 4)
            | bit(Button::Down, 8);
        self.interrupt_on_fall(before, interrupt_flag);
    }

    /// Requests the joypad interrupt if a line that was `before` high is
    /// now low (Pan Docs, "INT $60 — Joypad interrupt").
    fn interrupt_on_fall(&self, before: u8, interrupt_flag: &mut u8) {
        if before & !self.lines() != 0 {
            *interrupt_flag |= interrupts::JOYPAD;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selecting_a_group_with_a_button_held_interrupts() {
        let mut joypad = Joypad::default();
        let mut interrupt_flag = 0;
        let mut a = Buttons::default();
        a.set(Button::A, true);
        // Only the directions selected while A is pressed: no line falls.
        joypad.write(0x20, &mut interrupt_flag);
        joypad.set_buttons(a, &mut interrupt_flag);
        assert_eq!((joypad.read() & 0x0F, interrupt_flag), (0x0F, 0));
        // Selecting the actions pulls A's line low.
        joypad.write(0x10, &mut interrupt_flag);
        assert_eq!(
            (joypad.read() & 0x0F, interrupt_flag),
            (0x0E, interrupts::JOYPAD)
        );
        // Selecting both groups pulls no other line low.
        interrupt_flag = 0;
        joypad.write(0x00, &mut interrupt_flag);
        assert_eq!(interrupt_flag, 0);
    }
}
