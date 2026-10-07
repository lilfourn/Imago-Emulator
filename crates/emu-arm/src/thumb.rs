//! The 16-bit Thumb instruction set (GBATEK, "THUMB Opcodes"), formats 1
//! to 19, plus ARMv5's BLX and BKPT. R15 reads as the instruction's
//! address plus 4.

use crate::alu::{self, Shift};
use crate::{Access, Bus, Model, Processor, psr};

impl<M: Model> Processor<M> {
    pub(crate) fn execute_thumb<B: Bus>(&mut self, bus: &mut B, instruction: u16) {
        let op = u32::from(instruction);
        match op >> 8 {
            0x00..=0x17 => self.thumb_shift_immediate(op),
            0x18..=0x1F => self.thumb_add_subtract(op),
            0x20..=0x3F => self.thumb_immediate(op),
            0x40..=0x43 => self.thumb_alu(bus, op),
            0x44..=0x47 => self.thumb_high_register(bus, op),
            0x48..=0x4F => self.thumb_load_pc_relative(bus, op),
            0x50..=0x5F => self.thumb_load_store_register(bus, op),
            0x60..=0x7F => self.thumb_load_store_immediate(bus, op),
            0x80..=0x8F => self.thumb_load_store_half(bus, op),
            0x90..=0x9F => self.thumb_load_store_sp(bus, op),
            0xA0..=0xAF => self.thumb_load_address(op),
            0xB0 => self.thumb_adjust_sp(op),
            0xB4 | 0xB5 | 0xBC | 0xBD => self.thumb_push_pop(bus, op),
            0xC0..=0xCF => self.thumb_load_store_multiple(bus, op),
            // 0xDE (condition "always") is reserved, but the ARM7TDMI branches.
            0xD0..=0xDE => self.thumb_conditional_branch(bus, op),
            0xDF => self.software_interrupt(bus),
            0xE0..=0xE7 => self.thumb_branch(bus, op),
            0xE8..=0xEF if M::V5TE => self.thumb_branch_link_exchange(bus, op),
            0xBE if M::V5TE => self.breakpoint(bus),
            0xF0..=0xF7 => self.thumb_branch_link_high(op),
            0xF8..=0xFF => self.thumb_branch_link_low(bus, op),
            // 0xB1–0xB3, 0xB6–0xBB and 0xBF are undefined; so are 0xBE
            // (BKPT) and 0xE8–0xEF (the BLX suffix) on ARMv4T.
            _ => self.undefined(bus),
        }
    }

    fn low(op: u32, shift: u32) -> usize {
        ((op >> shift) & 7) as usize
    }

    /// Format 1: LSL, LSR, ASR by an immediate.
    fn thumb_shift_immediate(&mut self, op: u32) {
        let shift = Shift::from_bits(op >> 11);
        let amount = (op >> 6) & 0x1F;
        let (result, carry) =
            alu::shift_immediate(shift, self.r[Self::low(op, 3)], amount, self.carry());
        self.r[Self::low(op, 0)] = result;
        self.set_nz(result);
        self.set_flag(psr::C, carry);
        self.advance();
    }

    /// Format 2: ADD and SUB with a register or 3-bit immediate.
    fn thumb_add_subtract(&mut self, op: u32) {
        let lhs = self.r[Self::low(op, 3)];
        let rhs = if op & (1 << 10) != 0 {
            (op >> 6) & 7
        } else {
            self.r[Self::low(op, 6)]
        };
        let outcome = if op & (1 << 9) != 0 {
            alu::subtract(lhs, rhs)
        } else {
            alu::add_with_carry(lhs, rhs, false)
        };
        self.r[Self::low(op, 0)] = self.set_arithmetic_flags(outcome);
        self.advance();
    }

    /// Format 3: MOV, CMP, ADD, SUB with an 8-bit immediate.
    fn thumb_immediate(&mut self, op: u32) {
        let rd = Self::low(op, 8);
        let value = op & 0xFF;
        match (op >> 11) & 3 {
            0 => {
                self.r[rd] = value;
                self.set_nz(value);
            }
            1 => {
                self.set_arithmetic_flags(alu::subtract(self.r[rd], value));
            }
            2 => {
                self.r[rd] =
                    self.set_arithmetic_flags(alu::add_with_carry(self.r[rd], value, false))
            }
            _ => self.r[rd] = self.set_arithmetic_flags(alu::subtract(self.r[rd], value)),
        }
        self.advance();
    }

