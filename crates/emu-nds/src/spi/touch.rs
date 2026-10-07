//! The TSC2046 touchscreen controller (GBATEK, "DS Touch Screen Controller
//! (TSC)").
//!
//! A control byte with bit 7 set starts a conversion of the channel in bits
//! 4-6, 12-bit unless bit 3 asks for 8. The result shifts out over the next
//! two bytes, most significant bit first, after one zero bit: a 12-bit
//! value V reads as V >> 5 and then (V << 3) & FFh. A new control byte may
//! go out while the second byte of the last result comes in.
//!
//! The microphone is the AUX input, channel 6. Games sample it from an
//! ARM7 timer, thousands of times a second, in 8 or 12 bits. The frame's
//! input says how loud the player is; each conversion reads noise of that
//! loudness around mid-scale, the time of the conversion choosing the
//! noise, so a game listening for blowing hears it on every run alike.

use serde::{Deserialize, Serialize};

const START: u8 = 0x80;
const EIGHT_BIT: u8 = 0x08;

const TEMPERATURE_0: u8 = 0;
const Y: u8 = 1;
const BATTERY: u8 = 2;
const Z1: u8 = 3;
const Z2: u8 = 4;
const X: u8 = 5;
const MICROPHONE: u8 = 6;
const TEMPERATURE_1: u8 = 7;

/// Readings with nothing touching the screen.
const RELEASED_X: u16 = 0;
const RELEASED_Y: u16 = 0xFFF;
/// Pressure readings for a firm touch, and for none.
const PRESSED_Z: (u16, u16) = (0x200, 0xE00);
const RELEASED_Z: (u16, u16) = (0, 0xFFF);
/// About 26 °C: GBATEK's uncalibrated formula, (TP1 - TP0) × 8568 / 4096
/// kelvin, gives 299 K.
const TEMPERATURES: (u16, u16) = (0x2E2, 0x371);
/// The microphone at rest sits mid-scale.
const SILENCE: u16 = 0x800;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Touch {
    /// The pen's X and Y readings, while it touches the screen.
    pen: Option<(u16, u16)>,
    /// The result shifting out, next bit at bit 15.
    output: u16,
    /// How loud the microphone hears the player, 0–255. Set from every
    /// frame's input before it runs, so not saved.
    #[serde(skip)]
    microphone: u8,
}

impl Touch {
    /// Sets the pen's 12-bit X and Y readings, or `None` when lifted.
    pub(crate) fn set_pen(&mut self, readings: Option<(u16, u16)>) {
        self.pen = readings;
    }

    /// Sets how loud the microphone hears the player: 0 is silence, 255
    /// full-scale noise.
    pub(crate) fn set_microphone(&mut self, loudness: u8) {
        self.microphone = loudness;
    }

    /// Whether the pen touches the screen: /PENIRQ, EXTKEYIN bit 6.
    pub(crate) fn pen_down(&self) -> bool {
        self.pen.is_some()
    }

    /// Shifts one byte in and returns the byte shifted out. `now`, the
    /// ARM7's bus cycle, is when a conversion samples the microphone.
    pub(crate) fn transfer(&mut self, input: u8, now: u64) -> u8 {
        let out = (self.output >> 8) as u8;
        self.output <<= 8;
        if input & START != 0 {
            let value = self.convert((input >> 4) & 7, now);
            // After the zero bit: 12 bits end at bit 3, 8 bits at bit 7.
            self.output = if input & EIGHT_BIT != 0 {
                (value >> 4) << 7
            } else {
                value << 3
            };
        }
        out
    }

    /// Chip select goes high.
    pub(crate) fn release(&mut self) {
        self.output = 0;
    }

    /// The 12-bit reading of `channel` at bus cycle `now`.
    fn convert(&self, channel: u8, now: u64) -> u16 {
        let pressure = if self.pen.is_some() {
            PRESSED_Z
        } else {
            RELEASED_Z
        };
        match channel {
            TEMPERATURE_0 => TEMPERATURES.0,
            Y => self.pen.map_or(RELEASED_Y, |(_, y)| y),
            // The battery input is grounded on the DS.
            BATTERY => 0,
            Z1 => pressure.0,
            Z2 => pressure.1,
            X => self.pen.map_or(RELEASED_X, |(x, _)| x),
            MICROPHONE => microphone_sample(self.microphone, now),
            TEMPERATURE_1 => TEMPERATURES.1,
            _ => 0,
        }
    }
}

