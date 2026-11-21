//! The Sharp SM83 CPU.
//!
//! References: Pan Docs, "CPU Instruction Set"; Gekkio, "Game Boy: Complete
//! Technical Reference" for per-cycle timing.
//!
//! Every memory access goes through [`Bus`] and costs one M-cycle (four
//! clock cycles); instructions that spend time without touching memory call
//! [`Bus::tick`]. The rest of the machine therefore advances in step with
//! the CPU, at M-cycle granularity.

use serde::{Deserialize, Serialize};

/// What the CPU needs from the rest of the machine.
pub trait Bus {
    /// Reads a byte. Takes one M-cycle.
    fn read(&mut self, address: u16) -> u8;
    /// Reads a byte of the instruction stream (an opcode or operand).
    /// Takes one M-cycle. Only debuggers tell it apart from a data read.
    fn fetch(&mut self, address: u16) -> u8 {
        self.read(address)
    }
    /// Writes a byte. Takes one M-cycle.
    fn write(&mut self, address: u16, value: u8);
    /// Spends one M-cycle without a memory access.
    fn tick(&mut self);
    /// Interrupts both requested (IF) and enabled (IE), in bits 0 to 4.
    fn pending_interrupts(&self) -> u8;
    /// Clears the request flag `mask` in IF.
    fn acknowledge_interrupt(&mut self, mask: u8);
    /// The STOP instruction ran: on CGB this switches CPU speed if KEY1
    /// armed it.
    fn stop(&mut self) {}
}

const FLAG_Z: u8 = 0x80;
const FLAG_N: u8 = 0x40;
const FLAG_H: u8 = 0x20;
const FLAG_C: u8 = 0x10;

/// The CPU's registers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registers {
    /// Accumulator.
    pub a: u8,
    /// Flags: Z (bit 7), N, H, C (bit 4). The low nibble is always zero.
    pub f: u8,
    /// B.
    pub b: u8,
    /// C.
    pub c: u8,
    /// D.
    pub d: u8,
    /// E.
    pub e: u8,
    /// H.
    pub h: u8,
    /// L.
    pub l: u8,
    /// Stack pointer.
    pub sp: u16,
    /// Program counter.
    pub pc: u16,
}

impl Registers {
    /// The registers as the CGB boot ROM leaves them for a CGB game.
    pub const CGB_AFTER_BOOT: Self = Self {
        a: 0x11,
        f: 0x80,
        b: 0x00,
        c: 0x00,
        d: 0xFF,
        e: 0x56,
        h: 0x00,
        l: 0x0D,
        sp: 0xFFFE,
        pc: 0x0100,
    };

    /// The registers as the CGB boot ROM leaves them for a game made for
    /// the original Game Boy, whose title bytes of a game Nintendo published
    /// sum to `title_checksum` (0 for other games). HL points into the logo's
    /// tile map for the two titles that use it (Pan Docs, "Power Up
    /// Sequence").
    #[must_use]
    pub const fn cgb_dmg_mode_after_boot(title_checksum: u8) -> Self {
        let [h, l] = match title_checksum {
            0x43 | 0x58 => [0x99, 0x1A],
            _ => [0x00, 0x7C],
        };
        Self {
            a: 0x11,
            f: 0x80,
            b: title_checksum,
            c: 0x00,
            d: 0x00,
            e: 0x08,
            h,
            l,
            sp: 0xFFFE,
            pc: 0x0100,
        }
    }

    /// The registers as the DMG boot ROM leaves them (Pan Docs, "Power Up
    /// Sequence").
    pub const DMG_AFTER_BOOT: Self = Self {
        a: 0x01,
        f: 0xB0,
        b: 0x00,
        c: 0x13,
        d: 0x00,
        e: 0xD8,
        h: 0x01,
        l: 0x4D,
        sp: 0xFFFE,
        pc: 0x0100,
    };

    /// BC as a 16-bit pair.
    #[must_use]
    pub const fn bc(&self) -> u16 {
        u16::from_be_bytes([self.b, self.c])
    }

