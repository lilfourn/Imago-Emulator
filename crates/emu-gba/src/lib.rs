//! Game Boy Advance core.
//!
//! An ARM7TDMI from `emu-arm` on the GBA's memory map, with a scheduler
//! driving the PPU. The BIOS is emulated at a high level unless the user
//! provides a dump. See `docs/ROADMAP.md`, Phase 3, for what is emulated so
//! far.

mod apu;
mod bios;
mod bus;
mod debug;
mod dma;
mod graphics;
mod interrupts;
mod io_names;
mod link;
mod ppu;
mod prefetch;
mod sio;
mod timer;
mod timing;

use emu_arm::{Access, Cpu, Mode, Registers};
use emu_core::debug::{RunOutcome, Stop};
use emu_core::{
    Core, CoreInfo, Debugger, Frame, FrameRate, Input, LoadError, Media, MemoryRegion, RomPatch,
    ScreenSize, StateError, StereoSample, System, SystemFileKind,
};
use emu_gba_cart::Cartridge;
use emu_state::payload;
use serde::{Deserialize, Serialize};

use bus::Bus;

pub use link::Linked;

const SCREENS: [ScreenSize; 1] = [ScreenSize::new(240, 160)];

/// Cycles in one video frame: 228 lines of 1,232 cycles.
const CYCLES_PER_FRAME: u64 = 280_896;

/// Static facts about the Game Boy Advance core.
pub const INFO: CoreInfo = CoreInfo {
    system: System::Gba,
    screens: &SCREENS,
    touch_screen: None,
    players: 1,
    frame_rate: FrameRate::new(16_777_216, 280_896),
    audio_sample_rate: apu::SAMPLE_RATE,
    // Bump whenever any serialized struct changes shape.
    state_version: 12,
};

/// A Game Boy Advance with a cartridge inserted.
#[derive(Debug)]
pub struct Gba {
    machine: Machine,
    /// The BIOS is emulated in Rust rather than run from a dump.
    hle_bios: bool,
    screens: [Frame; 1],
    /// Frames completed since power-on.
    frames: u64,
}

/// Everything a save state captures.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Machine {
    cpu: Cpu,
    bus: Bus,
}

