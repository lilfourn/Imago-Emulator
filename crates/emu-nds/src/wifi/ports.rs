//! The CPU's view: register reads and writes and their side effects
//! (GBATEK, "DS Wifi I/O Map" and the chapters on each register).

use super::registers::*;
use super::{Beacon, ID_DS_LITE, RAM_SIZE, Wifi, irq};

impl Wifi {
    /// A 16-bit read at `address` in 0x04800000-0x0480FFFF, at bus cycle
    /// `now`. The 32 KiB repeats; in each, 0x0000-0x0FFF and its mirrors at
    /// 0x1000, 0x6000 and 0x7000 are registers, 0x4000-0x5FFF is RAM.
    /// Reads 0 while POWCNT2 has the wifi block off.
    pub(crate) fn read_16(&mut self, address: u32, now: u64) -> u16 {
        if !self.enabled {
            return 0;
        }
        self.advance(now);
        let offset = address & 0x7FFE;
        match offset {
            0x4000..=0x5FFF => self.ram_half(offset as usize),
            0x2000..=0x3FFF => 0xFFFF,
            _ => self.read_register(offset & 0x0FFF, offset < 0x1000, now),
        }
    }

    /// A 16-bit write; byte writes are ignored (GBATEK).
    pub(crate) fn write_16(&mut self, address: u32, value: u16, now: u64) {
        if !self.enabled {
            return;
        }
        self.advance(now);
        let offset = address & 0x7FFE;
        match offset {
            0x4000..=0x5FFF => self.set_ram_half(offset as usize, value),
            0x2000..=0x3FFF => {}
            _ => self.write_register(offset & 0x0FFF, value, now),
        }
    }

    /// The byte at `address` as the CPU would read it, without side
    /// effects, for debuggers.
    pub(crate) fn peek(&self, address: u32) -> Option<u8> {
        let offset = address & 0x7FFF;
        let half = match offset & !1 {
            _ if !self.enabled => 0,
            0x4000..=0x5FFF => self.ram_half(offset as usize),
            0x2000..=0x3FFF => 0xFFFF,
            register => self.register_value(register & 0x0FFF, self.now),
        };
        Some((half >> ((offset & 1) * 8)) as u8)
    }

    /// A register's value without side effects.
    fn register_value(&self, offset: u32, now: u64) -> u16 {
        let part = |value: u64, offset: u32| (value >> ((offset & 6) * 8)) as u16;
        match offset {
            W_RANDOM => self.random_latch,
            W_PREAMBLE => self.reg(offset) & 0x0003,
            W_TXBUSY => self.reg(offset) & 0x001F,
            W_US_COMPARE..=0x0F6 => part(self.us_compare, offset),
            W_US_COUNT..=0x0FE => part(self.us_count.value(now), offset),
            W_CONTENTFREE => self.content_free.value(now) as u16,
            W_CMD_COUNT => self.cmd_count.value(now).div_ceil(10) as u16,
            W_BB_READ => self.baseband.read(),
            W_BB_BUSY | W_RF_BUSY => 0,
            _ => self.reg(offset),
        }
    }

    /// Reads register `offset`; `active` is false in the mirrors where
    /// W_RXBUF_RD_DATA only repeats its last value.
    fn read_register(&mut self, offset: u32, active: bool, now: u64) -> u16 {
        match offset {
            W_RANDOM => self.read_random(now),
            W_RXBUF_RD_DATA if active => self.read_rx_data(),
            W_CMD_STAT..=W_CMD_STAT_END => {
                // Reading clears the counts.
                std::mem::take(&mut self.registers[offset as usize / 2])
            }
            _ => self.register_value(offset, now),
        }
    }

