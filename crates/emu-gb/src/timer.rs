//! DIV and TIMA (Pan Docs, "Timer and Divider Registers"; "Timer
//! obscure behaviour").
//!
//! DIV is the top byte of a 16-bit counter that advances every clock
//! cycle. TIMA counts falling edges of one counter bit, chosen by TAC and
//! gated by its enable bit, so writes to DIV or TAC can also tick TIMA.

use serde::{Deserialize, Serialize};

use crate::interrupts;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Timer {
    counter: u16,
    tima: u8,
    tma: u8,
    tac: u8,
    /// TIMA overflowed during the last M-cycle; it reads 0x00 until the
    /// reload in the next one.
    overflowed: bool,
    /// TIMA was reloaded from TMA during the current M-cycle.
    reloading: bool,
}

impl Timer {
    /// The state the DMG boot ROM leaves.
    pub(crate) const fn after_boot() -> Self {
        Self {
            counter: 0xABCC,
            tima: 0,
            tma: 0,
            tac: 0,
            overflowed: false,
            reloading: false,
        }
    }

    /// The internal 16-bit counter; DIV is its high byte. The APU's frame
    /// sequencer is clocked from it.
    pub(crate) const fn counter(&self) -> u16 {
        self.counter
    }

    /// Advances one M-cycle.
    pub(crate) fn tick(&mut self, interrupt_flag: &mut u8) {
        self.reloading = false;
        if self.overflowed {
            self.overflowed = false;
            self.tima = self.tma;
            self.reloading = true;
            *interrupt_flag |= interrupts::TIMER;
        }
        let old = self.counter;
        self.counter = self.counter.wrapping_add(4);
        if self.signal(old) && !self.signal(self.counter) {
            self.increment();
        }
    }

    /// The timer's input signal: the TAC-selected counter bit, ANDed with
    /// the enable bit.
    fn signal(&self, counter: u16) -> bool {
        let bit = match self.tac & 3 {
            0 => 9,
            1 => 3,
            2 => 5,
            _ => 7,
        };
        self.tac & 4 != 0 && counter & (1 << bit) != 0
    }

    fn increment(&mut self) {
        let (tima, overflow) = self.tima.overflowing_add(1);
        self.tima = tima;
        self.overflowed = overflow;
    }

    pub(crate) fn read(&self, address: u16) -> u8 {
        match address {
            0xFF04 => (self.counter >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            _ => self.tac | 0xF8,
        }
    }

    pub(crate) fn write(&mut self, address: u16, value: u8) {
        match address {
            0xFF04 => {
                let was_high = self.signal(self.counter);
                self.counter = 0;
                if was_high {
                    self.increment();
                }
            }
            0xFF05 => {
                // A write in the reload cycle loses to TMA; a write during
                // the overflow cycle cancels the reload and interrupt.
                if !self.reloading {
                    self.tima = value;
                    self.overflowed = false;
                }
            }
            0xFF06 => {
                self.tma = value;
                if self.reloading {
                    self.tima = value;
                }
            }
            _ => {
                let was_high = self.signal(self.counter);
                self.tac = value & 7;
                if was_high && !self.signal(self.counter) {
                    self.increment();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tima_counts_at_the_selected_rate_and_reloads() {
        let mut timer = Timer::after_boot();
        let mut interrupt_flag = 0;
        timer.write(0xFF04, 0);
        timer.write(0xFF06, 0xFE);
        timer.write(0xFF05, 0xFE);
        timer.write(0xFF07, 0b101); // enabled, every 16 clocks = 4 M-cycles
        for _ in 0..8 {
            timer.tick(&mut interrupt_flag);
        }
        // Two increments: 0xFE -> 0xFF -> overflow (reads 0x00).
        assert_eq!(timer.read(0xFF05), 0x00);
        assert_eq!(interrupt_flag, 0);
        timer.tick(&mut interrupt_flag);
        assert_eq!(timer.read(0xFF05), 0xFE);
        assert_eq!(interrupt_flag, interrupts::TIMER);
    }
}
