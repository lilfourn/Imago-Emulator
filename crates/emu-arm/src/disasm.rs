//! A disassembler for the ARM and Thumb instruction sets, for the
//! debugger (GBATEK, "ARM Opcodes" and "THUMB Opcodes"; ARM Architecture
//! Reference Manual, ARMv5TE).
//!
//! The decode trees follow the interpreter's (`arm.rs`, `armv5.rs` and
//! `thumb.rs`) test for test, so the two can be read side by side. The
//! text is the pre-UAL syntax GBATEK and the ARMv5 manual print, in upper
//! case: the condition comes before the S, B, H, T or D suffix (`ADDEQS`,
//! `LDRNEB`), immediates are hex (`#0x2A`, `[R1, #-0x4]`), shift amounts
//! are decimal (`LSL #2`), and branch targets are absolute addresses.
//!
//! Each architecture decodes the instructions it defines: the ARMv5TE
//! additions print as `UNDEFINED` for ARMv4T, as does condition 0b1111
//! (reserved on ARMv4, ARMv5's unconditional space), and so do encodings
//! neither architecture defines, even where the interpreter runs them as
//! something else. Coprocessor instructions decode on both, although only
//! the ARM9's CP15 answers any of them.

use std::fmt;

/// Which instructions a disassembler accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    /// ARMv4T: the ARM7TDMI (GBA, DS ARM7).
    V4T,
    /// ARMv5TE: the ARM946E-S (DS ARM9).
    V5TE,
}

/// An instruction decoded for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disassembly {
    /// Assembly text, e.g. `MOV R0, #0x2A`.
    pub text: String,
    /// Bytes the instruction takes: 4 for ARM; 2 for Thumb, or 4 for a
    /// Thumb BL/BLX pair decoded together.
    pub length: u32,
    /// Calls a subroutine that returns to the next instruction: ARM BL,
    /// BLX (immediate and register), Thumb BL/BLX pairs and BLX Rm.
    pub call: bool,
}

impl Disassembly {
    /// The same instruction, marked as a call.
    fn into_call(self) -> Self {
        Self { call: true, ..self }
    }
}

/// A 4-byte instruction (ARM, or a Thumb BL/BLX pair) that is not a call.
fn word(text: String) -> Disassembly {
    Disassembly {
        text,
        length: 4,
        call: false,
    }
}

/// A 2-byte Thumb instruction that is not a call.
fn halfword(text: String) -> Disassembly {
    Disassembly {
        text,
        length: 2,
        call: false,
    }
}

/// The text for an encoding the architecture does not define.
fn undefined() -> String {
    String::from("UNDEFINED")
}

/// Condition suffixes, by bits 31–28 (ARM) or 11–8 (Thumb format 16).
/// AL prints none.
const CONDITIONS: [&str; 16] = [
    "EQ", "NE", "CS", "CC", "MI", "PL", "VS", "VC", "HI", "LS", "GE", "LT", "GT", "LE", "", "NV",
];

/// Register names, by number.
const REGISTERS: [&str; 16] = [
    "R0", "R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8", "R9", "R10", "R11", "R12", "SP", "LR",
    "PC",
];

/// Barrel shifter operations, by their two-bit code.
const SHIFTS: [&str; 4] = ["LSL", "LSR", "ASR", "ROR"];

/// The register in the four bits of `opcode` at `shift`.
fn reg(opcode: u32, shift: u32) -> &'static str {
    REGISTERS[((opcode >> shift) & 0xF) as usize]
}

/// The low register (R0–R7) in the three bits of `opcode` at `shift`.
fn low(opcode: u32, shift: u32) -> &'static str {
    REGISTERS[((opcode >> shift) & 7) as usize]
}

/// Whether bit `bit` of `opcode` is set.
fn bit(opcode: u32, bit: u32) -> bool {
    opcode & (1 << bit) != 0
}

/// `suffix` when bit `n` of `opcode` is set, else nothing.
fn flag(opcode: u32, n: u32, suffix: &'static str) -> &'static str {
    if bit(opcode, n) { suffix } else { "" }
}

/// An ARM immediate operand: 8 bits rotated right by twice bits 11–8.
fn rotated_immediate(opcode: u32) -> u32 {
    (opcode & 0xFF).rotate_right(((opcode >> 8) & 0xF) * 2)
}

/// Rm shifted by an immediate (bits 11–0): `R1`, `R1, LSL #2`. LSR #0 and
/// ASR #0 encode a shift by 32, ROR #0 encodes RRX.
struct ShiftedRegister(u32);

impl fmt::Display for ShiftedRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rm = reg(self.0, 0);
        let kind = ((self.0 >> 5) & 3) as usize;
        match (kind, (self.0 >> 7) & 0x1F) {
            (0, 0) => write!(f, "{rm}"),
            (3, 0) => write!(f, "{rm}, RRX"),
            (_, 0) => write!(f, "{rm}, {} #32", SHIFTS[kind]),
            (_, amount) => write!(f, "{rm}, {} #{amount}", SHIFTS[kind]),
        }
    }
}

/// A data processing instruction's second operand (bits 25 and 11–0): a
/// rotated immediate, or Rm shifted by an immediate or by Rs.
struct Operand2(u32);

impl fmt::Display for Operand2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let opcode = self.0;
        if bit(opcode, 25) {
            write!(f, "#0x{:X}", rotated_immediate(opcode))
        } else if bit(opcode, 4) {
            let kind = SHIFTS[((opcode >> 5) & 3) as usize];
            write!(f, "{}, {kind} {}", reg(opcode, 0), reg(opcode, 8))
        } else {
            write!(f, "{}", ShiftedRegister(opcode))
        }
    }
}

/// The offset in a load or store address.
#[derive(Clone, Copy)]
enum Offset {
    /// `#0x4` or `#-0x4`.
    Immediate { add: bool, value: u32 },
    /// `R2` or `-R2`, with any shift: Rm and its shift are bits 11–0 of
    /// `opcode`.
    Register { add: bool, opcode: u32 },
}

impl fmt::Display for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Immediate { add: true, value } => write!(f, "#0x{value:X}"),
            Self::Immediate { add: false, value } => write!(f, "#-0x{value:X}"),
            Self::Register { add, opcode } => {
                let sign = if add { "" } else { "-" };
                write!(f, "{sign}{}", ShiftedRegister(opcode))
            }
        }
    }
}

/// A load or store address from bits 24 (P: pre-indexed), 21 (W:
/// writeback) and 19–16 (Rn), plus an offset: `[Rn, offset]`,
/// `[Rn, offset]!` or `[Rn], offset`. A pre-indexed zero offset prints as
/// `[Rn]`; a PC-relative address without writeback ends with a comment
/// holding the address it reads.
struct Address {
    /// The instruction's address.
    address: u32,
    opcode: u32,
    offset: Offset,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rn = reg(self.opcode, 16);
        if !bit(self.opcode, 24) {
            return write!(f, "[{rn}], {}", self.offset);
        }
        if bit(self.opcode, 21) {
            return write!(f, "[{rn}, {}]!", self.offset);
        }
        match self.offset {
            Offset::Immediate { add, value } if (self.opcode >> 16) & 0xF == 15 => {
                // The PC reads 8 ahead.
                let pc = self.address.wrapping_add(8);
                let target = if add {
                    pc.wrapping_add(value)
                } else {
                    pc.wrapping_sub(value)
                };
                write!(f, "[PC, {}] ; 0x{target:08X}", self.offset)
            }
            Offset::Immediate {
                add: true,
                value: 0,
            } => write!(f, "[{rn}]"),
            _ => write!(f, "[{rn}, {}]", self.offset),
        }
    }
}

