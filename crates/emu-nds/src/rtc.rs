//! The Seiko S-35190A real-time clock on the ARM7 (GBATEK, "DS Real-Time
//! Clock (RTC)").
//!
//! The ARM7 drives three pins through 0x04000138: data in bit 0, clock in
//! bit 1 and select in bit 2, with direction bits 4-6 (set when the CPU
//! drives the pin). With select high, each rising clock edge moves one bit,
//! least significant first. The first byte is a command, `0110 rrr d` from
//! bit 0 up: a fixed code, the register, and 1 to read; then come the
//! register's bytes. Time comes from the emulated clock, never the host, so
//! runs stay deterministic. Writing the date or time sets the clock; the
//! alarms are kept but never fire.

use serde::{Deserialize, Serialize};

const DATA: u16 = 0x01;
const CLOCK: u16 = 0x02;
const SELECT: u16 = 0x04;
const DATA_DIRECTION: u16 = 0x10;

const FIXED_CODE: u8 = 0x06;
const READ: u8 = 0x80;

// Registers, by command bits 4-6.
const STATUS_1: u8 = 0;
const ALARM_1: u8 = 1;
const DATE_TIME: u8 = 2;
const CLOCK_ADJUST: u8 = 3;
const STATUS_2: u8 = 4;
const ALARM_2: u8 = 5;
const TIME: u8 = 6;
const FREE: u8 = 7;

/// Status 1: bit 0 resets the chip, bit 1 selects the 24-hour clock, bits
/// 2-3 are free; bits 4-7 are read-only flags.
const RESET: u8 = 0x01;
const HOUR_24: u8 = 0x02;
const STATUS_1_WRITABLE: u8 = 0x0E;
/// Status 2 bit 2: register 1 holds the 3-byte alarm 1, not the 1-byte
/// frequency setting (which shares the alarm's minute byte).
const ALARM_1_MODE: u8 = 0x04;
/// The hour byte's PM flag.
const PM: u8 = 0x40;

/// ARM7 bus cycles per second.
const CYCLES_PER_SECOND: u64 = 33_513_982;
const SECONDS_PER_DAY: i64 = 86_400;
/// 2000-01-01 00:00:00, where a reset puts the clock.
const RESET_TIME: i64 = 946_684_800;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Rtc {
    /// 0x04000138 as last written.
    pins: u16,
    /// The data level the chip drives when the CPU reads.
    output: bool,
    /// The byte being shifted in, and how many bits have moved.
    shift: u8,
    bits: u8,
    /// The command since select went high, and the register bytes since.
    command: Option<u8>,
    index: usize,
    /// What a read command sends, in order.
    reply: Vec<u8>,
    /// Date or time bytes written so far, applied once all have arrived.
    incoming: Vec<u8>,
    status_1: u8,
    status_2: u8,
    /// Day of week, hour and minute of each alarm.
    alarm_1: [u8; 3],
    alarm_2: [u8; 3],
    clock_adjust: u8,
    free: u8,
    /// Seconds since the Unix epoch at cycle `synced`, and the cycles into
    /// that second.
    unix_time: i64,
    subsecond: u64,
    synced: u64,
}

impl Rtc {
    /// A clock showing `start_time` (Unix seconds) at cycle 0, in 24-hour
    /// mode.
    pub(crate) fn new(start_time: i64) -> Self {
        Self {
            pins: 0,
            output: false,
            shift: 0,
            bits: 0,
            command: None,
            index: 0,
            reply: Vec::new(),
            incoming: Vec::new(),
            status_1: HOUR_24,
            status_2: 0,
            alarm_1: [0; 3],
            alarm_2: [0; 3],
            clock_adjust: 0,
            free: 0,
            unix_time: start_time,
            subsecond: 0,
            synced: 0,
        }
    }

    /// Brings the clock up to cycle `now`.
    fn sync(&mut self, now: u64) {
        self.subsecond += now.saturating_sub(self.synced);
        self.synced = now;
        self.unix_time += i64::try_from(self.subsecond / CYCLES_PER_SECOND).unwrap_or(0);
        self.subsecond %= CYCLES_PER_SECOND;
    }

    /// The console was reset at cycle `now` and counts from zero again; the
    /// clock, on its battery, keeps running.
    pub(crate) fn restart_cycles(&mut self, now: u64) {
        self.sync(now);
        self.synced = 0;
    }

    /// Reads 0x04000138. With the data pin an input, bit 0 is the chip's.
    pub(crate) fn read(&self) -> u16 {
        if self.pins & DATA_DIRECTION != 0 {
            self.pins
        } else {
            (self.pins & !DATA) | u16::from(self.output)
        }
    }

