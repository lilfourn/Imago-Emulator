//! Each CPU's memory map and wait states (GBATEK, "DS Memory Map", "DS
//! Memory Timings").
//!
//! The ARM9's tightly coupled memory never reaches here: the CPU serves it.
//! Wait states are approximations: the ARM9's caches are not modelled, so
//! its code fetches from main RAM are charged as if they mostly hit.

use emu_arm::Access;

use crate::hardware::Hardware;
use crate::vram::{Region, Vram};

/// Access width, for wait states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Width {
    Byte,
    Half,
    Word,
}

fn half(memory: &[u8], index: usize) -> u16 {
    u16::from_le_bytes([memory[index], memory[index + 1]])
}

fn word(memory: &[u8], index: usize) -> u32 {
    u32::from_le_bytes([
        memory[index],
        memory[index + 1],
        memory[index + 2],
        memory[index + 3],
    ])
}

fn put_half(memory: &mut [u8], index: usize, value: u16) {
    memory[index..index + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_word(memory: &mut [u8], index: usize, value: u32) {
    memory[index..index + 4].copy_from_slice(&value.to_le_bytes());
}

/// Where a memory address lands, for the plain-memory regions both CPUs
/// share. Everything else is decoded per CPU.
#[derive(Debug, Clone, Copy)]
enum Place {
    MainRam(usize),
    SharedWram(usize),
    Arm7Wram(usize),
    /// Palette RAM: engine A below 0x400, engine B above.
    Palette(u32),
    /// OAM, split the same way.
    Oam(u32),
    Vram(Region, u32),
    Arm9Bios(usize),
    Arm7Bios(usize),
    Io,
    /// The GBA slot with no cartridge, or nothing at all.
    Open,
}

impl Hardware {
    fn read_palette(&self, offset: u32) -> u16 {
        if offset < 0x400 {
            self.engine_a.read_palette(offset)
        } else {
            self.engine_b.read_palette(offset & 0x3FF)
        }
    }

    fn write_palette(&mut self, offset: u32, value: u16) {
        if offset < 0x400 {
            self.engine_a.write_palette(offset, value);
        } else {
            self.engine_b.write_palette(offset & 0x3FF, value);
        }
    }

    fn read_oam(&self, offset: u32) -> u16 {
        if offset < 0x400 {
            self.engine_a.read_oam(offset)
        } else {
            self.engine_b.read_oam(offset & 0x3FF)
        }
    }

    fn write_oam(&mut self, offset: u32, value: u16) {
        if offset < 0x400 {
            self.engine_a.write_oam(offset, value);
        } else {
            self.engine_b.write_oam(offset & 0x3FF, value);
        }
    }

    fn arm9_place(&self, address: u32) -> Place {
        match address >> 24 {
            0x02 => Place::MainRam(address as usize & 0x3F_FFFF),
            0x03 => match self.wramcnt & 3 {
                0 => Place::SharedWram(address as usize & 0x7FFF),
                1 => Place::SharedWram(0x4000 + (address as usize & 0x3FFF)),
                2 => Place::SharedWram(address as usize & 0x3FFF),
                _ => Place::Open,
            },
            0x04 => Place::Io,
            0x05 => Place::Palette(address & 0x7FF),
            0x06 => match Vram::arm9_region(address) {
                Some((region, offset)) => Place::Vram(region, offset),
                None => Place::Open,
            },
            0x07 => Place::Oam(address & 0x7FF),
            0xFF if address >= 0xFFFF_0000 => Place::Arm9Bios(address as usize & 0xFFF),
            _ => Place::Open,
        }
    }

    fn arm7_place(&self, address: u32) -> Place {
        match address >> 24 {
            0x00 if address < 0x4000 => Place::Arm7Bios(address as usize),
            0x02 => Place::MainRam(address as usize & 0x3F_FFFF),
            0x03 if address < 0x0380_0000 => match self.wramcnt & 3 {
                // With no shared WRAM, the ARM7's own WRAM shows here too.
                0 => Place::Arm7Wram(address as usize & 0xFFFF),
                1 => Place::SharedWram(address as usize & 0x3FFF),
                2 => Place::SharedWram(0x4000 + (address as usize & 0x3FFF)),
                _ => Place::SharedWram(address as usize & 0x7FFF),
            },
            0x03 => Place::Arm7Wram(address as usize & 0xFFFF),
            0x04 => Place::Io,
            0x06 => Place::Vram(Region::Arm7, address & 0x3_FFFF),
            _ => Place::Open,
        }
    }

    fn place(&self, arm9: bool, address: u32) -> Place {
        if arm9 {
            self.arm9_place(address)
        } else {
            self.arm7_place(address)
        }
    }

    fn read_place_32(&mut self, arm9: bool, address: u32) -> u32 {
        let address = address & !3;
        match self.place(arm9, address) {
            Place::MainRam(index) => word(&self.main_ram, index),
            Place::SharedWram(index) => word(&self.shared_wram, index),
            Place::Arm7Wram(index) => word(&self.arm7_wram, index),
            Place::Palette(offset) => {
                u32::from(self.read_palette(offset))
                    | u32::from(self.read_palette(offset + 2)) << 16
            }
            Place::Oam(offset) => {
                u32::from(self.read_oam(offset)) | u32::from(self.read_oam(offset + 2)) << 16
            }
            Place::Vram(region, offset) => {
                u32::from(self.vram.read_16(region, offset))
                    | u32::from(self.vram.read_16(region, offset + 2)) << 16
            }
            Place::Arm9Bios(index) => word(&self.arm9_bios, index),
            Place::Arm7Bios(index) => word(&self.arm7_bios, index),
            Place::Io => {
                if arm9 {
                    self.io9_read_32(address)
                } else {
                    self.io7_read_32(address)
                }
            }
            Place::Open => 0xFFFF_FFFF,
        }
    }

    fn read_place_16(&mut self, arm9: bool, address: u32) -> u16 {
        let address = address & !1;
        match self.place(arm9, address) {
            Place::MainRam(index) => half(&self.main_ram, index),
            Place::SharedWram(index) => half(&self.shared_wram, index),
            Place::Arm7Wram(index) => half(&self.arm7_wram, index),
            Place::Palette(offset) => self.read_palette(offset),
            Place::Oam(offset) => self.read_oam(offset),
            Place::Vram(region, offset) => self.vram.read_16(region, offset),
            Place::Arm9Bios(index) => half(&self.arm9_bios, index),
            Place::Arm7Bios(index) => half(&self.arm7_bios, index),
            Place::Io => {
                if arm9 {
                    self.io9_read_16(address)
                } else {
                    self.io7_read_16(address)
                }
            }
            Place::Open => 0xFFFF,
        }
    }

    fn read_place_8(&mut self, arm9: bool, address: u32) -> u8 {
        match self.place(arm9, address) {
            Place::MainRam(index) => self.main_ram[index],
            Place::SharedWram(index) => self.shared_wram[index],
            Place::Arm7Wram(index) => self.arm7_wram[index],
            Place::Palette(offset) => (self.read_palette(offset) >> ((offset & 1) * 8)) as u8,
            Place::Oam(offset) => (self.read_oam(offset) >> ((offset & 1) * 8)) as u8,
            Place::Vram(region, offset) => self.vram.read_8(region, offset),
            Place::Arm9Bios(index) => self.arm9_bios[index],
            Place::Arm7Bios(index) => self.arm7_bios[index],
            Place::Io => {
                if arm9 {
                    self.io9_read_8(address)
                } else {
                    self.io7_read_8(address)
                }
            }
            Place::Open => 0xFF,
        }
    }

    fn write_place_32(&mut self, arm9: bool, address: u32, value: u32) {
        let address = address & !3;
        match self.place(arm9, address) {
            Place::MainRam(index) => put_word(&mut self.main_ram, index, value),
            Place::SharedWram(index) => put_word(&mut self.shared_wram, index, value),
            Place::Arm7Wram(index) => put_word(&mut self.arm7_wram, index, value),
            Place::Palette(offset) => {
                self.write_palette(offset, value as u16);
                self.write_palette(offset + 2, (value >> 16) as u16);
            }
            Place::Oam(offset) => {
                self.write_oam(offset, value as u16);
                self.write_oam(offset + 2, (value >> 16) as u16);
            }
            Place::Vram(region, offset) => {
                self.vram.write_16(region, offset, value as u16);
                self.vram.write_16(region, offset + 2, (value >> 16) as u16);
            }
            Place::Io => {
                if arm9 {
                    self.io9_write_32(address, value);
                } else {
                    self.io7_write_32(address, value);
                }
            }
            Place::Arm9Bios(_) | Place::Arm7Bios(_) | Place::Open => {}
        }
    }

    fn write_place_16(&mut self, arm9: bool, address: u32, value: u16) {
        let address = address & !1;
        match self.place(arm9, address) {
            Place::MainRam(index) => put_half(&mut self.main_ram, index, value),
            Place::SharedWram(index) => put_half(&mut self.shared_wram, index, value),
            Place::Arm7Wram(index) => put_half(&mut self.arm7_wram, index, value),
            Place::Palette(offset) => self.write_palette(offset, value),
            Place::Oam(offset) => self.write_oam(offset, value),
            Place::Vram(region, offset) => self.vram.write_16(region, offset, value),
            Place::Io => {
                if arm9 {
                    self.io9_write_16(address, value);
                } else {
                    self.io7_write_16(address, value);
                }
            }
            Place::Arm9Bios(_) | Place::Arm7Bios(_) | Place::Open => {}
        }
    }

    fn write_place_8(&mut self, arm9: bool, address: u32, value: u8) {
        match self.place(arm9, address) {
            Place::MainRam(index) => self.main_ram[index] = value,
            Place::SharedWram(index) => self.shared_wram[index] = value,
            Place::Arm7Wram(index) => self.arm7_wram[index] = value,
            Place::Vram(region, offset) => self.vram.write_8(region, offset, value),
            Place::Io => {
                if arm9 {
                    self.io9_write_8(address, value);
                } else {
                    self.io7_write_8(address, value);
                }
            }
            // Palette and OAM ignore byte writes.
            Place::Palette(_)
            | Place::Oam(_)
            | Place::Arm9Bios(_)
            | Place::Arm7Bios(_)
            | Place::Open => {}
        }
    }

    // The ARM9's view.

    pub(crate) fn arm9_read_32(&mut self, address: u32) -> u32 {
        self.read_place_32(true, address)
    }

    pub(crate) fn arm9_read_16(&mut self, address: u32) -> u16 {
        self.read_place_16(true, address)
    }

    pub(crate) fn arm9_read_8(&mut self, address: u32) -> u8 {
        self.read_place_8(true, address)
    }

    pub(crate) fn arm9_write_32(&mut self, address: u32, value: u32) {
        self.write_place_32(true, address, value);
    }

    pub(crate) fn arm9_write_16(&mut self, address: u32, value: u16) {
        self.write_place_16(true, address, value);
    }

    pub(crate) fn arm9_write_8(&mut self, address: u32, value: u8) {
        self.write_place_8(true, address, value);
    }

    // The ARM7's view.

    pub(crate) fn arm7_read_32(&mut self, address: u32) -> u32 {
        self.read_place_32(false, address)
    }

    pub(crate) fn arm7_read_16(&mut self, address: u32) -> u16 {
        self.read_place_16(false, address)
    }

    pub(crate) fn arm7_read_8(&mut self, address: u32) -> u8 {
        self.read_place_8(false, address)
    }

    pub(crate) fn arm7_write_32(&mut self, address: u32, value: u32) {
        self.write_place_32(false, address, value);
    }

    pub(crate) fn arm7_write_16(&mut self, address: u32, value: u16) {
        self.write_place_16(false, address, value);
    }

    pub(crate) fn arm7_write_8(&mut self, address: u32, value: u8) {
        self.write_place_8(false, address, value);
    }

    /// ARM9 cycles for an access, two per bus cycle.
    pub(crate) fn arm9_wait(&self, address: u32, width: Width, access: Access) -> u64 {
        let sequential = access.contains(Access::SEQUENTIAL);
        match self.arm9_place(address) {
            Place::MainRam(_) if access.contains(Access::CODE) => 2,
            Place::MainRam(_) => match (width, sequential) {
                (Width::Word, false) => 20,
                (_, false) => 18,
                (Width::Word, true) => 4,
                (_, true) => 2,
            },
            Place::Vram(..) | Place::Palette(_) | Place::Oam(_) if width == Width::Word => 4,
            _ => 2,
        }
    }

    /// ARM7 cycles (bus cycles) for an access.
    pub(crate) fn arm7_wait(&self, address: u32, width: Width, access: Access) -> u64 {
        let sequential = access.contains(Access::SEQUENTIAL);
        match self.arm7_place(address) {
            // Main RAM sits on a 16-bit bus.
            Place::MainRam(_) => match (width, sequential) {
                (Width::Word, false) => 9,
                (_, false) => 8,
                (Width::Word, true) => 2,
                (_, true) => 1,
            },
            Place::Vram(..) if width == Width::Word => 2,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hardware() -> Hardware {
        Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into())
    }

    #[test]
    fn main_ram_mirrors_every_4_mib() {
        let mut hw = hardware();
        hw.arm9_write_32(0x027F_FE00, 0x1234_5678);
        assert_eq!(hw.arm7_read_32(0x023F_FE00), 0x1234_5678);
        assert_eq!(hw.arm7_read_16(0x023F_FE02), 0x1234);
        assert_eq!(hw.arm9_read_8(0x02FF_FE00), 0x78);
    }

    #[test]
    fn shared_wram_follows_wramcnt() {
        let mut hw = hardware();
        hw.wramcnt = 3; // all to the ARM7
        hw.arm7_write_32(0x0300_0000, 7);
        assert_eq!(hw.arm9_read_32(0x0300_0000), 0xFFFF_FFFF);
        hw.wramcnt = 0; // all to the ARM9; the ARM7 sees its own WRAM
        assert_eq!(hw.arm9_read_32(0x0300_0000), 7);
        hw.arm7_write_32(0x0380_0004, 9);
        assert_eq!(hw.arm7_read_32(0x0300_0004), 9);
        hw.wramcnt = 1; // ARM9 gets the second half
        hw.arm9_write_8(0x0300_0000, 0xAA);
        assert_eq!(hw.shared_wram[0x4000], 0xAA);
    }

    #[test]
    fn byte_writes_skip_palette_and_oam() {
        let mut hw = hardware();
        hw.arm9_write_8(0x0500_0000, 0x12);
        hw.arm9_write_8(0x0700_0000, 0x12);
        assert_eq!(
            (hw.arm9_read_16(0x0500_0000), hw.arm9_read_16(0x0700_0000)),
            (0, 0)
        );
        hw.arm9_write_16(0x0500_0402, 0x7FFF);
        assert_eq!(hw.arm9_read_16(0x0500_0402), 0x7FFF);
        assert_eq!(hw.engine_b.read_palette(2), 0x7FFF);
    }

    #[test]
    fn arm7_sees_its_bios_and_vram_slots() {
        let mut hw = hardware();
        hw.arm7_bios[0] = 0xEA;
        assert_eq!(hw.arm7_read_8(0), 0xEA);
        assert_eq!(hw.arm9_read_8(0), 0xFF, "the ARM9 has no BIOS at zero");
        hw.vram.set_control(2, 0x82); // C to the ARM7
        hw.arm7_write_16(0x0600_0010, 0x55AA);
        assert_eq!(
            hw.arm7_read_16(0x0604_0010),
            0x55AA,
            "mirrors every 256 KiB"
        );
    }
}
