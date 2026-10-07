//! The names of the Game Boy and Game Boy Color I/O registers, for a
//! debugger's I/O view.
//!
//! Source: Pan Docs, "Hardware Registers" (the "I/O Ranges" summary table):
//! one entry per named register in 0xFF00–0xFF7F, plus IE at 0xFFFF, spelt
//! as Pan Docs spells them. Wave RAM (0xFF30–0xFF3F) is memory rather than a
//! register, and the unnamed undocumented registers are left out.

/// `(address, name, width in bits)` for each register, sorted by address.
pub(crate) const IO_REGISTERS: &[(u32, &str, u8)] = &[
    // Joypad, serial and timer.
    (0xFF00, "P1/JOYP", 8),
    (0xFF01, "SB", 8),
    (0xFF02, "SC", 8),
    (0xFF04, "DIV", 8),
    (0xFF05, "TIMA", 8),
    (0xFF06, "TMA", 8),
    (0xFF07, "TAC", 8),
    (0xFF0F, "IF", 8),
    // Sound.
    (0xFF10, "NR10", 8),
    (0xFF11, "NR11", 8),
    (0xFF12, "NR12", 8),
    (0xFF13, "NR13", 8),
    (0xFF14, "NR14", 8),
    (0xFF16, "NR21", 8),
    (0xFF17, "NR22", 8),
    (0xFF18, "NR23", 8),
    (0xFF19, "NR24", 8),
    (0xFF1A, "NR30", 8),
    (0xFF1B, "NR31", 8),
    (0xFF1C, "NR32", 8),
    (0xFF1D, "NR33", 8),
    (0xFF1E, "NR34", 8),
    (0xFF20, "NR41", 8),
    (0xFF21, "NR42", 8),
    (0xFF22, "NR43", 8),
    (0xFF23, "NR44", 8),
    (0xFF24, "NR50", 8),
    (0xFF25, "NR51", 8),
    (0xFF26, "NR52", 8),
    // LCD.
    (0xFF40, "LCDC", 8),
    (0xFF41, "STAT", 8),
    (0xFF42, "SCY", 8),
    (0xFF43, "SCX", 8),
    (0xFF44, "LY", 8),
    (0xFF45, "LYC", 8),
    (0xFF46, "DMA", 8),
    (0xFF47, "BGP", 8),
    (0xFF48, "OBP0", 8),
    (0xFF49, "OBP1", 8),
    (0xFF4A, "WY", 8),
    (0xFF4B, "WX", 8),
    // Game Boy Color only.
    (0xFF4D, "KEY1", 8),
    (0xFF4F, "VBK", 8),
    (0xFF51, "HDMA1", 8),
    (0xFF52, "HDMA2", 8),
    (0xFF53, "HDMA3", 8),
    (0xFF54, "HDMA4", 8),
    (0xFF55, "HDMA5", 8),
    (0xFF56, "RP", 8),
    (0xFF68, "BCPS/BGPI", 8),
    (0xFF69, "BCPD/BGPD", 8),
    (0xFF6A, "OCPS/OBPI", 8),
    (0xFF6B, "OCPD/OBPD", 8),
    (0xFF6C, "OPRI", 8),
    (0xFF70, "SVBK", 8),
    (0xFF76, "PCM12", 8),
    (0xFF77, "PCM34", 8),
    // Interrupts.
    (0xFFFF, "IE", 8),
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::IO_REGISTERS;

    #[test]
    fn registers_are_sorted_aligned_and_apart() {
        for &(address, name, bits) in IO_REGISTERS {
            assert!(matches!(bits, 8 | 16 | 32), "{name} is {bits} bits");
            assert_eq!(address % u32::from(bits / 8), 0, "{name} is misaligned");
        }
        for pair in IO_REGISTERS.windows(2) {
            let [(address, name, bits), (next, next_name, _)] = [pair[0], pair[1]];
            assert!(
                address + u32::from(bits / 8) <= next,
                "{name} runs into {next_name}"
            );
        }
        let names: HashSet<_> = IO_REGISTERS.iter().map(|&(_, name, _)| name).collect();
        assert_eq!(names.len(), IO_REGISTERS.len(), "a name is repeated");
    }
}
