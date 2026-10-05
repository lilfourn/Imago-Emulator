//! Nintendo DS core.
//!
//! Two CPUs from `emu-arm` (the ARM9 and the ARM7) run in lockstep slices
//! over the shared hardware in `hardware.rs`. The game direct-boots: the
//! machine loads both programs from the cartridge and starts them as the
//! BIOS and firmware would, and the BIOS is emulated at a high level
//! unless the user provides dumps. See `docs/ROADMAP.md`, Phase 4, for what
//! is emulated so far.

mod bios;
mod boot;
mod bus;
mod cart;
mod display;
mod dma;
mod gpu2d;
mod gpu3d;
mod hardware;
mod interrupt;
mod io;
mod ipc;
mod machine;
mod math;
mod memory;
mod rtc;
mod sound;
mod spi;
mod timer;
mod vram;

use emu_core::{
    Button, Core, CoreInfo, Frame, FrameRate, Input, LoadError, Media, MemoryRegion, ScreenSize,
    StateError, StereoSample, System, SystemFileKind,
};

use cart::Cart;
use display::FRAME_CYCLES;
use hardware::Hardware;
use machine::Machine;
use rtc::Rtc;
use spi::Spi;

const SCREEN: ScreenSize = ScreenSize::new(256, 192);
const SCREENS: [ScreenSize; 2] = [SCREEN, SCREEN];

/// Static facts about the Nintendo DS core. Screen 0 is the top screen and
/// screen 1 the bottom touchscreen.
pub const INFO: CoreInfo = CoreInfo {
    system: System::Nds,
    screens: &SCREENS,
    touch_screen: Some(1),
    players: 1,
    frame_rate: FrameRate::new(33_513_982, 560_190),
    audio_sample_rate: 32_768,
    // Bump whenever any serialized struct changes shape.
    state_version: 2,
};

/// CRC-16 of the Nintendo logo in every licensed cartridge header (GBATEK,
/// "DS Cartridge Header", 0x15C).
const LOGO_CRC: u16 = 0xCF56;

/// A Nintendo DS with a cartridge inserted.
#[derive(Debug)]
pub struct Nds {
    machine: Machine,
    screens: [Frame; 2],
}