    /// Writes 0x04000138 at cycle `now`.
    pub(crate) fn write(&mut self, value: u16, now: u64) {
        self.sync(now);
        let previous = std::mem::replace(&mut self.pins, value);
        let (clock, previous_clock) = (value & CLOCK != 0, previous & CLOCK != 0);
        if value & SELECT == 0 || previous & SELECT == 0 {
            // Deselected, or just selected: no transfer in progress.
            self.end_transfer();
        } else if previous_clock && !clock {
            self.clock_falls();
        } else if clock && !previous_clock {
            self.clock_rises(value & DATA != 0);
        }
    }

    fn end_transfer(&mut self) {
        self.command = None;
        self.index = 0;
        self.reply.clear();
        self.incoming.clear();
        self.shift = 0;
        self.bits = 0;
    }

    fn reading(&self) -> bool {
        self.command.is_some_and(|command| command & READ != 0)
    }

    /// A falling clock edge with the chip selected: a read puts its next
    /// bit on the data line, for the CPU to sample before or after the
    /// rising edge.
    fn clock_falls(&mut self) {
        if self.reading() {
            let byte = self.reply.get(self.index).copied().unwrap_or(0);
            self.output = byte >> self.bits & 1 != 0;
        }
    }

    /// A rising clock edge with the chip selected: one bit moves.
    fn clock_rises(&mut self, data: bool) {
        if !self.reading() {
            self.shift |= u8::from(data) << self.bits;
        }
        self.bits += 1;
        if self.bits < 8 {
            return;
        }
        self.bits = 0;
        let byte = std::mem::take(&mut self.shift);
        match self.command {
            None => self.receive_command(byte),
            Some(command) => {
                if command & READ == 0 {
                    self.receive_byte((command >> 4) & 7, byte);
                }
                self.index += 1;
            }
        }
    }

    fn receive_command(&mut self, byte: u8) {
        if byte & 0x0F != FIXED_CODE {
            return;
        }
        self.command = Some(byte);
        let register = (byte >> 4) & 7;
        if byte & READ != 0 {
            self.reply = self.register_bytes(register);
            if register == STATUS_1 {
                // The interrupt and power flags clear once read.
                self.status_1 &= 0x0F;
            }
        }
    }

    /// The bytes of `register` as a read sends them.
    fn register_bytes(&self, register: u8) -> Vec<u8> {
        match register {
            STATUS_1 => vec![self.status_1],
            ALARM_1 if self.status_2 & ALARM_1_MODE != 0 => self.alarm_1.to_vec(),
            ALARM_1 => vec![self.alarm_1[2]],
            DATE_TIME => self.date_time().to_vec(),
            CLOCK_ADJUST => vec![self.clock_adjust],
            STATUS_2 => vec![self.status_2],
            ALARM_2 => self.alarm_2.to_vec(),
            TIME => self.date_time()[4..].to_vec(),
            _ => vec![self.free],
        }
    }

    /// Register byte `self.index` of a write to `register`.
    fn receive_byte(&mut self, register: u8, byte: u8) {
        let index = self.index;
        match register {
            STATUS_1 if index == 0 => {
                if byte & RESET != 0 {
                    self.reset();
                } else {
                    self.status_1 =
                        (self.status_1 & !STATUS_1_WRITABLE) | (byte & STATUS_1_WRITABLE);
                }
            }
            ALARM_1 if self.status_2 & ALARM_1_MODE != 0 && index < 3 => self.alarm_1[index] = byte,
            ALARM_1 if index == 0 => self.alarm_1[2] = byte,
            CLOCK_ADJUST if index == 0 => self.clock_adjust = byte,
            STATUS_2 if index == 0 => self.status_2 = byte,
            ALARM_2 if index < 3 => self.alarm_2[index] = byte,
            FREE if index == 0 => self.free = byte,
            DATE_TIME if index < 7 => {
                self.incoming.push(byte);
                if index == 6 {
                    let [year, month, day, _weekday, hour, minute, second] =
                        <[u8; 7]>::try_from(self.incoming.as_slice()).unwrap_or_default();
                    let days = days_from_civil(
                        2000 + from_bcd(year),
                        from_bcd(month & 0x1F).clamp(1, 12),
                        from_bcd(day & 0x3F).clamp(1, 31),
                    );
                    self.set_time(days, self.seconds_of_day(hour, minute, second));
                }
            }
            TIME if index < 3 => {
                self.incoming.push(byte);
                if index == 2 {
                    let days = self.unix_time.div_euclid(SECONDS_PER_DAY);
                    let [hour, minute, second] = [0, 1, 2].map(|i| self.incoming[i]);
                    self.set_time(days, self.seconds_of_day(hour, minute, second));
                }
            }
            _ => {}
        }
    }

