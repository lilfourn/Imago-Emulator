//! Receiving: frames from the air into the circular buffer in MAC RAM,
//! a halfword at a time, then the RX filter (GBATEK, "DS Wifi Receive
//! Buffer", "DS Wifi Hardware Headers").

use serde::{Deserialize, Serialize};

use super::frame::{self, HEADER, Mac, Station, Transmission};
use super::registers::*;
use super::send::Phase;
use super::{RAM_SIZE, Wifi, clock, irq};

/// A frame on its way into the circular buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Rx {
    frame: Transmission,
    /// When its data started arriving here: when it was sent, or later if
    /// this console was busy then.
    start: u64,
    pub(super) end: u64,
    /// Bytes of IEEE frame kept: its length less W_RX_LEN_CROP.
    kept: usize,
    /// Halfwords written to RAM so far, and where the next goes.
    written: usize,
    address: usize,
}

impl Wifi {
    /// The circular buffer's bounds as byte offsets in RAM.
    pub(super) fn ring(&self) -> (usize, usize) {
        (
            usize::from(self.reg(W_RXBUF_BEGIN)) & (RAM_SIZE - 2),
            usize::from(self.reg(W_RXBUF_END)) & (RAM_SIZE - 2),
        )
    }

    /// `address` moved on by `bytes` (even) through the circular buffer.
    fn ring_step(&self, mut address: usize, bytes: usize) -> usize {
        let (begin, end) = self.ring();
        for _ in 0..bytes / 2 {
            address = (address + 2) & (RAM_SIZE - 2);
            if address == end {
                address = begin;
            }
        }
        address
    }

    /// Free bytes in the circular buffer, from the write cursor to the read
    /// cursor.
    fn rx_space(&self) -> usize {
        let (begin, end) = self.ring();
        let size = end.wrapping_sub(begin) & (RAM_SIZE - 2);
        let write = usize::from(self.reg(W_RXBUF_WRCSR)) * 2;
        let read = usize::from(self.reg(W_RXBUF_READCSR)) * 2;
        match read.cmp(&write) {
            std::cmp::Ordering::Equal => size,
            std::cmp::Ordering::Greater => read - write,
            std::cmp::Ordering::Less => size.saturating_sub(write - read),
        }
    }

    // Receiving.

    /// The next frame on the air reaches this console.
    pub(super) fn start_rx(&mut self, now: u64) {
        let Some(frame) = self.incoming.pop_front() else {
            return;
        };
        if !self.listening() {
            return;
        }
        let crop = self.reg(W_RX_LEN_CROP);
        let crop = if frame::half(frame.frame(), 0) & 0x4000 != 0 {
            crop >> 8
        } else {
            crop & 0xFF
        };
        let kept = frame
            .length()
            .saturating_sub(usize::from(crop) * 2)
            .min(frame.frame().len());
        if HEADER + kept.next_multiple_of(4) >= self.rx_space() {
            // W_RXSTAT's RX buffer full count.
            let full = &mut self.registers[0x1B4 / 2];
            *full = (*full & 0xFF00) | ((*full + 1) & 0xFF);
            return;
        }
        let start = frame.start.max(now);
        let end = start + frame.duration();
        let header = usize::from(self.reg(W_RXBUF_WRCSR)) * 2;
        let address = self.ring_step(header, HEADER);
        *self.reg_mut(W_RXTX_ADDR) = (address / 2) as u16;
        self.rx = Some(Rx {
            frame,
            start,
            end,
            kept,
            written: 0,
            address,
        });
        self.set_irq(irq::RX_START);
        self.set_status(6);
    }

