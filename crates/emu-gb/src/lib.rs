//! Game Boy and Game Boy Color core.
//!
//! A cartridge whose header asks for CGB features runs on an emulated Game
//! Boy Color in CGB mode. Every other cartridge runs on a DMG, or, if the
//! player asks for colour ([`Media::dmg_colors`]), on a Game Boy Color in
//! DMG mode with the palettes its boot ROM would pick.
//!
//! The CPU drives the machine: every memory access advances the timer,
//! PPU, serial port, cartridge clock and OAM DMA by one M-cycle, so
//! components see each other at M-cycle precision without a scheduler.
//! See `docs/ROADMAP.md`, Phase 1, for status.

mod apu;
mod bus;
mod cartridge;
mod compat_palettes;
mod cpu;
mod debug;
mod disasm;
mod graphics;
mod hdma;
mod interrupts;
mod io_names;
mod joypad;
mod link;
mod model;
mod ppu;
mod serial;
mod timer;

use emu_core::debug::RunOutcome;
use emu_core::{
    Core, CoreInfo, Debugger, DmgColors, Frame, FrameRate, Input, LoadError, Media, MemoryRegion,
    RomPatch, ScreenSize, StateError, StereoSample, System, SystemFileKind,
};
use serde::{Deserialize, Serialize};

pub use cpu::Registers;
pub use disasm::{Disassembly, disassemble};
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
    state_version: 11,
};

/// A Game Boy with a cartridge inserted.
#[derive(Debug)]
pub struct Gb {
    machine: Machine,
    screens: [Frame; 1],
    /// How the player chose to show games made for the original Game Boy,
    /// for powering on again.
    dmg_colors: DmgColors,
    /// When the frame under way began, in clock cycles, while a debugger
    /// holds it part-way: a frame with the LCD off lasts a frame's time
    /// from there, however often it stops.
    frame_start: Option<u64>,
}

/// Everything a save state captures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Machine {
    cpu: Cpu,
    bus: Bus,
}

impl Machine {
    /// A machine at power-on: running the boot ROM if there is one,
    /// otherwise in the state the boot ROM leaves. On a Game Boy Color in
    /// DMG mode that includes the palettes `dmg_colors` picks.
    fn power_on(
        model: Model,
        dmg_colors: DmgColors,
        cartridge: Cartridge,
        boot_rom: Option<Box<[u8]>>,
    ) -> Self {
        let title_checksum = compat_palettes::nintendo_title_checksum(cartridge.rom());
        let palettes = compat_palettes::palettes_for(cartridge.rom(), dmg_colors);
        let registers = match (boot_rom.is_some(), model) {
            (true, _) => Registers::default(),
            (false, Model::Dmg) => Registers::DMG_AFTER_BOOT,
            (false, Model::Cgb) => Registers::CGB_AFTER_BOOT,
            (false, Model::CgbDmgMode) => {
                Registers::cgb_dmg_mode_after_boot(title_checksum.unwrap_or(0))
            }
        };
        let mut bus = Bus::new(model, cartridge, boot_rom);
        if let (Model::CgbDmgMode, Some(palettes)) = (model, palettes) {
            bus.ppu.load_compat_palettes(&palettes);
        }
        Self {
            cpu: Cpu::new(registers),
            bus,
        }
    }
}

/// The model that runs `rom` when the player shows games made for the
/// original Game Boy as `dmg_colors` says.
fn model_for(rom: &[u8], dmg_colors: DmgColors) -> Model {
    match Model::for_rom(rom) {
        Model::Dmg if dmg_colors != DmgColors::Grey => Model::CgbDmgMode,
        model => model,
    }
}

impl Gb {
    /// Powers on a Game Boy with `media`'s cartridge.
    ///
    /// Without a boot ROM in `media`, the machine starts in the state the
    /// boot ROM leaves, at 0x0100. A game made for the original Game Boy
    /// runs on a Game Boy Color in DMG mode unless [`Media::dmg_colors`] is
    /// [`DmgColors::Grey`]; that never runs a boot ROM.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] if the header checksum is wrong (Pan Docs,
    /// "0x014D"), the cartridge type is not supported, or `media` has a
    /// second-slot cartridge, which a Game Boy has no slot for.
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        if media.second_slot.is_some() {
            return Err(LoadError::Unsupported("a second cartridge slot"));
        }
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

