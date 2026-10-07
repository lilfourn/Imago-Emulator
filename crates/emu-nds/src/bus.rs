//! The two CPUs' buses: `emu_arm::Bus` views of the shared [`Hardware`],
//! each charging wait states to its own CPU's clock, and DMA, which runs
//! on its CPU's bus between instructions.

use emu_arm::{Access, Bus};

use crate::hardware::Hardware;
use crate::interrupt::irq;
use crate::ipc::Side;
use crate::machine::Arm9;
use crate::memory::Width;

/// The ARM9's bus. TCM accesses never reach it; the CPU serves them.
pub(crate) struct Arm9Bus<'a>(pub(crate) &'a mut Hardware);

/// The ARM7's bus.
pub(crate) struct Arm7Bus<'a>(pub(crate) &'a mut Hardware);

impl Bus for Arm9Bus<'_> {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        self.0.arm9.cycles += self.0.arm9_wait(address, Width::Word, access);
        self.0.arm9_read_32(address)
    }

    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        self.0.arm9.cycles += self.0.arm9_wait(address, Width::Half, access);
        self.0.arm9_read_16(address)
    }

    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        self.0.arm9.cycles += self.0.arm9_wait(address, Width::Byte, access);
        self.0.arm9_read_8(address)
    }

    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        self.0.arm9.cycles += self.0.arm9_wait(address, Width::Word, access);
        self.0.arm9_write_32(address, value);
    }

    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        self.0.arm9.cycles += self.0.arm9_wait(address, Width::Half, access);
        self.0.arm9_write_16(address, value);
    }

    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        self.0.arm9.cycles += self.0.arm9_wait(address, Width::Byte, access);
        self.0.arm9_write_8(address, value);
    }

    fn idle(&mut self) {
        self.0.arm9.cycles += 1;
    }
}

impl Bus for Arm7Bus<'_> {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        self.0.arm7.cycles += self.0.arm7_wait(address, Width::Word, access);
        self.0.arm7_read_32(address)
    }

    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        self.0.arm7.cycles += self.0.arm7_wait(address, Width::Half, access);
        self.0.arm7_read_16(address)
    }

    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        self.0.arm7.cycles += self.0.arm7_wait(address, Width::Byte, access);
        self.0.arm7_read_8(address)
    }

    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        self.0.arm7.cycles += self.0.arm7_wait(address, Width::Word, access);
        self.0.arm7_write_32(address, value);
    }

    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        self.0.arm7.cycles += self.0.arm7_wait(address, Width::Half, access);
        self.0.arm7_write_16(address, value);
    }

    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        self.0.arm7.cycles += self.0.arm7_wait(address, Width::Byte, access);
        self.0.arm7_write_8(address, value);
    }

    fn idle(&mut self) {
        self.0.arm7.cycles += 1;
    }
}

/// A CPU's bus that can also halt and stall the CPU, for BIOS calls.
pub(crate) trait CpuBus: Bus {
    fn halt(&mut self);
    fn stall(&mut self, cycles: u64);
}

impl CpuBus for Arm9Bus<'_> {
    fn halt(&mut self) {
        self.0.arm9.halted = true;
    }
    fn stall(&mut self, cycles: u64) {
        self.0.arm9.cycles += cycles;
    }
}

impl CpuBus for Arm7Bus<'_> {
    fn halt(&mut self) {
        self.0.arm7.halted = true;
    }
    fn stall(&mut self, cycles: u64) {
        self.0.arm7.cycles += cycles;
    }
}

/// Memory as the ARM9's own code sees it, TCM included, for the BIOS
/// calls the machine runs outside the CPU. TCM accesses take a cycle.
///
/// The protection unit does not check these accesses. The real BIOS runs
/// privileged and would abort on an address the game's regions forbid,
/// but games pass the BIOS only memory they can use themselves. Like DMA,
/// which the protection unit never sees, the emulated calls just go ahead.
pub(crate) struct Arm9TcmBus<'a, 'b> {
    pub(crate) cpu: &'a mut Arm9,
    pub(crate) bus: Arm9Bus<'b>,
}

impl Arm9TcmBus<'_, '_> {
    /// `N` bytes from TCM at `address`, if it is TCM.
    fn tcm_read<const N: usize>(&mut self, address: u32) -> Option<[u8; N]> {
        let first = self.cpu.tcm_read_8(address)?;
        let mut bytes = [first; N];
        for (offset, byte) in (0u32..).zip(bytes.iter_mut()).skip(1) {
            *byte = self.cpu.tcm_read_8(address + offset).unwrap_or(0);
        }
        self.bus.idle();
        Some(bytes)
    }

