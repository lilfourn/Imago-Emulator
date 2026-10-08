//! The serial port (GBATEK, "GBA Communication Ports", "SIO Normal Mode",
//! "SIO Multi-Player Mode").
//!
//! Normal mode shifts 8 or 32 bits out on one console's clock and in from
//! the other's. Multi-player mode, which Pokémon uses for trades and
//! battles, has the parent start a transfer that collects one halfword from
//! every console into SIOMULTI0-3 on all of them. Alone, a console's SI pin
//! floats high, which makes it a child that cannot start a multi-player
//! transfer; a normal transfer on its own clock reads all ones.
//!
//! This module keeps one console's registers. The link cable itself is
//! [`crate::Linked`], which moves the data between two of them.

use serde::{Deserialize, Serialize};

const CYCLES_PER_SECOND: u64 = 16_777_216;
/// Multi-player baud rates, by SIOCNT bits 0-1.
const BAUD_RATES: [u64; 4] = [9600, 38_400, 57_600, 115_200];
/// Bits each console sends in a multi-player transfer: start, 16 data,
/// stop.
const MULTI_BITS_PER_UNIT: u64 = 18;

const START: u16 = 0x80;
const IRQ_ENABLE: u16 = 0x4000;

/// The transfer mode set by RCNT and SIOCNT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal8,
    Normal32,
    Multi,
    /// UART: no transfers emulated.
    Uart,
    /// JOY Bus or general-purpose (RCNT bit 15): no transfers emulated.
    Other,
}

/// A transfer one console started, waiting for the cable to deliver it.
/// The other console's side lasts the same `cycles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Outgoing {
    /// The parent started a multi-player transfer, sending `word`.
    Multi { word: u16, cycles: u64 },
    /// A normal transfer of `bits` bits on this console's clock.
    Normal { value: u32, bits: u32, cycles: u64 },
}

/// What a register write did to the transfer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    /// A transfer started that ends after this many cycles.
    Started(u64),
    /// The game stopped the transfer in progress.
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Sio {
    rcnt: u16,
    /// SIOCNT as written, with the busy bit kept by the transfer.
    control: u16,
    /// SIOMULTI0-3, or SIODATA32 in the first two.
    data: [u16; 4],
    /// SIOMLT_SEND, or SIODATA8.
    send: u16,
    /// Position on a cable: `None` alone, 0 the parent, 1-3 a child.
    position: Option<u8>,
    /// Consoles on the cable, this one included.
    units: u8,
    /// SD: every console on the cable is in multi-player mode.
    ready: bool,
    outgoing: Option<Outgoing>,
    /// What the data registers become when the transfer ends.
    incoming: [u16; 4],
}

impl Default for Sio {
    fn default() -> Self {
        Self {
            rcnt: 0,
            control: 0,
            data: [0; 4],
            send: 0,
            position: None,
            units: 1,
            ready: false,
            outgoing: None,
            incoming: [0xFFFF; 4],
        }
    }
}

impl Sio {
    fn mode(&self) -> Mode {
        if self.rcnt & 0x8000 != 0 {
            return Mode::Other;
        }
        match (self.control >> 12) & 3 {
            0 => Mode::Normal8,
            1 => Mode::Normal32,
            2 => Mode::Multi,
            _ => Mode::Uart,
        }
    }

    fn busy(&self) -> bool {
        self.control & START != 0
    }

    /// Reads the register at `offset` from 0x04000120. The data registers
    /// read as 0 in modes that do not use them, as do SIOCNT's and RCNT's
    /// unused bits (the mGBA suite's SIO register tests).
    pub(crate) fn read(&self, offset: u32) -> u16 {
        let mode = self.mode();
        let index = (offset / 2) as usize;
        match offset {
            // SIODATA32, or SIOMULTI0-1.
            0x00..=0x03 if matches!(mode, Mode::Normal32 | Mode::Multi) => self.data[index],
            // SIOMULTI2-3.
            0x04..=0x07 if mode == Mode::Multi => self.data[index],
            0x08 => self.read_control(),
            // UART's receive FIFO is always empty.
            0x0A if mode == Mode::Uart => 0,
            0x0A => self.send,
            0x14 => self.read_rcnt(),
            _ => 0,
        }
    }

    /// SIOCNT, laid out for the mode its own bits 12-13 select.
    fn read_control(&self) -> u16 {
        let control = self.control & 0x7FFF;
        if self.mode() == Mode::Multi {
            // SI is low only at the parent's end of a cable.
            let si = u16::from(self.position != Some(0));
            let sd = u16::from(self.ready);
            let id = u16::from(self.position.unwrap_or(0));
            return (control & !0x007C) | si << 2 | sd << 3 | id << 4;
        }
        // Bits 4-6 are unused in normal mode; in UART mode they say the
        // send FIFO is not full, the receive FIFO is empty, and no error.
        let uart = (self.control >> 12) & 3 == 3;
        (control & !0x0070) | if uart { 0x0020 } else { 0 }
    }

