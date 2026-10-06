//! The ARMv5TE additions to the ARM instruction set (ARM Architecture
//! Reference Manual, ARMv5TE; GBATEK, "ARM Opcodes"): CLZ, BLX, BKPT, the
//! saturating Q instructions, the signed halfword multiplies, LDRD/STRD,
//! PLD, and register transfers to coprocessors. Only models with `V5TE`
//! reach this code.
//!
//! Timing follows GBATEK's ARM9 notes loosely: one sequential cycle for
//! CLZ, the Q instructions and the 32-bit multiplies, and an internal cycle
//! more for SMLALxy; LDRD and STRD use the bus like a two-register LDM or
//! STM; MRC and MCR spend one internal cycle talking to CP15.

use crate::alu;
use crate::{Access, Bus, Mode, Model, Processor, psr};

/// The register number in the four bits at `shift`.
fn reg(instruction: u32, shift: u32) -> usize {
    ((instruction >> shift) & 0xF) as usize
}

/// The signed bottom or top halfword of `value`.
fn half(value: u32, top: bool) -> i32 {
    i32::from((if top { value >> 16 } else { value }) as u16 as i16)
}

impl<M: Model> Processor<M> {
    /// Runs `instruction` if it is one of the ARMv5TE encodings that ARMv4
    /// decodes as something else; returns whether it was.
    pub(crate) fn execute_arm_v5<B: Bus>(&mut self, bus: &mut B, instruction: u32) -> bool {
        let key = instruction & 0x0FF0_00F0;
        if key & 0x0F90_0000 == 0x0100_0000 && key & 0x90 != 0x90 {
            // The space ARMv4 gives to MRS, MSR and BX. (Bits 7 and 4 both
            // set mean SWP or a halfword or doubleword transfer instead.)
            match key & 0x0060_00F0 {
                0x0060_0010 => self.count_leading_zeros(bus, instruction),
                0x0020_0030 => self.branch_link_exchange_register(bus, instruction),
                0x0020_0070 => self.breakpoint(bus),
                _ if key & 0xF0 == 0x50 => self.saturating_arithmetic(bus, instruction),
                _ if key & 0x90 == 0x80 => self.signed_halfword_multiply(bus, instruction),
                _ => return false,
            }
        } else if key & 0x0E10_00D0 == 0x0000_00D0 {
            self.doubleword_transfer(bus, instruction);
        } else if key & 0x0F00_0010 == 0x0E00_0010 {
            self.coprocessor_register_transfer(bus, instruction);
        } else {
            return false;
        }
        true
    }

