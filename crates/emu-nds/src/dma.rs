//! One CPU's four DMA channels (GBATEK, "DS DMA Transfers").
//!
//! Like the GBA's, but each channel's control is one 32-bit register with
//! the word count in its low bits (21 bits on the ARM9, 14 or 16 on the
//! ARM7), and there are more start modes. Cartridge and geometry-FIFO modes
//! move a few units per trigger until the count runs out. This type holds
//! the registers and decides what to copy; the bus performs the copies
//! between CPU instructions.

use serde::{Deserialize, Serialize};

/// Which CPU's controller: they decode start modes and counts differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Owner {
    Arm9,
    Arm7,
}

/// When an enabled channel starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Timing {
    Immediate,
    VBlank,
    /// ARM9 only.
    HBlank,
    /// ARM9 only: the start of each displayed line.
    DisplayStart,
    /// ARM9 only.
    MainMemoryDisplay,
    /// A word of cartridge data is ready: one unit per trigger.
    Cart,
    GbaCart,
    /// ARM9 only: the geometry FIFO is less than half full: 112 units per
    /// trigger.
    GeometryFifo,
    /// ARM7 only.
    Wireless,
}

/// One run of a channel, as the bus should perform it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Transfer {
    pub(crate) source: u32,
    pub(crate) destination: u32,
    pub(crate) count: u32,
    pub(crate) words: bool,
    pub(crate) source_step: i32,
    pub(crate) destination_step: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Channel {
    source: u32,
    destination: u32,
    /// DMAxCNT: count in the low bits, control in the high bits.
    control: u32,
    current_source: u32,
    current_destination: u32,
    /// Units left before the channel finishes.
    remaining: u32,
    pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Dma {
    owner: Owner,
    channels: [Channel; 4],
    /// DMA fill data, 0x040000E0–0x040000EC (ARM9 only).
    pub(crate) fill: [u32; 4],
}

/// Address step for a control field: increment, decrement, fixed,
/// increment-and-reload.
fn step(field: u32, size: i32) -> i32 {
    match field & 3 {
        0 | 3 => size,
        1 => -size,
        _ => 0,
    }
}

impl Channel {
    fn enabled(&self) -> bool {
        self.control & (1 << 31) != 0
    }

    fn repeat(&self) -> bool {
        self.control & (1 << 25) != 0
    }

    fn words(&self) -> bool {
        self.control & (1 << 26) != 0
    }
}

impl Dma {
    pub(crate) fn new(owner: Owner) -> Self {
        Self {
            owner,
            channels: Default::default(),
            fill: [0; 4],
        }
    }

    fn timing(&self, index: usize) -> Timing {
        let control = self.channels[index].control;
        match self.owner {
            Owner::Arm9 => match (control >> 27) & 7 {
                0 => Timing::Immediate,
                1 => Timing::VBlank,
                2 => Timing::HBlank,
                3 => Timing::DisplayStart,
                4 => Timing::MainMemoryDisplay,
                5 => Timing::Cart,
                6 => Timing::GbaCart,
                _ => Timing::GeometryFifo,
            },
            Owner::Arm7 => match (control >> 28) & 3 {
                0 => Timing::Immediate,
                1 => Timing::VBlank,
                2 => Timing::Cart,
                _ if index.is_multiple_of(2) => Timing::Wireless,
                _ => Timing::GbaCart,
            },
        }
    }

    /// The word count in the control register; zero means the maximum.
    fn count(&self, index: usize) -> u32 {
        let bits = match (self.owner, index) {
            (Owner::Arm9, _) => 21,
            (Owner::Arm7, 3) => 16,
            (Owner::Arm7, _) => 14,
        };
        let max = 1 << bits;
        match self.channels[index].control & (max - 1) {
            0 => max,
            count => count,
        }
    }

    fn address_mask(&self, index: usize, destination: bool) -> u32 {
        match (self.owner, index, destination) {
            (Owner::Arm9, _, _) | (Owner::Arm7, 3, true) | (Owner::Arm7, 1..=3, false) => {
                0x0FFF_FFFE
            }
            _ => 0x07FF_FFFE,
        }
    }

    /// Reads the 32-bit register at `offset` from 0x040000B0.
    pub(crate) fn read(&self, offset: u32) -> u32 {
        let index = (offset / 12) as usize;
        match (self.channels.get(index), offset % 12) {
            (Some(channel), 0) => channel.source,
            (Some(channel), 4) => channel.destination,
            (Some(channel), _) => channel.control,
            (None, _) => 0,
        }
    }

    /// Writes the 32-bit register at `offset` from 0x040000B0.
    pub(crate) fn write(&mut self, offset: u32, value: u32) {
        let index = (offset / 12) as usize;
        if index >= 4 {
            return;
        }
        match offset % 12 {
            0 => self.channels[index].source = value,
            4 => self.channels[index].destination = value,
            _ => self.write_control(index, value),
        }
    }

    fn write_control(&mut self, index: usize, value: u32) {
        let was_enabled = self.channels[index].enabled();
        self.channels[index].control = value;
        let channel = &self.channels[index];
        if !channel.enabled() {
            self.channels[index].pending = false;
            return;
        }
        if was_enabled {
            return;
        }
        let source = channel.source & self.address_mask(index, false);
        let destination = channel.destination & self.address_mask(index, true);
        let remaining = self.count(index);
        let immediate = self.timing(index) == Timing::Immediate;
        let channel = &mut self.channels[index];
        channel.current_source = source;
        channel.current_destination = destination;
        channel.remaining = remaining;
        channel.pending = immediate;
    }

    /// Starts enabled channels waiting for `timing`.
    pub(crate) fn trigger(&mut self, timing: Timing) {
        for index in 0..4 {
            if self.channels[index].enabled() && self.timing(index) == timing {
                self.channels[index].pending = true;
            }
        }
    }

    /// Whether any enabled channel waits for `timing`.
    #[cfg(test)]
    pub(crate) fn waiting_for(&self, timing: Timing) -> bool {
        (0..4).any(|index| self.channels[index].enabled() && self.timing(index) == timing)
    }

    /// The highest-priority channel waiting to run.
    pub(crate) fn next_pending(&self) -> Option<usize> {
        self.channels.iter().position(|channel| channel.pending)
    }

    /// What channel `index` should copy now.
    pub(crate) fn transfer(&self, index: usize) -> Transfer {
        let channel = &self.channels[index];
        let words = channel.words();
        let size = if words { 4 } else { 2 };
        let per_trigger = match self.timing(index) {
            Timing::Cart => 1,
            Timing::GeometryFifo => 112,
            _ => u32::MAX,
        };
        Transfer {
            source: channel.current_source,
            destination: channel.current_destination,
            count: channel.remaining.min(per_trigger),
            words,
            source_step: step(channel.control >> 23, size),
            destination_step: step(channel.control >> 21, size),
        }
    }

    /// Ends channel `index`'s run of `count` units with the addresses it
    /// reached. Returns whether it requests its interrupt.
    pub(crate) fn finish(
        &mut self,
        index: usize,
        count: u32,
        source: u32,
        destination: u32,
    ) -> bool {
        let timing = self.timing(index);
        let full_count = self.count(index);
        let channel = &mut self.channels[index];
        channel.pending = false;
        channel.current_source = source;
        channel.current_destination = destination;
        channel.remaining = channel.remaining.saturating_sub(count);
        if channel.remaining > 0 {
            // Cartridge and geometry modes wait for their next trigger.
            return false;
        }
        if channel.repeat() && timing != Timing::Immediate {
            channel.remaining = full_count;
            if (channel.control >> 21) & 3 == 3 {
                channel.current_destination = channel.destination & 0x0FFF_FFFE;
            }
        } else {
            channel.control &= !(1 << 31);
        }
        channel.control & (1 << 30) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENABLE: u32 = 1 << 31;
    const WORDS: u32 = 1 << 26;

    #[test]
    fn immediate_transfers_run_once() {
        let mut dma = Dma::new(Owner::Arm9);
        dma.write(0, 0x0200_0000);
        dma.write(4, 0x0300_0000);
        dma.write(8, ENABLE | WORDS | 16);
        assert_eq!(dma.next_pending(), Some(0));
        let transfer = dma.transfer(0);
        assert_eq!(
            (transfer.count, transfer.words, transfer.source_step),
            (16, true, 4)
        );
        assert!(!dma.finish(0, 16, 0x0200_0040, 0x0300_0040));
        assert_eq!(dma.read(8) & ENABLE, 0);
    }

    #[test]
    fn arm9_counts_have_21_bits_and_arm7_14() {
        let mut arm9 = Dma::new(Owner::Arm9);
        arm9.write(8, ENABLE | 0x1F_FFFF);
        assert_eq!(arm9.transfer(0).count, 0x1F_FFFF);
        let mut arm7 = Dma::new(Owner::Arm7);
        arm7.write(8, ENABLE); // zero means the maximum
        assert_eq!(arm7.transfer(0).count, 0x4000);
    }

    #[test]
    fn cartridge_mode_moves_one_word_per_trigger() {
        let mut dma = Dma::new(Owner::Arm9);
        dma.write(0, 0x0410_0010);
        dma.write(8, ENABLE | WORDS | 5 << 27 | 2 << 23 | 1 << 30 | 2);
        assert_eq!(dma.next_pending(), None);
        dma.trigger(Timing::Cart);
        assert_eq!(dma.transfer(0).count, 1);
        assert_eq!(dma.transfer(0).source_step, 0);
        assert!(!dma.finish(0, 1, 0x0410_0010, 0x0200_0004));
        dma.trigger(Timing::Cart);
        assert!(dma.finish(0, 1, 0x0410_0010, 0x0200_0008), "IRQ at the end");
        assert_eq!(dma.read(8) & ENABLE, 0);
    }

    #[test]
    fn arm7_modes_decode_from_two_bits() {
        let mut dma = Dma::new(Owner::Arm7);
        dma.write(12 * 2 + 8, ENABLE | 1 << 28);
        assert!(dma.waiting_for(Timing::VBlank));
        dma.write(12 * 2 + 8, 0);
        dma.write(12 * 2 + 8, ENABLE | 2 << 28);
        assert!(dma.waiting_for(Timing::Cart));
    }
}