    /// DE as a 16-bit pair.
    #[must_use]
    pub const fn de(&self) -> u16 {
        u16::from_be_bytes([self.d, self.e])
    }

    /// HL as a 16-bit pair.
    #[must_use]
    pub const fn hl(&self) -> u16 {
        u16::from_be_bytes([self.h, self.l])
    }

    /// AF as a 16-bit pair.
    #[must_use]
    pub const fn af(&self) -> u16 {
        u16::from_be_bytes([self.a, self.f])
    }

    fn set_bc(&mut self, value: u16) {
        [self.b, self.c] = value.to_be_bytes();
    }

    fn set_de(&mut self, value: u16) {
        [self.d, self.e] = value.to_be_bytes();
    }

    fn set_hl(&mut self, value: u16) {
        [self.h, self.l] = value.to_be_bytes();
    }

    fn set_af(&mut self, value: u16) {
        let [a, f] = value.to_be_bytes();
        self.a = a;
        self.f = f & 0xF0;
    }

    fn flag(&self, flag: u8) -> bool {
        self.f & flag != 0
    }

    fn set_flags(&mut self, z: bool, n: bool, h: bool, c: bool) {
        self.f = u8::from(z) << 7 | u8::from(n) << 6 | u8::from(h) << 5 | u8::from(c) << 4;
    }
}

/// The SM83 core: registers plus interrupt and low-power state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cpu {
    pub(crate) regs: Registers,
    /// Interrupt master enable.
    ime: bool,
    /// Counts down to enabling IME; EI takes effect after the next
    /// instruction.
    ei_delay: u8,
    halted: bool,
    /// The HALT bug: the next opcode fetch does not advance PC.
    halt_bug: bool,
    /// An illegal opcode froze the CPU until reset.
    locked: bool,
}

impl Cpu {
    /// A CPU with `regs`, interrupts disabled.
    pub(crate) fn new(regs: Registers) -> Self {
        Self {
            regs,
            ..Self::default()
        }
    }

    /// Whether interrupts are enabled (IME).
    pub(crate) fn ime(&self) -> bool {
        self.ime
    }

    /// Whether HALT (or an illegal opcode, which freezes the CPU) is
    /// keeping it from running instructions.
    pub(crate) fn halted(&self) -> bool {
        self.halted || self.locked
    }

    /// Runs one instruction, one interrupt dispatch, or one M-cycle of
    /// HALT. Returns the opcode if an instruction ran (CB-prefixed
    /// instructions report `0xCB`).
    pub(crate) fn step<B: Bus>(&mut self, bus: &mut B) -> Option<u8> {
        if self.ei_delay > 0 {
            self.ei_delay -= 1;
            if self.ei_delay == 0 {
                self.ime = true;
            }
        }
        if self.locked {
            bus.tick();
            return None;
        }
        if self.halted {
            if bus.pending_interrupts() == 0 {
                bus.tick();
                return None;
            }
            self.halted = false;
        }
        if self.ime && bus.pending_interrupts() != 0 {
            self.dispatch_interrupt(bus);
            return None;
        }

        let opcode = bus.fetch(self.regs.pc);
        if self.halt_bug {
            self.halt_bug = false;
        } else {
            self.regs.pc = self.regs.pc.wrapping_add(1);
        }
        self.execute(opcode, bus);
        Some(opcode)
    }

    /// Pushes PC and jumps to the highest-priority pending interrupt's
    /// vector. Five M-cycles.
    fn dispatch_interrupt<B: Bus>(&mut self, bus: &mut B) {
        self.ime = false;
        bus.tick();
        bus.tick();
        // After EI; HALT with an interrupt pending, the HALT bug meets the
        // interrupt: the handler returns to the HALT, which runs again
        // (Pan Docs, "halt bug").
        let pc = if std::mem::take(&mut self.halt_bug) {
            self.regs.pc.wrapping_sub(1)
        } else {
            self.regs.pc
        };
        let [high, low] = pc.to_be_bytes();
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        bus.write(self.regs.sp, high);
        // Pushing the high byte can overwrite IE (SP = 0x0000), which
        // changes or cancels the interrupt being dispatched.
        let pending = bus.pending_interrupts();
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        bus.write(self.regs.sp, low);
        if pending == 0 {
            self.regs.pc = 0x0000;
        } else {
            let mask = pending & pending.wrapping_neg();
            bus.acknowledge_interrupt(mask);
            self.regs.pc = 0x0040 + 8 * mask.trailing_zeros() as u16;
        }
        bus.tick();
    }