    /// RCNT: in normal mode the SD and SO bits read 0, and in JOY Bus mode
    /// SC and SD do.
    fn read_rcnt(&self) -> u16 {
        let rcnt = self.rcnt & 0xC1FF;
        match self.mode() {
            Mode::Normal8 | Mode::Normal32 => rcnt & !0x000A,
            Mode::Other if rcnt & 0x4000 != 0 => rcnt & !0x0003,
            _ => rcnt,
        }
    }

    /// Writes the register at `offset` from 0x04000120.
    pub(crate) fn write(&mut self, offset: u32, value: u16) -> Action {
        match offset {
            // In multi-player mode, SIOMULTI0-3 hold received data only.
            0x00..=0x07 if self.mode() == Mode::Multi => {}
            0x00..=0x07 => self.data[(offset / 2) as usize] = value,
            0x08 => return self.write_control(value),
            0x0A => self.send = value,
            0x14 => self.rcnt = value,
            _ => {}
        }
        Action::None
    }

    fn write_control(&mut self, value: u16) -> Action {
        let was_busy = self.busy();
        self.control = (value & !START) | (self.control & START);
        if matches!(self.mode(), Mode::Uart | Mode::Other) {
            // No transfer runs, so bit 7 keeps what was written: UART's data
            // length, or a start that never completes. One in progress
            // stops.
            self.control = value;
            if was_busy && self.outgoing.take().is_some() {
                return Action::Cancelled;
            }
            return Action::None;
        }
        if value & START == 0 {
            // A multi-player transfer runs to the end; a normal one stops.
            if !was_busy || self.mode() == Mode::Multi {
                return Action::None;
            }
            self.control &= !START;
            self.outgoing = None;
            return Action::Cancelled;
        }
        if was_busy {
            return Action::None;
        }
        match self.mode() {
            Mode::Multi if self.position == Some(0) => {
                let cycles = self.multi_cycles();
                self.control |= START;
                self.outgoing = Some(Outgoing::Multi {
                    word: self.send,
                    cycles,
                });
                self.incoming = [self.send, 0xFFFF, 0xFFFF, 0xFFFF];
                Action::Started(cycles)
            }
            mode @ (Mode::Normal8 | Mode::Normal32) => {
                self.control |= START;
                // Without a partner, SI reads high: all ones come in.
                self.incoming = [0xFFFF; 4];
                if self.control & 1 == 0 {
                    // External clock: the partner's transfer drives this one.
                    return Action::None;
                }
                let bits = if mode == Mode::Normal8 { 8 } else { 32 };
                let cycles = self.normal_cycles(bits);
                self.outgoing = Some(Outgoing::Normal {
                    value: self.normal_value(),
                    bits,
                    cycles,
                });
                Action::Started(cycles)
            }
            // A child: START does nothing.
            _ => Action::None,
        }
    }

    fn multi_cycles(&self) -> u64 {
        let baud = BAUD_RATES[usize::from(self.control & 3)];
        CYCLES_PER_SECOND / baud * MULTI_BITS_PER_UNIT * u64::from(self.units)
    }

    /// The internal clock runs at 256 KHz or, with SIOCNT bit 1, 2 MHz.
    fn normal_cycles(&self, bits: u32) -> u64 {
        let per_bit = if self.control & 2 != 0 { 8 } else { 64 };
        per_bit * u64::from(bits)
    }

    fn normal_value(&self) -> u32 {
        match self.mode() {
            Mode::Normal8 => u32::from(self.send & 0xFF),
            _ => u32::from(self.data[0]) | u32::from(self.data[1]) << 16,
        }
    }

    /// The transfer in progress ends. Returns whether it requests the
    /// serial interrupt.
    pub(crate) fn finish(&mut self) -> bool {
        if !self.busy() {
            return false;
        }
        match self.mode() {
            Mode::Normal8 => self.send = (self.send & 0xFF00) | (self.incoming[0] & 0xFF),
            Mode::Normal32 => self.data[..2].copy_from_slice(&self.incoming[..2]),
            Mode::Multi => self.data = self.incoming,
            Mode::Uart | Mode::Other => {}
        }
        self.control &= !START;
        self.outgoing = None;
        self.control & IRQ_ENABLE != 0
    }

    // The cable's side, used by `Linked`.

    /// Plugs this console into a cable of `units` consoles at `position`
    /// (0 is the parent).
    pub(crate) fn connect(&mut self, position: u8, units: u8) {
        self.position = Some(position);
        self.units = units;
    }

