//! The two CPUs' buses: `emu_arm::Bus` views of the shared [`Hardware`],
//! each charging wait states to its own CPU's clock, and DMA, which runs
//! on its CPU's bus between instructions.

use emu_arm::{Access, Bus};

use crate::hardware::Hardware;
use crate::interrupt::irq;
use crate::ipc::Side;
use crate::machine::Arm9;
use crate::memory::{self, Width};

/// The ARM9's bus. TCM accesses never reach it; the CPU serves them.
pub(crate) struct Arm9Bus<'a>(pub(crate) &'a mut Hardware);

/// The ARM7's bus.
pub(crate) struct Arm7Bus<'a>(pub(crate) &'a mut Hardware);

/// The ARM9's bus tries main RAM through the caches first
/// ([`Hardware::arm9_cache_hit`]), then the memory map in these. An access
/// here may reach I/O and change what the machine checks before each
/// instruction, so it clears `CpuIo::checked`.
impl Arm9Bus<'_> {
    #[inline(never)]
    fn mapped_read_32(&mut self, address: u32, access: Access) -> u32 {
        let hw = &mut *self.0;
        let place = hw.arm9_place(address & !3);
        hw.arm9.checked = false;
        hw.arm9.cycles += hw.arm9_wait(place, Width::Word, access);
        hw.read_place_32(true, place, address)
    }

    #[inline(never)]
    fn mapped_read_16(&mut self, address: u32, access: Access) -> u16 {
        let hw = &mut *self.0;
        let place = hw.arm9_place(address & !1);
        hw.arm9.checked = false;
        hw.arm9.cycles += hw.arm9_wait(place, Width::Half, access);
        hw.read_place_16(true, place, address)
    }

    #[inline(never)]
    fn mapped_read_8(&mut self, address: u32, access: Access) -> u8 {
        let hw = &mut *self.0;
        let place = hw.arm9_place(address);
        hw.arm9.checked = false;
        hw.arm9.cycles += hw.arm9_wait(place, Width::Byte, access);
        hw.read_place_8(true, place, address)
    }

    #[inline(never)]
    fn mapped_write_32(&mut self, address: u32, value: u32, access: Access) {
        let hw = &mut *self.0;
        let place = hw.arm9_place(address & !3);
        hw.arm9.checked = false;
        hw.arm9.cycles += hw.arm9_wait(place, Width::Word, access);
        hw.write_place_32(true, place, address, value);
    }

    #[inline(never)]
    fn mapped_write_16(&mut self, address: u32, value: u16, access: Access) {
        let hw = &mut *self.0;
        let place = hw.arm9_place(address & !1);
        hw.arm9.checked = false;
        hw.arm9.cycles += hw.arm9_wait(place, Width::Half, access);
        hw.write_place_16(true, place, address, value);
    }

    #[inline(never)]
    fn mapped_write_8(&mut self, address: u32, value: u8, access: Access) {
        let hw = &mut *self.0;
        let place = hw.arm9_place(address);
        hw.arm9.checked = false;
        hw.arm9.cycles += hw.arm9_wait(place, Width::Byte, access);
        hw.write_place_8(true, place, address, value);
    }
}

impl Bus for Arm9Bus<'_> {
    #[inline]
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        match self.0.arm9_cache_hit(address & !3, access) {
            Some(index) => memory::word(&self.0.main_ram, index),
            None => self.mapped_read_32(address, access),
        }
    }

    #[inline]
    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        match self.0.arm9_cache_hit(address & !1, access) {
            Some(index) => memory::half(&self.0.main_ram, index),
            None => self.mapped_read_16(address, access),
        }
    }

    #[inline]
    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        match self.0.arm9_cache_hit(address, access) {
            Some(index) => self.0.main_ram[index],
            None => self.mapped_read_8(address, access),
        }
    }

    #[inline]
    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        match self.0.arm9_cache_hit(address & !3, access) {
            Some(index) => memory::put_word(&mut self.0.main_ram, index, value),
            None => self.mapped_write_32(address, value, access),
        }
    }

    #[inline]
    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        match self.0.arm9_cache_hit(address & !1, access) {
            Some(index) => memory::put_half(&mut self.0.main_ram, index, value),
            None => self.mapped_write_16(address, value, access),
        }
    }

    #[inline]
    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        match self.0.arm9_cache_hit(address, access) {
            Some(index) => self.0.main_ram[index] = value,
            None => self.mapped_write_8(address, value, access),
        }
    }

    #[inline]
    fn idle(&mut self) {
        self.0.arm9.cycles += 1;
    }
}