    /// W_RXBUF_RD_DATA: the halfword at W_RXBUF_RD_ADDR, which moves on
    /// through the circular buffer and its gap.
    fn read_rx_data(&mut self) -> u16 {
        let address = usize::from(self.reg(W_RXBUF_RD_ADDR));
        let value = self.ram_half(address);
        let (begin, end) = self.ring();
        let mut next = (address + 2) & (RAM_SIZE - 2);
        if next == end {
            next = begin;
        }
        if next == usize::from(self.reg(W_RXBUF_GAP)) {
            next += usize::from(self.reg(W_RXBUF_GAPDISP)) * 2;
            if next >= end {
                next = next + begin - end;
            }
            if self.reg(W_ID) == ID_DS_LITE {
                *self.reg_mut(W_RXBUF_GAPDISP) = 0;
            }
        }
        *self.reg_mut(W_RXBUF_RD_ADDR) = (next & (RAM_SIZE - 2)) as u16;
        *self.reg_mut(W_RXBUF_RD_DATA) = value;
        self.count_down(W_RXBUF_COUNT, irq::RXBUF_COUNT);
        value
    }

    /// Decrements W_RXBUF_COUNT or W_TXBUF_COUNT, raising `bit` as it
    /// reaches zero.
    fn count_down(&mut self, offset: u32, bit: u16) {
        let count = self.reg(offset);
        if count > 0 {
            *self.reg_mut(offset) = count - 1;
            if count == 1 {
                self.set_irq(bit);
            }
        }
    }

    /// Replaces bits 16 × (`offset` & 6) up of a 64-bit counter value.
    fn with_part(value: u64, offset: u32, part: u16) -> u64 {
        let shift = (offset & 6) * 8;
        (value & !(0xFFFF << shift)) | u64::from(part) << shift
    }

