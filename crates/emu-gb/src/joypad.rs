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

    pub(crate) fn write(&mut self, value: u8) {
        self.select = value & 0x30;
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
        if before & !self.lines() != 0 {
            *interrupt_flag |= interrupts::JOYPAD;
        }
    }
}