    /// Writes the halfwords of the frame being received that have arrived
    /// by bus cycle `now`.
    pub(super) fn catch_up_rx(&mut self, now: u64) {
        let Some(mut rx) = self.rx.take() else {
            return;
        };
        let total = rx.kept.div_ceil(2);
        let due = if now >= rx.end {
            total
        } else {
            let per_halfword = 2 * rx.frame.micros_per_byte();
            let arrived = clock::micros(now.saturating_sub(rx.start)) / per_halfword;
            (arrived as usize).min(total)
        };
        if rx.written < due {
            let bytes = &rx.frame.frame()[..rx.kept];
            while rx.written < due {
                let low = bytes[2 * rx.written];
                let high = bytes.get(2 * rx.written + 1).copied().unwrap_or(0);
                self.set_ram_half(rx.address, u16::from_le_bytes([low, high]));
                rx.address = self.ring_step(rx.address, 2);
                rx.written += 1;
            }
            *self.reg_mut(W_RXTX_ADDR) = (rx.address / 2) as u16;
        }
        self.rx = Some(rx);
    }

    fn station(&self) -> Station {
        Station {
            mac: self.address(W_MACADDR),
            bssid: self.address(W_BSSID),
            filter: self.reg(W_RXFILTER),
            filter2: self.reg(W_RXFILTER2),
            wep: self.reg(W_WEP_CNT) & 0x8000 != 0,
        }
    }

    /// The MAC address in the three registers from `offset`.
    pub(super) fn address(&self, offset: u32) -> Mac {
        let mut mac = [0; 6];
        for (index, pair) in mac.chunks_exact_mut(2).enumerate() {
            pair.copy_from_slice(&self.reg(offset + 2 * index as u32).to_le_bytes());
        }
        mac
    }

    /// The frame being received has all arrived: the RX filter decides
    /// whether it stays in the buffer.
    pub(super) fn finish_rx(&mut self, now: u64) {
        self.catch_up_rx(now);
        let Some(rx) = self.rx.take() else {
            return;
        };
        if self.tx.is_none() {
            self.settle_status();
        }
        let frame = rx.frame.frame();
        // The host hears each client in its own slot, whatever the filter
        // then keeps.
        if self
            .tx
            .as_ref()
            .is_some_and(|tx| tx.phase == Phase::Replies)
            && frame::is_mp_reply(frame)
            && rx.frame.reply_slot != 0
        {
            self.mp_answered |= 1 << rx.frame.reply_slot;
        }
        if let Some(flags) = frame::classify(frame, &self.station()) {
            self.keep_frame(&rx, flags);
            if frame::is_mp_cmd(flags) {
                let (reply_time, clients) = frame::mp_cmd_body(frame);
                self.answer_cmd(reply_time, clients, now);
            } else if frame::is_own_beacon(flags) {
                // The beacon's timestamp is the sender's W_US_COUNT as it
                // started; this console's counter takes it, plus the time
                // since.
                let mut timestamp = [0; 8];
                if let Some(bytes) = frame.get(24..32) {
                    timestamp.copy_from_slice(bytes);
                }
                let elapsed = clock::micros(now.saturating_sub(rx.frame.start));
                let value = u64::from_le_bytes(timestamp).wrapping_add(elapsed);
                self.us_count.set(value, now);
                self.plan_timers(now);
            }
        }
        self.start_next_tx(now);
    }

    /// Writes the RX header before a received frame and moves the write
    /// cursor past it, aligned to a word.
    fn keep_frame(&mut self, rx: &Rx, flags: u16) {
        let mut address = usize::from(self.reg(W_RXBUF_WRCSR)) * 2;
        let header = [
            Some(flags),
            Some(0x0040),
            None,
            Some(u16::from(rx.frame.bytes[8])),
            Some(rx.kept as u16),
            Some(0x4080),
        ];
        for value in header {
            if let Some(value) = value {
                self.set_ram_half(address, value);
            }
            address = self.ring_step(address, 2);
        }
        let mut end = rx.address;
        if end & 2 != 0 {
            end = self.ring_step(end, 2);
        }
        *self.reg_mut(W_RXBUF_WRCSR) = (end / 2) as u16;
        self.set_irq(irq::RX_DONE);
        self.heard += 1;
    }
}
