//! Sending: the LOC1-3, CMD and beacon slots and MP replies, each frame
//! a preamble then its bytes, and an MP host's wait for replies and its
//! ACK (GBATEK, "DS Wifi Transmit Control", "DS Wifi Multiplay Master",
//! "DS Wifi Multiplay Slave").

use serde::{Deserialize, Serialize};

use super::frame::{self, HEADER, RATE_2_MBIT, Transmission};
use super::registers::*;
use super::{RAM_SIZE, Wifi, clock, irq};

/// The places a frame is sent from, in W_TXBUSY's bit order (the MP reply
/// uses bit 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Slot {
    Loc1,
    Cmd,
    Loc2,
    Loc3,
    Beacon,
    Reply,
}

impl Slot {
    /// The order waiting slots go in. GBATEK: "LOC3 is transferred first,
    /// LOC1 last".
    const ORDER: [Self; 6] = [
        Self::Reply,
        Self::Beacon,
        Self::Loc3,
        Self::Loc2,
        Self::Cmd,
        Self::Loc1,
    ];

    pub(super) fn busy_bit(self) -> u16 {
        match self {
            Self::Loc1 => 0x01,
            Self::Cmd => 0x02,
            Self::Loc2 => 0x04,
            Self::Loc3 => 0x08,
            Self::Beacon => 0x10,
            Self::Reply => 0x80,
        }
    }

    /// The register with the frame's halfword address and enable bit 15.
    fn register(self) -> u32 {
        match self {
            Self::Loc1 => W_TXBUF_LOC1,
            Self::Cmd => W_TXBUF_CMD,
            Self::Loc2 => W_TXBUF_LOC2,
            Self::Loc3 => W_TXBUF_LOC3,
            Self::Beacon => W_TXBUF_BEACON,
            Self::Reply => W_TXBUF_REPLY2,
        }
    }

    /// LOC1-3's number in W_TXSTAT bits 12-13.
    fn loc_index(self) -> Option<u16> {
        match self {
            Self::Loc1 => Some(0),
            Self::Loc2 => Some(1),
            Self::Loc3 => Some(2),
            _ => None,
        }
    }
}

/// Where a frame being sent has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Phase {
    /// The preamble, and for an MP reply the wait for its slot before it.
    Preamble,
    /// The frame's bytes.
    Data,
    /// MP host: the clients' replies.
    Replies,
    /// MP host: the ACK after them.
    Ack,
}

/// A frame being sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Tx {
    slot: Slot,
    pub(super) phase: Phase,
    /// The TX header's byte offset in RAM.
    address: usize,
    /// Bytes of IEEE frame, checksum included.
    length: usize,
    /// The TX header's rate byte.
    rate: u8,
    /// An MP reply the hardware makes up because the game queued none.
    empty_reply: bool,
    /// The bus cycle the phase ends.
    pub(super) phase_end: u64,
}

impl Tx {
    /// Whether this, read from a save state, is a frame the registers
    /// could have named: a header in RAM and a 14-bit length.
    pub(super) fn in_range(&self) -> bool {
        self.address < RAM_SIZE && self.length <= 0x3FFF && self.phase_end < super::TIME_END
    }
}

impl Wifi {
    /// Microseconds of preamble at `rate`: short (96) only at 2 Mbit/s and
    /// if W_PREAMBLE bit 2 asks.
    fn preamble(&self, rate: u8) -> u64 {
        if rate == RATE_2_MBIT && self.reg(W_PREAMBLE) & 0x0004 != 0 {
            96
        } else {
            192
        }
    }

    /// Marks the LOC and CMD slots that are enabled and requested as
    /// waiting, and starts sending if the radio is free.
    pub(super) fn fire_tx(&mut self, now: u64) {
        if self.reg(W_RXCNT) & 0x8000 == 0 {
            return;
        }
        let mut start = 0;
        for slot in [Slot::Loc1, Slot::Cmd, Slot::Loc2, Slot::Loc3] {
            if self.reg(slot.register()) & 0x8000 != 0 {
                start |= slot.busy_bit();
            }
        }
        *self.reg_mut(W_TXBUSY) |= start & self.reg(W_TXREQ_READ);
        self.start_next_tx(now);
    }

