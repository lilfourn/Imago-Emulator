//! The interrupt controller: IE, IF and IME (GBATEK, "GBA Interrupt
//! Control"), and how late the CPU sees a request.
//!
//! Nothing here happens at once. A request, or a write to IE, IF or IME,
//! changes the registers one cycle later: it goes to the `next_*` values,
//! which [`Interrupts::sync`] copies over when the bus's sync event runs.
//! HALT notices that an enabled interrupt is requested one cycle after
//! that, and the CPU's IRQ input follows two cycles after the registers.
//! So a timer that overflows at cycle T has its interrupt taken by the first
//! instruction that starts at T + 3 or later; the mGBA suite's timer IRQ
//! tests measure exactly that. The delays are NanoBoyAdvance's.

use serde::{Deserialize, Serialize};

/// Cycles from a request or register write to the registers changing.
pub(crate) const SYNC_DELAY: u64 = 1;
/// Cycles from the registers changing to HALT seeing the request.
pub(crate) const HALT_DELAY: u64 = 1;
/// Cycles from the registers changing to the CPU's IRQ input following.
pub(crate) const LINE_DELAY: u64 = 2;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Interrupts {
    /// IE, IF and IME as the CPU reads them.
    enable: u16,
    flag: u16,
    master: bool,
    /// What IE, IF and IME become at the next sync.
    next_enable: u16,
    next_flag: u16,
    next_master: bool,
    /// Whether an enabled interrupt is requested, as HALT sees it.
    requested: bool,
    /// The CPU's IRQ input.
    line: bool,
    /// The value the IRQ input is on its way to.
    line_target: bool,
}

/// What changed at a sync, for the bus to deliver after its delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Changes {
    /// HALT's view of "an enabled interrupt is requested", if it changes.
    pub(crate) requested: Option<bool>,
    /// The CPU's IRQ input, if it changes.
    pub(crate) line: Option<bool>,
}

impl Interrupts {
    /// IE.
    pub(crate) const fn enable(&self) -> u16 {
        self.enable
    }

    /// IF.
    pub(crate) const fn flag(&self) -> u16 {
        self.flag
    }

    /// IME.
    pub(crate) const fn master(&self) -> bool {
        self.master
    }

    /// The value the next write to IE starts from, for byte writes.
    pub(crate) const fn next_enable(&self) -> u16 {
        self.next_enable
    }

    /// Requests the interrupts in `bits`. The caller schedules a sync.
    pub(crate) fn request(&mut self, bits: u16) {
        self.next_flag |= bits;
    }

    /// Writes IE. The caller schedules a sync.
    pub(crate) fn write_enable(&mut self, value: u16) {
        self.next_enable = value & 0x3FFF;
    }

    /// Writes 1s to IF, acknowledging those interrupts. The caller
    /// schedules a sync.
    pub(crate) fn acknowledge(&mut self, bits: u16) {
        self.next_flag &= !bits;
    }

    /// Writes IME. The caller schedules a sync.
    pub(crate) fn write_master(&mut self, on: bool) {
        self.next_master = on;
    }

    /// Copies the written values into the registers and returns what HALT
    /// and the CPU must learn, after [`HALT_DELAY`] and [`LINE_DELAY`].
    pub(crate) fn sync(&mut self) -> Changes {
        self.enable = self.next_enable;
        self.flag = self.next_flag;
        self.master = self.next_master;
        let requested = self.enable & self.flag != 0;
        let line = self.master && requested;
        let line_changes = line != self.line_target;
        self.line_target = line;
        Changes {
            requested: (requested != self.requested).then_some(requested),
            line: line_changes.then_some(line),
        }
    }

    /// Whether an enabled interrupt is requested, as HALT sees it.
    pub(crate) const fn requested(&self) -> bool {
        self.requested
    }

    pub(crate) fn set_requested(&mut self, requested: bool) {
        self.requested = requested;
    }

    /// The CPU's IRQ input.
    pub(crate) const fn line(&self) -> bool {
        self.line
    }

    pub(crate) fn set_line(&mut self, line: bool) {
        self.line = line;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_reach_the_registers_at_the_sync() {
        let mut interrupts = Interrupts::default();
        interrupts.write_enable(0x0008);
        interrupts.write_master(true);
        interrupts.request(0x0008);
        assert_eq!(interrupts.flag(), 0);
        let changes = interrupts.sync();
        assert_eq!((interrupts.enable(), interrupts.flag()), (0x0008, 0x0008));
        assert_eq!(
            changes,
            Changes {
                requested: Some(true),
                line: Some(true)
            }
        );
        // The bus delivers those later.
        assert!(!interrupts.line() && !interrupts.requested());
        interrupts.set_line(true);
        assert!(interrupts.line());
    }

    #[test]
    fn a_line_already_on_its_way_is_not_sent_twice() {
        let mut interrupts = Interrupts::default();
        interrupts.write_enable(1);
        interrupts.write_master(true);
        interrupts.request(1);
        assert_eq!(interrupts.sync().line, Some(true));
        interrupts.request(1);
        assert_eq!(interrupts.sync().line, None);
        interrupts.acknowledge(1);
        assert_eq!(interrupts.sync().line, Some(false));
    }

    #[test]
    fn without_ime_halt_wakes_but_the_line_stays_low() {
        let mut interrupts = Interrupts::default();
        interrupts.write_enable(1);
        interrupts.request(1);
        let changes = interrupts.sync();
        assert_eq!(changes.requested, Some(true));
        assert_eq!(changes.line, None);
    }
}
