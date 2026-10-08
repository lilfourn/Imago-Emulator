//! The wifi hardware on the ARM7 (GBATEK, "DS Wireless Communications"):
//! the MAC's registers at 0x04808000 and its 8 KiB of RAM at 0x04804000,
//! with the baseband and radio chips behind it.
//!
//! What it emulates:
//! - the registers, their mirrors and their power-on values; RAM, which
//!   the game fills with frames to send and the hardware fills with frames
//!   it receives;
//! - power: POWCNT2 bit 1 and W_POWER_US gate the clock, and the wake-up
//!   sequence through W_MODE_RST, W_POWERSTATE and W_POWERFORCE ends with
//!   IRQ11 after 2048 µs;
//! - the microsecond counter W_US_COUNT, with the beacon timers it drives
//!   every 1024 µs (W_BEACON_COUNT, W_POST_BEACON, W_US_COMPARE) and their
//!   IRQ13, IRQ14 and IRQ15; W_CMD_COUNT and W_CONTENTFREE;
//! - sending from the LOC1-3, CMD and beacon slots and MP replies, with
//!   each frame taking its preamble and its bytes' time at 1 or 2 Mbit/s;
//!   the MP host's wait for replies and its ACK; W_TXBUSY, W_TXSTAT and
//!   the TX header updates;
//! - receiving into the circular buffer a halfword at a time, the RX
//!   filter, MP clients answering CMD frames by themselves, and clocks
//!   syncing to the network's beacons;
//! - the serial links to the baseband and radio chips.
//!
//! Frames leave through [`Wifi::take_transmissions`] and arrive through
//! [`Wifi::receive`]; a linked pair of consoles (`link.rs`) carries them.
//! Nothing on the air is lost or corrupted: there are no collisions, no
//! channels and no ACKs, so every unicast frame counts as delivered. A
//! frame that arrives while the receiver is busy waits until it is free.
//!
//! The device keeps its own time. It is brought up to date before every
//! register access ([`Wifi::advance`]) and asks the machine for an event
//! at its next deadline ([`Wifi::reschedule`]). This file holds the state,
//! power and timers; `send.rs`, `receive.rs` and `ports.rs` the sending,
//! receiving and register sides, `frame.rs` what a frame is.

mod baseband;
mod clock;
mod frame;
mod ports;
mod receive;
mod registers;
mod rf;
mod send;

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use baseband::Baseband;
use clock::Counter;
use frame::Mac;
pub(crate) use frame::Transmission;
use receive::Rx;
use registers::*;
use rf::Rf;
use send::{Phase, Slot, Tx};

/// Bytes of MAC RAM.
const RAM_SIZE: usize = 0x2000;
/// Halfword registers in the 0x000-0xFFF page.
const REGISTERS: usize = 0x800;
/// Microseconds the radio takes to wake after W_MODE_RST or W_POWERFORCE
/// asks it to.
const WAKE_MICROS: u64 = 2048;
/// The W_RANDOM generator's period: it steps once per bus cycle.
const RANDOM_PERIOD: u64 = 0x5FD;
/// W_ID on the original DS and on the DS Lite.
const ID_DS: u16 = 0x1440;
const ID_DS_LITE: u16 = 0xC340;
/// The firmware header's console type for the original DS.
const CONSOLE_DS: u8 = 0xFF;

/// W_IF bits.
mod irq {
    pub(super) const RX_DONE: u16 = 0;
    pub(super) const TX_DONE: u16 = 1;
    pub(super) const RX_START: u16 = 6;
    pub(super) const TX_START: u16 = 7;
    pub(super) const RXBUF_COUNT: u16 = 9;
    pub(super) const TXBUF_COUNT: u16 = 8;
    pub(super) const WAKE: u16 = 11;
    pub(super) const MP_END: u16 = 12;
    pub(super) const POST_BEACON: u16 = 13;
    pub(super) const BEACON: u16 = 14;
    pub(super) const PRE_BEACON: u16 = 15;
}

/// The deadlines the device keeps, in the order it handles ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Timer {
    RxEnd,
    Wake,
    TxPhase,
    Incoming,
    PreBeacon,
    Millisecond,
}