    /// Starts sending the most urgent waiting frame, if the radio is free.
    pub(super) fn start_next_tx(&mut self, now: u64) {
        if self.tx.is_some() || self.rx.is_some() || !self.powered() || !self.awake() {
            return;
        }
        let mut slot = None;
        for candidate in Slot::ORDER {
            if candidate == Slot::Reply || self.reg(W_TXBUSY) & candidate.busy_bit() == 0 {
                continue;
            }
            // A LOC or CMD frame cancelled through W_TXBUF_RESET while it
            // waited is not sent.
            if candidate != Slot::Beacon && self.reg(candidate.register()) & 0x8000 == 0 {
                *self.reg_mut(W_TXBUSY) &= !candidate.busy_bit();
                continue;
            }
            slot = Some(candidate);
            break;
        }
        let Some(slot) = slot else {
            return;
        };
        let address = usize::from(self.reg(slot.register()) & 0x0FFF) * 2;
        let rate = self.ram_byte(address + 8);
        self.tx = Some(Tx {
            slot,
            phase: Phase::Preamble,
            address,
            length: usize::from(self.ram_half(address + 0xA) & 0x3FFF),
            rate,
            empty_reply: false,
            phase_end: now + clock::cycles(self.preamble(rate)),
        });
    }

    pub(super) fn tx_phase_done(&mut self, now: u64) {
        let Some(tx) = &self.tx else {
            return;
        };
        match (tx.phase, tx.slot) {
            (Phase::Preamble, _) => self.tx_data(now),
            (Phase::Data, Slot::Cmd) => self.cmd_sent(now),
            (Phase::Data, _) => self.tx_done(now),
            (Phase::Replies, _) => self.send_ack(now),
            (Phase::Ack, _) => self.cmd_done(now),
        }
    }

    fn sequence(&self) -> u16 {
        self.reg(W_TX_SEQNO) << 4
    }

    fn next_sequence(&mut self) {
        let next = (self.reg(W_TX_SEQNO) + 1) & 0x0FFF;
        *self.reg_mut(W_TX_SEQNO) = next;
    }

    /// The preamble is over (IRQ7): the frame's bytes go on the air.
    fn tx_data(&mut self, now: u64) {
        let Some(mut tx) = self.tx.take() else {
            return;
        };
        self.set_irq(irq::TX_START);
        self.set_status(if tx.slot == Slot::Reply { 8 } else { 3 });
        let bytes = if tx.empty_reply {
            frame::mp_empty_reply(
                self.address(W_BSSID),
                self.address(W_MACADDR),
                self.sequence(),
            )
        } else {
            // The hardware numbers frames, unless a CMD's bit 14 says the
            // game did.
            if !(tx.slot == Slot::Cmd && self.reg(W_TXBUF_CMD) & 0x4000 != 0) {
                let sequence = self.sequence();
                self.set_ram_half(tx.address + HEADER + 22, sequence);
                self.next_sequence();
            }
            let mut bytes: Vec<u8> = (0..HEADER + tx.length)
                .map(|offset| self.ram_byte(tx.address + offset))
                .collect();
            if tx.slot == Slot::Beacon && bytes.len() >= HEADER + 32 {
                // Beacons carry the sender's W_US_COUNT; RAM keeps its own.
                let timestamp = self.us_count.value(now).to_le_bytes();
                bytes[HEADER + 24..HEADER + 32].copy_from_slice(&timestamp);
            }
            *self.reg_mut(W_RXTX_ADDR) = (tx.address / 2) as u16;
            bytes
        };
        let reply_slot = if tx.slot == Slot::Reply {
            (self.reg(W_AID_LOW) & 0xF) as u8
        } else {
            0
        };
        self.outgoing.push(Transmission {
            start: now,
            bytes,
            reply_slot,
        });
        self.sent += 1;
        tx.phase = Phase::Data;
        tx.phase_end = now + clock::cycles(tx.length as u64 * frame::micros_per_byte(tx.rate));
        self.tx = Some(tx);
    }

