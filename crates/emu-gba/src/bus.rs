//! The GBA memory map (GBATEK, "GBA Memory Map", "GBA I/O Map"), cycle
//! counting and the event scheduler.
//!
//! Every CPU access adds the region's wait states to the cycle count and
//! advances the scheduler, which raises PPU line events (and, later, timer
//! and DMA events) at the exact cycle they are due.

use emu_arm::{Access, Bus as CpuBus};
use emu_sched::Scheduler;
use serde::{Deserialize, Serialize};

use crate::bios;
use crate::ppu::{self, Ppu};
use crate::timing::{WaitStates, Width};

/// Scheduled events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Event {
    /// The PPU reaches HBlank on the current line.
    HBlank,
    /// The PPU starts the next line.
    LineStart,
}

/// Interrupt request bits outside the PPU's.
pub(crate) mod irq {
    pub(crate) const KEYPAD: u16 = 1 << 12;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Bus {
    /// The BIOS: the HLE vector table or a user dump. Restored from media.
    #[serde(skip)]
    bios: Box<[u8]>,
    /// Game Pak ROM. Restored from media.
    #[serde(skip)]
    rom: Box<[u8]>,
    ewram: Box<[u8]>,
    iwram: Box<[u8]>,
    pub(crate) ppu: Ppu,
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
    pub(crate) fn new(rom: Box<[u8]>, bios: Box<[u8]>) -> Self {
        let mut scheduler = Scheduler::new();
        scheduler.schedule_at(ppu::HDRAW_CYCLES, Event::HBlank);
        let mut bus = Self {
            bios,
            rom,
            ewram: vec![0; 0x4_0000].into_boxed_slice(),
            iwram: vec![0; 0x8000].into_boxed_slice(),
            ppu: Ppu::new(),
            io: vec![0; 0x400].into_boxed_slice(),
            interrupt_enable: 0,
            interrupt_flag: 0,
            master_enable: false,
            waitcnt: 0,
            waits: WaitStates::new(0),
            keys: 0x03FF,
            halted: false,
            bios_open_bus: bios::OPEN_BUS_AFTER_BOOT,
            executing_bios: false,
            last_fetch: 0,
            scheduler,
            cycles: 0,
        };
        // RCNT and SOUNDBIAS as the BIOS leaves them.
        bus.write_io_16(0x134, 0x8000);
        bus.write_io_16(0x088, 0x0200);
        bus
    }

    /// Moves the ROM and BIOS out of `other`, which is about to be replaced
    /// by a loaded save state.
    pub(crate) fn take_media_from(&mut self, other: &mut Self) {
        self.rom = std::mem::take(&mut other.rom);
        self.bios = std::mem::take(&mut other.bios);
        self.waits = WaitStates::new(self.waitcnt);
    }

    pub(crate) const fn cycles(&self) -> u64 {
        self.cycles
    }

    pub(crate) fn halt(&mut self) {
        self.halted = true;
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
                    let (request, _signals) = self.ppu.start_hblank();
                    self.request_interrupt(request);
                    self.scheduler.schedule_at(
                        due.time + ppu::LINE_CYCLES - ppu::HDRAW_CYCLES,
                        Event::LineStart,
                    );
                }
                Event::LineStart => {
                    let (request, _signals) = self.ppu.start_line();
                    self.request_interrupt(request);
                    self.scheduler
                        .schedule_at(due.time + ppu::HDRAW_CYCLES, Event::HBlank);
                }
            }
        }
    }

    fn io_16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.io[offset], self.io[offset + 1]])
    }

    fn read_io_16(&self, offset: u32) -> u16 {
        let offset = (offset & 0x3FE) as usize;
        match offset {
            0x000..=0x056 => self.ppu.read_register(offset),
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

    fn read_16_raw(&self, address: u32) -> u16 {
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
                let index = (address & 0x01FF_FFFE) as usize;
                if index + 1 < self.rom.len() {
                    half(&self.rom, index)
                } else {
                    // Past the end, the cartridge bus returns the address.
                    (address >> 1) as u16
                }
            }
            0x0E | 0x0F => 0xFFFF,
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
            _ => {}
        }
    }

    fn read_8_raw(&self, address: u32) -> u8 {
        match address >> 24 {
            0x0E | 0x0F => 0xFF,
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
            _ => {}
        }
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

impl CpuBus for Bus {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
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
        let address = address & !3;
        self.tick(
            self.waits
                .cycles(address, Width::Word, access.contains(Access::SEQUENTIAL)),
        );
        self.write_16_raw(address, value as u16);
        self.write_16_raw(address | 2, (value >> 16) as u16);
    }

    fn write_16(&mut self, address: u32, value: u16, access: Access) {
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