    /// Format 4: register-to-register ALU operations.
    fn thumb_alu<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let rd = Self::low(op, 0);
        let lhs = self.r[rd];
        let rhs = self.r[Self::low(op, 3)];
        let carry = self.carry();
        let mut nonsequential = false;
        let mut shift = |cpu: &mut Self, kind: Shift| {
            bus.idle();
            nonsequential = true;
            let (result, carry_out) = alu::shift_register(kind, lhs, rhs & 0xFF, carry);
            cpu.set_flag(psr::C, carry_out);
            cpu.set_nz(result);
            Some(result)
        };
        let result = match (op >> 6) & 0xF {
            0x0 => Some(self.set_logical(lhs & rhs)),
            0x1 => Some(self.set_logical(lhs ^ rhs)),
            0x2 => shift(self, Shift::Lsl),
            0x3 => shift(self, Shift::Lsr),
            0x4 => shift(self, Shift::Asr),
            0x5 => Some(self.set_arithmetic_flags(alu::add_with_carry(lhs, rhs, carry))),
            0x6 => Some(self.set_arithmetic_flags(alu::subtract_with_carry(lhs, rhs, carry))),
            0x7 => shift(self, Shift::Ror),
            0x8 => {
                self.set_logical(lhs & rhs);
                None
            }
            0x9 => Some(self.set_arithmetic_flags(alu::subtract(0, rhs))),
            0xA => {
                self.set_arithmetic_flags(alu::subtract(lhs, rhs));
                None
            }
            0xB => {
                self.set_arithmetic_flags(alu::add_with_carry(lhs, rhs, false));
                None
            }
            0xC => Some(self.set_logical(lhs | rhs)),
            0xD => {
                for _ in 0..alu::multiply_cycles(lhs, true) {
                    bus.idle();
                }
                nonsequential = true;
                let result = lhs.wrapping_mul(rhs);
                self.set_nz(result);
                Some(result)
            }
            0xE => Some(self.set_logical(lhs & !rhs)),
            _ => Some(self.set_logical(!rhs)),
        };
        if let Some(result) = result {
            self.r[rd] = result;
        }
        if nonsequential {
            self.advance_after_data();
        } else {
            self.advance();
        }
    }

    /// Format 5: ADD, CMP, MOV with high registers, BX, and (ARMv5) BLX.
    fn thumb_high_register<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let rd = Self::low(op, 0) | if op & 0x80 != 0 { 8 } else { 0 };
        let rs = Self::low(op, 3) | if op & 0x40 != 0 { 8 } else { 0 };
        let value = self.r[rs];
        match (op >> 8) & 3 {
            0 => {
                self.r[rd] = self.r[rd].wrapping_add(value);
                if rd == 15 {
                    self.r[15] &= !1;
                    self.reload_pipeline(bus);
                    return;
                }
            }
            1 => {
                self.set_arithmetic_flags(alu::subtract(self.r[rd], value));
            }
            2 => {
                self.r[rd] = value;
                if rd == 15 {
                    self.r[15] &= !1;
                    self.reload_pipeline(bus);
                    return;
                }
            }
            _ => {
                // ARMv4T ignores bit 7 (H1); on ARMv5 it makes BX a BLX.
                if M::V5TE && op & 0x80 != 0 {
                    self.r[14] = self.r[15].wrapping_sub(2) | 1;
                }
                self.set_flag(psr::T, value & 1 != 0);
                self.r[15] = value & !1;
                self.reload_pipeline(bus);
                return;
            }
        }
        self.advance();
    }

    /// Format 6: LDR Rd, [PC, #imm], with PC word-aligned.
    fn thumb_load_pc_relative<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let address = (self.r[15] & !2).wrapping_add((op & 0xFF) * 4);
        self.advance_after_data();
        let value = self.read_32(bus, address, Access::NONSEQUENTIAL);
        self.finish_load(bus, Self::low(op, 8), value);
    }

    /// Formats 7 and 8: loads and stores with a register offset.
    fn thumb_load_store_register<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let address = self.r[Self::low(op, 3)].wrapping_add(self.r[Self::low(op, 6)]);
        let rd = Self::low(op, 0);
        self.advance_after_data();
        let kind = (op >> 9) & 7;
        let value = match kind {
            0 => {
                self.write_32(bus, address, self.r[rd], Access::NONSEQUENTIAL);
                self.abort_if_refused(bus);
                return;
            }
            1 => {
                self.write_16(bus, address, self.r[rd] as u16, Access::NONSEQUENTIAL);
                self.abort_if_refused(bus);
                return;
            }
            2 => {
                self.write_8(bus, address, self.r[rd] as u8, Access::NONSEQUENTIAL);
                self.abort_if_refused(bus);
                return;
            }
            3 => self.read_byte_signed(bus, address),
            4 => self.read_word_rotated(bus, address, Access::NONSEQUENTIAL),
            5 => self.read_half(bus, address, Access::NONSEQUENTIAL),
            6 => u32::from(self.read_8(bus, address, Access::NONSEQUENTIAL)),
            _ => self.read_half_signed(bus, address),
        };
        self.finish_load(bus, rd, value);
    }

    /// Ends a single load: the internal cycle, then Rd, unless the load
    /// aborted.
    fn finish_load<B: Bus>(&mut self, bus: &mut B, rd: usize, value: u32) {
        if self.abort_if_refused(bus) {
            return;
        }
        bus.idle();
        self.r[rd] = value;
    }

    /// Format 9: word and byte loads and stores with a 5-bit offset.
    fn thumb_load_store_immediate<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let byte = op & (1 << 12) != 0;
        let load = op & (1 << 11) != 0;
        let offset = (op >> 6) & 0x1F;
        let base = self.r[Self::low(op, 3)];
        let address = base.wrapping_add(if byte { offset } else { offset * 4 });
        let rd = Self::low(op, 0);
        self.advance_after_data();
        match (load, byte) {
            (false, false) => {
                self.write_32(bus, address, self.r[rd], Access::NONSEQUENTIAL);
                self.abort_if_refused(bus);
            }
            (false, true) => {
                self.write_8(bus, address, self.r[rd] as u8, Access::NONSEQUENTIAL);
                self.abort_if_refused(bus);
            }
            (true, byte) => {
                let value = if byte {
                    u32::from(self.read_8(bus, address, Access::NONSEQUENTIAL))
                } else {
                    self.read_word_rotated(bus, address, Access::NONSEQUENTIAL)
                };
                self.finish_load(bus, rd, value);
            }
        }
    }

    /// Format 10: halfword loads and stores with a 5-bit offset.
    fn thumb_load_store_half<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let address = self.r[Self::low(op, 3)].wrapping_add(((op >> 6) & 0x1F) * 2);
        let rd = Self::low(op, 0);
        self.advance_after_data();
        if op & (1 << 11) != 0 {
            let value = self.read_half(bus, address, Access::NONSEQUENTIAL);
            self.finish_load(bus, rd, value);
        } else {
            self.write_16(bus, address, self.r[rd] as u16, Access::NONSEQUENTIAL);
            self.abort_if_refused(bus);
        }
    }

    /// Format 11: SP-relative loads and stores.
    fn thumb_load_store_sp<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let address = self.r[13].wrapping_add((op & 0xFF) * 4);
        let rd = Self::low(op, 8);
        self.advance_after_data();
        if op & (1 << 11) != 0 {
            let value = self.read_word_rotated(bus, address, Access::NONSEQUENTIAL);
            self.finish_load(bus, rd, value);
        } else {
            self.write_32(bus, address, self.r[rd], Access::NONSEQUENTIAL);
            self.abort_if_refused(bus);
        }
    }

    /// Format 12: ADD Rd, PC or SP, #imm.
    fn thumb_load_address(&mut self, op: u32) {
        let base = if op & (1 << 11) != 0 {
            self.r[13]
        } else {
            self.r[15] & !2
        };
        self.r[Self::low(op, 8)] = base.wrapping_add((op & 0xFF) * 4);
        self.advance();
    }

    /// Format 13: ADD SP, #±imm.
    fn thumb_adjust_sp(&mut self, op: u32) {
        let offset = (op & 0x7F) * 4;
        self.r[13] = if op & 0x80 != 0 {
            self.r[13].wrapping_sub(offset)
        } else {
            self.r[13].wrapping_add(offset)
        };
        self.advance();
    }

    /// Format 14: PUSH {list, LR} and POP {list, PC}.
    fn thumb_push_pop<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let load = op & (1 << 11) != 0;
        let extra = op & (1 << 8) != 0;
        let mut list = op & 0xFF;
        if extra {
            list |= if load { 1 << 15 } else { 1 << 14 };
        }
        if load {
            self.thumb_transfer(bus, 13, list, true, true);
        } else {
            self.thumb_store_descending(bus, list);
        }
    }

    /// Format 15: LDMIA and STMIA.
    fn thumb_load_store_multiple<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let rb = Self::low(op, 8);
        let load = op & (1 << 11) != 0;
        self.thumb_transfer(bus, rb, op & 0xFF, load, true);
    }

    /// An ascending block transfer from `rb` with writeback, like
    /// LDMIA/STMIA. An empty list transfers R15 and moves the base by 0x40.
    fn thumb_transfer<B: Bus>(
        &mut self,
        bus: &mut B,
        rb: usize,
        list: u32,
        load: bool,
        writeback: bool,
    ) {
        // POP {PC} stays in Thumb on ARMv4T; on ARMv5 bit 0 of the loaded
        // value picks the state, as for LDM.
        self.transfer_multiple(bus, rb, list, false, true, false, writeback, load);
    }

    /// PUSH: a descending store, lowest register at the lowest address.
    fn thumb_store_descending<B: Bus>(&mut self, bus: &mut B, list: u32) {
        self.transfer_multiple(bus, 13, list, true, false, false, true, false);
    }

    /// Format 16: conditional branch.
    fn thumb_conditional_branch<B: Bus>(&mut self, bus: &mut B, op: u32) {
        if !self.condition_passed((op >> 8) & 0xF) {
            self.advance();
            return;
        }
        let offset = (((op & 0xFF) as u8 as i8) as i32 * 2) as u32;
        self.r[15] = self.r[15].wrapping_add(offset);
        self.reload_pipeline(bus);
    }

    /// Format 18: unconditional branch.
    fn thumb_branch<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let offset = (((op << 21) as i32) >> 20) as u32;
        self.r[15] = self.r[15].wrapping_add(offset);
        self.reload_pipeline(bus);
    }

    /// Format 19, first half: LR = PC + (offset << 12).
    fn thumb_branch_link_high(&mut self, op: u32) {
        let offset = (((op << 21) as i32) >> 9) as u32;
        self.r[14] = self.r[15].wrapping_add(offset);
        self.advance();
    }

    /// Format 19, second half: jump to LR + (offset << 1), LR = return
    /// address with bit 0 set.
    fn thumb_branch_link_low<B: Bus>(&mut self, bus: &mut B, op: u32) {
        let target = self.r[14].wrapping_add((op & 0x7FF) << 1);
        self.r[14] = self.r[15].wrapping_sub(2) | 1;
        self.r[15] = target & !1;
        self.reload_pipeline(bus);
    }

    /// ARMv5 BLX suffix: like format 19's second half, but to ARM state at
    /// a word-aligned target. An odd offset is undefined.
    fn thumb_branch_link_exchange<B: Bus>(&mut self, bus: &mut B, op: u32) {
        if op & 1 != 0 {
            self.undefined(bus);
            return;
        }
        let target = self.r[14].wrapping_add((op & 0x7FF) << 1);
        self.r[14] = self.r[15].wrapping_sub(2) | 1;
        self.set_flag(psr::T, false);
        self.r[15] = target & !3;
        self.reload_pipeline(bus);
    }

    fn set_logical(&mut self, result: u32) -> u32 {
        self.set_nz(result);
        result
    }

    fn set_arithmetic_flags(&mut self, (result, carry, overflow): (u32, bool, bool)) -> u32 {
        self.set_nz(result);
        self.set_flag(psr::C, carry);
        self.set_flag(psr::V, overflow);
        result
    }
}
