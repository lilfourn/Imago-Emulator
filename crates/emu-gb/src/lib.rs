//! Game Boy and Game Boy Color core.
//!
//! A cartridge whose header asks for CGB features runs on an emulated Game
//! Boy Color in CGB mode; every other cartridge runs on a DMG.
//!
//! The CPU drives the machine: every memory access advances the timer,
//! PPU, serial port, cartridge clock and OAM DMA by one M-cycle, so
//! components see each other at M-cycle precision without a scheduler.
//! See `docs/ROADMAP.md`, Phase 1, for status.

mod apu;
mod bus;
mod cartridge;
mod cpu;
mod hdma;
mod interrupts;
mod joypad;
mod link;
mod model;
mod ppu;
mod serial;
mod timer;

use emu_core::{
    Core, CoreInfo, Frame, FrameRate, Input, LoadError, Media, MemoryRegion, ScreenSize,
    StateError, StereoSample, System, SystemFileKind,
};
use serde::{Deserialize, Serialize};

pub use cpu::Registers;
pub use link::Linked;
pub use model::Model;

use bus::Bus;
use cartridge::Cartridge;
use cpu::Cpu;

const SCREENS: [ScreenSize; 1] = [ScreenSize::new(160, 144)];

/// Clock cycles in one video frame.
pub(crate) const CYCLES_PER_FRAME: u64 = 70_224;

/// Static facts about the Game Boy core.
pub const INFO: CoreInfo = CoreInfo {
    system: System::Gb,
    screens: &SCREENS,
    touch_screen: None,
    players: 1,
    frame_rate: FrameRate::new(4_194_304, 70_224),
    audio_sample_rate: apu::SAMPLE_RATE,
    // Bump whenever any serialized struct changes shape.
    state_version: 7,
};

/// A Game Boy with a cartridge inserted.
#[derive(Debug)]
pub struct Gb {
    machine: Machine,
    screens: [Frame; 1],
}

/// Everything a save state captures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Machine {
    cpu: Cpu,
    bus: Bus,
}

impl Machine {
    /// A machine at power-on: running the boot ROM if there is one,
    /// otherwise in the state the boot ROM leaves.
    fn power_on(model: Model, cartridge: Cartridge, boot_rom: Option<Box<[u8]>>) -> Self {
        let registers = match (boot_rom.is_some(), model) {
            (true, _) => Registers::default(),
            (false, Model::Dmg) => Registers::DMG_AFTER_BOOT,
            (false, Model::Cgb) => Registers::CGB_AFTER_BOOT,
        };
        Self {
            cpu: Cpu::new(registers),
            bus: Bus::new(model, cartridge, boot_rom),
        }
    }
}

impl Gb {
    /// Powers on a Game Boy with `media`'s cartridge.
    ///
    /// Without a boot ROM in `media`, the machine starts in the state the
    /// DMG boot ROM leaves, at 0x0100.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] if the header checksum is wrong (Pan Docs,
    /// "0x014D") or the cartridge type is not supported.
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        let header = media.rom.get(0x134..=0x14D).ok_or(LoadError::InvalidRom {
            system: System::Gb,
            reason: "shorter than the 0x150-byte header",
        })?;
        let (title_to_version, checksum) = header.split_at(header.len() - 1);
        let computed = title_to_version
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        if computed != checksum[0] {
            return Err(LoadError::InvalidRom {
                system: System::Gb,
                reason: "header checksum does not match",
            });
        }

