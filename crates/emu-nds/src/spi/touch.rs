//! The TSC2046 touchscreen controller (GBATEK, "DS Touch Screen Controller
//! (TSC)").
//!
//! A control byte with bit 7 set starts a conversion of the channel in bits
//! 4-6, 12-bit unless bit 3 asks for 8. The result shifts out over the next
//! two bytes, most significant bit first, after one zero bit: a 12-bit
//! value V reads as V >> 5 and then (V << 3) & FFh. A new control byte may
//! go out while the second byte of the last result comes in.

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
}

impl Touch {
    /// Sets the pen's 12-bit X and Y readings, or `None` when lifted.
    pub(crate) fn set_pen(&mut self, readings: Option<(u16, u16)>) {
        self.pen = readings;
    }

    /// Whether the pen touches the screen: /PENIRQ, EXTKEYIN bit 6.
    pub(crate) fn pen_down(&self) -> bool {
        self.pen.is_some()
    }

    /// Shifts one byte in and returns the byte shifted out.
    pub(crate) fn transfer(&mut self, input: u8) -> u8 {
        let out = (self.output >> 8) as u8;
        self.output <<= 8;
        if input & START != 0 {
            let value = self.convert((input >> 4) & 7);
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

    /// The 12-bit reading of `channel`.
    fn convert(&self, channel: u8) -> u16 {
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
            MICROPHONE => SILENCE,
            TEMPERATURE_1 => TEMPERATURES.1,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Converts `channel` as libnds does: control byte, two reads, the
    /// second carrying the next control byte or zero.
    fn read(touch: &mut Touch, channel: u8) -> u16 {
        touch.transfer(START | channel << 4);
        let high = touch.transfer(0);
        let low = touch.transfer(0);
        touch.release();
        u16::from(high & 0x7F) << 5 | u16::from(low >> 3)
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
        touch.transfer(START | X << 4);
        let high = touch.transfer(0);
        let low = touch.transfer(START | Y << 4);
        assert_eq!(u16::from(high) << 5 | u16::from(low >> 3), 0x9AB);
        let high = touch.transfer(0);
        let low = touch.transfer(0);
        assert_eq!(u16::from(high) << 5 | u16::from(low >> 3), 0x123);
    }

    #[test]
    fn eight_bit_mode_drops_the_low_bits() {
        let mut touch = Touch::default();
        touch.set_pen(Some((0x9AB, 0x123)));
        touch.transfer(START | EIGHT_BIT | X << 4);
        let high = touch.transfer(0);
        let low = touch.transfer(0);
        assert_eq!(u16::from(high) << 1 | u16::from(low >> 7), 0x9A);
    }
}
