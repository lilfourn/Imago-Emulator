//! The names of the DS's I/O registers, for a debugger's I/O view: one
//! table for what the ARM9 sees, one for the ARM7.
//!
//! Source: GBATEK, "DS I/O Map" ("ARM9 I/O Map" and "ARM7 I/O Map"): one
//! entry per named register, spelt and sized as GBATEK lists them, except
//! that 64-bit registers are split into 32-bit `_L` and `_H` halves and
//! display engine B's registers, which GBATEK lists under the same names as
//! engine A's, get a " (B)" suffix. Left out: the 3D geometry command ports
//! (0x04000400–0x040005FF on the ARM9), the fog and toon tables, the 3D
//! result matrices, the DS-mode debug serial port and the unnamed gamecard
//! command and data registers.

/// `(address, name, width in bits)` for each ARM9 register, sorted by
/// address.
pub(crate) const ARM9_IO_REGISTERS: &[(u32, &str, u8)] = &[
    // Display engine A.
    (0x0400_0000, "DISPCNT", 32),
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
    (0x0400_0060, "DISP3DCNT", 16),
    (0x0400_0064, "DISPCAPCNT", 32),
    (0x0400_0068, "DISP_MMEM_FIFO", 32),
    (0x0400_006C, "MASTER_BRIGHT", 16),
    // DMA.
    (0x0400_00B0, "DMA0SAD", 32),
    (0x0400_00B4, "DMA0DAD", 32),
    (0x0400_00B8, "DMA0CNT", 32),
    (0x0400_00BC, "DMA1SAD", 32),
    (0x0400_00C0, "DMA1DAD", 32),
    (0x0400_00C4, "DMA1CNT", 32),
    (0x0400_00C8, "DMA2SAD", 32),
    (0x0400_00CC, "DMA2DAD", 32),
    (0x0400_00D0, "DMA2CNT", 32),
    (0x0400_00D4, "DMA3SAD", 32),
    (0x0400_00D8, "DMA3DAD", 32),
    (0x0400_00DC, "DMA3CNT", 32),
    (0x0400_00E0, "DMA0FILL", 32),
    (0x0400_00E4, "DMA1FILL", 32),
    (0x0400_00E8, "DMA2FILL", 32),
    (0x0400_00EC, "DMA3FILL", 32),
    // Timers.
    (0x0400_0100, "TM0CNT_L", 16),
    (0x0400_0102, "TM0CNT_H", 16),
    (0x0400_0104, "TM1CNT_L", 16),
    (0x0400_0106, "TM1CNT_H", 16),
    (0x0400_0108, "TM2CNT_L", 16),
    (0x0400_010A, "TM2CNT_H", 16),
    (0x0400_010C, "TM3CNT_L", 16),
    (0x0400_010E, "TM3CNT_H", 16),
    // Keypad.
    (0x0400_0130, "KEYINPUT", 16),
    (0x0400_0132, "KEYCNT", 16),
    // IPC.
    (0x0400_0180, "IPCSYNC", 16),
    (0x0400_0184, "IPCFIFOCNT", 16),
    (0x0400_0188, "IPCFIFOSEND", 32),
    // Gamecard.
    (0x0400_01A0, "AUXSPICNT", 16),
    (0x0400_01A2, "AUXSPIDATA", 16),
    (0x0400_01A4, "ROMCTRL", 32),
    // Memory and interrupt control.
    (0x0400_0204, "EXMEMCNT", 16),
    (0x0400_0208, "IME", 16),
    (0x0400_0210, "IE", 32),
    (0x0400_0214, "IF", 32),
    (0x0400_0240, "VRAMCNT_A", 8),
    (0x0400_0241, "VRAMCNT_B", 8),
    (0x0400_0242, "VRAMCNT_C", 8),
    (0x0400_0243, "VRAMCNT_D", 8),
    (0x0400_0244, "VRAMCNT_E", 8),
    (0x0400_0245, "VRAMCNT_F", 8),
    (0x0400_0246, "VRAMCNT_G", 8),
    (0x0400_0247, "WRAMCNT", 8),
    (0x0400_0248, "VRAMCNT_H", 8),
    (0x0400_0249, "VRAMCNT_I", 8),
    // Maths.
    (0x0400_0280, "DIVCNT", 16),
    (0x0400_0290, "DIV_NUMER_L", 32),
    (0x0400_0294, "DIV_NUMER_H", 32),
    (0x0400_0298, "DIV_DENOM_L", 32),
    (0x0400_029C, "DIV_DENOM_H", 32),
    (0x0400_02A0, "DIV_RESULT_L", 32),
    (0x0400_02A4, "DIV_RESULT_H", 32),
    (0x0400_02A8, "DIVREM_RESULT_L", 32),
    (0x0400_02AC, "DIVREM_RESULT_H", 32),
    (0x0400_02B0, "SQRTCNT", 16),
    (0x0400_02B4, "SQRT_RESULT", 32),
    (0x0400_02B8, "SQRT_PARAM_L", 32),
    (0x0400_02BC, "SQRT_PARAM_H", 32),
    // Power.
    (0x0400_0300, "POSTFLG", 8),
    (0x0400_0304, "POWCNT1", 16),
    // 3D rendering.
    (0x0400_0320, "RDLINES_COUNT", 8),
    (0x0400_0330, "EDGE_COLOR_0", 16),
    (0x0400_0332, "EDGE_COLOR_1", 16),
    (0x0400_0334, "EDGE_COLOR_2", 16),
    (0x0400_0336, "EDGE_COLOR_3", 16),
    (0x0400_0338, "EDGE_COLOR_4", 16),
    (0x0400_033A, "EDGE_COLOR_5", 16),
    (0x0400_033C, "EDGE_COLOR_6", 16),
    (0x0400_033E, "EDGE_COLOR_7", 16),
    (0x0400_0340, "ALPHA_TEST_REF", 8),
    (0x0400_0350, "CLEAR_COLOR", 32),
    (0x0400_0354, "CLEAR_DEPTH", 16),
    (0x0400_0356, "CLRIMAGE_OFFSET", 16),
    (0x0400_0358, "FOG_COLOR", 32),
    (0x0400_035C, "FOG_OFFSET", 16),
    // 3D geometry status.
    (0x0400_0600, "GXSTAT", 32),
    (0x0400_0604, "RAM_COUNT", 32),
    (0x0400_0610, "DISP_1DOT_DEPTH", 16),
    // Display engine B.
    (0x0400_1000, "DISPCNT (B)", 32),
    (0x0400_1008, "BG0CNT (B)", 16),
    (0x0400_100A, "BG1CNT (B)", 16),
    (0x0400_100C, "BG2CNT (B)", 16),
    (0x0400_100E, "BG3CNT (B)", 16),
    (0x0400_1010, "BG0HOFS (B)", 16),
    (0x0400_1012, "BG0VOFS (B)", 16),
    (0x0400_1014, "BG1HOFS (B)", 16),
    (0x0400_1016, "BG1VOFS (B)", 16),
    (0x0400_1018, "BG2HOFS (B)", 16),
    (0x0400_101A, "BG2VOFS (B)", 16),
    (0x0400_101C, "BG3HOFS (B)", 16),
    (0x0400_101E, "BG3VOFS (B)", 16),
    (0x0400_1020, "BG2PA (B)", 16),
    (0x0400_1022, "BG2PB (B)", 16),
    (0x0400_1024, "BG2PC (B)", 16),
    (0x0400_1026, "BG2PD (B)", 16),
    (0x0400_1028, "BG2X (B)", 32),
    (0x0400_102C, "BG2Y (B)", 32),
    (0x0400_1030, "BG3PA (B)", 16),
    (0x0400_1032, "BG3PB (B)", 16),
    (0x0400_1034, "BG3PC (B)", 16),
    (0x0400_1036, "BG3PD (B)", 16),
    (0x0400_1038, "BG3X (B)", 32),
    (0x0400_103C, "BG3Y (B)", 32),
    (0x0400_1040, "WIN0H (B)", 16),
    (0x0400_1042, "WIN1H (B)", 16),
    (0x0400_1044, "WIN0V (B)", 16),
    (0x0400_1046, "WIN1V (B)", 16),
    (0x0400_1048, "WININ (B)", 16),
    (0x0400_104A, "WINOUT (B)", 16),
    (0x0400_104C, "MOSAIC (B)", 16),
    (0x0400_1050, "BLDCNT (B)", 16),
    (0x0400_1052, "BLDALPHA (B)", 16),
    (0x0400_1054, "BLDY (B)", 16),
    (0x0400_106C, "MASTER_BRIGHT (B)", 16),
    // IPC, again.
    (0x0410_0000, "IPCFIFORECV", 32),
];