/// The ARM7's bus tries its working RAM first ([`Hardware::arm7_wram_hit`]),
/// then the rest of the memory map in these, which clear
/// `CpuIo::checked` as the ARM9's do.
impl Arm7Bus<'_> {
    #[inline(never)]
    fn mapped_read_32(&mut self, address: u32, access: Access) -> u32 {
        let hw = &mut *self.0;
        let place = hw.arm7_place(address & !3);
        hw.arm7.checked = false;
        hw.arm7.cycles += hw.arm7_wait(place, address, Width::Word, access);
        hw.read_place_32(false, place, address)
    }

    #[inline(never)]
    fn mapped_read_16(&mut self, address: u32, access: Access) -> u16 {
        let hw = &mut *self.0;
        let place = hw.arm7_place(address & !1);
        hw.arm7.checked = false;
        hw.arm7.cycles += hw.arm7_wait(place, address, Width::Half, access);
        hw.read_place_16(false, place, address)
    }

    #[inline(never)]
    fn mapped_read_8(&mut self, address: u32, access: Access) -> u8 {
        let hw = &mut *self.0;
        let place = hw.arm7_place(address);
        hw.arm7.checked = false;
        hw.arm7.cycles += hw.arm7_wait(place, address, Width::Byte, access);
        hw.read_place_8(false, place, address)
    }

    #[inline(never)]
    fn mapped_write_32(&mut self, address: u32, value: u32, access: Access) {
        let hw = &mut *self.0;
        let place = hw.arm7_place(address & !3);
        hw.arm7.checked = false;
        hw.arm7.cycles += hw.arm7_wait(place, address, Width::Word, access);
        hw.write_place_32(false, place, address, value);
    }

    #[inline(never)]
    fn mapped_write_16(&mut self, address: u32, value: u16, access: Access) {
        let hw = &mut *self.0;
        let place = hw.arm7_place(address & !1);
        hw.arm7.checked = false;
        hw.arm7.cycles += hw.arm7_wait(place, address, Width::Half, access);
        hw.write_place_16(false, place, address, value);
    }

    #[inline(never)]
    fn mapped_write_8(&mut self, address: u32, value: u8, access: Access) {
        let hw = &mut *self.0;
        let place = hw.arm7_place(address);
        hw.arm7.checked = false;
        hw.arm7.cycles += hw.arm7_wait(place, address, Width::Byte, access);
        hw.write_place_8(false, place, address, value);
    }
}