    /// Writes `bytes` to TCM at `address`; returns whether it is TCM.
    fn tcm_write(&mut self, address: u32, bytes: &[u8]) -> bool {
        if !self.cpu.tcm_write_8(address, bytes[0]) {
            return false;
        }
        for (offset, byte) in (0u32..).zip(bytes).skip(1) {
            self.cpu.tcm_write_8(address + offset, *byte);
        }
        self.bus.idle();
        true
    }
}

impl Bus for Arm9TcmBus<'_, '_> {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        let address = address & !3;
        match self.tcm_read::<4>(address) {
            Some(bytes) => u32::from_le_bytes(bytes),
            None => self.bus.read_32(address, access),
        }
    }

    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        let address = address & !1;
        match self.tcm_read::<2>(address) {
            Some(bytes) => u16::from_le_bytes(bytes),
            None => self.bus.read_16(address, access),
        }
    }

    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        match self.tcm_read::<1>(address) {
            Some([byte]) => byte,
            None => self.bus.read_8(address, access),
        }
    }

    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        let address = address & !3;
        if !self.tcm_write(address, &value.to_le_bytes()) {
            self.bus.write_32(address, value, access);
        }
    }

    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        let address = address & !1;
        if !self.tcm_write(address, &value.to_le_bytes()) {
            self.bus.write_16(address, value, access);
        }
    }

    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        if !self.tcm_write(address, &[value]) {
            self.bus.write_8(address, value, access);
        }
    }

    fn idle(&mut self) {
        self.bus.idle();
    }
}

impl CpuBus for Arm9TcmBus<'_, '_> {
    fn halt(&mut self) {
        self.bus.halt();
    }
    fn stall(&mut self, cycles: u64) {
        self.bus.stall(cycles);
    }
}

impl Hardware {
    /// Runs `side`'s highest-priority pending DMA channel, if any, on that
    /// CPU's bus. Returns whether one ran.
    pub(crate) fn run_dma(&mut self, side: Side) -> bool {
        let Some(index) = self.cpu(side).dma.next_pending() else {
            return false;
        };
        let transfer = self.cpu(side).dma.transfer(index);
        let (mut source, mut destination) = (transfer.source, transfer.destination);
        let access = |unit: u32| {
            let order = if unit == 0 {
                Access::NONSEQUENTIAL
            } else {
                Access::SEQUENTIAL
            };
            order | Access::DMA
        };
        match side {
            Side::Arm9 => {
                let mut bus = Arm9Bus(self);
                for unit in 0..transfer.count {
                    if transfer.words {
                        let value = bus.read_32(source, access(unit));
                        bus.write_32(destination, value, access(unit));
                    } else {
                        let value = bus.read_16(source, access(unit));
                        bus.write_16(destination, value, access(unit));
                    }
                    source = source.wrapping_add_signed(transfer.source_step);
                    destination = destination.wrapping_add_signed(transfer.destination_step);
                }
            }
            Side::Arm7 => {
                let mut bus = Arm7Bus(self);
                for unit in 0..transfer.count {
                    if transfer.words {
                        let value = bus.read_32(source, access(unit));
                        bus.write_32(destination, value, access(unit));
                    } else {
                        let value = bus.read_16(source, access(unit));
                        bus.write_16(destination, value, access(unit));
                    }
                    source = source.wrapping_add_signed(transfer.source_step);
                    destination = destination.wrapping_add_signed(transfer.destination_step);
                }
            }
        }
        let cpu = self.cpu_mut(side);
        if cpu.dma.finish(index, transfer.count, source, destination) {
            cpu.interrupts.request(irq::DMA0 << index);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_copies_on_its_own_cpu_and_charges_its_clock() {
        let mut hw = Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into());
        for index in 0..4u32 {
            hw.arm7_write_32(0x0380_0000 + index * 4, index + 1);
        }
        hw.arm7_write_32(0x0400_00B0, 0x0380_0000);
        hw.arm7_write_32(0x0400_00B4, 0x0200_0000);
        hw.arm7_write_32(0x0400_00B8, 0x8400_0004 | 1 << 30);
        let before = hw.arm7.cycles;
        assert!(hw.run_dma(Side::Arm7));
        assert_eq!(hw.arm9_read_32(0x0200_000C), 4);
        assert!(hw.arm7.cycles > before);
        assert_eq!(hw.arm9.cycles, 0);
        assert_ne!(hw.arm7.interrupts.flags & irq::DMA0, 0);
    }
}