/// `(address, name, width in bits)` for each ARM7 register, sorted by
/// address.
pub(crate) const ARM7_IO_REGISTERS: &[(u32, &str, u8)] = &[
    // Display.
    (0x0400_0004, "DISPSTAT", 16),
    (0x0400_0006, "VCOUNT", 16),
    // DMA.
    (0x0400_00B0, "DMA0SAD", 32),
    (0x0400_00B4, "DMA0DAD", 32),
    (0x0400_00B8, "DMA0CNT", 32),
    (0x0400_00BC, "DMA1SAD", 32),
    (0x0400_00C0, "DMA1DAD", 32),
    (0x0400_00C4, "DMA1CNT", 32),
    (0x0400_00C8, "DMA2SAD", 32),
    (0x0400_00CC, "DMA2DAD", 32),
    (0x0400_00D0, "DMA2CNT", 32),
    (0x0400_00D4, "DMA3SAD", 32),
    (0x0400_00D8, "DMA3DAD", 32),
    (0x0400_00DC, "DMA3CNT", 32),
    // Timers.
    (0x0400_0100, "TM0CNT_L", 16),
    (0x0400_0102, "TM0CNT_H", 16),
    (0x0400_0104, "TM1CNT_L", 16),
    (0x0400_0106, "TM1CNT_H", 16),
    (0x0400_0108, "TM2CNT_L", 16),
    (0x0400_010A, "TM2CNT_H", 16),
    (0x0400_010C, "TM3CNT_L", 16),
    (0x0400_010E, "TM3CNT_H", 16),
    // Keypad and real-time clock.
    (0x0400_0130, "KEYINPUT", 16),
    (0x0400_0132, "KEYCNT", 16),
    (0x0400_0136, "EXTKEYIN", 16),
    (0x0400_0138, "RTC", 8),
    // IPC.
    (0x0400_0180, "IPCSYNC", 16),
    (0x0400_0184, "IPCFIFOCNT", 16),
    (0x0400_0188, "IPCFIFOSEND", 32),
    // Gamecard.
    (0x0400_01A0, "AUXSPICNT", 16),
    (0x0400_01A2, "AUXSPIDATA", 16),
    (0x0400_01A4, "ROMCTRL", 32),
    // SPI bus: power management, touchscreen, firmware.
    (0x0400_01C0, "SPICNT", 16),
    (0x0400_01C2, "SPIDATA", 16),
    // Memory and interrupt control.
    (0x0400_0204, "EXMEMSTAT", 16),
    (0x0400_0206, "WIFIWAITCNT", 16),
    (0x0400_0208, "IME", 16),
    (0x0400_0210, "IE", 32),
    (0x0400_0214, "IF", 32),
    (0x0400_0240, "VRAMSTAT", 8),
    (0x0400_0241, "WRAMSTAT", 8),
    // Power.
    (0x0400_0300, "POSTFLG", 8),
    (0x0400_0301, "HALTCNT", 8),
    (0x0400_0304, "POWCNT2", 16),
    (0x0400_0308, "BIOSPROT", 32),
    // Sound channels.
    (0x0400_0400, "SOUND0CNT", 32),
    (0x0400_0404, "SOUND0SAD", 32),
    (0x0400_0408, "SOUND0TMR", 16),
    (0x0400_040A, "SOUND0PNT", 16),
    (0x0400_040C, "SOUND0LEN", 32),
    (0x0400_0410, "SOUND1CNT", 32),
    (0x0400_0414, "SOUND1SAD", 32),
    (0x0400_0418, "SOUND1TMR", 16),
    (0x0400_041A, "SOUND1PNT", 16),
    (0x0400_041C, "SOUND1LEN", 32),
    (0x0400_0420, "SOUND2CNT", 32),
    (0x0400_0424, "SOUND2SAD", 32),
    (0x0400_0428, "SOUND2TMR", 16),
    (0x0400_042A, "SOUND2PNT", 16),
    (0x0400_042C, "SOUND2LEN", 32),
    (0x0400_0430, "SOUND3CNT", 32),
    (0x0400_0434, "SOUND3SAD", 32),
    (0x0400_0438, "SOUND3TMR", 16),
    (0x0400_043A, "SOUND3PNT", 16),
    (0x0400_043C, "SOUND3LEN", 32),
    (0x0400_0440, "SOUND4CNT", 32),
    (0x0400_0444, "SOUND4SAD", 32),
    (0x0400_0448, "SOUND4TMR", 16),
    (0x0400_044A, "SOUND4PNT", 16),
    (0x0400_044C, "SOUND4LEN", 32),
    (0x0400_0450, "SOUND5CNT", 32),
    (0x0400_0454, "SOUND5SAD", 32),
    (0x0400_0458, "SOUND5TMR", 16),
    (0x0400_045A, "SOUND5PNT", 16),
    (0x0400_045C, "SOUND5LEN", 32),
    (0x0400_0460, "SOUND6CNT", 32),
    (0x0400_0464, "SOUND6SAD", 32),
    (0x0400_0468, "SOUND6TMR", 16),
    (0x0400_046A, "SOUND6PNT", 16),
    (0x0400_046C, "SOUND6LEN", 32),
    (0x0400_0470, "SOUND7CNT", 32),
    (0x0400_0474, "SOUND7SAD", 32),
    (0x0400_0478, "SOUND7TMR", 16),
    (0x0400_047A, "SOUND7PNT", 16),
    (0x0400_047C, "SOUND7LEN", 32),
    (0x0400_0480, "SOUND8CNT", 32),
    (0x0400_0484, "SOUND8SAD", 32),
    (0x0400_0488, "SOUND8TMR", 16),
    (0x0400_048A, "SOUND8PNT", 16),
    (0x0400_048C, "SOUND8LEN", 32),
    (0x0400_0490, "SOUND9CNT", 32),
    (0x0400_0494, "SOUND9SAD", 32),
    (0x0400_0498, "SOUND9TMR", 16),
    (0x0400_049A, "SOUND9PNT", 16),
    (0x0400_049C, "SOUND9LEN", 32),
    (0x0400_04A0, "SOUND10CNT", 32),
    (0x0400_04A4, "SOUND10SAD", 32),
    (0x0400_04A8, "SOUND10TMR", 16),
    (0x0400_04AA, "SOUND10PNT", 16),
    (0x0400_04AC, "SOUND10LEN", 32),
    (0x0400_04B0, "SOUND11CNT", 32),
    (0x0400_04B4, "SOUND11SAD", 32),
    (0x0400_04B8, "SOUND11TMR", 16),
    (0x0400_04BA, "SOUND11PNT", 16),
    (0x0400_04BC, "SOUND11LEN", 32),
    (0x0400_04C0, "SOUND12CNT", 32),
    (0x0400_04C4, "SOUND12SAD", 32),
    (0x0400_04C8, "SOUND12TMR", 16),
    (0x0400_04CA, "SOUND12PNT", 16),
    (0x0400_04CC, "SOUND12LEN", 32),
    (0x0400_04D0, "SOUND13CNT", 32),
    (0x0400_04D4, "SOUND13SAD", 32),
    (0x0400_04D8, "SOUND13TMR", 16),
    (0x0400_04DA, "SOUND13PNT", 16),
    (0x0400_04DC, "SOUND13LEN", 32),
    (0x0400_04E0, "SOUND14CNT", 32),
    (0x0400_04E4, "SOUND14SAD", 32),
    (0x0400_04E8, "SOUND14TMR", 16),
    (0x0400_04EA, "SOUND14PNT", 16),
    (0x0400_04EC, "SOUND14LEN", 32),
    (0x0400_04F0, "SOUND15CNT", 32),
    (0x0400_04F4, "SOUND15SAD", 32),
    (0x0400_04F8, "SOUND15TMR", 16),
    (0x0400_04FA, "SOUND15PNT", 16),
    (0x0400_04FC, "SOUND15LEN", 32),
    // Sound control and capture.
    (0x0400_0500, "SOUNDCNT", 16),
    (0x0400_0504, "SOUNDBIAS", 16),
    (0x0400_0508, "SNDCAP0CNT", 8),
    (0x0400_0509, "SNDCAP1CNT", 8),
    (0x0400_0510, "SNDCAP0DAD", 32),
    (0x0400_0514, "SNDCAP0LEN", 16),
    (0x0400_0518, "SNDCAP1DAD", 32),
    (0x0400_051C, "SNDCAP1LEN", 16),
    // IPC, again.
    (0x0410_0000, "IPCFIFORECV", 32),
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{ARM7_IO_REGISTERS, ARM9_IO_REGISTERS};

    #[track_caller]
    fn check_sorted_aligned_and_apart(table: &[(u32, &str, u8)]) {
        for &(address, name, bits) in table {
            assert!(matches!(bits, 8 | 16 | 32), "{name} is {bits} bits");
            assert_eq!(address % u32::from(bits / 8), 0, "{name} is misaligned");
        }
        for pair in table.windows(2) {
            let [(address, name, bits), (next, next_name, _)] = [pair[0], pair[1]];
            assert!(
                address + u32::from(bits / 8) <= next,
                "{name} runs into {next_name}"
            );
        }
        let names: HashSet<_> = table.iter().map(|&(_, name, _)| name).collect();
        assert_eq!(names.len(), table.len(), "a name is repeated");
    }

    #[test]
    fn arm9_registers_are_sorted_aligned_and_apart() {
        check_sorted_aligned_and_apart(ARM9_IO_REGISTERS);
    }

    #[test]
    fn arm7_registers_are_sorted_aligned_and_apart() {
        check_sorted_aligned_and_apart(ARM7_IO_REGISTERS);
    }
}