/// What raised IRQ14.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Beacon {
    Compare,
    Count,
    Forced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Wifi {
    /// The 0x000-0xFFF register page as halfwords, as written or as the
    /// hardware last set them.
    registers: Vec<u16>,
    ram: Vec<u8>,
    baseband: Baseband,
    rf: Rf,
    /// POWCNT2 bit 1: the wifi block has power, and its ports respond.
    enabled: bool,
    /// W_US_COUNT, and W_US_COMPARE's millisecond bits.
    us_count: Counter,
    /// The W_US_COUNT value at which its low 10 bits next wrap.
    millisecond_at: u64,
    us_compare: u64,
    /// W_US_COMPARE0 bit 0: IRQ14 from W_BEACON_COUNT waits for the next
    /// compare match.
    beacon_blocked: bool,
    /// Microseconds left of W_CMD_COUNT and W_CONTENTFREE.
    cmd_count: Counter,
    content_free: Counter,
    /// W_RANDOM's generator as of bus cycle `random_mark`, and the value
    /// the last read latched.
    random: u16,
    random_mark: u64,
    random_latch: u16,
    /// When the radio finishes waking, and whether W_POWERFORCE forced it.
    wake_at: Option<u64>,
    wake_forced: bool,
    /// The W_US_COUNT value at which IRQ15 comes before the next beacon.
    pre_beacon_at: Option<u64>,
    tx: Option<Tx>,
    rx: Option<Rx>,
    /// Frames from other consoles, by start, not yet received.
    incoming: VecDeque<Transmission>,
    /// Frames this console sent that the link has not taken.
    outgoing: Vec<Transmission>,
    /// MP host: the clients the current CMD named, and those that answered.
    mp_clients: u16,
    mp_answered: u16,
    /// (W_IF & W_IE) became non-zero: the CPU's IF bit 24 is due.
    irq: bool,
    /// The bus cycle the device is up to.
    now: u64,
    /// When the machine will next call [`Wifi::advance`].
    scheduled: Option<u64>,
    /// Frames sent and frames kept since power-on, for debugging.
    sent: u64,
    heard: u64,
}

impl Wifi {
    /// The hardware as the firmware's boot code leaves it, given the
    /// firmware's header and wifi settings (its first 0x200 bytes): the
    /// baseband registers from 0x64, the radio's type and registers from
    /// 0x40 and 0xCE. Missing bytes read as zero.
    pub(crate) fn new(settings: &[u8]) -> Self {
        let byte = |offset: usize| settings.get(offset).copied().unwrap_or(0);
        let slice = |start: usize, length: usize| {
            settings
                .get(start..(start + length).min(settings.len()))
                .unwrap_or_default()
        };
        let mut registers = vec![0; REGISTERS];
        for (offset, value) in POWER_ON {
            registers[offset as usize / 2] = value;
        }
        registers[W_ID as usize / 2] = if byte(0x1D) == CONSOLE_DS {
            ID_DS
        } else {
            ID_DS_LITE
        };
        let rf_kind = byte(0x40);
        let rf_bits = byte(0x41);
        let rf_entries = usize::from(byte(0x42));
        let rf_initial = if rf_kind == 3 {
            slice(0xCE, rf_entries)
        } else {
            slice(0xCE, rf_entries * usize::from(rf_bits & 0x3F).div_ceil(8))
        };
        if rf_bits != 0 {
            registers[W_RF_CNT as usize / 2] = (u16::from(rf_bits) + 0x80) & 0x017F;
        }
        Self {
            registers,
            ram: vec![0; RAM_SIZE],
            baseband: Baseband::new(slice(0x64, 0x69)),
            rf: Rf::new(rf_kind, rf_initial),
            enabled: false,
            us_count: Counter::up(),
            millisecond_at: 0x400,
            us_compare: 0xFFFF_FFFF_FFFF_FC00,
            beacon_blocked: false,
            cmd_count: Counter::down(),
            content_free: Counter::down(),
            random: 1,
            random_mark: 0,
            random_latch: 1,
            wake_at: None,
            wake_forced: false,
            pre_beacon_at: None,
            tx: None,
            rx: None,
            incoming: VecDeque::new(),
            outgoing: Vec::new(),
            mp_clients: 0,
            mp_answered: 0,
            irq: false,
            now: 0,
            scheduled: None,
            sent: 0,
            heard: 0,
        }
    }

    fn reg(&self, offset: u32) -> u16 {
        self.registers[offset as usize / 2]
    }

    fn reg_mut(&mut self, offset: u32) -> &mut u16 {
        &mut self.registers[offset as usize / 2]
    }

    fn ram_byte(&self, address: usize) -> u8 {
        self.ram[address % RAM_SIZE]
    }

