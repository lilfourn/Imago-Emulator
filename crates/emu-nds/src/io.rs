//! Each CPU's I/O registers at 0x04000000 (GBATEK, "DS I/O Map").
//!
//! Registers are handled at their natural width: most are 16-bit, with
//! 32-bit accesses split in two; DMA, the maths unit and the IPC FIFO are
//! 32-bit; VRAMCNT, WRAMCNT, POSTFLG and HALTCNT are bytes. Registers
//! without side effects are kept raw so they read back what was written.

use crate::dma::Timing;
use crate::hardware::{Event, Hardware};
use crate::interrupt::irq;
use crate::ipc::Side;

/// Registers that live in the ARM9's or ARM7's 0x04000000 page.
const IO_PAGE: u32 = 0x1FFF;

impl Hardware {
    fn raw(&self, side: Side) -> &[u8] {
        match side {
            Side::Arm9 => &self.io9,
            Side::Arm7 => &self.io7,
        }
    }

    fn raw_mut(&mut self, side: Side) -> &mut [u8] {
        match side {
            Side::Arm9 => &mut self.io9,
            Side::Arm7 => &mut self.io7,
        }
    }

    fn raw_16(&self, side: Side, offset: u32) -> u16 {
        let index = (offset & IO_PAGE & !1) as usize;
        let raw = self.raw(side);
        u16::from_le_bytes([raw[index], raw[index + 1]])
    }

