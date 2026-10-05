//! The serial port (Pan Docs, "Serial Data Transfer").
//!
//! With no link partner, a transfer started on the internal clock shifts in
//! 0xFF and finishes after 8 bits at 8192 Hz (or 262 kHz with the CGB's
//! fast clock). Bytes sent are recorded so test ROMs that report over
//! serial (Blargg's, Mooneye's) can be checked.
//!
//! For link play, a transfer this side clocks offers its byte through
//! [`Serial::take_outgoing`]; the other side answers with
//! [`Serial::receive`] if it is waiting on the external clock.

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
    /// The byte shifted in when the current transfer finishes; 0xFF when
    /// nothing is connected.
    incoming: u8,
    /// A transfer on the internal clock started and no partner has taken
    /// its byte yet.
    outgoing: Option<u8>,
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
            incoming: 0xFF,
            outgoing: None,
            sent: Vec::new(),
        }
    }

    /// The control bits that exist on this model: start, clock source, and
    /// on CGB the clock speed.
    fn control_mask(&self) -> u8 {
        if self.model == Model::Cgb { 0x83 } else { 0x81 }
    }

    /// The byte of a transfer this side clocks, and the M-cycles until it
    /// finishes, if no partner has taken it yet.
    pub(crate) fn take_outgoing(&mut self) -> Option<(u8, u16)> {
        self.outgoing.take().map(|byte| (byte, self.remaining))
    }

    /// Sets the byte shifted in when this side's transfer finishes.
    pub(crate) fn set_incoming(&mut self, byte: u8) {
        self.incoming = byte;
    }

    /// The partner's clock shifts `byte` in, finishing after `remaining`
    /// M-cycles. Returns the byte shifted out: this side's data if it was
    /// waiting on the external clock, otherwise 0xFF.
    pub(crate) fn receive(&mut self, byte: u8, remaining: u16) -> u8 {
        let waiting = self.control & 0x81 == 0x80 && self.remaining == 0;
        if !waiting {
            return 0xFF;
        }
        self.incoming = byte;
        self.remaining = remaining.max(1);
        self.data
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
            self.data = std::mem::replace(&mut self.incoming, 0xFF);
            self.outgoing = None;
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
            self.outgoing = Some(self.data);
            self.incoming = 0xFF;
            self.remaining = if self.control & 0x02 != 0 {
                FAST_TRANSFER_CYCLES
            } else {
                TRANSFER_CYCLES
            };
        }
    }
}