        let model = Model::for_rom(media.rom);
        let cartridge = Cartridge::new(media.rom, media.backup, media.start_time)?;
        let boot_rom_kind = match model {
            Model::Dmg => SystemFileKind::GbBootRom,
            Model::Cgb => SystemFileKind::GbcBootRom,
        };
        let boot_rom = media
            .system_file(boot_rom_kind)
            .map(|rom| rom.to_vec().into_boxed_slice());
        Ok(Self {
            machine: Machine::power_on(model, cartridge, boot_rom),
            screens: [Frame::new(SCREENS[0])],
        })
    }

    /// Runs one instruction (or one interrupt dispatch, or one M-cycle of
    /// HALT) and returns the opcode if an instruction ran. Test harnesses
    /// use this to stop at Mooneye's `LD B, B` (0x40) breakpoint.
    pub fn step(&mut self) -> Option<u8> {
        let opcode = self.machine.cpu.step(&mut self.machine.bus);
        if self.machine.bus.ppu.take_frame_ready() {
            self.present();
        }
        opcode
    }

    /// Sets the buttons held from now on. [`Core::run_frame`] calls this
    /// with each frame's input; harnesses that drive the core with
    /// [`Gb::step`] call it directly.
    pub fn set_input(&mut self, input: &Input) {
        let bus = &mut self.machine.bus;
        bus.joypad
            .set_buttons(input.buttons, &mut bus.interrupt_flag);
    }

    /// The hardware model being emulated.
    #[must_use]
    pub fn model(&self) -> Model {
        self.machine.bus.model()
    }

    /// The CPU registers.
    #[must_use]
    pub fn registers(&self) -> Registers {
        self.machine.cpu.regs
    }

    /// Clock cycles since power-on.
    #[must_use]
    pub fn cycles(&self) -> u64 {
        self.machine.bus.cycles()
    }

    /// Reads memory as the CPU would, without advancing time.
    #[must_use]
    pub fn peek(&self, address: u16) -> u8 {
        self.machine.bus.peek(address)
    }

    /// Every byte sent through the serial port since power-on.
    #[must_use]
    pub fn serial_output(&self) -> &[u8] {
        self.machine.bus.serial.sent()
    }

    /// Cartridge RAM, where some test ROMs write their results.
    #[must_use]
    pub fn cartridge_ram(&self) -> &[u8] {
        self.machine.bus.cartridge.ram()
    }

    pub(crate) fn present(&mut self) {
        let [screen] = &mut self.screens;
        screen
            .pixels_mut()
            .copy_from_slice(self.machine.bus.ppu.pixels());
    }
}

