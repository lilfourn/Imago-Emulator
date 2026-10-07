//! Registers and status words as a debugger shows them.

use crate::{Mode, Model, Processor, psr};

/// Register names in a debugger's order: R0–R12, then the three with
/// roles.
const NAMES: [&str; 16] = [
    "R0", "R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8", "R9", "R10", "R11", "R12", "SP", "LR",
    "PC",
];

impl<M: Model> Processor<M> {
    /// R0–R15 as the current mode sees them (R15 reads two instructions
    /// ahead, as the hardware's does), then CPSR, then SPSR in the modes
    /// that have one, each with its name.
    #[must_use]
    pub fn named_registers(&self) -> Vec<(&'static str, u32)> {
        let mut registers: Vec<(&'static str, u32)> = NAMES.into_iter().zip(self.r).collect();
        registers.push(("CPSR", self.cpsr));
        if let Some(spsr) = self.saved_psr() {
            registers.push(("SPSR", spsr));
        }
        registers
    }

    /// The address of the instruction that runs next: R15 less the two
    /// instructions the pipeline has fetched ahead.
    #[must_use]
    pub fn next_instruction(&self) -> u32 {
        self.r[15].wrapping_sub(if self.thumb() { 4 } else { 8 })
    }
}

/// A program status register as text: the N, Z, C and V flags (`-` when
/// clear), the I, F and T bits, the state and the mode, as in
/// `-ZC- I-- ARM System`.
#[must_use]
pub fn describe_psr(value: u32) -> String {
    let letters = |bits: &[(u32, char)]| -> String {
        bits.iter()
            .map(|&(bit, letter)| if value & bit != 0 { letter } else { '-' })
            .collect()
    };
    let flags = letters(&[(psr::N, 'N'), (psr::Z, 'Z'), (psr::C, 'C'), (psr::V, 'V')]);
    let control = letters(&[(psr::I, 'I'), (psr::F, 'F'), (psr::T, 'T')]);
    let state = if value & psr::T != 0 { "Thumb" } else { "ARM" };
    let mode = match Mode::from_bits(value) {
        Mode::User => "User",
        Mode::Fiq => "FIQ",
        Mode::Irq => "IRQ",
        Mode::Supervisor => "Supervisor",
        Mode::Abort => "Abort",
        Mode::Undefined => "Undefined",
        Mode::System => "System",
    };
    format!("{flags} {control} {state} {mode}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Access, Cpu, Registers};

    #[test]
    fn status_words_read_as_flags_state_and_mode() {
        let value = psr::Z | psr::C | psr::I | psr::T | Mode::Irq.bits();
        assert_eq!(describe_psr(value), "-ZC- I-T Thumb IRQ");
        assert_eq!(describe_psr(Mode::System.bits()), "---- --- ARM System");
    }

    #[test]
    fn registers_come_named_with_the_saved_psr_where_there_is_one() {
        let mut registers = Registers {
            cpsr: Mode::Supervisor.bits() | psr::T,
            ..Registers::default()
        };
        registers.r[15] = 0x0800_0104;
        registers.spsr[1] = 0x1F;
        let cpu = Cpu::from_registers(&registers, [0, 0], Access::SEQUENTIAL);
        let named = cpu.named_registers();
        assert_eq!(named[15], ("PC", 0x0800_0104));
        assert_eq!(named[17], ("SPSR", 0x1F));
        assert_eq!(cpu.next_instruction(), 0x0800_0100);
    }
}