    fn write_register(&mut self, offset: u32, value: u16, now: u64) {
        if READ_ONLY.contains(&offset) {
            return;
        }
        let stored = match offset {
            W_MODE_RST => {
                self.write_mode_reset(value, now);
                value
            }
            W_MODE_WEP => {
                let value = value & 0x007F;
                match value & 7 {
                    1 => *self.reg_mut(W_POWER_UNKNOWN) |= 0x0002,
                    2 => *self.reg_mut(W_POWER_UNKNOWN) = 0x0003,
                    _ => {}
                }
                value
            }
            W_POWER_UNKNOWN => match self.reg(W_MODE_WEP) & 7 {
                1 => (value & 3) | 2,
                2 => 3,
                _ => value & 3,
            },
            W_IF => {
                *self.reg_mut(W_IF) &= !value;
                return;
            }
            W_IE => {
                let before = self.pending_irqs();
                *self.reg_mut(W_IE) = value;
                self.check_irq(before);
                return;
            }
            W_IF_SET => {
                let before = self.pending_irqs();
                *self.reg_mut(W_IF) |= value & 0xFBFF;
                self.check_irq(before);
                return;
            }
            W_AID_LOW => value & 0x000F,
            W_AID_FULL => value & 0x07FF,
            W_POWER_US => {
                *self.reg_mut(W_POWER_US) = value & 3;
                self.update_clocks(now);
                self.check_wake_request(now);
                return;
            }
            W_POWERSTATE => {
                *self.reg_mut(W_POWERSTATE) |= value & 0x0002;
                if self.reg(W_MODE_RST) & 1 != 0
                    && value & 0x0002 != 0
                    && self.reg(W_POWERFORCE) == 1
                {
                    self.start_wake(now, false);
                }
                self.check_wake_request(now);
                return;
            }
            W_POWERFORCE => {
                let value = value & 0x8001;
                *self.reg_mut(W_POWERFORCE) = value;
                match value {
                    0x8001 => {
                        *self.reg_mut(W_INTERNAL_034) = 0x0002;
                        *self.reg_mut(W_POWERSTATE) = 0x0200;
                        *self.reg_mut(W_TXREQ_READ) = 0;
                        self.power_down();
                    }
                    0x0001 if self.reg(W_POWERSTATE) & 0x0002 != 0 => self.start_wake(now, false),
                    0x8000 => self.start_wake(now, true),
                    _ => {}
                }
                return;
            }
            W_US_COUNTCNT | W_CMD_COUNTCNT => {
                *self.reg_mut(offset) = value & 1;
                self.update_clocks(now);
                return;
            }
            W_US_COMPARECNT => {
                *self.reg_mut(offset) = value & 1;
                if value & 2 != 0 {
                    self.beacon(Beacon::Forced, now);
                }
                self.plan_timers(now);
                return;
            }
            W_US_COUNT..=0x0FE => {
                let count = Self::with_part(self.us_count.value(now), offset, value);
                self.us_count.set(count, now);
                self.plan_timers(now);
                return;
            }
            W_US_COMPARE..=0x0F6 => {
                let part = if offset == W_US_COMPARE {
                    if value & 1 != 0 {
                        self.beacon_blocked = true;
                    }
                    value & 0xFC00
                } else {
                    value
                };
                self.us_compare = Self::with_part(self.us_compare, offset, part);
                return;
            }
            W_CMD_COUNT => {
                self.cmd_count.set(u64::from(value) * 10, now);
                return;
            }
            W_CONTENTFREE => {
                self.content_free.set(u64::from(value), now);
                return;
            }
            W_BEACON_COUNT | W_PRE_BEACON => {
                *self.reg_mut(offset) = value;
                self.plan_timers(now);
                return;
            }
            W_BEACONINT => value & 0x03FF,
            W_BB_CNT => {
                self.baseband.transfer(value, self.reg(W_BB_WRITE));
                value
            }
            W_RF_DATA2 => {
                let (data1, data2) = self.rf.transfer(self.reg(W_RF_DATA1), value);
                *self.reg_mut(W_RF_DATA1) = data1;
                data2
            }
            W_RF_CNT => value & 0x413F,
            W_RXCNT => {
                if value & 0x0001 != 0 {
                    *self.reg_mut(W_RXBUF_WRCSR) = self.reg(W_RXBUF_WR_ADDR);
                }
                if value & 0x0080 != 0 {
                    *self.reg_mut(W_TXBUF_REPLY2) = self.reg(W_TXBUF_REPLY1);
                    *self.reg_mut(W_TXBUF_REPLY1) = 0;
                }
                *self.reg_mut(W_RXCNT) = value & 0xFF0E;
                if value & 0x8000 != 0 {
                    self.fire_tx(now);
                }
                return;
            }
            W_RXBUF_RD_DATA => {
                // Writing the read-only port still counts down.
                self.count_down(W_RXBUF_COUNT, irq::RXBUF_COUNT);
                return;
            }
            W_RXBUF_RD_ADDR | W_RXBUF_GAP | W_TXBUF_WR_ADDR | W_TXBUF_GAP => value & 0x1FFE,
            W_RXBUF_GAPDISP | W_RXBUF_COUNT | W_RXBUF_WR_ADDR | W_RXBUF_READCSR
            | W_TXBUF_GAPDISP | W_TXBUF_COUNT => value & 0x0FFF,
            W_TXBUF_WR_DATA => {
                self.write_tx_data(value);
                return;
            }
            W_TXREQ_RESET => {
                *self.reg_mut(W_TXREQ_READ) &= !(value & 0x000F);
                return;
            }
            W_TXREQ_SET => {
                *self.reg_mut(W_TXREQ_READ) |= value & 0x000F;
                self.fire_tx(now);
                return;
            }
            W_TXBUF_RESET => {
                for (bit, register) in [
                    (0x01, W_TXBUF_LOC1),
                    (0x02, W_TXBUF_CMD),
                    (0x04, W_TXBUF_LOC2),
                    (0x08, W_TXBUF_LOC3),
                    (0x40, W_TXBUF_REPLY2),
                    (0x80, W_TXBUF_REPLY1),
                ] {
                    if value & bit != 0 {
                        *self.reg_mut(register) &= 0x7FFF;
                    }
                }
                return;
            }
            W_TXBUF_LOC1 | W_TXBUF_LOC2 | W_TXBUF_LOC3 | W_TXBUF_CMD => {
                *self.reg_mut(offset) = value;
                self.fire_tx(now);
                return;
            }
            W_RXSTAT..=W_RXSTAT_END => value,
            _ => value,
        };
        *self.reg_mut(offset) = stored;
    }

