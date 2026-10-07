//! The GBA memory map (GBATEK, "GBA Memory Map", "GBA I/O Map"), cycle
//! counting and the event scheduler.
//!
//! Every CPU access adds the region's wait states to the cycle count and
//! advances the scheduler, which raises PPU line events (and, later, timer
//! and DMA events) at the exact cycle they are due.

use emu_arm::{Access, Bus as CpuBus};
use emu_core::RomPatch;
use emu_gba_cart::Cartridge;
use emu_sched::Scheduler;
use serde::{Deserialize, Serialize};

use crate::apu::{self, Apu};
use crate::bios;
use crate::dma::{Dma, Timing};
use crate::ppu::{self, Ppu};
use crate::sio::{self, Sio};
use crate::timer::Timers;
use crate::timing::{WaitStates, Width};

/// Scheduled events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Event {
    /// The PPU reaches HBlank on the current line.
    HBlank,
    /// The PPU starts the next line.
    LineStart,
    /// A timer counts past 0xFFFF.
    TimerOverflow(u8),
    /// Time for the next audio output sample.
    AudioSample,
    /// The serial transfer in progress ends.
    SerialDone,
}

/// Interrupt request bits outside the PPU's.
pub(crate) mod irq {
    /// Timer `n` overflowed: bit 3 + n.
    pub(crate) const TIMER0: u16 = 1 << 3;
    pub(crate) const SERIAL: u16 = 1 << 7;
    /// DMA channel `n` finished: bit 8 + n.
    pub(crate) const DMA0: u16 = 1 << 8;
    pub(crate) const KEYPAD: u16 = 1 << 12;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Bus {
    /// The BIOS: the HLE vector table or a user dump. Restored from media.
    #[serde(skip)]
    bios: Box<[u8]>,
    pub(crate) cartridge: Cartridge,
    /// Cheats' changes to what cartridge reads return, by offset into the
    /// 32 MiB window, so they cover every mirror. Like the ROM, never saved
    /// in states.
    #[serde(skip)]
    rom_patches: Box<[RomPatch]>,
    ewram: Box<[u8]>,
    iwram: Box<[u8]>,
    pub(crate) ppu: Ppu,
    dma: Dma,
    timers: Timers,
    pub(crate) apu: Apu,
    pub(crate) sio: Sio,
    /// Raw values written to I/O registers, for byte merges and reading back
    /// registers without side effects.
    io: Box<[u8]>,
    interrupt_enable: u16,
    interrupt_flag: u16,
    master_enable: bool,
    waitcnt: u16,
    #[serde(skip, default = "default_waits")]
    waits: WaitStates,
    /// KEYINPUT: buttons, active low.
    keys: u16,
    halted: bool,
    /// The SWI instruction of the VBlankIntrWait call the CPU halted in,
    /// which it repeats on waking. The repeat must keep the flag the IRQ
    /// handler just set.
    vblank_wait_swi: Option<u32>,
    /// The opcode the BIOS last fetched; BIOS reads from elsewhere see it.
    bios_open_bus: u32,
    executing_bios: bool,
    /// The last instruction fetched, which unmapped reads return.
    last_fetch: u32,
    scheduler: Scheduler<Event>,
    cycles: u64,
}

fn default_waits() -> WaitStates {
    WaitStates::new(0)
}

impl Bus {
    pub(crate) fn new(cartridge: Cartridge, bios: Box<[u8]>) -> Self {
        let mut scheduler = Scheduler::new();
        scheduler.schedule_at(ppu::HDRAW_CYCLES, Event::HBlank);
        scheduler.schedule_at(apu::CYCLES_PER_SAMPLE, Event::AudioSample);
        let mut bus = Self {
            bios,
            cartridge,
            rom_patches: Box::default(),
            ewram: vec![0; 0x4_0000].into_boxed_slice(),
            iwram: vec![0; 0x8000].into_boxed_slice(),
            ppu: Ppu::new(),
            dma: Dma::default(),
            timers: Timers::default(),
            apu: Apu::new(),
            sio: Sio::default(),
            io: vec![0; 0x400].into_boxed_slice(),
            interrupt_enable: 0,
            interrupt_flag: 0,
            master_enable: false,
            waitcnt: 0,
            waits: WaitStates::new(0),
            keys: 0x03FF,
            halted: false,
            vblank_wait_swi: None,
            bios_open_bus: bios::OPEN_BUS_AFTER_BOOT,
            executing_bios: false,
            last_fetch: 0,
            scheduler,
            cycles: 0,
        };
        // RCNT, SOUNDBIAS and the identity affine matrices, as the BIOS
        // leaves them.
        bus.write_io_16(0x134, 0x8000);
        bus.write_io_16(0x088, 0x0200);
        for offset in [0x20, 0x26, 0x30, 0x36] {
            bus.write_io_16(offset, 0x0100);
        }
        bus
    }

