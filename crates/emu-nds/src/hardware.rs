//! Everything in the DS except the two CPUs: memory, I/O state, the event
//! scheduler and the devices both CPUs share (GBATEK, "DS Memory Map").
//!
//! Each CPU sees `Hardware` through its own bus (`bus.rs`) with its own
//! memory map (`memory.rs`) and I/O map (`io.rs`), and keeps its own clock:
//! the ARM7 counts bus cycles (33.51 MHz) and the ARM9 counts its own
//! cycles, two per bus cycle. The scheduler runs in bus cycles.

use emu_sched::Scheduler;
use serde::{Deserialize, Serialize};

use crate::cart::{Cart, RomEvent};
use crate::dma::{Dma, Owner, Timing};
use crate::gpu2d::Engine2d;
use crate::gpu3d::Gpu3d;
use crate::interrupt::{Interrupts, irq};
use crate::ipc::{Ipc, Side};
use crate::math::Math;
use crate::rtc::Rtc;
use crate::sound::{self, Sound};
use crate::spi::Spi;
use crate::timer::Timers;
use crate::vram::{Engine, Vram};

/// Main RAM: 4 MiB, mirrored through 0x02000000–0x02FFFFFF.
pub(crate) const MAIN_RAM_SIZE: usize = 0x40_0000;

/// Scheduled events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Event {
    /// The current line reaches HBlank.
    HBlank,
    /// The next line starts.
    LineStart,
    /// One CPU's timer counts past 0xFFFF.
    TimerOverflow(Side, u8),
    /// Time for the next audio output sample.
    SoundSample,
    /// The game card's ROM transfer moves on.
    CartStep,
    /// A byte on the game card's SPI bus (its save chip) is done.
    CartSpiDone,
    /// A byte on the ARM7's SPI bus is done.
    SpiDone,
}

/// One CPU's private devices and clock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CpuIo {
    pub(crate) interrupts: Interrupts,
    pub(crate) timers: Timers,
    pub(crate) dma: Dma,
    /// DISPSTAT bits this CPU wrote: IRQ enables and the VCount setting.
    pub(crate) dispstat: u16,
    /// Stopped by HALTCNT, a BIOS call or CP15 until an interrupt.
    pub(crate) halted: bool,
    /// Cycles of this CPU's clock since power-on.
    pub(crate) cycles: u64,
    /// KEYCNT.
    pub(crate) keycnt: u16,
    /// POSTFLG.
    pub(crate) postflg: u8,
}

