//! A disassembler for the SM83, for debuggers.
//!
//! It turns one instruction's bytes into RGBDS-style assembly: upper-case
//! mnemonics, `$` hexadecimal, `[ ]` around memory operands, and absolute
//! targets for relative jumps. Instruction lengths follow what `cpu.rs`
//! executes.
//!
//! Reference: Pan Docs, "CPU Instruction Set" and its opcode table. As in
//! `cpu.rs`, an opcode splits into bit fields: bits 7–6 pick one of four
//! blocks of 64 opcodes, and within a block `y` (bits 5–3), `z` (bits 2–0)
//! and `p` (bits 5–4) pick the operation or its operands.

/// An instruction decoded for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disassembly {
    /// Assembly text, e.g. `LD A, [HL]`.
    pub text: String,
    /// Bytes the instruction takes: 1 to 3 (CB-prefixed ones take 2).
    pub length: u8,
    /// Calls a subroutine that returns to the next instruction: CALL (all
    /// conditions) and RST.
    pub call: bool,
}

/// 8-bit operands, by `y` or `z`: B, C, D, E, H, L, the byte at HL, A.
const R8: [&str; 8] = ["B", "C", "D", "E", "H", "L", "[HL]", "A"];
/// 16-bit registers for loads and arithmetic, by `p`.
const R16: [&str; 4] = ["BC", "DE", "HL", "SP"];
/// 16-bit registers for PUSH and POP, by `p`.
const R16_STACK: [&str; 4] = ["BC", "DE", "HL", "AF"];
/// The memory operands of `LD [r16], A` and `LD A, [r16]`, by `p`.
const R16_MEMORY: [&str; 4] = ["[BC]", "[DE]", "[HL+]", "[HL-]"];
/// Jump conditions, by the low two bits of `y`.
const CONDITIONS: [&str; 4] = ["NZ", "Z", "NC", "C"];
/// Operations on A, by `y`: all of block 2, and `op A, n8` in block 3.
const ALU: [&str; 8] = ["ADD", "ADC", "SUB", "SBC", "AND", "XOR", "OR", "CP"];
/// The operations on A and the carry flag in column 7 of block 0, by `y`.
const ACCUMULATOR: [&str; 8] = ["RLCA", "RRCA", "RLA", "RRA", "DAA", "CPL", "SCF", "CCF"];
/// CB-prefixed rotates and shifts, by `y`.
const ROTATE_SHIFT: [&str; 8] = ["RLC", "RRC", "RL", "RR", "SLA", "SRA", "SWAP", "SRL"];

/// Disassembles the instruction whose bytes start with `bytes` (the opcode
/// and the two bytes after it; unused ones are ignored) at `address`.
#[must_use]
pub fn disassemble(address: u16, bytes: [u8; 3]) -> Disassembly {
    let (text, length) = decode(address, bytes);
    Disassembly {
        text,
        length,
        call: is_call(bytes[0]),
    }
}

/// Whether `opcode` is a CALL (any condition) or an RST.
fn is_call(opcode: u8) -> bool {
    matches!(
        opcode,
        0xC4 | 0xCC | 0xD4 | 0xDC | 0xCD | 0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF
    )
}