    /// A freshly powered-on bus for a console reset, taking the BIOS, the
    /// cartridge (save memory and clock included) and the ROM patches from
    /// `other`.
    pub(crate) fn reset_from(other: &mut Self) -> Self {
        let mut cartridge = std::mem::take(&mut other.cartridge);
        cartridge.restart_cycles(other.cycles);
        let mut bus = Self::new(cartridge, std::mem::take(&mut other.bios));
        bus.rom_patches = std::mem::take(&mut other.rom_patches);
        bus
    }

    /// Moves the ROM, its patches and the BIOS out of `other`, which is
    /// about to be replaced by a loaded save state.
    pub(crate) fn take_media_from(&mut self, other: &mut Self) {
        self.cartridge.take_rom_from(&mut other.cartridge);
        self.rom_patches = std::mem::take(&mut other.rom_patches);
        self.bios = std::mem::take(&mut other.bios);
        self.waits = WaitStates::new(self.waitcnt);
    }

    /// Replaces the ROM patches. Patches outside the cartridge's ROM window
    /// (0x08000000-0x0DFFFFFF) are dropped; the rest apply at their offset
    /// in all three wait-state mirrors.
    pub(crate) fn set_rom_patches(&mut self, patches: &[RomPatch]) {
        self.rom_patches = patches
            .iter()
            .filter(|patch| (0x0800_0000..0x0E00_0000).contains(&patch.address))
            .map(|patch| RomPatch {
                address: patch.address & 0x01FF_FFFF,
                ..*patch
            })
            .collect();
    }

    /// `value`, read from the cartridge at `address`, as the ROM patches
    /// change it. Cheat devices watched the cartridge bus, so they patch
    /// whatever answers there.
    fn patch_rom_16(&self, address: u32, value: u16) -> u16 {
        if self.rom_patches.is_empty() {
            return value;
        }
        let offset = address & 0x01FF_FFFE;
        let [low, high] = value.to_le_bytes();
        u16::from_le_bytes([
            RomPatch::apply(&self.rom_patches, offset, low),
            RomPatch::apply(&self.rom_patches, offset | 1, high),
        ])
    }

    pub(crate) const fn cycles(&self) -> u64 {
        self.cycles
    }

    pub(crate) fn halt(&mut self) {
        self.halted = true;
    }

    /// Notes that the VBlankIntrWait call made by the SWI at `swi` halted
    /// the CPU, which repeats the call once it wakes.
    pub(crate) fn remember_vblank_wait(&mut self, swi: u32) {
        self.vblank_wait_swi = Some(swi);
    }

    /// Whether the VBlankIntrWait call made by the SWI at `swi` repeats one
    /// that halted. Answers yes once per halt; calls from elsewhere leave
    /// the halted call's note alone.
    pub(crate) fn resumes_vblank_wait(&mut self, swi: u32) -> bool {
        let resumes = self.vblank_wait_swi == Some(swi);
        if resumes {
            self.vblank_wait_swi = None;
        }
        resumes
    }

    /// Forgets a halted VBlankIntrWait call that will never repeat.
    pub(crate) fn forget_vblank_wait(&mut self) {
        self.vblank_wait_swi = None;
    }

    pub(crate) fn set_bios_open_bus(&mut self, value: u32) {
        self.bios_open_bus = value;
    }

    /// Whether the CPU's IRQ input is high: an enabled interrupt is
    /// requested and IME is set.
    pub(crate) fn irq_line(&self) -> bool {
        self.master_enable && self.interrupt_enable & self.interrupt_flag != 0
    }

    /// Wakes from HALT when any enabled interrupt is requested, whatever
    /// IME says. Returns whether the CPU is running.
    pub(crate) fn wake_if_requested(&mut self) -> bool {
        if self.halted && self.interrupt_enable & self.interrupt_flag != 0 {
            self.halted = false;
        }
        !self.halted
    }