    fn ram_half(&self, address: usize) -> u16 {
        let address = address & (RAM_SIZE - 2);
        u16::from_le_bytes([self.ram[address], self.ram[address + 1]])
    }

    fn set_ram_half(&mut self, address: usize, value: u16) {
        let address = address & (RAM_SIZE - 2);
        self.ram[address..address + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// Whether the wifi block has power and its clock runs: POWCNT2 bit 1
    /// and W_POWER_US bit 0 clear.
    pub(crate) fn powered(&self) -> bool {
        self.enabled && self.reg(W_POWER_US) & 1 == 0
    }

    /// The radio is awake (W_POWERSTATE bits 8 and 9 clear).
    fn awake(&self) -> bool {
        self.reg(W_POWERSTATE) & 0x0300 == 0
    }

    /// Whether frames that start now are received.
    fn listening(&self) -> bool {
        self.powered()
            && self.awake()
            && self.reg(W_RXCNT) & 0x8000 != 0
            && self.reg(W_RXBUF_BEGIN) != self.reg(W_RXBUF_END)
    }

    /// POWCNT2 bit 1 changed at bus cycle `now`.
    pub(crate) fn set_enabled(&mut self, enabled: bool, now: u64) {
        self.advance(now);
        self.enabled = enabled;
        self.update_clocks(now);
        self.check_wake_request(now);
    }

    /// Starts or stops the counters after the power or their enables
    /// changed.
    fn update_clocks(&mut self, now: u64) {
        let powered = self.powered();
        let counting = powered && self.reg(W_US_COUNTCNT) & 1 != 0;
        self.us_count.run(counting, now);
        let commands = powered && self.reg(W_CMD_COUNTCNT) & 1 != 0;
        self.cmd_count.run(commands, now);
        self.content_free.run(powered, now);
        self.plan_timers(now);
    }

    /// Takes the interrupt to raise: whether (W_IF & W_IE) went from zero
    /// to non-zero since the last call, which sets the ARM7's IF bit 24.
    pub(crate) fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq)
    }

    /// The bus cycle at which the machine should next call
    /// [`Wifi::advance`], if it changed since the last call: `Some(None)`
    /// cancels the event.
    pub(crate) fn reschedule(&mut self) -> Option<Option<u64>> {
        let deadline = self.next_timer().map(|(time, _)| time);
        (deadline != self.scheduled).then(|| {
            self.scheduled = deadline;
            deadline
        })
    }

    /// Frames this console sent since the last call, for the link to carry.
    pub(crate) fn take_transmissions(&mut self) -> Vec<Transmission> {
        std::mem::take(&mut self.outgoing)
    }

    /// A frame another console sent. It is received when it starts, if
    /// this console is listening then.
    pub(crate) fn receive(&mut self, frame: Transmission) {
        if !self.enabled {
            return;
        }
        let at = self
            .incoming
            .iter()
            .position(|waiting| waiting.start > frame.start)
            .unwrap_or(self.incoming.len());
        self.incoming.insert(at, frame);
    }

    /// The wifi state as one `key=value` line, for debugging.
    pub(crate) fn describe(&self) -> String {
        let mac = self.address(W_MACADDR);
        let bssid = self.address(W_BSSID);
        let hex = |mac: Mac| mac.map(|byte| format!("{byte:02X}")).join(":");
        format!(
            "wifi_on={} mode={:04X} power={:04X} status={} if={:04X} ie={:04X} us={} mac={} bssid={} aid={} rxcnt={:04X} filter={:04X} txbusy={:02X} txreq={:02X} beacon={:04X} cmd={:04X} seq={} sent={} heard={}",
            u8::from(self.powered()),
            self.reg(W_MODE_RST),
            self.reg(W_POWERSTATE),
            self.reg(W_RF_STATUS),
            self.reg(W_IF),
            self.reg(W_IE),
            self.us_count.value(self.now),
            hex(mac),
            hex(bssid),
            self.reg(W_AID_LOW),
            self.reg(W_RXCNT),
            self.reg(W_RXFILTER),
            self.reg(W_TXBUSY),
            self.reg(W_TXREQ_READ),
            self.reg(W_TXBUF_BEACON),
            self.reg(W_TXBUF_CMD),
            self.reg(W_TX_SEQNO),
            self.sent,
            self.heard,
        )
    }

