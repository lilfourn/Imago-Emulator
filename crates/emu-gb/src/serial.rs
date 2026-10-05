//! The serial port (Pan Docs, "Serial Data Transfer").
//!
//! With no link partner, a transfer started on the internal clock shifts in
//! 0xFF and finishes after 8 bits at 8192 Hz (or 262 kHz with the CGB's
//! fast clock). Bytes sent are recorded so test ROMs that report over
//! serial (Blargg's, Mooneye's) can be checked.

use serde::{Deserialize, Serialize};

use crate::interrupts;
use crate::model::Model;

/// M-cycles to shift 8 bits at 8192 Hz.
const TRANSFER_CYCLES: u16 = 1024;
/// M-cycles to shift 8 bits with the CGB fast clock (SC bit 1).
const FAST_TRANSFER_CYCLES: u16 = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Serial {
    model: Model,
    data: u8,
    control: u8,
    remaining: u16,
    /// Every byte the game has sent. Not machine state.
    #[serde(skip)]
    sent: Vec<u8>,
}

impl Serial {
    pub(crate) fn new(model: Model) -> Self {
        Self {
            model,
            data: 0,
            control: 0,
            remaining: 0,
            sent: Vec::new(),
        }
    }

    /// The control bits that exist on this model: start, clock source, and
    /// on CGB the clock speed.
    fn control_mask(&self) -> u8 {
        if self.model == Model::Cgb { 0x83 } else { 0x81 }
    }

    pub(crate) fn sent(&self) -> &[u8] {
        &self.sent
    }

    pub(crate) fn tick(&mut self, interrupt_flag: &mut u8) {
        if self.remaining == 0 {
            return;
        }
        self.remaining -= 1;
        if self.remaining == 0 {
            self.data = 0xFF;
            self.control &= 0x7F;
            *interrupt_flag |= interrupts::SERIAL;
        }
    }

    pub(crate) fn read(&self, address: u16) -> u8 {
        if address == 0xFF01 {
            self.data
        } else {
            self.control | !self.control_mask()
        }
    }

    pub(crate) fn write(&mut self, address: u16, value: u8) {
        if address == 0xFF01 {
            self.data = value;
            return;
        }
        self.control = value & self.control_mask();
        if value & 0x81 == 0x81 {
            self.sent.push(self.data);
            self.remaining = if self.control & 0x02 != 0 {
                FAST_TRANSFER_CYCLES
            } else {
                TRANSFER_CYCLES
            };
        }
    }
}
