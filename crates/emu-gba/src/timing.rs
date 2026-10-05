//! How many cycles a memory access takes (GBATEK, "GBA Memory Map",
//! "GBA System Control": WAITCNT).
//!
//! Every access costs one cycle plus the region's wait states. Game Pak ROM
//! and SRAM wait states come from WAITCNT, with separate first
//! (non-sequential) and next (sequential) timings. Regions on a 16-bit bus
//! (EWRAM, ROM, palette, VRAM) take two accesses for a word.

/// Access width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Width {
    Byte,
    Half,
    Word,
}

/// Cycles for each access, rebuilt whenever WAITCNT changes.
#[derive(Debug, Clone)]
pub(crate) struct WaitStates {
    /// [region][width: 0 byte/half, 1 word][sequential] → cycles.
    table: [[[u8; 2]; 2]; 16],
}

/// Fixed cycles for regions WAITCNT does not configure: (16-bit, 32-bit).
const FIXED: [(u8, u8); 8] = [
    (1, 1), // 0x00 BIOS
    (1, 1), // 0x01 unmapped
    (3, 6), // 0x02 EWRAM, 16-bit bus with 2 wait states
    (1, 1), // 0x03 IWRAM
    (1, 1), // 0x04 I/O
    (1, 2), // 0x05 palette, 16-bit bus
    (1, 2), // 0x06 VRAM, 16-bit bus
    (1, 1), // 0x07 OAM
];

/// First-access wait states selected by WAITCNT's two-bit fields.
const FIRST_WAIT: [u8; 4] = [4, 3, 2, 8];

impl WaitStates {
    pub(crate) fn new(waitcnt: u16) -> Self {
        let mut table = [[[1; 2]; 2]; 16];
        for (region, &(half, word)) in FIXED.iter().enumerate() {
            table[region] = [[half, half], [word, word]];
        }
        // Three ROM mirrors with their own wait states, each 32 MiB wide.
        let second_wait = [[2, 1], [4, 1], [8, 1]];
        for state in 0..3 {
            let field = u32::from(waitcnt) >> (2 + state * 3);
            let first = FIRST_WAIT[(field & 3) as usize] + 1;
            let second = second_wait[state as usize][((field >> 2) & 1) as usize] + 1;
            for region in [0x08 + state * 2, 0x09 + state * 2] {
                table[region as usize] = [[first, second], [first + second, second + second]];
            }
        }
        let sram = FIRST_WAIT[usize::from(waitcnt & 3)] + 1;
        for region in [0x0E, 0x0F] {
            table[region] = [[sram, sram], [sram, sram]];
        }
        Self { table }
    }

    /// Cycles for an access of `width` to `address`.
    pub(crate) fn cycles(&self, address: u32, width: Width, sequential: bool) -> u32 {
        let region = (address >> 24) as usize & 0xF;
        let wide = usize::from(width == Width::Word);
        u32::from(self.table[region][wide][usize::from(sequential)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rom_timing_is_4_2() {
        let waits = WaitStates::new(0);
        assert_eq!(waits.cycles(0x0800_0000, Width::Half, false), 5);
        assert_eq!(waits.cycles(0x0800_0000, Width::Half, true), 3);
        assert_eq!(waits.cycles(0x0800_0000, Width::Word, false), 8);
    }

    #[test]
    fn fast_rom_timing_from_waitcnt() {
        // WS0 first = 2 wait states (field 2), second = 1 (bit 4 set).
        let waits = WaitStates::new(0b1_1000);
        assert_eq!(waits.cycles(0x0900_0000, Width::Half, false), 3);
        assert_eq!(waits.cycles(0x0900_0000, Width::Half, true), 2);
    }

    #[test]
    fn ewram_words_take_two_accesses() {
        let waits = WaitStates::new(0);
        assert_eq!(waits.cycles(0x0200_0000, Width::Word, true), 6);
        assert_eq!(waits.cycles(0x0300_0000, Width::Word, false), 1);
    }
}