    /// The earliest deadline, and which it is.
    fn next_timer(&self) -> Option<(u64, Timer)> {
        let rx_free =
            self.rx.is_none() && self.tx.as_ref().is_none_or(|tx| tx.phase == Phase::Replies);
        let incoming = self
            .incoming
            .front()
            .filter(|_| rx_free)
            .map(|frame| frame.start.max(self.now));
        let millisecond = self.us_count.time_of(self.millisecond_at);
        let pre_beacon = self
            .pre_beacon_at
            .and_then(|value| self.us_count.time_of(value));
        [
            (self.rx.as_ref().map(|rx| rx.end), Timer::RxEnd),
            (self.wake_at, Timer::Wake),
            (self.tx.as_ref().map(|tx| tx.phase_end), Timer::TxPhase),
            (incoming, Timer::Incoming),
            (pre_beacon, Timer::PreBeacon),
            (millisecond, Timer::Millisecond),
        ]
        .into_iter()
        .filter_map(|(time, timer)| time.map(|time| (time, timer)))
        .min_by_key(|(time, _)| *time)
    }

    /// Brings the device up to bus cycle `now`: everything due by then
    /// happens, in order, at its own time.
    pub(crate) fn advance(&mut self, now: u64) {
        let now = now.max(self.now);
        while let Some((time, timer)) = self.next_timer()
            && time <= now
        {
            self.catch_up_rx(time);
            self.now = time;
            match timer {
                Timer::RxEnd => self.finish_rx(time),
                Timer::Wake => self.wake_done(time),
                Timer::TxPhase => self.tx_phase_done(time),
                Timer::Incoming => self.start_rx(time),
                Timer::PreBeacon => self.pre_beacon(),
                Timer::Millisecond => self.millisecond(time),
            }
        }
        self.catch_up_rx(now);
        self.now = now;
    }

    // Interrupts.

    fn pending_irqs(&self) -> u16 {
        self.reg(W_IF) & self.reg(W_IE)
    }

    /// Notes the CPU interrupt if W_IF & W_IE just became non-zero.
    fn check_irq(&mut self, before: u16) {
        if before == 0 && self.pending_irqs() != 0 {
            self.irq = true;
        }
    }

    fn set_irq(&mut self, bit: u16) {
        let before = self.pending_irqs();
        *self.reg_mut(W_IF) |= 1 << bit;
        self.check_irq(before);
    }

    /// W_RF_STATUS and the W_RF_PINS that go with it.
    fn set_status(&mut self, status: u16) {
        const PINS: [u16; 10] = [0x04, 0x84, 0, 0x46, 0, 0x84, 0x87, 0, 0x46, 0x04];
        *self.reg_mut(W_RF_STATUS) = status;
        *self.reg_mut(W_RF_PINS) = PINS[usize::from(status)];
    }

    /// Back to listening once a frame is done, or idle if the radio was
    /// put to sleep meanwhile.
    fn settle_status(&mut self) {
        let status = if self.awake() { 1 } else { 9 };
        self.set_status(status);
    }

    // Power.

    /// The radio goes to sleep: no more sending or receiving.
    fn power_down(&mut self) {
        *self.reg_mut(W_TXREQ_READ) &= !0x000F;
        *self.reg_mut(W_POWERSTATE) |= 0x0200;
        if self.tx.is_none() && self.rx.is_none() {
            self.set_status(9);
        }
    }

    /// Starts the radio's 2048 µs wake-up.
    fn start_wake(&mut self, now: u64, forced: bool) {
        *self.reg_mut(W_POWERSTATE) |= 0x0100;
        self.wake_at = Some(now + clock::cycles(WAKE_MICROS));
        self.wake_forced = forced;
    }

    /// A wake request (W_POWERSTATE bit 1) with no wake-up under way takes
    /// effect at once.
    fn check_wake_request(&mut self, now: u64) {
        if self.wake_at.is_none() && self.reg(W_POWERSTATE) & 0x0002 != 0 && self.powered() {
            self.wake_at = Some(now);
            self.wake_forced = false;
        }
    }

    fn wake_done(&mut self, now: u64) {
        self.wake_at = None;
        let automatic = self.reg(W_POWER_UNKNOWN) & 1 != 0 || self.wake_forced;
        if automatic || self.reg(W_POWERSTATE) & 0x0002 != 0 {
            *self.reg_mut(W_POWERSTATE) = 0;
            *self.reg_mut(W_RF_PINS) = 0x0084;
            self.set_irq(irq::WAKE);
            self.start_next_tx(now);
        }
    }

    // Timers.

