//! Everything in the DS except the two CPUs: memory, I/O state, the event
//! scheduler and the devices both CPUs share (GBATEK, "DS Memory Map").
//!
//! Each CPU sees `Hardware` through its own bus (`bus.rs`) with its own
//! memory map (`memory.rs`) and I/O map (`io.rs`), and keeps its own clock:
//! the ARM7 counts bus cycles (33.51 MHz) and the ARM9 counts its own
//! cycles, two per bus cycle. The scheduler runs in bus cycles.

use emu_sched::Scheduler;
use serde::{Deserialize, Serialize};

use crate::dma::{Dma, Owner};
use crate::interrupt::Interrupts;
use crate::ipc::{Ipc, Side};
use crate::math::Math;
use crate::timer::Timers;
use crate::vram::Vram;

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
    /// Palette RAM and OAM, 1 KiB per engine each.
    pub(crate) palette: Box<[u8]>,
    pub(crate) oam: Box<[u8]>,
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
    /// Raw values of I/O registers without side effects, for reading back:
    /// one 0x2000-byte page per CPU.
    pub(crate) io9: Box<[u8]>,
    pub(crate) io7: Box<[u8]>,
    pub(crate) scheduler: Scheduler<Event>,
}

impl Hardware {
    pub(crate) fn new(arm9_bios: Box<[u8]>, arm7_bios: Box<[u8]>) -> Self {
        Self {
            main_ram: vec![0; MAIN_RAM_SIZE].into_boxed_slice(),
            shared_wram: vec![0; 0x8000].into_boxed_slice(),
            arm7_wram: vec![0; 0x1_0000].into_boxed_slice(),
            vram: Vram::default(),
            wramcnt: 0,
            palette: vec![0; 0x800].into_boxed_slice(),
            oam: vec![0; 0x800].into_boxed_slice(),
            arm9_bios,
            arm7_bios,
            arm9: CpuIo::new(Owner::Arm9),
            arm7: CpuIo::new(Owner::Arm7),
            ipc: Ipc::default(),
            math: Math::default(),
            keys: 0x03FF,
            ext_keys: 0x007F,
            powcnt1: 0,
            powcnt2: 0,
            exmemcnt: 0,
            vcount: 0,
            hblank: false,
            io9: vec![0; 0x2000].into_boxed_slice(),
            io7: vec![0; 0x2000].into_boxed_slice(),
            scheduler: Scheduler::new(),
        }
    }

    /// Moves the BIOS images out of `other`, which is about to be replaced
    /// by a loaded save state.
    pub(crate) fn take_media_from(&mut self, other: &mut Self) {
        self.arm9_bios = std::mem::take(&mut other.arm9_bios);
        self.arm7_bios = std::mem::take(&mut other.arm7_bios);
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

    /// Replaces timer `index`'s overflow event on `side` after it changed.
    pub(crate) fn schedule_timer(&mut self, side: Side, index: usize) {
        let event = Event::TimerOverflow(side, index as u8);
        self.scheduler.cancel(|pending| *pending == event);
        if let Some(time) = self.cpu(side).timers.overflow_time(index) {
            self.scheduler.schedule_at(time, event);
        }
    }
}