    /// A LOC, beacon or MP reply frame is sent (IRQ1).
    fn tx_done(&mut self, now: u64) {
        let Some(tx) = self.tx.take() else {
            return;
        };
        if !tx.empty_reply {
            self.ram[(tx.address + 5) % RAM_SIZE] = 0;
        }
        let control = self.reg(tx.slot.register());
        match tx.slot {
            Slot::Reply => {
                if self.reg(W_TXSTATCNT) & 0x1000 != 0 {
                    *self.reg_mut(W_TXSTAT) = 0x0401;
                    self.set_irq(irq::TX_DONE);
                }
                if tx.empty_reply {
                    self.next_sequence();
                }
            }
            Slot::Beacon => {
                self.set_ram_half(tx.address, 0x0001);
                if self.reg(W_TXSTATCNT) & 0x8000 != 0 {
                    *self.reg_mut(W_TXSTAT) = 0x0301;
                    self.set_irq(irq::TX_DONE);
                }
            }
            _ => {
                self.set_ram_half(tx.address, 0x0001);
                let index = tx.slot.loc_index().unwrap_or(0);
                let marked = if control & 0x1000 != 0 { 0x0700 } else { 0 };
                *self.reg_mut(W_TXSTAT) = 0x0001 | marked | index << 12;
                self.set_irq(irq::TX_DONE);
                *self.reg_mut(tx.slot.register()) = control & 0x7FFF;
            }
        }
        *self.reg_mut(W_TXBUSY) &= !tx.slot.busy_bit();
        self.end_tx(now);
    }

    /// The radio is free again after sending.
    fn end_tx(&mut self, now: u64) {
        if self.awake() {
            self.set_status(1);
        } else {
            *self.reg_mut(W_TXBUSY) = 0;
            self.set_status(9);
        }
        self.start_next_tx(now);
    }

    /// An MP host's CMD frame is sent: its clients answer next.
    fn cmd_sent(&mut self, now: u64) {
        let Some(mut tx) = self.tx.take() else {
            return;
        };
        self.ram[(tx.address + 5) % RAM_SIZE] = 0;
        if self.reg(W_TXSTATCNT) & 0x4000 != 0 {
            *self.reg_mut(W_TXSTAT) = 0x0800;
            self.set_irq(irq::TX_DONE);
        }
        self.set_status(5);
        // The clients are in the second halfword of the CMD's body.
        let clients = self.ram_half(tx.address + HEADER + 24 + 2);
        self.mp_clients = clients & 0xFFFE;
        self.mp_answered = 0;
        let slot = 10 + u64::from(self.reg(W_CMD_REPLYTIME));
        let wait = 16 + slot * u64::from(self.mp_clients.count_ones()) + self.preamble(tx.rate);
        tx.phase = Phase::Replies;
        tx.phase_end = now + clock::cycles(wait);
        self.tx = Some(tx);
    }