/// A register list such as `{R0-R3, R5, LR}`. Runs of three or more of
/// R0–R12 print as ranges; SP, LR and PC always print by name.
struct RegisterList(u32);

impl fmt::Display for RegisterList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = self.0;
        let mut separator = "";
        let mut first = 0;
        f.write_str("{")?;
        while first < 16 {
            if list & (1 << first) == 0 {
                first += 1;
                continue;
            }
            let mut last = first;
            while last < 12 && list & (1 << (last + 1)) != 0 {
                last += 1;
            }
            if last >= first + 2 {
                write!(f, "{separator}{}-{}", REGISTERS[first], REGISTERS[last])?;
                first = last + 1;
            } else {
                write!(f, "{separator}{}", REGISTERS[first])?;
                first += 1;
            }
            separator = ", ";
        }
        f.write_str("}")
    }
}

/// `[Rb]` or `[Rb, #0x4]`: a Thumb base register plus an immediate.
struct BaseOffset {
    base: &'static str,
    offset: u32,
}

impl fmt::Display for BaseOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.offset {
            0 => write!(f, "[{}]", self.base),
            offset => write!(f, "[{}, #0x{offset:X}]", self.base),
        }
    }
}

/// Disassembles the ARM-state instruction `opcode` at `address`.
#[must_use]
pub fn disassemble_arm(address: u32, opcode: u32, architecture: Architecture) -> Disassembly {
    let condition = opcode >> 28;
    if condition == 0xF {
        return match architecture {
            Architecture::V4T => word(undefined()),
            Architecture::V5TE => unconditional(address, opcode),
        };
    }
    let cond = CONDITIONS[condition as usize];
    if architecture == Architecture::V5TE
        && let Some(disassembly) = armv5(address, opcode, cond)
    {
        return disassembly;
    }
    armv4(address, opcode, cond)
}

/// The ARMv4 instruction set, decoded from bits 27–20 and 7–4 in the
/// interpreter's order (`arm::decode`).
fn armv4(address: u32, opcode: u32, cond: &str) -> Disassembly {
    let key = opcode & 0x0FF0_00F0;
    let text = if key & 0x0FF0_00F0 == 0x0120_0010 {
        format!("BX{cond} {}", reg(opcode, 0))
    } else if key & 0x0FB0_00F0 == 0x0100_0090 {
        swap(opcode, cond)
    } else if key & 0x0FC0_00F0 == 0x0000_0090 {
        multiply(opcode, cond)
    } else if key & 0x0F80_00F0 == 0x0080_0090 {
        multiply_long(opcode, cond)
    } else if key & 0x0E00_0090 == 0x0000_0090 {
        halfword_transfer(address, opcode, cond)
    } else if key & 0x0FB0_00F0 == 0x0100_0000 {
        move_from_psr(opcode, cond)
    } else if key & 0x0FB0_00F0 == 0x0120_0000 || key & 0x0FB0_0000 == 0x0320_0000 {
        move_to_psr(opcode, cond)
    } else if key & 0x0C00_0000 == 0 {
        data_processing(opcode, cond)
    } else if key & 0x0E00_0010 == 0x0600_0010 {
        undefined()
    } else if key & 0x0C00_0000 == 0x0400_0000 {
        single_transfer(address, opcode, cond)
    } else if key & 0x0E00_0000 == 0x0800_0000 {
        block_transfer(opcode, cond)
    } else if key & 0x0E00_0000 == 0x0A00_0000 {
        return branch(address, opcode, cond);
    } else if key & 0x0F00_0000 == 0x0F00_0000 {
        format!("SWI{cond} 0x{:06X}", opcode & 0x00FF_FFFF)
    } else if key & 0x0F00_0000 == 0x0E00_0000 {
        coprocessor_operation(opcode, cond)
    } else {
        coprocessor_transfer(address, opcode, cond)
    };
    word(text)
}

/// The ARMv5TE encodings that ARMv4 decodes as something else (the
/// interpreter's `armv5::decode`), or `None`. MRC and MCR are ARMv4
/// instructions too, so [`armv4`] decodes them.
fn armv5(address: u32, opcode: u32, cond: &str) -> Option<Disassembly> {
    let key = opcode & 0x0FF0_00F0;
    let text = if key & 0x0F90_0000 == 0x0100_0000 && key & 0x90 != 0x90 {
        // The space ARMv4 gives to MRS, MSR and BX.
        match key & 0x0060_00F0 {
            0x0060_0010 => format!("CLZ{cond} {}, {}", reg(opcode, 12), reg(opcode, 0)),
            0x0020_0030 => return Some(word(format!("BLX{cond} {}", reg(opcode, 0))).into_call()),
            0x0020_0070 => {
                let comment = ((opcode >> 4) & 0xFFF0) | (opcode & 0xF);
                format!("BKPT{cond} 0x{comment:X}")
            }
            _ if key & 0xF0 == 0x50 => saturating_arithmetic(opcode, cond),
            _ if key & 0x90 == 0x80 => signed_halfword_multiply(opcode, cond),
            _ => return None,
        }
    } else if key & 0x0E10_00D0 == 0x0000_00D0 {
        doubleword_transfer(address, opcode, cond)
    } else if opcode & 0x0FE0_0000 == 0x0C40_0000 {
        coprocessor_register_pair(opcode, cond)
    } else {
        return None;
    };
    Some(word(text))
}

/// ARMv5's condition 0b1111 (the interpreter's `execute_unconditional`):
/// BLX with an immediate, PLD, and the coprocessor "2" forms.
fn unconditional(address: u32, opcode: u32) -> Disassembly {
    let text = if opcode & 0x0E00_0000 == 0x0A00_0000 {
        // PC + offset × 4 + H × 2, where H is bit 24.
        let offset = (((opcode << 8) as i32 >> 6) as u32) | ((opcode >> 23) & 2);
        let target = address.wrapping_add(8).wrapping_add(offset);
        return word(format!("BLX 0x{target:08X}")).into_call();
    } else if opcode & 0x0F00_0000 == 0x0F00_0000 {
        undefined()
    } else if opcode & 0x0F00_0000 == 0x0E00_0000 {
        coprocessor_operation(opcode, "2")
    } else if opcode & 0x0E00_0000 == 0x0C00_0000 {
        coprocessor_transfer(address, opcode, "2")
    } else if opcode & 0x0D70_F000 == 0x0550_F000 {
        let offset = single_transfer_offset(opcode);
        let address = Address {
            address,
            opcode,
            offset,
        };
        format!("PLD {address}")
    } else {
        undefined()
    };
    word(text)
}

/// B and BL: PC + offset × 4.
fn branch(address: u32, opcode: u32, cond: &str) -> Disassembly {
    let offset = ((opcode << 8) as i32 >> 6) as u32;
    let target = address.wrapping_add(8).wrapping_add(offset);
    if bit(opcode, 24) {
        word(format!("BL{cond} 0x{target:08X}")).into_call()
    } else {
        word(format!("B{cond} 0x{target:08X}"))
    }
}

