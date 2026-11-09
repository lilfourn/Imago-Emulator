//! The Game Pak prefetch buffer (GBATEK, "GBA System Control", WAITCNT
//! bit 14).
//!
//! While the CPU runs code from the cartridge, the prefetch unit keeps
//! reading the opcodes after the last one fetched into an eight-halfword
//! buffer whenever the cartridge bus is free: during internal cycles and
//! accesses to other memory. A fetch of the oldest buffered opcode takes a
//! single cycle; a fetch of the opcode being read waits for that read to
//! finish; anything else is an ordinary cartridge access, after which the
//! unit starts again from the new address. Data accesses to the cartridge
//! and its save memory stop the unit. The behaviour follows
//! NanoBoyAdvance's model, which the mGBA suite's timing tests confirm.

use serde::{Deserialize, Serialize};

/// What a code fetch from the cartridge finds in the prefetch unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// The opcode is the oldest in the buffer: one cycle.
    Buffered,
    /// The opcode is being read, done in this many cycles.
    InFlight(u32),
    /// The unit does not have it: an ordinary cartridge access.
    Miss,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Prefetch {
    /// Whether the unit is following a stream of opcodes from the
    /// cartridge.
    active: bool,
    /// The oldest buffered opcode's address; the buffer holds `count`
    /// opcodes from there.
    head: u32,
    count: u32,
    /// Whether a read is in flight, of the opcode after the buffered ones,
    /// done in `countdown` cycles.
    fetching: bool,
    countdown: u32,
    /// Opcode size in bytes: 2 (Thumb) or 4 (ARM).
    width: u32,
    /// Cycles one sequential read of an opcode takes.
    duty: u32,
}

/// The buffer holds eight halfwords: eight Thumb opcodes or four ARM ones.
const CAPACITY_BYTES: u32 = 16;

impl Prefetch {
    /// Whether this, read from a save state, is a unit as
    /// [`Prefetch::start`] and its running leave it: opcodes of two or
    /// four bytes, no more than fill the buffer, and reads that take a
    /// cycle or more, the longest wait state 16.
    pub(crate) fn in_range(&self) -> bool {
        // As it is before the first code fetch from the cartridge.
        let unused = !self.active && !self.fetching && self.width == 0;
        let used = matches!(self.width, 2 | 4)
            && self.count * self.width <= CAPACITY_BYTES
            && (1..=64).contains(&self.duty)
            && (1..=self.duty).contains(&self.countdown);
        unused || used
    }

    /// The address of the opcode after the buffered ones.
    fn next(&self) -> u32 {
        self.head.wrapping_add(self.count * self.width)
    }

    fn full(&self) -> bool {
        self.count * self.width >= CAPACITY_BYTES
    }

    /// Lets `cycles` pass with the cartridge bus free. `enabled` is
    /// WAITCNT's prefetch bit: once it is clear, the read in flight
    /// finishes but no other starts.
    #[inline]
    pub(crate) fn run(&mut self, cycles: u32, enabled: bool) {
        if !(self.active && self.fetching) {
            return;
        }
        let mut cycles = cycles;
        while self.active && self.fetching && cycles >= self.countdown {
            cycles -= self.countdown;
            self.count += 1;
            self.fetching = enabled && !self.full();
            self.countdown = self.duty;
        }
        if self.active && self.fetching {
            self.countdown -= cycles;
        }
    }

    /// Looks up a code fetch of `address`.
    pub(crate) fn look_up(&self, address: u32) -> Lookup {
        if !self.active {
            Lookup::Miss
        } else if self.count > 0 && address == self.head {
            Lookup::Buffered
        } else if self.fetching && address == self.next() {
            Lookup::InFlight(self.countdown)
        } else {
            Lookup::Miss
        }
    }

    /// The CPU takes the oldest buffered opcode, or (after waiting for it)
    /// the one that was in flight; anything buffered before that is
    /// dropped. A unit that stopped because the buffer was full starts
    /// reading again if `enabled`.
    pub(crate) fn take(&mut self, address: u32, enabled: bool) {
        // Dividing by the width, a power of two, by shifting saves a
        // division on every prefetched fetch.
        let passed = address.wrapping_sub(self.head) >> self.width.trailing_zeros();
        let taken = (passed + 1).min(self.count);
        self.head = address.wrapping_add(self.width);
        self.count -= taken;
        if enabled && !self.fetching && !self.full() {
            self.fetching = true;
            self.countdown = self.duty;
        }
    }

    /// Starts streaming the opcodes after `address`, just fetched, each
    /// `width` bytes and taking `duty` cycles.
    pub(crate) fn start(&mut self, address: u32, width: u32, duty: u32) {
        *self = Self {
            active: true,
            head: address.wrapping_add(width),
            count: 0,
            fetching: true,
            countdown: duty,
            width,
            duty,
        };
    }

    /// Stops the unit for a data access to the cartridge. Returns whether
    /// the access waits a cycle for the unit's read to finish: it does when
    /// the CPU runs from the cartridge and a halfword read is in its last
    /// cycle.
    pub(crate) fn stop(&mut self, code_in_cartridge: bool) -> bool {
        let half_read_ending = self.fetching
            && (self.countdown == 1 || (self.width == 4 && self.countdown == self.duty / 2 + 1));
        let penalty = self.active && code_in_cartridge && half_read_ending;
        self.active = false;
        penalty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcodes_buffer_while_the_bus_is_free() {
        let mut prefetch = Prefetch::default();
        // Thumb at 0x08000000, two cycles per sequential halfword.
        prefetch.start(0x0800_0000, 2, 2);
        assert_eq!(prefetch.look_up(0x0800_0002), Lookup::InFlight(2));
        prefetch.run(5, true);
        assert_eq!(prefetch.look_up(0x0800_0002), Lookup::Buffered);
        prefetch.take(0x0800_0002, true);
        assert_eq!(prefetch.look_up(0x0800_0004), Lookup::Buffered);
        prefetch.take(0x0800_0004, true);
        assert_eq!(prefetch.look_up(0x0800_0006), Lookup::InFlight(1));
        assert_eq!(prefetch.look_up(0x0800_0010), Lookup::Miss);
    }

    #[test]
    fn the_buffer_holds_eight_halfwords() {
        let mut prefetch = Prefetch::default();
        prefetch.start(0x0800_0000, 2, 2);
        prefetch.run(100, true);
        assert_eq!(prefetch.count, 8);
        assert_eq!(prefetch.look_up(0x0800_0012), Lookup::Miss);
        // ARM: four words.
        prefetch.start(0x0800_0000, 4, 4);
        prefetch.run(100, true);
        assert_eq!(prefetch.count, 4);
    }

    #[test]
    fn disabling_lets_the_read_in_flight_finish() {
        let mut prefetch = Prefetch::default();
        prefetch.start(0x0800_0000, 2, 3);
        prefetch.run(10, false);
        assert_eq!(prefetch.count, 1);
        assert_eq!(prefetch.look_up(0x0800_0004), Lookup::Miss);
    }

    #[test]
    fn stopping_in_a_reads_last_cycle_costs_one() {
        let mut prefetch = Prefetch::default();
        prefetch.start(0x0800_0000, 2, 3);
        prefetch.run(2, true);
        assert!(prefetch.stop(true));
        prefetch.start(0x0800_0000, 2, 3);
        prefetch.run(2, true);
        assert!(!prefetch.stop(false));
        assert_eq!(prefetch.look_up(0x0800_0002), Lookup::Miss);
    }
}
