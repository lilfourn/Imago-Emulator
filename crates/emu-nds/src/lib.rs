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
mod link;
mod machine;
mod math;
mod memory;
mod rtc;
mod sound;
mod spi;
mod timer;
mod vram;
mod wifi;

use emu_core::{
    Button, Core, CoreInfo, Debugger, Frame, FrameRate, Input, LoadError, Media, MemoryRegion,
    ScreenSize, StateError, StereoSample, System, SystemFileKind,
};
use emu_gba_cart::Cartridge as GbaCartridge;
use emu_state::payload;
pub use link::Linked;

use cart::Cart;
use display::FRAME_CYCLES;
use gba_slot::GbaSlot;
use hardware::Hardware;
use machine::{Machine, Plain, Stepper};
use rtc::Rtc;
use spi::Spi;
use wifi::Wifi;

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
    audio_sample_rate: sound::SAMPLE_RATE,
    // Bump whenever any serialized struct changes shape.
    state_version: 7,
};

/// A Nintendo DS with a game card inserted, and maybe a GBA cartridge.
#[derive(Debug)]
pub struct Nds {
    machine: Machine,
    screens: [Frame; 2],
    bios: Bios,
}

/// The BIOS a session has: the emulated one, always, and the user's dumps
/// if `Media` held both.
///
/// The session decides which a machine powers on and resets with: the
/// dumps if it has them. A save state decides nothing, but it is of a
/// machine that ran one way, part-way through whatever it was doing: a
/// CPU may be inside a BIOS routine or an interrupt it will return from
/// to an address in the image it had. So a loaded state runs with the
/// images it was made with ([`Nds::install`]), and one made with dumps is
/// refused where there are none ([`Nds::check_state`]).
#[derive(Debug)]
struct Bios {
    /// The emulated BIOS's images, the ARM9's and the ARM7's, whose
    /// system calls the machine catches and runs itself.
    emulated: [Box<[u8]>; 2],
    /// The user's dumps.
    dumps: Option<[Box<[u8]>; 2]>,
}

impl Bios {
    /// Whether the session emulates BIOS calls: it has no dumps.
    fn emulated(&self) -> bool {
        self.dumps.is_none()
    }

    /// The images a machine runs when BIOS calls are `emulated`, or from
    /// the dumps: `None` for dumps the session does not have.
    fn images(&self, emulated: bool) -> Option<[Box<[u8]>; 2]> {
        if emulated {
            Some(self.emulated.clone())
        } else {
            self.dumps.clone()
        }
    }

    /// The images the session powers on and resets with.
    fn session(&self) -> [Box<[u8]>; 2] {
        self.dumps.as_ref().unwrap_or(&self.emulated).clone()
    }
}