/// Data processing: `ADD{cond}{S} Rd, Rn, Op2`, `MOV{cond}{S} Rd, Op2`,
/// `CMP{cond} Rn, Op2`.
fn data_processing(opcode: u32, cond: &str) -> String {
    const MNEMONICS: [&str; 16] = [
        "AND", "EOR", "SUB", "RSB", "ADD", "ADC", "SBC", "RSC", "TST", "TEQ", "CMP", "CMN", "ORR",
        "MOV", "BIC", "MVN",
    ];
    let operation = (opcode >> 21) & 0xF;
    let mnemonic = MNEMONICS[operation as usize];
    let set_flags = bit(opcode, 20);
    let s = flag(opcode, 20, "S");
    let (rd, rn, operand) = (reg(opcode, 12), reg(opcode, 16), Operand2(opcode));
    match operation {
        // Without S, the comparisons' encodings belong to MRS, MSR, BX and
        // ARMv5's additions; the rest of them are undefined.
        0x8..=0xB if !set_flags => undefined(),
        0x8..=0xB => format!("{mnemonic}{cond} {rn}, {operand}"),
        0xD | 0xF => format!("{mnemonic}{cond}{s} {rd}, {operand}"),
        _ => format!("{mnemonic}{cond}{s} {rd}, {rn}, {operand}"),
    }
}

/// MRS Rd, CPSR or SPSR.
fn move_from_psr(opcode: u32, cond: &str) -> String {
    let psr = if bit(opcode, 22) { "SPSR" } else { "CPSR" };
    format!("MRS{cond} {}, {psr}", reg(opcode, 12))
}

/// MSR CPSR or SPSR, with the fields bits 19–16 select (f, s, x, c), from
/// a register or a rotated immediate.
fn move_to_psr(opcode: u32, cond: &str) -> String {
    let psr = if bit(opcode, 22) { "SPSR" } else { "CPSR" };
    let (f, s) = (flag(opcode, 19, "f"), flag(opcode, 18, "s"));
    let (x, c) = (flag(opcode, 17, "x"), flag(opcode, 16, "c"));
    // The register form has no shift: bits 11–4 are ignored.
    let source = Operand2(if bit(opcode, 25) {
        opcode
    } else {
        opcode & !0xFF0
    });
    format!("MSR{cond} {psr}_{f}{s}{x}{c}, {source}")
}

/// MUL Rd, Rm, Rs and MLA Rd, Rm, Rs, Rn.
fn multiply(opcode: u32, cond: &str) -> String {
    let s = flag(opcode, 20, "S");
    let (rd, rn, rs, rm) = (
        reg(opcode, 16),
        reg(opcode, 12),
        reg(opcode, 8),
        reg(opcode, 0),
    );
    if bit(opcode, 21) {
        format!("MLA{cond}{s} {rd}, {rm}, {rs}, {rn}")
    } else {
        format!("MUL{cond}{s} {rd}, {rm}, {rs}")
    }
}

/// UMULL, UMLAL, SMULL and SMLAL RdLo, RdHi, Rm, Rs.
fn multiply_long(opcode: u32, cond: &str) -> String {
    let mnemonic = ["UMULL", "UMLAL", "SMULL", "SMLAL"][((opcode >> 21) & 3) as usize];
    let s = flag(opcode, 20, "S");
    let (rd_high, rd_low) = (reg(opcode, 16), reg(opcode, 12));
    let (rs, rm) = (reg(opcode, 8), reg(opcode, 0));
    format!("{mnemonic}{cond}{s} {rd_low}, {rd_high}, {rm}, {rs}")
}

/// SWP and SWPB Rd, Rm, [Rn].
fn swap(opcode: u32, cond: &str) -> String {
    let b = flag(opcode, 22, "B");
    let (rd, rm, rn) = (reg(opcode, 12), reg(opcode, 0), reg(opcode, 16));
    format!("SWP{cond}{b} {rd}, {rm}, [{rn}]")
}

/// The offset of LDR, STR and PLD: a 12-bit immediate, or (bit 25) a
/// register shifted by an immediate, added or (bit 23 clear) subtracted.
fn single_transfer_offset(opcode: u32) -> Offset {
    let add = bit(opcode, 23);
    if bit(opcode, 25) {
        Offset::Register { add, opcode }
    } else {
        Offset::Immediate {
            add,
            value: opcode & 0xFFF,
        }
    }
}

/// LDR, STR, LDRB, STRB, and the T forms (post-indexed with W set) that
/// use User mode's permissions.
fn single_transfer(address: u32, opcode: u32, cond: &str) -> String {
    let mnemonic = if bit(opcode, 20) { "LDR" } else { "STR" };
    let b = flag(opcode, 22, "B");
    let t = if !bit(opcode, 24) && bit(opcode, 21) {
        "T"
    } else {
        ""
    };
    let offset = single_transfer_offset(opcode);
    let address = Address {
        address,
        opcode,
        offset,
    };
    format!("{mnemonic}{cond}{b}{t} {}, {address}", reg(opcode, 12))
}

/// The offset of a halfword or doubleword transfer: an 8-bit immediate
/// split across bits 11–8 and 3–0 (bit 22 set), or Rm.
fn split_offset(opcode: u32) -> Offset {
    let add = bit(opcode, 23);
    if bit(opcode, 22) {
        let value = ((opcode >> 4) & 0xF0) | (opcode & 0xF);
        Offset::Immediate { add, value }
    } else {
        Offset::Register {
            add,
            opcode: opcode & 0xF,
        }
    }
}

/// LDRH, STRH, LDRSB and LDRSH, by bits 20 (load) and 6–5.
fn halfword_transfer(address: u32, opcode: u32, cond: &str) -> String {
    let load = bit(opcode, 20);
    let suffix = match (load, (opcode >> 5) & 3) {
        (_, 1) => "H",
        (true, 2) => "SB",
        (true, 3) => "SH",
        // Bits 6–5 clear belong to SWP and the multiplies; stores of
        // signed halfwords are ARMv5's LDRD and STRD.
        _ => return undefined(),
    };
    let mnemonic = if load { "LDR" } else { "STR" };
    let offset = split_offset(opcode);
    let address = Address {
        address,
        opcode,
        offset,
    };
    format!("{mnemonic}{cond}{suffix} {}, {address}", reg(opcode, 12))
}

/// LDM and STM: `LDM{cond}{IA|IB|DA|DB} Rn{!}, {list}{^}`.
fn block_transfer(opcode: u32, cond: &str) -> String {
    let mnemonic = if bit(opcode, 20) { "LDM" } else { "STM" };
    // Bit 24 (P) picks before or after, bit 23 (U) up or down.
    let mode = ["DA", "IA", "DB", "IB"][((opcode >> 23) & 3) as usize];
    let writeback = flag(opcode, 21, "!");
    let user = flag(opcode, 22, "^");
    let list = RegisterList(opcode & 0xFFFF);
    format!(
        "{mnemonic}{cond}{mode} {}{writeback}, {list}{user}",
        reg(opcode, 16)
    )
}

/// CDP and (bit 4 set) MRC and MCR. `suffix` is the condition, or `2`
/// for ARMv5's unconditional forms.
fn coprocessor_operation(opcode: u32, suffix: &str) -> String {
    let coprocessor = (opcode >> 8) & 0xF;
    let (crn, crm) = ((opcode >> 16) & 0xF, opcode & 0xF);
    let operation2 = (opcode >> 5) & 7;
    if bit(opcode, 4) {
        let mnemonic = if bit(opcode, 20) { "MRC" } else { "MCR" };
        let operation1 = (opcode >> 21) & 7;
        let rd = reg(opcode, 12);
        format!(
            "{mnemonic}{suffix} P{coprocessor}, {operation1}, {rd}, C{crn}, C{crm}, {operation2}"
        )
    } else {
        let operation1 = (opcode >> 20) & 0xF;
        let crd = (opcode >> 12) & 0xF;
        format!("CDP{suffix} P{coprocessor}, {operation1}, C{crd}, C{crn}, C{crm}, {operation2}")
    }
}

