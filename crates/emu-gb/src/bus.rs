//! The memory map (Pan Docs, "Memory Map", "I/O Ranges") and the clock that
//! drives every component one M-cycle at a time.

use serde::{Deserialize, Serialize};

use crate::apu::Apu;
use crate::cartridge::Cartridge;
use crate::cpu;
use crate::joypad::Joypad;
use crate::ppu::Ppu;
use crate::serial::Serial;
use crate::timer::Timer;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Bus {
    pub(crate) cartridge: Cartridge,
    /// The boot ROM, if the user supplied one. Restored from media, never
    /// saved in states.
    #[serde(skip)]
    boot_rom: Option<Box<[u8]>>,
    boot_rom_mapped: bool,
    wram: Box<[u8]>,
    hram: Box<[u8]>,
    pub(crate) timer: Timer,
    pub(crate) ppu: Ppu,
    pub(crate) apu: Apu,
    pub(crate) joypad: Joypad,
    pub(crate) serial: Serial,
    pub(crate) interrupt_flag: u8,
    interrupt_enable: u8,
    dma: Dma,
    /// Clock cycles since power-on.
    cycles: u64,
}

/// OAM DMA: copies 160 bytes to OAM, one per M-cycle, after a one-cycle
/// start-up delay. The CPU is locked out of the bus only in cycles where a
/// byte is actually copied.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Dma {
    /// Last value written to 0xFF46.
    register: u8,
    /// Source address of the next byte, while a transfer runs.
    active: Option<u16>,
    /// A transfer requested by the last write, starting next M-cycle.
    requested: Option<u16>,
    /// The byte copied during the current M-cycle, if any. A CPU read
    /// from the bus the transfer is using sees this byte instead.
    copying: Option<u8>,
    /// The transfer reads VRAM rather than the external bus.
    from_vram: bool,
}

impl Bus {
    pub(crate) fn new(cartridge: Cartridge, boot_rom: Option<Box<[u8]>>) -> Self {
        Self {
            cartridge,
            boot_rom_mapped: boot_rom.is_some(),
            boot_rom,
            wram: vec![0; 0x2000].into_boxed_slice(),
            hram: vec![0; 0x7F].into_boxed_slice(),
            timer: Timer::after_boot(),
            ppu: Ppu::after_boot(),
            apu: Apu::after_boot(),
            joypad: Joypad::default(),
            serial: Serial::default(),
            interrupt_flag: 0x01,
            interrupt_enable: 0,
            dma: Dma {
                register: 0xFF,
                ..Dma::default()
            },
            cycles: 0,
        }
    }

    /// Takes the cartridge and boot ROM out, leaving an empty bus; for
    /// power-cycling.
    pub(crate) fn take_media(&mut self) -> (Cartridge, Option<Box<[u8]>>) {
        (std::mem::take(&mut self.cartridge), self.boot_rom.take())
    }

    /// Moves the ROM and boot ROM out of `other`, which is about to be
    /// replaced by a loaded save state.
    pub(crate) fn take_media_from(&mut self, other: &mut Self) {
        self.cartridge.take_rom_from(&mut other.cartridge);
        self.boot_rom = other.boot_rom.take();
    }

    /// Clock cycles since power-on.
    pub(crate) const fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Advances every component by one M-cycle.
    fn step_components(&mut self) {
        self.cycles += 4;
        let div_before = self.timer.counter();
        self.timer.tick(&mut self.interrupt_flag);
        // The frame sequencer runs off falling edges of DIV bit 4.
        let div_fell = div_before & 0x1000 != 0 && self.timer.counter() & 0x1000 == 0;
        self.apu.tick(div_fell);
        self.serial.tick(&mut self.interrupt_flag);
        self.ppu.tick(&mut self.interrupt_flag);
        self.cartridge.tick();
        self.step_dma();
    }

    fn step_dma(&mut self) {
        self.dma.copying = None;
        if let Some(source) = self.dma.active {
            // Sources above 0xDFFF read the WRAM echo, not OAM or I/O.
            let address = if source >= 0xE000 {
                source - 0x2000
            } else {
                source
            };
            let value = self.peek(address);
            let index = (source & 0xFF) as u8;
            self.ppu.write_oam_dma(index, value);
            self.dma.copying = Some(value);
            self.dma.from_vram = (0x8000..0xA000).contains(&source);
            self.dma.active = (index < 159).then_some(source + 1);
        }
        // A restart replaces the old transfer only after its start-up cycle.
        if let Some(source) = self.dma.requested.take() {
            self.dma.active = Some(source);
        }
    }