impl Gba {
    /// Powers on a Game Boy Advance with `media`'s cartridge.
    ///
    /// With a BIOS dump in `media` the console boots through it, intro and
    /// all. Without one it starts the game directly, in the state the BIOS
    /// would leave, and emulates BIOS calls itself. The save memory type
    /// comes from the ROM, filled from `media.backup` if given.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::InvalidRom`] if the cartridge header lacks its
    /// fixed value (GBATEK, "GBA Cartridge Header", 0xB2), or
    /// [`LoadError::Unsupported`] if `media` has a second-slot cartridge,
    /// which a GBA has no slot for.
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        if media.second_slot.is_some() {
            return Err(LoadError::Unsupported("a second cartridge slot"));
        }
        let invalid = |reason| LoadError::InvalidRom {
            system: System::Gba,
            reason,
        };
        let fixed = media
            .rom
            .get(0xB2)
            .ok_or(invalid("shorter than the 192-byte header"))?;
        if *fixed != 0x96 {
            return Err(invalid("header fixed value at 0xB2 is not 0x96"));
        }
        let cartridge = Cartridge::new(
            media.rom.to_vec().into_boxed_slice(),
            media.backup,
            media.start_time,
        );
        let dump = media
            .system_file(SystemFileKind::GbaBios)
            .filter(|bios| bios.len() == 0x4000);
        let hle_bios = dump.is_none();
        let bios = dump.map_or_else(bios::image, |bios| bios.to_vec().into_boxed_slice());
        let mut gba = Self {
            machine: Machine {
                cpu: Cpu::default(),
                bus: Bus::new(cartridge, bios),
            },
            hle_bios,
            screens: [Frame::new(SCREENS[0])],
            frames: 0,
        };
        gba.power_on();
        Ok(gba)
    }

    fn power_on(&mut self) {
        let Machine { cpu, bus } = &mut self.machine;
        if self.hle_bios {
            // The state the BIOS leaves when it hands over to the game.
            let mut registers = Registers {
                cpsr: Mode::System.bits(),
                ..Registers::default()
            };
            registers.r[13] = 0x0300_7F00;
            registers.usr[5] = 0x0300_7F00;
            registers.svc[0] = 0x0300_7FE0;
            registers.irq[0] = 0x0300_7FA0;
            *cpu = Cpu::from_registers(&registers, [0, 0], Access::NONSEQUENTIAL);
            cpu.jump(bus, 0x0800_0000);
        } else {
            cpu.reset(bus);
        }
    }

    /// Runs one instruction (or one stretch of HALT, or one HLE BIOS call).
    pub fn step(&mut self) {
        self.step_with(|cpu, bus| cpu.step(bus));
    }

    /// One step, as [`Gba::step`], with `execute` running the CPU's
    /// instruction if that is what the step does. Returns whether the CPU
    /// ran something (an instruction or an HLE BIOS call) rather than
    /// sitting in HALT, waiting for an interrupt or a DMA.
    fn step_with(&mut self, execute: impl FnOnce(&mut Cpu, &mut Bus)) -> bool {
        let Machine { cpu, bus } = &mut self.machine;
        let ran = if !bus.wake_if_requested() {
            // Halted: DMA still runs; otherwise nothing happens until the
            // next event. (A running CPU lets DMA in at its next access.)
            if bus.run_dma() == 0 {
                bus.idle_to_next_event();
            }
            false
        } else if self.hle_bios
            && cpu.reg(15) == bios::SWI_ENTERED_PC
            && !cpu.thumb()
            && cpu.mode() == Mode::Supervisor
        {
            bios::call(cpu, bus);
            true
        } else {
            cpu.set_irq_line(bus.irq_line());
            execute(cpu, bus);
            true
        };
        if self.machine.bus.ppu.take_frame_ready() {
            self.present();
            self.frames += 1;
        }
        ran
    }

    /// Runs `step` until a frame finishes or a step stops, for
    /// [`Core::run_frame`] and [`Debugger::run_until`]. A frame ends at
    /// VBlank; with the display off, after a frame's time.
    fn run(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        mut step: impl FnMut(&mut Self) -> Option<Stop>,
    ) -> RunOutcome {
        self.set_input(&inputs.first().copied().unwrap_or_default());
        let start = self.cycles();
        let frame = self.frames;
        let frame_done =
            |gba: &Self| gba.frames != frame || gba.cycles() - start >= CYCLES_PER_FRAME;
        let mut stop = None;
        while stop.is_none() && !frame_done(self) {
            stop = step(self);
        }
        let frame_done = frame_done(self);
        if frame_done {
            self.machine.bus.apu.drain_samples(audio);
        }
        RunOutcome { frame_done, stop }
    }

    /// The CPU, for test harnesses and debuggers.
    #[must_use]
    pub fn cpu(&self) -> &Cpu {
        &self.machine.cpu
    }

    /// Clock cycles (16.78 MHz) since power-on.
    #[must_use]
    pub fn cycles(&self) -> u64 {
        self.machine.bus.cycles()
    }

    fn present(&mut self) {
        let [screen] = &mut self.screens;
        screen
            .pixels_mut()
            .copy_from_slice(self.machine.bus.ppu.pixels());
    }

    fn set_input(&mut self, input: &Input) {
        // Button bit order already matches KEYINPUT.
        self.machine.bus.set_buttons(input.buttons.bits() & 0x03FF);
    }
}