/// LDC and STC (bit 22 for the L forms), with a word offset, or (P and W
/// clear, U set; ARMv5) an unindexed option for the coprocessor.
fn coprocessor_transfer(address: u32, opcode: u32, suffix: &str) -> String {
    let mnemonic = if bit(opcode, 20) { "LDC" } else { "STC" };
    let long = flag(opcode, 22, "L");
    let coprocessor = (opcode >> 8) & 0xF;
    let crd = (opcode >> 12) & 0xF;
    let add = bit(opcode, 23);
    if !bit(opcode, 24) && !bit(opcode, 21) {
        let (rn, option) = (reg(opcode, 16), opcode & 0xFF);
        return if add {
            format!("{mnemonic}{suffix}{long} P{coprocessor}, C{crd}, [{rn}], {{{option}}}")
        } else {
            undefined()
        };
    }
    let offset = Offset::Immediate {
        add,
        value: (opcode & 0xFF) * 4,
    };
    let address = Address {
        address,
        opcode,
        offset,
    };
    format!("{mnemonic}{suffix}{long} P{coprocessor}, C{crd}, {address}")
}

/// ARMv5TE's MCRR and MRRC: two registers to or from a coprocessor.
fn coprocessor_register_pair(opcode: u32, cond: &str) -> String {
    let mnemonic = if bit(opcode, 20) { "MRRC" } else { "MCRR" };
    let coprocessor = (opcode >> 8) & 0xF;
    let operation = (opcode >> 4) & 0xF;
    let (rd, rn, crm) = (reg(opcode, 12), reg(opcode, 16), opcode & 0xF);
    format!("{mnemonic}{cond} P{coprocessor}, {operation}, {rd}, {rn}, C{crm}")
}

/// QADD, QSUB, QDADD and QDSUB Rd, Rm, Rn.
fn saturating_arithmetic(opcode: u32, cond: &str) -> String {
    let mnemonic = ["QADD", "QSUB", "QDADD", "QDSUB"][((opcode >> 21) & 3) as usize];
    let (rd, rm, rn) = (reg(opcode, 12), reg(opcode, 0), reg(opcode, 16));
    format!("{mnemonic}{cond} {rd}, {rm}, {rn}")
}

/// SMLAxy, SMLAWy, SMULWy, SMLALxy and SMULxy, where x (bit 5) picks Rm's
/// bottom or top half and y (bit 6) Rs's.
fn signed_halfword_multiply(opcode: u32, cond: &str) -> String {
    let x = if bit(opcode, 5) { "T" } else { "B" };
    let y = if bit(opcode, 6) { "T" } else { "B" };
    let (rd, rn) = (reg(opcode, 16), reg(opcode, 12));
    let (rs, rm) = (reg(opcode, 8), reg(opcode, 0));
    match (opcode >> 21) & 3 {
        0 => format!("SMLA{x}{y}{cond} {rd}, {rm}, {rs}, {rn}"),
        // Bit 5 here picks SMULWy instead of a half of Rm.
        1 if !bit(opcode, 5) => format!("SMLAW{y}{cond} {rd}, {rm}, {rs}, {rn}"),
        1 => format!("SMULW{y}{cond} {rd}, {rm}, {rs}"),
        // RdLo is bits 15–12, RdHi bits 19–16.
        2 => format!("SMLAL{x}{y}{cond} {rn}, {rd}, {rm}, {rs}"),
        _ => format!("SMUL{x}{y}{cond} {rd}, {rm}, {rs}"),
    }
}

/// LDRD and STRD (bit 5): Rd and Rd+1, addressed like LDRH. An odd Rd is
/// undefined.
fn doubleword_transfer(address: u32, opcode: u32, cond: &str) -> String {
    if bit(opcode, 12) {
        return undefined();
    }
    let mnemonic = if bit(opcode, 5) { "STR" } else { "LDR" };
    let offset = split_offset(opcode);
    let address = Address {
        address,
        opcode,
        offset,
    };
    format!("{mnemonic}{cond}D {}, {address}", reg(opcode, 12))
}

/// Disassembles the Thumb instruction `opcode` at `address`. `next` is the
/// halfword after it: when `opcode` is the first half of a BL/BLX pair and
/// `next` its second half, both decode together as one 4-byte instruction
/// with the full target. A lone first half prints as `BL prefix` and the
/// value it puts in LR; a lone second half as `BL suffix` (or `BLX
/// suffix`) and the offset it adds to LR, and is a call.
#[must_use]
pub fn disassemble_thumb(
    address: u32,
    opcode: u16,
    next: u16,
    architecture: Architecture,
) -> Disassembly {
    let op = u32::from(opcode);
    let v5 = architecture == Architecture::V5TE;
    let text = match op >> 8 {
        0x00..=0x17 => thumb_shift_immediate(op),
        0x18..=0x1F => thumb_add_subtract(op),
        0x20..=0x3F => thumb_immediate(op),
        0x40..=0x43 => thumb_alu(op),
        0x44..=0x47 => return thumb_high_register(op, architecture),
        0x48..=0x4F => thumb_load_pc_relative(address, op),
        0x50..=0x5F => thumb_load_store_register(op),
        0x60..=0x7F => thumb_load_store_immediate(op),
        0x80..=0x8F => thumb_load_store_half(op),
        0x90..=0x9F => thumb_load_store_sp(op),
        0xA0..=0xAF => thumb_load_address(op),
        0xB0 => thumb_adjust_sp(op),
        0xB4 | 0xB5 | 0xBC | 0xBD => thumb_push_pop(op),
        0xC0..=0xCF => thumb_load_store_multiple(op),
        // 0xDE (condition "always") is reserved, but the ARM7TDMI branches.
        0xD0..=0xDE => thumb_conditional_branch(address, op),
        0xDF => format!("SWI 0x{:X}", op & 0xFF),
        0xE0..=0xE7 => thumb_branch(address, op),
        0xE8..=0xEF if v5 => return thumb_branch_link_suffix(op),
        0xBE if v5 => format!("BKPT 0x{:X}", op & 0xFF),
        0xF0..=0xF7 => return thumb_branch_link(address, op, u32::from(next), architecture),
        0xF8..=0xFF => return thumb_branch_link_suffix(op),
        // 0xB1–0xB3, 0xB6–0xBB and 0xBF are undefined; so are 0xBE
        // (BKPT) and 0xE8–0xEF (the BLX suffix) on ARMv4T.
        _ => undefined(),
    };
    halfword(text)
}

/// Format 1: LSL, LSR and ASR Rd, Rs, #amount. LSR #0 and ASR #0 shift
/// by 32.
fn thumb_shift_immediate(op: u32) -> String {
    let kind = (op >> 11) & 3;
    let amount = match (op >> 6) & 0x1F {
        0 if kind != 0 => 32,
        amount => amount,
    };
    let mnemonic = SHIFTS[kind as usize];
    format!("{mnemonic} {}, {}, #{amount}", low(op, 0), low(op, 3))
}

