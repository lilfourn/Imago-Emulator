//! The names of the GBA's I/O registers, for a debugger's I/O view.
//!
//! Source: GBATEK, "GBA I/O Map": one entry per named register in
//! 0x04000000–0x04000301, spelt and sized as GBATEK lists them. Left out:
//! wave RAM (memory rather than a register), the 32-bit SIODATA32 and 8-bit
//! SIODATA8 (other names for SIOMULTI0–1 and SIOMLT_SEND), the prototypes'
//! IR register and the unnamed undocumented registers.

/// `(address, name, width in bits)` for each register, sorted by address.
pub(crate) const IO_REGISTERS: &[(u32, &str, u8)] = &[
    // LCD.
    (0x0400_0000, "DISPCNT", 16),
    (0x0400_0002, "GREENSWP", 16),
    (0x0400_0004, "DISPSTAT", 16),
    (0x0400_0006, "VCOUNT", 16),
    (0x0400_0008, "BG0CNT", 16),
    (0x0400_000A, "BG1CNT", 16),
    (0x0400_000C, "BG2CNT", 16),
    (0x0400_000E, "BG3CNT", 16),
    (0x0400_0010, "BG0HOFS", 16),
    (0x0400_0012, "BG0VOFS", 16),
    (0x0400_0014, "BG1HOFS", 16),
    (0x0400_0016, "BG1VOFS", 16),
    (0x0400_0018, "BG2HOFS", 16),
    (0x0400_001A, "BG2VOFS", 16),
    (0x0400_001C, "BG3HOFS", 16),
    (0x0400_001E, "BG3VOFS", 16),
    (0x0400_0020, "BG2PA", 16),
    (0x0400_0022, "BG2PB", 16),
    (0x0400_0024, "BG2PC", 16),
    (0x0400_0026, "BG2PD", 16),
    (0x0400_0028, "BG2X", 32),
    (0x0400_002C, "BG2Y", 32),
    (0x0400_0030, "BG3PA", 16),
    (0x0400_0032, "BG3PB", 16),
    (0x0400_0034, "BG3PC", 16),
    (0x0400_0036, "BG3PD", 16),
    (0x0400_0038, "BG3X", 32),
    (0x0400_003C, "BG3Y", 32),
    (0x0400_0040, "WIN0H", 16),
    (0x0400_0042, "WIN1H", 16),
    (0x0400_0044, "WIN0V", 16),
    (0x0400_0046, "WIN1V", 16),
    (0x0400_0048, "WININ", 16),
    (0x0400_004A, "WINOUT", 16),
    (0x0400_004C, "MOSAIC", 16),
    (0x0400_0050, "BLDCNT", 16),
    (0x0400_0052, "BLDALPHA", 16),
    (0x0400_0054, "BLDY", 16),
    // Sound.
    (0x0400_0060, "SOUND1CNT_L", 16),
    (0x0400_0062, "SOUND1CNT_H", 16),
    (0x0400_0064, "SOUND1CNT_X", 16),
    (0x0400_0068, "SOUND2CNT_L", 16),
    (0x0400_006C, "SOUND2CNT_H", 16),
    (0x0400_0070, "SOUND3CNT_L", 16),
    (0x0400_0072, "SOUND3CNT_H", 16),
    (0x0400_0074, "SOUND3CNT_X", 16),
    (0x0400_0078, "SOUND4CNT_L", 16),
    (0x0400_007C, "SOUND4CNT_H", 16),
    (0x0400_0080, "SOUNDCNT_L", 16),
    (0x0400_0082, "SOUNDCNT_H", 16),
    (0x0400_0084, "SOUNDCNT_X", 16),
    (0x0400_0088, "SOUNDBIAS", 16),
    (0x0400_00A0, "FIFO_A", 32),
    (0x0400_00A4, "FIFO_B", 32),
    // DMA.
    (0x0400_00B0, "DMA0SAD", 32),
    (0x0400_00B4, "DMA0DAD", 32),
    (0x0400_00B8, "DMA0CNT_L", 16),
    (0x0400_00BA, "DMA0CNT_H", 16),
    (0x0400_00BC, "DMA1SAD", 32),
    (0x0400_00C0, "DMA1DAD", 32),
    (0x0400_00C4, "DMA1CNT_L", 16),
    (0x0400_00C6, "DMA1CNT_H", 16),
    (0x0400_00C8, "DMA2SAD", 32),
    (0x0400_00CC, "DMA2DAD", 32),
    (0x0400_00D0, "DMA2CNT_L", 16),
    (0x0400_00D2, "DMA2CNT_H", 16),
    (0x0400_00D4, "DMA3SAD", 32),
    (0x0400_00D8, "DMA3DAD", 32),
    (0x0400_00DC, "DMA3CNT_L", 16),
    (0x0400_00DE, "DMA3CNT_H", 16),
    // Timers.
    (0x0400_0100, "TM0CNT_L", 16),
    (0x0400_0102, "TM0CNT_H", 16),
    (0x0400_0104, "TM1CNT_L", 16),
    (0x0400_0106, "TM1CNT_H", 16),
    (0x0400_0108, "TM2CNT_L", 16),
    (0x0400_010A, "TM2CNT_H", 16),
    (0x0400_010C, "TM3CNT_L", 16),
    (0x0400_010E, "TM3CNT_H", 16),
    // Serial (1).
    (0x0400_0120, "SIOMULTI0", 16),
    (0x0400_0122, "SIOMULTI1", 16),
    (0x0400_0124, "SIOMULTI2", 16),
    (0x0400_0126, "SIOMULTI3", 16),
    (0x0400_0128, "SIOCNT", 16),
    (0x0400_012A, "SIOMLT_SEND", 16),
    // Keypad.
    (0x0400_0130, "KEYINPUT", 16),
    (0x0400_0132, "KEYCNT", 16),
    // Serial (2).
    (0x0400_0134, "RCNT", 16),
    (0x0400_0140, "JOYCNT", 16),
    (0x0400_0150, "JOY_RECV", 32),
    (0x0400_0154, "JOY_TRANS", 32),
    (0x0400_0158, "JOYSTAT", 16),
    // Interrupt, waitstate and power-down control.
    (0x0400_0200, "IE", 16),
    (0x0400_0202, "IF", 16),
    (0x0400_0204, "WAITCNT", 16),
    (0x0400_0208, "IME", 16),
    (0x0400_0300, "POSTFLG", 8),
    (0x0400_0301, "HALTCNT", 8),
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