impl Nds {
    /// Powers on a DS with `media`'s cartridge and direct-boots it.
    ///
    /// The BIOS is emulated unless `media` holds both the ARM9 (4 KiB) and
    /// ARM7 (16 KiB) BIOS dumps.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::InvalidRom`] if the cartridge header is missing
    /// or its logo checksum is wrong.
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        let invalid = |reason| LoadError::InvalidRom {
            system: System::Nds,
            reason,
        };
        let logo_crc = media
            .rom
            .get(0x15C..0x15E)
            .ok_or(invalid("shorter than the 0x180-byte header"))?;
        if u16::from_le_bytes([logo_crc[0], logo_crc[1]]) != LOGO_CRC {
            return Err(invalid("logo checksum at 0x15C is not 0xCF56"));
        }
        let dumps = media
            .system_file(SystemFileKind::NdsBios9)
            .filter(|bios| bios.len() == 0x1000)
            .zip(
                media
                    .system_file(SystemFileKind::NdsBios7)
                    .filter(|bios| bios.len() == 0x4000),
            );
        let hle_bios = dumps.is_none();
        let (arm9_bios, arm7_bios) = dumps.map_or_else(
            || (bios::arm9_image(), bios::arm7_image()),
            |(arm9, arm7)| (arm9.into(), arm7.into()),
        );
        let mut hw = Hardware::new(arm9_bios, arm7_bios);
        hw.cart = Cart::new(media.rom.into(), media.backup);
        hw.spi = Spi::new(media.system_file(SystemFileKind::NdsFirmware));
        hw.rtc = Rtc::new(media.start_time);
        Ok(Self {
            machine: Machine::boot(hw, hle_bios),
            screens: [Frame::new(SCREEN), Frame::new(SCREEN)],
        })
    }

    fn set_input(&mut self, input: &Input) {
        let hw = &mut self.machine.hw;
        // Button bit order matches KEYINPUT for the first ten buttons.
        hw.keys = !input.buttons.bits() & 0x03FF;
        let held = |button| input.buttons.contains(button);
        let mut ext_keys = 0x007F;
        if held(Button::X) {
            ext_keys &= !1;
        }
        if held(Button::Y) {
            ext_keys &= !2;
        }
        let touch = input
            .touch
            .map(|point| (point.x.min(255) as u8, point.y.min(191) as u8));
        hw.spi.set_touch(touch);
        if hw.spi.pen_down() {
            ext_keys &= !(1 << 6);
        }
        hw.ext_keys = ext_keys;
        hw.check_keypad_interrupt();
    }

    fn present(&mut self) {
        let hw = &self.machine.hw;
        // POWCNT1 bit 15 puts engine A on the top screen.
        let (top, bottom) = if hw.powcnt1 & 0x8000 != 0 {
            (&hw.screen_a, &hw.screen_b)
        } else {
            (&hw.screen_b, &hw.screen_a)
        };
        let [top_frame, bottom_frame] = &mut self.screens;
        top_frame.pixels_mut().copy_from_slice(top);
        bottom_frame.pixels_mut().copy_from_slice(bottom);
    }

    /// Both CPUs' program counters, modes and halt states, and the
    /// interrupt registers, as one `key=value` line for debugging.
    #[must_use]
    pub fn debug_state(&self) -> String {
        let hw = &self.machine.hw;
        let cpu = |name: &str, pc: u32, cpsr: u32, io: &hardware::CpuIo| {
            format!(
                "{name}_pc={pc:08X} {name}_cpsr={cpsr:08X} {name}_halted={} {name}_ime={} {name}_ie={:08X} {name}_if={:08X}",
                io.halted,
                u8::from(io.interrupts.master_enable),
                io.interrupts.enable,
                io.interrupts.flags,
            )
        };
        format!(
            "{} {} vcount={}",
            cpu(
                "arm9",
                self.machine.arm9.reg(15),
                self.machine.arm9.cpsr(),
                &hw.arm9
            ),
            cpu(
                "arm7",
                self.machine.arm7.reg(15),
                self.machine.arm7.cpsr(),
                &hw.arm7
            ),
            hw.vcount,
        )
    }

    /// Bus cycles (33.51 MHz) since power-on.
    #[must_use]
    pub fn cycles(&self) -> u64 {
        self.machine.now()
    }
}