/// Format 2: ADD and SUB Rd, Rs, with a register or 3-bit immediate.
fn thumb_add_subtract(op: u32) -> String {
    let mnemonic = if bit(op, 9) { "SUB" } else { "ADD" };
    let (rd, rs) = (low(op, 0), low(op, 3));
    if bit(op, 10) {
        format!("{mnemonic} {rd}, {rs}, #0x{:X}", (op >> 6) & 7)
    } else {
        format!("{mnemonic} {rd}, {rs}, {}", low(op, 6))
    }
}

/// Format 3: MOV, CMP, ADD and SUB Rd, #imm8.
fn thumb_immediate(op: u32) -> String {
    let mnemonic = ["MOV", "CMP", "ADD", "SUB"][((op >> 11) & 3) as usize];
    format!("{mnemonic} {}, #0x{:X}", low(op, 8), op & 0xFF)
}

/// Format 4: register-to-register ALU operations, Rd, Rs.
fn thumb_alu(op: u32) -> String {
    const MNEMONICS: [&str; 16] = [
        "AND", "EOR", "LSL", "LSR", "ASR", "ADC", "SBC", "ROR", "TST", "NEG", "CMP", "CMN", "ORR",
        "MUL", "BIC", "MVN",
    ];
    let mnemonic = MNEMONICS[((op >> 6) & 0xF) as usize];
    format!("{mnemonic} {}, {}", low(op, 0), low(op, 3))
}

/// Format 5: ADD, CMP and MOV with high registers, BX, and (ARMv5) BLX,
/// which ARMv4T runs as BX.
fn thumb_high_register(op: u32, architecture: Architecture) -> Disassembly {
    // Bit 7 (H1) extends Rd; bit 6 (H2) extends Rs, so bits 6–3 are Rs.
    let rd = REGISTERS[((op & 7) | ((op >> 4) & 8)) as usize];
    let rs = reg(op, 3);
    match (op >> 8) & 3 {
        0 => halfword(format!("ADD {rd}, {rs}")),
        1 => halfword(format!("CMP {rd}, {rs}")),
        2 => halfword(format!("MOV {rd}, {rs}")),
        _ if architecture == Architecture::V5TE && bit(op, 7) => {
            halfword(format!("BLX {rs}")).into_call()
        }
        _ => halfword(format!("BX {rs}")),
    }
}

/// Format 6: LDR Rd, [PC, #imm], where the PC (4 ahead) is word-aligned.
fn thumb_load_pc_relative(address: u32, op: u32) -> String {
    let offset = (op & 0xFF) * 4;
    let target = (address.wrapping_add(4) & !3).wrapping_add(offset);
    format!("LDR {}, [PC, #0x{offset:X}] ; 0x{target:08X}", low(op, 8))
}

/// Formats 7 and 8: loads and stores of Rd at [Rb, Ro].
fn thumb_load_store_register(op: u32) -> String {
    const MNEMONICS: [&str; 8] = [
        "STR", "STRH", "STRB", "LDRSB", "LDR", "LDRH", "LDRB", "LDRSH",
    ];
    let mnemonic = MNEMONICS[((op >> 9) & 7) as usize];
    let (rd, rb, ro) = (low(op, 0), low(op, 3), low(op, 6));
    format!("{mnemonic} {rd}, [{rb}, {ro}]")
}

/// Format 9: word and byte loads and stores with a 5-bit offset (in words
/// for word transfers).
fn thumb_load_store_immediate(op: u32) -> String {
    let byte = bit(op, 12);
    let mnemonic = match (bit(op, 11), byte) {
        (false, false) => "STR",
        (false, true) => "STRB",
        (true, false) => "LDR",
        (true, true) => "LDRB",
    };
    let offset = (op >> 6) & 0x1F;
    let offset = if byte { offset } else { offset * 4 };
    let base = low(op, 3);
    format!("{mnemonic} {}, {}", low(op, 0), BaseOffset { base, offset })
}

/// Format 10: LDRH and STRH with a 5-bit offset in halfwords.
fn thumb_load_store_half(op: u32) -> String {
    let mnemonic = if bit(op, 11) { "LDRH" } else { "STRH" };
    let offset = ((op >> 6) & 0x1F) * 2;
    let base = low(op, 3);
    format!("{mnemonic} {}, {}", low(op, 0), BaseOffset { base, offset })
}

/// Format 11: LDR and STR Rd, [SP, #imm].
fn thumb_load_store_sp(op: u32) -> String {
    let mnemonic = if bit(op, 11) { "LDR" } else { "STR" };
    let offset = (op & 0xFF) * 4;
    let base = "SP";
    format!("{mnemonic} {}, {}", low(op, 8), BaseOffset { base, offset })
}

/// Format 12: ADD Rd, PC or SP, #imm.
fn thumb_load_address(op: u32) -> String {
    let base = if bit(op, 11) { "SP" } else { "PC" };
    format!("ADD {}, {base}, #0x{:X}", low(op, 8), (op & 0xFF) * 4)
}

/// Format 13: ADD SP, #±imm.
fn thumb_adjust_sp(op: u32) -> String {
    let sign = if bit(op, 7) { "-" } else { "" };
    format!("ADD SP, #{sign}0x{:X}", (op & 0x7F) * 4)
}

/// Format 14: PUSH {list, LR} and POP {list, PC}.
fn thumb_push_pop(op: u32) -> String {
    let load = bit(op, 11);
    let mut list = op & 0xFF;
    if bit(op, 8) {
        list |= if load { 1 << 15 } else { 1 << 14 };
    }
    let mnemonic = if load { "POP" } else { "PUSH" };
    format!("{mnemonic} {}", RegisterList(list))
}

/// Format 15: LDMIA and STMIA Rb!, {list}.
fn thumb_load_store_multiple(op: u32) -> String {
    let mnemonic = if bit(op, 11) { "LDMIA" } else { "STMIA" };
    format!("{mnemonic} {}!, {}", low(op, 8), RegisterList(op & 0xFF))
}

/// Format 16: conditional branch to PC + offset × 2.
fn thumb_conditional_branch(address: u32, op: u32) -> String {
    let offset = (i32::from((op & 0xFF) as u8 as i8) * 2) as u32;
    let target = address.wrapping_add(4).wrapping_add(offset);
    let cond = CONDITIONS[((op >> 8) & 0xF) as usize];
    format!("B{cond} 0x{target:08X}")
}

/// Format 18: unconditional branch to PC + offset × 2.
fn thumb_branch(address: u32, op: u32) -> String {
    let offset = (((op << 21) as i32) >> 20) as u32;
    let target = address.wrapping_add(4).wrapping_add(offset);
    format!("B 0x{target:08X}")
}

/// Format 19's first half, which sets LR = PC + (offset << 12): with the
/// second half in `next`, BL (or on ARMv5, BLX to ARM code at a
/// word-aligned target) to LR + (next's offset << 1).
fn thumb_branch_link(address: u32, op: u32, next: u32, architecture: Architecture) -> Disassembly {
    let lr = address
        .wrapping_add(4)
        .wrapping_add((((op << 21) as i32) >> 9) as u32);
    let target = lr.wrapping_add((next & 0x7FF) << 1);
    let blx_suffix = next >> 11 == 0x1D && next & 1 == 0;
    if next >> 11 == 0x1F {
        word(format!("BL 0x{target:08X}")).into_call()
    } else if blx_suffix && architecture == Architecture::V5TE {
        word(format!("BLX 0x{:08X}", target & !3)).into_call()
    } else {
        halfword(format!("BL prefix 0x{lr:08X}"))
    }
}