    /// While halted, skips straight to the next scheduled event.
    pub(crate) fn idle_to_next_event(&mut self) {
        let wait = self.scheduler.cycles_until_next().unwrap_or(1).max(1);
        self.tick(u32::try_from(wait).unwrap_or(u32::MAX));
    }

    pub(crate) fn request_interrupt(&mut self, bits: u16) {
        self.interrupt_flag |= bits;
    }

    /// Sets the held buttons (KEYINPUT bit order) and raises the keypad
    /// interrupt if KEYCNT asks for it.
    pub(crate) fn set_buttons(&mut self, pressed: u16) {
        self.keys = !pressed & 0x03FF;
        self.check_keypad_interrupt();
    }

    fn check_keypad_interrupt(&mut self) {
        let keycnt = self.io_16(0x132);
        if keycnt & (1 << 14) == 0 {
            return;
        }
        let selected = keycnt & 0x03FF;
        let pressed = !self.keys & 0x03FF & selected;
        let fire = if keycnt & (1 << 15) != 0 {
            pressed == selected
        } else {
            pressed != 0
        };
        if fire {
            self.request_interrupt(irq::KEYPAD);
        }
    }

    /// Advances time by `cycles` and runs every event that came due.
    fn tick(&mut self, cycles: u32) {
        self.cycles += u64::from(cycles);
        self.scheduler.advance(u64::from(cycles));
        while let Some(due) = self.scheduler.pop_due() {
            match due.event {
                Event::HBlank => {
                    let (request, signals) = self.ppu.start_hblank();
                    self.request_interrupt(request);
                    if signals.hblank {
                        self.dma.trigger(Timing::HBlank);
                    }
                    self.scheduler.schedule_at(
                        due.time + ppu::LINE_CYCLES - ppu::HDRAW_CYCLES,
                        Event::LineStart,
                    );
                }
                Event::LineStart => {
                    let (request, signals) = self.ppu.start_line();
                    self.request_interrupt(request);
                    if signals.vblank {
                        self.dma.trigger(Timing::VBlank);
                    }
                    self.scheduler
                        .schedule_at(due.time + ppu::HDRAW_CYCLES, Event::HBlank);
                }
                Event::TimerOverflow(index) => self.timer_overflow(usize::from(index), due.time),
                Event::AudioSample => {
                    self.apu.sample();
                    self.scheduler
                        .schedule_at(due.time + apu::CYCLES_PER_SAMPLE, Event::AudioSample);
                }
                Event::SerialDone => {
                    if self.sio.finish() {
                        self.request_interrupt(irq::SERIAL);
                    }
                }
            }
        }
    }

    fn timer_overflow(&mut self, index: usize, time: u64) {
        let overflowed = self.timers.overflow(index, time);
        for timer in (0..4).filter(|timer| overflowed & (1 << timer) != 0) {
            if self.timers.interrupt_enabled(timer) {
                self.request_interrupt(irq::TIMER0 << timer);
            }
            // Timers 0 and 1 clock the DMA sound FIFOs.
            if timer < 2 {
                let refill = self.apu.timer_overflow(timer);
                for fifo in (0..2).filter(|fifo| refill & (1 << fifo) != 0) {
                    self.dma.trigger_fifo(fifo);
                }
            }
        }
        self.schedule_timer(index);
    }

    /// Ends the serial transfer just started (by this console or, through
    /// the cable, by another) after `cycles`.
    pub(crate) fn schedule_serial_done(&mut self, cycles: u64) {
        self.scheduler
            .cancel(|pending| *pending == Event::SerialDone);
        self.scheduler
            .schedule_at(self.cycles + cycles, Event::SerialDone);
    }

    fn serial_action(&mut self, action: sio::Action) {
        match action {
            sio::Action::None => {}
            sio::Action::Started(cycles) => self.schedule_serial_done(cycles),
            sio::Action::Cancelled => {
                self.scheduler
                    .cancel(|pending| *pending == Event::SerialDone);
            }
        }
    }

    /// Replaces timer `index`'s overflow event after it was reconfigured.
    fn schedule_timer(&mut self, index: usize) {
        let event = Event::TimerOverflow(index as u8);
        self.scheduler.cancel(|pending| *pending == event);
        if let Some(time) = self.timers.overflow_time(index) {
            self.scheduler.schedule_at(time, event);
        }
    }