impl Bus for Arm7Bus<'_> {
    #[inline]
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        match self.0.arm7_wram_hit(address & !3) {
            Some((wram, index)) => memory::word(wram, index),
            None => self.mapped_read_32(address, access),
        }
    }

    #[inline]
    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        match self.0.arm7_wram_hit(address & !1) {
            Some((wram, index)) => memory::half(wram, index),
            None => self.mapped_read_16(address, access),
        }
    }

    #[inline]
    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        match self.0.arm7_wram_hit(address) {
            Some((wram, index)) => wram[index],
            None => self.mapped_read_8(address, access),
        }
    }

    #[inline]
    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        match self.0.arm7_wram_hit(address & !3) {
            Some((wram, index)) => memory::put_word(wram, index, value),
            None => self.mapped_write_32(address, value, access),
        }
    }

    #[inline]
    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        match self.0.arm7_wram_hit(address & !1) {
            Some((wram, index)) => memory::put_half(wram, index, value),
            None => self.mapped_write_16(address, value, access),
        }
    }

    #[inline]
    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        match self.0.arm7_wram_hit(address) {
            Some((wram, index)) => wram[index] = value,
            None => self.mapped_write_8(address, value, access),
        }
    }

    #[inline]
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
    ///
    /// The machine asks before every instruction and the answer is nearly
    /// always no, so the check is inlined and the copy is not.
    #[inline]
    pub(crate) fn run_dma(&mut self, side: Side) -> bool {
        let Some(index) = self.cpu(side).dma.next_pending() else {
            return false;
        };
        self.run_dma_channel(side, index);
        true
    }

    /// Runs DMA channel `index` of `side`'s, which is pending.
    #[inline(never)]
    fn run_dma_channel(&mut self, side: Side, index: usize) {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Place;

    /// Addresses across the memory map but I/O (whose reads have side
    /// effects), aligned and not, with distinct bytes in the RAMs so that a
    /// wrong index shows.
    fn addresses_and_hardware(wramcnt: u8) -> (Vec<u32>, Hardware) {
        let mut hw = Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into());
        hw.wramcnt = wramcnt;
        let pattern = |memory: &mut [u8], seed: usize| {
            for (index, byte) in memory.iter_mut().enumerate() {
                *byte = (index * 7 + index / 251 + seed) as u8;
            }
        };
        pattern(&mut hw.main_ram, 1);
        pattern(&mut hw.shared_wram, 2);
        pattern(&mut hw.arm7_wram, 3);
        let offsets = [
            0, 1, 2, 3, 0x1235, 0x3FFE, 0x4001, 0x7FFF, 0x8002, 0xFFFF, 0x7F_8000, 0x7F_FFFF,
            0x80_0001, 0x80_FFFE, 0x81_0003, 0xFF_0000, 0xFF_FFFD,
        ];
        let regions = (0x00..=0x0B)
            .chain([0x10, 0xFF])
            .filter(|&region| region != 0x04);
        let addresses = regions
            .flat_map(|region: u32| offsets.map(move |offset| region << 24 | offset))
            .collect();
        (addresses, hw)
    }

    const ACCESSES: [Access; 4] = [
        Access::NONSEQUENTIAL,
        Access::SEQUENTIAL,
        Access::CODE,
        Access::DMA,
    ];

    /// What an ARM7 read through the memory map gives: the value at
    /// `place` and its cycles.
    fn arm7_plain(hw: &mut Hardware, address: u32, width: Width) -> (u32, u64) {
        let aligned = match width {
            Width::Word => address & !3,
            Width::Half => address & !1,
            Width::Byte => address,
        };
        let place: Place = hw.arm7_place(aligned);
        let cycles = hw.arm7_wait(place, address, width, Access::NONSEQUENTIAL);
        let value = match width {
            Width::Word => hw.read_place_32(false, place, address),
            Width::Half => u32::from(hw.read_place_16(false, place, address)),
            Width::Byte => u32::from(hw.read_place_8(false, place, address)),
        };
        (value, cycles)
    }

    #[test]
    fn the_arm7_bus_reads_and_writes_as_its_memory_map_says() {
        for wramcnt in 0..4 {
            let (addresses, mut hw) = addresses_and_hardware(wramcnt);
            let (_, mut plain) = addresses_and_hardware(wramcnt);
            for &address in &addresses {
                let fast = |hw: &mut Hardware, width: Width| {
                    let before = hw.arm7.cycles;
                    let mut bus = Arm7Bus(hw);
                    let value = match width {
                        Width::Word => bus.read_32(address, Access::NONSEQUENTIAL),
                        Width::Half => u32::from(bus.read_16(address, Access::NONSEQUENTIAL)),
                        Width::Byte => u32::from(bus.read_8(address, Access::NONSEQUENTIAL)),
                    };
                    (value, hw.arm7.cycles - before)
                };
                for width in [Width::Word, Width::Half, Width::Byte] {
                    assert_eq!(
                        fast(&mut hw, width),
                        arm7_plain(&mut plain, address, width),
                        "WRAMCNT {wramcnt}, {address:08X}, {width:?}"
                    );
                }
                // Writes land where the map says, at the same cost.
                let value = address.rotate_left(7) ^ 0x5A5A_5A5A;
                let writes = [
                    (address, Width::Word),
                    (address.wrapping_add(5), Width::Half),
                    (address.wrapping_add(2), Width::Byte),
                ];
                for (at, width) in writes {
                    let before = hw.arm7.cycles;
                    let mut bus = Arm7Bus(&mut hw);
                    match width {
                        Width::Word => bus.write_32(at, value, Access::SEQUENTIAL),
                        Width::Half => bus.write_16(at, value as u16, Access::SEQUENTIAL),
                        Width::Byte => bus.write_8(at, value as u8, Access::SEQUENTIAL),
                    }
                    let cost = hw.arm7.cycles - before;
                    let aligned = match width {
                        Width::Word => at & !3,
                        Width::Half => at & !1,
                        Width::Byte => at,
                    };
                    let place = plain.arm7_place(aligned);
                    match width {
                        Width::Word => plain.write_place_32(false, place, at, value),
                        Width::Half => plain.write_place_16(false, place, at, value as u16),
                        Width::Byte => plain.write_place_8(false, place, at, value as u8),
                    }
                    let plain_cost = plain.arm7_wait(place, at, width, Access::SEQUENTIAL);
                    assert_eq!(cost, plain_cost, "{at:08X}, {width:?}");
                }
            }
            assert!(hw.main_ram == plain.main_ram);
            assert!(hw.shared_wram == plain.shared_wram);
            assert!(hw.arm7_wram == plain.arm7_wram);
        }
    }

    #[test]
    fn the_arm9_bus_reads_as_its_memory_map_says() {
        for wramcnt in 0..4 {
            let (addresses, mut hw) = addresses_and_hardware(wramcnt);
            for &address in &addresses {
                for access in ACCESSES {
                    let before = hw.arm9.cycles;
                    let fast = Arm9Bus(&mut hw).read_32(address, access);
                    let fast_cycles = hw.arm9.cycles - before;
                    let place = hw.arm9_place(address & !3);
                    let cycles = hw.arm9_wait(place, Width::Word, access);
                    let plain = hw.read_place_32(true, place, address);
                    assert_eq!((fast, fast_cycles), (plain, cycles), "{address:08X}");
                }
            }
        }
    }

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