    /// The replies' time is up: the host's hardware sends the ACK.
    fn send_ack(&mut self, now: u64) {
        let Some(mut tx) = self.tx.take() else {
            return;
        };
        self.set_irq(irq::TX_START);
        self.set_status(8);
        *self.reg_mut(W_RXTX_ADDR) = 0x0FC0;
        let failed = self.mp_clients & !self.mp_answered;
        // W_CMD_STAT: a byte per client counting missed replies.
        for client in (1..16).filter(|client| failed & (1 << client) != 0) {
            let index = W_CMD_STAT as usize / 2 + client / 2;
            let shift = (client % 2) * 8;
            let count = ((self.registers[index] >> shift) as u8).wrapping_add(1);
            self.registers[index] =
                (self.registers[index] & !(0xFF << shift)) | u16::from(count) << shift;
        }
        let ack = frame::mp_ack(
            tx.rate,
            self.address(W_BSSID),
            self.address(W_MACADDR),
            self.sequence(),
            failed,
        );
        self.outgoing.push(Transmission {
            start: now,
            bytes: ack,
            reply_slot: 0,
        });
        tx.phase = Phase::Ack;
        tx.phase_end = now + clock::cycles(32 * frame::micros_per_byte(tx.rate));
        self.tx = Some(tx);
    }

    /// The ACK is sent and the CMD's exchange is over (IRQ12).
    fn cmd_done(&mut self, now: u64) {
        let Some(tx) = self.tx.take() else {
            return;
        };
        *self.reg_mut(W_TXBUSY) &= !Slot::Cmd.busy_bit();
        *self.reg_mut(W_TXBUF_CMD) &= 0x7FFF;
        let failed = self.mp_clients & !self.mp_answered;
        self.set_ram_half(tx.address, if failed == 0 { 0x0001 } else { 0x0005 });
        self.set_ram_half(tx.address + 2, failed);
        self.count_try(tx.address);
        self.next_sequence();
        if self.reg(W_TXSTATCNT) & 0x2000 != 0 {
            *self.reg_mut(W_TXSTAT) = 0x0B01;
            self.set_irq(irq::TX_DONE);
        }
        self.set_irq(irq::MP_END);
        self.end_tx(now);
    }

    /// TX header byte 4 counts the times a frame was sent; byte 5 clears.
    fn count_try(&mut self, address: usize) {
        let count = self.ram_byte(address + 4).saturating_add(1);
        self.set_ram_half(address + 4, u16::from(count));
    }

    /// An MP client received a CMD naming `clients`: unless it is not one
    /// of them, its hardware queues the reply the game left in REPLY1 (or
    /// an empty one) for its slot.
    pub(super) fn answer_cmd(&mut self, reply_time: u16, clients: u16, now: u64) {
        let aid = self.reg(W_AID_LOW) & 0xF;
        if aid == 0 || clients & (1 << aid) == 0 || self.tx.is_some() {
            return;
        }
        let old = self.reg(W_TXBUF_REPLY2);
        if old & 0x8000 != 0 {
            self.set_ram_half(usize::from(old & 0x0FFF) * 2, 0x0001);
        }
        let reply = self.reg(W_TXBUF_REPLY1);
        *self.reg_mut(W_TXBUF_REPLY2) = reply;
        *self.reg_mut(W_TXBUF_REPLY1) = 0;
        let address = usize::from(reply & 0x0FFF) * 2;
        let length = usize::from(self.ram_half(address + 0xA) & 0x3FFF);
        let rate = self.ram_byte(address + 8);
        // A reply longer than the slot is not sent.
        let airtime = self.preamble(rate) + length as u64 * frame::micros_per_byte(rate);
        let queued = reply & 0x8000 != 0 && airtime <= u64::from(reply_time);
        if queued {
            self.count_try(address);
        }
        let earlier = (clients & ((1 << aid) - 1) & !1).count_ones();
        let (rate, length) = if queued {
            (rate, length)
        } else {
            (RATE_2_MBIT, 28)
        };
        let wait = 16 + (u64::from(reply_time) + 10) * u64::from(earlier) + self.preamble(rate);
        self.tx = Some(Tx {
            slot: Slot::Reply,
            phase: Phase::Preamble,
            address,
            length,
            rate,
            empty_reply: !queued,
            phase_end: now + clock::cycles(wait),
        });
        *self.reg_mut(W_TXBUSY) |= Slot::Reply.busy_bit();
    }
}
