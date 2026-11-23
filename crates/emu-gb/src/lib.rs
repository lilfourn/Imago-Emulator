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
use emu_state::payload;
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
    /// A wrong header checksum (Pan Docs, "0x014D") is no bar. A boot ROM
    /// given in `media` locks up on it, as on hardware; without one the
    /// game starts, as in other emulators. Some homebrew, mostly demos
    /// written on emulators in the 1990s, never set the checksum.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] if the ROM is shorter than its header, the
    /// cartridge type is not supported, or `media` has a second-slot
    /// cartridge, which a Game Boy has no slot for.
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        if media.second_slot.is_some() {
            return Err(LoadError::Unsupported("a second cartridge slot"));
        }
        // The cartridge reads its header up to 0x149; a tiny intro may end
        // just after it.
        if media.rom.len() < 0x14E {
            return Err(LoadError::InvalidRom {
                system: System::Gb,
                reason: "shorter than the 0x150-byte header",
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
        payload::append(&self.machine, out);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut machine: Machine = payload::load(state, "not a Game Boy state")?;
        machine.bus.fits(&self.machine.bus)?;
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
    use emu_state::mutation;

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

    /// A Game Boy Color cartridge with an MBC3, its clock and 32 KiB of
    /// RAM, running `program`: the most a cartridge puts in a state.
    fn rom_with_a_clock(program: &[u8]) -> Vec<u8> {
        let mut rom = rom_for(Model::Cgb, program);
        rom[0x147] = 0x10;
        rom[0x149] = 0x03;
        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        rom
    }

    /// Loads into a Game Boy playing `rom` the wrong states made from its
    /// own after `frames` frames ([`mutation::try_each`]), and runs each
    /// it takes: two frames, the memory a debugger reads, a save, a reset
    /// and a frame more.
    fn try_wrong_states(rom: &[u8], frames: usize, every: usize) -> mutation::Outcome {
        let mut gb = Gb::new(&Media::new(rom)).expect("valid rom");
        let mut audio = Vec::new();
        for _ in 0..frames {
            gb.run_frame(&[Input::default()], &mut audio);
        }
        let mut good = Vec::new();
        gb.save_state(&mut good);
        gb.load_state(&good).expect("its own state loads");
        let machine = gb.machine.clone();
        mutation::try_each(&machine, every, |wrong| {
            if gb.load_state(wrong).is_err() {
                return;
            }
            let mut audio = Vec::new();
            gb.run_frame(&[Input::default()], &mut audio);
            gb.run_frame(&[Input::default()], &mut audio);
            for address in (0..=0xFFFF).step_by(0x55) {
                let _ = Core::peek(&gb, 0, address);
                let _ = gb.peek(address as u16);
            }
            let _ = gb.memory(MemoryRegion::VideoRam);
            let mut saved = Vec::new();
            gb.save_state(&mut saved);
            gb.reset();
            gb.run_frame(&[Input::default()], &mut audio);
        })
    }

    /// A state is written after whatever the caller's buffer holds, as
    /// the app's rewind snapshot counts on, and is the same bytes at any
    /// place in it.
    #[test]
    fn a_state_is_appended_to_what_the_buffer_holds() {
        let rom = rom_with_program(&[0x18, 0xFE]);
        let mut core = Gb::new(&Media::new(&rom)).expect("valid rom");
        core.run_frame(&[Input::default()], &mut Vec::new());
        let mut alone = Vec::new();
        core.save_state(&mut alone);
        let mut after = vec![0xAA, 0xBB, 0xCC];
        core.save_state(&mut after);
        assert_eq!(after[..3], [0xAA, 0xBB, 0xCC]);
        assert!(after[3..] == alone[..], "the same state");
    }

    /// A state with bytes after it is not that state: a linked pair's
    /// begins with one console's, and used to load into a console on its
    /// own, dropping the second player.
    #[test]
    fn a_state_with_bytes_left_over_is_refused() {
        let rom = rom_with_program(&[0x18, 0xFE]);
        let media = Media::new(&rom);
        let mut one = Gb::new(&media).expect("valid rom");
        let mut pair = Linked::new(&[media, media]).expect("two games");
        let (mut alone, mut together) = (Vec::new(), Vec::new());
        one.save_state(&mut alone);
        pair.save_state(&mut together);
        assert!(one.load_state(&alone).is_ok() && pair.load_state(&together).is_ok());
        // As they are after a load, which counts as a write to the save.
        let (mut before, mut before_pair) = (Vec::new(), Vec::new());
        one.save_state(&mut before);
        pair.save_state(&mut before_pair);
        let refused =
            |result: Result<(), StateError>| matches!(result, Err(StateError::Malformed(_)));
        assert!(refused(one.load_state(&together)), "a pair's into one");
        assert!(refused(pair.load_state(&alone)), "one's into a pair");
        for state in [&mut alone, &mut together] {
            state.push(0);
        }
        assert!(refused(one.load_state(&alone)), "one more byte");
        assert!(refused(pair.load_state(&together)), "one more byte");
        // A refused state changes nothing.
        let (mut after, mut after_pair) = (Vec::new(), Vec::new());
        one.save_state(&mut after);
        pair.save_state(&mut after_pair);
        assert!(after == before, "the console is as it was");
        assert!(after_pair == before_pair, "the pair is as it was");
    }

    /// As [`try_wrong_states`], for two Game Boys playing `rom` on a link cable:
    /// the wrong states are made from the pair's own, and each one the
    /// pair takes runs a frame, saves, resets and runs a frame more.
    fn try_wrong_pair_states(rom: &[u8], frames: usize, every: usize) -> mutation::Outcome {
        let media = Media::new(rom);
        let mut pair = Linked::new(&[media, media]).expect("two games");
        let inputs = [Input::default(); 2];
        let mut audio = Vec::new();
        for _ in 0..frames {
            pair.run_frame(&inputs, &mut audio);
            audio.clear();
        }
        let mut good = Vec::new();
        pair.save_state(&mut good);
        pair.load_state(&good).expect("its own state loads");
        let machines: (Machine, Machine) = payload::load(&good, "its own state").expect("reads");
        mutation::try_each(&machines, every, |wrong| {
            if pair.load_state(wrong).is_err() {
                return;
            }
            let mut audio = Vec::new();
            pair.run_frame(&inputs, &mut audio);
            let mut saved = Vec::new();
            pair.save_state(&mut saved);
            pair.reset();
            pair.run_frame(&inputs, &mut audio);
        })
    }

    /// A state that is wrong in one place is refused, or loads and runs:
    /// it never panics or hangs the core. This tries every place in one
    /// cartridge's state and a sample of a linked pair's;
    /// `every_wrong_state_is_refused_or_runs` tries more machines.
    #[test]
    fn a_wrong_state_is_refused_or_runs() {
        // LD A, 0x0A; LD (0x0000), A; LD A, 0x08; LD (0x4000), A; JR -2:
        // RAM on and the clock's seconds mapped, then spin.
        let program = [
            0x3E, 0x0A, 0xEA, 0x00, 0x00, 0x3E, 0x08, 0xEA, 0x00, 0x40, 0x18, 0xFE,
        ];
        let outcome = try_wrong_states(&rom_with_a_clock(&program), 3, 1);
        assert!(outcome.tried > 1500, "{} states", outcome.tried);
        outcome.assert_no_panics();
        let outcome = try_wrong_pair_states(&rom_with_a_clock(&program), 3, 29);
        assert!(outcome.tried > 100, "{} states", outcome.tried);
        outcome.assert_no_panics();
    }

    /// Every place in the states of a cartridge of each kind, and of the
    /// test ROMs that use the most of the machine, if they are there
    /// (`scripts/fetch-test-roms.sh`), each on a thread of its own. A
    /// minute or two.
    #[test]
    #[ignore = "minutes: every byte of several states' structure"]
    fn every_wrong_state_is_refused_or_runs() {
        let program = [
            0x3E, 0x0A, 0xEA, 0x00, 0x00, 0x3E, 0x08, 0xEA, 0x00, 0x40, 0x18, 0xFE,
        ];
        let mut machines: Vec<(String, Box<dyn FnOnce() -> mutation::Outcome + Send>)> = Vec::new();
        // ROM only, MBC1, MBC2, MBC3 with its clock, MBC5.
        for kind in [0x00, 0x03, 0x06, 0x10, 0x1B] {
            let mut rom = rom_with_a_clock(&program);
            rom[0x147] = kind;
            rom[0x14D] = rom[0x134..0x14D]
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
            machines.push((
                format!("cartridge type {kind:#04X}"),
                Box::new(move || try_wrong_states(&rom, 3, 1)),
            ));
        }
        let linked = rom_with_a_clock(&program);
        machines.push((
            "a linked pair".to_owned(),
            Box::new(move || try_wrong_pair_states(&linked, 3, 1)),
        ));
        let roms = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-roms/game-boy-test-roms");
        for (name, frames) in [
            ("blargg/cgb_sound/cgb_sound.gb", 700),
            ("blargg/dmg_sound/dmg_sound.gb", 300),
            ("cgb-acid2/cgb-acid2.gbc", 30),
            ("dmg-acid2/dmg-acid2.gb", 30),
        ] {
            let Ok(rom) = std::fs::read(roms.join(name)) else {
                eprintln!("skipping {name}: run scripts/fetch-test-roms.sh");
                continue;
            };
            machines.push((
                name.to_owned(),
                Box::new(move || try_wrong_states(&rom, frames, 1)),
            ));
        }
        mutation::sweep(machines).assert_no_panics();
    }

    #[test]
    fn starts_a_game_with_a_bad_header_checksum() {
        let mut rom = rom_with_program(&[0x18, 0xFE]);
        rom[0x14D] ^= 0xFF;
        let mut gb = Gb::new(&Media::new(&rom)).expect("starts without a boot ROM");
        gb.run_frame(&[Input::default()], &mut Vec::new());
        assert!(
            Gb::new(&Media::new(&rom[..0x14E])).is_ok(),
            "ends after its checksum"
        );
        assert!(Gb::new(&Media::new(&rom[..0x14D])).is_err(), "too short");
    }

    #[test]
    fn a_frame_takes_one_frame_of_cycles() {
        // JR -2: spin forever.
        let rom = rom_with_program(&[0x18, 0xFE]);
        let mut gb = Gb::new(&Media::new(&rom)).expect("valid rom");
        gb.run_frame(&[Input::default()], &mut Vec::new());
        let first = gb.cycles();
        gb.run_frame(&[Input::default()], &mut Vec::new());
        // 154 lines of 456 dots.
        assert_eq!(gb.cycles() - first, 70_224);
    }

    #[test]
    fn switching_the_lcd_off_blanks_the_screen_but_not_the_frames_time() {
        // LD A,0; LDH (LCDC),A; JR -2
        let rom = rom_with_program(&[0x3E, 0x00, 0xE0, 0x40, 0x18, 0xFE]);
        let mut gb = Gb::new(&Media::new(&rom)).expect("valid rom");
        gb.screens[0].pixels_mut().fill(0);
        gb.run_frame(&[Input::default()], &mut Vec::new());
        // A JR takes 12 cycles, so the frame ends within 12 of its time.
        assert!((70_224..70_224 + 12).contains(&gb.cycles()));
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