    /// Sets SD: whether every console on the cable is in multi-player mode.
    pub(crate) fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
    }

    pub(crate) fn in_multi_mode(&self) -> bool {
        self.mode() == Mode::Multi
    }

    /// Whether a transfer is waiting for the cable.
    pub(crate) fn has_outgoing(&self) -> bool {
        self.outgoing.is_some()
    }

    pub(crate) fn take_outgoing(&mut self) -> Option<Outgoing> {
        self.outgoing.take()
    }

    /// This console's word for a multi-player transfer, if it is in
    /// multi-player mode.
    pub(crate) fn multi_word(&self) -> Option<u16> {
        self.in_multi_mode().then_some(self.send)
    }

    /// Sets what the running transfer delivers.
    pub(crate) fn set_incoming(&mut self, words: [u16; 4]) {
        self.incoming = words;
    }

    /// A child joins the parent's multi-player transfer, which delivers
    /// `words`. Returns whether it did: it must be in multi-player mode and
    /// idle.
    pub(crate) fn join_multi(&mut self, words: [u16; 4]) -> bool {
        if !self.in_multi_mode() || self.busy() {
            return false;
        }
        self.control |= START;
        self.incoming = words;
        true
    }

    /// The partner clocks a `bits`-bit normal transfer carrying `value`
    /// into this console. If it waits on the external clock with the same
    /// length, it takes the value and returns its own.
    pub(crate) fn clocked_by_partner(&mut self, value: u32, bits: u32) -> Option<u32> {
        let length = match self.mode() {
            Mode::Normal8 => 8,
            Mode::Normal32 => 32,
            _ => return None,
        };
        if length != bits || !self.busy() || self.control & 1 != 0 {
            return None;
        }
        let reply = self.normal_value();
        self.incoming = [value as u16, (value >> 16) as u16, 0xFFFF, 0xFFFF];
        Some(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTI_115200: u16 = 0x2003;

    /// Hardware values from the mGBA suite's SIO register tests, after
    /// writing ones.
    #[test]
    fn registers_read_back_as_their_mode_uses_them() {
        let mut sio = Sio::default();
        // Normal 8-bit mode: no 32-bit data, SIOCNT bits 4-6 and RCNT's SD
        // and SO read 0.
        sio.write(0x00, 0xFFFF);
        sio.write(0x08, 0xCF7F);
        sio.write(0x14, 0x3FFF);
        assert_eq!(sio.read(0x00), 0);
        assert_eq!(sio.read(0x08) & 0xFF7F, 0x4F0F);
        assert_eq!(sio.read(0x14), 0x01F5);
        // Normal 32-bit mode: the data written earlier shows.
        sio.write(0x08, 0x1000);
        assert_eq!(sio.read(0x00), 0xFFFF);
        assert_eq!(sio.read(0x04), 0);
        // UART: bit 7 sticks, the receive FIFO is empty.
        sio.write(0x08, 0xFFFF);
        assert_eq!(sio.read(0x08), 0x7FAF);
        assert_eq!(sio.read(0x0A), 0);
        // JOY Bus: RCNT's SC and SD read 0.
        sio.write(0x14, 0xFFFF);
        assert_eq!(sio.read(0x14), 0xC1FC);
    }

    #[test]
    fn a_lone_console_is_a_child_and_cannot_start_multi_player() {
        let mut sio = Sio::default();
        sio.write(0x08, MULTI_115200);
        assert_eq!(sio.read(0x08) & 0x7C, 0b0100); // SI high, SD low, ID 0
        assert_eq!(sio.write(0x08, MULTI_115200 | START), Action::None);
        assert_eq!(sio.read(0x08) & START, 0);
    }

    #[test]
    fn the_parent_collects_every_word() {
        let mut parent = Sio::default();
        parent.connect(0, 2);
        parent.set_ready(true);
        parent.write(0x08, MULTI_115200);
        parent.write(0x0A, 0x1234);
        assert_eq!(parent.read(0x08) & 0x7C, 0b1000); // SI low, SD high
        let Action::Started(cycles) = parent.write(0x08, MULTI_115200 | START | IRQ_ENABLE) else {
            panic!("the parent starts transfers");
        };
        assert_eq!(cycles, 16_777_216 / 115_200 * 18 * 2);
        assert_eq!(
            parent.take_outgoing(),
            Some(Outgoing::Multi {
                word: 0x1234,
                cycles
            })
        );
        parent.set_incoming([0x1234, 0x5678, 0xFFFF, 0xFFFF]);
        assert!(parent.finish());
        assert_eq!(
            [0, 2, 4, 6].map(|offset| parent.read(offset)),
            [0x1234, 0x5678, 0xFFFF, 0xFFFF]
        );
    }

    #[test]
    fn a_normal_transfer_alone_reads_all_ones() {
        let mut sio = Sio::default();
        sio.write(0x0A, 0x42);
        // 8-bit, internal 2 MHz clock.
        assert_eq!(sio.write(0x08, 0x0003 | START), Action::Started(64));
        sio.finish();
        assert_eq!(sio.read(0x0A), 0xFF);
    }

    #[test]
    fn the_external_clock_waits_for_a_partner() {
        let mut sio = Sio::default();
        sio.write(0x00, 0xBEEF);
        sio.write(0x02, 0xDEAD);
        assert_eq!(sio.write(0x08, 0x1000 | START), Action::None); // 32-bit, external
        assert_eq!(sio.clocked_by_partner(0x0102_0304, 32), Some(0xDEAD_BEEF));
        sio.finish();
        assert_eq!((sio.read(0x00), sio.read(0x02)), (0x0304, 0x0102));
    }
}