    /// Runs an instruction with condition 0b1111, which ARMv5 uses for BLX
    /// with an immediate, PLD, and the coprocessor "2" forms.
    pub(crate) fn execute_unconditional<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        if instruction & 0x0E00_0000 == 0x0A00_0000 {
            self.branch_link_exchange_immediate(bus, instruction);
        } else if instruction & 0x0C00_0000 == 0x0C00_0000 {
            // CDP2, LDC2, STC2, MCR2 and MRC2: CP15 takes none of them.
            self.undefined(bus);
        } else {
            // PLD (a cache hint), and the encodings ARMv5 leaves
            // unpredictable, do nothing, as condition "never" did on ARMv4.
            self.advance();
        }
    }

    /// Writes an instruction's result and moves on. A result for R15
    /// (unpredictable for these instructions) jumps, as data processing
    /// does.
    fn finish_write<B: Bus>(&mut self, bus: &mut B, rd: usize, value: u32) {
        self.r[rd] = value;
        if rd == 15 {
            self.reload_pipeline(bus);
        } else {
            self.advance();
        }
    }

    /// CLZ: the number of leading zero bits in Rm.
    fn count_leading_zeros<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let count = self.r[reg(instruction, 0)].leading_zeros();
        self.finish_write(bus, reg(instruction, 12), count);
    }

    /// BLX Rm: calls Rm, in Thumb if its bit 0 is set.
    fn branch_link_exchange_register<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let target = self.r[reg(instruction, 0)];
        self.r[14] = self.r[15].wrapping_sub(4);
        self.set_flag(psr::T, target & 1 != 0);
        self.r[15] = target & !1;
        self.reload_pipeline(bus);
    }

    /// BLX label: calls Thumb code at PC + offset × 4 + H × 2.
    fn branch_link_exchange_immediate<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let offset = ((instruction << 8) as i32 >> 6) as u32 | ((instruction >> 23) & 2);
        self.r[14] = self.r[15].wrapping_sub(4);
        self.set_flag(psr::T, true);
        self.r[15] = self.r[15].wrapping_add(offset);
        self.reload_pipeline(bus);
    }

    /// QADD, QSUB, QDADD, QDSUB: Rd = Rm ± Rn, where the D forms first
    /// double Rn; each step saturates, and any saturation sets the sticky
    /// Q flag.
    fn saturating_arithmetic<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let rm = self.r[reg(instruction, 0)];
        let rn = self.r[reg(instruction, 16)];
        let (operand, doubling_saturated) = if instruction & (1 << 22) != 0 {
            alu::saturating_add(rn, rn)
        } else {
            (rn, false)
        };
        let (result, saturated) = if instruction & (1 << 21) != 0 {
            alu::saturating_subtract(rm, operand)
        } else {
            alu::saturating_add(rm, operand)
        };
        if doubling_saturated || saturated {
            self.cpsr |= psr::Q;
        }
        self.finish_write(bus, reg(instruction, 12), result);
    }

    /// SMLAxy, SMLAWy, SMULWy, SMLALxy and SMULxy: signed multiplies of
    /// halfwords, where bit 5 picks Rm's top half and bit 6 Rs's (the W
    /// forms use all of Rm and keep the product's top 32 of 48 bits). The
    /// 32-bit accumulations wrap and set Q on overflow; no other flags
    /// change.
    fn signed_halfword_multiply<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let rd = reg(instruction, 16);
        let rn = reg(instruction, 12);
        let rm = self.r[reg(instruction, 0)];
        let rm_top = instruction & (1 << 5) != 0;
        let rs = half(self.r[reg(instruction, 8)], instruction & (1 << 6) != 0);
        match (instruction >> 21) & 3 {
            0 => {
                let sum = self.add_setting_q(half(rm, rm_top) * rs, self.r[rn]);
                self.finish_write(bus, rd, sum);
            }
            1 => {
                let product = ((i64::from(rm as i32) * i64::from(rs)) >> 16) as i32;
                let result = if rm_top {
                    // Bit 5 set here means SMULWy, not a top half.
                    product as u32
                } else {
                    self.add_setting_q(product, self.r[rn])
                };
                self.finish_write(bus, rd, result);
            }
            2 => {
                // SMLALxy: RdHi (bits 16–19) : RdLo (bits 12–15) += product.
                let product = i64::from(half(rm, rm_top) * rs);
                let accumulator = (u64::from(self.r[rd]) << 32 | u64::from(self.r[rn])) as i64;
                let result = accumulator.wrapping_add(product) as u64;
                bus.idle();
                self.r[rn] = result as u32;
                self.r[rd] = (result >> 32) as u32;
                if rd == 15 || rn == 15 {
                    self.reload_pipeline(bus);
                } else {
                    self.advance_after_data();
                }
            }
            _ => {
                let product = half(rm, rm_top) * rs;
                self.finish_write(bus, rd, product as u32);
            }
        }
    }

    /// `product + accumulator`, wrapping, with Q set on signed overflow.
    fn add_setting_q(&mut self, product: i32, accumulator: u32) -> u32 {
        let (sum, overflow) = product.overflowing_add(accumulator as i32);
        if overflow {
            self.cpsr |= psr::Q;
        }
        sum as u32
    }

    /// LDRD and STRD: Rd and Rd+1 from or to two words, addressed like
    /// LDRH. An odd Rd (unpredictable) is treated as undefined. The bus
    /// sees a two-register LDM or STM. Writeback to Rd or Rd+1
    /// (unpredictable) loses to the loaded value, as for LDR.
    fn doubleword_transfer<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let pre = instruction & (1 << 24) != 0;
        let add = instruction & (1 << 23) != 0;
        let immediate = instruction & (1 << 22) != 0;
        let writeback = instruction & (1 << 21) != 0;
        let store = instruction & (1 << 5) != 0;
        let rn = reg(instruction, 16);
        let rd = reg(instruction, 12);
        if rd & 1 != 0 {
            self.undefined(bus);
            return;
        }

        let offset = if immediate {
            (instruction & 0xF) | ((instruction >> 4) & 0xF0)
        } else {
            self.r[reg(instruction, 0)]
        };
        let base = self.r[rn];
        let offset_address = if add {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if pre { offset_address } else { base };
        let second = address.wrapping_add(4);

        self.advance_after_data();
        if store {
            self.write_32(bus, address, self.r[rd], Access::NONSEQUENTIAL);
            self.write_32(bus, second, self.r[rd + 1], Access::SEQUENTIAL);
            if writeback || !pre {
                self.write_back(bus, rn, offset_address);
            }
        } else {
            let low = self.read_32(bus, address, Access::NONSEQUENTIAL);
            let high = self.read_32(bus, second, Access::SEQUENTIAL);
            bus.idle();
            if (writeback || !pre) && rn != rd && rn != rd + 1 {
                self.write_back(bus, rn, offset_address);
            }
            self.r[rd] = low;
            self.r[rd + 1] = high;
            if rd + 1 == 15 {
                if self.loads_interwork() {
                    self.set_state_from_pc();
                }
                self.reload_pipeline(bus);
            }
        }
    }

    /// MRC and MCR. CP15, the only coprocessor, answers privileged code
    /// only; anything else is an undefined instruction. MRC to R15 sets N,
    /// Z, C and V from the value's top bits.
    fn coprocessor_register_transfer<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        if self.mode() == Mode::User {
            self.undefined(bus);
            return;
        }
        let rd = reg(instruction, 12);
        if instruction & (1 << 20) != 0 {
            let Some(value) = self.model.coprocessor_read(instruction) else {
                self.undefined(bus);
                return;
            };
            bus.idle();
            if rd == 15 {
                self.cpsr = (self.cpsr & 0x0FFF_FFFF) | (value & 0xF000_0000);
            } else {
                self.r[rd] = value;
            }
        } else if self.model.coprocessor_write(instruction, self.r[rd]) {
            bus.idle();
        } else {
            self.undefined(bus);
            return;
        }
        self.advance();
    }
}
