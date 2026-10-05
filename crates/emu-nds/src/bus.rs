//! The two CPUs' buses: `emu_arm::Bus` views of the shared [`Hardware`],
//! each charging wait states to its own CPU's clock, and DMA, which runs
//! on its CPU's bus between instructions.

use emu_arm::{Access, Bus};

use crate::hardware::Hardware;
use crate::interrupt::irq;
use crate::ipc::Side;
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