impl Core for Nds {
    fn info(&self) -> CoreInfo {
        INFO
    }

    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>) {
        self.set_input(&inputs.first().copied().unwrap_or_default());
        let start = self.machine.now();
        // A frame ends at VBlank, or after a frame's time if that never
        // comes.
        while self.machine.now() - start < FRAME_CYCLES {
            if self.machine.run_slice(|_, _| {}).frame_done {
                break;
            }
        }
        self.machine.hw.sound.drain_samples(audio);
        self.present();
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        // The cartridge (with its save), firmware and clock carry over.
        let old = &mut self.machine.hw;
        let mut hw = Hardware::new(
            std::mem::take(&mut old.arm9_bios),
            std::mem::take(&mut old.arm7_bios),
        );
        let now = old.scheduler.now();
        std::mem::swap(&mut hw.cart, &mut old.cart);
        std::mem::swap(&mut hw.spi, &mut old.spi);
        std::mem::swap(&mut hw.rtc, &mut old.rtc);
        hw.rtc.restart_cycles(now);
        self.machine = Machine::boot(hw, self.machine.hle_bios);
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        let bytes = postcard::to_stdvec(&self.machine).expect("machine state always serializes");
        out.extend_from_slice(&bytes);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut machine: Machine = postcard::from_bytes(state)
            .map_err(|_| StateError::Malformed("not a Nintendo DS state"))?;
        machine.hw.take_media_from(&mut self.machine.hw);
        self.machine = machine;
        self.present();
        Ok(())
    }

    fn backup_generation(&self, player: usize) -> Option<u64> {
        (player == 0)
            .then(|| self.machine.hw.cart.backup_generation())
            .flatten()
    }

    fn save_backup(&self, player: usize, out: &mut Vec<u8>) {
        if player == 0 {
            self.machine.hw.cart.save_backup(out);
        }
    }

    fn peek(&self, player: usize, address: u32) -> Option<u8> {
        let main_ram = address >> 24 == 0x02;
        (player == 0 && main_ram)
            .then(|| self.machine.hw.main_ram[address as usize & (hardware::MAIN_RAM_SIZE - 1)])
    }

    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool {
        let main_ram = address >> 24 == 0x02;
        if player == 0 && main_ram {
            self.machine.hw.main_ram[address as usize & (hardware::MAIN_RAM_SIZE - 1)] = value;
        }
        player == 0 && main_ram
    }

    fn memory(&self, region: MemoryRegion) -> Option<&[u8]> {
        match region {
            MemoryRegion::MainRam => Some(&self.machine.hw.main_ram),
            MemoryRegion::VideoRam => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cartridge whose ARM9 and ARM7 programs each store a value in main
    /// RAM and loop.
    fn rom() -> Vec<u8> {
        let mut rom = vec![0; 0x400];
        let mut put = |offset: usize, value: u32| {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(0x20, 0x200);
        put(0x24, 0x0200_4000);
        put(0x28, 0x0200_4000);
        put(0x2C, 16);
        put(0x30, 0x300);
        put(0x34, 0x0238_0000);
        put(0x38, 0x0238_0000);
        put(0x3C, 16);
        // MOV R0, #0x02000000; MOV R1, #42; STR R1, [R0]; B .
        for (index, word) in [0xE3A0_0402, 0xE3A0_102A, 0xE580_1000, 0xEAFF_FFFE]
            .into_iter()
            .enumerate()
        {
            put(0x200 + index * 4, word);
        }
        // MOV R0, #0x02000000; MOV R1, #7; STR R1, [R0, #4]; B .
        for (index, word) in [0xE3A0_0402, 0xE3A0_1007, 0xE580_1004, 0xEAFF_FFFE]
            .into_iter()
            .enumerate()
        {
            put(0x300 + index * 4, word);
        }
        rom[0x15C..0x15E].copy_from_slice(&LOGO_CRC.to_le_bytes());
        rom
    }

    #[test]
    fn rejects_a_bad_logo_checksum() {
        assert!(Nds::new(&Media::new(&[0; 0x200])).is_err());
    }

    #[test]
    fn both_cpus_run_their_programs() {
        let rom = rom();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        nds.run_frame(&[], &mut Vec::new());
        let word = |address: u32| {
            u32::from_le_bytes([0, 1, 2, 3].map(|byte| nds.peek(0, address + byte).unwrap_or(0)))
        };
        assert_eq!(word(0x0200_0000), 42);
        assert_eq!(word(0x0200_0004), 7);
    }

    #[test]
    fn a_frame_takes_one_frame_of_cycles() {
        let rom = rom();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        nds.run_frame(&[], &mut Vec::new());
        let first = nds.cycles();
        nds.run_frame(&[], &mut Vec::new());
        assert_eq!(nds.cycles() - first, FRAME_CYCLES);
    }

    #[test]
    fn save_states_round_trip() {
        let rom = rom();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        nds.run_frame(&[], &mut Vec::new());
        let mut state = Vec::new();
        nds.save_state(&mut state);
        let mut other = Nds::new(&Media::new(&rom)).expect("valid header");
        other.load_state(&state).expect("loads");
        assert_eq!(other.cycles(), nds.cycles());
        assert_eq!(other.peek(0, 0x0200_0000), Some(42));
    }
}