    /// Works out the next millisecond boundary of W_US_COUNT, and when
    /// IRQ15 comes: W_PRE_BEACON microseconds before the boundary at which
    /// W_BEACON_COUNT reaches zero. Called whenever the count or those
    /// registers change.
    fn plan_timers(&mut self, now: u64) {
        self.millisecond_at = (self.us_count.value(now) | 0x3FF) + 1;
        self.pre_beacon_at = None;
        let count = u64::from(self.reg(W_BEACON_COUNT));
        if count == 0 || !self.us_count.running() {
            return;
        }
        let value = self.us_count.value(now);
        let beacon = (value | 0x3FF) + 1 + (count - 1) * 1024;
        self.pre_beacon_at = beacon
            .checked_sub(u64::from(self.reg(W_PRE_BEACON)))
            .filter(|&at| at > value);
    }

    fn pre_beacon(&mut self) {
        self.pre_beacon_at = None;
        if self.reg(W_US_COMPARECNT) & 1 != 0 {
            self.set_irq(irq::PRE_BEACON);
            if self.reg(W_POWER_TX) & 1 != 0 {
                *self.reg_mut(W_RF_PINS) |= 0x0080;
                *self.reg_mut(W_RF_STATUS) = 1;
            }
        }
    }

    /// The low 10 bits of W_US_COUNT wrapped: the beacon timers count.
    fn millisecond(&mut self, now: u64) {
        self.millisecond_at += 0x400;
        let post = self.reg(W_POST_BEACON);
        if post != 0 {
            *self.reg_mut(W_POST_BEACON) = post - 1;
            if post == 1 {
                self.post_beacon();
            }
        }
        let value = self.us_count.value(now);
        if self.reg(W_US_COMPARECNT) & 1 != 0 && value & !0x3FF == self.us_compare {
            self.beacon_blocked = false;
            self.beacon(Beacon::Compare, now);
        }
        let count = self.reg(W_BEACON_COUNT);
        if count != 0 {
            *self.reg_mut(W_BEACON_COUNT) = count - 1;
            if count == 1 {
                self.beacon(Beacon::Count, now);
            }
        }
    }

    /// IRQ14: the beacon timeslot (GBATEK, "IRQ14 Notes").
    fn beacon(&mut self, source: Beacon, now: u64) {
        if source != Beacon::Forced {
            *self.reg_mut(W_BEACON_COUNT) = self.reg(W_BEACONINT);
            self.plan_timers(now);
        }
        if source == Beacon::Count && self.beacon_blocked {
            return;
        }
        if self.reg(W_US_COMPARECNT) & 1 == 0 {
            return;
        }
        self.set_irq(irq::BEACON);
        *self.reg_mut(W_POST_BEACON) = 0xFFFF;
        *self.reg_mut(W_TXREQ_READ) &= 0xFFF2;
        if self.reg(W_TXBUF_BEACON) & 0x8000 != 0 {
            *self.reg_mut(W_TXBUSY) |= Slot::Beacon.busy_bit();
            self.start_next_tx(now);
        }
        if self.reg(W_LISTENCOUNT) == 0 {
            *self.reg_mut(W_LISTENCOUNT) = self.reg(W_LISTENINT);
        }
        let listen = self.reg(W_LISTENCOUNT);
        *self.reg_mut(W_LISTENCOUNT) = listen.wrapping_sub(1) & 0xFF;
    }

    /// IRQ13: after the beacon; without W_POWER_TX bit 1 the radio idles.
    fn post_beacon(&mut self) {
        self.set_irq(irq::POST_BEACON);
        if self.reg(W_POWER_TX) & 0x0002 == 0 {
            *self.reg_mut(W_INTERNAL_034) = 0x0002;
            *self.reg_mut(W_RF_PINS) = 0x0046;
            *self.reg_mut(W_RF_STATUS) = 9;
        }
    }

    /// W_RANDOM: the value latched by the previous read, latching a new
    /// one. The generator steps `x = (x & 1) ^ (x rol 1)` within 11 bits
    /// every bus cycle.
    fn read_random(&mut self, now: u64) -> u16 {
        let steps = now.saturating_sub(self.random_mark) % RANDOM_PERIOD;
        for _ in 0..steps {
            let x = self.random;
            self.random = (x & 1) ^ (((x << 1) | (x >> 10)) & 0x7FF);
        }
        self.random_mark = now;
        std::mem::replace(&mut self.random_latch, self.random)
    }
}

#[cfg(test)]
mod tests;