/// The text and length of the instruction at `address` that starts with
/// `opcode`, followed by `n8` and `high`. The match lists every opcode, so
/// the compiler checks none is missing.
fn decode(address: u16, [opcode, n8, high]: [u8; 3]) -> (String, u8) {
    let n16 = u16::from_le_bytes([n8, high]);
    let y = usize::from((opcode >> 3) & 7);
    let z = usize::from(opcode & 7);
    let p = usize::from((opcode >> 4) & 3);

    match opcode {
        // Block 0 (0x00–0x3F): 16-bit loads and arithmetic, INC and DEC,
        // immediate loads, operations on A, relative jumps.
        0x00 => ("NOP".to_owned(), 1),
        // STOP's second byte is fetched and ignored.
        0x10 => ("STOP".to_owned(), 2),
        0x08 => (format!("LD [${n16:04X}], SP"), 3),
        0x18 => (format!("JR ${:04X}", jump_target(address, n8)), 2),
        0x20 | 0x28 | 0x30 | 0x38 => {
            let target = jump_target(address, n8);
            (format!("JR {}, ${target:04X}", CONDITIONS[y - 4]), 2)
        }
        0x01 | 0x11 | 0x21 | 0x31 => (format!("LD {}, ${n16:04X}", R16[p]), 3),
        0x09 | 0x19 | 0x29 | 0x39 => (format!("ADD HL, {}", R16[p]), 1),
        0x02 | 0x12 | 0x22 | 0x32 => (format!("LD {}, A", R16_MEMORY[p]), 1),
        0x0A | 0x1A | 0x2A | 0x3A => (format!("LD A, {}", R16_MEMORY[p]), 1),
        0x03 | 0x13 | 0x23 | 0x33 => (format!("INC {}", R16[p]), 1),
        0x0B | 0x1B | 0x2B | 0x3B => (format!("DEC {}", R16[p]), 1),
        0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => (format!("INC {}", R8[y]), 1),
        0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => (format!("DEC {}", R8[y]), 1),
        0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
            (format!("LD {}, ${n8:02X}", R8[y]), 2)
        }
        0x07 | 0x0F | 0x17 | 0x1F | 0x27 | 0x2F | 0x37 | 0x3F => (ACCUMULATOR[y].to_owned(), 1),

        // Block 1 (0x40–0x7F): LD r8, r8, with HALT where LD [HL], [HL]
        // would be.
        0x76 => ("HALT".to_owned(), 1),
        0x40..=0x7F => (format!("LD {}, {}", R8[y], R8[z]), 1),

        // Block 2 (0x80–0xBF): operations on A and an 8-bit operand.
        0x80..=0xBF => (format!("{} A, {}", ALU[y], R8[z]), 1),

        // Block 3 (0xC0–0xFF): jumps, calls and returns, the stack,
        // immediate operands, high RAM and the CB prefix.
        0xC0 | 0xC8 | 0xD0 | 0xD8 => (format!("RET {}", CONDITIONS[y]), 1),
        0xC9 => ("RET".to_owned(), 1),
        0xD9 => ("RETI".to_owned(), 1),
        0xC2 | 0xCA | 0xD2 | 0xDA => (format!("JP {}, ${n16:04X}", CONDITIONS[y]), 3),
        0xC3 => (format!("JP ${n16:04X}"), 3),
        0xE9 => ("JP HL".to_owned(), 1),
        0xC4 | 0xCC | 0xD4 | 0xDC => (format!("CALL {}, ${n16:04X}", CONDITIONS[y]), 3),
        0xCD => (format!("CALL ${n16:04X}"), 3),
        0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => (format!("RST ${:02X}", y * 8), 1),
        0xC1 | 0xD1 | 0xE1 | 0xF1 => (format!("POP {}", R16_STACK[p]), 1),
        0xC5 | 0xD5 | 0xE5 | 0xF5 => (format!("PUSH {}", R16_STACK[p]), 1),
        0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
            (format!("{} A, ${n8:02X}", ALU[y]), 2)
        }
        0xCB => (prefixed(n8), 2),
        0xE0 => (format!("LDH [$FF{n8:02X}], A"), 2),
        0xF0 => (format!("LDH A, [$FF{n8:02X}]"), 2),
        0xE2 => ("LD [$FF00+C], A".to_owned(), 1),
        0xF2 => ("LD A, [$FF00+C]".to_owned(), 1),
        0xEA => (format!("LD [${n16:04X}], A"), 3),
        0xFA => (format!("LD A, [${n16:04X}]"), 3),
        0xE8 => (format!("ADD SP, {}", signed(n8)), 2),
        0xF8 => {
            let plus = if n8.cast_signed() < 0 { "" } else { "+" };
            (format!("LD HL, SP{plus}{}", signed(n8)), 2)
        }
        0xF9 => ("LD SP, HL".to_owned(), 1),
        0xF3 => ("DI".to_owned(), 1),
        0xFB => ("EI".to_owned(), 1),
        // Opcodes the SM83 lacks: running one freezes the CPU. Shown as a
        // data byte, as assemblers would write it.
        0xD3 | 0xDB | 0xDD | 0xE3 | 0xE4 | 0xEB | 0xEC | 0xED | 0xF4 | 0xFC | 0xFD => {
            (format!("DB ${opcode:02X}"), 1)
        }
    }
}