impl Nds {
    /// Powers on a DS with `media`'s cartridge and direct-boots it.
    /// `media.second_slot` goes in the GBA slot, where DS games can read a
    /// GBA game and its save.
    ///
    /// The BIOS is emulated unless `media` holds both the ARM9 (4 KiB) and
    /// ARM7 (16 KiB) BIOS dumps. That holds for the session: at power-on
    /// and at every reset. A save state made without dumps keeps its BIOS
    /// calls emulated wherever it is loaded, and one made with dumps
    /// loads only where there are dumps.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::InvalidRom`] if the cartridge header is missing,
    /// either program lies outside the ROM, or the firmware dump is too
    /// short to hold its header and wifi settings. The Nintendo logo is not
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
        let firmware = media.system_file(SystemFileKind::NdsFirmware);
        if firmware.is_some_and(|image| image.len() < spi::firmware::MIN_IMAGE_SIZE) {
            return Err(invalid(
                "the firmware dump is shorter than its header and wifi settings",
            ));
        }
        let dumps = media
            .system_file(SystemFileKind::NdsBios9)
            .filter(|bios| bios.len() == 0x1000)
            .zip(
                media
                    .system_file(SystemFileKind::NdsBios7)
                    .filter(|bios| bios.len() == 0x4000),
            );
        let mut gba_slot = GbaSlot::new(media.second_slot.map(|cartridge| {
            GbaCartridge::new(cartridge.rom.into(), cartridge.backup, media.start_time)
        }));
        let mut emulated = [bios::arm9_image(), bios::arm7_image()];
        if let Some(logo) = gba_slot.bios_logo() {
            emulated[0][gba_slot::BIOS_LOGO].copy_from_slice(&logo);
        }
        let bios = Bios {
            emulated,
            dumps: dumps.map(|(arm9, arm7)| [arm9.into(), arm7.into()]),
        };
        let [arm9_bios, arm7_bios] = bios.session();
        let mut hw = Hardware::new(arm9_bios, arm7_bios);
        hw.cart = Cart::new(media.rom.into(), media.backup);
        hw.gba_slot = gba_slot;
        hw.spi = Spi::new(firmware);
        hw.wifi = Wifi::new(hw.spi.wifi_settings());
        hw.rtc = Rtc::new(media.start_time);
        Ok(Self {
            machine: Machine::boot(hw, bios.emulated()),
            screens: [Frame::new(SCREEN), Frame::new(SCREEN)],
            bios,
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

    /// The wifi hardware's state, as one `key=value` line for debugging.
    #[must_use]
    pub fn debug_wifi(&self) -> String {
        self.machine.hw.wifi.describe()
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

    /// Whether a saved `machine` is one the DS could have left and fits
    /// this session. Leaves it ready to install ([`Machine::settle`]).
    ///
    /// A state made with BIOS dumps does not fit a session without them:
    /// its CPUs may be part-way through the BIOS's own code, which is not
    /// here to run. (The other way round fits: see [`Nds::install`].)
    fn check_state(&self, machine: &mut Machine) -> Result<(), StateError> {
        machine.settle()?;
        payload::check(
            machine.hw.gba_slot.is_occupied() == self.machine.hw.gba_slot.is_occupied(),
            "the state's GBA slot does not match this session's",
        )?;
        payload::check(
            machine.hle_bios || self.bios.dumps.is_some(),
            "the state was made with BIOS dumps, which this session does not have: \
             add the BIOS files to load it",
        )
    }

    /// Replaces the machine with a saved one that [`Nds::check_state`]
    /// accepted, keeping the media and display settings.
    ///
    /// The machine runs with the BIOS images it was made with, whichever
    /// the session powers on with, so the flag that says its BIOS calls
    /// are emulated and the images it runs always go together: a state
    /// made without dumps goes on with its calls emulated where there are
    /// dumps too, until a reset.
    fn install(&mut self, mut machine: Machine) {
        machine.hw.take_media_from(&mut self.machine.hw);
        if machine.hle_bios != self.machine.hle_bios
            && let Some([arm9, arm7]) = self.bios.images(machine.hle_bios)
        {
            machine.hw.arm9_bios = arm9;
            machine.hw.arm7_bios = arm7;
        }
        machine.hw.take_display_from(&mut self.machine.hw);
        self.machine = machine;
        // The rendered 3D image is not saved.
        let hw = &mut self.machine.hw;
        hw.gpu3d.render_frame(&hw.vram);
        self.present();
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
            let progress = self.machine.run_slice_with(stepper, start + FRAME_CYCLES);
            if progress.stopped {
                return false;
            }
            if progress.frame_done {
                break;
            }
        }
        // A lone console's frames reach nobody.
        self.machine.hw.wifi.take_transmissions();
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
        // The BIOS is the session's, whatever a loaded state ran with.
        let old = &mut self.machine.hw;
        let [arm9_bios, arm7_bios] = self.bios.session();
        let mut hw = Hardware::new(arm9_bios, arm7_bios);
        let now = old.scheduler.now();
        std::mem::swap(&mut hw.cart, &mut old.cart);
        std::mem::swap(&mut hw.spi, &mut old.spi);
        std::mem::swap(&mut hw.rtc, &mut old.rtc);
        std::mem::swap(&mut hw.gba_slot, &mut old.gba_slot);
        hw.wifi = Wifi::new(hw.spi.wifi_settings());
        hw.rtc.restart_cycles(now);
        hw.gba_slot.restart_cycles(now);
        hw.take_display_from(old);
        self.machine = Machine::boot(hw, self.bios.emulated());
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        payload::append(&self.machine, out);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut machine: Machine = payload::load(state, "not a Nintendo DS state")?;
        self.check_state(&mut machine)?;
        self.install(machine);
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
    use emu_state::mutation;

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

    /// A cartridge whose two programs each start a BIOS decompression
    /// that reads through callbacks, Huffman on the ARM9 and LZ77 on the
    /// ARM7, and never return from the callback that reads a byte: the
    /// calls wait, part-way, in every state.
    fn rom_waiting_in_the_bios() -> Vec<u8> {
        let mut rom = vec![0; 0x400];
        let mut put = |offset: usize, value: u32| {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        // SWI 13h with a header of 8-bit symbols, SWI 12h with an LZ77 one.
        for (header, at, base, swi, data_header) in [
            (0x20, 0x200, 0x0200_4000u32, 0xEF13_0000u32, 0xE3A0_0F4Au32),
            (0x30, 0x300, 0x0238_0000, 0xEF12_0000, 0xE3A0_0E11),
        ] {
            put(header, at as u32);
            put(header + 4, base);
            put(header + 8, base);
            put(header + 12, 0x3C);
            let program = [
                0xE28F_3010, // ADD R3, PC, #16  (the callbacks below)
                0xE3A0_0000, // MOV R0, #0
                0xE3A0_1402, // MOV R1, #0x02000000
                0xE3A0_2402, // MOV R2, #0x02000000
                swi,         // SWI 12h or 13h
                0xEAFF_FFFE, // B .
                base + 0x30, // Open
                0,           // Close: none
                base + 0x38, // Get_8bit
                0,           // Get_16bit: never called
                base + 0x38, // Get_32bit
                0,
                data_header, // Open: MOV R0, #header
                0xE12F_FF1E, //       BX LR
                0xEAFF_FFFE, // Get_8bit: B .
            ];
            for (index, word) in program.into_iter().enumerate() {
                put(at + index * 4, word);
            }
        }
        rom
    }

    /// Puts every part of the machine that a state holds to use, through
    /// the registers a game would write: polygons drawn and more waiting
    /// behind a buffer swap, half a triangle strip, sound channels
    /// playing, words in both IPC FIFOs and timers counting.
    fn keep_busy(nds: &mut Nds) {
        let quad = |hw: &mut Hardware, kind: u32, corners: &[(i32, i32)]| {
            hw.arm9_write_32(0x0400_0500, kind); // BEGIN_VTXS
            for &(x, y) in corners {
                // VTX_16: X and Y, then Z.
                hw.arm9_write_32(0x0400_048C, (x as u32 & 0xFFFF) | (y as u32) << 16);
                hw.arm9_write_32(0x0400_048C, 0);
            }
        };
        let hw = &mut nds.machine.hw;
        hw.arm9_write_16(0x0400_0304, 0x820F); // POWCNT1: everything on
        hw.arm9_write_32(0x0400_0000, 0x0001_0108); // DISPCNT: BG0 as 3D
        hw.arm9_write_8(0x0400_0240, 0x83); // VRAMCNT_A: textures
        hw.arm9_write_32(0x0400_0454, 0); // MTX_IDENTITY
        hw.arm9_write_32(0x0400_0580, 0xBFFF_0000); // VIEWPORT: the screen
        hw.arm9_write_32(0x0400_04A4, 31 << 16 | 0xC0); // POLYGON_ATTR
        hw.arm9_write_32(0x0400_0480, 0x7FFF); // COLOR
        quad(
            hw,
            1,
            &[
                (-0x800, 0x800),
                (-0x800, -0x800),
                (0x800, -0x800),
                (0x800, 0x800),
            ],
        );
        quad(hw, 0, &[(0, 0x400), (-0x400, -0x400), (0x400, -0x400)]);
        hw.arm9_write_32(0x0400_0540, 0); // SWAP_BUFFERS
        // After the swap, which waits for VBlank: these stay in the FIFO.
        quad(
            hw,
            2,
            &[(0, 0), (0x200, 0), (0, 0x200), (0x200, 0x200), (0x400, 0)],
        );
        // Timers 0 and 3 on each CPU.
        for timer in [0x0400_0100, 0x0400_010C] {
            hw.arm9_write_32(timer, 0x00C0_F000);
            hw.arm7_write_32(timer, 0x00C1_FF00);
        }
        // The wifi: power, its microsecond clock counting, the radio
        // awake and listening, and a beacon timer.
        hw.arm7_write_16(0x0400_0304, 0x0003); // POWCNT2: speakers, wifi
        let wifi = |hw: &mut Hardware, register: u32, value: u16| {
            hw.arm7_write_16(0x0480_8000 + register, value);
        };
        wifi(hw, 0x036, 0); // W_POWER_US: on
        wifi(hw, 0x006, 2); // W_MODE_WEP
        wifi(hw, 0x004, 1); // W_MODE_RST: wake
        wifi(hw, 0x050, 0x4C00); // W_RXBUF_BEGIN
        wifi(hw, 0x052, 0x5F60); // W_RXBUF_END
        wifi(hw, 0x030, 0x8001); // W_RXCNT: receive
        wifi(hw, 0x08C, 100); // W_BEACONINT
        wifi(hw, 0x0EA, 1); // W_US_COMPARECNT
        wifi(hw, 0x0E8, 1); // W_US_COUNTCNT: count
        wifi(hw, 0x012, 0xFFFF); // W_IE
        // IPC: both FIFOs on, three words each way.
        hw.arm9_write_16(0x0400_0184, 0x8000);
        hw.arm7_write_16(0x0400_0184, 0x8000);
        for word in [0x1234_5678, 0x9ABC_DEF0, 3] {
            hw.arm9_write_32(0x0400_0188, word);
            hw.arm7_write_32(0x0400_0188, word);
        }
        // Sound: an ADPCM loop on channel 0, a square wave on 8, noise
        // on 14, and a capture.
        hw.arm7_write_16(0x0400_0500, 0x807F); // SOUNDCNT: master on
        hw.arm7_write_32(0x0400_0404, 0x0200_1000); // SOUND0SAD
        hw.arm7_write_16(0x0400_0408, 0xFE00); // SOUND0TMR
        hw.arm7_write_32(0x0400_040C, 0x40); // SOUND0LEN
        hw.arm7_write_32(0x0400_0400, 0xC840_007F); // SOUND0CNT: ADPCM, loop
        hw.arm7_write_16(0x0400_0488, 0xFC00);
        hw.arm7_write_32(0x0400_0480, 0xE340_0040); // SOUND8CNT: PSG
        hw.arm7_write_16(0x0400_04E8, 0xFC00);
        hw.arm7_write_32(0x0400_04E0, 0xE040_0040); // SOUND14CNT: noise
        hw.arm7_write_32(0x0400_0510, 0x0200_2000); // SNDCAP0DAD
        hw.arm7_write_16(0x0400_0514, 0x20); // SNDCAP0LEN
        hw.arm7_write_8(0x0400_0508, 0x80); // SNDCAP0CNT: start
    }

    /// Loads into a DS playing `rom` the wrong states made from its own
    /// after `frames` frames ([`mutation::try_each`]), at render scale
    /// `scale`, and runs each it takes: a frame, the memory a debugger
    /// reads, a save, a reset and a frame more. `busy` first puts the
    /// whole machine to use ([`keep_busy`]).
    fn try_wrong_states(
        rom: &[u8],
        (frames, busy, scale): (usize, bool, u32),
        every: usize,
    ) -> mutation::Outcome {
        let mut nds = Nds::new(&Media::new(rom)).expect("valid header");
        nds.set_render_scale(scale);
        let mut audio = Vec::new();
        for frame in 0..frames {
            // Two frames before the end, so that a swap has drawn.
            if busy && frame + 2 == frames {
                keep_busy(&mut nds);
            }
            nds.run_frame(&[Input::default()], &mut audio);
            audio.clear();
        }
        let mut good = Vec::new();
        nds.save_state(&mut good);
        nds.load_state(&good).expect("its own state loads");
        let machine: Machine = payload::load(&good, "its own state").expect("reads");
        if busy {
            let list = &machine.hw.gpu3d;
            assert!(list.polygons_drawn() >= 2, "polygons are in the state");
            assert!(machine.hw.wifi.powered(), "the wifi is on in the state");
        }
        if rom == rom_waiting_in_the_bios() {
            assert_eq!(machine.calls_waiting(), [1, 1], "BIOS calls wait");
        }
        mutation::try_each(&machine, every, |wrong| {
            if nds.load_state(wrong).is_err() {
                return;
            }
            let mut audio = Vec::new();
            nds.run_frame(&[Input::default()], &mut audio);
            for address in (0..0x1000_0000u32).step_by(0x7_7771) {
                let _ = nds.peek(0, address);
            }
            let _ = nds.memory(MemoryRegion::MainRam);
            let _ = nds.memory(MemoryRegion::VideoRam);
            let _ = (nds.debug_state(), nds.debug_video(), nds.debug_wifi());
            let mut saved = Vec::new();
            nds.save_state(&mut saved);
            let mut backup = Vec::new();
            nds.save_backup(0, &mut backup);
            nds.reset();
            nds.run_frame(&[Input::default()], &mut audio);
        })
    }

    /// A state with bytes after it is not that state: a linked pair's
    /// begins with one console's, and used to load into a console on its
    /// own, dropping the second player.
    #[test]
    fn a_state_with_bytes_left_over_is_refused() {
        let rom = rom();
        let media = Media::new(&rom);
        let mut one = Nds::new(&media).expect("valid rom");
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

    /// As [`try_wrong_states`], for two DSes playing `rom` in wireless range:
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
        let outcome = try_wrong_states(&rom_waiting_in_the_bios(), (4, true, 1), 79);
        assert!(outcome.tried > 100, "{} states", outcome.tried);
        outcome.assert_no_panics();
        let outcome = try_wrong_pair_states(&rom(), 3, 401);
        assert!(outcome.tried > 10, "{} states", outcome.tried);
        outcome.assert_no_panics();
    }

    /// Every place in the states of a busy machine, at the native scale
    /// and enlarged, and of the test ROMs, if they are there
    /// (`scripts/fetch-test-roms.sh`), each on a thread of its own.
    /// Several minutes.
    #[test]
    #[ignore = "minutes: every byte of several states' structure"]
    fn every_wrong_state_is_refused_or_runs() {
        let waiting = rom_waiting_in_the_bios();
        let plain = rom();
        let mut machines: Vec<(String, Box<dyn FnOnce() -> mutation::Outcome + Send + '_>)> = vec![
            (
                "busy".to_owned(),
                Box::new(|| try_wrong_states(&waiting, (4, true, 1), 1)),
            ),
            (
                "busy at 2x".to_owned(),
                Box::new(|| try_wrong_states(&waiting, (4, true, 2), 3)),
            ),
            (
                "idle".to_owned(),
                Box::new(|| try_wrong_states(&plain, (3, false, 1), 2)),
            ),
        ];
        machines.push((
            "a linked pair".to_owned(),
            Box::new(|| try_wrong_pair_states(&plain, 3, 3)),
        ));
        let roms = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-roms");
        for (name, frames) in [
            ("rockwrestler/rockwrestler.nds", 300),
            ("armwrestler-ds/armwrestler.nds", 200),
        ] {
            let Ok(rom) = std::fs::read(roms.join(name)) else {
                eprintln!("skipping {name}: run scripts/fetch-test-roms.sh");
                continue;
            };
            machines.push((
                name.to_owned(),
                Box::new(move || try_wrong_states(&rom, (frames, false, 1), 2)),
            ));
        }
        mutation::sweep(machines).assert_no_panics();
    }

    /// A cartridge whose ARM9 divides 100 by 7 with the BIOS (SWI 09h)
    /// and stores the quotient at 0x02000000, then loops. The ARM7 loops.
    fn rom_that_divides() -> Vec<u8> {
        let mut rom = vec![0; 0x400];
        let mut put = |offset: usize, value: u32| {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        for (header, at, base) in [(0x20, 0x200, 0x0200_4000), (0x30, 0x300, 0x0238_0000)] {
            put(header, at as u32);
            put(header + 4, base);
            put(header + 8, base);
            put(header + 12, 0x18);
        }
        let program = [
            0xE3A0_0064, // MOV R0, #100
            0xE3A0_1007, // MOV R1, #7
            0xEF09_0000, // SWI 09h  (Div)
            0xE3A0_2402, // MOV R2, #0x02000000
            0xE582_0000, // STR R0, [R2]
            0xEAFF_FFFE, // B .
        ];
        for (index, word) in program.into_iter().enumerate() {
            put(0x200 + index * 4, word);
        }
        put(0x300, 0xEAFF_FFFE); // B .
        rom
    }

    /// BIOS dumps that are not Nintendo's: every vector returns at once,
    /// so a system call does nothing.
    fn dumps() -> (Vec<u8>, Vec<u8>) {
        let image = |size: usize| {
            let mut image = vec![0; size];
            for vector in (0..0x20).step_by(4) {
                // MOVS PC, LR
                image[vector..vector + 4].copy_from_slice(&0xE1B0_F00Eu32.to_le_bytes());
            }
            image
        };
        (image(0x1000), image(0x4000))
    }

    /// A DS playing `rom`, with `dumps` as its BIOS if given.
    fn nds_with(rom: &[u8], dumps: Option<&(Vec<u8>, Vec<u8>)>) -> Nds {
        let files: Vec<emu_core::SystemFile<'_>> = dumps
            .iter()
            .flat_map(|(arm9, arm7)| {
                [
                    (SystemFileKind::NdsBios9, arm9),
                    (SystemFileKind::NdsBios7, arm7),
                ]
            })
            .map(|(kind, data)| emu_core::SystemFile { kind, data })
            .collect();
        Nds::new(&Media {
            system_files: &files,
            ..Media::new(rom)
        })
        .expect("valid header")
    }

    /// The quotient [`rom_that_divides`] stored after one frame: 14 where
    /// the BIOS call was emulated, 100 where [`dumps`] did nothing.
    fn quotient(nds: &mut Nds) -> u32 {
        nds.run_frame(&[Input::default()], &mut Vec::new());
        u32::from_le_bytes([0, 1, 2, 3].map(|byte| nds.peek(0, 0x0200_0000 + byte).unwrap_or(0)))
    }

    /// The flag that says BIOS calls are emulated came from the state and
    /// the images from the session: a state made without dumps kept
    /// trapping calls over the user's BIOS, for good, and one made with
    /// dumps ran the emulated BIOS's empty vectors.
    #[test]
    fn the_session_decides_the_bios_and_a_state_keeps_the_one_it_was_made_with() {
        let rom = rom_that_divides();
        let dumps = dumps();
        let mut emulating = nds_with(&rom, None);
        let mut with_dumps = nds_with(&rom, Some(&dumps));
        // Each one's state at power-on, before the call.
        let (mut emulated_state, mut dump_state) = (Vec::new(), Vec::new());
        emulating.save_state(&mut emulated_state);
        with_dumps.save_state(&mut dump_state);
        assert_eq!(quotient(&mut emulating), 14);
        assert_eq!(quotient(&mut with_dumps), 100);

        // A state made without dumps loads where there are dumps, and its
        // calls are still emulated, by the emulated BIOS's own images.
        with_dumps.load_state(&emulated_state).expect("loads");
        assert_eq!(quotient(&mut with_dumps), 14);
        // The machine is still one whose calls are emulated, as a state
        // saved from it will say.
        assert!(with_dumps.machine.hle_bios);
        // A reset goes back to the session's BIOS: the dumps.
        with_dumps.reset();
        assert!(!with_dumps.machine.hle_bios);
        assert_eq!(quotient(&mut with_dumps), 100);
        // And its own states still load.
        with_dumps.load_state(&dump_state).expect("loads");
        assert_eq!(quotient(&mut with_dumps), 100);

        // A state made with dumps is refused where there are none, and
        // says why; the session carries on as it was.
        let refused = emulating.load_state(&dump_state);
        let Err(StateError::Malformed(why)) = refused else {
            panic!("{refused:?}");
        };
        assert!(why.contains("BIOS dumps"), "{why}");
        emulating.reset();
        assert_eq!(quotient(&mut emulating), 14);
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
    fn rejects_a_firmware_dump_too_short_for_its_header() {
        let rom = rom();
        let with_firmware = |image: &[u8]| {
            let files = [emu_core::SystemFile {
                kind: SystemFileKind::NdsFirmware,
                data: image,
            }];
            Nds::new(&Media {
                system_files: &files,
                ..Media::new(&rom)
            })
            .is_ok()
        };
        assert!(!with_firmware(&[0; 0x20]), "no user settings location");
        assert!(!with_firmware(&[0; 0x1FF]));
        assert!(with_firmware(&[0; 0x200]));
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
    fn sound_comes_at_the_rate_the_core_reports() {
        let rom = rom();
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        let mut audio = Vec::new();
        // The first frame runs from power-on to the first VBlank.
        nds.run_frame(&[], &mut audio);
        audio.clear();
        let start = nds.cycles();
        for _ in 0..600 {
            nds.run_frame(&[], &mut audio);
        }
        let seconds = (nds.cycles() - start) as f64 / 33_513_982.0;
        let rate = audio.len() as f64 / seconds;
        assert!(
            (rate - f64::from(INFO.audio_sample_rate)).abs() < 1.0,
            "{rate} Hz"
        );
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
