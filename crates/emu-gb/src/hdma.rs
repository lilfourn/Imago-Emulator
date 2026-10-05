//! CGB VRAM DMA (Pan Docs, "VRAM DMA Transfers"): copies 16-byte blocks
//! into VRAM, either all at once (general-purpose DMA) or one block at the
//! start of each HBlank.
//!
//! This type only tracks registers and which block comes next; the bus
//! copies the bytes and stalls the CPU.

use serde::{Deserialize, Serialize};

/// One 16-byte block to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Block {
    /// Source address as the CPU sees it.
    pub(crate) source: u16,
    /// Destination address in 0x8000–0x9FF0.
    pub(crate) destination: u16,
}

/// What a write to HDMA5 (0xFF55) started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Started {
    /// Copy every block now, with the CPU halted.
    General,
    /// Copy one block per HBlank, or nothing (a stop or an HBlank start).
    Nothing,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Hdma {
    source: u16,
    /// Offset into VRAM, 0x0000–0x1FF0.
    destination: u16,
    /// Blocks still to copy.
    remaining: u8,
    hblank: bool,
}

impl Hdma {
    /// Whether an HBlank transfer is waiting for the next HBlank.
    pub(crate) fn hblank_active(&self) -> bool {
        self.hblank && self.remaining > 0
    }

    /// HDMA5 as read: blocks left minus one, with bit 7 clear while an
    /// HBlank transfer is active; 0xFF once finished.
    pub(crate) fn read_control(&self) -> u8 {
        match (self.remaining, self.hblank) {
            (0, _) => 0xFF,
            (remaining, true) => (remaining - 1) & 0x7F,
            (remaining, false) => 0x80 | ((remaining - 1) & 0x7F),
        }
    }

    pub(crate) fn write(&mut self, address: u16, value: u8) -> Started {
        match address {
            0xFF51 => self.source = (self.source & 0x00FF) | u16::from(value) << 8,
            0xFF52 => self.source = (self.source & 0xFF00) | u16::from(value & 0xF0),
            0xFF53 => self.destination = (self.destination & 0x00FF) | u16::from(value & 0x1F) << 8,
            0xFF54 => self.destination = (self.destination & 0xFF00) | u16::from(value & 0xF0),
            _ => {
                // Writing with bit 7 clear during an HBlank transfer stops it.
                if self.hblank_active() && value & 0x80 == 0 {
                    self.hblank = false;
                    return Started::Nothing;
                }
                self.remaining = (value & 0x7F) + 1;
                self.hblank = value & 0x80 != 0;
                if !self.hblank {
                    return Started::General;
                }
            }
        }
        Started::Nothing
    }

    /// Takes the next block, advancing both addresses.
    pub(crate) fn next_block(&mut self) -> Option<Block> {
        if self.remaining == 0 {
            return None;
        }
        let block = Block {
            source: self.source,
            destination: 0x8000 | self.destination,
        };
        self.source = self.source.wrapping_add(16);
        self.destination = (self.destination + 16) & 0x1FF0;
        self.remaining -= 1;
        Some(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> Hdma {
        let mut hdma = Hdma::default();
        for (address, value) in [
            (0xFF51, 0xC1),
            (0xFF52, 0x2F),
            (0xFF53, 0x81),
            (0xFF54, 0x0F),
        ] {
            hdma.write(address, value);
        }
        hdma
    }

    #[test]
    fn general_transfer_ignores_low_address_bits() {
        let mut hdma = configured();
        assert_eq!(hdma.write(0xFF55, 0x01), Started::General);
        assert_eq!(
            hdma.next_block(),
            Some(Block {
                source: 0xC120,
                destination: 0x8100
            })
        );
        assert_eq!(
            hdma.next_block(),
            Some(Block {
                source: 0xC130,
                destination: 0x8110
            })
        );
        assert_eq!(hdma.next_block(), None);
        assert_eq!(hdma.read_control(), 0xFF);
    }

    #[test]
    fn hblank_transfer_reports_progress_and_can_stop() {
        let mut hdma = configured();
        assert_eq!(hdma.write(0xFF55, 0x82), Started::Nothing);
        assert!(hdma.hblank_active());
        hdma.next_block();
        assert_eq!(hdma.read_control(), 0x01);
        hdma.write(0xFF55, 0x00);
        assert!(!hdma.hblank_active());
        assert_eq!(hdma.read_control(), 0x81);
    }
}
