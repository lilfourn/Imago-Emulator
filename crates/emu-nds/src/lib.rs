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
mod capture;
mod cart;
mod debug;
mod display;
mod dma;
mod gba_slot;
mod gpu2d;
mod gpu3d;
mod graphics;
mod hardware;
mod interrupt;
mod io;
mod io_names;
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
    Button, Core, CoreInfo, Debugger, Frame, FrameRate, Input, LoadError, Media, MemoryRegion,
    ScreenSize, StateError, StereoSample, System, SystemFileKind,
};
use emu_gba_cart::Cartridge as GbaCartridge;

use cart::Cart;
use display::FRAME_CYCLES;
use gba_slot::GbaSlot;
use hardware::Hardware;
use machine::{Machine, Plain, Stepper};
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
    state_version: 6,
};

/// A Nintendo DS with a game card inserted, and maybe a GBA cartridge.
#[derive(Debug)]
pub struct Nds {
    machine: Machine,
    screens: [Frame; 2],
}

impl Nds {
    /// Powers on a DS with `media`'s cartridge and direct-boots it.
    /// `media.second_slot` goes in the GBA slot, where DS games can read a
    /// GBA game and its save.
    ///
    /// The BIOS is emulated unless `media` holds both the ARM9 (4 KiB) and
    /// ARM7 (16 KiB) BIOS dumps.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::InvalidRom`] if the cartridge header is missing
    /// or either program lies outside the ROM. The Nintendo logo is not
    /// checked, so homebrew without it runs too.
    pub fn new(media: &Media<'_>) -> Result<Self, LoadError> {
        let invalid = |reason| LoadError::InvalidRom {
            system: System::Nds,
            reason,
        };
        if media.rom.len() < 0x200 {
            return Err(invalid("shorter than the 0x200-byte header"));
        }
        let header = boot::Header::parse(media.rom);
        let fits = |program: boot::Program| {
            (program.rom_offset as usize)
                .checked_add(program.size as usize)
                .is_some_and(|end| end <= media.rom.len())
        };
        if !fits(header.arm9) || !fits(header.arm7) {
            return Err(invalid("a program in the header lies outside the ROM"));
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
        let (mut arm9_bios, arm7_bios) = dumps.map_or_else(
            || (bios::arm9_image(), bios::arm7_image()),
            |(arm9, arm7)| (arm9.into(), arm7.into()),
        );
        let mut gba_slot = GbaSlot::new(media.second_slot.map(|cartridge| {
            GbaCartridge::new(cartridge.rom.into(), cartridge.backup, media.start_time)
        }));
        if hle_bios && let Some(logo) = gba_slot.bios_logo() {
            arm9_bios[gba_slot::BIOS_LOGO].copy_from_slice(&logo);
        }
        let mut hw = Hardware::new(arm9_bios, arm7_bios);
        hw.cart = Cart::new(media.rom.into(), media.backup);
        hw.gba_slot = gba_slot;
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
        hw.spi.set_microphone(input.microphone);
        if hw.spi.pen_down() {
            ext_keys &= !(1 << 6);
        }
        hw.ext_keys = ext_keys;
        hw.check_keypad_interrupt();
    }

    /// Copies the engines' screens, or their larger versions with
    /// upscaled 3D, into the frames.
    fn present(&mut self) {
        let hw = &self.machine.hw;
        let (a, b) = if hw.scaled.scale > 1 {
            (&hw.scaled.a, &hw.scaled.b)
        } else {
            (&hw.screen_a, &hw.screen_b)
        };
        // POWCNT1 bit 15 puts engine A on the top screen.
        let (top, bottom) = if hw.powcnt1 & 0x8000 != 0 {
            (a, b)
        } else {
            (b, a)
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

    /// BIOS functions each CPU has called, with counts, for debugging.
    #[must_use]
    pub fn debug_system_calls(&self) -> String {
        let describe = |name: &str, counts: &[u32; 0x20]| {
            let calls: Vec<String> = (0..0x20)
                .filter(|number| counts[*number] > 0)
                .map(|number| format!("{number:02X}x{}", counts[number]))
                .collect();
            format!("{name}_swi={}", calls.join(","))
        };
        let [arm9, arm7] = &self.machine.system_calls;
        format!("{} {}", describe("arm9", arm9), describe("arm7", arm7))
    }

    /// Video registers, as one `key=value` line for debugging.
    #[must_use]
    pub fn debug_video(&self) -> String {
        let hw = &self.machine.hw;
        let dispcnt = |engine: &gpu2d::Engine2d| {
            u32::from(engine.read_register(0)) | u32::from(engine.read_register(2)) << 16
        };
        let banks: Vec<String> = (0..9)
            .map(|bank| format!("{:02X}", hw.vram.control(bank)))
            .collect();
        format!(
            "powcnt1={:04X} dispcnt_a={:08X} dispcnt_b={:08X} bright_a={:04X} bright_b={:04X} vramcnt={} disp3dcnt={:04X} bgcnt_a={:04X},{:04X},{:04X},{:04X}",
            hw.powcnt1,
            dispcnt(&hw.engine_a),
            dispcnt(&hw.engine_b),
            hw.engine_a.read_register(0x6C),
            hw.engine_b.read_register(0x6C),
            banks.join(","),
            hw.gpu3d.read_register(0x60) & 0xFFFF,
            hw.engine_a.read_register(0x08),
            hw.engine_a.read_register(0x0A),
            hw.engine_a.read_register(0x0C),
            hw.engine_a.read_register(0x0E),
        )
    }

    /// The ARM9's and ARM7's R0–R15, for debugging.
    #[must_use]
    pub fn debug_registers(&self) -> [[u32; 16]; 2] {
        [
            std::array::from_fn(|index| self.machine.arm9.reg(index)),
            std::array::from_fn(|index| self.machine.arm7.reg(index)),
        ]
    }

    /// Bus cycles (33.51 MHz) since power-on.
    #[must_use]
    pub fn cycles(&self) -> u64 {
        self.machine.now()
    }

    /// Runs slices, each instruction by `stepper`, until a frame finishes
    /// or `stepper` stops one part-way, for [`Core::run_frame`] and
    /// [`Debugger::run_until`]. Returns whether the frame finished; the
    /// next call carries on after a stop.
    fn run<S: Stepper>(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stepper: &mut S,
    ) -> bool {
        self.set_input(&inputs.first().copied().unwrap_or_default());
        if self.machine.hw.spi.powered_off() {
            // The game switched the console off: dark screens, no sound.
            for screen in &mut self.screens {
                screen.pixels_mut().fill(0xFF00_0000);
            }
            return true;
        }
        let start = self.machine.now();
        // A frame ends at VBlank, or after a frame's time if that never
        // comes.
        while self.machine.now() - start < FRAME_CYCLES {
            let progress = self.machine.run_slice_with(stepper, |_, _| {});
            if progress.stopped {
                return false;
            }
            if progress.frame_done {
                break;
            }
        }
        self.machine.hw.sound.drain_samples(audio);
        self.present();
        true
    }
}

impl Core for Nds {
    fn info(&self) -> CoreInfo {
        INFO
    }

    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>) {
        self.run(inputs, audio, &mut Plain);
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    /// Up to 8 times the native resolution. The game's own 3D reads (display
    /// capture) still see the native frame, which is drawn as well.
    fn set_render_scale(&mut self, scale: u32) {
        let scale = usize::try_from(scale).unwrap_or(usize::MAX);
        let hw = &mut self.machine.hw;
        hw.set_render_scale(scale);
        let scale = hw.scaled.scale as u16;
        let size = ScreenSize::new(SCREEN.width * scale, SCREEN.height * scale);
        self.screens = [Frame::new(size), Frame::new(size)];
        self.present();
    }

    fn reset(&mut self) {
        // The cartridges (with their saves), firmware and clock carry over.
        let old = &mut self.machine.hw;
        let mut hw = Hardware::new(
            std::mem::take(&mut old.arm9_bios),
            std::mem::take(&mut old.arm7_bios),
        );
        let now = old.scheduler.now();
        std::mem::swap(&mut hw.cart, &mut old.cart);
        std::mem::swap(&mut hw.spi, &mut old.spi);
        std::mem::swap(&mut hw.rtc, &mut old.rtc);
        std::mem::swap(&mut hw.gba_slot, &mut old.gba_slot);
        hw.rtc.restart_cycles(now);
        hw.gba_slot.restart_cycles(now);
        hw.take_display_from(old);
        self.machine = Machine::boot(hw, self.machine.hle_bios);
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        let bytes = postcard::to_stdvec(&self.machine).expect("machine state always serializes");
        out.extend_from_slice(&bytes);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut machine: Machine = postcard::from_bytes(state)
            .map_err(|_| StateError::Malformed("not a Nintendo DS state"))?;
        if machine.hw.gba_slot.is_occupied() != self.machine.hw.gba_slot.is_occupied() {
            return Err(StateError::Malformed(
                "the state's GBA slot does not match this session's",
            ));
        }
        machine.hw.take_media_from(&mut self.machine.hw);
        machine.hw.take_display_from(&mut self.machine.hw);
        self.machine = machine;
        // The rendered 3D image is not saved.
        let hw = &mut self.machine.hw;
        hw.gpu3d.render_frame(&hw.vram);
        self.present();
        Ok(())
    }

    /// Cartridge 0 is the game card, 1 the GBA slot's cartridge.
    fn backup_generation(&self, cartridge: usize) -> Option<u64> {
        let hw = &self.machine.hw;
        match cartridge {
            0 => hw.cart.backup_generation(),
            1 => hw.gba_slot.backup_generation(),
            _ => None,
        }
    }

    fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
        let hw = &self.machine.hw;
        match cartridge {
            0 => hw.cart.save_backup(out),
            1 => hw.gba_slot.save_backup(out),
            _ => {}
        }
    }

    /// Main RAM, which both CPUs share, at 0x02000000–0x02FFFFFF with its
    /// mirrors; and for cheats, the ARM7's timers, KEYINPUT and EXTKEYIN
    /// (see `Hardware::peek_cheat_register`).
    fn peek(&self, player: usize, address: u32) -> Option<u8> {
        let hw = &self.machine.hw;
        match address >> 24 {
            _ if player != 0 => None,
            0x02 => Some(hw.main_ram[address as usize & (hardware::MAIN_RAM_SIZE - 1)]),
            _ => hw.peek_cheat_register(address),
        }
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

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
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
        rom
    }

    /// A halfword through [`Core::peek`].
    fn peek_half(nds: &Nds, address: u32) -> u16 {
        u16::from_le_bytes([address, address + 1].map(|byte| nds.peek(0, byte).unwrap_or(0)))
    }

    #[test]
    fn cheats_can_read_the_keys_and_the_arm7_timers() {
        let rom = rom();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        let mut input = Input::default();
        input.buttons.set(Button::L, true);
        input.buttons.set(Button::Y, true);
        nds.run_frame(&[input], &mut Vec::new());
        // Both active low: L is KEYINPUT bit 9, Y is EXTKEYIN bit 1.
        assert_eq!(peek_half(&nds, 0x0400_0130), 0x01FF);
        assert_eq!(peek_half(&nds, 0x0400_0136), 0x007D);
        // Start ARM7 timer 0 (prescaler 1) and let it count.
        nds.machine.hw.arm7_write_16(0x0400_0102, 0x0080);
        let before = peek_half(&nds, 0x0400_0100);
        nds.run_frame(&[input], &mut Vec::new());
        assert_ne!(peek_half(&nds, 0x0400_0100), before);
        assert_eq!(peek_half(&nds, 0x0400_0102), 0x0080);
        assert_eq!(nds.peek(0, 0x0400_0000), None, "other I/O stays hidden");
        assert_eq!(nds.peek(0, 0x0400_0110), None);
    }

    #[test]
    fn rejects_programs_outside_the_rom() {
        assert!(Nds::new(&Media::new(&[0; 0x100])).is_err());
        let mut rom = rom();
        rom[0x2C..0x30].copy_from_slice(&0x1000u32.to_le_bytes());
        assert!(Nds::new(&Media::new(&rom)).is_err());
    }

    #[test]
    fn the_frame_s_microphone_reaches_the_touchscreen_controller() {
        let rom = rom();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        // libnds's micReadData12 through SPICNT and SPIDATA: control byte
        // 0xE4 with chip select held, then two reads.
        let read_microphone = |nds: &mut Nds| {
            let hw = &mut nds.machine.hw;
            hw.arm7_write_16(0x0400_01C0, 0x8A01);
            hw.arm7_write_16(0x0400_01C2, 0xE4);
            hw.arm7_write_16(0x0400_01C2, 0);
            let high = hw.arm7_read_16(0x0400_01C2);
            hw.arm7_write_16(0x0400_01C0, 0x8201);
            hw.arm7_write_16(0x0400_01C2, 0);
            let low = hw.arm7_read_16(0x0400_01C2);
            (high & 0x7F) << 5 | low >> 3
        };
        nds.run_frame(&[Input::default()], &mut Vec::new());
        assert_eq!(read_microphone(&mut nds), 0x800, "silence");
        let blowing = Input {
            microphone: Input::LOUDEST,
            ..Input::default()
        };
        let readings: Vec<u16> = (0..4)
            .map(|_| {
                nds.run_frame(&[blowing], &mut Vec::new());
                read_microphone(&mut nds)
            })
            .collect();
        assert!(
            readings
                .iter()
                .all(|&value| value != 0x800 && value <= 0xFFF),
            "{readings:X?}"
        );
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

    /// [`rom`] with an ARM9 program that turns the top screen white.
    fn white_screen_rom() -> Vec<u8> {
        let mut rom = rom();
        // MOV R0, #0x04000000; MOV R1, #0x10000; STR R1, [R0] (DISPCNT:
        // display on); MOV R0, #0x05000000; MVN R1, #0; STRH R1, [R0]
        // (white backdrop); B .
        let program: [u32; 7] = [
            0xE3A0_0301,
            0xE3A0_1801,
            0xE580_1000,
            0xE3A0_0405,
            0xE3E0_1000,
            0xE1C0_10B0,
            0xEAFF_FFFE,
        ];
        rom[0x2C..0x30].copy_from_slice(&28u32.to_le_bytes());
        for (index, word) in program.into_iter().enumerate() {
            rom[0x200 + index * 4..0x204 + index * 4].copy_from_slice(&word.to_le_bytes());
        }
        rom
    }

    #[test]
    fn the_arm9_program_can_paint_the_top_screen() {
        let rom = white_screen_rom();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        nds.run_frame(&[], &mut Vec::new());
        nds.run_frame(&[], &mut Vec::new());
        assert_eq!(nds.screens()[0].pixels()[0], 0xFFFF_FFFF);
    }

    #[test]
    fn render_scale_enlarges_the_screens_but_not_the_state() {
        let rom = white_screen_rom();
        let mut native = Nds::new(&Media::new(&rom)).expect("valid header");
        let mut larger = Nds::new(&Media::new(&rom)).expect("valid header");
        larger.set_render_scale(3);
        for _ in 0..3 {
            native.run_frame(&[], &mut Vec::new());
            larger.run_frame(&[], &mut Vec::new());
        }
        let top = &larger.screens()[0];
        assert_eq!(top.size(), ScreenSize::new(768, 576));
        assert!(top.pixels().iter().all(|&pixel| pixel == 0xFFFF_FFFF));
        let (mut native_state, mut larger_state) = (Vec::new(), Vec::new());
        native.save_state(&mut native_state);
        larger.save_state(&mut larger_state);
        assert!(native_state == larger_state, "the scale is not saved");

        // States load at any scale, and the scale stays.
        larger.load_state(&native_state).expect("loads");
        assert_eq!(larger.screens()[1].size(), ScreenSize::new(768, 576));
        larger.reset();
        larger.run_frame(&[], &mut Vec::new());
        assert_eq!(larger.screens()[0].size(), ScreenSize::new(768, 576));
        larger.set_render_scale(0);
        assert_eq!(larger.screens()[0].size(), SCREEN);
        larger.set_render_scale(100);
        assert_eq!(larger.screens()[0].size(), ScreenSize::new(2048, 1536));
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
        let rom = white_screen_rom();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        nds.run_frame(&[], &mut Vec::new());
        nds.run_frame(&[], &mut Vec::new());
        let mut state = Vec::new();
        nds.save_state(&mut state);
        let mut other = Nds::new(&Media::new(&rom)).expect("valid header");
        other.load_state(&state).expect("loads");
        assert_eq!(other.cycles(), nds.cycles());
        assert_eq!(other.peek(0, 0x0200_0004), Some(7));
        // A paused frontend shows the loaded frame before any frame runs.
        assert_eq!(other.screens()[0].pixels()[0], 0xFFFF_FFFF);
        assert!(other.screens() == nds.screens());
    }

    /// A GBA ROM with 128 KiB of Flash, a made-up logo and game code.
    fn gba_rom() -> Vec<u8> {
        let mut rom: Vec<u8> = (0..0x400u32).map(|index| (index * 7) as u8).collect();
        rom[0x9C] = 0x21;
        rom[0x9E] = 0xF8;
        rom[0xAC..0xB2].copy_from_slice(b"TEST01");
        rom[0xB2] = 0x96;
        rom[0xBE..0xC0].fill(0);
        rom[0x200..0x209].copy_from_slice(b"FLASH1M_V");
        rom
    }

    /// A DS with `gba`, and its save if given, in the GBA slot.
    fn nds_with_slot(rom: &[u8], gba: &[u8], save: Option<&[u8]>) -> Nds {
        let media = Media {
            second_slot: Some(emu_core::Cartridge {
                rom: gba,
                backup: save,
            }),
            ..Media::new(rom)
        };
        Nds::new(&media).expect("valid header")
    }

    /// Programs `value` at `offset` of the GBA slot's Flash bank 0 through
    /// the ARM9's bus.
    fn program_flash(nds: &mut Nds, offset: u32, value: u8) {
        let hw = &mut nds.machine.hw;
        hw.arm9_write_8(0x0A00_5555, 0xAA);
        hw.arm9_write_8(0x0A00_2AAA, 0x55);
        hw.arm9_write_8(0x0A00_5555, 0xA0);
        hw.arm9_write_8(0x0A00_0000 + offset, value);
    }

    #[test]
    fn boot_leaves_the_gba_header_and_logo_where_the_library_looks() {
        let rom = rom();
        let gba = gba_rom();
        let nds = nds_with_slot(&rom, &gba, None);
        let ram = |address: u32| nds.peek(0, address).unwrap_or(0);
        assert_eq!([ram(0x027F_FC30), ram(0x027F_FC31)], [0, 0], "module ID");
        let codes: Vec<u8> = (0x027F_FC36..0x027F_FC3C).map(ram).collect();
        assert_eq!(codes, b"01TEST");
        assert_eq!(
            &nds.machine.hw.arm9_bios[gba_slot::BIOS_LOGO],
            &gba[0x04..0xA0]
        );

        let empty = Nds::new(&Media::new(&rom)).expect("valid header");
        let ram = |address: u32| empty.peek(0, address).unwrap_or(0);
        assert_eq!([ram(0x027F_FC30), ram(0x027F_FC31)], [0xFF, 0xFF]);
        assert!(
            empty.machine.hw.arm9_bios[gba_slot::BIOS_LOGO]
                .iter()
                .all(|&byte| byte == 0)
        );
    }

    #[test]
    fn the_gba_save_is_cartridge_1() {
        let rom = rom();
        let gba = gba_rom();
        let mut nds = nds_with_slot(&rom, &gba, None);
        assert_eq!(nds.backup_generation(1), Some(0));
        assert_eq!(nds.backup_generation(2), None);
        program_flash(&mut nds, 0x10, 0x5A);
        assert_eq!(nds.backup_generation(1), Some(1));
        let mut save = Vec::new();
        nds.save_backup(1, &mut save);
        assert_eq!((save.len(), save[0x10]), (0x2_0000, 0x5A));
        let mut card = Vec::new();
        nds.save_backup(0, &mut card);
        assert_ne!(card, save, "the game card keeps its own save");

        let mut again = nds_with_slot(&rom, &gba, Some(&save));
        assert_eq!(again.machine.hw.arm9_read_8(0x0A00_0010), 0x5A);
        let empty = Nds::new(&Media::new(&rom)).expect("valid header");
        assert_eq!(empty.backup_generation(1), None);
    }

    #[test]
    fn states_and_resets_keep_the_gba_cartridge() {
        let rom = rom();
        let gba = gba_rom();
        let mut nds = nds_with_slot(&rom, &gba, None);
        nds.run_frame(&[], &mut Vec::new());
        program_flash(&mut nds, 0x20, 0x77);
        let mut state = Vec::new();
        nds.save_state(&mut state);

        let mut other = nds_with_slot(&rom, &gba, None);
        let before = other.backup_generation(1);
        other.load_state(&state).expect("loads");
        let hw = &mut other.machine.hw;
        assert_eq!(hw.arm9_read_8(0x0A00_0020), 0x77);
        assert_eq!(hw.arm9_read_32(0x0800_00AC), u32::from_le_bytes(*b"TEST"));
        assert_ne!(
            other.backup_generation(1),
            before,
            "the frontend saves again"
        );

        let mut empty = Nds::new(&Media::new(&rom)).expect("valid header");
        assert!(empty.load_state(&state).is_err(), "no GBA slot to restore");

        other.reset();
        let hw = &mut other.machine.hw;
        assert_eq!(hw.arm9_read_8(0x0A00_0020), 0x77);
        assert_eq!(hw.arm9_read_8(0x0800_00B2), 0x96);
    }
}
