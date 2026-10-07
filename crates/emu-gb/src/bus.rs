//! The memory map (Pan Docs, "Memory Map", "I/O Ranges") and the clock that
//! drives every component one M-cycle at a time.
//!
//! In CGB double-speed mode the CPU, timer, serial port and OAM DMA run at
//! twice the speed, while the PPU, APU, cartridge clock and HDMA keep real
//! time, so each CPU M-cycle advances them by two clock cycles instead of
//! four.

use serde::{Deserialize, Serialize};

use crate::apu::Apu;
use crate::cartridge::Cartridge;
use crate::cpu;
use crate::hdma::{Block, Hdma, Started};
use crate::joypad::Joypad;
use crate::model::Model;
use crate::ppu::Ppu;
use crate::serial::Serial;
use crate::timer::Timer;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Bus {
    model: Model,
    pub(crate) cartridge: Cartridge,
    /// The boot ROM, if the user supplied one. Restored from media, never
    /// saved in states.
    #[serde(skip)]
    boot_rom: Option<Box<[u8]>>,
    boot_rom_mapped: bool,
    /// Eight 4 KiB banks; DMG uses the first two.
    wram: Box<[u8]>,
    /// SVBK: the bank at 0xD000–0xDFFF, 1 to 7.
    wram_bank: u8,
    hram: Box<[u8]>,
    pub(crate) timer: Timer,
    pub(crate) ppu: Ppu,
    pub(crate) apu: Apu,
    pub(crate) joypad: Joypad,
    pub(crate) serial: Serial,
    pub(crate) interrupt_flag: u8,
    interrupt_enable: u8,
    dma: Dma,
    hdma: Hdma,
    double_speed: bool,
    /// KEY1 bit 0: the next STOP switches speed.
    speed_switch_armed: bool,
    /// Undocumented CGB registers 0xFF72–0xFF75 and the infrared port.
    undocumented: [u8; 4],
    infrared: u8,
    /// Real-time clock cycles since power-on (4.19 MHz, whatever the CPU
    /// speed).
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
    pub(crate) fn new(model: Model, cartridge: Cartridge, boot_rom: Option<Box<[u8]>>) -> Self {
        Self {
            model,
            cartridge,
            boot_rom_mapped: boot_rom.is_some(),
            boot_rom,
            wram: vec![0; 0x8000].into_boxed_slice(),
            wram_bank: 1,
            hram: vec![0; 0x7F].into_boxed_slice(),
            timer: Timer::after_boot(),
            ppu: Ppu::after_boot(model),
            apu: Apu::after_boot(model),
            joypad: Joypad::default(),
            serial: Serial::new(model),
            interrupt_flag: 0x01,
            interrupt_enable: 0,
            dma: Dma {
                register: 0xFF,
                ..Dma::default()
            },
            hdma: Hdma::default(),
            double_speed: false,
            speed_switch_armed: false,
            undocumented: [0; 4],
            infrared: 0,
            cycles: 0,
        }
    }

    pub(crate) fn model(&self) -> Model {
        self.model
    }

    fn cgb(&self) -> bool {
        self.model == Model::Cgb
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

    /// Real-time clock cycles since power-on.
    pub(crate) const fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Advances every component by one CPU M-cycle, then runs an HBlank
    /// DMA block if one is due.
    fn step_components(&mut self) {
        self.advance();
        if self.ppu.take_hblank_started()
            && self.hdma.hblank_active()
            && let Some(block) = self.hdma.next_block()
        {
            self.copy_block(block);
        }
    }

    fn advance(&mut self) {
        let real_cycles = if self.double_speed { 2 } else { 4 };
        self.cycles += u64::from(real_cycles);
        let div_before = self.timer.counter();
        self.timer.tick(&mut self.interrupt_flag);
        let frame_sequencer = self.div_bit_fell(div_before);
        self.apu.tick(real_cycles, frame_sequencer);
        self.serial.tick(&mut self.interrupt_flag);
        self.ppu.tick(real_cycles, &mut self.interrupt_flag);
        self.cartridge.tick(u32::from(real_cycles));
        self.step_dma();
    }

    /// Whether the DIV bit that clocks the APU's 512 Hz frame sequencer fell
    /// since the counter was `before`: bit 4 of DIV, or bit 5 in double
    /// speed, where DIV runs twice as fast.
    fn div_bit_fell(&self, before: u16) -> bool {
        let bit = if self.double_speed { 0x2000 } else { 0x1000 };
        before & bit != 0 && self.timer.counter() & bit == 0
    }

    /// Copies one HDMA block to VRAM. The CPU is halted for the copy: 8
    /// M-cycles, or 16 in double speed.
    fn copy_block(&mut self, block: Block) {
        for offset in 0..16 {
            let value = self.peek(block.source.wrapping_add(offset));
            self.ppu.write_vram(block.destination + offset, value);
        }
        let stall = if self.double_speed { 16 } else { 8 };
        for _ in 0..stall {
            self.advance();
        }
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

    /// Index into `wram` for an address in 0xC000–0xFDFF.
    fn wram_index(&self, address: u16) -> usize {
        let offset = usize::from((address - 0xC000) & 0x1FFF);
        if offset < 0x1000 {
            offset
        } else {
            usize::from(self.wram_bank) * 0x1000 + offset - 0x1000
        }
    }

    /// Reads without side effects or timing: for DMA, debuggers and
    /// tracing.
    pub(crate) fn peek(&self, address: u16) -> u8 {
        let cgb = self.cgb();
        match address {
            0x0000..=0x00FF if self.boot_rom_mapped => self.boot_rom_byte(address),
            0x0200..=0x08FF if self.boot_rom_mapped && cgb => self.boot_rom_byte(address),
            0x0000..=0x7FFF => self.cartridge.read_rom(address),
            0x8000..=0x9FFF => self.ppu.read_vram(address),
            0xA000..=0xBFFF => self.cartridge.read_ram(address),
            0xC000..=0xFDFF => self.wram[self.wram_index(address)],
            0xFE00..=0xFE9F => self.ppu.read_oam(address),
            0xFEA0..=0xFEFF => 0x00,
            0xFF00 => self.joypad.read(),
            0xFF01 | 0xFF02 => self.serial.read(address),
            0xFF04..=0xFF07 => self.timer.read(address),
            0xFF0F => self.interrupt_flag | 0xE0,
            0xFF10..=0xFF3F => self.apu.read(address),
            0xFF46 => self.dma.register,
            0xFF4D if cgb => {
                0x7E | u8::from(self.double_speed) << 7 | u8::from(self.speed_switch_armed)
            }
            0xFF55 if cgb => self.hdma.read_control(),
            0xFF56 if cgb => 0x3C | (self.infrared & 0xC1) | 0x02,
            0xFF70 if cgb => 0xF8 | self.wram_bank,
            0xFF72 | 0xFF73 if cgb => self.undocumented[usize::from(address - 0xFF72)],
            0xFF74 if cgb => self.undocumented[2],
            0xFF75 if cgb => 0x8F | (self.undocumented[3] & 0x70),
            0xFF76 | 0xFF77 if cgb => 0x00,
            0xFF40..=0xFF4F | 0xFF68..=0xFF6C => self.ppu.read_register(address),
            0xFF80..=0xFFFE => self.hram[usize::from(address - 0xFF80)],
            0xFFFF => self.interrupt_enable,
            _ => 0xFF,
        }
    }

    /// Reads plain RAM (cartridge RAM, WRAM, HRAM), for cheats; `None`
    /// elsewhere.
    pub(crate) fn peek_ram(&self, address: u16) -> Option<u8> {
        is_ram(address).then(|| self.peek(address))
    }

    /// Writes plain RAM, for cheats. Returns whether `address` is RAM.
    pub(crate) fn poke_ram(&mut self, address: u16, value: u8) -> bool {
        let ram = is_ram(address);
        if ram {
            self.poke(address, value);
        }
        ram
    }

    /// Writes RAM in bank `bank` of the switchable region holding
    /// `address`, whichever bank is mapped, for cheats: cartridge RAM, or
    /// CGB WRAM at 0xD000–0xDFFF (and its echo), where bank 0 means 1.
    /// Elsewhere it is [`Bus::poke_ram`]. Returns whether `address` is RAM.
    pub(crate) fn poke_ram_bank(&mut self, bank: u8, address: u16, value: u8) -> bool {
        match address {
            0xA000..=0xBFFF => self.cartridge.write_ram_bank(bank, address, value),
            0xD000..=0xDFFF | 0xF000..=0xFDFF if self.cgb() => {
                let bank = usize::from((bank & 7).max(1));
                self.wram[bank * 0x1000 + usize::from(address & 0x0FFF)] = value;
            }
            _ => return self.poke_ram(address, value),
        }
        true
    }

    /// Writes memory without side effects, for debuggers: cartridge RAM,
    /// WRAM and HRAM, VRAM (the selected bank) and OAM. Returns whether
    /// `address` is memory; ROM and I/O registers refuse.
    pub(crate) fn poke_memory(&mut self, address: u16, value: u8) -> bool {
        match address {
            0x8000..=0x9FFF => self.ppu.write_vram(address, value),
            0xFE00..=0xFE9F => self.ppu.write_oam(address, value),
            _ => return self.poke_ram(address, value),
        }
        true
    }

    fn boot_rom_byte(&self, address: u16) -> u8 {
        self.boot_rom
            .as_ref()
            .and_then(|rom| rom.get(usize::from(address)).copied())
            .unwrap_or(0xFF)
    }

    fn poke(&mut self, address: u16, value: u8) {
        let cgb = self.cgb();
        match address {
            0x0000..=0x7FFF => self.cartridge.write_rom(address, value),
            0x8000..=0x9FFF => self.ppu.write_vram(address, value),
            0xA000..=0xBFFF => self.cartridge.write_ram(address, value),
            0xC000..=0xFDFF => {
                let index = self.wram_index(address);
                self.wram[index] = value;
            }
            0xFE00..=0xFE9F => self.ppu.write_oam(address, value),
            0xFF00 => self.joypad.write(value),
            0xFF01 | 0xFF02 => self.serial.write(address, value),
            0xFF04..=0xFF07 => {
                let div_before = self.timer.counter();
                self.timer.write(address, value);
                if self.div_bit_fell(div_before) {
                    self.apu.clock_frame_sequencer();
                }
            }
            0xFF0F => self.interrupt_flag = value & 0x1F,
            0xFF10..=0xFF3F => self.apu.write(address, value),
            0xFF46 => {
                self.dma.register = value;
                self.dma.requested = Some(u16::from(value) << 8);
            }
            0xFF4D if cgb => self.speed_switch_armed = value & 1 != 0,
            0xFF50 => {
                if value & 1 != 0 {
                    self.boot_rom_mapped = false;
                }
            }
            0xFF51..=0xFF55 if cgb => {
                if self.hdma.write(address, value) == Started::General {
                    while let Some(block) = self.hdma.next_block() {
                        self.copy_block(block);
                    }
                }
            }
            0xFF56 if cgb => self.infrared = value,
            0xFF70 if cgb => self.wram_bank = (value & 7).max(1),
            0xFF72 | 0xFF73 if cgb => self.undocumented[usize::from(address - 0xFF72)] = value,
            0xFF74 if cgb => self.undocumented[2] = value,
            0xFF75 if cgb => self.undocumented[3] = value,
            0xFF40..=0xFF4F | 0xFF68..=0xFF6C => {
                self.ppu
                    .write_register(address, value, &mut self.interrupt_flag);
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

    /// STOP switches CPU speed on CGB when KEY1 armed it, and resets DIV.
    fn stop(&mut self) {
        if self.cgb() && self.speed_switch_armed {
            self.speed_switch_armed = false;
            self.double_speed = !self.double_speed;
            self.timer.write(0xFF04, 0);
        }
    }
}

/// Cartridge RAM, WRAM (with its echo) and HRAM.
fn is_ram(address: u16) -> bool {
    matches!(address, 0xA000..=0xFDFF | 0xFF80..=0xFFFE)
}
