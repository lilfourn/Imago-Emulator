//! The Seiko S-3511 real-time clock behind the cartridge's GPIO port
//! (GBATEK, "GBA Cart Real-Time Clock (RTC)", "GBA Cart I/O Port (GPIO)").
//!
//! The game drives three pins at 0x080000C4: SCK (bit 0), SIO (bit 1) and
//! CS (bit 2). With CS high, each rising SCK edge moves one bit, least
//! significant first. The first byte is a command, `0110 ccc r`, then the
//! data bytes it reads or writes. Time comes from the emulated clock, never
//! the host, so runs stay deterministic.

use serde::{Deserialize, Serialize};

/// Data bytes for each command number.
const COMMAND_BYTES: [u8; 8] = [0, 0, 7, 0, 1, 0, 3, 0];
const COMMAND_RESET: u8 = 0;
const COMMAND_DATETIME: u8 = 2;
const COMMAND_STATUS: u8 = 4;
const COMMAND_TIME: u8 = 6;

/// System clock cycles per second.
const CYCLES_PER_SECOND: u64 = crate::CLOCK_HZ;

/// Game codes (first three characters) of cartridges with this clock.
const GAMES: [&[u8; 3]; 5] = [
    b"AXV", // Pokémon Ruby
    b"AXP", // Pokémon Sapphire
    b"BPE", // Pokémon Emerald
    b"U3I", // Boktai
    b"U32", // Boktai 2
];