    fn set_raw_16(&mut self, side: Side, offset: u32, value: u16) {
        let index = (offset & IO_PAGE & !1) as usize;
        self.raw_mut(side)[index..index + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// The registers cheat codes test, read without side effects: the
    /// ARM7's timers (0x04000100–0x0400010F), KEYINPUT (0x04000130) and
    /// EXTKEYIN (0x04000136). The ARM7's, because the Action Replay runs
    /// there. `None` for any other address.
    pub(crate) fn peek_cheat_register(&self, address: u32) -> Option<u8> {
        let half = match address & !1 {
            0x0400_0100..=0x0400_010E => {
                let offset = (address & !1) - 0x0400_0100;
                self.arm7.timers.read(offset, self.now(Side::Arm7))
            }
            0x0400_0130 => self.keys,
            0x0400_0136 => self.ext_keys,
            _ => return None,
        };
        Some((half >> ((address & 1) * 8)) as u8)
    }

    /// The GBA-slot timing bits (EXMEMCNT bits 0-6) as `side` last wrote
    /// them: each CPU keeps its own.
    pub(crate) fn gba_slot_timing(&self, side: Side) -> u16 {
        match side {
            Side::Arm9 => self.exmemcnt & 0x7F,
            Side::Arm7 => self.raw_16(Side::Arm7, 0x204) & 0x7F,
        }
    }

    /// DISPSTAT as `side` reads it.
    fn dispstat(&self, side: Side) -> u16 {
        let stored = self.cpu(side).dispstat;
        let setting = (stored >> 8) | (stored & 0x80) << 1;
        let vblank = (192..262).contains(&self.vcount);
        u16::from(vblank)
            | u16::from(self.hblank) << 1
            | u16::from(self.vcount == setting) << 2
            | (stored & 0xFFB8)
    }

    /// Reads one of the registers both CPUs have at the same offset.
    fn read_common_16(&mut self, side: Side, offset: u32) -> Option<u16> {
        Some(match offset {
            0x004 => self.dispstat(side),
            0x006 => self.vcount,
            0x0B0..=0x0DF => {
                let value = self.cpu(side).dma.read((offset - 0xB0) & !3);
                (value >> ((offset & 2) * 8)) as u16
            }
            0x100..=0x10F => {
                let now = self.now(side);
                self.cpu(side).timers.read(offset - 0x100, now)
            }
            0x130 => self.keys,
            0x132 => self.cpu(side).keycnt,
            0x180 => self.ipc.read_sync(side),
            0x184 => self.ipc.read_control(side),
            0x208 => u16::from(self.cpu(side).interrupts.master_enable),
            0x210 => self.cpu(side).interrupts.enable as u16,
            0x212 => (self.cpu(side).interrupts.enable >> 16) as u16,
            0x214 => self.cpu(side).interrupts.flags as u16,
            0x216 => (self.cpu(side).interrupts.flags >> 16) as u16,
            // The game card belongs to one CPU at a time; the other reads
            // nothing.
            0x1A0..=0x1BB if self.cart_owner() != side => 0,
            0x1A0 => self.cart.spi_control(),
            0x1A2 => u16::from(self.cart.spi_data()),
            0x1A4 => self.cart.rom_control() as u16,
            0x1A6 => (self.cart.rom_control() >> 16) as u16,
            // Command bytes and encryption seeds are write-only.
            0x1A8..=0x1BB => 0,
            _ => return None,
        })
    }

    /// Writes one of the registers both CPUs have. Returns whether
    /// `offset` was one.
    fn write_common_16(&mut self, side: Side, offset: u32, value: u16) -> bool {
        match offset {
            0x004 => self.cpu_mut(side).dispstat = value & 0xFFB8,
            0x0B0..=0x0DF => {
                let register = (offset - 0xB0) & !3;
                let current = self.cpu(side).dma.read(register);
                let shift = (offset & 2) * 8;
                let merged = (current & !(0xFFFF << shift)) | u32::from(value) << shift;
                self.cpu_mut(side).dma.write(register, merged);
            }
            0x100..=0x10F => {
                let now = self.now(side);
                if let Some(timer) = self.cpu_mut(side).timers.write(offset - 0x100, value, now) {
                    self.schedule_timer(side, timer);
                }
            }
            0x132 => {
                self.cpu_mut(side).keycnt = value;
                self.check_keypad_interrupt();
            }
            0x180 => {
                let raised = self.ipc.write_sync(side, value);
                self.raise(raised);
            }
            0x184 => {
                let raised = self.ipc.write_control(side, value);
                self.raise(raised);
            }
            0x208 => self.cpu_mut(side).interrupts.master_enable = value & 1 != 0,
            0x210 => {
                let interrupts = &mut self.cpu_mut(side).interrupts;
                interrupts.enable = (interrupts.enable & !0xFFFF) | u32::from(value);
            }
            0x212 => {
                let interrupts = &mut self.cpu_mut(side).interrupts;
                interrupts.enable = (interrupts.enable & 0xFFFF) | u32::from(value) << 16;
            }
            0x214 => self.cpu_mut(side).interrupts.acknowledge(u32::from(value)),
            0x216 => self
                .cpu_mut(side)
                .interrupts
                .acknowledge(u32::from(value) << 16),
            0x1A0..=0x1BB if self.cart_owner() != side => {}
            0x1A0 => self.cart.write_spi_control(value),
            0x1A2 => {
                if let Some(cycles) = self.cart.write_spi_data(value as u8) {
                    self.schedule_after(side, cycles, Event::CartSpiDone);
                }
            }
            0x1A4 | 0x1A6 => {
                let shift = (offset & 2) * 8;
                let current = self.cart.rom_control();
                let merged = (current & !(0xFFFF << shift)) | u32::from(value) << shift;
                let event = self.cart.write_rom_control(merged);
                self.rom_event(event);
            }
            0x1A8..=0x1AF => {
                let index = (offset - 0x1A8) as usize;
                self.cart.write_command(index, value as u8);
                self.cart.write_command(index + 1, (value >> 8) as u8);
            }
            0x1B0..=0x1BB => {}
            _ => return false,
        }
        true
    }

    /// Raises the keypad interrupt on each CPU whose KEYCNT asks for it.
    pub(crate) fn check_keypad_interrupt(&mut self) {
        for side in [Side::Arm9, Side::Arm7] {
            let keycnt = self.cpu(side).keycnt;
            if keycnt & (1 << 14) == 0 {
                continue;
            }
            let selected = keycnt & 0x03FF;
            let pressed = !self.keys & 0x03FF & selected;
            let fire = if keycnt & (1 << 15) != 0 {
                pressed == selected
            } else {
                pressed != 0
            };
            if fire {
                self.cpu_mut(side).interrupts.request(irq::KEYPAD);
            }
        }
    }

    /// Reads a 32-bit register that is not two 16-bit halves.
    fn read_wide(&mut self, side: Side, address: u32) -> Option<u32> {
        match address {
            0x0410_0000 => {
                let (value, raised) = self.ipc.receive(side);
                self.raise(raised);
                Some(value)
            }
            0x0410_0010 if self.cart_owner() == side => {
                let (value, event) = self.cart.read_data();
                self.rom_event(event);
                Some(value)
            }
            0x0410_0010 => Some(0),
            0x0400_00B0..=0x0400_00DF => {
                Some(self.cpu(side).dma.read((address - 0x0400_00B0) & !3))
            }
            0x0400_00E0..=0x0400_00EF if side == Side::Arm9 => {
                Some(self.arm9.dma.fill[((address >> 2) & 3) as usize])
            }
            0x0400_0280..=0x0400_02BF if side == Side::Arm9 => {
                Some(self.math.read(address - 0x0400_0280))
            }
            0x0400_0060 | 0x0400_0320..=0x0400_06A3 if side == Side::Arm9 => {
                Some(self.gpu3d.read_register(address & 0xFFF))
            }
            _ => None,
        }
    }

    /// Writes a 32-bit register that is not two 16-bit halves. Returns
    /// whether `address` was one.
    fn write_wide(&mut self, side: Side, address: u32, value: u32) -> bool {
        match address {
            0x0400_0188 => {
                let raised = self.ipc.send(side, value);
                self.raise(raised);
            }
            0x0400_01A4 if self.cart_owner() == side => {
                let event = self.cart.write_rom_control(value);
                self.rom_event(event);
            }
            0x0400_00B0..=0x0400_00DF => {
                self.cpu_mut(side).dma.write(address - 0x0400_00B0, value);
            }
            0x0400_00E0..=0x0400_00EF if side == Side::Arm9 => {
                self.arm9.dma.fill[((address >> 2) & 3) as usize] = value;
            }
            0x0400_0280..=0x0400_02BF if side == Side::Arm9 => {
                self.math.write(address - 0x0400_0280, value);
            }
            0x0400_0060 | 0x0400_0320..=0x0400_06A3 if side == Side::Arm9 => {
                self.gpu3d.write_register(address & 0xFFF, value);
            }
            _ => return false,
        }
        true
    }

    // The ARM9.

    pub(crate) fn io9_read_32(&mut self, address: u32) -> u32 {
        if let Some(value) = self.read_wide(Side::Arm9, address) {
            return value;
        }
        u32::from(self.io9_read_16(address)) | u32::from(self.io9_read_16(address + 2)) << 16
    }

    pub(crate) fn io9_read_16(&mut self, address: u32) -> u16 {
        if address >= 0x0400_2000 {
            // The IPC FIFO and card data ports are words.
            let word = self.read_wide(Side::Arm9, address & !3).unwrap_or(0);
            return (word >> ((address & 2) * 8)) as u16;
        }
        let offset = address & IO_PAGE;
        if let Some(value) = self.read_common_16(Side::Arm9, offset) {
            return value;
        }
        match offset {
            0x060..=0x063 | 0x320..=0x6A3 => {
                (self.gpu3d.read_register(offset & !3) >> ((offset & 2) * 8)) as u16
            }
            0x000..=0x05F | 0x06C..=0x06F => self.engine_a.read_register(offset),
            0x1000..=0x106F => self.engine_b.read_register(offset - 0x1000),
            0x204 => self.exmemcnt,
            0x240..=0x249 => {
                u16::from(self.io9_read_8(address)) | u16::from(self.io9_read_8(address + 1)) << 8
            }
            0x280..=0x2BF => (self.math.read((offset - 0x280) & !3) >> ((offset & 2) * 8)) as u16,
            0x300 => u16::from(self.arm9.postflg),
            0x304 => self.powcnt1,
            _ => self.raw_16(Side::Arm9, offset),
        }
    }

    pub(crate) fn io9_read_8(&mut self, address: u32) -> u8 {
        let offset = address & IO_PAGE;
        match offset {
            0x240..=0x246 | 0x248 | 0x249 => {
                let bank = if offset < 0x247 {
                    offset - 0x240
                } else {
                    offset - 0x241
                };
                self.vram.control(bank as usize)
            }
            0x247 => self.wramcnt,
            _ => (self.io9_read_16(address & !1) >> ((address & 1) * 8)) as u8,
        }
    }

    pub(crate) fn io9_write_32(&mut self, address: u32, value: u32) {
        if self.write_wide(Side::Arm9, address, value) {
            return;
        }
        self.io9_write_16(address, value as u16);
        self.io9_write_16(address + 2, (value >> 16) as u16);
    }

    pub(crate) fn io9_write_16(&mut self, address: u32, value: u16) {
        if address >= 0x0400_2000 {
            return;
        }
        let offset = address & IO_PAGE;
        self.set_raw_16(Side::Arm9, offset, value);
        if self.write_common_16(Side::Arm9, offset, value) {
            return;
        }
        match offset {
            0x060..=0x063 | 0x320..=0x6A3 => self.gpu3d.write_register_16(offset, value),
            0x000..=0x05F | 0x06C..=0x06F => self.engine_a.write_register(offset, value),
            0x1000..=0x106F => self.engine_b.write_register(offset - 0x1000, value),
            0x204 => self.exmemcnt = value,
            0x240..=0x249 => {
                self.io9_write_8(address, value as u8);
                self.io9_write_8(address + 1, (value >> 8) as u8);
            }
            0x280..=0x2BF => {
                let register = (offset - 0x280) & !3;
                let shift = (offset & 2) * 8;
                let current = self.math.read(register);
                let merged = (current & !(0xFFFF << shift)) | u32::from(value) << shift;
                self.math.write(register, merged);
            }
            0x300 => self.arm9.postflg = (value & 3) as u8,
            0x304 => self.powcnt1 = value & 0x820F,
            _ => {}
        }
    }

    pub(crate) fn io9_write_8(&mut self, address: u32, value: u8) {
        let offset = address & IO_PAGE;
        match offset {
            0x240..=0x246 | 0x248 | 0x249 => {
                let bank = if offset < 0x247 {
                    offset - 0x240
                } else {
                    offset - 0x241
                };
                self.vram.set_control(bank as usize, value);
            }
            0x247 => self.wramcnt = value & 3,
            // IF: only this byte's ones clear flags.
            0x214..=0x217 => self
                .arm9
                .interrupts
                .acknowledge(u32::from(value) << ((offset - 0x214) * 8)),
            0x300 => self.arm9.postflg = value & 3,
            0x060..=0x063 | 0x320..=0x6A3 => self.gpu3d.write_register_8(offset, value),
            _ => {
                let mut bytes = self.raw_16(Side::Arm9, offset & !1).to_le_bytes();
                bytes[(offset & 1) as usize] = value;
                self.io9_write_16(address & !1, u16::from_le_bytes(bytes));
            }
        }
    }

    // The ARM7.

    pub(crate) fn io7_read_32(&mut self, address: u32) -> u32 {
        if let Some(value) = self.read_wide(Side::Arm7, address) {
            return value;
        }
        u32::from(self.io7_read_16(address)) | u32::from(self.io7_read_16(address + 2)) << 16
    }

    pub(crate) fn io7_read_16(&mut self, address: u32) -> u16 {
        if address >= 0x0400_2000 {
            let word = self.read_wide(Side::Arm7, address & !3).unwrap_or(0);
            return (word >> ((address & 2) * 8)) as u16;
        }
        let offset = address & IO_PAGE;
        if let Some(value) = self.read_common_16(Side::Arm7, offset) {
            return value;
        }
        match offset {
            0x400..=0x51F => (self.sound.read(offset & !3 & 0x1FF) >> ((offset & 2) * 8)) as u16,
            0x136 => self.ext_keys,
            0x138 => self.rtc.read(),
            0x1C0 => self.spi.control(),
            0x1C2 => u16::from(self.spi.data()),
            // EXMEMSTAT: the ARM7's GBA-slot timing bits, then the ARM9's
            // EXMEMCNT.
            0x204 => (self.raw_16(Side::Arm7, 0x204) & 0x7F) | (self.exmemcnt & 0xFF80),
            0x240 => u16::from(self.vram.arm7_status()) | u16::from(self.wramcnt) << 8,
            0x300 => u16::from(self.arm7.postflg),
            0x304 => self.powcnt2,
            _ => self.raw_16(Side::Arm7, offset),
        }
    }

    pub(crate) fn io7_read_8(&mut self, address: u32) -> u8 {
        (self.io7_read_16(address & !1) >> ((address & 1) * 8)) as u8
    }

    pub(crate) fn io7_write_32(&mut self, address: u32, value: u32) {
        if self.write_wide(Side::Arm7, address, value) {
            return;
        }
        self.io7_write_16(address, value as u16);
        self.io7_write_16(address + 2, (value >> 16) as u16);
    }

    pub(crate) fn io7_write_16(&mut self, address: u32, value: u16) {
        if address >= 0x0400_2000 {
            return;
        }
        let offset = address & IO_PAGE;
        if offset == 0x300 {
            self.io7_write_8(address, value as u8);
            self.io7_write_8(address + 1, (value >> 8) as u8);
            return;
        }
        self.set_raw_16(Side::Arm7, offset, value);
        if self.write_common_16(Side::Arm7, offset, value) {
            return;
        }
        match offset {
            0x304 => self.powcnt2 = value & 3,
            0x138 => {
                let now = self.now(Side::Arm7);
                self.rtc.write(value, now);
            }
            0x1C0 => self.spi.write_control(value),
            0x1C2 => {
                let now = self.now(Side::Arm7);
                if let Some(cycles) = self.spi.write_data(value as u8, now) {
                    self.schedule_after(Side::Arm7, cycles, Event::SpiDone);
                }
            }
            0x400..=0x51F => {
                let shift = (offset & 2) * 8;
                self.sound.write(
                    offset & !3 & 0x1FF,
                    u32::from(value) << shift,
                    0xFFFF << shift,
                );
            }
            _ => {}
        }
    }

    pub(crate) fn io7_write_8(&mut self, address: u32, value: u8) {
        let offset = address & IO_PAGE;
        match offset {
            0x214..=0x217 => self
                .arm7
                .interrupts
                .acknowledge(u32::from(value) << ((offset - 0x214) * 8)),
            // POSTFLG can only be set, and only from the BIOS.
            0x300 => self.arm7.postflg |= value & 1,
            // Sound registers take single bytes without touching neighbours.
            0x400..=0x51F => {
                let shift = (offset & 3) * 8;
                self.sound.write(
                    offset & !3 & 0x1FF,
                    u32::from(value) << shift,
                    0xFF << shift,
                );
            }
            // HALTCNT: 2 halts, 3 sleeps (treated as halting).
            0x301 => {
                if value >> 6 >= 2 {
                    self.arm7.halted = true;
                }
            }
            _ => {
                let mut bytes = self.raw_16(Side::Arm7, offset & !1).to_le_bytes();
                bytes[(offset & 1) as usize] = value;
                self.io7_write_16(address & !1, u16::from_le_bytes(bytes));
            }
        }
    }

    /// Starts DMA channels on both CPUs waiting for `timing`.
    pub(crate) fn trigger_dma(&mut self, timing: Timing) {
        self.arm9.dma.trigger(timing);
        self.arm7.dma.trigger(timing);
    }
}

#[cfg(test)]
mod tests {
    use crate::hardware::Hardware;
    use crate::interrupt::irq;

    fn hardware() -> Hardware {
        Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into())
    }

    #[test]
    fn ipc_words_cross_between_the_cpus() {
        let mut hw = hardware();
        hw.arm9_write_16(0x0400_0184, 0x8000);
        hw.arm7_write_16(0x0400_0184, 0x8400); // receive-not-empty IRQ
        hw.arm9_write_32(0x0400_0188, 0xCAFE_F00D);
        assert_ne!(hw.arm7.interrupts.flags & irq::IPC_RECEIVE_NOT_EMPTY, 0);
        assert_eq!(hw.arm7_read_32(0x0410_0000), 0xCAFE_F00D);
    }

    #[test]
    fn interrupt_flags_clear_by_writing_ones() {
        let mut hw = hardware();
        hw.arm9.interrupts.request(irq::VBLANK | irq::TIMER0);
        hw.arm9_write_8(0x0400_0214, irq::VBLANK as u8);
        assert_eq!(hw.arm9.interrupts.flags, irq::TIMER0);
        hw.arm9_write_32(0x0400_0210, 0x0001_0008);
        assert_eq!(hw.arm9_read_32(0x0400_0210), 0x0001_0008);
    }

    #[test]
    fn vramcnt_and_wramcnt_are_bytes() {
        let mut hw = hardware();
        hw.arm9_write_8(0x0400_0242, 0x82); // C to the ARM7
        hw.arm9_write_8(0x0400_0247, 3);
        assert_eq!(hw.arm7_read_16(0x0400_0240), 0x0301);
        hw.arm9_write_16(0x0400_0248, 0x8281); // H and I
        assert_eq!(hw.vram.control(7), 0x81);
        assert_eq!(hw.vram.control(8), 0x82);
    }

    #[test]
    fn dma_control_halves_merge() {
        let mut hw = hardware();
        hw.arm9_write_16(0x0400_00B8, 4); // count
        hw.arm9_write_16(0x0400_00BA, 0x8400); // enable, words
        assert_eq!(hw.arm9_read_32(0x0400_00B8), 0x8400_0004);
        assert_eq!(hw.arm9.dma.next_pending(), Some(0));
    }

    #[test]
    fn the_maths_unit_takes_halfword_control_writes() {
        let mut hw = hardware();
        hw.arm9_write_16(0x0400_02B0, 1); // SQRTCNT: 64-bit input
        hw.arm9_write_32(0x0400_02B8, 0);
        hw.arm9_write_32(0x0400_02BC, 1);
        assert_eq!(hw.arm9_read_32(0x0400_02B4), 65_536);
        assert_eq!(hw.arm9_read_16(0x0400_02B0), 1);
    }

    #[test]
    fn the_maths_unit_answers_through_io() {
        let mut hw = hardware();
        hw.arm9_write_32(0x0400_0290, 100);
        hw.arm9_write_32(0x0400_0298, 7);
        assert_eq!(hw.arm9_read_32(0x0400_02A0), 14);
        assert_eq!(
            hw.arm7_read_32(0x0400_02A0),
            0,
            "the ARM7 has no maths unit"
        );
    }

    #[test]
    fn halting_the_arm7_through_haltcnt() {
        let mut hw = hardware();
        hw.arm7_write_8(0x0400_0301, 0x80);
        assert!(hw.arm7.halted);
    }
}