impl Core for Gb {
    fn info(&self) -> CoreInfo {
        INFO
    }

    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>) {
        self.set_input(&inputs.first().copied().unwrap_or_default());
        let start = self.cycles();
        loop {
            self.machine.cpu.step(&mut self.machine.bus);
            if self.machine.bus.ppu.take_frame_ready() {
                self.present();
                break;
            }
            // With the LCD off no frame ever completes; keep time anyway.
            let elapsed = self.cycles() - start;
            if !self.machine.bus.ppu.is_on() && elapsed >= CYCLES_PER_FRAME {
                break;
            }
            if elapsed >= 2 * CYCLES_PER_FRAME {
                break;
            }
        }
        self.machine.bus.apu.drain_samples(audio);
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        let model = self.model();
        let (mut cartridge, boot_rom) = self.machine.bus.take_media();
        cartridge.power_cycle();
        self.machine = Machine::power_on(model, cartridge, boot_rom);
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        let bytes = postcard::to_stdvec(&self.machine).expect("machine state always serializes");
        out.extend_from_slice(&bytes);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut machine: Machine = postcard::from_bytes(state)
            .map_err(|_| StateError::Malformed("not a Game Boy state"))?;
        machine.bus.take_media_from(&mut self.machine.bus);
        self.machine = machine;
        self.present();
        Ok(())
    }

    fn backup_generation(&self, player: usize) -> Option<u64> {
        (player == 0).then(|| self.machine.bus.cartridge.backup_generation())?
    }

    fn save_backup(&self, player: usize, out: &mut Vec<u8>) {
        if player == 0 {
            self.machine.bus.cartridge.save_backup(out);
        }
    }

    fn memory(&self, region: MemoryRegion) -> Option<&[u8]> {
        match region {
            MemoryRegion::MainRam => None,
            MemoryRegion::VideoRam => Some(self.machine.bus.ppu.vram()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32 KiB ROM-only cartridge running `program` at 0x0100.
    fn rom_with_program(program: &[u8]) -> Vec<u8> {
        rom_for(Model::Dmg, program)
    }

    /// Like [`rom_with_program`], with the header asking for `model`.
    fn rom_for(model: Model, program: &[u8]) -> Vec<u8> {
        let mut rom = vec![0; 0x8000];
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        if model == Model::Cgb {
            rom[0x143] = 0x80;
        }
        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        rom
    }

    #[test]
    fn rejects_a_bad_header_checksum() {
        let mut rom = rom_with_program(&[]);
        rom[0x14D] ^= 0xFF;
        assert!(Gb::new(&Media::new(&rom)).is_err());
    }

    #[test]
    fn a_frame_takes_one_frame_of_cycles() {
        // JR -2: spin forever.
        let rom = rom_with_program(&[0x18, 0xFE]);
        let mut gb = Gb::new(&Media::new(&rom)).expect("valid rom");
        gb.run_frame(&[Input::default()], &mut Vec::new());
        let first = gb.cycles();
        gb.run_frame(&[Input::default()], &mut Vec::new());
        assert_eq!(gb.cycles() - first, CYCLES_PER_FRAME);
    }

    #[test]
    fn cgb_flag_selects_the_model() {
        let dmg = Gb::new(&Media::new(&rom_for(Model::Dmg, &[]))).expect("valid rom");
        let cgb = Gb::new(&Media::new(&rom_for(Model::Cgb, &[]))).expect("valid rom");
        assert_eq!((dmg.model(), cgb.model()), (Model::Dmg, Model::Cgb));
        assert_eq!(cgb.registers().a, 0x11);
    }

    #[test]
    fn stop_switches_to_double_speed_when_armed() {
        // LD A,1; LDH (KEY1),A; STOP 0; NOP; NOP
        let program = [0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00, 0x00, 0x00];
        let mut gb = Gb::new(&Media::new(&rom_for(Model::Cgb, &program))).expect("valid rom");
        for _ in 0..3 {
            gb.step();
        }
        assert_eq!(gb.peek(0xFF4D), 0xFE);
        // A NOP is one CPU M-cycle: half the real time at double speed.
        let before = gb.cycles();
        gb.step();
        assert_eq!(gb.cycles() - before, 2);
    }

    #[test]
    fn general_hdma_copies_to_the_selected_vram_bank() {
        let program = [
            0x3E, 0xC0, 0xE0, 0x51, // HDMA1 = 0xC0 (source 0xC000)
            0xAF, 0xE0, 0x52, // HDMA2 = 0x00
            0xE0, 0x53, // HDMA3 = 0x00 (destination 0x8000)
            0xE0, 0x54, // HDMA4 = 0x00
            0x3E, 0x01, 0xE0, 0x4F, // VBK = 1
            0x3E, 0x7E, 0xEA, 0x0F, 0xC0, // (0xC00F) = 0x7E
            0x3E, 0x00, 0xE0, 0x55, // HDMA5 = 0: one 16-byte block, now
        ];
        let mut gb = Gb::new(&Media::new(&rom_for(Model::Cgb, &program))).expect("valid rom");
        for _ in 0..13 {
            gb.step();
        }
        assert_eq!(gb.peek(0xFF55), 0xFF);
        assert_eq!(gb.peek(0x800F), 0x7E);
        assert_eq!(
            gb.memory(MemoryRegion::VideoRam).map(|vram| vram[0x200F]),
            Some(0x7E)
        );
    }

    #[test]
    fn save_state_restores_registers_and_memory() {
        // LD A, 0x42; LD (0xC000), A; INC A; JR -3 (back to INC A)
        let rom = rom_with_program(&[0x3E, 0x42, 0xEA, 0x00, 0xC0, 0x3C, 0x18, 0xFD]);
        let mut gb = Gb::new(&Media::new(&rom)).expect("valid rom");
        for _ in 0..3 {
            gb.step();
        }
        let mut state = Vec::new();
        gb.save_state(&mut state);
        let saved = gb.registers();
        for _ in 0..100 {
            gb.step();
        }
        assert_ne!(gb.registers(), saved);
        gb.load_state(&state).expect("own state loads");
        assert_eq!(gb.registers(), saved);
        assert_eq!(gb.peek(0xC000), 0x42);
        assert!(gb.load_state(&state[..10]).is_err());
    }
}
