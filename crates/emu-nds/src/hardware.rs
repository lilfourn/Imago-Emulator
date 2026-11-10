//! Everything in the DS except the two CPUs: memory, I/O state, the event
//! scheduler and the devices both CPUs share (GBATEK, "DS Memory Map").
//!
//! Each CPU sees `Hardware` through its own bus (`bus.rs`) with its own
//! memory map (`memory.rs`) and I/O map (`io.rs`), and keeps its own clock:
//! the ARM7 counts bus cycles (33.51 MHz) and the ARM9 counts its own
//! cycles, two per bus cycle. The scheduler runs in bus cycles.

use emu_core::StateError;
use emu_sched::Scheduler;
use emu_state::payload;
use serde::{Deserialize, Serialize};

use crate::cart::{Cart, RomEvent};
use crate::display::ScaledScreens;
use crate::dma::{Dma, Owner, Timing};
use crate::gba_slot::GbaSlot;
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
use crate::wifi::Wifi;

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
    TimerOverflow(Side, #[serde(with = "payload::Below::<4>")] u8),
    /// Time for the next audio output sample.
    SoundSample,
    /// The game card's ROM transfer moves on.
    CartStep,
    /// A byte on the game card's SPI bus (its save chip) is done.
    CartSpiDone,
    /// A byte on the ARM7's SPI bus is done.
    SpiDone,
    /// The wifi hardware has something to do.
    Wifi,
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
    /// Whether what the machine checked before this CPU's last checked
    /// instruction (no DMA to run, not halted, the IRQ line) still holds,
    /// so it can skip those checks. The bus's RAM fast paths leave it set;
    /// every other access clears it, as does the start of a slice. Not
    /// machine state.
    #[serde(skip)]
    pub(crate) checked: bool,
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
            checked: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Hardware {
    #[serde(with = "payload::Exact::<MAIN_RAM_SIZE>")]
    pub(crate) main_ram: Box<[u8]>,
    #[serde(with = "payload::Exact::<0x8000>")]
    pub(crate) shared_wram: Box<[u8]>,
    #[serde(with = "payload::Exact::<0x1_0000>")]
    pub(crate) arm7_wram: Box<[u8]>,
    pub(crate) vram: Vram,
    /// WRAMCNT: how the 32 KiB shared WRAM splits between the CPUs.
    pub(crate) wramcnt: u8,
    /// The 2D engines, which own their registers, palette RAM and OAM.
    pub(crate) engine_a: Engine2d,
    pub(crate) engine_b: Engine2d,
    pub(crate) gpu3d: Gpu3d,
    /// What each engine drew this frame, 256×192 pixels as 0xFFRRGGBB.
    /// Saved in states, so a state loaded while paused shows its frame.
    #[serde(with = "payload::ExactList::<{ 256 * 192 }>")]
    pub(crate) screen_a: Box<[u32]>,
    #[serde(with = "payload::ExactList::<{ 256 * 192 }>")]
    pub(crate) screen_b: Box<[u32]>,
    /// The same, larger, with upscaled 3D: a display setting, not saved.
    #[serde(skip)]
    pub(crate) scaled: ScaledScreens,
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
    pub(crate) gba_slot: GbaSlot,
    pub(crate) spi: Spi,
    pub(crate) rtc: Rtc,
    pub(crate) wifi: Wifi,
    /// KEYINPUT: A, B, Select, Start, Right, Left, Up, Down, R, L; active low.
    pub(crate) keys: u16,
    /// EXTKEYIN bits 0 (X), 1 (Y), 6 (pen down) and 7 (hinge), active low.
    pub(crate) ext_keys: u16,
    pub(crate) powcnt1: u16,
    pub(crate) powcnt2: u16,
    /// EXMEMCNT: GBA and DS slot ownership and timing.
    pub(crate) exmemcnt: u16,
    /// The line being drawn, 0–262.
    #[serde(with = "payload::Below::<263>")]
    pub(crate) vcount: u16,
    /// Between HBlank and the next line's start.
    pub(crate) hblank: bool,
    /// A display capture is writing this frame's lines.
    pub(crate) capturing: bool,
    /// What each CPU last wrote to each offset of its I/O page, one
    /// 0x2000-byte page per CPU. Offsets with no register behind them
    /// (DISPCAPCNT among them) read back from here; for the rest it is
    /// kept only because a save state holds it (`io.rs`).
    #[serde(with = "payload::Exact::<0x2000>")]
    pub(crate) io9: Box<[u8]>,
    #[serde(with = "payload::Exact::<0x2000>")]
    pub(crate) io7: Box<[u8]>,
    pub(crate) scheduler: Scheduler<Event>,
}

/// The most events a state's scheduler may hold, far more than the dozen
/// the machine ever has pending, and how long overdue one may be: events
/// run at the end of the slice they come due in, so about not at all.
const EVENTS_MOST: usize = 256;
const EVENT_BEHIND: u64 = 1 << 16;

/// How far a CPU's own clock, which timers and the wifi are written at,
/// may stand ahead of the scheduler's between slices in a state: far more
/// than the longest DMA, which is what leaves it furthest ahead.
const CLOCK_AHEAD: u64 = 1 << 24;

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
            scaled: ScaledScreens::default(),
            arm9_bios,
            arm7_bios,
            arm9: CpuIo::new(Owner::Arm9),
            arm7: CpuIo::new(Owner::Arm7),
            ipc: Ipc::default(),
            math: Math::default(),
            sound: Sound::default(),
            cart: Cart::new(Box::default(), None),
            gba_slot: GbaSlot::default(),
            spi: Spi::new(None),
            rtc: Rtc::new(0),
            wifi: Wifi::new(&[]),
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

    /// Makes this hardware, read from a save state, ready to use: checks,
    /// beyond what each field checks as it is read, that its parts are
    /// ones the machine could have left, and works out again what follows
    /// from the rest of the state (the VRAM map).
    ///
    /// # Errors
    ///
    /// Returns [`StateError::Malformed`] saying which part is wrong.
    pub(crate) fn settle(&mut self) -> Result<(), StateError> {
        payload::check(
            self.vram.settle(),
            "the state's video memory banks are not the DS's sizes",
        )?;
        payload::check(
            self.gpu3d.in_range(),
            "the state's 3D engine holds polygons it cannot",
        )?;
        payload::check(
            self.engine_a.in_range() && self.engine_b.in_range(),
            "the state's 2D registers hold bits they do not have",
        )?;
        payload::check(
            self.sound.in_range(),
            "the state's sound channels hold steps they cannot",
        )?;
        let now = self.scheduler.now();
        payload::check(
            self.arm9.timers.in_range(now, CLOCK_AHEAD)
                && self.arm7.timers.in_range(now, CLOCK_AHEAD),
            "the state's timers are not at its time",
        )?;
        payload::check(
            self.arm9.dma.in_range() && self.arm7.dma.in_range(),
            "the state's DMA channels have more left than they move",
        )?;
        payload::check(
            self.cart.in_range() && self.spi.in_range(),
            "the state's save or firmware chip is not a size its kind comes in",
        )?;
        payload::check(
            self.gba_slot.in_range(),
            "the state's GBA cartridge holds what its chips cannot",
        )?;
        payload::check(
            self.wifi.in_range(now, CLOCK_AHEAD),
            "the state's wifi frames or clock are not ones it could have",
        )?;
        payload::check(
            self.scheduler.plausible(EVENTS_MOST, EVENT_BEHIND),
            "the state's events are not ones the machine schedules",
        )
    }

    /// Moves the BIOS images and the ROMs out of `other`, which is about to
    /// be replaced by a loaded save state.
    pub(crate) fn take_media_from(&mut self, other: &mut Self) {
        self.arm9_bios = std::mem::take(&mut other.arm9_bios);
        self.arm7_bios = std::mem::take(&mut other.arm7_bios);
        self.cart.take_rom_from(&mut other.cart);
        self.gba_slot.take_rom_from(&mut other.gba_slot);
    }

    /// The CPU that EXMEMCNT gives the GBA slot.
    pub(crate) fn gba_slot_owner(&self) -> Side {
        if self.exmemcnt & (1 << 7) != 0 {
            Side::Arm7
        } else {
            Side::Arm9
        }
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

    /// Mixes one audio sample, with channels reading and captures writing
    /// memory as the ARM7 sees it, and schedules the next.
    pub(crate) fn sound_sample(&mut self, time: u64) {
        let mut sound = std::mem::take(&mut self.sound);
        sound.sample(self);
        self.sound = sound;
        self.scheduler
            .schedule_at(time + sound::CYCLES_PER_SAMPLE, Event::SoundSample);
    }

    /// Brings the wifi hardware up to bus cycle `time`, for its event.
    pub(crate) fn run_wifi(&mut self, time: u64) {
        self.wifi.advance(time);
        self.sync_wifi();
    }

    /// Raises the ARM7's wifi interrupt if the wifi hardware asks, and
    /// moves its event to its next deadline, after anything that may have
    /// changed either.
    pub(crate) fn sync_wifi(&mut self) {
        if self.wifi.take_irq() {
            self.arm7.interrupts.request(irq::WIFI);
        }
        if let Some(deadline) = self.wifi.reschedule() {
            self.scheduler.cancel(|event| *event == Event::Wifi);
            if let Some(time) = deadline {
                self.scheduler.schedule_at(time, Event::Wifi);
            }
        }
    }

    /// Raises the interrupts of the timers in `requested` (bit `n` for
    /// timer `n`) on `side`.
    pub(crate) fn request_timer_interrupts(&mut self, side: Side, requested: u8) {
        for timer in (0..4).filter(|timer| requested & (1 << timer) != 0) {
            self.cpu_mut(side).interrupts.request(irq::TIMER0 << timer);
        }
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

impl sound::Memory for Hardware {
    fn read_8(&mut self, address: u32) -> u8 {
        self.arm7_read_8(address)
    }

    fn write_8(&mut self, address: u32, value: u8) {
        self.arm7_write_8(address, value);
    }
}