/// The CB-prefixed instruction whose second byte is `opcode`: by bits 7–6,
/// a rotate or shift (picked by `y`), BIT, RES or SET (of bit `y`), on
/// operand `z`.
fn prefixed(opcode: u8) -> String {
    let y = (opcode >> 3) & 7;
    let operand = R8[usize::from(opcode & 7)];
    match opcode >> 6 {
        0 => format!("{} {operand}", ROTATE_SHIFT[usize::from(y)]),
        1 => format!("BIT {y}, {operand}"),
        2 => format!("RES {y}, {operand}"),
        _ => format!("SET {y}, {operand}"),
    }
}

/// Where a JR at `address` with signed `offset` jumps: the offset counts
/// from the end of its two bytes.
fn jump_target(address: u16, offset: u8) -> u16 {
    address
        .wrapping_add(2)
        .wrapping_add_signed(i16::from(offset.cast_signed()))
}

/// A signed byte as RGBDS writes it: `$05`, or `-$02` for 0xFE.
fn signed(byte: u8) -> String {
    let value = byte.cast_signed();
    let minus = if value < 0 { "-" } else { "" };
    format!("{minus}${:02X}", value.unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{Bus, Cpu, Registers};

    /// Disassembles each `(bytes, text)` case at 0x0100 and checks the
    /// text, and that the length is the number of bytes given. Bytes past
    /// the instruction are 0xFF, which it must ignore.
    #[track_caller]
    fn check(cases: &[(&[u8], &str)]) {
        for &(bytes, text) in cases {
            let mut padded = [0xFF; 3];
            padded[..bytes.len()].copy_from_slice(bytes);
            let instruction = disassemble(0x0100, padded);
            assert_eq!(
                (instruction.text.as_str(), usize::from(instruction.length)),
                (text, bytes.len()),
                "bytes {bytes:02X?}",
            );
        }
    }

    #[test]
    fn block_0_loads_arithmetic_and_relative_jumps() {
        check(&[
            (&[0x00], "NOP"),
            (&[0x01, 0x34, 0x12], "LD BC, $1234"),
            (&[0x11, 0x34, 0x12], "LD DE, $1234"),
            (&[0x21, 0x00, 0xC0], "LD HL, $C000"),
            (&[0x31, 0xFE, 0xFF], "LD SP, $FFFE"),
            (&[0x02], "LD [BC], A"),
            (&[0x12], "LD [DE], A"),
            (&[0x22], "LD [HL+], A"),
            (&[0x32], "LD [HL-], A"),
            (&[0x0A], "LD A, [BC]"),
            (&[0x1A], "LD A, [DE]"),
            (&[0x2A], "LD A, [HL+]"),
            (&[0x3A], "LD A, [HL-]"),
            (&[0x03], "INC BC"),
            (&[0x13], "INC DE"),
            (&[0x23], "INC HL"),
            (&[0x33], "INC SP"),
            (&[0x0B], "DEC BC"),
            (&[0x3B], "DEC SP"),
            (&[0x09], "ADD HL, BC"),
            (&[0x19], "ADD HL, DE"),
            (&[0x29], "ADD HL, HL"),
            (&[0x39], "ADD HL, SP"),
            (&[0x04], "INC B"),
            (&[0x0C], "INC C"),
            (&[0x34], "INC [HL]"),
            (&[0x3C], "INC A"),
            (&[0x05], "DEC B"),
            (&[0x35], "DEC [HL]"),
            (&[0x3D], "DEC A"),
            (&[0x06, 0x00], "LD B, $00"),
            (&[0x2E, 0x7F], "LD L, $7F"),
            (&[0x36, 0x12], "LD [HL], $12"),
            (&[0x3E, 0x42], "LD A, $42"),
            (&[0x07], "RLCA"),
            (&[0x0F], "RRCA"),
            (&[0x17], "RLA"),
            (&[0x1F], "RRA"),
            (&[0x27], "DAA"),
            (&[0x2F], "CPL"),
            (&[0x37], "SCF"),
            (&[0x3F], "CCF"),
            (&[0x08, 0x00, 0xC0], "LD [$C000], SP"),
            (&[0x10, 0x00], "STOP"),
            (&[0x18, 0x03], "JR $0105"),
            (&[0x18, 0xFE], "JR $0100"),
            (&[0x20, 0x03], "JR NZ, $0105"),
            (&[0x28, 0x7F], "JR Z, $0181"),
            (&[0x30, 0x80], "JR NC, $0082"),
            (&[0x38, 0xFD], "JR C, $00FF"),
        ]);
    }

    #[test]
    fn block_1_loads_between_registers_and_halt() {
        check(&[
            (&[0x40], "LD B, B"),
            (&[0x41], "LD B, C"),
            (&[0x65], "LD H, L"),
            (&[0x6E], "LD L, [HL]"),
            (&[0x70], "LD [HL], B"),
            (&[0x75], "LD [HL], L"),
            (&[0x76], "HALT"),
            (&[0x77], "LD [HL], A"),
            (&[0x7E], "LD A, [HL]"),
            (&[0x7F], "LD A, A"),
        ]);
    }

    #[test]
    fn block_2_operates_on_a_and_a_register() {
        check(&[
            (&[0x80], "ADD A, B"),
            (&[0x89], "ADC A, C"),
            (&[0x92], "SUB A, D"),
            (&[0x9B], "SBC A, E"),
            (&[0xA4], "AND A, H"),
            (&[0xAD], "XOR A, L"),
            (&[0xB6], "OR A, [HL]"),
            (&[0xBF], "CP A, A"),
        ]);
    }

    #[test]
    fn block_3_jumps_stack_immediates_and_high_ram() {
        check(&[
            (&[0xC0], "RET NZ"),
            (&[0xC8], "RET Z"),
            (&[0xD0], "RET NC"),
            (&[0xD8], "RET C"),
            (&[0xC9], "RET"),
            (&[0xD9], "RETI"),
            (&[0xC2, 0x34, 0x12], "JP NZ, $1234"),
            (&[0xCA, 0x34, 0x12], "JP Z, $1234"),
            (&[0xD2, 0x34, 0x12], "JP NC, $1234"),
            (&[0xDA, 0x34, 0x12], "JP C, $1234"),
            (&[0xC3, 0x50, 0x01], "JP $0150"),
            (&[0xE9], "JP HL"),
            (&[0xC4, 0x34, 0x12], "CALL NZ, $1234"),
            (&[0xCC, 0x34, 0x12], "CALL Z, $1234"),
            (&[0xD4, 0x34, 0x12], "CALL NC, $1234"),
            (&[0xDC, 0x34, 0x12], "CALL C, $1234"),
            (&[0xCD, 0x00, 0x02], "CALL $0200"),
            (&[0xC7], "RST $00"),
            (&[0xCF], "RST $08"),
            (&[0xD7], "RST $10"),
            (&[0xDF], "RST $18"),
            (&[0xE7], "RST $20"),
            (&[0xEF], "RST $28"),
            (&[0xF7], "RST $30"),
            (&[0xFF], "RST $38"),
            (&[0xC1], "POP BC"),
            (&[0xD1], "POP DE"),
            (&[0xE1], "POP HL"),
            (&[0xF1], "POP AF"),
            (&[0xC5], "PUSH BC"),
            (&[0xD5], "PUSH DE"),
            (&[0xE5], "PUSH HL"),
            (&[0xF5], "PUSH AF"),
            (&[0xC6, 0x12], "ADD A, $12"),
            (&[0xCE, 0x12], "ADC A, $12"),
            (&[0xD6, 0x12], "SUB A, $12"),
            (&[0xDE, 0x12], "SBC A, $12"),
            (&[0xE6, 0x0F], "AND A, $0F"),
            (&[0xEE, 0xFF], "XOR A, $FF"),
            (&[0xF6, 0x80], "OR A, $80"),
            (&[0xFE, 0x90], "CP A, $90"),
            (&[0xE0, 0x44], "LDH [$FF44], A"),
            (&[0xF0, 0x00], "LDH A, [$FF00]"),
            (&[0xE2], "LD [$FF00+C], A"),
            (&[0xF2], "LD A, [$FF00+C]"),
            (&[0xEA, 0x00, 0xC0], "LD [$C000], A"),
            (&[0xFA, 0x50, 0x01], "LD A, [$0150]"),
            (&[0xE8, 0x05], "ADD SP, $05"),
            (&[0xE8, 0xFE], "ADD SP, -$02"),
            (&[0xE8, 0x80], "ADD SP, -$80"),
            (&[0xF8, 0x05], "LD HL, SP+$05"),
            (&[0xF8, 0x00], "LD HL, SP+$00"),
            (&[0xF8, 0xFE], "LD HL, SP-$02"),
            (&[0xF9], "LD SP, HL"),
            (&[0xF3], "DI"),
            (&[0xFB], "EI"),
        ]);
    }

    #[test]
    fn illegal_opcodes_are_one_data_byte() {
        check(&[
            (&[0xD3], "DB $D3"),
            (&[0xDB], "DB $DB"),
            (&[0xDD], "DB $DD"),
            (&[0xE3], "DB $E3"),
            (&[0xE4], "DB $E4"),
            (&[0xEB], "DB $EB"),
            (&[0xEC], "DB $EC"),
            (&[0xED], "DB $ED"),
            (&[0xF4], "DB $F4"),
            (&[0xFC], "DB $FC"),
            (&[0xFD], "DB $FD"),
        ]);
    }

    #[test]
    fn cb_prefix_rotates_shifts_and_bits() {
        check(&[
            (&[0xCB, 0x00], "RLC B"),
            (&[0xCB, 0x09], "RRC C"),
            (&[0xCB, 0x12], "RL D"),
            (&[0xCB, 0x1B], "RR E"),
            (&[0xCB, 0x24], "SLA H"),
            (&[0xCB, 0x2D], "SRA L"),
            (&[0xCB, 0x37], "SWAP A"),
            (&[0xCB, 0x3E], "SRL [HL]"),
            (&[0xCB, 0x40], "BIT 0, B"),
            (&[0xCB, 0x7E], "BIT 7, [HL]"),
            (&[0xCB, 0x87], "RES 0, A"),
            (&[0xCB, 0xAE], "RES 5, [HL]"),
            (&[0xCB, 0xDC], "SET 3, H"),
            (&[0xCB, 0xFE], "SET 7, [HL]"),
        ]);
    }

    #[test]
    fn relative_jumps_wrap_around_the_address_space() {
        assert_eq!(disassemble(0x0000, [0x18, 0xFD, 0]).text, "JR $FFFF");
        assert_eq!(disassemble(0xFFFE, [0x20, 0x7F, 0]).text, "JR NZ, $007F");
    }

    /// The serial test program in `link.rs`, walked instruction by
    /// instruction with each one's length.
    #[test]
    fn disassembles_a_program_in_sequence() {
        let program = [
            0x06, 0x00, 0x05, 0x20, 0xFD, 0x3E, 0x42, 0xE0, 0x01, 0x3E, 0x81, 0xE0, 0x02, 0xF0,
            0x02, 0xCB, 0x7F, 0x20, 0xFA, 0xF0, 0x01, 0xEA, 0x00, 0xC0, 0x18, 0xFE,
        ];
        let expected = [
            (0x0100, "LD B, $00"),
            (0x0102, "DEC B"),
            (0x0103, "JR NZ, $0102"),
            (0x0105, "LD A, $42"),
            (0x0107, "LDH [$FF01], A"),
            (0x0109, "LD A, $81"),
            (0x010B, "LDH [$FF02], A"),
            (0x010D, "LDH A, [$FF02]"),
            (0x010F, "BIT 7, A"),
            (0x0111, "JR NZ, $010D"),
            (0x0113, "LDH A, [$FF01]"),
            (0x0115, "LD [$C000], A"),
            (0x0118, "JR $0118"),
        ];
        let mut listing = Vec::new();
        let mut offset = 0;
        while offset < program.len() {
            let mut bytes = [0; 3];
            for (byte, &value) in bytes.iter_mut().zip(&program[offset..]) {
                *byte = value;
            }
            let address = 0x0100 + offset as u16;
            let instruction = disassemble(address, bytes);
            listing.push((address, instruction.text));
            offset += usize::from(instruction.length);
        }
        let expected: Vec<_> = expected
            .iter()
            .map(|&(address, text)| (address, text.to_owned()))
            .collect();
        assert_eq!(listing, expected);
    }

    #[test]
    fn only_call_and_rst_are_calls() {
        let calls: Vec<u8> = (0..=0xFF)
            .filter(|&opcode| disassemble(0, [opcode, 0, 0]).call)
            .collect();
        assert_eq!(
            calls,
            [
                0xC4, 0xC7, 0xCC, 0xCD, 0xCF, 0xD4, 0xD7, 0xDC, 0xDF, 0xE7, 0xEF, 0xF7, 0xFF
            ]
        );
        for opcode in 0..=0xFF {
            let instruction = disassemble(0, [opcode, 0, 0]);
            let text = &instruction.text;
            assert_eq!(
                instruction.call,
                text.starts_with("CALL") || text.starts_with("RST"),
                "{text}"
            );
        }
    }

    /// Flat RAM without interrupts, enough to run one instruction.
    struct FlatBus(Vec<u8>);

    impl Bus for FlatBus {
        fn read(&mut self, address: u16) -> u8 {
            self.0[usize::from(address)]
        }

        fn write(&mut self, address: u16, value: u8) {
            self.0[usize::from(address)] = value;
        }

        fn tick(&mut self) {}

        fn pending_interrupts(&self) -> u8 {
            0
        }

        fn acknowledge_interrupt(&mut self, _mask: u8) {}
    }

    /// PC after the CPU runs the instruction `bytes` at 0x0100 with flags
    /// `f`, HL = 0x0101 and 0x0101 on top of the stack.
    fn pc_after(bytes: [u8; 3], f: u8) -> u16 {
        let mut memory = vec![0; 0x1_0000];
        memory[0x0100..0x0103].copy_from_slice(&bytes);
        memory[0xD000..0xD002].copy_from_slice(&[0x01, 0x01]);
        let mut cpu = Cpu::new(Registers {
            f,
            h: 0x01,
            l: 0x01,
            sp: 0xD000,
            pc: 0x0100,
            ..Registers::default()
        });
        cpu.step(&mut FlatBus(memory));
        cpu.regs.pc
    }

    /// The CPU is the ground truth for lengths. Each opcode's operands make
    /// its jump, if any, land where it ends anyway: a JR offset of 0, a JP
    /// or CALL target of 0x0103, and HL and the return address on the
    /// stack 0x0101 for the one-byte JP HL, RET and RETI. Conditional ones
    /// run with the flags all clear and all set, taking both paths. RST
    /// always jumps to its vector, so it is left out.
    #[test]
    fn lengths_match_how_far_the_cpu_moves_pc() {
        for opcode in (0..=0xFF).filter(|opcode| opcode & 0xC7 != 0xC7) {
            let length = disassemble(0x0100, [opcode, 0, 0]).length;
            let bytes = if length == 3 {
                [opcode, 0x03, 0x01]
            } else {
                [opcode, 0x00, 0x00]
            };
            for flags in [0x00, 0xF0] {
                assert_eq!(
                    pc_after(bytes, flags),
                    0x0100 + u16::from(length),
                    "opcode {opcode:02X}, flags {flags:02X}"
                );
            }
        }
        for opcode in 0..=0xFF {
            assert_eq!(disassemble(0x0100, [0xCB, opcode, 0]).length, 2);
            assert_eq!(pc_after([0xCB, opcode, 0], 0), 0x0102, "CB {opcode:02X}");
        }
    }
}