    /// Clears every register and sets the clock to 2000-01-01 00:00:00.
    fn reset(&mut self) {
        self.status_1 = 0;
        self.status_2 = 0;
        self.alarm_1 = [0; 3];
        self.alarm_2 = [0; 3];
        self.clock_adjust = 0;
        self.free = 0;
        self.unix_time = RESET_TIME;
        self.subsecond = 0;
    }

    fn set_time(&mut self, days: i64, seconds: i64) {
        self.unix_time = days * SECONDS_PER_DAY + seconds;
        self.subsecond = 0;
    }

    /// Seconds into the day for BCD hour, minute and second bytes. In
    /// 12-hour mode the hour byte's PM flag adds 12 hours.
    fn seconds_of_day(&self, hour: u8, minute: u8, second: u8) -> i64 {
        let pm = self.status_1 & HOUR_24 == 0 && hour & PM != 0;
        let hour = (from_bcd(hour & 0x3F) + if pm { 12 } else { 0 }).min(23);
        let minute = from_bcd(minute & 0x7F).min(59);
        let second = from_bcd(second & 0x7F).min(59);
        hour * 3600 + minute * 60 + second
    }

    /// Year, month, day, weekday (0 = Sunday), hour, minute, second, in
    /// BCD. The hour's bit 6 flags PM in either mode.
    fn date_time(&self) -> [u8; 7] {
        let days = self.unix_time.div_euclid(SECONDS_PER_DAY);
        let seconds = self.unix_time.rem_euclid(SECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday
        let hour = seconds / 3600;
        let pm = if hour >= 12 { PM } else { 0 };
        let shown_hour = if self.status_1 & HOUR_24 != 0 {
            hour
        } else {
            hour % 12
        };
        [
            bcd(year.rem_euclid(100)),
            bcd(i64::from(month)),
            bcd(i64::from(day)),
            bcd(weekday),
            bcd(shown_hour) | pm,
            bcd(seconds / 60 % 60),
            bcd(seconds % 60),
        ]
    }
}

fn bcd(value: i64) -> u8 {
    (value / 10 * 16 + value % 10) as u8
}

fn from_bcd(value: u8) -> i64 {
    i64::from(value >> 4) * 10 + i64::from(value & 0xF)
}

/// The (year, month, day) of a day count from 1970-01-01 (Howard Hinnant's
/// civil-from-days algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The day count from 1970-01-01 of a date (the inverse of
/// [`civil_from_days`]).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cycle the tests talk to the chip at: one second in.
    const NOW: u64 = CYCLES_PER_SECOND;
    /// Select high, the CPU driving all three pins.
    const SELECTED: u16 = SELECT | 0x70;

    fn select(rtc: &mut Rtc) {
        rtc.write(0x70 | CLOCK, NOW);
        rtc.write(SELECTED | CLOCK, NOW);
    }

    fn deselect(rtc: &mut Rtc) {
        rtc.write(0x70 | CLOCK, NOW);
    }

    /// Clocks `byte` into the chip, least significant bit first.
    fn send(rtc: &mut Rtc, byte: u8) {
        for bit in 0..8 {
            let data = u16::from(byte >> bit & 1);
            rtc.write(SELECTED | data, NOW);
            rtc.write(SELECTED | CLOCK | data, NOW);
        }
    }

    /// Clocks a byte out of the chip with the data pin as an input.
    fn receive(rtc: &mut Rtc) -> u8 {
        let input = SELECTED & !DATA_DIRECTION;
        (0..8).fold(0, |byte, bit| {
            rtc.write(input, NOW);
            rtc.write(input | CLOCK, NOW);
            byte | u8::from(rtc.read() & DATA != 0) << bit
        })
    }

    fn command(register: u8, read: bool) -> u8 {
        FIXED_CODE | register << 4 | if read { READ } else { 0 }
    }

    fn read_register(rtc: &mut Rtc, register: u8, length: usize) -> Vec<u8> {
        select(rtc);
        send(rtc, command(register, true));
        let bytes = (0..length).map(|_| receive(rtc)).collect();
        deselect(rtc);
        bytes
    }

    fn write_register(rtc: &mut Rtc, register: u8, bytes: &[u8]) {
        select(rtc);
        send(rtc, command(register, false));
        for &byte in bytes {
            send(rtc, byte);
        }
        deselect(rtc);
    }

