//! The ARM7TDMI's multiplier, simulated to find the carry flag that its
//! flag-setting multiplies leave behind.
//!
//! ARM documents C after MULS, MLAS, UMULLS and the rest as meaningless,
//! but it is not random: it is a bit of the multiplier's internal state.
//! The multiplier recodes the multiplier operand into radix-4 Booth digits,
//! feeds four addends a cycle through a chain of carry-save adders, stops
//! early once the remaining multiplier bits are all zeros (or, signed, all
//! ones), and lets the ALU add the final sum and carry vectors. C is the
//! barrel shifter's carry-out from that last step, one bit of the carry
//! vector. See zaydlang and calc84maniac, "Solving the Mystery of ARM7TDMI
//! Multiply Carry Flag" (2024, <https://bmchtech.github.io/post/multiply/>).
//!
//! This module is altered from the original source: it is a Rust
//! translation of `impl.h` from <https://github.com/zaydlang/multiplication-algorithm>,
//! restructured into functions without global state, which this project
//! uses to compute the carry flag (the product it also computes is checked
//! against ordinary multiplication in the tests). The original's license
//! follows.
//!
//! > Copyright (c) 2024 zaydlang
//! >
//! > This software is provided 'as-is', without any express or implied
//! > warranty. In no event will the authors be held liable for any damages
//! > arising from the use of this software.
//! >
//! > Permission is granted to anyone to use this software for any purpose,
//! > including commercial applications, and to alter it and redistribute it
//! > freely, subject to the following restrictions:
//! >
//! > 1. The origin of this software must not be misrepresented; you must not
//! >    claim that you wrote the original software. If you use this software
//! >    in a product, an acknowledgment in the product documentation would be
//! >    appreciated but is not required.
//! > 2. Altered source versions must be plainly marked as such, and must not
//! >    be misrepresented as being the original software.
//! > 3. This notice may not be removed or altered from any source
//! >    distribution.

/// Which multiply instruction is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// MUL and MLA (and Thumb MUL), which stop early like a signed multiply.
    Short,
    /// SMULL and SMLAL.
    LongSigned,
    /// UMULL and UMLAL.
    LongUnsigned,
}

impl Kind {
    fn long(self) -> bool {
        self != Self::Short
    }

    fn signed(self) -> bool {
        self != Self::LongUnsigned
    }
}

/// What the multiplier produces: the result the ALU adds up, and the
/// carry flag the instruction leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) product: u64,
    pub(crate) carry: bool,
}

/// Bit `n` of `value`, as 0 or 1.
fn bit(value: u64, n: u32) -> u64 {
    (value >> n) & 1
}

/// `value` with bit `from - 1` copied into every bit above it.
fn sign_extend(value: u64, from: u32) -> u64 {
    if bit(value, from - 1) != 0 {
        value | (u64::MAX << from)
    } else {
        value
    }
}

/// `value`, a `size`-bit number, shifted right arithmetically by `shift`
/// and kept to `size` bits.
fn asr(value: u64, shift: u32, size: u32) -> u64 {
    let shifted = (sign_extend(value, size) as i64) >> shift;
    shifted as u64 & ((1 << size) - 1)
}

/// One addend from Booth recoding: the multiplicand times a digit from −2
/// to 2, with negation done by inverting and the +1 left as `carry`.
#[derive(Debug, Clone, Copy)]
struct Addend {
    value: u64,
    carry: bool,
}

/// The addend for the three multiplier bits in `chunk`.
fn booth_recode(multiplicand: u64, chunk: u64) -> Addend {
    let (value, carry) = match chunk {
        1 | 2 => (multiplicand, false),
        3 => (multiplicand << 1, false),
        4 => (!(multiplicand << 1), true),
        5 | 6 => (!multiplicand, true),
        _ => (0, false),
    };
    Addend {
        value: value & 0x3_FFFF_FFFF,
        carry,
    }
}

/// A sum and carry vector pair, as the carry-save adders pass along.
#[derive(Debug, Clone, Copy, Default)]
struct Vectors {
    sum: u64,
    carry: u64,
}