    fn fetch<B: Bus>(&mut self, bus: &mut B) -> u8 {
        let value = bus.fetch(self.regs.pc);
        self.regs.pc = self.regs.pc.wrapping_add(1);
        value
    }

    fn fetch16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let low = self.fetch(bus);
        let high = self.fetch(bus);
        u16::from_le_bytes([low, high])
    }

    fn push<B: Bus>(&mut self, bus: &mut B, value: u16) {
        let [high, low] = value.to_be_bytes();
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        bus.write(self.regs.sp, high);
        self.regs.sp = self.regs.sp.wrapping_sub(1);
        bus.write(self.regs.sp, low);
    }

    fn pop<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let low = bus.read(self.regs.sp);
        self.regs.sp = self.regs.sp.wrapping_add(1);
        let high = bus.read(self.regs.sp);
        self.regs.sp = self.regs.sp.wrapping_add(1);
        u16::from_le_bytes([low, high])
    }

    /// Reads 8-bit operand `index`: B, C, D, E, H, L, (HL), A.
    fn read_r8<B: Bus>(&mut self, bus: &mut B, index: u8) -> u8 {
        match index & 7 {
            0 => self.regs.b,
            1 => self.regs.c,
            2 => self.regs.d,
            3 => self.regs.e,
            4 => self.regs.h,
            5 => self.regs.l,
            6 => bus.read(self.regs.hl()),
            _ => self.regs.a,
        }
    }

    /// Writes 8-bit operand `index`: B, C, D, E, H, L, (HL), A.
    fn write_r8<B: Bus>(&mut self, bus: &mut B, index: u8, value: u8) {
        match index & 7 {
            0 => self.regs.b = value,
            1 => self.regs.c = value,
            2 => self.regs.d = value,
            3 => self.regs.e = value,
            4 => self.regs.h = value,
            5 => self.regs.l = value,
            6 => bus.write(self.regs.hl(), value),
            _ => self.regs.a = value,
        }
    }

    /// 16-bit register `index` for loads and arithmetic: BC, DE, HL, SP.
    fn r16(&self, index: u8) -> u16 {
        match index & 3 {
            0 => self.regs.bc(),
            1 => self.regs.de(),
            2 => self.regs.hl(),
            _ => self.regs.sp,
        }
    }

    fn set_r16(&mut self, index: u8, value: u16) {
        match index & 3 {
            0 => self.regs.set_bc(value),
            1 => self.regs.set_de(value),
            2 => self.regs.set_hl(value),
            _ => self.regs.sp = value,
        }
    }

    /// Address for `LD (rr), A` and `LD A, (rr)`: BC, DE, HL+, HL-.
    fn indirect_address(&mut self, index: u8) -> u16 {
        match index & 3 {
            0 => self.regs.bc(),
            1 => self.regs.de(),
            2 => {
                let hl = self.regs.hl();
                self.regs.set_hl(hl.wrapping_add(1));
                hl
            }
            _ => {
                let hl = self.regs.hl();
                self.regs.set_hl(hl.wrapping_sub(1));
                hl
            }
        }
    }

    /// Condition `index`: NZ, Z, NC, C.
    fn condition(&self, index: u8) -> bool {
        match index & 3 {
            0 => !self.regs.flag(FLAG_Z),
            1 => self.regs.flag(FLAG_Z),
            2 => !self.regs.flag(FLAG_C),
            _ => self.regs.flag(FLAG_C),
        }
    }

    #[allow(clippy::too_many_lines)] // One arm per instruction group reads best as a table.
    fn execute<B: Bus>(&mut self, opcode: u8, bus: &mut B) {
        // Bit fields of the opcode, as in most SM83/Z80 decoding tables.
        let y = (opcode >> 3) & 7;
        let z = opcode & 7;
        let p = (opcode >> 4) & 3;

        match opcode {
            0x00 => {}
            0x10 => {
                // STOP is two bytes long. Low-power mode is not emulated;
                // the bus handles CGB speed switching.
                self.fetch(bus);
                bus.stop();
            }
            0x01 | 0x11 | 0x21 | 0x31 => {
                let value = self.fetch16(bus);
                self.set_r16(p, value);
            }
            0x02 | 0x12 | 0x22 | 0x32 => {
                let address = self.indirect_address(p);
                bus.write(address, self.regs.a);
            }
            0x0A | 0x1A | 0x2A | 0x3A => {
                let address = self.indirect_address(p);
                self.regs.a = bus.read(address);
            }
            0x03 | 0x13 | 0x23 | 0x33 => {
                self.set_r16(p, self.r16(p).wrapping_add(1));
                bus.tick();
            }
            0x0B | 0x1B | 0x2B | 0x3B => {
                self.set_r16(p, self.r16(p).wrapping_sub(1));
                bus.tick();
            }
            0x09 | 0x19 | 0x29 | 0x39 => {
                let hl = self.regs.hl();
                let value = self.r16(p);
                let (result, carry) = hl.overflowing_add(value);
                let half = (hl & 0x0FFF) + (value & 0x0FFF) > 0x0FFF;
                self.regs
                    .set_flags(self.regs.flag(FLAG_Z), false, half, carry);
                self.regs.set_hl(result);
                bus.tick();
            }
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
                let value = self.read_r8(bus, y);
                let result = value.wrapping_add(1);
                let carry = self.regs.flag(FLAG_C);
                self.regs
                    .set_flags(result == 0, false, value & 0x0F == 0x0F, carry);
                self.write_r8(bus, y, result);
            }
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let value = self.read_r8(bus, y);
                let result = value.wrapping_sub(1);
                let carry = self.regs.flag(FLAG_C);
                self.regs
                    .set_flags(result == 0, true, value & 0x0F == 0, carry);
                self.write_r8(bus, y, result);
            }
            0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
                let value = self.fetch(bus);
                self.write_r8(bus, y, value);
            }
            0x07 | 0x0F | 0x17 | 0x1F => {
                // RLCA, RRCA, RLA, RRA: CB rotates on A, but Z is always clear.
                let result = self.rotate_shift(y, self.regs.a);
                self.regs.a = result;
                self.regs.f &= !FLAG_Z;
            }
            0x08 => {
                let address = self.fetch16(bus);
                let [low, high] = self.regs.sp.to_le_bytes();
                bus.write(address, low);
                bus.write(address.wrapping_add(1), high);
            }
            0x18 => {
                let offset = self.fetch(bus);
                self.jump_relative(bus, offset);
            }
            0x20 | 0x28 | 0x30 | 0x38 => {
                let offset = self.fetch(bus);
                if self.condition(y - 4) {
                    self.jump_relative(bus, offset);
                }
            }
            0x27 => self.daa(),
            0x2F => {
                self.regs.a = !self.regs.a;
                self.regs.f |= FLAG_N | FLAG_H;
            }
            0x37 => {
                let z = self.regs.flag(FLAG_Z);
                self.regs.set_flags(z, false, false, true);
            }
            0x3F => {
                let z = self.regs.flag(FLAG_Z);
                let c = self.regs.flag(FLAG_C);
                self.regs.set_flags(z, false, false, !c);
            }
            0x76 => {
                if !self.ime && bus.pending_interrupts() != 0 {
                    self.halt_bug = true;
                } else {
                    self.halted = true;
                }
            }
            0x40..=0x7F => {
                let value = self.read_r8(bus, z);
                self.write_r8(bus, y, value);
            }
            0x80..=0xBF => {
                let value = self.read_r8(bus, z);
                self.alu(y, value);
            }
            0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
                let value = self.fetch(bus);
                self.alu(y, value);
            }
            0xC0 | 0xC8 | 0xD0 | 0xD8 => {
                bus.tick();
                if self.condition(y) {
                    self.regs.pc = self.pop(bus);
                    bus.tick();
                }
            }
            0xC9 => {
                self.regs.pc = self.pop(bus);
                bus.tick();
            }
            0xD9 => {
                self.regs.pc = self.pop(bus);
                bus.tick();
                self.ime = true;
            }
            0xC1 | 0xD1 | 0xE1 | 0xF1 => {
                let value = self.pop(bus);
                match p {
                    0 => self.regs.set_bc(value),
                    1 => self.regs.set_de(value),
                    2 => self.regs.set_hl(value),
                    _ => self.regs.set_af(value),
                }
            }
            0xC5 | 0xD5 | 0xE5 | 0xF5 => {
                let value = match p {
                    0 => self.regs.bc(),
                    1 => self.regs.de(),
                    2 => self.regs.hl(),
                    _ => self.regs.af(),
                };
                bus.tick();
                self.push(bus, value);
            }
            0xC2 | 0xCA | 0xD2 | 0xDA => {
                let address = self.fetch16(bus);
                if self.condition(y) {
                    self.regs.pc = address;
                    bus.tick();
                }
            }
            0xC3 => {
                self.regs.pc = self.fetch16(bus);
                bus.tick();
            }
            0xE9 => self.regs.pc = self.regs.hl(),
            0xC4 | 0xCC | 0xD4 | 0xDC => {
                let address = self.fetch16(bus);
                if self.condition(y) {
                    self.call(bus, address);
                }
            }
            0xCD => {
                let address = self.fetch16(bus);
                self.call(bus, address);
            }
            0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
                self.call(bus, u16::from(y) * 8);
            }
            0xCB => self.execute_cb(bus),
            0xE0 => {
                let offset = self.fetch(bus);
                bus.write(0xFF00 | u16::from(offset), self.regs.a);
            }
            0xF0 => {
                let offset = self.fetch(bus);
                self.regs.a = bus.read(0xFF00 | u16::from(offset));
            }
            0xE2 => bus.write(0xFF00 | u16::from(self.regs.c), self.regs.a),
            0xF2 => self.regs.a = bus.read(0xFF00 | u16::from(self.regs.c)),
            0xEA => {
                let address = self.fetch16(bus);
                bus.write(address, self.regs.a);
            }
            0xFA => {
                let address = self.fetch16(bus);
                self.regs.a = bus.read(address);
            }
            0xE8 => {
                let offset = self.fetch(bus);
                self.regs.sp = self.add_sp_offset(offset);
                bus.tick();
                bus.tick();
            }
            0xF8 => {
                let offset = self.fetch(bus);
                let value = self.add_sp_offset(offset);
                self.regs.set_hl(value);
                bus.tick();
            }
            0xF9 => {
                self.regs.sp = self.regs.hl();
                bus.tick();
            }
            0xF3 => {
                self.ime = false;
                self.ei_delay = 0;
            }
            0xFB => {
                if !self.ime && self.ei_delay == 0 {
                    self.ei_delay = 2;
                }
            }
            0xD3 | 0xDB | 0xDD | 0xE3 | 0xE4 | 0xEB | 0xEC | 0xED | 0xF4 | 0xFC | 0xFD => {
                self.locked = true;
            }
        }
    }

    fn execute_cb<B: Bus>(&mut self, bus: &mut B) {
        let opcode = self.fetch(bus);
        let y = (opcode >> 3) & 7;
        let z = opcode & 7;
        let value = self.read_r8(bus, z);
        match opcode >> 6 {
            0 => {
                let result = self.rotate_shift(y, value);
                self.write_r8(bus, z, result);
            }
            1 => {
                let carry = self.regs.flag(FLAG_C);
                self.regs
                    .set_flags(value & (1 << y) == 0, false, true, carry);
            }
            2 => self.write_r8(bus, z, value & !(1 << y)),
            _ => self.write_r8(bus, z, value | (1 << y)),
        }
    }

    /// CB-prefix rotate/shift `op`: RLC, RRC, RL, RR, SLA, SRA, SWAP, SRL.
    /// Sets all four flags.
    fn rotate_shift(&mut self, op: u8, value: u8) -> u8 {
        let carry_in = u8::from(self.regs.flag(FLAG_C));
        let (result, carry_out) = match op {
            0 => (value.rotate_left(1), value & 0x80 != 0),
            1 => (value.rotate_right(1), value & 0x01 != 0),
            2 => (value << 1 | carry_in, value & 0x80 != 0),
            3 => (value >> 1 | carry_in << 7, value & 0x01 != 0),
            4 => (value << 1, value & 0x80 != 0),
            5 => (value >> 1 | (value & 0x80), value & 0x01 != 0),
            6 => (value.rotate_left(4), false),
            _ => (value >> 1, value & 0x01 != 0),
        };
        self.regs.set_flags(result == 0, false, false, carry_out);
        result
    }

    /// ALU operation `op` on A: ADD, ADC, SUB, SBC, AND, XOR, OR, CP.
    fn alu(&mut self, op: u8, value: u8) {
        let a = self.regs.a;
        let carry = u8::from(self.regs.flag(FLAG_C));
        match op {
            0 | 1 => {
                let carry = if op == 1 { carry } else { 0 };
                let sum = u16::from(a) + u16::from(value) + u16::from(carry);
                let half = (a & 0x0F) + (value & 0x0F) + carry > 0x0F;
                self.regs.a = sum as u8;
                self.regs.set_flags(sum as u8 == 0, false, half, sum > 0xFF);
            }
            2 | 3 | 7 => {
                let carry = if op == 3 { carry } else { 0 };
                let result = a.wrapping_sub(value).wrapping_sub(carry);
                let half = (a & 0x0F) < (value & 0x0F) + carry;
                let borrow = u16::from(a) < u16::from(value) + u16::from(carry);
                if op != 7 {
                    self.regs.a = result;
                }
                self.regs.set_flags(result == 0, true, half, borrow);
            }
            4 => {
                self.regs.a = a & value;
                self.regs.set_flags(self.regs.a == 0, false, true, false);
            }
            5 => {
                self.regs.a = a ^ value;
                self.regs.set_flags(self.regs.a == 0, false, false, false);
            }
            _ => {
                self.regs.a = a | value;
                self.regs.set_flags(self.regs.a == 0, false, false, false);
            }
        }
    }

    fn daa(&mut self) {
        let subtract = self.regs.flag(FLAG_N);
        let mut carry = self.regs.flag(FLAG_C);
        let mut adjust = 0u8;
        if self.regs.flag(FLAG_H) || (!subtract && self.regs.a & 0x0F > 0x09) {
            adjust |= 0x06;
        }
        if carry || (!subtract && self.regs.a > 0x99) {
            adjust |= 0x60;
            carry = true;
        }
        self.regs.a = if subtract {
            self.regs.a.wrapping_sub(adjust)
        } else {
            self.regs.a.wrapping_add(adjust)
        };
        self.regs
            .set_flags(self.regs.a == 0, subtract, false, carry);
    }

    /// SP plus a signed offset, with H and C from the unsigned low-byte add.
    fn add_sp_offset(&mut self, offset: u8) -> u16 {
        let sp = self.regs.sp;
        let half = (sp & 0x0F) + u16::from(offset & 0x0F) > 0x0F;
        let carry = (sp & 0xFF) + u16::from(offset) > 0xFF;
        self.regs.set_flags(false, false, half, carry);
        sp.wrapping_add_signed(i16::from(offset as i8))
    }

    fn jump_relative<B: Bus>(&mut self, bus: &mut B, offset: u8) {
        self.regs.pc = self.regs.pc.wrapping_add_signed(i16::from(offset as i8));
        bus.tick();
    }

    fn call<B: Bus>(&mut self, bus: &mut B, address: u16) {
        bus.tick();
        let return_address = self.regs.pc;
        self.push(bus, return_address);
        self.regs.pc = address;
    }
}
