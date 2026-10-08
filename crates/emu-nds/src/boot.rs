//! Direct boot: the state the BIOS and firmware leave when they start a
//! cartridge (GBATEK, "DS Cartridge Header", "BIOS RAM Usage").
//!
//! Instead of running the firmware, the machine copies the header and both
//! programs from the cartridge to where its header says, writes the values
//! the boot sequence leaves in main RAM, and starts each CPU at its entry
//! point with the stacks and system registers the BIOS sets up.

use crate::hardware::Hardware;

/// Where a program goes, from the cartridge header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Program {
    /// Offset in the ROM.
    pub(crate) rom_offset: u32,
    pub(crate) entry: u32,
    /// Load address.
    pub(crate) ram_address: u32,
    pub(crate) size: u32,
}

/// The parts of the cartridge header that booting needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) arm9: Program,
    pub(crate) arm7: Program,
    pub(crate) header_crc: u16,
    pub(crate) secure_area_crc: u16,
}

fn word(rom: &[u8], offset: usize) -> u32 {
    rom.get(offset..offset + 4).map_or(0, |bytes| {
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    })
}

fn half(rom: &[u8], offset: usize) -> u16 {
    rom.get(offset..offset + 2)
        .map_or(0, |bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
}

impl Header {
    pub(crate) fn parse(rom: &[u8]) -> Self {
        let program = |base: usize| Program {
            rom_offset: word(rom, base),
            entry: word(rom, base + 4),
            ram_address: word(rom, base + 8),
            size: word(rom, base + 12),
        };
        Self {
            arm9: program(0x20),
            arm7: program(0x30),
            header_crc: half(rom, 0x15E),
            secure_area_crc: half(rom, 0x6C),
        }
    }
}

/// Stack pointers the BIOS leaves for each mode: (supervisor, IRQ,
/// system/user).
pub(crate) const ARM9_STACKS: (u32, u32, u32) = (0x0080_3FC0, 0x0080_3FA0, 0x0080_3EC0);
pub(crate) const ARM7_STACKS: (u32, u32, u32) = (0x0380_FFDC, 0x0380_FFB0, 0x0380_FF00);

/// Where the ARM9 BIOS leaves DTCM (16 KiB) and ITCM (32 KiB at 0, its
/// region covering 32 MiB), as CP15 region register values.
pub(crate) const DTCM_REGION: u32 = 0x0080_000A;
pub(crate) const ITCM_REGION: u32 = 0x0000_0020;

/// Copies the cartridge into memory and writes what the boot sequence
/// leaves in RAM and I/O. `user_settings` comes from the firmware;
/// `chip_id` from the cartridge.
pub(crate) fn load(hw: &mut Hardware, rom: &[u8], user_settings: &[u8], chip_id: u32) -> Header {
    let header = Header::parse(rom);
    // All shared WRAM to the ARM7, before its program may load there.
    hw.wramcnt = 3;
    // The header, at the top of main RAM.
    for (offset, byte) in rom.iter().take(0x170).enumerate() {
        hw.arm9_write_8(0x027F_FE00 + offset as u32, *byte);
    }
    let copy = |hw: &mut Hardware, program: Program, arm9: bool| {
        let start = program.rom_offset as usize;
        let end = start.saturating_add(program.size as usize).min(rom.len());
        for (offset, byte) in rom.get(start..end).unwrap_or(&[]).iter().enumerate() {
            let address = program.ram_address.wrapping_add(offset as u32);
            if arm9 {
                hw.arm9_write_8(address, *byte);
            } else {
                hw.arm7_write_8(address, *byte);
            }
        }
    };
    copy(hw, header.arm9, true);
    copy(hw, header.arm7, false);

    let put_32 = |hw: &mut Hardware, address: u32, value: u32| hw.arm9_write_32(address, value);
    let crcs = u32::from(header.header_crc) | u32::from(header.secure_area_crc) << 16;
    for base in [0x027F_F800, 0x027F_FC00] {
        put_32(hw, base, chip_id);
        put_32(hw, base + 4, chip_id);
        put_32(hw, base + 8, crcs);
    }
    // The ARM7 BIOS's CRC, which games check to tell a DS from a DSi.
    hw.arm9_write_16(0x027F_F850, 0x5835);
    hw.arm9_write_16(0x027F_FC10, 0x5835);
    // What the firmware read from the GBA slot's cartridge header; a
    // module ID of 0xFFFF means the slot is empty.
    let module_info = hw.gba_slot.module_info();
    for (offset, byte) in (0u32..).zip(module_info) {
        hw.arm9_write_8(0x027F_FC30 + offset, byte);
    }
    // Booted from a cartridge.
    hw.arm9_write_16(0x027F_FC40, 1);
    for (offset, byte) in user_settings.iter().take(0x70).enumerate() {
        hw.arm9_write_8(0x027F_FC80 + offset as u32, *byte);
    }

    // System registers as the BIOS leaves them.
    hw.arm9.postflg = 1;
    hw.arm7.postflg = 1;
    // Both LCDs and 2D engines on, 3D on, engine A on the top screen.
    hw.powcnt1 = 0x820F;
    // Sound on, wifi off; the firmware left WIFIWAITCNT at 0030h.
    hw.powcnt2 = 1;
    hw.arm7_write_16(0x0400_0206, 0x0030);
    hw.arm7_write_16(0x0400_0504, 0x0200);
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom() -> Vec<u8> {
        let mut rom = vec![0; 0x9000];
        let mut put = |offset: usize, value: u32| {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        // ARM9: 16 bytes from 0x4000 to 0x02004000, entry 0x02004000.
        put(0x20, 0x4000);
        put(0x24, 0x0200_4000);
        put(0x28, 0x0200_4000);
        put(0x2C, 0x10);
        // ARM7: 8 bytes from 0x8000 to ARM7 WRAM.
        put(0x30, 0x8000);
        put(0x34, 0x037F_8000);
        put(0x38, 0x037F_8000);
        put(0x3C, 0x8);
        put(0x4000, 0xE3A0_002A);
        put(0x8000, 0xE3A0_102A);
        rom
    }

    #[test]
    fn copies_programs_and_header_where_the_header_says() {
        let rom = rom();
        let mut hw = Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into());
        let header = load(&mut hw, &rom, &[0xAB; 0x70], 0x0000_7FC2);
        assert_eq!(header.arm9.entry, 0x0200_4000);
        assert_eq!(hw.arm9_read_32(0x0200_4000), 0xE3A0_002A);
        // 0x037F8000 is shared WRAM, which WRAMCNT 3 gives the ARM7.
        assert_eq!(hw.arm7_read_32(0x037F_8000), 0xE3A0_102A);
        assert_eq!(hw.arm9_read_32(0x027F_FE20), 0x4000, "header copy");
        assert_eq!(hw.arm9_read_32(0x027F_FC00), 0x0000_7FC2, "chip ID");
        assert_eq!(hw.arm9_read_8(0x027F_FC80), 0xAB, "user settings");
        assert_eq!(hw.wramcnt, 3);
    }
}