impl Core for Gba {
    fn info(&self) -> CoreInfo {
        INFO
    }

    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>) {
        self.run(inputs, audio, |gba| {
            gba.step();
            None
        });
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        self.machine = Machine {
            cpu: Cpu::default(),
            bus: Bus::reset_from(&mut self.machine.bus),
        };
        self.power_on();
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        payload::append(&self.machine, out);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut machine: Machine = payload::load(state, "not a Game Boy Advance state")?;
        machine.bus.fits()?;
        machine.bus.take_media_from(&mut self.machine.bus);
        self.machine = machine;
        self.present();
        Ok(())
    }

    fn backup_generation(&self, cartridge: usize) -> Option<u64> {
        (cartridge == 0)
            .then(|| self.machine.bus.cartridge.backup_generation())
            .flatten()
    }

    fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
        if cartridge == 0 {
            self.machine.bus.cartridge.save_backup(out);
        }
    }

    /// EWRAM, IWRAM and KEYINPUT.
    fn peek(&self, player: usize, address: u32) -> Option<u8> {
        let bus = &self.machine.bus;
        (player == 0)
            .then(|| bus.peek_ram(address).or_else(|| bus.peek_keys(address)))
            .flatten()
    }

    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool {
        player == 0 && self.machine.bus.poke_ram(address, value)
    }

    /// I/O registers at 0x04000000-0x040003FE, but not POSTFLG and
    /// HALTCNT.
    fn poke_io_16(&mut self, player: usize, address: u32, value: u16) -> bool {
        player == 0 && self.machine.bus.poke_io_16(address, value)
    }

    /// Patches apply in the ROM window, 0x08000000-0x0DFFFFFF, at their
    /// offset in every wait-state mirror; others are dropped.
    fn set_rom_patches(&mut self, player: usize, patches: &[RomPatch]) {
        if player == 0 {
            self.machine.bus.set_rom_patches(patches);
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

    /// Loads into a GBA playing `rom` the wrong states made from its own
    /// after `frames` frames ([`mutation::try_each`]), and runs each it
    /// takes: a frame, the memory a debugger reads, a save, a reset and
    /// a frame more.
    fn try_wrong_states(rom: &[u8], frames: usize, every: usize) -> mutation::Outcome {
        let mut gba = Gba::new(&Media::new(rom)).expect("valid rom");
        let mut audio = Vec::new();
        for _ in 0..frames {
            gba.run_frame(&[], &mut audio);
            audio.clear();
        }
        let mut good = Vec::new();
        gba.save_state(&mut good);
        gba.load_state(&good).expect("its own state loads");
        let machine = gba.machine.clone();
        mutation::try_each(&machine, every, |wrong| {
            if gba.load_state(wrong).is_err() {
                return;
            }
            let mut audio = Vec::new();
            gba.run_frame(&[], &mut audio);
            for address in (0..0x1000_0000u32).step_by(0x3_3331) {
                let _ = gba.peek(0, address);
            }
            let _ = gba.memory(MemoryRegion::MainRam);
            let _ = gba.memory(MemoryRegion::VideoRam);
            let mut saved = Vec::new();
            gba.save_state(&mut saved);
            let mut backup = Vec::new();
            gba.save_backup(0, &mut backup);
            gba.reset();
            gba.run_frame(&[], &mut audio);
        })
    }

    /// A ROM that spins after writing a byte to save memory, with `code`
    /// as its game code and `library` as its save library's name: what
    /// picks its save chip and clock.
    fn rom_with_a_save(code: &[u8; 4], library: &[u8]) -> Vec<u8> {
        // MOV R0, #0x0E000000; STRB R0, [R0]; B .
        let mut rom = rom(&[0xE3A0_040E, 0xE5C0_0000, 0xEAFF_FFFE]);
        rom[0xAC..0xB0].copy_from_slice(code);
        rom[0x100..0x100 + library.len()].copy_from_slice(library);
        rom
    }

    /// A state is written after whatever the caller's buffer holds, as
    /// the app's rewind snapshot counts on, and is the same bytes at any
    /// place in it.
    #[test]
    fn a_state_is_appended_to_what_the_buffer_holds() {
        let rom = rom(&[0xEAFF_FFFE]);
        let mut core = Gba::new(&Media::new(&rom)).expect("valid rom");
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
        let rom = rom(&[0xEAFF_FFFE]);
        let media = Media::new(&rom);
        let mut one = Gba::new(&media).expect("valid rom");
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

    /// As [`try_wrong_states`], for two GBAs playing `rom` on a link cable:
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
    /// it never panics or hangs the core. This tries a sample of the
    /// places; `every_wrong_state_is_refused_or_runs` tries them all.
    #[test]
    fn a_wrong_state_is_refused_or_runs() {
        // Pokémon Emerald's code: 128 KiB of Flash and the clock.
        let outcome = try_wrong_states(&rom_with_a_save(b"BPEE", b""), 3, 5);
        assert!(outcome.tried > 300, "{} states", outcome.tried);
        outcome.assert_no_panics();
        let outcome = try_wrong_pair_states(&rom_with_a_save(b"BPEE", b""), 3, 29);
        assert!(outcome.tried > 100, "{} states", outcome.tried);
        outcome.assert_no_panics();
    }

    /// Every place in the states of a cartridge of each kind, and of the
    /// test ROMs that use the most of the machine, if they are there
    /// (`scripts/fetch-test-roms.sh`, `scripts/fetch-mgba-suite.sh`), each
    /// on a thread of its own. Several minutes.
    #[test]
    #[ignore = "minutes: every byte of several states' structure"]
    fn every_wrong_state_is_refused_or_runs() {
        let mut machines: Vec<(String, Box<dyn FnOnce() -> mutation::Outcome + Send>)> = Vec::new();
        for (code, library) in [
            (b"BPEE", &b""[..]),
            (b"TEST", b"SRAM_V"),
            (b"TEST", b"EEPROM_V"),
            (b"TEST", b"FLASH_V"),
            (b"TEST", b""),
        ] {
            let name = format!(
                "{} {}",
                String::from_utf8_lossy(code),
                String::from_utf8_lossy(library)
            );
            let rom = rom_with_a_save(code, library);
            machines.push((name, Box::new(move || try_wrong_states(&rom, 3, 1))));
        }
        let linked = rom_with_a_save(b"BPEE", b"");
        machines.push((
            "a linked pair".to_owned(),
            Box::new(move || try_wrong_pair_states(&linked, 3, 1)),
        ));
        let roms = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-roms");
        for (name, frames) in [
            ("mgba-suite/suite.gba", 200),
            ("armwrestler-gba/armwrestler-gba-fixed.gba", 300),
            ("gba-tests/bios/bios.gba", 20),
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

    /// A ROM whose code at 0x08000000 is `program` (ARM), with a valid
    /// header fixed value.
    fn rom(program: &[u32]) -> Vec<u8> {
        let mut rom = vec![0; 0x200];
        for (index, word) in program.iter().enumerate() {
            rom[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        rom[0xB2] = 0x96;
        rom
    }

    #[test]
    fn rejects_a_missing_fixed_value() {
        assert!(Gba::new(&Media::new(&[0; 0xC0])).is_err());
    }

    #[test]
    fn direct_boot_runs_rom_code() {
        // MOV R0, #42; B .
        let rom = rom(&[0xE3A0_002A, 0xEAFF_FFFE]);
        let mut gba = Gba::new(&Media::new(&rom)).expect("valid rom");
        for _ in 0..4 {
            gba.step();
        }
        assert_eq!(gba.cpu().reg(0), 42);
        assert_eq!(gba.cpu().reg(13), 0x0300_7F00);
    }

    #[test]
    fn a_frame_takes_one_frame_of_cycles() {
        let rom = rom(&[0xEAFF_FFFE]); // B .
        let mut gba = Gba::new(&Media::new(&rom)).expect("valid rom");
        gba.run_frame(&[], &mut Vec::new());
        let first = gba.cycles();
        gba.run_frame(&[], &mut Vec::new());
        let length = gba.cycles() - first;
        assert!(
            (CYCLES_PER_FRAME - 8..=CYCLES_PER_FRAME + 8).contains(&length),
            "{length}"
        );
    }

    #[test]
    fn cheats_can_read_the_keypad() {
        let rom = rom(&[0xEAFF_FFFE]); // B .
        let mut gba = Gba::new(&Media::new(&rom)).expect("valid rom");
        let mut input = Input::default();
        input.buttons.set(emu_core::Button::L, true);
        gba.run_frame(&[input], &mut Vec::new());
        // KEYINPUT is active low: L is bit 9.
        let keys = [0x0400_0130, 0x0400_0131].map(|address| gba.peek(0, address));
        assert_eq!(keys, [Some(0xFF), Some(0x01)]);
        assert_eq!(gba.peek(0, 0x0400_0132), None);
        assert_eq!(gba.peek(1, 0x0400_0130), None);
    }

    #[test]
    fn hle_swi_divides() {
        // MOV R0, #100; MOV R1, #7; SWI 0x060000; B .
        let rom = rom(&[0xE3A0_0064, 0xE3A0_1007, 0xEF06_0000, 0xEAFF_FFFE]);
        let mut gba = Gba::new(&Media::new(&rom)).expect("valid rom");
        for _ in 0..8 {
            gba.step();
        }
        assert_eq!((gba.cpu().reg(0), gba.cpu().reg(1)), (14, 2));
        assert_eq!(gba.cpu().mode(), Mode::System);
    }
}
