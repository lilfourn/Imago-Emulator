//! The barrel shifter and flag-setting arithmetic shared by ARM and Thumb
//! (ARM7TDMI Technical Reference Manual, "Shifts"; GBATEK, "ARM Opcodes:
//! Data Processing").

/// A shift type, as encoded in bits 5–6 of ARM data-processing operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shift {
    Lsl,
    Lsr,
    Asr,
    Ror,
}

impl Shift {
    pub(crate) const fn from_bits(bits: u32) -> Self {
        match bits & 3 {
            0 => Self::Lsl,
            1 => Self::Lsr,
            2 => Self::Asr,
            _ => Self::Ror,
        }
    }
}

/// Shifts by an immediate amount (0–31), with the encodings where 0 means
/// something else: LSR #0 and ASR #0 shift by 32, ROR #0 is RRX. Returns the
/// result and the shifter carry-out.
pub(crate) fn shift_immediate(shift: Shift, value: u32, amount: u32, carry: bool) -> (u32, bool) {
    match (shift, amount) {
        (Shift::Lsl, 0) => (value, carry),
        (Shift::Lsr | Shift::Asr, 0) => shift_register(shift, value, 32, carry),
        (Shift::Ror, 0) => (u32::from(carry) << 31 | value >> 1, value & 1 != 0),
        _ => shift_register(shift, value, amount, carry),
    }
}

/// Shifts by an amount from a register (its bottom byte, 0–255). An amount
/// of 0 leaves value and carry alone; amounts of 32 or more follow the
/// architecture's rules for each shift.
pub(crate) fn shift_register(shift: Shift, value: u32, amount: u32, carry: bool) -> (u32, bool) {
    if amount == 0 {
        return (value, carry);
    }
    match shift {
        Shift::Lsl => match amount {
            1..=31 => (value << amount, value >> (32 - amount) & 1 != 0),
            32 => (0, value & 1 != 0),
            _ => (0, false),
        },
        Shift::Lsr => match amount {
            1..=31 => (value >> amount, value >> (amount - 1) & 1 != 0),
            32 => (0, value >> 31 != 0),
            _ => (0, false),
        },
        Shift::Asr => match amount {
            1..=31 => (
                ((value as i32) >> amount) as u32,
                value >> (amount - 1) & 1 != 0,
            ),
            _ => {
                let sign = value >> 31 != 0;
                (if sign { u32::MAX } else { 0 }, sign)
            }
        },
        Shift::Ror => {
            let amount = amount % 32;
            if amount == 0 {
                (value, value >> 31 != 0)
            } else {
                (value.rotate_right(amount), value >> (amount - 1) & 1 != 0)
            }
        }
    }
}

/// `a + b + carry_in` with (result, carry, overflow).
pub(crate) fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let wide = u64::from(a) + u64::from(b) + u64::from(carry_in);
    let result = wide as u32;
    let overflow = (!(a ^ b) & (a ^ result)) >> 31 != 0;
    (result, wide > u64::from(u32::MAX), overflow)
}

/// `a - b`, as ARM computes it: `a + !b + 1`. Carry means "no borrow".
pub(crate) fn subtract(a: u32, b: u32) -> (u32, bool, bool) {
    add_with_carry(a, !b, true)
}

/// `a - b - !carry_in`.
pub(crate) fn subtract_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    add_with_carry(a, !b, carry_in)
}

/// `a + b` as signed 32-bit values, clamped to the signed range (ARMv5TE
/// QADD), with whether it clamped.
pub(crate) fn saturating_add(a: u32, b: u32) -> (u32, bool) {
    let (a, b) = (a as i32, b as i32);
    match a.checked_add(b) {
        Some(sum) => (sum as u32, false),
        None => (a.saturating_add(b) as u32, true),
    }
}

/// `a - b` as signed 32-bit values, clamped to the signed range (ARMv5TE
/// QSUB), with whether it clamped.
pub(crate) fn saturating_subtract(a: u32, b: u32) -> (u32, bool) {
    let (a, b) = (a as i32, b as i32);
    match a.checked_sub(b) {
        Some(difference) => (difference as u32, false),
        None => (a.saturating_sub(b) as u32, true),
    }
}

/// Internal cycles a multiply spends, from the multiplier's leading bits:
/// 1 if its top 24 bits are all zeros or all ones, 2 for the top 16, 3 for
/// the top 8, else 4. Unsigned multiplies only stop early on zeros.
pub(crate) fn multiply_cycles(multiplier: u32, signed: bool) -> u32 {
    let matches = |mask: u32| {
        let top = multiplier & mask;
        top == 0 || (signed && top == mask)
    };
    if matches(0xFFFF_FF00) {
        1
    } else if matches(0xFFFF_0000) {
        2
    } else if matches(0xFF00_0000) {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_zero_encodings() {
        assert_eq!(
            shift_immediate(Shift::Lsr, 0x8000_0000, 0, false),
            (0, true)
        );
        assert_eq!(
            shift_immediate(Shift::Asr, 0x8000_0000, 0, false),
            (u32::MAX, true)
        );
        assert_eq!(shift_immediate(Shift::Ror, 3, 0, true), (0x8000_0001, true));
    }

    #[test]
    fn register_shifts_past_31() {
        assert_eq!(shift_register(Shift::Lsl, 1, 32, false), (0, true));
        assert_eq!(shift_register(Shift::Lsl, 1, 33, true), (0, false));
        assert_eq!(
            shift_register(Shift::Ror, 0x8000_0001, 32, false),
            (0x8000_0001, true)
        );
        assert_eq!(shift_register(Shift::Lsr, 5, 0, true), (5, true));
    }

    #[test]
    fn arithmetic_flags() {
        assert_eq!(
            add_with_carry(0x7FFF_FFFF, 1, false),
            (0x8000_0000, false, true)
        );
        assert_eq!(subtract(0, 1), (u32::MAX, false, false));
        assert_eq!(subtract(5, 5), (0, true, false));
    }

    #[test]
    fn saturation() {
        assert_eq!(saturating_add(0x7FFF_FFFF, 1), (0x7FFF_FFFF, true));
        assert_eq!(saturating_add(0x8000_0000, u32::MAX), (0x8000_0000, true));
        assert_eq!(saturating_add(u32::MAX, 1), (0, false));
        assert_eq!(saturating_subtract(0x8000_0000, 1), (0x8000_0000, true));
        assert_eq!(
            saturating_subtract(0x7FFF_FFFF, u32::MAX),
            (0x7FFF_FFFF, true)
        );
        assert_eq!(saturating_subtract(0, 1), (u32::MAX, false));
    }
}