/// Format 19's second half on its own: a call to LR + (offset << 1). The
/// BLX form (ARMv5) goes to ARM code; with an odd offset it is undefined.
fn thumb_branch_link_suffix(op: u32) -> Disassembly {
    let offset = (op & 0x7FF) << 1;
    if op >> 11 == 0x1F {
        halfword(format!("BL suffix #0x{offset:X}")).into_call()
    } else if op & 1 != 0 {
        halfword(undefined())
    } else {
        halfword(format!("BLX suffix #0x{offset:X}")).into_call()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROM: u32 = 0x0800_0000;
    /// Where the ARM9 tests in `tests/arm9.rs` run.
    const MAIN: u32 = 0x0200_1000;

    /// Checks ARM instructions both architectures decode alike, at `ROM`;
    /// none is a call.
    fn arm_both(cases: &[(u32, &str)]) {
        for &(opcode, text) in cases {
            for architecture in [Architecture::V4T, Architecture::V5TE] {
                let disassembly = disassemble_arm(ROM, opcode, architecture);
                let expected = Disassembly {
                    text: text.to_string(),
                    length: 4,
                    call: false,
                };
                assert_eq!(disassembly, expected, "{opcode:08X} on {architecture:?}");
            }
        }
    }

    /// Checks ARMv5TE-only instructions at `ROM`: undefined on ARMv4T.
    fn arm_v5_only(cases: &[(u32, &str)]) {
        for &(opcode, text) in cases {
            let v5 = disassemble_arm(ROM, opcode, Architecture::V5TE);
            assert_eq!(v5.text, text, "{opcode:08X}");
            assert!(!v5.call, "{opcode:08X}");
            let v4 = disassemble_arm(ROM, opcode, Architecture::V4T);
            assert_eq!(v4.text, "UNDEFINED", "{opcode:08X} on ARMv4T");
        }
    }

    /// Checks Thumb instructions both architectures decode alike, at `ROM`
    /// and followed by a non-BL halfword; none is a call.
    fn thumb_both(cases: &[(u16, &str)]) {
        for &(opcode, text) in cases {
            for architecture in [Architecture::V4T, Architecture::V5TE] {
                let disassembly = disassemble_thumb(ROM, opcode, 0, architecture);
                let expected = Disassembly {
                    text: text.to_string(),
                    length: 2,
                    call: false,
                };
                assert_eq!(disassembly, expected, "{opcode:04X} on {architecture:?}");
            }
        }
    }

    fn arm(address: u32, opcode: u32, architecture: Architecture) -> Disassembly {
        disassemble_arm(address, opcode, architecture)
    }

    fn thumb(address: u32, opcode: u16, next: u16, architecture: Architecture) -> Disassembly {
        disassemble_thumb(address, opcode, next, architecture)
    }

    fn call(text: &str, length: u32) -> Disassembly {
        Disassembly {
            text: text.to_string(),
            length,
            call: true,
        }
    }

    #[test]
    fn data_processing() {
        arm_both(&[
            (0xE3A0_002A, "MOV R0, #0x2A"),
            (0xE3A0_0301, "MOV R0, #0x4000000"),
            (0xE3E0_0000, "MVN R0, #0x0"),
            (0x0091_0002, "ADDEQS R0, R1, R2"),
            (0xE1B0_F00E, "MOVS PC, LR"),
            (0xE25E_F004, "SUBS PC, LR, #0x4"),
            (0xE351_0001, "CMP R1, #0x1"),
            (0x1311_0080, "TSTNE R1, #0x80"),
            (0xC1D0_0001, "BICGTS R0, R0, R1"),
            (0xE1A0_2602, "MOV R2, R2, LSL #12"),
            (0xE1A0_3623, "MOV R3, R3, LSR #12"),
            (0xE1A0_0021, "MOV R0, R1, LSR #32"),
            (0xE1A0_0041, "MOV R0, R1, ASR #32"),
            (0xE1A0_0461, "MOV R0, R1, ROR #8"),
            (0xE1A0_0061, "MOV R0, R1, RRX"),
            (0xE081_0312, "ADD R0, R1, R2, LSL R3"),
            (0xE180_0231, "ORR R0, R0, R1, LSR R2"),
            (0xE0D0_0251, "SBCS R0, R0, R1, ASR R2"),
            // A comparison without S is not one.
            (0xE100_0011, "UNDEFINED"),
        ]);
    }

    #[test]
    fn psr_transfers() {
        arm_both(&[
            (0xE10F_0000, "MRS R0, CPSR"),
            (0xE14F_1000, "MRS R1, SPSR"),
            (0xE129_F000, "MSR CPSR_fc, R0"),
            (0xE16F_F001, "MSR SPSR_fsxc, R1"),
            (0xE328_F20F, "MSR CPSR_f, #0xF0000000"),
            (0xE368_F20F, "MSR SPSR_f, #0xF0000000"),
            (0xE328_F000, "MSR CPSR_f, #0x0"),
        ]);
    }

    #[test]
    fn multiplies_and_swaps() {
        arm_both(&[
            (0xE000_0291, "MUL R0, R1, R2"),
            (0xE030_3291, "MLAS R0, R1, R2, R3"),
            (0xE081_0392, "UMULL R0, R1, R2, R3"),
            (0xE0A1_0392, "UMLAL R0, R1, R2, R3"),
            (0x10C1_0392, "SMULLNE R0, R1, R2, R3"),
            (0xE0F1_0392, "SMLALS R0, R1, R2, R3"),
            (0xE101_0092, "SWP R0, R2, [R1]"),
            (0xE141_0092, "SWPB R0, R2, [R1]"),
        ]);
    }

    #[test]
    fn single_transfers() {
        arm_both(&[
            (0xE591_0000, "LDR R0, [R1]"),
            (0xE591_0004, "LDR R0, [R1, #0x4]"),
            (0xE5B1_0004, "LDR R0, [R1, #0x4]!"),
            (0xE512_3008, "LDR R3, [R2, #-0x8]"),
            (0xE491_0004, "LDR R0, [R1], #0x4"),
            (0xE611_0102, "LDR R0, [R1], -R2, LSL #2"),
            (0xE791_0102, "LDR R0, [R1, R2, LSL #2]"),
            (0xE721_0002, "STR R0, [R1, -R2]!"),
            (0xE5A1_0004, "STR R0, [R1, #0x4]!"),
            (0x15D1_0000, "LDRNEB R0, [R1]"),
            (0xE5C1_0000, "STRB R0, [R1]"),
            (0xE4B1_0000, "LDRT R0, [R1], #0x0"),
            (0xE4F1_0001, "LDRBT R0, [R1], #0x1"),
            (0xE590_F000, "LDR PC, [R0]"),
            (0xE59F_0010, "LDR R0, [PC, #0x10] ; 0x08000018"),
            (0xE51F_0010, "LDR R0, [PC, #-0x10] ; 0x07FFFFF8"),
            (0xE600_0010, "UNDEFINED"),
        ]);
    }

    #[test]
    fn halfword_transfers() {
        arm_both(&[
            (0xE1C0_10B4, "STRH R1, [R0, #0x4]"),
            (0xE0C1_00B2, "STRH R0, [R1], #0x2"),
            (0xE1D1_00B0, "LDRH R0, [R1]"),
            (0xE111_00B2, "LDRH R0, [R1, -R2]"),
            (0xE1D1_00D0, "LDRSB R0, [R1]"),
            (0xE1F1_00F2, "LDRSH R0, [R1, #0x2]!"),
            (0x01DF_00F4, "LDREQSH R0, [PC, #0x4] ; 0x0800000C"),
            // The multiply and swap space, not a transfer.
            (0xE110_0090, "UNDEFINED"),
        ]);
    }

    #[test]
    fn block_transfers() {
        arm_both(&[
            (0xE884_5005, "STMIA R4, {R0, R2, R12, LR}"),
            (0xE891_8001, "LDMIA R1, {R0, PC}"),
            (0xE8B1_000D, "LDMIA R1!, {R0, R2, R3}"),
            (0xE921_000D, "STMDB R1!, {R0, R2, R3}"),
            (0xE8B0_002E, "LDMIA R0!, {R1-R3, R5}"),
            (0xE92D_4FF0, "STMDB SP!, {R4-R11, LR}"),
            (0xE8FD_9FFF, "LDMIA SP!, {R0-R12, PC}^"),
            (0xE8C0_0002, "STMIA R0, {R1}^"),
            (0xE980_0006, "STMIB R0, {R1, R2}"),
            (0x0800_E000, "STMEQDA R0, {SP, LR, PC}"),
            (0xE890_0000, "LDMIA R0, {}"),
        ]);
    }

    #[test]
    fn branches() {
        let v4 = Architecture::V4T;
        assert_eq!(arm(ROM, 0xEAFF_FFFE, v4).text, "B 0x08000000");
        assert_eq!(arm(0x0800_0010, 0xEAFF_FFFC, v4).text, "B 0x08000008");
        assert_eq!(arm(ROM, 0xEA00_0010, v4).text, "B 0x08000048");
        assert!(!arm(ROM, 0xEA00_0010, v4).call);
        // Backward past address 0 wraps.
        assert_eq!(arm(0, 0xEAFF_FFFD, v4).text, "B 0xFFFFFFFC");
        assert_eq!(arm(ROM, 0xEB00_0010, v4), call("BL 0x08000048", 4));
        assert_eq!(arm(ROM, 0x0B00_03FE, v4), call("BLEQ 0x08001000", 4));
        assert_eq!(arm(ROM, 0xEBFF_FFFE, v4), call("BL 0x08000000", 4));
        assert_eq!(arm(ROM, 0xE12F_FF1E, v4).text, "BX LR");
        assert!(!arm(ROM, 0xE12F_FF1E, v4).call);
        assert_eq!(arm(ROM, 0x112F_FF10, v4).text, "BXNE R0");
    }

    #[test]
    fn software_interrupts() {
        arm_both(&[
            (0xEF05_0000, "SWI 0x050000"),
            (0xEF00_0006, "SWI 0x000006"),
            (0x1F12_3456, "SWINE 0x123456"),
        ]);
    }

    #[test]
    fn coprocessor_instructions() {
        arm_both(&[
            (0xEE11_0F10, "MRC P15, 0, R0, C1, C0, 0"),
            (0xEE10_FF10, "MRC P15, 0, PC, C0, C0, 0"),
            (0xEE07_0F90, "MCR P15, 0, R0, C7, C0, 4"),
            (0xEE06_2F32, "MCR P15, 0, R2, C6, C2, 1"),
            (0x1E01_2103, "CDPNE P1, 0, C2, C1, C3, 0"),
            (0xED91_5E02, "LDC P14, C5, [R1, #0x8]"),
            (0xEC61_5E02, "STCL P14, C5, [R1], #-0x8"),
            (0xEC81_5E02, "STC P14, C5, [R1], {2}"),
        ]);
        arm_v5_only(&[
            (0xEC41_2F30, "MCRR P15, 3, R2, R1, C0"),
            (0xEC51_2F30, "MRRC P15, 3, R2, R1, C0"),
            (0xFE11_0F10, "MRC2 P15, 0, R0, C1, C0, 0"),
            (0xFE01_2103, "CDP2 P1, 0, C2, C1, C3, 0"),
            (0xFD91_5E02, "LDC2 P14, C5, [R1, #0x8]"),
        ]);
    }

    #[test]
    fn armv5_additions() {
        arm_v5_only(&[
            (0xE16F_0F11, "CLZ R0, R1"),
            (0x116F_0F11, "CLZNE R0, R1"),
            (0xE102_0051, "QADD R0, R1, R2"),
            (0xE122_0051, "QSUB R0, R1, R2"),
            (0xE142_0051, "QDADD R0, R1, R2"),
            (0xE162_0051, "QDSUB R0, R1, R2"),
            (0xE100_3281, "SMLABB R0, R1, R2, R3"),
            (0xE100_32C1, "SMLABT R0, R1, R2, R3"),
            (0x0100_32A1, "SMLATBEQ R0, R1, R2, R3"),
            (0xE120_32C1, "SMLAWT R0, R1, R2, R3"),
            (0xE120_02A1, "SMULWB R0, R1, R2"),
            (0xE141_03E2, "SMLALTT R0, R1, R2, R3"),
            (0xE160_02A1, "SMULTB R0, R1, R2"),
            (0xE120_0070, "BKPT 0x0"),
            (0xE12A_BC7D, "BKPT 0xABCD"),
            (0xE1C1_20D0, "LDRD R2, [R1]"),
            (0xE100_40D5, "LDRD R4, [R0, -R5]"),
            (0xE1E0_20D8, "LDRD R2, [R0, #0x8]!"),
            (0xE0C1_20F8, "STRD R2, [R1], #0x8"),
            (0xE041_21F0, "STRD R2, [R1], #-0x10"),
            (0xF5D0_F020, "PLD [R0, #0x20]"),
            (0xF7D0_F102, "PLD [R0, R2, LSL #2]"),
        ]);
        // LDRD with an odd register is undefined on ARMv5 too.
        let v5 = Architecture::V5TE;
        assert_eq!(arm(ROM, 0xE1C0_10D0, v5).text, "UNDEFINED");
    }

    #[test]
    fn armv5_calls() {
        let (v4, v5) = (Architecture::V4T, Architecture::V5TE);
        assert_eq!(arm(ROM, 0xE12F_FF33, v5), call("BLX R3", 4));
        assert_eq!(arm(ROM, 0x012F_FF33, v5), call("BLXEQ R3", 4));
        // Forward with H set, and backward (`tests/arm9.rs`).
        assert_eq!(arm(MAIN, 0xFB00_003E, v5), call("BLX 0x02001102", 4));
        assert_eq!(arm(MAIN, 0xFAFF_FFBE, v5), call("BLX 0x02000F00", 4));
        for opcode in [0xE12F_FF33, 0xFB00_003E] {
            let undefined = arm(ROM, opcode, v4);
            assert_eq!(
                (undefined.text.as_str(), undefined.call),
                ("UNDEFINED", false)
            );
        }
    }

    #[test]
    fn condition_never() {
        let (v4, v5) = (Architecture::V4T, Architecture::V5TE);
        // Reserved on ARMv4T; unconditional, and undefined, here on ARMv5.
        assert_eq!(arm(ROM, 0xF3A0_0000, v4).text, "UNDEFINED");
        assert_eq!(arm(ROM, 0xF3A0_0000, v5).text, "UNDEFINED");
        assert_eq!(arm(ROM, 0xFF00_0000, v5).text, "UNDEFINED");
    }

    #[test]
    fn thumb_registers_and_immediates() {
        thumb_both(&[
            // Format 1.
            (0x0088, "LSL R0, R1, #2"),
            (0x0808, "LSR R0, R1, #32"),
            (0x115A, "ASR R2, R3, #5"),
            // Format 2.
            (0x1888, "ADD R0, R1, R2"),
            (0x1EC8, "SUB R0, R1, #0x3"),
            // Format 3.
            (0x202A, "MOV R0, #0x2A"),
            (0x2BFF, "CMP R3, #0xFF"),
            (0x3501, "ADD R5, #0x1"),
            (0x3E10, "SUB R6, #0x10"),
            // Format 4.
            (0x4008, "AND R0, R1"),
            (0x4248, "NEG R0, R1"),
            (0x435A, "MUL R2, R3"),
            (0x43C8, "MVN R0, R1"),
            (0x41C8, "ROR R0, R1"),
            // Format 5.
            (0x4680, "MOV R8, R0"),
            (0x4478, "ADD R0, PC"),
            (0x45C8, "CMP R8, R9"),
            (0x4770, "BX LR"),
            // Formats 12 and 13.
            (0xA004, "ADD R0, PC, #0x10"),
            (0xA902, "ADD R1, SP, #0x8"),
            (0xB004, "ADD SP, #0x10"),
            (0xB082, "ADD SP, #-0x8"),
        ]);
    }

    #[test]
    fn thumb_loads_and_stores() {
        thumb_both(&[
            // Formats 7 and 8.
            (0x5088, "STR R0, [R1, R2]"),
            (0x5288, "STRH R0, [R1, R2]"),
            (0x5488, "STRB R0, [R1, R2]"),
            (0x5688, "LDRSB R0, [R1, R2]"),
            (0x5888, "LDR R0, [R1, R2]"),
            (0x5A88, "LDRH R0, [R1, R2]"),
            (0x5C88, "LDRB R0, [R1, R2]"),
            (0x5E88, "LDRSH R0, [R1, R2]"),
            // Format 9.
            (0x6808, "LDR R0, [R1]"),
            (0x6848, "LDR R0, [R1, #0x4]"),
            (0x77C8, "STRB R0, [R1, #0x1F]"),
            // Format 10.
            (0x8048, "STRH R0, [R1, #0x2]"),
            (0x8808, "LDRH R0, [R1]"),
            // Format 11.
            (0x9004, "STR R0, [SP, #0x10]"),
            (0x9800, "LDR R0, [SP]"),
            // Formats 14 and 15.
            (0xB510, "PUSH {R4, LR}"),
            (0xBD10, "POP {R4, PC}"),
            (0xB40D, "PUSH {R0, R2, R3}"),
            (0xBDF0, "POP {R4-R7, PC}"),
            (0xC10D, "STMIA R1!, {R0, R2, R3}"),
            (0xC90D, "LDMIA R1!, {R0, R2, R3}"),
        ]);
        // Format 6: the PC is the address plus 4, word-aligned.
        let v4 = Architecture::V4T;
        let text = "LDR R0, [PC, #0x10] ; 0x08000014";
        assert_eq!(thumb(0x0800_0002, 0x4804, 0, v4).text, text);
        assert_eq!(thumb(0x0800_0000, 0x4804, 0, v4).text, text);
    }

    #[test]
    fn thumb_branches() {
        let (v4, v5) = (Architecture::V4T, Architecture::V5TE);
        thumb_both(&[
            (0xD105, "BNE 0x0800000E"),
            (0xE010, "B 0x08000024"),
            (0xE7FE, "B 0x08000000"),
            (0xDF06, "SWI 0x6"),
        ]);
        // Backward.
        assert_eq!(thumb(0x0800_0010, 0xD0FE, 0, v4).text, "BEQ 0x08000010");
        assert_eq!(thumb(0x0800_0010, 0xDA80, 0, v4).text, "BGE 0x07FFFF14");
        // BLX Rm is a call on ARMv5; ARMv4T ignores H1 and runs BX.
        assert_eq!(thumb(ROM, 0x4788, 0, v5), call("BLX R1", 2));
        let bx = thumb(ROM, 0x4788, 0, v4);
        assert_eq!((bx.text.as_str(), bx.call), ("BX R1", false));
    }

    #[test]
    fn thumb_branch_with_link_pairs() {
        let (v4, v5) = (Architecture::V4T, Architecture::V5TE);
        for architecture in [v4, v5] {
            assert_eq!(
                thumb(ROM, 0xF000, 0xF810, architecture),
                call("BL 0x08000024", 4)
            );
            // Backward: -0x1000 + 0xFE0.
            assert_eq!(
                thumb(0x0800_2000, 0xF7FF, 0xFFF0, architecture),
                call("BL 0x08001FE4", 4)
            );
            // Lone halves.
            let prefix = thumb(ROM, 0xF000, 0x2000, architecture);
            assert_eq!(
                (prefix.text.as_str(), prefix.length),
                ("BL prefix 0x08000004", 2)
            );
            assert!(!prefix.call);
            assert_eq!(
                thumb(ROM, 0xF810, 0, architecture),
                call("BL suffix #0x20", 2)
            );
        }
        // BLX to ARM code (`tests/arm9.rs`), an ARMv5 suffix.
        assert_eq!(thumb(MAIN, 0xF000, 0xE87E, v5), call("BLX 0x02001100", 4));
        assert_eq!(thumb(MAIN, 0xE87E, 0, v5), call("BLX suffix #0xFC", 2));
        let lone = thumb(MAIN, 0xF000, 0xE87E, v4);
        assert_eq!(
            (lone.text.as_str(), lone.length, lone.call),
            ("BL prefix 0x02001004", 2, false)
        );
        assert_eq!(thumb(MAIN, 0xE87E, 0, v4).text, "UNDEFINED");
        // An odd BLX offset is undefined, so the prefix stands alone.
        assert_eq!(thumb(MAIN, 0xF000, 0xE87F, v5).length, 2);
        assert_eq!(thumb(MAIN, 0xE87F, 0, v5).text, "UNDEFINED");
    }

    #[test]
    fn thumb_undefined_and_breakpoints() {
        let (v4, v5) = (Architecture::V4T, Architecture::V5TE);
        assert_eq!(thumb(ROM, 0xBE12, 0, v5).text, "BKPT 0x12");
        assert_eq!(thumb(ROM, 0xBE12, 0, v4).text, "UNDEFINED");
        thumb_both(&[
            (0xB100, "UNDEFINED"),
            (0xB600, "UNDEFINED"),
            (0xBF00, "UNDEFINED"),
        ]);
    }

    #[test]
    fn every_encoding_decodes_to_some_text() {
        for architecture in [Architecture::V4T, Architecture::V5TE] {
            for opcode in 0..=u16::MAX {
                let disassembly = thumb(ROM, opcode, 0xF800, architecture);
                assert!(!disassembly.text.is_empty(), "{opcode:04X}");
                assert!(matches!(disassembly.length, 2 | 4), "{opcode:04X}");
            }
            // Every value of bits 27–20 and 7–4, the decoder's key, with
            // varied other bits and conditions.
            for key in 0..0x1000_u32 {
                let opcode = (key >> 4) << 20 | (key & 0xF) << 4 | (key % 16) << 28 | 0x000A_5A0F;
                let disassembly = arm(ROM, opcode, architecture);
                assert!(!disassembly.text.is_empty(), "{opcode:08X}");
                assert_eq!(disassembly.length, 4, "{opcode:08X}");
            }
        }
    }
}
