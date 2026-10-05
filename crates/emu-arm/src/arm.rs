//! The 32-bit ARM instruction set (GBATEK, "ARM Opcodes").
//!
//! Instructions are decoded from bits 27–20 and 7–4, as the hardware's
//! decoder and NanoBoyAdvance do, so odd encodings behave as the tests
//! recorded them. R15 reads as the instruction's address plus 8 while an
//! instruction runs; handlers advance it (or reload the pipeline) at the
//! cycle the hardware does, which is visible to instructions that store
//! R15 or read it after a register shift.

use crate::alu::{self, Shift};
use crate::{Access, Bus, Cpu, Mode, psr};

impl Cpu {
    pub(crate) fn execute_arm<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let key = instruction & 0x0FF0_00F0;
        if key & 0x0FF0_00F0 == 0x0120_0010 {
            self.branch_exchange(bus, instruction);
        } else if key & 0x0FB0_00F0 == 0x0100_0090 {
            self.swap(bus, instruction);
        } else if key & 0x0FC0_00F0 == 0x0000_0090 {
            self.multiply(bus, instruction);
        } else if key & 0x0F80_00F0 == 0x0080_0090 {
            self.multiply_long(bus, instruction);
        } else if key & 0x0E00_0090 == 0x0000_0090 {
            self.halfword_transfer(bus, instruction);
        } else if key & 0x0FB0_00F0 == 0x0100_0000 {
            self.move_from_psr(instruction);
        } else if key & 0x0FB0_00F0 == 0x0120_0000 || key & 0x0FB0_0000 == 0x0320_0000 {
            self.move_to_psr(instruction);
        } else if key & 0x0C00_0000 == 0 {
            self.data_processing(bus, instruction);
        } else if key & 0x0E00_0010 == 0x0600_0010 {
            self.undefined(bus);
        } else if key & 0x0C00_0000 == 0x0400_0000 {
            self.single_transfer(bus, instruction);
        } else if key & 0x0E00_0000 == 0x0800_0000 {
            self.block_transfer(bus, instruction);
        } else if key & 0x0E00_0000 == 0x0A00_0000 {
            self.branch(bus, instruction);
        } else if key & 0x0F00_0000 == 0x0F00_0000 {
            self.software_interrupt(bus);
        } else {
            // Coprocessor instructions: the GBA has no coprocessor.
            self.undefined(bus);
        }
    }

    fn branch<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let offset = ((instruction << 8) as i32 >> 6) as u32;
        if instruction & (1 << 24) != 0 {
            self.r[14] = self.r[15].wrapping_sub(4);
        }
        self.r[15] = self.r[15].wrapping_add(offset);
        self.reload_pipeline(bus);
    }

    fn branch_exchange<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let target = self.r[(instruction & 0xF) as usize];
        self.set_flag(psr::T, target & 1 != 0);
        self.r[15] = target & !1;
        self.reload_pipeline(bus);
    }

    fn data_processing<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let opcode = (instruction >> 21) & 0xF;
        let set_flags = instruction & (1 << 20) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let rd = ((instruction >> 12) & 0xF) as usize;
        let mut op1 = self.r[rn];
        let carry = self.carry();
        let mut register_shift = false;

        let (op2, shifter_carry) = if instruction & (1 << 25) != 0 {
            let rotate = ((instruction >> 8) & 0xF) * 2;
            let value = (instruction & 0xFF).rotate_right(rotate);
            let carry = if rotate == 0 { carry } else { value >> 31 != 0 };
            (value, carry)
        } else {
            let rm = (instruction & 0xF) as usize;
            let shift = Shift::from_bits(instruction >> 5);
            let mut value = self.r[rm];
            if instruction & 0x10 != 0 {
                // Shift by register: an extra internal cycle, during which
                // the PC has moved on, so R15 operands read 12 ahead.
                let amount = self.r[((instruction >> 8) & 0xF) as usize] & 0xFF;
                if rn == 15 {
                    op1 = op1.wrapping_add(4);
                }
                if rm == 15 {
                    value = value.wrapping_add(4);
                }
                bus.idle();
                register_shift = true;
                alu::shift_register(shift, value, amount, carry)
            } else {
                alu::shift_immediate(shift, value, (instruction >> 7) & 0x1F, carry)
            }
        };

        let logical = |cpu: &mut Self, result: u32| {
            if set_flags {
                cpu.set_nz(result);
                cpu.set_flag(psr::C, shifter_carry);
            }
            result
        };
        let arithmetic = |cpu: &mut Self, (result, carry, overflow): (u32, bool, bool)| {
            if set_flags {
                cpu.set_nz(result);
                cpu.set_flag(psr::C, carry);
                cpu.set_flag(psr::V, overflow);
            }
            result
        };
        let c = self.carry();
        let result = match opcode {
            0x0 => Some(logical(self, op1 & op2)),
            0x1 => Some(logical(self, op1 ^ op2)),
            0x2 => Some(arithmetic(self, alu::subtract(op1, op2))),
            0x3 => Some(arithmetic(self, alu::subtract(op2, op1))),
            0x4 => Some(arithmetic(self, alu::add_with_carry(op1, op2, false))),
            0x5 => Some(arithmetic(self, alu::add_with_carry(op1, op2, c))),
            0x6 => Some(arithmetic(self, alu::subtract_with_carry(op1, op2, c))),
            0x7 => Some(arithmetic(self, alu::subtract_with_carry(op2, op1, c))),
            0x8 => {
                logical(self, op1 & op2);
                None
            }
            0x9 => {
                logical(self, op1 ^ op2);
                None
            }
            0xA => {
                arithmetic(self, alu::subtract(op1, op2));
                None
            }
            0xB => {
                arithmetic(self, alu::add_with_carry(op1, op2, false));
                None
            }
            0xC => Some(logical(self, op1 | op2)),
            0xD => Some(logical(self, op2)),
            0xE => Some(logical(self, op1 & !op2)),
            _ => Some(logical(self, !op2)),
        };

        if rd == 15 && (result.is_some() || set_flags) {
            // Writing the PC with S set returns from an exception.
            if set_flags {
                self.restore_cpsr();
            }
            if let Some(result) = result {
                self.r[15] = result;
                self.reload_pipeline(bus);
                return;
            }
        }
        if let Some(result) = result {
            self.r[rd] = result;
        }
        if register_shift {
            // The internal cycle broke the sequence of fetches.
            self.advance_after_data();
        } else {
            self.advance();
        }
    }

    fn move_from_psr(&mut self, instruction: u32) {
        let rd = ((instruction >> 12) & 0xF) as usize;
        let value = if instruction & (1 << 22) != 0 {
            self.spsr().unwrap_or(self.cpsr)
        } else {
            self.cpsr
        };
        self.r[rd] = value;
        self.advance();
    }

    fn move_to_psr(&mut self, instruction: u32) {
        let operand = if instruction & (1 << 25) != 0 {
            let rotate = ((instruction >> 8) & 0xF) * 2;
            (instruction & 0xFF).rotate_right(rotate)
        } else {
            self.r[(instruction & 0xF) as usize]
        };
        let mut mask = 0;
        for (bit, field) in [
            (16, 0x0000_00FF),
            (17, 0x0000_FF00),
            (18, 0x00FF_0000),
            (19, 0xFF00_0000),
        ] {
            if instruction & (1 << bit) != 0 {
                mask |= field;
            }
        }
        if instruction & (1 << 22) != 0 {
            if let Some(spsr) = self.spsr() {
                self.set_spsr((spsr & !mask) | (operand & mask));
            }
        } else {
            if self.mode() == Mode::User {
                mask &= 0xFF00_0000;
            }
            // Mode bit 4 always reads as 1 on ARMv4.
            let control = if mask & 0xFF != 0 { 0x10 } else { 0 };
            // Advance as an ARM instruction even if this sets the T bit.
            self.advance();
            self.set_cpsr((self.cpsr & !mask) | (operand & mask) | control);
            return;
        }
        self.advance();
    }

    fn multiply<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let rd = ((instruction >> 16) & 0xF) as usize;
        let rn = ((instruction >> 12) & 0xF) as usize;
        let accumulate = instruction & (1 << 21) != 0;
        // Operands are read after the PC has moved on: R15 reads 12 ahead.
        self.advance_after_data();
        let rs = self.r[((instruction >> 8) & 0xF) as usize];
        let rm = self.r[(instruction & 0xF) as usize];
        let accumulator = self.r[rn];
        for _ in 0..alu::multiply_cycles(rs, true) {
            bus.idle();
        }
        let mut result = rm.wrapping_mul(rs);
        if accumulate {
            bus.idle();
            result = result.wrapping_add(accumulator);
        }
        if instruction & (1 << 20) != 0 {
            self.set_nz(result);
        }
        self.r[rd] = result;
        if rd == 15 {
            self.reload_pipeline(bus);
        }
    }

    fn multiply_long<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let rd_high = ((instruction >> 16) & 0xF) as usize;
        let rd_low = ((instruction >> 12) & 0xF) as usize;
        let signed = instruction & (1 << 22) != 0;
        let accumulate = instruction & (1 << 21) != 0;
        self.advance_after_data();
        let rs = self.r[((instruction >> 8) & 0xF) as usize];
        let rm = self.r[(instruction & 0xF) as usize];
        let accumulator = u64::from(self.r[rd_high]) << 32 | u64::from(self.r[rd_low]);
        for _ in 0..=alu::multiply_cycles(rs, signed) {
            bus.idle();
        }
        let mut result = if signed {
            (i64::from(rm as i32) * i64::from(rs as i32)) as u64
        } else {
            u64::from(rm) * u64::from(rs)
        };
        if accumulate {
            bus.idle();
            result = result.wrapping_add(accumulator);
        }
        if instruction & (1 << 20) != 0 {
            self.set_flag(psr::N, result >> 63 != 0);
            self.set_flag(psr::Z, result == 0);
        }
        self.r[rd_low] = result as u32;
        self.r[rd_high] = (result >> 32) as u32;
        if rd_low == 15 || rd_high == 15 {
            self.reload_pipeline(bus);
        }
    }

    fn swap<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let rn = ((instruction >> 16) & 0xF) as usize;
        let rd = ((instruction >> 12) & 0xF) as usize;
        let rm = (instruction & 0xF) as usize;
        self.advance_after_data();
        let address = self.r[rn];
        let source = self.r[rm];
        let loaded = if instruction & (1 << 22) != 0 {
            let value = bus.read_8(address, Access::NONSEQUENTIAL);
            bus.write_8(address, source as u8, Access::LOCK);
            u32::from(value)
        } else {
            let value = self.read_word_rotated(bus, address, Access::NONSEQUENTIAL);
            bus.write_32(address, source, Access::LOCK);
            value
        };
        bus.idle();
        self.r[rd] = loaded;
        if rd == 15 {
            self.reload_pipeline(bus);
        }
    }

    fn single_transfer<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let pre = instruction & (1 << 24) != 0;
        let add = instruction & (1 << 23) != 0;
        let byte = instruction & (1 << 22) != 0;
        let writeback = instruction & (1 << 21) != 0;
        let load = instruction & (1 << 20) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let rd = ((instruction >> 12) & 0xF) as usize;

        let offset = if instruction & (1 << 25) == 0 {
            instruction & 0xFFF
        } else {
            let shift = Shift::from_bits(instruction >> 5);
            let amount = (instruction >> 7) & 0x1F;
            alu::shift_immediate(
                shift,
                self.r[(instruction & 0xF) as usize],
                amount,
                self.carry(),
            )
            .0
        };
        let base = self.r[rn];
        let offset_address = if add {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if pre { offset_address } else { base };

        // The PC moves on before the data access: STR R15 stores +12.
        self.advance_after_data();
        if load {
            let value = if byte {
                u32::from(bus.read_8(address, Access::NONSEQUENTIAL))
            } else {
                self.read_word_rotated(bus, address, Access::NONSEQUENTIAL)
            };
            bus.idle();
            if (writeback || !pre) && rn != rd {
                self.write_back(bus, rn, offset_address);
            }
            self.r[rd] = value;
            if rd == 15 {
                self.reload_pipeline(bus);
            }
        } else {
            let value = self.r[rd];
            if byte {
                bus.write_8(address, value as u8, Access::NONSEQUENTIAL);
            } else {
                bus.write_32(address, value, Access::NONSEQUENTIAL);
            }
            if writeback || !pre {
                self.write_back(bus, rn, offset_address);
            }
        }
    }

    /// Writes a transfer's updated base address. Writing back to R15 (an
    /// unpredictable encoding) jumps, as the ARM7TDMI does, to the address
    /// plus 4.
    fn write_back<B: Bus>(&mut self, bus: &mut B, rn: usize, address: u32) {
        if rn == 15 {
            self.r[15] = address.wrapping_add(4);
            self.reload_pipeline(bus);
        } else {
            self.r[rn] = address;
        }
    }

    fn halfword_transfer<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let pre = instruction & (1 << 24) != 0;
        let add = instruction & (1 << 23) != 0;
        let immediate = instruction & (1 << 22) != 0;
        let writeback = instruction & (1 << 21) != 0;
        let load = instruction & (1 << 20) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let rd = ((instruction >> 12) & 0xF) as usize;
        let kind = (instruction >> 5) & 3;

        let offset = if immediate {
            (instruction & 0xF) | ((instruction >> 4) & 0xF0)
        } else {
            self.r[(instruction & 0xF) as usize]
        };
        let base = self.r[rn];
        let offset_address = if add {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if pre { offset_address } else { base };

        self.advance_after_data();
        if load {
            let value = match kind {
                2 => self.read_byte_signed(bus, address),
                3 => self.read_half_signed(bus, address),
                _ => self.read_half_rotated(bus, address, Access::NONSEQUENTIAL),
            };
            bus.idle();
            if (writeback || !pre) && rn != rd {
                self.write_back(bus, rn, offset_address);
            }
            self.r[rd] = value;
            if rd == 15 {
                self.reload_pipeline(bus);
            }
        } else {
            // ARMv4 has only STRH; the signed forms store a halfword too.
            bus.write_16(address, self.r[rd] as u16, Access::NONSEQUENTIAL);
            if writeback || !pre {
                self.write_back(bus, rn, offset_address);
            }
        }
    }

    fn block_transfer<B: Bus>(&mut self, bus: &mut B, instruction: u32) {
        let pre = instruction & (1 << 24) != 0;
        let add = instruction & (1 << 23) != 0;
        let user_bank = instruction & (1 << 22) != 0;
        let writeback = instruction & (1 << 21) != 0;
        let load = instruction & (1 << 20) != 0;
        let rn = ((instruction >> 16) & 0xF) as usize;
        let list = instruction & 0xFFFF;
        self.transfer_multiple(bus, rn, list, pre, add, user_bank, writeback, load);
    }

    /// LDM/STM, shared with Thumb's PUSH, POP, LDMIA and STMIA.
    #[allow(clippy::too_many_arguments)] // Mirrors the instruction's fields.
    pub(crate) fn transfer_multiple<B: Bus>(
        &mut self,
        bus: &mut B,
        rn: usize,
        list: u32,
        pre: bool,
        add: bool,
        user_bank: bool,
        writeback: bool,
        load: bool,
    ) {
        // An empty list transfers R15 and moves the base by 0x40 (ARMv4).
        let empty = list == 0;
        let (list, bytes) = if empty {
            (1 << 15, 0x40)
        } else {
            (list, list.count_ones() * 4)
        };
        let base = self.r[rn];
        let (start, final_base) = if add {
            (
                if pre { base.wrapping_add(4) } else { base },
                base.wrapping_add(bytes),
            )
        } else {
            let start = base.wrapping_sub(bytes);
            (if pre { start } else { start.wrapping_add(4) }, start)
        };

        // S without a PC load means "the User bank".
        let pc_load = load && list & (1 << 15) != 0;
        let mode = self.mode();
        let switch_bank = user_bank && !pc_load && mode != Mode::User && mode != Mode::System;
        if switch_bank {
            self.store_bank(mode);
            self.load_bank(Mode::User);
        }

        self.advance_after_data();
        let mut address = start;
        let mut first = true;
        for register in (0..16).filter(|r| list & (1 << r) != 0) {
            let access = if first {
                Access::NONSEQUENTIAL
            } else {
                Access::SEQUENTIAL
            };
            if load {
                let value = bus.read_32(address, access);
                if first && writeback {
                    self.r[rn] = final_base;
                }
                self.r[register] = value;
            } else {
                bus.write_32(address, self.r[register], access);
                if first && writeback {
                    self.r[rn] = final_base;
                }
            }
            address = address.wrapping_add(4);
            first = false;
        }

        if switch_bank {
            self.store_bank(Mode::User);
            self.load_bank(mode);
        }
        if writeback && rn == 15 && !pc_load {
            self.reload_pipeline(bus);
            return;
        }
        if load {
            bus.idle();
            if pc_load {
                if user_bank {
                    self.restore_cpsr();
                }
                // Thumb POP {PC} clears bit 0; an empty list loads it raw.
                if self.thumb() && !empty {
                    self.r[15] &= !1;
                }
                self.reload_pipeline(bus);
            }
        }
    }

    /// A word load from any address: misaligned loads rotate the word.
    pub(crate) fn read_word_rotated<B: Bus>(
        &mut self,
        bus: &mut B,
        address: u32,
        access: Access,
    ) -> u32 {
        bus.read_32(address, access).rotate_right((address & 3) * 8)
    }

    /// A halfword load: from an odd address the halfword is rotated.
    pub(crate) fn read_half_rotated<B: Bus>(
        &mut self,
        bus: &mut B,
        address: u32,
        access: Access,
    ) -> u32 {
        u32::from(bus.read_16(address, access)).rotate_right((address & 1) * 8)
    }

    /// LDRSB.
    pub(crate) fn read_byte_signed<B: Bus>(&mut self, bus: &mut B, address: u32) -> u32 {
        i32::from(bus.read_8(address, Access::NONSEQUENTIAL) as i8) as u32
    }

    /// LDRSH: from an odd address the ARM7TDMI sign-extends the high byte
    /// of the halfword instead.
    pub(crate) fn read_half_signed<B: Bus>(&mut self, bus: &mut B, address: u32) -> u32 {
        let half = bus.read_16(address, Access::NONSEQUENTIAL);
        if address & 1 != 0 {
            i32::from((half >> 8) as u8 as i8) as u32
        } else {
            i32::from(half as i16) as u32
        }
    }
}