    /// W_MODE_RST: bit 0 turns the MAC on (which starts the radio waking
    /// if W_POWER_UNKNOWN bit 1 is set) or off; bits 13 and 14 reset
    /// groups of registers (GBATEK, "DS Wifi Control").
    fn write_mode_reset(&mut self, value: u16, now: u64) {
        let old = self.reg(W_MODE_RST);
        if old & 1 == 0 && value & 1 != 0 {
            let forced_wake = self.wake_at.is_some() && self.wake_forced;
            if !forced_wake {
                *self.reg_mut(W_INTERNAL_034) = 0x0002;
                *self.reg_mut(W_INTERNAL_27C) = 0x0005;
                if self.reg(W_POWER_UNKNOWN) & 0x0002 != 0 {
                    self.start_wake(now, false);
                }
            }
        } else if old & 1 != 0 && value & 1 == 0 {
            *self.reg_mut(W_INTERNAL_27C) = 0x000A;
            self.power_down();
        }
        if value & 0x2000 != 0 {
            for (offset, reset) in [
                (W_RXBUF_WR_ADDR, 0),
                (W_CMD_TOTALTIME, 0),
                (W_CMD_REPLYTIME, 0),
                (W_X_1A4, 0),
                (W_INTERNAL_278, 0x000F),
            ] {
                *self.reg_mut(offset) = reset;
            }
        }
        if value & 0x4000 != 0 {
            for (offset, reset) in [
                (W_MODE_WEP, 0),
                (W_TXSTATCNT, 0),
                (W_X_00A, 0),
                (W_MACADDR, 0),
                (W_MACADDR + 2, 0),
                (W_MACADDR + 4, 0),
                (W_BSSID, 0),
                (W_BSSID + 2, 0),
                (W_BSSID + 4, 0),
                (W_AID_LOW, 0),
                (W_AID_FULL, 0),
                (W_TX_RETRYLIMIT, 0x0707),
                (W_INTERNAL_02E, 0),
                (W_RXBUF_BEGIN, 0x4000),
                (W_RXBUF_END, 0x4800),
                (W_TXBUF_TIM, 0),
                (W_PREAMBLE, 0x0001),
                (W_RXFILTER, 0x0401),
                (W_CONFIG_0D4, 0x0001),
                (W_RXFILTER2, 0x0008),
                (W_CONFIG_0EC, 0x3F03),
                (W_TX_HDR_CNT, 0),
                (W_INTERNAL_198, 0),
                (W_X_1A2, 0x0001),
                (W_INTERNAL_224, 0x0003),
                (W_INTERNAL_230, 0x0047),
            ] {
                *self.reg_mut(offset) = reset;
            }
        }
    }

    /// W_TXBUF_WR_DATA: stores a halfword at W_TXBUF_WR_ADDR, which moves
    /// on, past the gap.
    fn write_tx_data(&mut self, value: u16) {
        let address = usize::from(self.reg(W_TXBUF_WR_ADDR));
        self.set_ram_half(address, value);
        let mut next = address + 2;
        if next == usize::from(self.reg(W_TXBUF_GAP)) {
            next += usize::from(self.reg(W_TXBUF_GAPDISP)) * 2;
        }
        *self.reg_mut(W_TXBUF_WR_ADDR) = (next & (RAM_SIZE - 2)) as u16;
        self.count_down(W_TXBUF_COUNT, irq::TXBUF_COUNT);
    }
}
