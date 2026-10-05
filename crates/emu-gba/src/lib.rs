//! Game Boy Advance core.
//!
//! An ARM7TDMI from `emu-arm` on the GBA's memory map, with a scheduler
//! driving the PPU. The BIOS is emulated at a high level unless the user
//! provides a dump. See `docs/ROADMAP.md`, Phase 3, for what is emulated so
//! far.

mod apu;
mod backup;
mod bios;
mod bus;
mod cartridge;
mod dma;
mod link;
mod ppu;
mod rtc;
mod sio;
mod timer;
mod timing;

use emu_arm::{Access, Cpu, Mode, Registers};
use emu_core::{
    Core, CoreInfo, Frame, FrameRate, Input, LoadError, Media, MemoryRegion, ScreenSize,
    StateError, StereoSample, System, SystemFileKind,
};
use serde::{Deserialize, Serialize};

use bus::Bus;
use cartridge::Cartridge;

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
    state_version: 6,
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
    /// fixed value (GBATEK, "GBA Cartridge Header", 0xB2).
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
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
        let Machine { cpu, bus } = &mut self.machine;
        if bus.run_pending_dma() {
            // The CPU waited for the transfer.
        } else if !bus.wake_if_requested() {
            bus.idle_to_next_event();
        } else if self.hle_bios
            && cpu.reg(15) == bios::SWI_ENTERED_PC
            && !cpu.thumb()
            && cpu.mode() == Mode::Supervisor
        {
            bios::call(cpu, bus);
        } else {
            cpu.set_irq_line(bus.irq_line());
            cpu.step(bus);
        }
        if self.machine.bus.ppu.take_frame_ready() {
            self.present();
            self.frames += 1;
        }
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
        self.set_input(&inputs.first().copied().unwrap_or_default());
        let start = self.cycles();
        let frame = self.frames;
        // A frame ends at VBlank; with the display off, after a frame's time.
        while self.frames == frame && self.cycles() - start < CYCLES_PER_FRAME {
            self.step();
        }
        self.machine.bus.apu.drain_samples(audio);
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
        let bytes = postcard::to_stdvec(&self.machine).expect("machine state always serializes");
        out.extend_from_slice(&bytes);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut machine: Machine = postcard::from_bytes(state)
            .map_err(|_| StateError::Malformed("not a Game Boy Advance state"))?;
        machine.bus.take_media_from(&mut self.machine.bus);
        self.machine = machine;
        self.present();
        Ok(())
    }

    fn backup_generation(&self, player: usize) -> Option<u64> {
        (player == 0)
            .then(|| self.machine.bus.cartridge.backup_generation())
            .flatten()
    }

    fn save_backup(&self, player: usize, out: &mut Vec<u8>) {
        if player == 0 {
            self.machine.bus.cartridge.save_backup(out);
        }
    }

    fn peek(&self, player: usize, address: u32) -> Option<u8> {
        (player == 0)
            .then(|| self.machine.bus.peek_ram(address))
            .flatten()
    }

    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool {
        player == 0 && self.machine.bus.poke_ram(address, value)
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