    /// Runs the highest-priority pending DMA transfer, if any. The CPU waits
    /// while it runs. Returns whether one ran.
    pub(crate) fn run_pending_dma(&mut self) -> bool {
        let Some(index) = self.dma.next_pending() else {
            return false;
        };
        let transfer = self.dma.transfer(index);
        let (mut source, mut destination) = (transfer.source, transfer.destination);
        if self.cartridge.eeprom_at(destination) {
            // The request's length tells the EEPROM's size.
            self.cartridge.note_eeprom_dma(transfer.count);
        }
        // Two internal cycles to start up.
        self.tick(2);
        for unit in 0..transfer.count {
            let order = if unit == 0 {
                Access::NONSEQUENTIAL
            } else {
                Access::SEQUENTIAL
            };
            let access = order | Access::DMA;
            if transfer.words {
                let value = self.read_32(source, access);
                self.write_32(destination, value, access);
            } else {
                let value = self.read_16(source, access);
                self.write_16(destination, value, access);
            }
            source = source.wrapping_add_signed(transfer.source_step);
            destination = destination.wrapping_add_signed(transfer.destination_step);
        }
        if self.dma.finish(index, source, destination) {
            self.request_interrupt(irq::DMA0 << index);
        }
        true
    }

    fn io_16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.io[offset], self.io[offset + 1]])
    }

    fn read_io_16(&self, offset: u32) -> u16 {
        let offset = (offset & 0x3FE) as usize;
        match offset {
            0x000..=0x056 => self.ppu.read_register(offset),
            0x060..=0x0A6 => {
                let offset = offset as u32 - 0x60;
                u16::from(self.apu.read_8(offset)) | u16::from(self.apu.read_8(offset + 1)) << 8
            }
            0x0B0..=0x0DE => self.dma.read(offset as u32 - 0xB0),
            0x100..=0x10E => self.timers.read(offset as u32 - 0x100, self.cycles),
            0x120..=0x12A | 0x134 => self.sio.read(offset as u32 - 0x120),
            0x130 => self.keys,
            0x200 => self.interrupt_enable,
            0x202 => self.interrupt_flag,
            0x204 => self.waitcnt,
            0x208 => u16::from(self.master_enable),
            0x300 => self.io_16(0x300) & 0x00FF,
            // Not yet emulated (sound, DMA, timers, serial): read back.
            _ => self.io_16(offset),
        }
    }

    fn write_io_16(&mut self, offset: u32, value: u16) {
        let offset = (offset & 0x3FE) as usize;
        if offset != 0x202 {
            self.io[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        match offset {
            0x000..=0x056 => self.ppu.write_register(offset, value),
            0x060..=0x0A6 => {
                let offset = offset as u32 - 0x60;
                self.apu.write_8(offset, value as u8);
                self.apu.write_8(offset + 1, (value >> 8) as u8);
            }
            0x0B0..=0x0DE => self.dma.write(offset as u32 - 0xB0, value),
            0x100..=0x10E => {
                if let Some(timer) = self.timers.write(offset as u32 - 0x100, value, self.cycles) {
                    self.schedule_timer(timer);
                }
            }
            0x120..=0x12A | 0x134 => {
                let action = self.sio.write(offset as u32 - 0x120, value);
                self.serial_action(action);
            }
            0x132 => self.check_keypad_interrupt(),
            0x200 => self.interrupt_enable = value & 0x3FFF,
            0x202 => self.interrupt_flag &= !value,
            0x204 => {
                self.waitcnt = value & 0x5FFF;
                self.waits = WaitStates::new(self.waitcnt);
            }
            0x208 => self.master_enable = value & 1 != 0,
            _ => {}
        }
    }

    fn write_io_8(&mut self, offset: u32, value: u8) {
        let offset = offset & 0x3FF;
        match offset {
            // IF: writing 1 clears; touch only this byte.
            0x202 => self.interrupt_flag &= !u16::from(value),
            0x203 => self.interrupt_flag &= !(u16::from(value) << 8),
            0x300 => self.io[0x300] = value,
            // Sound registers take single bytes without touching neighbours.
            0x060..=0x0A7 => self.apu.write_8(offset - 0x60, value),
            0x301 => {
                // HALTCNT: bit 7 clear halts, set stops (treated as halt).
                self.halted = true;
            }
            _ => {
                let aligned = offset & !1;
                let mut bytes = self.io_16(aligned as usize).to_le_bytes();
                bytes[(offset & 1) as usize] = value;
                self.write_io_16(aligned, u16::from_le_bytes(bytes));
            }
        }
    }

    fn open_bus_16(&self, address: u32) -> u16 {
        (self.last_fetch >> ((address & 2) * 8)) as u16
    }

    fn read_16_raw(&mut self, address: u32) -> u16 {
        let half =
            |memory: &[u8], index: usize| u16::from_le_bytes([memory[index], memory[index + 1]]);
        match address >> 24 {
            0x00 if address < 0x4000 => {
                if self.executing_bios {
                    half(&self.bios, (address & 0x3FFE) as usize)
                } else {
                    (self.bios_open_bus >> ((address & 2) * 8)) as u16
                }
            }
            0x02 => half(&self.ewram, (address & 0x3_FFFE) as usize),
            0x03 => half(&self.iwram, (address & 0x7FFE) as usize),
            0x04 if address & 0x00FF_FFFF < 0x400 => self.read_io_16(address),
            0x05 => self.ppu.read_palette(address),
            0x06 => self.ppu.read_vram(address),
            0x07 => self.ppu.read_oam(address),
            0x08..=0x0D => {
                let value = self.cartridge.read_16(address);
                self.patch_rom_16(address, value)
            }
            // An 8-bit bus: the byte repeats across the halfword.
            0x0E | 0x0F => u16::from(self.cartridge.read_backup(address)) * 0x0101,
            _ => self.open_bus_16(address),
        }
    }

    fn write_16_raw(&mut self, address: u32, value: u16) {
        let put = |memory: &mut [u8], index: usize| {
            memory[index..index + 2].copy_from_slice(&value.to_le_bytes());
        };
        match address >> 24 {
            0x02 => put(&mut self.ewram, (address & 0x3_FFFE) as usize),
            0x03 => put(&mut self.iwram, (address & 0x7FFE) as usize),
            0x04 if address & 0x00FF_FFFF < 0x400 => self.write_io_16(address, value),
            0x05 => self.ppu.write_palette(address, value),
            0x06 => self.ppu.write_vram(address, value),
            0x07 => self.ppu.write_oam(address, value),
            0x08..=0x0D => self.cartridge.write_16(address, value, self.cycles),
            // Only the byte lane at the address reaches the 8-bit bus.
            0x0E | 0x0F => self
                .cartridge
                .write_backup(address, (value >> ((address & 1) * 8)) as u8),
            _ => {}
        }
    }

    fn read_8_raw(&mut self, address: u32) -> u8 {
        match address >> 24 {
            0x0E | 0x0F => self.cartridge.read_backup(address),
            _ => (self.read_16_raw(address) >> ((address & 1) * 8)) as u8,
        }
    }

    fn write_8_raw(&mut self, address: u32, value: u8) {
        match address >> 24 {
            0x02 => self.ewram[(address & 0x3_FFFF) as usize] = value,
            0x03 => self.iwram[(address & 0x7FFF) as usize] = value,
            0x04 if address & 0x00FF_FFFF < 0x400 => self.write_io_8(address, value),
            // Palette takes the byte in both halves; OAM ignores byte writes.
            0x05 => self
                .ppu
                .write_palette(address, u16::from_le_bytes([value, value])),
            0x06 => self.ppu.write_vram_byte(address, value),
            0x0E | 0x0F => self.cartridge.write_backup(address, value),
            _ => {}
        }
    }

    /// Reads EWRAM or IWRAM, for cheats; `None` elsewhere.
    pub(crate) fn peek_ram(&self, address: u32) -> Option<u8> {
        match address >> 24 {
            0x02 => Some(self.ewram[(address & 0x3_FFFF) as usize]),
            0x03 => Some(self.iwram[(address & 0x7FFF) as usize]),
            _ => None,
        }
    }

    /// KEYINPUT (0x04000130–0x04000131), for cheats that test buttons;
    /// `None` elsewhere.
    pub(crate) fn peek_keys(&self, address: u32) -> Option<u8> {
        match address {
            0x0400_0130 => Some(self.keys as u8),
            0x0400_0131 => Some((self.keys >> 8) as u8),
            _ => None,
        }
    }

    /// Writes EWRAM or IWRAM, for cheats. Returns whether `address` is RAM.
    pub(crate) fn poke_ram(&mut self, address: u32, value: u8) -> bool {
        match address >> 24 {
            0x02 => self.ewram[(address & 0x3_FFFF) as usize] = value,
            0x03 => self.iwram[(address & 0x7FFF) as usize] = value,
            _ => return false,
        }
        true
    }

    /// Writes the I/O register halfword at `address` (0x04000000-0x040003FE)
    /// as a CPU store would, for cheats, without taking time. POSTFLG and
    /// HALTCNT refuse: halting the CPU from outside its program would
    /// stall the game. Returns whether the register was written.
    pub(crate) fn poke_io_16(&mut self, address: u32, value: u16) -> bool {
        let offset = address.wrapping_sub(0x0400_0000) & !1;
        if offset >= 0x400 || offset == 0x300 {
            return false;
        }
        self.write_io_16(offset, value);
        true
    }

    /// The byte at `address` as the CPU reads it, without side effects or
    /// time passing, for debuggers: the BIOS (readable here even while
    /// code runs elsewhere), RAM, I/O registers (reads of which change
    /// nothing on the GBA), palette RAM, VRAM, OAM, the ROM with its
    /// patches, and SRAM or Flash. `None` for the EEPROM and unmapped
    /// space.
    pub(crate) fn peek(&self, address: u32) -> Option<u8> {
        let byte = |half: u16| (half >> ((address & 1) * 8)) as u8;
        Some(match address >> 24 {
            0x00 if address < 0x4000 => self.bios[address as usize],
            0x02 | 0x03 => return self.peek_ram(address),
            0x04 if address & 0x00FF_FFFF < 0x400 => byte(self.read_io_16(address)),
            0x05 => byte(self.ppu.read_palette(address)),
            0x06 => byte(self.ppu.read_vram(address)),
            0x07 => byte(self.ppu.read_oam(address)),
            0x08..=0x0D if !self.cartridge.eeprom_at(address) => {
                let offset = (address & 0x01FF_FFFE) as usize;
                let rom = self.cartridge.rom();
                // Past the end, the cartridge bus returns the address.
                let value = rom
                    .get(offset..offset + 2)
                    .map_or((address >> 1) as u16, |bytes| {
                        u16::from_le_bytes([bytes[0], bytes[1]])
                    });
                byte(self.patch_rom_16(address, value))
            }
            0x0E | 0x0F => self.cartridge.read_backup(address),
            _ => return None,
        })
    }

    /// Writes the byte at `address` without side effects or time passing,
    /// for debuggers: EWRAM, IWRAM, palette RAM, VRAM and OAM, a byte at a
    /// time (unlike CPU byte stores to video memory). Returns whether
    /// `address` is one of those.
    pub(crate) fn poke_memory(&mut self, address: u32, value: u8) -> bool {
        let merge = |half: u16| {
            let shift = (address & 1) * 8;
            (half & !(0xFF << shift)) | u16::from(value) << shift
        };
        match address >> 24 {
            0x02 | 0x03 => return self.poke_ram(address, value),
            0x05 => {
                let half = merge(self.ppu.read_palette(address));
                self.ppu.write_palette(address, half);
            }
            0x06 => {
                let half = merge(self.ppu.read_vram(address));
                self.ppu.write_vram(address, half);
            }
            0x07 => {
                let half = merge(self.ppu.read_oam(address));
                self.ppu.write_oam(address, half);
            }
            _ => return false,
        }
        true
    }

    /// The event scheduler, for debuggers.
    pub(crate) fn scheduler(&self) -> &Scheduler<Event> {
        &self.scheduler
    }

    /// Whether the CPU is halted, waiting for an interrupt.
    pub(crate) fn halted(&self) -> bool {
        self.halted
    }

    /// Records an instruction fetch's value: the BIOS's last opcode for
    /// read protection, and the bus's open-bus value.
    fn note_fetch(&mut self, value: u32) {
        if self.executing_bios {
            self.bios_open_bus = value;
        }
        self.last_fetch = value;
    }
}

/// Whether `address` is in the SRAM/Flash region, which sits on an 8-bit
/// bus and sees unaligned addresses as they are.
fn is_save_region(address: u32) -> bool {
    matches!(address >> 24, 0x0E | 0x0F)
}

impl CpuBus for Bus {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        if is_save_region(address) {
            self.tick(self.waits.cycles(address, Width::Word, false));
            return u32::from(self.cartridge.read_backup(address)) * 0x0101_0101;
        }
        let address = address & !3;
        self.tick(
            self.waits
                .cycles(address, Width::Word, access.contains(Access::SEQUENTIAL)),
        );
        let code = access.contains(Access::CODE);
        if code {
            // Fetching from the BIOS is what unlocks reading it.
            self.executing_bios = address < 0x4000;
        }
        let value = if address < 0x4000 && !self.executing_bios {
            self.bios_open_bus
        } else {
            u32::from(self.read_16_raw(address)) | u32::from(self.read_16_raw(address | 2)) << 16
        };
        if code {
            self.note_fetch(value);
        }
        value
    }

    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        if is_save_region(address) {
            self.tick(self.waits.cycles(address, Width::Half, false));
            return u16::from(self.cartridge.read_backup(address)) * 0x0101;
        }
        let address = address & !1;
        self.tick(
            self.waits
                .cycles(address, Width::Half, access.contains(Access::SEQUENTIAL)),
        );
        let code = access.contains(Access::CODE);
        if code {
            self.executing_bios = address < 0x4000;
        }
        let value = self.read_16_raw(address);
        if code {
            // Thumb fetches leave the halfword in both halves of the bus.
            self.note_fetch(u32::from(value) * 0x0001_0001);
        }
        value
    }

    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        self.tick(
            self.waits
                .cycles(address, Width::Byte, access.contains(Access::SEQUENTIAL)),
        );
        self.read_8_raw(address)
    }

    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        if is_save_region(address) {
            // The byte lane at the (unaligned) address reaches the 8-bit bus.
            self.tick(self.waits.cycles(address, Width::Word, false));
            let byte = value.rotate_right((address & 3) * 8) as u8;
            self.cartridge.write_backup(address, byte);
            return;
        }
        let address = address & !3;
        self.tick(
            self.waits
                .cycles(address, Width::Word, access.contains(Access::SEQUENTIAL)),
        );
        self.write_16_raw(address, value as u16);
        self.write_16_raw(address | 2, (value >> 16) as u16);
    }

    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        if is_save_region(address) {
            self.tick(self.waits.cycles(address, Width::Half, false));
            let byte = (value >> ((address & 1) * 8)) as u8;
            self.cartridge.write_backup(address, byte);
            return;
        }
        let address = address & !1;
        self.tick(
            self.waits
                .cycles(address, Width::Half, access.contains(Access::SEQUENTIAL)),
        );
        self.write_16_raw(address, value);
    }

    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        self.tick(
            self.waits
                .cycles(address, Width::Byte, access.contains(Access::SEQUENTIAL)),
        );
        self.write_8_raw(address, value);
    }

    fn idle(&mut self) {
        self.tick(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eeprom_bus() -> Bus {
        let mut rom = vec![0; 0x400];
        rom[0x200..0x208].copy_from_slice(b"EEPROM_V");
        Bus::new(
            Cartridge::new(rom.into_boxed_slice(), None, 0),
            Box::default(),
        )
    }

    /// Runs DMA 3 copying `count` halfwords from `source` to `destination`.
    fn dma3(bus: &mut Bus, source: u32, destination: u32, count: u16) {
        bus.write_io_16(0xD4, source as u16);
        bus.write_io_16(0xD6, (source >> 16) as u16);
        bus.write_io_16(0xD8, destination as u16);
        bus.write_io_16(0xDA, (destination >> 16) as u16);
        bus.write_io_16(0xDC, count);
        bus.write_io_16(0xDE, 0x8000);
        while bus.run_pending_dma() {}
    }

    /// Stores `bits`, one per halfword, at 0x02000000.
    fn stage(bus: &mut Bus, bits: &[u16]) {
        for (index, bit) in (0u32..).zip(bits) {
            bus.write_16(0x0200_0000 + index * 2, *bit, Access::NONSEQUENTIAL);
        }
    }

    #[test]
    fn eeprom_round_trips_a_block_through_dma() {
        let mut bus = eeprom_bus();
        let data: Vec<u16> = (0..64).map(|bit| u16::from(bit % 3 == 0)).collect();
        // Write block 5: 1 0, a 6-bit address, 64 data bits, 0.
        let address = [0, 0, 0, 1, 0, 1];
        let write: Vec<u16> = [1, 0]
            .iter()
            .chain(&address)
            .chain(&data)
            .chain(&[0])
            .copied()
            .collect();
        stage(&mut bus, &write);
        dma3(&mut bus, 0x0200_0000, 0x0D00_0000, 73);
        assert_eq!(bus.cartridge.backup_generation(), Some(1));
        // Read it back: 1 1, the address, 0; then 4 junk bits and the data.
        let read: Vec<u16> = [1, 1].iter().chain(&address).chain(&[0]).copied().collect();
        stage(&mut bus, &read);
        dma3(&mut bus, 0x0200_0000, 0x0D00_0000, 9);
        dma3(&mut bus, 0x0D00_0000, 0x0200_1000, 68);
        let received: Vec<u16> = (0..68)
            .map(|index| bus.read_16(0x0200_1000 + index * 2, Access::NONSEQUENTIAL) & 1)
            .collect();
        assert_eq!(received[4..], data[..]);
    }

    #[test]
    fn sram_sees_one_byte_lane() {
        let mut rom = vec![0; 0x400];
        rom[0x200..0x206].copy_from_slice(b"SRAM_V");
        let mut bus = Bus::new(
            Cartridge::new(rom.into_boxed_slice(), None, 0),
            Box::default(),
        );
        bus.write_32(0x0E00_0002, 0xAABB_CCDD, Access::NONSEQUENTIAL);
        assert_eq!(bus.read_8(0x0E00_0002, Access::NONSEQUENTIAL), 0xBB);
        assert_eq!(bus.read_8(0x0E00_0003, Access::NONSEQUENTIAL), 0xFF);
        assert_eq!(bus.read_32(0x0E00_0002, Access::NONSEQUENTIAL), 0xBBBB_BBBB);
    }

    #[test]
    fn rom_patches_cover_every_mirror_and_width() {
        let rom: Vec<u8> = (0..0x400).map(|index| index as u8).collect();
        let mut bus = Bus::new(Cartridge::new(rom.into(), None, 0), Box::default());
        let patch = |address, value| RomPatch {
            address,
            value,
            compare: None,
        };
        bus.set_rom_patches(&[
            patch(0x0800_0100, 0xCD),
            patch(0x0800_0101, 0xAB),
            // Not the cartridge's ROM: dropped.
            patch(0x0200_0000, 0x11),
        ]);
        assert_eq!(bus.rom_patches.len(), 2);
        for mirror in [0x0800_0100, 0x0A00_0100, 0x0C00_0100] {
            assert_eq!(bus.read_16(mirror, Access::NONSEQUENTIAL), 0xABCD);
        }
        assert_eq!(bus.read_8(0x0800_0101, Access::NONSEQUENTIAL), 0xAB);
        assert_eq!(bus.read_32(0x0800_0100, Access::NONSEQUENTIAL), 0x0302_ABCD);
        assert_eq!(bus.read_16(0x0800_0102, Access::NONSEQUENTIAL), 0x0302);

        // States leave them out, but resets and loaded states keep them;
        // an empty list removes them.
        let mut bus = Bus::reset_from(&mut bus);
        let state = postcard::to_stdvec(&bus).expect("serializes");
        let mut loaded: Bus = postcard::from_bytes(&state).expect("deserializes");
        assert!(loaded.rom_patches.is_empty());
        loaded.take_media_from(&mut bus);
        assert_eq!(loaded.read_16(0x0800_0100, Access::NONSEQUENTIAL), 0xABCD);
        loaded.set_rom_patches(&[]);
        assert_eq!(loaded.read_16(0x0800_0100, Access::NONSEQUENTIAL), 0x0100);
    }

    #[test]
    fn cheat_io_writes_act_like_stores_but_never_halt() {
        let mut bus = Bus::new(
            Cartridge::new(vec![0; 0x400].into(), None, 0),
            Box::default(),
        );
        let cycles = bus.cycles();
        assert!(bus.poke_io_16(0x0400_0000, 0x0403));
        assert_eq!(bus.read_io_16(0x000), 0x0403);
        // IF: writing 1 clears the bit.
        bus.request_interrupt(0x0009);
        assert!(bus.poke_io_16(0x0400_0202, 0x0001));
        assert_eq!(bus.interrupt_flag, 0x0008);
        // POSTFLG and HALTCNT, and anything outside the I/O registers.
        for address in [0x0400_0300, 0x0400_0301, 0x0400_0400, 0x0200_0000] {
            assert!(!bus.poke_io_16(address, 0), "{address:08X}");
        }
        assert!(!bus.halted);
        assert_eq!(bus.cycles(), cycles);
    }
}