impl CpuIo {
    fn new(owner: Owner) -> Self {
        Self {
            interrupts: Interrupts::default(),
            timers: Timers::default(),
            dma: Dma::new(owner),
            dispstat: 0,
            halted: false,
            cycles: 0,
            keycnt: 0,
            postflg: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Hardware {
    pub(crate) main_ram: Box<[u8]>,
    pub(crate) shared_wram: Box<[u8]>,
    pub(crate) arm7_wram: Box<[u8]>,
    pub(crate) vram: Vram,
    /// WRAMCNT: how the 32 KiB shared WRAM splits between the CPUs.
    pub(crate) wramcnt: u8,
    /// The 2D engines, which own their registers, palette RAM and OAM.
    pub(crate) engine_a: Engine2d,
    pub(crate) engine_b: Engine2d,
    pub(crate) gpu3d: Gpu3d,
    /// What each engine drew this frame, 256×192 pixels as 0xFFRRGGBB.
    #[serde(skip, default = "blank_screen")]
    pub(crate) screen_a: Box<[u32]>,
    #[serde(skip, default = "blank_screen")]
    pub(crate) screen_b: Box<[u32]>,
    /// The BIOS images: our HLE vectors, or the user's dumps. Restored
    /// from media.
    #[serde(skip)]
    pub(crate) arm9_bios: Box<[u8]>,
    #[serde(skip)]
    pub(crate) arm7_bios: Box<[u8]>,
    pub(crate) arm9: CpuIo,
    pub(crate) arm7: CpuIo,
    pub(crate) ipc: Ipc,
    pub(crate) math: Math,
    pub(crate) sound: Sound,
    pub(crate) cart: Cart,
    pub(crate) spi: Spi,
    pub(crate) rtc: Rtc,
    /// KEYINPUT: A, B, Select, Start, Right, Left, Up, Down, R, L; active low.
    pub(crate) keys: u16,
    /// EXTKEYIN bits 0 (X), 1 (Y), 6 (pen down) and 7 (hinge), active low.
    pub(crate) ext_keys: u16,
    pub(crate) powcnt1: u16,
    pub(crate) powcnt2: u16,
    /// EXMEMCNT: GBA and DS slot ownership and timing.
    pub(crate) exmemcnt: u16,
    /// The line being drawn, 0–262.
    pub(crate) vcount: u16,
    /// Between HBlank and the next line's start.
    pub(crate) hblank: bool,
    /// A display capture is writing this frame's lines.
    pub(crate) capturing: bool,
    /// Raw values of I/O registers without side effects, for reading back:
    /// one 0x2000-byte page per CPU.
    pub(crate) io9: Box<[u8]>,
    pub(crate) io7: Box<[u8]>,
    pub(crate) scheduler: Scheduler<Event>,
}

fn blank_screen() -> Box<[u32]> {
    vec![0xFF00_0000; 256 * 192].into_boxed_slice()
}

impl Hardware {
    pub(crate) fn new(arm9_bios: Box<[u8]>, arm7_bios: Box<[u8]>) -> Self {
        let mut hw = Self::unset(arm9_bios, arm7_bios);
        // Both engines' affine BG matrices reset to identity (PA = PD = 1.0).
        for engine in [&mut hw.engine_a, &mut hw.engine_b] {
            for offset in [0x20, 0x26, 0x30, 0x36] {
                engine.write_register(offset, 0x0100);
            }
        }
        hw
    }

    fn unset(arm9_bios: Box<[u8]>, arm7_bios: Box<[u8]>) -> Self {
        Self {
            main_ram: vec![0; MAIN_RAM_SIZE].into_boxed_slice(),
            shared_wram: vec![0; 0x8000].into_boxed_slice(),
            arm7_wram: vec![0; 0x1_0000].into_boxed_slice(),
            vram: Vram::default(),
            wramcnt: 0,
            engine_a: Engine2d::new(Engine::A),
            engine_b: Engine2d::new(Engine::B),
            gpu3d: Gpu3d::new(),
            screen_a: blank_screen(),
            screen_b: blank_screen(),
            arm9_bios,
            arm7_bios,
            arm9: CpuIo::new(Owner::Arm9),
            arm7: CpuIo::new(Owner::Arm7),
            ipc: Ipc::default(),
            math: Math::default(),
            sound: Sound::default(),
            cart: Cart::new(Box::default(), None),
            spi: Spi::new(None),
            rtc: Rtc::new(0),
            keys: 0x03FF,
            ext_keys: 0x007F,
            powcnt1: 0,
            powcnt2: 0,
            exmemcnt: 0,
            vcount: 0,
            hblank: false,
            capturing: false,
            io9: vec![0; 0x2000].into_boxed_slice(),
            io7: vec![0; 0x2000].into_boxed_slice(),
            scheduler: Scheduler::new(),
        }
    }

    /// Moves the BIOS images and the ROM out of `other`, which is about to
    /// be replaced by a loaded save state.
    pub(crate) fn take_media_from(&mut self, other: &mut Self) {
        self.arm9_bios = std::mem::take(&mut other.arm9_bios);
        self.arm7_bios = std::mem::take(&mut other.arm7_bios);
        self.cart.take_rom_from(&mut other.cart);
    }

    /// The CPU that EXMEMCNT gives the game card slot.
    pub(crate) fn cart_owner(&self) -> Side {
        if self.exmemcnt & (1 << 11) != 0 {
            Side::Arm7
        } else {
            Side::Arm9
        }
    }

    /// Acts on what a game card ROM access asks for.
    pub(crate) fn rom_event(&mut self, event: RomEvent) {
        let owner = self.cart_owner();
        match event {
            RomEvent::None => {}
            RomEvent::StepAfter(cycles) => {
                let time = self.now(owner).max(self.scheduler.now()) + cycles;
                self.scheduler.schedule_at(time, Event::CartStep);
            }
            RomEvent::WordReady => self.cpu_mut(owner).dma.trigger(Timing::Cart),
            RomEvent::Finished { irq } => {
                if irq {
                    self.cpu_mut(owner).interrupts.request(irq::CART_TRANSFER);
                }
            }
        }
    }

    /// Schedules `event` `cycles` after `side`'s current time.
    pub(crate) fn schedule_after(&mut self, side: Side, cycles: u64, event: Event) {
        let time = self.now(side).max(self.scheduler.now()) + cycles;
        self.scheduler.schedule_at(time, event);
    }

    pub(crate) fn cpu(&self, side: Side) -> &CpuIo {
        match side {
            Side::Arm9 => &self.arm9,
            Side::Arm7 => &self.arm7,
        }
    }

    pub(crate) fn cpu_mut(&mut self, side: Side) -> &mut CpuIo {
        match side {
            Side::Arm9 => &mut self.arm9,
            Side::Arm7 => &mut self.arm7,
        }
    }

    /// `side`'s current time in bus cycles.
    pub(crate) fn now(&self, side: Side) -> u64 {
        match side {
            Side::Arm9 => self.arm9.cycles / 2,
            Side::Arm7 => self.arm7.cycles,
        }
    }

    /// Raises IF bits on each CPU: `[arm9, arm7]`.
    pub(crate) fn raise(&mut self, bits: [u32; 2]) {
        self.arm9.interrupts.request(bits[0]);
        self.arm7.interrupts.request(bits[1]);
    }

    /// Mixes one audio sample, reading channel data as the ARM7 sees
    /// memory, and schedules the next.
    pub(crate) fn sound_sample(&mut self, time: u64) {
        let mut sound = std::mem::take(&mut self.sound);
        sound.sample(|address| self.arm7_read_8(address));
        self.sound = sound;
        self.scheduler
            .schedule_at(time + sound::CYCLES_PER_SAMPLE, Event::SoundSample);
    }

    /// Replaces timer `index`'s overflow event on `side` after it changed.
    pub(crate) fn schedule_timer(&mut self, side: Side, index: usize) {
        let event = Event::TimerOverflow(side, index as u8);
        self.scheduler.cancel(|pending| *pending == event);
        if let Some(time) = self.cpu(side).timers.overflow_time(index) {
            self.scheduler.schedule_at(time, event);
        }
    }
}