/// One carry-save adder: three inputs in, a sum and a carry vector out.
fn carry_save_add(a: u64, b: u64, c: u64) -> Vectors {
    Vectors {
        sum: a ^ b ^ c,
        carry: (a & b) | (b & c) | (c & a),
    }
}

/// One cycle through the chain of four carry-save adders, adding the four
/// `addends` to the vectors from the cycle before. `accumulator_high`
/// holds the accumulator's high bits, which the chain feeds in two at a
/// time.
fn carry_save_cycle(input: Vectors, addends: [Addend; 4], accumulator_high: &mut u64) -> Vectors {
    let mut vectors = input;
    let mut result = Vectors::default();
    for (index, addend) in addends.iter().enumerate() {
        vectors.sum &= 0x1_FFFF_FFFF;
        vectors.carry &= 0x1_FFFF_FFFF;
        let mut next = carry_save_add(vectors.sum, addend.value & 0x1_FFFF_FFFF, vectors.carry);
        // The +1 that completes a negated addend goes in the free low bit.
        next.carry = next.carry << 1 | u64::from(addend.carry);
        // Later addends are at least four times larger, so the low two bits
        // are final and go straight to the result.
        result.sum |= (next.sum & 3) << (2 * index);
        result.carry |= (next.carry & 3) << (2 * index);
        next.sum >>= 2;
        next.carry >>= 2;
        // Sign extension and the accumulator's high bits, fed in at the top.
        let magic =
            bit(*accumulator_high, 0) + (1 - bit(vectors.carry, 32)) + (1 - bit(addend.value, 33));
        next.sum |= magic << 31;
        next.carry |= (1 - bit(*accumulator_high, 1)) << 32;
        *accumulator_high >>= 2;
        vectors = next;
    }
    result.sum |= vectors.sum << 8;
    result.carry |= vectors.carry << 8;
    result
}

/// A 32-bit add with carry in and out, as the ALU does it.
fn add(a: u64, b: u64, carry: bool) -> (u32, bool) {
    let total = (a & 0xFFFF_FFFF) + (b & 0xFFFF_FFFF) + u64::from(carry);
    (total as u32, total > 0xFFFF_FFFF)
}