    /// What a CPU access to `address` meets while OAM DMA copies a byte:
    /// `Some(value)` if the transfer owns that memory, with the value a
    /// read returns. OAM reads 0xFF; the bus the transfer reads from (VRAM,
    /// or the external bus for ROM, cartridge RAM and WRAM) returns the byte
    /// in flight. I/O and HRAM stay reachable (Pan Docs, "OAM DMA Transfer").
    fn dma_conflict(&self, address: u16) -> Option<u8> {
        let in_flight = self.dma.copying?;
        let vram = (0x8000..0xA000).contains(&address);
        match address {
            0xFE00..=0xFEFF => Some(0xFF),
            0xFF00..=0xFFFF => None,
            _ if vram == self.dma.from_vram => Some(in_flight),
            _ => None,
        }
    }

    /// Reads without side effects or timing: for DMA, debuggers and
    /// tracing.
    pub(crate) fn peek(&self, address: u16) -> u8 {
        match address {
            0x0000..=0x00FF if self.boot_rom_mapped => self
                .boot_rom
                .as_ref()
                .and_then(|rom| rom.get(usize::from(address)).copied())
                .unwrap_or(0xFF),
            0x0000..=0x7FFF => self.cartridge.read_rom(address),
            0x8000..=0x9FFF => self.ppu.read_vram(address),
            0xA000..=0xBFFF => self.cartridge.read_ram(address),
            0xC000..=0xDFFF => self.wram[usize::from(address - 0xC000)],
            0xE000..=0xFDFF => self.wram[usize::from(address - 0xE000)],
            0xFE00..=0xFE9F => self.ppu.read_oam(address),
            0xFEA0..=0xFEFF => 0x00,
            0xFF00 => self.joypad.read(),
            0xFF01 | 0xFF02 => self.serial.read(address),
            0xFF04..=0xFF07 => self.timer.read(address),
            0xFF0F => self.interrupt_flag | 0xE0,
            0xFF10..=0xFF3F => self.apu.read(address),
            0xFF46 => self.dma.register,
            0xFF40..=0xFF4B => self.ppu.read_register(address),
            0xFF80..=0xFFFE => self.hram[usize::from(address - 0xFF80)],
            0xFFFF => self.interrupt_enable,
            _ => 0xFF,
        }
    }

    fn poke(&mut self, address: u16, value: u8) {
        match address {
            0x0000..=0x7FFF => self.cartridge.write_rom(address, value),
            0x8000..=0x9FFF => self.ppu.write_vram(address, value),
            0xA000..=0xBFFF => self.cartridge.write_ram(address, value),
            0xC000..=0xDFFF => self.wram[usize::from(address - 0xC000)] = value,
            0xE000..=0xFDFF => self.wram[usize::from(address - 0xE000)] = value,
            0xFE00..=0xFE9F => self.ppu.write_oam(address, value),
            0xFF00 => self.joypad.write(value),
            0xFF01 | 0xFF02 => self.serial.write(address, value),
            0xFF04..=0xFF07 => {
                let div_before = self.timer.counter();
                self.timer.write(address, value);
                if div_before & 0x1000 != 0 && self.timer.counter() & 0x1000 == 0 {
                    self.apu.clock_frame_sequencer();
                }
            }
            0xFF0F => self.interrupt_flag = value & 0x1F,
            0xFF10..=0xFF3F => self.apu.write(address, value),
            0xFF46 => {
                self.dma.register = value;
                self.dma.requested = Some(u16::from(value) << 8);
            }
            0xFF40..=0xFF4B => self
                .ppu
                .write_register(address, value, &mut self.interrupt_flag),
            0xFF50 => {
                if value & 1 != 0 {
                    self.boot_rom_mapped = false;
                }
            }
            0xFF80..=0xFFFE => self.hram[usize::from(address - 0xFF80)] = value,
            0xFFFF => self.interrupt_enable = value,
            _ => {}
        }
    }
}

impl cpu::Bus for Bus {
    fn read(&mut self, address: u16) -> u8 {
        self.step_components();
        self.dma_conflict(address)
            .unwrap_or_else(|| self.peek(address))
    }

    fn write(&mut self, address: u16, value: u8) {
        self.step_components();
        if self.dma_conflict(address).is_none() {
            self.poke(address, value);
        }
    }

    fn tick(&mut self) {
        self.step_components();
    }

    fn pending_interrupts(&self) -> u8 {
        self.interrupt_enable & self.interrupt_flag & 0x1F
    }

    fn acknowledge_interrupt(&mut self, mask: u8) {
        self.interrupt_flag &= !mask;
    }
}
