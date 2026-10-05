//! The ARM9's division and square-root unit (GBATEK, "DS Maths").
//!
//! Writing an operand or a control register recomputes the result at once.
//! The hardware takes a few dozen cycles; the busy bits always read zero
//! here, which games that poll them accept.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Math {
    div_control: u32,
    numerator: u64,
    denominator: u64,
    quotient: u64,
    remainder: u64,
    sqrt_control: u32,
    sqrt_input: u64,
    sqrt_result: u32,
}

impl Math {
    /// Reads the 32-bit register at `offset` from 0x04000280.
    pub(crate) fn read(&self, offset: u32) -> u32 {
        let low = |value: u64| value as u32;
        let high = |value: u64| (value >> 32) as u32;
        match offset {
            0x00 => self.div_control | u32::from(self.denominator == 0) << 14,
            0x10 => low(self.numerator),
            0x14 => high(self.numerator),
            0x18 => low(self.denominator),
            0x1C => high(self.denominator),
            0x20 => low(self.quotient),
            0x24 => high(self.quotient),
            0x28 => low(self.remainder),
            0x2C => high(self.remainder),
            0x30 => self.sqrt_control,
            0x34 => self.sqrt_result,
            0x38 => low(self.sqrt_input),
            0x3C => high(self.sqrt_input),
            _ => 0,
        }
    }

    /// Writes the 32-bit register at `offset` from 0x04000280.
    pub(crate) fn write(&mut self, offset: u32, value: u32) {
        let set_low = |target: &mut u64| *target = (*target & !0xFFFF_FFFF) | u64::from(value);
        let set_high = |target: &mut u64| *target = (*target & 0xFFFF_FFFF) | u64::from(value) << 32;
        match offset {
            0x00 => self.div_control = value & 3,
            0x10 => set_low(&mut self.numerator),
            0x14 => set_high(&mut self.numerator),
            0x18 => set_low(&mut self.denominator),
            0x1C => set_high(&mut self.denominator),
            0x30 => self.sqrt_control = value & 1,
            0x38 => set_low(&mut self.sqrt_input),
            0x3C => set_high(&mut self.sqrt_input),
            _ => return,
        }
        if offset < 0x30 {
            self.divide();
        } else {
            self.square_root();
        }
    }

    fn divide(&mut self) {
        let (quotient, remainder) = match self.div_control {
            0 => divide_32(self.numerator as i32, self.denominator as i32),
            1 => divide_64(self.numerator as i64, i64::from(self.denominator as i32)),
            _ => divide_64(self.numerator as i64, self.denominator as i64),
        };
        self.quotient = quotient as u64;
        self.remainder = remainder as u64;
    }

    fn square_root(&mut self) {
        let input = if self.sqrt_control == 0 {
            self.sqrt_input & 0xFFFF_FFFF
        } else {
            self.sqrt_input
        };
        self.sqrt_result = input.isqrt() as u32;
    }
}

/// 32/32 division. By zero, the quotient is ±1 against the numerator's
/// sign, with its high word of the other sign, and the remainder is the
/// numerator.
fn divide_32(numerator: i32, denominator: i32) -> (i64, i64) {
    if denominator == 0 {
        let low: i32 = if numerator < 0 { 1 } else { -1 };
        let quotient = i64::from(-low) << 32 | i64::from(low as u32);
        return (quotient, i64::from(numerator));
    }
    if numerator == i32::MIN && denominator == -1 {
        // The quotient overflows 32 bits but not the 64-bit result.
        return (0x8000_0000, 0);
    }
    (
        i64::from(numerator / denominator),
        i64::from(numerator % denominator),
    )
}

fn divide_64(numerator: i64, denominator: i64) -> (i64, i64) {
    if denominator == 0 {
        return (if numerator < 0 { 1 } else { -1 }, numerator);
    }
    if numerator == i64::MIN && denominator == -1 {
        return (i64::MIN, 0);
    }
    (numerator / denominator, numerator % denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(math: &Math) -> (u64, u64) {
        let pair = |low: u32, high: u32| u64::from(math.read(low)) | u64::from(math.read(high)) << 32;
        (pair(0x20, 0x24), pair(0x28, 0x2C))
    }

    #[test]
    fn divides_in_each_mode() {
        let mut math = Math::default();
        math.write(0x10, (-7i32) as u32);
        math.write(0x18, 2);
        assert_eq!(result(&math), ((-3i64) as u64, (-1i64) as u64));
        math.write(0x00, 2); // 64/64
        math.write(0x14, 0);
        math.write(0x10, 100);
        math.write(0x1C, 0);
        math.write(0x18, 7);
        assert_eq!(result(&math), (14, 2));
    }

    #[test]
    fn division_by_zero_flags_and_returns_plus_or_minus_one() {
        let mut math = Math::default();
        math.write(0x10, 5);
        math.write(0x18, 0);
        assert_ne!(math.read(0x00) & 1 << 14, 0);
        let (quotient, remainder) = result(&math);
        assert_eq!(quotient, 0x0000_0001_FFFF_FFFF);
        assert_eq!(remainder, 5);
    }

    #[test]
    fn square_roots_truncate() {
        let mut math = Math::default();
        math.write(0x38, 99);
        assert_eq!(math.read(0x34), 9);
        math.write(0x30, 1); // 64-bit input
        math.write(0x3C, 1);
        math.write(0x38, 0);
        assert_eq!(math.read(0x34), 65_536);
    }
}
