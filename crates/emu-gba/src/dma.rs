//! The four DMA channels (GBATEK, "GBA DMA Transfers").
//!
//! This type holds the registers and decides what to copy; the bus performs
//! the copies, paying the usual wait states, between CPU instructions.

use serde::{Deserialize, Serialize};

/// When an enabled channel starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Timing {
    Immediate,
    VBlank,
    HBlank,
    /// Sound FIFO refill (channels 1 and 2) or video capture (channel 3).
    Special,
}

/// One transfer, as the bus should run it.
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
    count: u16,
    control: u16,
    /// Addresses and count latched when the channel was enabled (the count
    /// again at each repeat).
    current_source: u32,
    current_destination: u32,
    current_count: u32,
    pending: bool,
    /// The last value the channel moved, as a word (a halfword in both
    /// halves). Reads from the BIOS region, where nothing answers a DMA,
    /// copy it again.
    latch: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Dma {
    channels: [Channel; 4],
}

/// FIFO A and B, which channels 1 and 2 feed in sound mode.
const FIFO_ADDRESSES: [u32; 2] = [0x0400_00A0, 0x0400_00A4];

impl Channel {
    fn enabled(&self) -> bool {
        self.control & 0x8000 != 0
    }

    /// Bytes per unit: 4 for words, 2 for halfwords.
    fn unit(&self) -> u32 {
        if self.control & (1 << 10) != 0 { 4 } else { 2 }
    }

    /// Units to move, from the count register: zero means the maximum,
    /// 0x4000 (0x10000 for channel 3).
    fn count_for(&self, index: usize) -> u32 {
        let max = if index == 3 { 0x1_0000 } else { 0x4000 };
        match u32::from(self.count) & (max - 1) {
            0 => max,
            count => count,
        }
    }

    fn timing(&self) -> Timing {
        match (self.control >> 12) & 3 {
            0 => Timing::Immediate,
            1 => Timing::VBlank,
            2 => Timing::HBlank,
            _ => Timing::Special,
        }
    }
}

/// Address step for a control field: increment, decrement, fixed,
/// increment-and-reload.
fn step(field: u16, size: i32) -> i32 {
    match field & 3 {
        0 | 3 => size,
        1 => -size,
        _ => 0,
    }
}

impl Dma {
    /// Writes a register at `offset` from 0x040000B0.
    pub(crate) fn write(&mut self, offset: u32, value: u16) {
        let index = (offset / 12) as usize;
        let Some(channel) = self.channels.get_mut(index) else {
            return;
        };
        match offset % 12 {
            0 => channel.source = (channel.source & 0xFFFF_0000) | u32::from(value),
            2 => channel.source = (channel.source & 0xFFFF) | u32::from(value) << 16,
            4 => channel.destination = (channel.destination & 0xFFFF_0000) | u32::from(value),
            6 => channel.destination = (channel.destination & 0xFFFF) | u32::from(value) << 16,
            8 => channel.count = value,
            _ => {
                let was_enabled = channel.enabled();
                // Channel 3 alone may use the Game Pak DRQ bit.
                channel.control = if index == 3 { value } else { value & 0xF7FF };
                if !was_enabled && channel.enabled() {
                    // Addresses start aligned to the unit size.
                    let align = !(channel.unit() - 1);
                    let source_mask = if index == 0 { 0x07FF_FFFF } else { 0x0FFF_FFFF };
                    let destination_mask = if index == 3 { 0x0FFF_FFFF } else { 0x07FF_FFFF };
                    channel.current_source = channel.source & source_mask & align;
                    channel.current_destination = channel.destination & destination_mask & align;
                    channel.current_count = channel.count_for(index);
                    channel.pending = channel.timing() == Timing::Immediate;
                }
                if !channel.enabled() {
                    channel.pending = false;
                }
            }
        }
    }

    /// Reads a register at `offset` from 0x040000B0. Only the control
    /// registers can be read, without their five unused bits; the count
    /// registers read as 0. `None` for the write-only addresses and the
    /// unused space after the channels, which read as open bus.
    pub(crate) fn read(&self, offset: u32) -> Option<u16> {
        let index = (offset / 12) as usize;
        match (self.channels.get(index), offset % 12) {
            (Some(channel), 10) => Some(channel.control & 0xFFE0),
            (Some(_), 8) => Some(0),
            _ => None,
        }
    }

    /// Starts enabled channels waiting for `timing` (VBlank or HBlank).
    pub(crate) fn trigger(&mut self, timing: Timing) {
        for channel in &mut self.channels {
            if channel.enabled() && channel.timing() == timing {
                channel.pending = true;
            }
        }
    }

    /// Starts a sound-mode channel feeding FIFO `fifo` (0 for A, 1 for B).
    pub(crate) fn trigger_fifo(&mut self, fifo: usize) {
        for channel in &mut self.channels[1..3] {
            if channel.enabled()
                && channel.timing() == Timing::Special
                && channel.current_destination == FIFO_ADDRESSES[fifo]
            {
                channel.pending = true;
            }
        }
    }

    /// The highest-priority channel waiting to run.
    pub(crate) fn next_pending(&self) -> Option<usize> {
        self.channels.iter().position(|channel| channel.pending)
    }