    #[test]
    fn calendar_dates_round_trip() {
        for days in [0, 11_016, 20_366, -1, 47_482] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(
                days_from_civil(year, i64::from(month), i64::from(day)),
                days
            );
        }
    }

    #[test]
    fn reads_the_date_and_time_in_bcd() {
        // 2025-10-05 14:30:58 UTC, a Sunday, read a second later.
        let mut rtc = Rtc::new(1_759_674_658);
        assert_eq!(
            read_register(&mut rtc, DATE_TIME, 7),
            [0x25, 0x10, 0x05, 0x00, 0x14 | PM, 0x30, 0x59]
        );
        assert_eq!(read_register(&mut rtc, TIME, 3), [0x14 | PM, 0x30, 0x59]);
        assert_eq!(read_register(&mut rtc, STATUS_1, 1), [HOUR_24]);
    }

    #[test]
    fn bits_can_be_sampled_before_the_rising_edge() {
        let mut rtc = Rtc::new(1_759_674_658);
        select(&mut rtc);
        send(&mut rtc, command(DATE_TIME, true));
        let input = SELECTED & !DATA_DIRECTION;
        let year = (0..8).fold(0, |byte, bit| {
            rtc.write(input, NOW);
            let sampled = rtc.read() & DATA != 0;
            rtc.write(input | CLOCK, NOW);
            byte | u8::from(sampled) << bit
        });
        assert_eq!(year, 0x25);
    }

    #[test]
    fn a_libnds_style_command_reads_the_same() {
        // libnds sends 65h most significant bit first: the same wire bits
        // as A6h least significant first.
        let mut rtc = Rtc::new(1_759_674_658);
        select(&mut rtc);
        for bit in (0..8).rev() {
            let data = u16::from(0x65u8 >> bit & 1);
            rtc.write(SELECTED | data, NOW);
            rtc.write(SELECTED | CLOCK | data, NOW);
        }
        assert_eq!(receive(&mut rtc), 0x25);
    }

    #[test]
    fn twelve_hour_mode_shows_pm_hours_from_zero() {
        let mut rtc = Rtc::new(1_759_674_658);
        write_register(&mut rtc, STATUS_1, &[0]);
        assert_eq!(read_register(&mut rtc, TIME, 1), [0x02 | PM]);
    }

    #[test]
    fn writing_the_date_and_time_sets_the_clock() {
        let mut rtc = Rtc::new(0);
        // 2024-02-29 23:59:30.
        write_register(
            &mut rtc,
            DATE_TIME,
            &[0x24, 0x02, 0x29, 0x04, 0x23, 0x59, 0x30],
        );
        assert_eq!(
            read_register(&mut rtc, DATE_TIME, 7),
            [0x24, 0x02, 0x29, 0x04, 0x23 | PM, 0x59, 0x30]
        );
        // Forty seconds later the date rolls over.
        rtc.write(0x70, NOW + 40 * CYCLES_PER_SECOND);
        assert_eq!(read_register(&mut rtc, DATE_TIME, 3), [0x24, 0x03, 0x01]);
        write_register(&mut rtc, TIME, &[0x08, 0x15, 0x00]);
        assert_eq!(read_register(&mut rtc, TIME, 3), [0x08, 0x15, 0x00]);
    }

    #[test]
    fn keeps_alarms_and_free_registers() {
        let mut rtc = Rtc::new(0);
        write_register(&mut rtc, ALARM_2, &[0x83, 0x87, 0x30]);
        assert_eq!(read_register(&mut rtc, ALARM_2, 3), [0x83, 0x87, 0x30]);
        write_register(&mut rtc, ALARM_1, &[0x11]);
        write_register(&mut rtc, STATUS_2, &[ALARM_1_MODE]);
        write_register(&mut rtc, ALARM_1, &[0x01, 0x02]);
        assert_eq!(read_register(&mut rtc, ALARM_1, 3), [0x01, 0x02, 0x11]);
        write_register(&mut rtc, FREE, &[0x5A]);
        write_register(&mut rtc, CLOCK_ADJUST, &[0x12]);
        assert_eq!(read_register(&mut rtc, FREE, 1), [0x5A]);
        assert_eq!(read_register(&mut rtc, CLOCK_ADJUST, 1), [0x12]);
        write_register(&mut rtc, STATUS_1, &[RESET]);
        assert_eq!(read_register(&mut rtc, FREE, 1), [0]);
        assert_eq!(read_register(&mut rtc, DATE_TIME, 3), [0x00, 0x01, 0x01]);
    }
}