/// The microphone's 12-bit reading at bus cycle `now`: noise spread
/// `loudness` / 255 of the way from mid-scale to either end. The noise is
/// a hash of the time (SplitMix64's mixing function), so it needs no state
/// and repeats exactly on every run.
fn microphone_sample(loudness: u8, now: u64) -> u16 {
    let mut hash = now;
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    hash ^= hash >> 31;
    // The top 12 bits, centred on zero: -0x800 to 0x7FF.
    let noise = (hash >> 52) as i32 - i32::from(SILENCE);
    let sample = i32::from(SILENCE) + noise * i32::from(loudness) / 255;
    sample as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Converts `channel` as libnds does: control byte, two reads, the
    /// second carrying the next control byte or zero.
    fn read(touch: &mut Touch, channel: u8) -> u16 {
        read_at(touch, channel, 0)
    }

    /// [`read`], converting at bus cycle `now`.
    fn read_at(touch: &mut Touch, channel: u8, now: u64) -> u16 {
        touch.transfer(START | channel << 4, now);
        let high = touch.transfer(0, now);
        let low = touch.transfer(0, now);
        touch.release();
        u16::from(high & 0x7F) << 5 | u16::from(low >> 3)
    }

    /// An 8-bit conversion at bus cycle `now`, as libnds's
    /// `micReadData8` does (control byte 0xEC for the microphone).
    fn read_8_at(touch: &mut Touch, channel: u8, now: u64) -> u8 {
        touch.transfer(START | EIGHT_BIT | channel << 4 | 0x04, now);
        let high = touch.transfer(0, now);
        let low = touch.transfer(0, now);
        touch.release();
        (high & 0x7F) << 1 | low >> 7
    }

    #[test]
    fn reads_the_pen_in_12_bits() {
        let mut touch = Touch::default();
        assert_eq!((read(&mut touch, X), read(&mut touch, Y)), (0, 0xFFF));
        assert!(!touch.pen_down());
        touch.set_pen(Some((0x9AB, 0x123)));
        assert!(touch.pen_down());
        assert_eq!((read(&mut touch, X), read(&mut touch, Y)), (0x9AB, 0x123));
        assert_eq!(read(&mut touch, MICROPHONE), SILENCE);
    }

    #[test]
    fn a_new_command_can_overlap_the_last_byte() {
        let mut touch = Touch::default();
        touch.set_pen(Some((0x9AB, 0x123)));
        touch.transfer(START | X << 4, 0);
        let high = touch.transfer(0, 0);
        let low = touch.transfer(START | Y << 4, 0);
        assert_eq!(u16::from(high) << 5 | u16::from(low >> 3), 0x9AB);
        let high = touch.transfer(0, 0);
        let low = touch.transfer(0, 0);
        assert_eq!(u16::from(high) << 5 | u16::from(low >> 3), 0x123);
    }

    #[test]
    fn eight_bit_mode_drops_the_low_bits() {
        let mut touch = Touch::default();
        touch.set_pen(Some((0x9AB, 0x123)));
        touch.transfer(START | EIGHT_BIT | X << 4, 0);
        let high = touch.transfer(0, 0);
        let low = touch.transfer(0, 0);
        assert_eq!(u16::from(high) << 1 | u16::from(low >> 7), 0x9A);
    }

    /// Readings of the microphone at loudness `loudness`, sampled every
    /// 2,048 bus cycles (a 16 kHz timer).
    fn microphone_readings(loudness: u8) -> Vec<u16> {
        let mut touch = Touch::default();
        touch.set_microphone(loudness);
        (0..2000)
            .map(|n| read_at(&mut touch, MICROPHONE, 1_000_000 + n * 2048))
            .collect()
    }

    #[test]
    fn a_quiet_microphone_reads_mid_scale() {
        let mut touch = Touch::default();
        assert!(microphone_readings(0).iter().all(|&value| value == SILENCE));
        assert_eq!(read_8_at(&mut touch, MICROPHONE, 12_345), 0x80);
    }

    #[test]
    fn blowing_reads_as_full_scale_noise() {
        let readings = microphone_readings(255);
        let low = readings.iter().filter(|&&value| value < 0x200).count();
        let high = readings.iter().filter(|&&value| value > 0xE00).count();
        // Uniform noise: about an eighth of the readings at each end.
        assert!(low > 150 && high > 150, "{low} low, {high} high");
        let mean = readings.iter().map(|&value| u64::from(value)).sum::<u64>() / 2000;
        assert!((0x780..0x880).contains(&mean), "mean {mean:#X}");
        assert!(readings.iter().all(|&value| value <= 0xFFF));
    }

    #[test]
    fn loudness_scales_the_noise() {
        let quiet = microphone_readings(64);
        let spread = |value: &u16| i32::from(*value) - i32::from(SILENCE);
        assert!(
            quiet
                .iter()
                .all(|value| spread(value).abs() <= 0x800 * 64 / 255)
        );
        assert!(quiet.iter().any(|value| spread(value).abs() > 0x180));
    }

    #[test]
    fn the_same_moment_reads_the_same_sample_in_8_and_12_bits() {
        let mut touch = Touch::default();
        touch.set_microphone(255);
        for now in [7, 99_999, 1 << 40] {
            let twelve = read_at(&mut touch, MICROPHONE, now);
            assert_eq!(read_at(&mut touch, MICROPHONE, now), twelve, "repeatable");
            assert_eq!(
                u16::from(read_8_at(&mut touch, MICROPHONE, now)),
                twelve >> 4
            );
        }
    }
}