/// Whether the game in `rom` has the RTC, by its header game code.
pub(crate) fn present(rom: &[u8]) -> bool {
    rom.get(0xAC..0xAF)
        .is_some_and(|code| GAMES.iter().any(|game| game.as_slice() == code))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Rtc {
    /// GPIO data written by the game, direction (1 = game drives the pin),
    /// and whether the port reads back (otherwise ROM data shows).
    data: u8,
    direction: u8,
    readable: bool,
    /// The SIO level the chip drives when the game reads.
    output: bool,
    /// Transfer progress: waiting for CS to rise with SCK high.
    selected: bool,
    previous_sck: bool,
    /// The byte being shifted in or out, and how many bits have moved.
    shift: u8,
    bits: u8,
    /// The active command and the data bytes it still has.
    command: Option<u8>,
    remaining: u8,
    /// Bytes a read command sends, most recent byte first.
    reply: Vec<u8>,
    /// Status register: bit 6 selects the 24-hour clock.
    status: u8,
    /// Seconds since the Unix epoch at system cycle `synced`, and the
    /// cycles into that second.
    unix_time: i64,
    subsecond: u64,
    synced: u64,
}

impl Rtc {
    pub(crate) fn new(start_time: i64) -> Self {
        Self {
            status: 0x40,
            unix_time: start_time,
            ..Self::default()
        }
    }

    /// Brings the clock up to system cycle `now`.
    fn sync(&mut self, now: u64) {
        self.subsecond += now.saturating_sub(self.synced);
        self.synced = now;
        self.unix_time += i64::try_from(self.subsecond / CYCLES_PER_SECOND).unwrap_or(0);
        self.subsecond %= CYCLES_PER_SECOND;
    }

    /// The console was reset at system cycle `now` and counts from zero
    /// again; the clock, on the cartridge's battery, keeps running.
    pub(crate) fn restart_cycles(&mut self, now: u64) {
        self.sync(now);
        self.synced = 0;
    }

    /// Reads GPIO register `offset` (0xC4, 0xC6 or 0xC8), if the port is
    /// readable; otherwise the cartridge returns ROM data.
    pub(crate) fn read(&self, offset: u32) -> Option<u16> {
        if !self.readable {
            return None;
        }
        Some(match offset {
            0xC4 => {
                let chip = if self.output { 0b010 } else { 0 };
                u16::from((self.data & self.direction) | (chip & !self.direction))
            }
            0xC6 => u16::from(self.direction),
            _ => u16::from(self.readable),
        })
    }

    /// Writes GPIO register `offset` at system cycle `now`.
    pub(crate) fn write(&mut self, offset: u32, value: u16, now: u64) {
        match offset {
            0xC4 => {
                self.sync(now);
                self.data = (value & 0xF) as u8;
                self.update_pins();
            }
            0xC6 => self.direction = (value & 0xF) as u8,
            0xC8 => self.readable = value & 1 != 0,
            _ => {}
        }
    }

    fn update_pins(&mut self) {
        let sck = self.data & 1 != 0;
        let sio = self.data & 2 != 0;
        let cs = self.data & 4 != 0;
        if !cs {
            // Deselected: end any transfer.
            self.selected = false;
            self.command = None;
            self.reply.clear();
            self.bits = 0;
            self.shift = 0;
        } else if !self.selected {
            self.selected = sck;
        } else if sck && !self.previous_sck {
            self.clock_bit(sio);
        }
        self.previous_sck = sck;
    }

    /// One rising SCK edge with the chip selected.
    fn clock_bit(&mut self, sio: bool) {
        let reading = self.command.is_some_and(|_| !self.reply.is_empty());
        if reading {
            let byte = *self.reply.last().unwrap_or(&0);
            self.output = byte >> self.bits & 1 != 0;
        } else {
            self.shift |= u8::from(sio) << self.bits;
        }
        self.bits += 1;
        if self.bits < 8 {
            return;
        }
        self.bits = 0;
        if reading {
            self.reply.pop();
            self.finish_byte();
        } else {
            let byte = std::mem::take(&mut self.shift);
            self.receive_byte(byte);
        }
    }

    fn receive_byte(&mut self, byte: u8) {
        let Some(command) = self.command else {
            // A command byte: 0110 in the low nibble.
            if byte & 0x0F != 0x06 {
                return;
            }
            let number = (byte >> 4) & 7;
            let read = byte & 0x80 != 0;
            self.command = Some(number);
            self.remaining = COMMAND_BYTES[usize::from(number)];
            if number == COMMAND_RESET {
                self.status = 0;
            }
            if read {
                let mut bytes = match number {
                    COMMAND_DATETIME => self.datetime().to_vec(),
                    COMMAND_TIME => self.datetime()[4..].to_vec(),
                    COMMAND_STATUS => vec![self.status],
                    _ => Vec::new(),
                };
                bytes.reverse();
                self.reply = bytes;
            }
            if self.remaining == 0 {
                self.command = None;
            }
            return;
        };
        if command == COMMAND_STATUS {
            self.status = byte;
        }
        // Writing the date or time is accepted but not kept: the emulated
        // clock is the time source.
        self.finish_byte();
    }

    fn finish_byte(&mut self) {
        self.remaining = self.remaining.saturating_sub(1);
        if self.remaining == 0 {
            self.command = None;
            self.reply.clear();
        }
    }

    /// Year, month, day, weekday, hour, minute, second, in BCD.
    fn datetime(&self) -> [u8; 7] {
        let days = self.unix_time.div_euclid(86_400);
        let seconds = self.unix_time.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday
        let hour = seconds / 3600;
        let shown_hour = if self.status & 0x40 != 0 {
            bcd(hour)
        } else {
            // 12-hour clock with bit 7 for PM.
            bcd(hour % 12) | if hour >= 12 { 0x80 } else { 0 }
        };
        [
            bcd(year.rem_euclid(100)),
            bcd(i64::from(month)),
            bcd(i64::from(day)),
            bcd(weekday),
            shown_hour,
            bcd(seconds / 60 % 60),
            bcd(seconds % 60),
        ]
    }
}

fn bcd(value: i64) -> u8 {
    (value / 10 * 16 + value % 10) as u8
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_366), (2025, 10, 5));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    /// The system cycle the tests talk to the chip at: one second in.
    const NOW: u64 = CYCLES_PER_SECOND;

    /// Clocks `byte` into the chip, least significant bit first.
    fn send(rtc: &mut Rtc, byte: u8) {
        for bit in 0..8 {
            let sio = (byte >> bit & 1) << 1;
            rtc.write(0xC4, u16::from(0b100 | sio), NOW);
            rtc.write(0xC4, u16::from(0b101 | sio), NOW);
        }
    }

    fn receive(rtc: &mut Rtc) -> u8 {
        let mut byte = 0;
        for bit in 0..8 {
            rtc.write(0xC4, 0b100, NOW);
            rtc.write(0xC4, 0b101, NOW);
            byte |= u8::from(rtc.read(0xC4).unwrap_or(0) & 2 != 0) << bit;
        }
        byte
    }

    #[test]
    fn reads_the_date_and_time_in_bcd() {
        // 2025-10-05 14:30:58 UTC, a Sunday, read a second later.
        let mut rtc = Rtc::new(1_759_674_658);
        rtc.write(0xC8, 1, 0);
        rtc.write(0xC6, 0b111, 0);
        rtc.write(0xC4, 0b001, 0);
        rtc.write(0xC4, 0b101, 0); // select with SCK high
        send(&mut rtc, 0x06 | COMMAND_DATETIME << 4 | 0x80);
        rtc.write(0xC6, 0b101, 0); // SIO becomes an input
        let bytes: Vec<u8> = (0..7).map(|_| receive(&mut rtc)).collect();
        assert_eq!(bytes, [0x25, 0x10, 0x05, 0x00, 0x14, 0x30, 0x59]);
    }

    #[test]
    fn detects_pokemon_by_game_code() {
        let mut rom = vec![0; 0xC0];
        rom[0xAC..0xB0].copy_from_slice(b"BPEE");
        assert!(present(&rom));
        rom[0xAC..0xB0].copy_from_slice(b"BPRE");
        assert!(!present(&rom));
    }
}
