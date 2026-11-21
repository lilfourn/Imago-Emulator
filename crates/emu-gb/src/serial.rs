//! The serial port (Pan Docs, "Serial Data Transfer").
//!
//! With no link partner, a transfer started on the internal clock shifts in
//! 0xFF and finishes after 8 bits at 8192 Hz (or 262 kHz with the CGB's
//! fast clock). The latest bytes sent are recorded so test ROMs that report
//! over serial (Blargg's, Mooneye's) can be checked.
//!
//! The internal clock is divided from the same counter as DIV, so its
//! edges keep their phase from power-on rather than from the write that
//! starts a transfer: the first bit can take anything up to a whole bit
//! time (Mooneye `serial/boot_sclk_align`).
//!
//! For link play, a transfer this side clocks offers its byte through
//! [`Serial::take_outgoing`]; the other side answers with
//! [`Serial::receive`] if it is waiting on the external clock.

use serde::{Deserialize, Serialize};

use crate::interrupts;
use crate::model::Model;

/// Clock cycles per bit at 8192 Hz.
const BIT_CLOCKS: u16 = 512;
/// Clock cycles per bit with the CGB fast clock (SC bit 1).
const FAST_BIT_CLOCKS: u16 = 16;
/// How many of the latest bytes sent [`Serial::sent`] keeps at least:
/// more than any test ROM's report.
const SENT_KEPT: usize = 4096;

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
    /// The latest bytes the game has sent, between [`SENT_KEPT`] and
    /// twice that once it has sent that many. Not machine state.
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

    /// The latest bytes sent, oldest first: at least the last
    /// [`SENT_KEPT`].
    pub(crate) fn sent(&self) -> &[u8] {
        &self.sent
    }

    /// Records a byte sent, dropping the oldest half once the record is
    /// full so it never grows past twice [`SENT_KEPT`].
    fn record_sent(&mut self, byte: u8) {
        if self.sent.len() == 2 * SENT_KEPT {
            self.sent.drain(..SENT_KEPT);
        }
        self.sent.push(byte);
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

    /// Writes SB or SC. `counter` is the timer's internal counter, which
    /// the internal serial clock is divided from.
    pub(crate) fn write(&mut self, address: u16, value: u8, counter: u16) {
        if address == 0xFF01 {
            self.data = value;
            return;
        }
        self.control = value & self.control_mask();
        if value & 0x81 == 0x81 {
            self.record_sent(self.data);
            self.outgoing = Some(self.data);
            self.incoming = 0xFF;
            let bit_clocks = if self.control & 0x02 != 0 {
                FAST_BIT_CLOCKS
            } else {
                BIT_CLOCKS
            };
            self.remaining = transfer_cycles(counter, bit_clocks);
        }
    }
}

/// M-cycles until a transfer started now on the internal clock finishes:
/// to the clock's next falling edge, plus seven more bit times. The counter
/// advances four clock cycles per M-cycle. The serial clock's edges fall
/// when the counter, plus one M-cycle, crosses a multiple of the bit time:
/// one M-cycle ahead of DIV, as measured against DIV's own phase by
/// Mooneye's `boot_div` and `serial/boot_sclk_align`.
fn transfer_cycles(counter: u16, bit_clocks: u16) -> u16 {
    let phase = (counter.wrapping_add(4)) % bit_clocks;
    let to_first_edge = (bit_clocks - phase).div_ceil(4);
    to_first_edge + 7 * (bit_clocks / 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfers_end_on_the_serial_clock_not_eight_bits_after_the_write() {
        // The edge is one M-cycle away: barely more than seven bit times.
        assert_eq!(transfer_cycles(0x01F8, BIT_CLOCKS), 1 + 7 * 128);
        // Just past an edge: a whole eight bit times.
        assert_eq!(transfer_cycles(0x01FC, BIT_CLOCKS), 8 * 128);
        assert_eq!(transfer_cycles(0x0000, BIT_CLOCKS), 127 + 7 * 128);
        assert_eq!(transfer_cycles(0x000C, FAST_BIT_CLOCKS), 4 + 7 * 4);
    }

    #[test]
    fn a_transfer_without_a_partner_shifts_in_ff_and_interrupts() {
        let mut serial = Serial::new(Model::Dmg);
        let mut interrupt_flag = 0;
        serial.write(0xFF01, 0x42, 0);
        serial.write(0xFF02, 0x81, 0);
        // From counter 0 the first edge is 127 M-cycles away, then 7 bits.
        for _ in 0..127 + 7 * 128 - 1 {
            serial.tick(&mut interrupt_flag);
        }
        assert_eq!((serial.read(0xFF02), interrupt_flag), (0xFF, 0));
        serial.tick(&mut interrupt_flag);
        assert_eq!(serial.read(0xFF01), 0xFF);
        assert_eq!(serial.read(0xFF02), 0x7F);
        assert_eq!(interrupt_flag, interrupts::SERIAL);
        assert_eq!(serial.sent(), [0x42]);
    }

    #[test]
    fn the_record_of_bytes_sent_keeps_the_latest_and_stays_bounded() {
        let mut serial = Serial::new(Model::Dmg);
        for index in 0..5 * SENT_KEPT {
            serial.write(0xFF01, index as u8, 0);
            serial.write(0xFF02, 0x81, 0);
            assert!(serial.sent().len() <= 2 * SENT_KEPT);
        }
        let sent = serial.sent();
        assert!(sent.len() >= SENT_KEPT);
        let latest = (4 * SENT_KEPT..5 * SENT_KEPT).map(|index| index as u8);
        assert!(sent[sent.len() - SENT_KEPT..].iter().copied().eq(latest));
    }
}