/// Runs the multiplier on `multiplicand` (Rm) times `multiplier` (Rs),
/// plus `accumulator` (Rn, or RdHi:RdLo, or 0).
pub(crate) fn multiply(kind: Kind, multiplicand: u32, multiplier: u32, accumulator: u64) -> Output {
    let widen = |value: u32| {
        if kind.signed() {
            sign_extend(u64::from(value), 32) & 0x3_FFFF_FFFF
        } else {
            u64::from(value)
        }
    };
    let alu_carry_in = multiplier & 1 != 0;
    let mut multiplier = widen(multiplier);
    let multiplicand = widen(multiplicand);

    let mut vectors = Vectors {
        sum: accumulator,
        carry: if multiplier & 1 != 0 {
            !multiplicand
        } else {
            0
        },
    };
    let mut accumulator_high = accumulator >> 34;
    // The result's bits, collected in the low end and rotated right as they
    // come in.
    let mut sum = u128::from(vectors.sum & 1).rotate_right(1);
    let mut carry = u128::from(vectors.carry & 1).rotate_right(1);
    vectors.sum >>= 1;
    vectors.carry >>= 1;

    let mut iterations = 0;
    loop {
        let addends =
            [0, 1, 2, 3].map(|index| booth_recode(multiplicand, (multiplier >> (2 * index)) & 7));
        vectors = carry_save_cycle(vectors, addends, &mut accumulator_high);
        sum = (sum | u128::from(vectors.sum & 0xFF)).rotate_right(8);
        carry = (carry | u128::from(vectors.carry & 0xFF)).rotate_right(8);
        vectors.sum >>= 8;
        vectors.carry >>= 8;
        multiplier = asr(multiplier, 8, 33);
        iterations += 1;
        let done = multiplier == 0 || (kind.signed() && multiplier == 0x1_FFFF_FFFF);
        if done {
            break;
        }
    }
    sum |= u128::from(vectors.sum);
    carry |= u128::from(vectors.carry);

    // Undo the rotations. (The reference's authors note these amounts are
    // one off from the patents' tables, unexplained.)
    let correction = [23, 15, 7, 31][iterations - 1];
    let sum = sum.rotate_right(correction);
    let carry = carry.rotate_right(correction);
    let (sum_low, sum_high) = (sum as u64, (sum >> 64) as u64);
    let (carry_low, carry_high) = (carry as u64, (carry >> 64) as u64);

    if !kind.long() {
        let (product, flag) = if iterations == 4 {
            (
                add(sum_high, carry_high, alu_carry_in).0,
                bit(carry_high, 31),
            )
        } else {
            let product = add(sum_high >> 32, carry_high >> 32, alu_carry_in).0;
            (product, bit(carry_high, 63))
        };
        return Output {
            product: u64::from(product),
            carry: flag != 0,
        };
    }
    let (low, high) = if iterations == 4 {
        let (low, carry_out) = add(sum_high, carry_high, alu_carry_in);
        (low, add(sum_high >> 32, carry_high >> 32, carry_out).0)
    } else {
        let (low, carry_out) = add(sum_high >> 32, carry_high >> 32, alu_carry_in);
        // The reference's authors found this extra bit by experiment.
        let shift = 8 * iterations as u32 + 2;
        let carry_low = sign_extend(carry_low, shift);
        let sum_low = sum_low | accumulator_high << shift;
        (low, add(sum_low, carry_low, carry_out).0)
    };
    Output {
        product: u64::from(high) << 32 | u64::from(low),
        carry: bit(carry_high, 63) != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A few thousand operand pairs: edge values and a simple generator.
    fn operands() -> Vec<u32> {
        let mut values = vec![
            0,
            1,
            2,
            0x7F,
            0x80,
            0x81,
            0xFF,
            0x100,
            0xFFFF,
            0x1_0000,
            0x7FFF_FFFF,
            0x8000_0000,
            0x8000_0001,
            0xFFFF_FF00,
            0xFFFF_FFFE,
            u32::MAX,
        ];
        let mut state = 0x1234_5678_u32;
        for _ in 0..48 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            values.push(state >> (state % 29));
        }
        values
    }

    #[test]
    fn the_simulated_multiplier_computes_products() {
        for &a in &operands() {
            for &b in &operands() {
                let accumulator = u64::from(a ^ b.rotate_left(7)) << 32 | u64::from(b ^ 0x5555);
                let short = multiply(Kind::Short, a, b, accumulator & 0xFFFF_FFFF);
                let expected = a.wrapping_mul(b).wrapping_add(accumulator as u32);
                assert_eq!(short.product, u64::from(expected), "{a:x} * {b:x}");
                let unsigned = multiply(Kind::LongUnsigned, a, b, accumulator);
                let expected = (u64::from(a) * u64::from(b)).wrapping_add(accumulator);
                assert_eq!(unsigned.product, expected, "{a:x} * {b:x}");
                let signed = multiply(Kind::LongSigned, a, b, accumulator);
                let expected = (i64::from(a as i32) * i64::from(b as i32)) as u64;
                assert_eq!(signed.product, expected.wrapping_add(accumulator));
            }
        }
    }

    /// Carry flags the mGBA suite's multiply long tests measured on a GBA.
    #[test]
    fn long_multiplies_leave_the_hardwares_carry() {
        let umull = |a: u32, b: u32| multiply(Kind::LongUnsigned, a, b, 0).carry;
        let smull = |a: u32, b: u32| multiply(Kind::LongSigned, a, b, 0).carry;
        assert!(umull(u32::MAX, u32::MAX));
        assert!(umull(0x7FFF_FFFF, u32::MAX));
        assert!(smull(u32::MAX, 0x7FFF_FFFF));
        assert!(umull(u32::MAX, 0x7FFF_FFFF));
        assert!(smull(0, 0x8000_0000));
        assert!(umull(0x8000_0000, 0x8000_0000));
        assert!(!smull(1, 1));
        assert!(!umull(0, 0));
    }
}
