//! Each CPU's memory map and wait states (GBATEK, "DS Memory Map", "DS
//! Memory Timings").
//!
//! The ARM9's tightly coupled memory never reaches here: the CPU serves it.
//! Wait states are approximations: the ARM9's caches are not modelled, so
//! its main RAM accesses are charged as if they hit.

use emu_arm::Access;

use crate::gba_slot;
use crate::hardware::{Hardware, MAIN_RAM_SIZE};
use crate::ipc::Side;
use crate::vram::{Region, Vram};

/// ARM9 cycles for the CPU's own main RAM accesses, charged as cache
/// hits.
const ARM9_CACHE_HIT: u64 = 2;

/// ARM7 cycles for an access to its working RAM, shared or its own, at
/// any width.
const ARM7_WRAM: u64 = 1;

/// Access width, for wait states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Width {
    Byte,
    Half,
    Word,
}

// The bytes are taken as one slice, so an access costs one bounds check
// and one load rather than one per byte.

pub(crate) fn half(memory: &[u8], index: usize) -> u16 {
    let bytes = &memory[index..index + 2];
    u16::from_le_bytes([bytes[0], bytes[1]])
}

pub(crate) fn word(memory: &[u8], index: usize) -> u32 {
    let bytes = &memory[index..index + 4];
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

pub(crate) fn put_half(memory: &mut [u8], index: usize, value: u16) {
    memory[index..index + 2].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_word(memory: &mut [u8], index: usize, value: u32) {
    memory[index..index + 4].copy_from_slice(&value.to_le_bytes());
}

/// The CPU an access comes from.
fn side(arm9: bool) -> Side {
    if arm9 { Side::Arm9 } else { Side::Arm7 }
}

/// Where a memory address lands, for the plain-memory regions both CPUs
/// share. Everything else is decoded per CPU. A bus access decodes its
/// address once, for both its wait states and the access itself.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Place {
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
    /// The GBA slot's ROM, 0x08000000-0x09FFFFFF, on a 16-bit bus.
    GbaRom,
    /// The GBA slot's SRAM or Flash, 0x0A000000-0x0AFFFFFF, on an 8-bit
    /// bus that sees unaligned addresses as they are.
    GbaRam,
    /// The GBA slot while EXMEMCNT gives it to the other CPU: zeros.
    GbaDeselected,
    /// The ARM7's wifi registers and RAM, 0x04800000-0x0480FFFF, on a
    /// 16-bit bus.
    Wifi,
    /// Nothing at all.
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

    /// `place` in the GBA slot if `side` owns it.
    fn gba_slot_place(&self, side: Side, place: Place) -> Place {
        if self.gba_slot_owner() == side {
            place
        } else {
            Place::GbaDeselected
        }
    }

    #[inline]
    pub(crate) fn arm9_place(&self, address: u32) -> Place {
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
            0x08 | 0x09 => self.gba_slot_place(Side::Arm9, Place::GbaRom),
            0x0A => self.gba_slot_place(Side::Arm9, Place::GbaRam),
            0xFF if address >= 0xFFFF_0000 => Place::Arm9Bios(address as usize & 0xFFF),
            _ => Place::Open,
        }
    }

    #[inline]
    pub(crate) fn arm7_place(&self, address: u32) -> Place {
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
            0x04 if (0x0480_0000..0x0481_0000).contains(&address) => Place::Wifi,
            0x04 => Place::Io,
            0x06 => Place::Vram(Region::Arm7, address & 0x3_FFFF),
            0x08 | 0x09 => self.gba_slot_place(Side::Arm7, Place::GbaRom),
            0x0A => self.gba_slot_place(Side::Arm7, Place::GbaRam),
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

    /// A word from `place`, where the ARM9 (if `arm9`) or the ARM7 sees
    /// `address` with its low two bits cleared; `address` as the access
    /// gave it.
    pub(crate) fn read_place_32(&mut self, arm9: bool, place: Place, address: u32) -> u32 {
        let unaligned = address;
        let address = address & !3;
        match place {
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
            Place::GbaRom => {
                u32::from(self.gba_slot.read_rom(address))
                    | u32::from(self.gba_slot.read_rom(address | 2)) << 16
            }
            // An 8-bit bus: the byte repeats across the word.
            Place::GbaRam => u32::from(self.gba_slot.read_ram(unaligned)) * 0x0101_0101,
            Place::GbaDeselected => 0,
            Place::Wifi => {
                u32::from(self.wifi_read_16(address))
                    | u32::from(self.wifi_read_16(address | 2)) << 16
            }
            Place::Open => 0xFFFF_FFFF,
        }
    }

    /// A halfword from `place`, where `address` lands with bit 0 cleared.
    pub(crate) fn read_place_16(&mut self, arm9: bool, place: Place, address: u32) -> u16 {
        let unaligned = address;
        let address = address & !1;
        match place {
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
            Place::GbaRom => self.gba_slot.read_rom(address),
            Place::GbaRam => u16::from(self.gba_slot.read_ram(unaligned)) * 0x0101,
            Place::GbaDeselected => 0,
            Place::Wifi => self.wifi_read_16(address),
            Place::Open => 0xFFFF,
        }
    }

    /// A byte from `place`, where `address` lands.
    pub(crate) fn read_place_8(&mut self, arm9: bool, place: Place, address: u32) -> u8 {
        match place {
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
            Place::GbaRom => (self.gba_slot.read_rom(address & !1) >> ((address & 1) * 8)) as u8,
            Place::GbaRam => self.gba_slot.read_ram(address),
            Place::GbaDeselected => 0,
            Place::Wifi => (self.wifi_read_16(address & !1) >> ((address & 1) * 8)) as u8,
            Place::Open => 0xFF,
        }
    }

    /// Writes a word to `place`, where `address` lands with its low two
    /// bits cleared.
    pub(crate) fn write_place_32(&mut self, arm9: bool, place: Place, address: u32, value: u32) {
        let unaligned = address;
        let address = address & !3;
        match place {
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
            Place::GbaRom => {
                let now = self.now(side(arm9));
                self.gba_slot.write_rom(address, value as u16, now);
                self.gba_slot
                    .write_rom(address | 2, (value >> 16) as u16, now);
            }
            // Only the byte lane at the (unaligned) address reaches the
            // 8-bit bus.
            Place::GbaRam => {
                let byte = value.rotate_right((unaligned & 3) * 8) as u8;
                self.gba_slot.write_ram(unaligned, byte);
            }
            Place::Wifi => {
                self.wifi_write_16(address, value as u16);
                self.wifi_write_16(address | 2, (value >> 16) as u16);
            }
            Place::Arm9Bios(_) | Place::Arm7Bios(_) | Place::GbaDeselected | Place::Open => {}
        }
    }

    /// Writes a halfword to `place`, where `address` lands with bit 0
    /// cleared.
    pub(crate) fn write_place_16(&mut self, arm9: bool, place: Place, address: u32, value: u16) {
        let unaligned = address;
        let address = address & !1;
        match place {
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
            Place::GbaRom => {
                let now = self.now(side(arm9));
                self.gba_slot.write_rom(address, value, now);
            }
            Place::GbaRam => {
                let byte = (value >> ((unaligned & 1) * 8)) as u8;
                self.gba_slot.write_ram(unaligned, byte);
            }
            Place::Wifi => self.wifi_write_16(address, value),
            Place::Arm9Bios(_) | Place::Arm7Bios(_) | Place::GbaDeselected | Place::Open => {}
        }
    }

    /// Writes a byte to `place`, where `address` lands.
    pub(crate) fn write_place_8(&mut self, arm9: bool, place: Place, address: u32, value: u8) {
        match place {
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
            Place::GbaRam => self.gba_slot.write_ram(address, value),
            // Palette, OAM, the GBA slot's ROM and wifi ignore byte writes.
            Place::Palette(_)
            | Place::Oam(_)
            | Place::Wifi
            | Place::Arm9Bios(_)
            | Place::Arm7Bios(_)
            | Place::GbaRom
            | Place::GbaDeselected
            | Place::Open => {}
        }
    }

    /// The byte at `address` as the ARM9 (if `arm9`) or the ARM7 reads
    /// it, without side effects or time passing, for debuggers: memory,
    /// the BIOS, the GBA slot and the I/O registers whose reads change
    /// nothing. `None` for unmapped space and I/O reads that would. The
    /// ARM9's TCM is the CPU's to answer.
    pub(crate) fn peek(&self, arm9: bool, address: u32) -> Option<u8> {
        let byte = |half: u16| (half >> ((address & 1) * 8)) as u8;
        Some(match self.place(arm9, address) {
            Place::MainRam(index) => self.main_ram[index],
            Place::SharedWram(index) => self.shared_wram[index],
            Place::Arm7Wram(index) => self.arm7_wram[index],
            Place::Palette(offset) => byte(self.read_palette(offset)),
            Place::Oam(offset) => byte(self.read_oam(offset)),
            Place::Vram(region, offset) => self.vram.read_8(region, offset),
            Place::Arm9Bios(index) => self.arm9_bios[index],
            Place::Arm7Bios(index) => self.arm7_bios[index],
            Place::Io => return self.peek_io(side(arm9), address),
            Place::GbaRom => byte(self.gba_slot.peek_rom(address)),
            Place::GbaRam => self.gba_slot.read_ram(address),
            Place::GbaDeselected => 0,
            Place::Wifi => return self.wifi.peek(address),
            Place::Open => return None,
        })
    }

    /// Writes the byte at `address` as the ARM9 (if `arm9`) or the ARM7
    /// sees it, without side effects, for debuggers: main RAM, WRAM,
    /// palette RAM, VRAM and OAM, a byte at a time (unlike CPU byte stores
    /// to palette RAM and OAM). Returns whether `address` is one of those.
    pub(crate) fn poke(&mut self, arm9: bool, address: u32, value: u8) -> bool {
        let merge = |half: u16| {
            let shift = (address & 1) * 8;
            (half & !(0xFF << shift)) | u16::from(value) << shift
        };
        match self.place(arm9, address) {
            Place::MainRam(index) => self.main_ram[index] = value,
            Place::SharedWram(index) => self.shared_wram[index] = value,
            Place::Arm7Wram(index) => self.arm7_wram[index] = value,
            Place::Palette(offset) => {
                let half = merge(self.read_palette(offset));
                self.write_palette(offset, half);
            }
            Place::Oam(offset) => {
                let half = merge(self.read_oam(offset));
                self.write_oam(offset, half);
            }
            Place::Vram(region, offset) => self.vram.write_8(region, offset, value),
            _ => return false,
        }
        true
    }

    // The ARM9's view.

    #[cfg(test)]
    pub(crate) fn arm9_read_32(&mut self, address: u32) -> u32 {
        self.read_place_32(true, self.arm9_place(address & !3), address)
    }

    #[cfg(test)]
    pub(crate) fn arm9_read_16(&mut self, address: u32) -> u16 {
        self.read_place_16(true, self.arm9_place(address & !1), address)
    }

    #[cfg(test)]
    pub(crate) fn arm9_read_8(&mut self, address: u32) -> u8 {
        self.read_place_8(true, self.arm9_place(address), address)
    }

    pub(crate) fn arm9_write_32(&mut self, address: u32, value: u32) {
        self.write_place_32(true, self.arm9_place(address & !3), address, value);
    }

    pub(crate) fn arm9_write_16(&mut self, address: u32, value: u16) {
        self.write_place_16(true, self.arm9_place(address & !1), address, value);
    }

    pub(crate) fn arm9_write_8(&mut self, address: u32, value: u8) {
        self.write_place_8(true, self.arm9_place(address), address, value);
    }

    // The ARM7's view.

    #[cfg(test)]
    pub(crate) fn arm7_read_32(&mut self, address: u32) -> u32 {
        self.read_place_32(false, self.arm7_place(address & !3), address)
    }

    #[cfg(test)]
    pub(crate) fn arm7_read_16(&mut self, address: u32) -> u16 {
        self.read_place_16(false, self.arm7_place(address & !1), address)
    }

    pub(crate) fn arm7_read_8(&mut self, address: u32) -> u8 {
        self.read_place_8(false, self.arm7_place(address), address)
    }

    #[cfg(test)]
    pub(crate) fn arm7_write_32(&mut self, address: u32, value: u32) {
        self.write_place_32(false, self.arm7_place(address & !3), address, value);
    }

    pub(crate) fn arm7_write_16(&mut self, address: u32, value: u16) {
        self.write_place_16(false, self.arm7_place(address & !1), address, value);
    }

    pub(crate) fn arm7_write_8(&mut self, address: u32, value: u8) {
        self.write_place_8(false, self.arm7_place(address), address, value);
    }

    /// Where an ARM9 access to `address` lands in main RAM if it is one
    /// the caches serve, the CPU's own rather than DMA's, after charging
    /// its cycles as [`Hardware::arm9_wait`] does; `None` for every other
    /// access. Nearly all the ARM9's bus traffic is these, so its bus tries
    /// them before the memory map.
    #[inline]
    pub(crate) fn arm9_cache_hit(&mut self, address: u32, access: Access) -> Option<usize> {
        if address >> 24 != 0x02 || access.contains(Access::DMA) {
            return None;
        }
        self.arm9.cycles += ARM9_CACHE_HIT;
        Some(address as usize & (MAIN_RAM_SIZE - 1))
    }

    /// ARM9 cycles for an access to `place`, two per bus cycle. The CPU's
    /// own main RAM accesses are charged as cache hits, which nearly all of
    /// them are on hardware; DMA bypasses the caches and pays the full
    /// cost.
    pub(crate) fn arm9_wait(&self, place: Place, width: Width, access: Access) -> u64 {
        let sequential = access.contains(Access::SEQUENTIAL);
        match place {
            Place::MainRam(_) if !access.contains(Access::DMA) => ARM9_CACHE_HIT,
            Place::MainRam(_) => match (width, sequential) {
                (Width::Word, false) => 20,
                (_, false) => 18,
                (Width::Word, true) => 4,
                (_, true) => 2,
            },
            Place::Vram(..) | Place::Palette(_) | Place::Oam(_) if width == Width::Word => 4,
            Place::GbaRom => {
                2 * gba_slot::rom_cycles(self.gba_slot_timing(Side::Arm9), width, sequential)
            }
            Place::GbaRam => 2 * gba_slot::ram_cycles(self.gba_slot_timing(Side::Arm9)),
            _ => 2,
        }
    }

    /// The ARM7's working RAM, shared or its own, if that is where `place`
    /// is, and the index there, after charging the access's cycle as
    /// [`Hardware::arm7_wait`] does; `None` for every other place. The
    /// ARM7's code and stacks live there, so its bus tries it before the
    /// rest of the memory map.
    #[inline]
    pub(crate) fn arm7_wram_hit(&mut self, place: Place) -> Option<(&mut [u8], usize)> {
        let (memory, index) = match place {
            Place::SharedWram(index) => (&mut self.shared_wram, index),
            Place::Arm7Wram(index) => (&mut self.arm7_wram, index),
            _ => return None,
        };
        self.arm7.cycles += ARM7_WRAM;
        Some((memory, index))
    }

    /// ARM7 cycles (bus cycles) for an access to `address`, at `place`.
    pub(crate) fn arm7_wait(
        &self,
        place: Place,
        address: u32,
        width: Width,
        access: Access,
    ) -> u64 {
        let sequential = access.contains(Access::SEQUENTIAL);
        match place {
            // Main RAM sits on a 16-bit bus.
            Place::MainRam(_) => match (width, sequential) {
                (Width::Word, false) => 9,
                (_, false) => 8,
                (Width::Word, true) => 2,
                (_, true) => 1,
            },
            Place::Vram(..) if width == Width::Word => 2,
            Place::GbaRom => {
                gba_slot::rom_cycles(self.gba_slot_timing(Side::Arm7), width, sequential)
            }
            Place::GbaRam => gba_slot::ram_cycles(self.gba_slot_timing(Side::Arm7)),
            Place::Wifi => self.wifi_cycles(address, width, sequential),
            Place::SharedWram(_) | Place::Arm7Wram(_) => ARM7_WRAM,
            _ => 1,
        }
    }

    /// ARM7 cycles for a wifi access, by WIFIWAITCNT (0x04000206): each
    /// halfword takes the first or next access time of its 32 KiB region,
    /// WS0 (RAM's usual home) or WS1 (the registers'). GBATEK, "DS Power
    /// Control".
    fn wifi_cycles(&self, address: u32, width: Width, sequential: bool) -> u64 {
        const FIRST: [u64; 4] = [10, 8, 6, 18];
        let control = self.raw_16(Side::Arm7, 0x206);
        let (first, next) = if address & 0x8000 == 0 {
            (
                FIRST[usize::from(control & 3)],
                [6, 4][usize::from(control >> 2 & 1)],
            )
        } else {
            (
                FIRST[usize::from(control >> 3 & 3)],
                [10, 4][usize::from(control >> 5 & 1)],
            )
        };
        let first = if sequential { next } else { first };
        if width == Width::Word {
            first + next
        } else {
            first
        }
    }

    /// A halfword from the wifi hardware, read by the ARM7 at its time.
    fn wifi_read_16(&mut self, address: u32) -> u16 {
        let now = self.now(Side::Arm7);
        let value = self.wifi.read_16(address, now);
        self.sync_wifi();
        value
    }

    fn wifi_write_16(&mut self, address: u32, value: u16) {
        let now = self.now(Side::Arm7);
        self.wifi.write_16(address, value, now);
        self.sync_wifi();
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

    /// A GBA ROM with `library` (a save library ID string) at 0x200 and a
    /// recognisable header.
    fn gba_rom(library: &[u8]) -> Vec<u8> {
        let mut rom: Vec<u8> = (0..0x400u32).map(|index| index as u8).collect();
        rom[0xAC..0xB0].copy_from_slice(b"TEST");
        rom[0xB2] = 0x96;
        rom[0x200..0x200 + library.len()].copy_from_slice(library);
        rom
    }

    fn with_cartridge(rom: Vec<u8>) -> Hardware {
        let mut hw = hardware();
        let cartridge = emu_gba_cart::Cartridge::new(rom.into(), None, 0);
        hw.gba_slot = crate::gba_slot::GbaSlot::new(Some(cartridge));
        hw
    }

    #[test]
    fn the_arm9_reads_gba_rom_at_every_width() {
        let mut hw = with_cartridge(gba_rom(b""));
        assert_eq!(hw.arm9_read_8(0x0800_00AC), b'T');
        assert_eq!(hw.arm9_read_8(0x0800_00B2), 0x96);
        assert_eq!(hw.arm9_read_16(0x0800_0010), 0x1110);
        assert_eq!(hw.arm9_read_32(0x0800_00AC), u32::from_le_bytes(*b"TEST"));
        // Past the end of the ROM the bus returns the halfword address.
        assert_eq!(hw.arm9_read_16(0x0900_0010), 0x0008);
        assert_eq!(hw.arm9_read_32(0x0800_1000), 0x0801_0800);
        // ROM ignores writes.
        hw.arm9_write_16(0x0800_0010, 0);
        assert_eq!(hw.arm9_read_16(0x0800_0010), 0x1110);
    }

    #[test]
    fn exmemcnt_bit_7_hands_the_slot_to_the_arm7() {
        let mut hw = with_cartridge(gba_rom(b"SRAM_V"));
        hw.arm9_write_8(0x0A00_0000, 0x5A);
        assert_eq!(hw.arm7_read_32(0x0800_00AC), 0, "the ARM7 is deselected");
        assert_eq!(hw.arm7_read_8(0x0A00_0000), 0);
        hw.arm9_write_16(0x0400_0204, 0x0080);
        assert_eq!(hw.arm9_read_32(0x0800_00AC), 0);
        assert_eq!(hw.arm9_read_8(0x0A00_0000), 0);
        hw.arm9_write_8(0x0A00_0000, 0x11);
        assert_eq!(hw.arm7_read_32(0x0800_00AC), u32::from_le_bytes(*b"TEST"));
        assert_eq!(
            hw.arm7_read_8(0x0A00_0000),
            0x5A,
            "the ARM9's write was dropped"
        );
        assert_eq!(
            hw.arm7_read_16(0x0400_0204) & 0x80,
            0x80,
            "EXMEMSTAT shows it"
        );
    }

    #[test]
    fn sram_sits_on_an_8_bit_bus() {
        let mut hw = with_cartridge(gba_rom(b"SRAM_V"));
        hw.arm9_write_32(0x0A00_0002, 0xAABB_CCDD);
        assert_eq!(hw.arm9_read_8(0x0A00_0002), 0xBB);
        assert_eq!(hw.arm9_read_8(0x0A00_0003), 0xFF);
        assert_eq!(hw.arm9_read_32(0x0A00_0002), 0xBBBB_BBBB);
        hw.arm9_write_16(0x0A00_0005, 0x1234);
        assert_eq!(hw.arm9_read_16(0x0A00_0005), 0x1212);
        // 32 KiB of SRAM, repeating.
        assert_eq!(hw.arm9_read_8(0x0A00_8002), 0xBB);
        assert_eq!(hw.gba_slot.backup_generation(), Some(2));
    }

    /// Sends a Flash command through the DS bus.
    fn flash_command(hw: &mut Hardware, command: u8) {
        hw.arm9_write_8(0x0A00_5555, 0xAA);
        hw.arm9_write_8(0x0A00_2AAA, 0x55);
        hw.arm9_write_8(0x0A00_5555, command);
    }

    #[test]
    fn flash_128k_answers_through_the_slot() {
        let mut hw = with_cartridge(gba_rom(b"FLASH1M_V"));
        flash_command(&mut hw, 0x90);
        let id = (hw.arm9_read_8(0x0A00_0000), hw.arm9_read_8(0x0A00_0001));
        assert_eq!(id, (0xC2, 0x09), "Macronix 128 KiB");
        flash_command(&mut hw, 0xF0);
        // Program a byte in each bank.
        flash_command(&mut hw, 0xA0);
        hw.arm9_write_8(0x0A00_1234, 0x42);
        flash_command(&mut hw, 0xB0);
        hw.arm9_write_8(0x0A00_0000, 1);
        assert_eq!(hw.arm9_read_8(0x0A00_1234), 0xFF, "bank 1 is blank");
        flash_command(&mut hw, 0xA0);
        hw.arm9_write_8(0x0A00_1234, 0x24);
        flash_command(&mut hw, 0xB0);
        hw.arm9_write_8(0x0A00_0000, 0);
        assert_eq!(hw.arm9_read_8(0x0A00_1234), 0x42);
        let mut save = Vec::new();
        hw.gba_slot.save_backup(&mut save);
        assert_eq!(
            (save.len(), save[0x1234], save[0x1_1234]),
            (0x2_0000, 0x42, 0x24)
        );
        // Erase bank 0's 4 KiB sector holding it.
        flash_command(&mut hw, 0x80);
        hw.arm9_write_8(0x0A00_5555, 0xAA);
        hw.arm9_write_8(0x0A00_2AAA, 0x55);
        hw.arm9_write_8(0x0A00_1000, 0x30);
        assert_eq!(hw.arm9_read_8(0x0A00_1234), 0xFF);
    }

    #[test]
    fn an_empty_slot_reads_all_ones() {
        let mut hw = hardware();
        assert_eq!(hw.arm9_read_32(0x0800_0000), 0xFFFF_FFFF);
        assert_eq!(hw.arm9_read_16(0x09FF_FFFE), 0xFFFF);
        assert_eq!(hw.arm9_read_8(0x0A00_0000), 0xFF);
        assert_eq!(hw.arm9_read_32(0x0A00_0000), 0xFFFF_FFFF);
        hw.arm9_write_8(0x0A00_0000, 0);
        assert_eq!(hw.arm9_read_8(0x0A00_0000), 0xFF);
        assert_eq!(hw.gba_slot.backup_generation(), None);
    }

    fn arm9_wait(hw: &Hardware, address: u32, width: Width, access: Access) -> u64 {
        hw.arm9_wait(hw.arm9_place(address), width, access)
    }

    fn arm7_wait(hw: &Hardware, address: u32, width: Width, access: Access) -> u64 {
        hw.arm7_wait(hw.arm7_place(address), address, width, access)
    }

    #[test]
    fn gba_slot_wait_states_follow_each_cpus_exmem_bits() {
        let mut hw = with_cartridge(gba_rom(b""));
        let n = Access::NONSEQUENTIAL;
        let s = Access::SEQUENTIAL;
        // Power-on: 10-cycle first access, 6-cycle second, 10-cycle SRAM.
        assert_eq!(arm9_wait(&hw, 0x0800_0000, Width::Half, n), 20);
        assert_eq!(arm9_wait(&hw, 0x0800_0000, Width::Word, n), 32);
        assert_eq!(arm9_wait(&hw, 0x0A00_0000, Width::Word, n), 20);
        // 6/4 ROM and 18-cycle SRAM for the ARM9 only.
        hw.arm9_write_16(0x0400_0204, 0x0018 | 3);
        assert_eq!(arm9_wait(&hw, 0x0800_0000, Width::Word, s), 16);
        assert_eq!(arm9_wait(&hw, 0x0A00_0000, Width::Byte, n), 36);
        // Handed to the ARM7, which times it by its own bits.
        hw.arm9_write_16(0x0400_0204, 0x0080);
        hw.arm7_write_16(0x0400_0204, 0x000C);
        assert_eq!(arm7_wait(&hw, 0x0800_0000, Width::Half, n), 18);
        assert_eq!(arm7_wait(&hw, 0x0A00_0000, Width::Half, n), 10);
    }
}