        let model = model_for(media.rom, media.dmg_colors);
        let cartridge = Cartridge::new(media.rom, media.backup, media.start_time)?;
        // The CGB boot ROM switches to DMG mode itself, which is not
        // emulated, so DMG mode starts where it would leave off.
        let boot_rom_kind = match model {
            Model::Dmg => Some(SystemFileKind::GbBootRom),
            Model::Cgb => Some(SystemFileKind::GbcBootRom),
            Model::CgbDmgMode => None,
        };
        let boot_rom = boot_rom_kind
            .and_then(|kind| media.system_file(kind))
            .map(|rom| rom.to_vec().into_boxed_slice());
        Ok(Self {
            machine: Machine::power_on(model, media.dmg_colors, cartridge, boot_rom),
            screens: [Frame::new(SCREENS[0])],
            dmg_colors: media.dmg_colors,
            frame_start: None,
        })
    }

    /// Runs one instruction (or one interrupt dispatch, or one M-cycle of
    /// HALT) and returns the opcode if an instruction ran. Test harnesses
    /// use this to stop at Mooneye's `LD B, B` (0x40) breakpoint.
    pub fn step(&mut self) -> Option<u8> {
        let opcode = self.machine.cpu.step(&mut self.machine.bus);
        self.take_frame();
        opcode
    }

    /// Shows the frame the PPU just finished, if it did; returns whether
    /// it did.
    fn take_frame(&mut self) -> bool {
        let ready = self.machine.bus.ppu.take_frame_ready();
        if ready {
            self.present();
        }
        ready
    }

    /// Runs `step` until a frame finishes or a step stops, for
    /// [`Core::run_frame`] and [`Debugger::run_until`]. A frame finishes
    /// at VBlank, or after two frames' time if VBlank never comes. With the
    /// LCD off it finishes after a frame's time, including the frame the
    /// LCD is switched off in, so the frontend gets a whole frame of sound.
    fn run(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        mut step: impl FnMut(&mut Self) -> RunOutcome,
    ) -> RunOutcome {
        self.set_input(&inputs.first().copied().unwrap_or_default());
        let start = *self.frame_start.get_or_insert(self.cycles());
        let outcome = loop {
            let mut outcome = step(self);
            let elapsed = self.cycles().saturating_sub(start);
            // Switching the LCD off shows a blank screen at once, but the
            // frame runs its full time.
            outcome.frame_done = if self.machine.bus.ppu.is_on() {
                outcome.frame_done || elapsed >= 2 * CYCLES_PER_FRAME
            } else {
                elapsed >= CYCLES_PER_FRAME
            };
            if outcome.frame_done || outcome.stop.is_some() {
                break outcome;
            }
        };
        if outcome.frame_done {
            self.frame_start = None;
            self.machine.bus.apu.drain_samples(audio);
        }
        outcome
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

    /// The latest bytes sent through the serial port, oldest first: at
    /// least the last 4 KiB, more than any test ROM's report.
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
        self.run(inputs, audio, |gb| {
            gb.machine.cpu.step(&mut gb.machine.bus);
            RunOutcome {
                frame_done: gb.take_frame(),
                stop: None,
            }
        });
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        let (mut cartridge, boot_rom) = self.machine.bus.take_media();
        cartridge.power_cycle();
        // The player's colour choice picks the model, not a loaded state,
        // which may come from the other choice: the model always matches
        // the palettes and the boot ROM.
        let model = model_for(cartridge.rom(), self.dmg_colors);
        self.machine = Machine::power_on(model, self.dmg_colors, cartridge, boot_rom);
        self.frame_start = None;
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
        self.frame_start = None;
        self.present();
        Ok(())
    }

    fn backup_generation(&self, cartridge: usize) -> Option<u64> {
        (cartridge == 0).then(|| self.machine.bus.cartridge.backup_generation())?
    }

    fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
        if cartridge == 0 {
            self.machine.bus.cartridge.save_backup(out);
        }
    }

    fn peek(&self, player: usize, address: u32) -> Option<u8> {
        let address = u16::try_from(address).ok().filter(|_| player == 0)?;
        self.machine.bus.peek_ram(address)
    }

    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool {
        let Some(address) = u16::try_from(address).ok().filter(|_| player == 0) else {
            return false;
        };
        self.machine.bus.poke_ram(address, value)
    }

    /// Banks are cartridge RAM's at 0xA000–0xBFFF and, on a Game Boy
    /// Color, WRAM's at 0xD000–0xDFFF.
    fn poke_bank(&mut self, player: usize, bank: u8, address: u32, value: u8) -> bool {
        let Some(address) = u16::try_from(address).ok().filter(|_| player == 0) else {
            return false;
        };
        self.machine.bus.poke_ram_bank(bank, address, value)
    }

    /// Patches apply at 0x0000–0x7FFF, in every bank; others are dropped.
    fn set_rom_patches(&mut self, player: usize, patches: &[RomPatch]) {
        if player == 0 {
            self.machine.bus.cartridge.set_rom_patches(patches);
        }
    }

    fn memory(&self, region: MemoryRegion) -> Option<&[u8]> {
        match region {
            MemoryRegion::MainRam => None,
            MemoryRegion::VideoRam => Some(self.machine.bus.ppu.vram()),
        }
    }

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
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
    fn switching_the_lcd_off_blanks_the_screen_but_not_the_frames_time() {
        // LD A,0; LDH (LCDC),A; JR -2
        let rom = rom_with_program(&[0x3E, 0x00, 0xE0, 0x40, 0x18, 0xFE]);
        let mut gb = Gb::new(&Media::new(&rom)).expect("valid rom");
        gb.screens[0].pixels_mut().fill(0);
        gb.run_frame(&[Input::default()], &mut Vec::new());
        // A JR takes 12 cycles, so the frame ends within 12 of its time.
        assert!((CYCLES_PER_FRAME..CYCLES_PER_FRAME + 12).contains(&gb.cycles()));
        assert!(
            gb.screens()[0]
                .pixels()
                .iter()
                .all(|pixel| *pixel == ppu::DMG_SHADES[0])
        );
    }

    #[test]
    fn a_boot_rom_starts_with_the_lcd_off() {
        // The boot ROM spins: JR -2.
        let mut boot_rom = vec![0; 0x100];
        boot_rom[..2].copy_from_slice(&[0x18, 0xFE]);
        let rom = rom_with_program(&[]);
        let mut gb = Gb::new(&Media {
            system_files: &[emu_core::SystemFile {
                kind: SystemFileKind::GbBootRom,
                data: &boot_rom,
            }],
            ..Media::new(&rom)
        })
        .expect("valid rom");
        assert_eq!(gb.registers().pc, 0x0000);
        assert_eq!(gb.peek(0xFF40), 0x00);
        for _ in 0..1000 {
            gb.step();
        }
        assert_eq!(gb.peek(0xFF44), 0);
    }

    #[test]
    fn cgb_flag_selects_the_model() {
        let dmg = Gb::new(&Media::new(&rom_for(Model::Dmg, &[]))).expect("valid rom");
        let cgb = Gb::new(&Media::new(&rom_for(Model::Cgb, &[]))).expect("valid rom");
        assert_eq!((dmg.model(), cgb.model()), (Model::Dmg, Model::Cgb));
        assert_eq!(cgb.registers().a, 0x11);
    }

    #[test]
    fn dmg_games_in_colour_run_on_a_cgb_in_dmg_mode() {
        let mut rom = rom_for(Model::Dmg, &[]);
        rom[0x134..0x13F].copy_from_slice(b"POKEMON RED");
        rom[0x14B] = 0x01;
        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        let in_colour = Media {
            dmg_colors: DmgColors::ByTitle,
            ..Media::new(&rom)
        };
        let mut gb = Gb::new(&in_colour).expect("valid rom");
        assert_eq!(gb.model(), Model::CgbDmgMode);
        // A = 0x11 tells games they are on a CGB; B holds the checksum.
        let registers = gb.registers();
        assert_eq!((registers.a, registers.b, registers.l), (0x11, 0x14, 0x7C));
        // The palettes are the boot ROM's, out of the game's reach.
        assert_eq!(gb.machine.bus.ppu.bg_palette_ram()[2..4], [0x1F, 0x42]);
        assert_eq!(gb.peek(0xFF68), 0xFF);
        assert_eq!(gb.peek(0xFF4F), 0xFF);
        gb.reset();
        assert_eq!(gb.machine.bus.ppu.bg_palette_ram()[2..4], [0x1F, 0x42]);

        let grey = Gb::new(&Media::new(&rom)).expect("valid rom");
        assert_eq!(grey.model(), Model::Dmg);
        let cgb_rom = rom_for(Model::Cgb, &[]);
        let cgb = Gb::new(&Media {
            dmg_colors: DmgColors::ByTitle,
            ..Media::new(&cgb_rom)
        })
        .expect("valid rom");
        assert_eq!(cgb.model(), Model::Cgb);
    }

    #[test]
    fn reset_keeps_the_players_colour_choice_after_a_state_from_the_other() {
        let rom = rom_with_program(&[0x18, 0xFE]);
        let mut grey = Gb::new(&Media::new(&rom)).expect("valid rom");
        let mut colour = Gb::new(&Media {
            dmg_colors: DmgColors::ByTitle,
            ..Media::new(&rom)
        })
        .expect("valid rom");
        let mut grey_state = Vec::new();
        grey.save_state(&mut grey_state);
        let mut colour_state = Vec::new();
        colour.save_state(&mut colour_state);

        grey.load_state(&colour_state).expect("same game");
        assert_eq!(grey.model(), Model::CgbDmgMode);
        grey.reset();
        assert_eq!(grey.model(), Model::Dmg);

        colour.load_state(&grey_state).expect("same game");
        assert_eq!(colour.model(), Model::Dmg);
        colour.reset();
        assert_eq!(colour.model(), Model::CgbDmgMode);
        // Combination 0's background colour 1, not palette RAM's 0xFF.
        assert_eq!(colour.machine.bus.ppu.bg_palette_ram()[2..4], [0xEF, 0x1B]);
    }

    #[test]
    fn bank_pokes_reach_unmapped_wram_banks() {
        // LD A,3; LDH (SVBK),A
        let program = [0x3E, 0x03, 0xE0, 0x70];
        let mut gb = Gb::new(&Media::new(&rom_for(Model::Cgb, &program))).expect("valid rom");
        // WRAM bank 1 is mapped at power-on; write bank 3 behind its back.
        assert!(gb.poke_bank(0, 3, 0xD010, 0x63));
        assert_eq!(Core::peek(&gb, 0, 0xD010), Some(0));
        gb.step();
        gb.step();
        assert_eq!(Core::peek(&gb, 0, 0xD010), Some(0x63));
        // Outside the banked regions it is a plain poke.
        assert!(gb.poke_bank(0, 5, 0xC000, 0x11));
        assert_eq!(Core::peek(&gb, 0, 0xC000), Some(0x11));
        assert!(!gb.poke_bank(0, 1, 0x8000, 0x11));
    }

    #[test]
    fn rom_patches_change_reads_and_outlast_states_and_resets() {
        // LD A,(0x0150); LD (0xC000),A; JR -2
        let rom = rom_with_program(&[0xFA, 0x50, 0x01, 0xEA, 0x00, 0xC0, 0x18, 0xFE]);
        let mut gb = Gb::new(&Media::new(&rom)).expect("valid rom");
        let mut unpatched = Vec::new();
        gb.save_state(&mut unpatched);
        gb.set_rom_patches(
            0,
            &[RomPatch {
                address: 0x0150,
                value: 0x99,
                compare: None,
            }],
        );
        let mut patched = Vec::new();
        gb.save_state(&mut patched);
        assert_eq!(patched, unpatched, "patches are not machine state");
        gb.step();
        gb.step();
        assert_eq!(Core::peek(&gb, 0, 0xC000), Some(0x99));

        gb.load_state(&unpatched).expect("own state loads");
        assert_eq!(gb.peek(0x0150), 0x99);
        gb.reset();
        assert_eq!(gb.peek(0x0150), 0x99);
        gb.set_rom_patches(0, &[]);
        assert_eq!(gb.peek(0x0150), 0);
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