    /// What channel `index` should copy now.
    pub(crate) fn transfer(&self, index: usize) -> Transfer {
        let channel = &self.channels[index];
        if (index == 1 || index == 2) && channel.timing() == Timing::Special {
            // Sound FIFO: four words to a fixed address.
            return Transfer {
                source: channel.current_source,
                destination: channel.current_destination,
                count: 4,
                words: true,
                source_step: step(channel.control >> 7, 4),
                destination_step: 0,
            };
        }
        let words = channel.control & (1 << 10) != 0;
        let size = channel.unit() as i32;
        // The Game Pak's ROM only streams forwards: a ROM source increments
        // whatever the control says.
        let in_rom = (0x0800_0000..0x0E00_0000).contains(&channel.current_source);
        let source_step = if in_rom {
            size
        } else {
            step(channel.control >> 7, size)
        };
        Transfer {
            source: channel.current_source,
            destination: channel.current_destination,
            count: channel.current_count,
            words,
            source_step,
            destination_step: step(channel.control >> 5, size),
        }
    }

    /// Channel `index`'s latch: the last value it moved.
    pub(crate) fn latch(&self, index: usize) -> u32 {
        self.channels[index].latch
    }

    pub(crate) fn set_latch(&mut self, index: usize, value: u32) {
        self.channels[index].latch = value;
    }

    /// Ends channel `index`'s transfer with the addresses it reached.
    /// Returns whether it requests its interrupt.
    pub(crate) fn finish(&mut self, index: usize, source: u32, destination: u32) -> bool {
        let channel = &mut self.channels[index];
        channel.pending = false;
        channel.current_source = source;
        let fifo = (index == 1 || index == 2) && channel.timing() == Timing::Special;
        if !fifo {
            channel.current_destination = destination;
        }
        let repeat = channel.control & (1 << 9) != 0 && channel.timing() != Timing::Immediate;
        if repeat {
            channel.current_count = channel.count_for(index);
            if (channel.control >> 5) & 3 == 3 {
                channel.current_destination = channel.destination;
            }
        } else {
            channel.control &= !0x8000;
        }
        channel.control & (1 << 14) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configure(
        dma: &mut Dma,
        index: u32,
        source: u32,
        destination: u32,
        count: u16,
        control: u16,
    ) {
        let base = index * 12;
        dma.write(base, source as u16);
        dma.write(base + 2, (source >> 16) as u16);
        dma.write(base + 4, destination as u16);
        dma.write(base + 6, (destination >> 16) as u16);
        dma.write(base + 8, count);
        dma.write(base + 10, control);
    }

    #[test]
    fn immediate_transfer_runs_once_and_disables() {
        let mut dma = Dma::default();
        configure(&mut dma, 3, 0x0200_0000, 0x0600_0000, 8, 0x8400);
        assert_eq!(dma.next_pending(), Some(3));
        let transfer = dma.transfer(3);
        assert_eq!(
            (transfer.count, transfer.words, transfer.destination_step),
            (8, true, 4)
        );
        assert!(!dma.finish(3, 0x0200_0020, 0x0600_0020));
        assert_eq!(dma.read(3 * 12 + 10), Some(0x0400));
    }

    #[test]
    fn hblank_repeat_waits_for_its_trigger_and_reloads() {
        let mut dma = Dma::default();
        // Repeat, HBlank, destination increment-and-reload, IRQ.
        configure(&mut dma, 0, 0x0300_0000, 0x0400_0040, 1, 0xE260);
        assert_eq!(dma.next_pending(), None);
        dma.trigger(Timing::HBlank);
        assert_eq!(dma.next_pending(), Some(0));
        assert!(dma.finish(0, 0x0300_0002, 0x0400_0042));
        assert_eq!(dma.transfer(0).destination, 0x0400_0040);
    }

    #[test]
    fn the_count_is_latched_at_enable_and_reloaded_at_each_repeat() {
        let mut dma = Dma::default();
        // VBlank, repeat, halfwords, count 2.
        configure(&mut dma, 3, 0x0200_0000, 0x0300_0000, 2, 0x9200);
        dma.write(3 * 12 + 8, 4);
        assert_eq!(dma.transfer(3).count, 2);
        dma.finish(3, 0x0200_0004, 0x0300_0004);
        assert_eq!(dma.transfer(3).count, 4);
    }

    #[test]
    fn addresses_start_aligned_and_rom_sources_always_increment() {
        let mut dma = Dma::default();
        // Words, source and destination fixed.
        configure(&mut dma, 1, 0x0800_0003, 0x0300_0002, 1, 0x8540);
        let transfer = dma.transfer(1);
        assert_eq!(
            (transfer.source, transfer.destination),
            (0x0800_0000, 0x0300_0000)
        );
        assert_eq!((transfer.source_step, transfer.destination_step), (4, 0));
    }

    #[test]
    fn zero_count_means_the_maximum() {
        let mut dma = Dma::default();
        configure(&mut dma, 0, 0, 0, 0, 0x8000);
        assert_eq!(dma.transfer(0).count, 0x4000);
        configure(&mut dma, 3, 0, 0, 0, 0x8000);
        assert_eq!(dma.transfer(3).count, 0x1_0000);
    }
}
