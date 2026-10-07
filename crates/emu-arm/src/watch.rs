//! A bus wrapper for debuggers' watchpoints.

use crate::{Access, Bus};

/// A [`Bus`] that reports every data access the CPU makes to `report`,
/// for debuggers' watchpoints: loads and stores on the bus, and those an
/// ARM9 serves from its tightly coupled memory. Instruction fetches and
/// idle cycles pass through unreported, and so do DMA accesses, which the
/// CPU never makes.
///
/// `report` gets the address as the instruction computed it, the width in
/// bytes (1, 2 or 4), whether it was a store, and the value loaded or
/// stored.
#[derive(Debug)]
pub struct Watched<'a, B, F> {
    bus: &'a mut B,
    report: F,
}

impl<'a, B: Bus, F: FnMut(u32, u32, bool, u32)> Watched<'a, B, F> {
    /// Watches the CPU's accesses to `bus`.
    pub fn new(bus: &'a mut B, report: F) -> Self {
        Self { bus, report }
    }

    fn data(&mut self, access: Access, address: u32, bytes: u32, write: bool, value: u32) {
        if !access.contains(Access::CODE) && !access.contains(Access::DMA) {
            (self.report)(address, bytes, write, value);
        }
    }
}

impl<B: Bus, F: FnMut(u32, u32, bool, u32)> Bus for Watched<'_, B, F> {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        let value = self.bus.read_32(address, access);
        self.data(access, address, 4, false, value);
        value
    }

    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        let value = self.bus.read_16(address, access);
        self.data(access, address, 2, false, value.into());
        value
    }

    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        let value = self.bus.read_8(address, access);
        self.data(access, address, 1, false, value.into());
        value
    }

    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        self.bus.write_32(address, value, access);
        self.data(access, address, 4, true, value);
    }

    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        self.bus.write_16(address, value, access);
        self.data(access, address, 2, true, value.into());
    }

    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        self.bus.write_8(address, value, access);
        self.data(access, address, 1, true, value.into());
    }

    fn idle(&mut self) {
        self.bus.idle();
    }

    fn tcm_data(&mut self, address: u32, bytes: u32, write: bool, value: u32) {
        (self.report)(address, bytes, write, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Arm9, Cpu, cp15};

    /// 64 KiB of RAM at address 0.
    struct Ram(Vec<u8>);

    impl Bus for Ram {
        fn read_32(&mut self, address: u32, _: Access) -> u32 {
            let index = (address & 0xFFFC) as usize;
            u32::from_le_bytes(self.0[index..index + 4].try_into().expect("4 bytes"))
        }
        fn read_16(&mut self, address: u32, _: Access) -> u16 {
            let index = (address & 0xFFFE) as usize;
            u16::from_le_bytes([self.0[index], self.0[index + 1]])
        }
        fn read_8(&mut self, address: u32, _: Access) -> u8 {
            self.0[(address & 0xFFFF) as usize]
        }
        fn write_32(&mut self, address: u32, value: u32, _: Access) {
            let index = (address & 0xFFFC) as usize;
            self.0[index..index + 4].copy_from_slice(&value.to_le_bytes());
        }
        fn write_16(&mut self, address: u32, value: u16, _: Access) {
            let index = (address & 0xFFFE) as usize;
            self.0[index..index + 2].copy_from_slice(&value.to_le_bytes());
        }
        fn write_8(&mut self, address: u32, value: u8, _: Access) {
            self.0[(address & 0xFFFF) as usize] = value;
        }
        fn idle(&mut self) {}
    }

    /// RAM holding `program` (ARM words) at 0.
    fn ram(program: &[u32]) -> Ram {
        let mut ram = Ram(vec![0; 0x1_0000]);
        for (index, word) in program.iter().enumerate() {
            ram.write_32(index as u32 * 4, *word, Access::NONSEQUENTIAL);
        }
        ram
    }

    #[test]
    fn data_accesses_are_reported_and_fetches_are_not() {
        // MOV R0, #0x100; MOV R1, #7; STRB R1, [R0]; LDR R2, [R0]
        let mut bus = ram(&[0xE3A0_0C01, 0xE3A0_1007, 0xE5C0_1000, 0xE590_2000]);
        let mut cpu = Cpu::default();
        cpu.jump(&mut bus, 0);
        let mut seen = Vec::new();
        for _ in 0..4 {
            cpu.step(&mut Watched::new(
                &mut bus,
                |address, bytes, write, value| {
                    seen.push((address, bytes, write, value));
                },
            ));
        }
        assert_eq!(seen, [(0x100, 1, true, 7), (0x100, 4, false, 7)]);
    }

    #[test]
    fn tcm_accesses_are_reported_too() {
        // MOV R0, #0x100; MOV R1, #7; STR R1, [R0], with ITCM at 0.
        let mut bus = ram(&[0xE3A0_0C01, 0xE3A0_1007, 0xE580_1000]);
        let mut cpu = Arm9::default();
        let cp15 = cpu.cp15_mut();
        cp15.set_control(cp15::control::RESET | cp15::control::ITCM_ENABLE);
        cp15.set_itcm_region(0x0C);
        for (offset, byte) in bus.0[..12].iter().enumerate() {
            cpu.tcm_write_8(offset as u32, *byte);
        }
        cpu.jump(&mut bus, 0);
        let mut seen = Vec::new();
        for _ in 0..3 {
            cpu.step(&mut Watched::new(
                &mut bus,
                |address, bytes, write, value| {
                    seen.push((address, bytes, write, value));
                },
            ));
        }
        assert_eq!(seen, [(0x100, 4, true, 7)]);
        assert_eq!(cpu.tcm_read_8(0x100), Some(7), "the store went to ITCM");
        assert_eq!(bus.0[0x100], 0, "and not to the bus");
    }
}
