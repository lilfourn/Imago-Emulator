//! The GBA memory map (GBATEK, "GBA Memory Map", "GBA I/O Map"), cycle
//! counting and the event scheduler.
//!
//! Every CPU access adds the region's wait states to the cycle count and
//! advances the scheduler, which raises PPU line events (and, later, timer
//! and DMA events) at the exact cycle they are due.

use emu_arm::{Access, Bus as CpuBus};
use emu_core::RomPatch;
use emu_gba_cart::Cartridge;
use emu_sched::Scheduler;
use serde::{Deserialize, Serialize};

use crate::apu::{self, Apu};
use crate::bios;
use crate::dma::{self, Dma, Timing};
use crate::interrupts::{self, Interrupts};
use crate::ppu::{self, Ppu};
use crate::prefetch::{Lookup, Prefetch};
use crate::sio::{self, Sio};
use crate::timer::{Staged, Timers};
use crate::timing::{WaitStates, Width};

/// Scheduled events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Event {
    /// The PPU reaches HBlank on the current line.
    HBlank,
    /// The PPU starts the next line.
    LineStart,
    /// A timer counts past 0xFFFF.
    TimerOverflow(u8),
    /// A value written to a timer's reload register lands.
    TimerReload(u8),
    /// A value written to a timer's control register lands.
    TimerControl(u8),
    /// Interrupt requests and writes to IE, IF and IME land.
    InterruptSync,
    /// HALT learns whether an enabled interrupt is requested.
    InterruptRequested(bool),
    /// The CPU's IRQ input changes.
    InterruptLine(bool),
    /// Time for the next audio output sample.
    AudioSample,
    /// The serial transfer in progress ends.
    SerialDone,
    /// A triggered DMA channel starts.
    DmaStart(u8),
}

/// Interrupt request bits outside the PPU's.
pub(crate) mod irq {
    /// Timer `n` overflowed: bit 3 + n.
    pub(crate) const TIMER0: u16 = 1 << 3;
    pub(crate) const SERIAL: u16 = 1 << 7;
    /// DMA channel `n` finished: bit 8 + n.
    pub(crate) const DMA0: u16 = 1 << 8;
    pub(crate) const KEYPAD: u16 = 1 << 12;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Bus {
    /// The BIOS: the HLE vector table or a user dump. Restored from media.
    #[serde(skip)]
    bios: Box<[u8]>,
    pub(crate) cartridge: Cartridge,
    /// Cheats' changes to what cartridge reads return, by offset into the
    /// 32 MiB window, so they cover every mirror. Like the ROM, never saved
    /// in states.
    #[serde(skip)]
    rom_patches: Box<[RomPatch]>,
    ewram: Box<[u8]>,
    iwram: Box<[u8]>,
    pub(crate) ppu: Ppu,
    dma: Dma,
    timers: Timers,
    pub(crate) apu: Apu,
    pub(crate) sio: Sio,
    /// Raw values written to I/O registers, for byte merges and reading back
    /// registers without side effects.
    io: Box<[u8]>,
    interrupts: Interrupts,
    waitcnt: u16,
    #[serde(skip, default = "default_waits")]
    waits: WaitStates,
    /// KEYINPUT: buttons, active low.
    keys: u16,
    halted: bool,
    /// The SWI instruction of the VBlankIntrWait call the CPU halted in,
    /// which it repeats on waking. The repeat must keep the flag the IRQ
    /// handler just set.
    vblank_wait_swi: Option<u32>,
    /// The opcode the BIOS last fetched; BIOS reads from elsewhere see it.
    bios_open_bus: u32,
    executing_bios: bool,
    /// The last instruction fetched, which unmapped reads return.
    last_fetch: u32,
    /// The Game Pak prefetch unit.
    prefetch: Prefetch,
    /// Whether the last instruction fetch was from the cartridge.
    code_in_cartridge: bool,
    /// Whether the last memory access was a DMA's.
    last_access_dma: bool,
    /// WAITCNT's prefetch bit was cleared since the last code fetch from
    /// the cartridge, which then starts a new burst.
    prefetch_switched_off: bool,
    /// Internal CPU cycles still free because they overlap a DMA transfer
    /// that began during an internal cycle.
    idle_during_dma: u32,
    /// The value the last DMA unit put on the bus, which an unmapped read
    /// straight after a DMA returns instead of the last opcode.
    dma_bus: u32,
    /// Whether the access in progress follows a DMA's.
    follows_dma: bool,
    scheduler: Scheduler<Event>,
    cycles: u64,
}

fn default_waits() -> WaitStates {
    WaitStates::new(0)
}

impl Bus {
    pub(crate) fn new(cartridge: Cartridge, bios: Box<[u8]>) -> Self {
        let mut scheduler = Scheduler::new();
        scheduler.schedule_at(ppu::HDRAW_CYCLES, Event::HBlank);
        scheduler.schedule_at(apu::CYCLES_PER_SAMPLE, Event::AudioSample);
        let mut bus = Self {
            bios,
            cartridge,
            rom_patches: Box::default(),
            ewram: vec![0; 0x4_0000].into_boxed_slice(),
            iwram: vec![0; 0x8000].into_boxed_slice(),
            ppu: Ppu::new(),
            dma: Dma::default(),
            timers: Timers::default(),
            apu: Apu::new(),
            sio: Sio::default(),
            io: vec![0; 0x400].into_boxed_slice(),
            interrupts: Interrupts::default(),
            waitcnt: 0,
            waits: WaitStates::new(0),
            keys: 0x03FF,
            halted: false,
            vblank_wait_swi: None,
            bios_open_bus: bios::OPEN_BUS_AFTER_BOOT,
            executing_bios: false,
            last_fetch: 0,
            prefetch: Prefetch::default(),
            code_in_cartridge: false,
            last_access_dma: false,
            prefetch_switched_off: false,
            idle_during_dma: 0,
            dma_bus: 0,
            follows_dma: false,
            scheduler,
            cycles: 0,
        };
        // RCNT, SOUNDBIAS and the identity affine matrices, as the BIOS
        // leaves them.
        bus.write_io_16(0x134, 0x8000);
        bus.write_io_16(0x088, 0x0200);
        for offset in [0x20, 0x26, 0x30, 0x36] {
            bus.write_io_16(offset, 0x0100);
        }
        bus
    }

    /// A freshly powered-on bus for a console reset, taking the BIOS, the
    /// cartridge (save memory and clock included) and the ROM patches from
    /// `other`.
    pub(crate) fn reset_from(other: &mut Self) -> Self {
        let mut cartridge = std::mem::take(&mut other.cartridge);
        cartridge.restart_cycles(other.cycles);
        let mut bus = Self::new(cartridge, std::mem::take(&mut other.bios));
        bus.rom_patches = std::mem::take(&mut other.rom_patches);
        bus
    }

    /// Moves the ROM, its patches and the BIOS out of `other`, which is
    /// about to be replaced by a loaded save state.
    pub(crate) fn take_media_from(&mut self, other: &mut Self) {
        self.cartridge.take_rom_from(&mut other.cartridge);
        self.rom_patches = std::mem::take(&mut other.rom_patches);
        self.bios = std::mem::take(&mut other.bios);
        self.waits = WaitStates::new(self.waitcnt);
    }

    /// Replaces the ROM patches. Patches outside the cartridge's ROM window
    /// (0x08000000-0x0DFFFFFF) are dropped; the rest apply at their offset
    /// in all three wait-state mirrors.
    pub(crate) fn set_rom_patches(&mut self, patches: &[RomPatch]) {
        self.rom_patches = patches
            .iter()
            .filter(|patch| (0x0800_0000..0x0E00_0000).contains(&patch.address))
            .map(|patch| RomPatch {
                address: patch.address & 0x01FF_FFFF,
                ..*patch
            })
            .collect();
    }

    /// `value`, read from the cartridge at `address`, as the ROM patches
    /// change it. Cheat devices watched the cartridge bus, so they patch
    /// whatever answers there.
    fn patch_rom_16(&self, address: u32, value: u16) -> u16 {
        if self.rom_patches.is_empty() {
            return value;
        }
        let offset = address & 0x01FF_FFFE;
        let [low, high] = value.to_le_bytes();
        u16::from_le_bytes([
            RomPatch::apply(&self.rom_patches, offset, low),
            RomPatch::apply(&self.rom_patches, offset | 1, high),
        ])
    }

    pub(crate) const fn cycles(&self) -> u64 {
        self.cycles
    }

    pub(crate) fn halt(&mut self) {
        self.halted = true;
    }

    /// Notes that the VBlankIntrWait call made by the SWI at `swi` halted
    /// the CPU, which repeats the call once it wakes.
    pub(crate) fn remember_vblank_wait(&mut self, swi: u32) {
        self.vblank_wait_swi = Some(swi);
    }

    /// Whether the VBlankIntrWait call made by the SWI at `swi` repeats one
    /// that halted. Answers yes once per halt; calls from elsewhere leave
    /// the halted call's note alone.
    pub(crate) fn resumes_vblank_wait(&mut self, swi: u32) -> bool {
        let resumes = self.vblank_wait_swi == Some(swi);
        if resumes {
            self.vblank_wait_swi = None;
        }
        resumes
    }

    /// Forgets a halted VBlankIntrWait call that will never repeat.
    pub(crate) fn forget_vblank_wait(&mut self) {
        self.vblank_wait_swi = None;
    }

    pub(crate) fn set_bios_open_bus(&mut self, value: u32) {
        self.bios_open_bus = value;
    }

    /// Whether the CPU's IRQ input is high: an enabled interrupt is
    /// requested and IME is set, as of a few cycles ago (see
    /// [`crate::interrupts`]).
    pub(crate) fn irq_line(&self) -> bool {
        self.interrupts.line()
    }

    /// Wakes from HALT when any enabled interrupt is requested, whatever
    /// IME says; waking takes a cycle. Returns whether the CPU is running.
    pub(crate) fn wake_if_requested(&mut self) -> bool {
        if self.halted && self.interrupts.requested() {
            self.halted = false;
            self.tick(1);
        }
        !self.halted
    }

    /// While halted, skips straight to the next scheduled event.
    pub(crate) fn idle_to_next_event(&mut self) {
        let wait = self.scheduler.cycles_until_next().unwrap_or(1).max(1);
        self.tick(u32::try_from(wait).unwrap_or(u32::MAX));
    }

    /// Requests the interrupts in `bits` now.
    pub(crate) fn request_interrupt(&mut self, bits: u16) {
        self.request_interrupt_at(bits, self.cycles);
    }

    /// Requests the interrupts in `bits` at cycle `time`, which an event
    /// handler passes as the cycle its event was due.
    fn request_interrupt_at(&mut self, bits: u16, time: u64) {
        if bits != 0 {
            self.interrupts.request(bits);
            self.sync_interrupts_after(time);
        }
    }

    /// Lands the interrupt controller's pending changes a cycle after
    /// `time`.
    fn sync_interrupts_after(&mut self, time: u64) {
        self.scheduler
            .schedule_at(time + interrupts::SYNC_DELAY, Event::InterruptSync);
    }

    fn sync_interrupts(&mut self, time: u64) {
        let changes = self.interrupts.sync();
        if let Some(requested) = changes.requested {
            self.scheduler.schedule_at(
                time + interrupts::HALT_DELAY,
                Event::InterruptRequested(requested),
            );
        }
        if let Some(line) = changes.line {
            self.scheduler
                .schedule_at(time + interrupts::LINE_DELAY, Event::InterruptLine(line));
        }
    }

    /// Sets the held buttons (KEYINPUT bit order) and raises the keypad
    /// interrupt if KEYCNT asks for it.
    pub(crate) fn set_buttons(&mut self, pressed: u16) {
        self.keys = !pressed & 0x03FF;
        self.check_keypad_interrupt();
    }

    fn check_keypad_interrupt(&mut self) {
        let keycnt = self.io_16(0x132);
        if keycnt & (1 << 14) == 0 {
            return;
        }
        let selected = keycnt & 0x03FF;
        let pressed = !self.keys & 0x03FF & selected;
        let fire = if keycnt & (1 << 15) != 0 {
            pressed == selected
        } else {
            pressed != 0
        };
        if fire {
            self.request_interrupt(irq::KEYPAD);
        }
    }

    /// Advances time by `cycles` and runs every event that came due.
    fn tick(&mut self, cycles: u32) {
        self.prefetch.run(cycles, self.prefetch_enabled());
        self.cycles += u64::from(cycles);
        self.scheduler.advance(u64::from(cycles));
        while let Some(due) = self.scheduler.pop_due() {
            match due.event {
                Event::HBlank => {
                    let (request, signals) = self.ppu.start_hblank();
                    self.request_interrupt_at(request, due.time + 1);
                    if signals.hblank {
                        self.start_dmas(self.dma.triggered(Timing::HBlank), due.time);
                    }
                    if signals.capture {
                        self.start_dmas(self.dma.triggered_by_capture(), due.time);
                    }
                    self.scheduler.schedule_at(
                        due.time + ppu::LINE_CYCLES - ppu::HDRAW_CYCLES,
                        Event::LineStart,
                    );
                }
                Event::LineStart => {
                    let (request, signals) = self.ppu.start_line();
                    self.request_interrupt_at(request & ppu::irq::VBLANK, due.time + 1);
                    self.request_interrupt_at(request & ppu::irq::VCOUNT, due.time + 2);
                    if signals.vblank {
                        self.start_dmas(self.dma.triggered(Timing::VBlank), due.time);
                    }
                    if signals.capture_end {
                        self.dma.end_capture();
                    }
                    self.scheduler
                        .schedule_at(due.time + ppu::HDRAW_CYCLES, Event::HBlank);
                }
                Event::TimerOverflow(index) => {
                    let index = usize::from(index);
                    let overflowed = self.timers.overflow(index, due.time);
                    self.timers_overflowed(overflowed, due.time);
                    self.schedule_timer(index);
                }
                Event::TimerReload(index) => self.timers.apply_reload(usize::from(index)),
                Event::TimerControl(index) => {
                    let index = usize::from(index);
                    let overflowed = self.timers.apply_control(index, due.time);
                    self.timers_overflowed(overflowed, due.time);
                    self.schedule_timer(index);
                }
                Event::InterruptSync => self.sync_interrupts(due.time),
                Event::InterruptRequested(requested) => self.interrupts.set_requested(requested),
                Event::InterruptLine(line) => self.interrupts.set_line(line),
                Event::AudioSample => {
                    self.apu.sample();
                    self.scheduler
                        .schedule_at(due.time + apu::CYCLES_PER_SAMPLE, Event::AudioSample);
                }
                Event::DmaStart(index) => self.dma.start(usize::from(index)),
                Event::SerialDone => {
                    if self.sio.finish() {
                        self.request_interrupt_at(irq::SERIAL, due.time);
                    }
                }
            }
        }
    }

    /// Requests the interrupts and refills the sound FIFOs of the timers
    /// in `overflowed` (bit `n` for timer `n`), which overflowed at `time`.
    fn timers_overflowed(&mut self, overflowed: u8, time: u64) {
        for timer in (0..4).filter(|timer| overflowed & (1 << timer) != 0) {
            if self.timers.interrupt_enabled(timer) {
                self.request_interrupt_at(irq::TIMER0 << timer, time);
            }
            // Timers 0 and 1 clock the DMA sound FIFOs.
            if timer < 2 {
                let refill = self.apu.timer_overflow(timer);
                for fifo in (0..2).filter(|fifo| refill & (1 << fifo) != 0) {
                    self.start_dmas(self.dma.triggered_by_fifo(fifo), time);
                }
            }
        }
    }

    /// Ends the serial transfer just started (by this console or, through
    /// the cable, by another) after `cycles`.
    pub(crate) fn schedule_serial_done(&mut self, cycles: u64) {
        self.scheduler
            .cancel(|pending| *pending == Event::SerialDone);
        self.scheduler
            .schedule_at(self.cycles + cycles, Event::SerialDone);
    }

    fn serial_action(&mut self, action: sio::Action) {
        match action {
            sio::Action::None => {}
            sio::Action::Started(cycles) => self.schedule_serial_done(cycles),
            sio::Action::Cancelled => {
                self.scheduler
                    .cancel(|pending| *pending == Event::SerialDone);
            }
        }
    }

    /// Replaces timer `index`'s overflow event after it was reconfigured.
    fn schedule_timer(&mut self, index: usize) {
        let event = Event::TimerOverflow(index as u8);
        self.scheduler.cancel(|pending| *pending == event);
        if let Some(time) = self.timers.overflow_time(index) {
            self.scheduler.schedule_at(time, event);
        }
    }

    /// Starts the DMA channels in `set` (bit `n` for channel `n`), which
    /// their trigger at cycle `time` asked for, after their start delay.
    fn start_dmas(&mut self, set: u8, time: u64) {
        for index in (0..4u8).filter(|index| set & (1 << index) != 0) {
            self.scheduler
                .schedule_at(time + dma::START_DELAY, Event::DmaStart(index));
        }
    }

    /// Before a CPU access to the bus: any DMA that has started runs
    /// first, and the CPU waits. The second access of a SWP is locked to
    /// the first, so it never waits.
    #[inline]
    fn yield_to_dma(&mut self, access: Access) {
        self.idle_during_dma = 0;
        if self.dma.next_pending().is_some()
            && !access.contains(Access::DMA)
            && !access.contains(Access::LOCK)
        {
            self.run_dma();
        }
    }

    /// Lets `cycles` pass while the BIOS runs code from its own memory, a
    /// started DMA taking the bus at any of them.
    pub(crate) fn spend(&mut self, cycles: u32) {
        for _ in 0..cycles {
            self.yield_to_dma(Access::NONSEQUENTIAL);
            self.tick(1);
        }
    }

    /// Runs every started DMA transfer, highest priority first, while the
    /// CPU waits. Returns the cycles that took (0 if none had started).
    pub(crate) fn run_dma(&mut self) -> u32 {
        if self.dma.next_pending().is_none() {
            return 0;
        }
        let start = self.cycles;
        self.tick(1);
        while let Some(index) = self.dma.next_pending() {
            self.run_dma_channel(index);
        }
        self.tick(1);
        u32::try_from(self.cycles - start).unwrap_or(u32::MAX)
    }

    /// Moves channel `index`'s units until its transfer ends or a
    /// higher-priority channel starts, which then runs first.
    fn run_dma_channel(&mut self, index: usize) {
        let transfer = self.dma.transfer(index);
        let (mut source, mut destination) = (transfer.source, transfer.destination);
        if transfer.count == transfer.total && self.cartridge.eeprom_at(destination) {
            // The request's length tells the EEPROM's size.
            self.cartridge.note_eeprom_dma(transfer.total);
        }
        let mut latch = self.dma.latch(index);
        // Each run's first cartridge access is non-sequential; everything
        // else follows on.
        let mut cartridge_seen = false;
        let mut remaining = transfer.count;
        while remaining > 0 {
            let mut first_cartridge_access = |address: u32| {
                let first = !cartridge_seen && address >= 0x0800_0000;
                cartridge_seen |= first;
                if first {
                    Access::NONSEQUENTIAL | Access::DMA
                } else {
                    Access::SEQUENTIAL | Access::DMA
                }
            };
            let read_access = first_cartridge_access(source);
            let write_access = first_cartridge_access(destination);
            if let Some(value) = self.dma_read(source, transfer.words, read_access) {
                latch = value;
                self.dma_bus = value;
            }
            if transfer.words {
                self.write_32(destination, latch, write_access);
            } else {
                // A halfword goes out on its own lane of the latch.
                let half = (latch >> ((destination & 2) * 8)) as u16;
                self.write_16(destination, half, write_access);
            }
            source = source.wrapping_add_signed(transfer.source_step);
            destination = destination.wrapping_add_signed(transfer.destination_step);
            remaining -= 1;
            if remaining > 0 && self.dma.next_pending() != Some(index) {
                self.dma.set_latch(index, latch);
                self.dma.pause(index, source, destination, remaining);
                return;
            }
        }
        self.dma.set_latch(index, latch);
        if self.dma.finish(index, source, destination) {
            self.request_interrupt(irq::DMA0 << index);
        }
    }

    /// One DMA read of a word, or of a halfword returned in both halves.
    /// `None` below EWRAM, where nothing answers a DMA (the BIOS lets only
    /// its own code read it): the channel then moves its last value again.
    fn dma_read(&mut self, source: u32, words: bool, access: Access) -> Option<u32> {
        let width = if words { Width::Word } else { Width::Half };
        if source < 0x0200_0000 {
            self.wait_for_access(source, width, access);
            return None;
        }
        Some(if words {
            self.read_32(source, access)
        } else {
            u32::from(self.read_16(source, access)) * 0x0001_0001
        })
    }

    fn io_16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.io[offset], self.io[offset + 1]])
    }

    /// Reads the I/O register halfword at `offset`, with the bits the
    /// hardware does not read back as 0. `None` where nothing answers:
    /// write-only registers and unused space, which read as open bus.
    fn read_io_16(&self, offset: u32) -> Option<u16> {
        let offset = (offset & 0x3FE) as usize;
        Some(match offset {
            0x000..=0x05E => return self.ppu.read_register(offset),
            0x060..=0x0AE => return self.apu.read_16(offset as u32 - 0x60),
            0x0B0..=0x0DE => return self.dma.read(offset as u32 - 0xB0),
            0x100..=0x10E => self.timers.read(offset as u32 - 0x100, self.cycles),
            0x120..=0x12A | 0x134 => self.sio.read(offset as u32 - 0x120),
            0x130 => self.keys,
            0x132 => self.io_16(0x132) & 0xC3FF,
            0x140 => self.io_16(0x140) & 0x0040, // JOYCNT: IRQ enable
            0x158 => self.io_16(0x158) & 0x0030, // JOYSTAT: general-purpose bits
            0x200 => self.interrupts.enable(),
            0x202 => self.interrupts.flag(),
            0x204 => self.waitcnt,
            0x208 => u16::from(self.interrupts.master()),
            0x300 => self.io_16(0x300) & 0x00FF,
            // JOY_RECV and JOY_TRANS (no Joy Bus device is ever attached),
            // and the unused upper halves of 32-bit registers.
            0x136 | 0x142 | 0x150..=0x156 | 0x15A | 0x206 | 0x20A | 0x302 => 0,
            _ => return None,
        })
    }

    fn write_io_16(&mut self, offset: u32, value: u16) {
        let offset = (offset & 0x3FE) as usize;
        if offset != 0x202 {
            self.io[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        match offset {
            0x000..=0x056 => self.ppu.write_register(offset, value),
            0x060..=0x0A6 => {
                let offset = offset as u32 - 0x60;
                self.apu.write_8(offset, value as u8);
                self.apu.write_8(offset + 1, (value >> 8) as u8);
            }
            0x0B0..=0x0DE => {
                let offset = offset as u32 - 0xB0;
                if let Some(index) = self.dma.write(offset, value) {
                    self.start_dmas(1 << index, self.cycles);
                }
                // A channel disabled before it started never starts.
                let index = (offset / 12) as u8;
                if offset % 12 == 10 && !self.dma.enabled(usize::from(index)) {
                    self.scheduler
                        .cancel(|pending| *pending == Event::DmaStart(index));
                }
            }
            0x100..=0x10E => {
                // The write lands a cycle later.
                let event = match self.timers.stage(offset as u32 - 0x100, value) {
                    Staged::Reload(index) => Event::TimerReload(index as u8),
                    Staged::Control(index) => Event::TimerControl(index as u8),
                };
                self.scheduler.schedule_at(self.cycles + 1, event);
            }
            0x120..=0x12A | 0x134 => {
                let action = self.sio.write(offset as u32 - 0x120, value);
                self.serial_action(action);
            }
            0x132 => self.check_keypad_interrupt(),
            0x200 | 0x202 | 0x208 => {
                match offset {
                    0x200 => self.interrupts.write_enable(value),
                    0x202 => self.interrupts.acknowledge(value),
                    _ => self.interrupts.write_master(value & 1 != 0),
                }
                self.sync_interrupts_after(self.cycles);
            }
            0x204 => {
                let was_prefetching = self.prefetch_enabled();
                self.waitcnt = value & 0x5FFF;
                self.waits = WaitStates::new(self.waitcnt);
                if was_prefetching && !self.prefetch_enabled() {
                    self.prefetch_switched_off = true;
                }
            }
            _ => {}
        }
    }

    fn write_io_8(&mut self, offset: u32, value: u8) {
        let offset = offset & 0x3FF;
        match offset {
            // IE merges with what it is about to hold; IF clears the 1s in
            // this byte alone.
            0x200 | 0x201 => {
                let shift = (offset & 1) * 8;
                let merged =
                    (self.interrupts.next_enable() & !(0xFF << shift)) | u16::from(value) << shift;
                self.write_io_16(0x200, merged);
            }
            0x202 | 0x203 => self.write_io_16(0x202, u16::from(value) << ((offset & 1) * 8)),
            // IME's and the timer controls' upper bytes hold nothing.
            0x209 | 0x103 | 0x107 | 0x10B | 0x10F => {}
            0x300 => self.io[0x300] = value,
            // Sound registers take single bytes without touching neighbours.
            0x060..=0x0A7 => self.apu.write_8(offset - 0x60, value),
            0x301 => {
                // HALTCNT: bit 7 clear halts, set stops (treated as halt).
                self.halted = true;
            }
            _ => {
                let aligned = offset & !1;
                let mut bytes = self.io_16(aligned as usize).to_le_bytes();
                bytes[(offset & 1) as usize] = value;
                self.write_io_16(aligned, u16::from_le_bytes(bytes));
            }
        }
    }

    /// What an unmapped read sees: the last value on the bus, which is the
    /// last opcode fetched, or a DMA's last value right after a DMA.
    fn open_bus_16(&self, address: u32) -> u16 {
        let bus = if self.follows_dma {
            self.dma_bus
        } else {
            self.last_fetch
        };
        (bus >> ((address & 2) * 8)) as u16
    }

    fn read_16_raw(&mut self, address: u32) -> u16 {
        let half =
            |memory: &[u8], index: usize| u16::from_le_bytes([memory[index], memory[index + 1]]);
        match address >> 24 {
            0x00 if address < 0x4000 => {
                if self.executing_bios {
                    half(&self.bios, (address & 0x3FFE) as usize)
                } else {
                    (self.bios_open_bus >> ((address & 2) * 8)) as u16
                }
            }
            0x02 => half(&self.ewram, (address & 0x3_FFFE) as usize),
            0x03 => half(&self.iwram, (address & 0x7FFE) as usize),
            0x04 if address & 0x00FF_FFFF < 0x400 => self
                .read_io_16(address)
                .unwrap_or_else(|| self.open_bus_16(address)),
            0x05 => self.ppu.read_palette(address),
            0x06 => self.ppu.read_vram(address),
            0x07 => self.ppu.read_oam(address),
            0x08..=0x0D => {
                let value = self.cartridge.read_16(address);
                self.patch_rom_16(address, value)
            }
            // An 8-bit bus: the byte repeats across the halfword.
            0x0E | 0x0F => u16::from(self.cartridge.read_backup(address)) * 0x0101,
            _ => self.open_bus_16(address),
        }
    }

    fn write_16_raw(&mut self, address: u32, value: u16) {
        let put = |memory: &mut [u8], index: usize| {
            memory[index..index + 2].copy_from_slice(&value.to_le_bytes());
        };
        match address >> 24 {
            0x02 => put(&mut self.ewram, (address & 0x3_FFFE) as usize),
            0x03 => put(&mut self.iwram, (address & 0x7FFE) as usize),
            0x04 if address & 0x00FF_FFFF < 0x400 => self.write_io_16(address, value),
            0x05 => self.ppu.write_palette(address, value),
            0x06 => self.ppu.write_vram(address, value),
            0x07 => self.ppu.write_oam(address, value),
            0x08..=0x0D => self.cartridge.write_16(address, value, self.cycles),
            // Only the byte lane at the address reaches the 8-bit bus.
            0x0E | 0x0F => self
                .cartridge
                .write_backup(address, (value >> ((address & 1) * 8)) as u8),
            _ => {}
        }
    }

    fn read_8_raw(&mut self, address: u32) -> u8 {
        match address >> 24 {
            0x0E | 0x0F => self.cartridge.read_backup(address),
            _ => (self.read_16_raw(address) >> ((address & 1) * 8)) as u8,
        }
    }

    fn write_8_raw(&mut self, address: u32, value: u8) {
        match address >> 24 {
            0x02 => self.ewram[(address & 0x3_FFFF) as usize] = value,
            0x03 => self.iwram[(address & 0x7FFF) as usize] = value,
            0x04 if address & 0x00FF_FFFF < 0x400 => self.write_io_8(address, value),
            // Palette takes the byte in both halves; OAM ignores byte writes.
            0x05 => self
                .ppu
                .write_palette(address, u16::from_le_bytes([value, value])),
            0x06 => self.ppu.write_vram_byte(address, value),
            0x0E | 0x0F => self.cartridge.write_backup(address, value),
            _ => {}
        }
    }

    /// Reads EWRAM or IWRAM, for cheats; `None` elsewhere.
    pub(crate) fn peek_ram(&self, address: u32) -> Option<u8> {
        match address >> 24 {
            0x02 => Some(self.ewram[(address & 0x3_FFFF) as usize]),
            0x03 => Some(self.iwram[(address & 0x7FFF) as usize]),
            _ => None,
        }
    }

    /// KEYINPUT (0x04000130–0x04000131), for cheats that test buttons;
    /// `None` elsewhere.
    pub(crate) fn peek_keys(&self, address: u32) -> Option<u8> {
        match address {
            0x0400_0130 => Some(self.keys as u8),
            0x0400_0131 => Some((self.keys >> 8) as u8),
            _ => None,
        }
    }

    /// Writes EWRAM or IWRAM, for cheats. Returns whether `address` is RAM.
    pub(crate) fn poke_ram(&mut self, address: u32, value: u8) -> bool {
        match address >> 24 {
            0x02 => self.ewram[(address & 0x3_FFFF) as usize] = value,
            0x03 => self.iwram[(address & 0x7FFF) as usize] = value,
            _ => return false,
        }
        true
    }

    /// Writes the I/O register halfword at `address` (0x04000000-0x040003FE)
    /// as a CPU store would, for cheats, without taking time. POSTFLG and
    /// HALTCNT refuse: halting the CPU from outside its program would
    /// stall the game. Returns whether the register was written.
    pub(crate) fn poke_io_16(&mut self, address: u32, value: u16) -> bool {
        let offset = address.wrapping_sub(0x0400_0000) & !1;
        if offset >= 0x400 || offset == 0x300 {
            return false;
        }
        self.write_io_16(offset, value);
        true
    }

    /// The byte at `address` as the CPU reads it, without side effects or
    /// time passing, for debuggers: the BIOS (readable here even while
    /// code runs elsewhere), RAM, I/O registers (reads of which change
    /// nothing on the GBA; write-only ones show the value last written
    /// rather than open bus), palette RAM, VRAM, OAM, the ROM with its
    /// patches, and SRAM or Flash. `None` for the EEPROM and unmapped
    /// space.
    pub(crate) fn peek(&self, address: u32) -> Option<u8> {
        let byte = |half: u16| (half >> ((address & 1) * 8)) as u8;
        Some(match address >> 24 {
            0x00 if address < 0x4000 => self.bios[address as usize],
            0x02 | 0x03 => return self.peek_ram(address),
            // Write-only registers show what was last written.
            0x04 if address & 0x00FF_FFFF < 0x400 => byte(
                self.read_io_16(address)
                    .unwrap_or_else(|| self.io_16((address & 0x3FE) as usize)),
            ),
            0x05 => byte(self.ppu.read_palette(address)),
            0x06 => byte(self.ppu.read_vram(address)),
            0x07 => byte(self.ppu.read_oam(address)),
            0x08..=0x0D if !self.cartridge.eeprom_at(address) => {
                let offset = (address & 0x01FF_FFFE) as usize;
                let rom = self.cartridge.rom();
                // Past the end, the cartridge bus returns the address.
                let value = rom
                    .get(offset..offset + 2)
                    .map_or((address >> 1) as u16, |bytes| {
                        u16::from_le_bytes([bytes[0], bytes[1]])
                    });
                byte(self.patch_rom_16(address, value))
            }
            0x0E | 0x0F => self.cartridge.read_backup(address),
            _ => return None,
        })
    }

    /// Writes the byte at `address` without side effects or time passing,
    /// for debuggers: EWRAM, IWRAM, palette RAM, VRAM and OAM, a byte at a
    /// time (unlike CPU byte stores to video memory). Returns whether
    /// `address` is one of those.
    pub(crate) fn poke_memory(&mut self, address: u32, value: u8) -> bool {
        let merge = |half: u16| {
            let shift = (address & 1) * 8;
            (half & !(0xFF << shift)) | u16::from(value) << shift
        };
        match address >> 24 {
            0x02 | 0x03 => return self.poke_ram(address, value),
            0x05 => {
                let half = merge(self.ppu.read_palette(address));
                self.ppu.write_palette(address, half);
            }
            0x06 => {
                let half = merge(self.ppu.read_vram(address));
                self.ppu.write_vram(address, half);
            }
            0x07 => {
                let half = merge(self.ppu.read_oam(address));
                self.ppu.write_oam(address, half);
            }
            _ => return false,
        }
        true
    }

    /// The event scheduler, for debuggers.
    pub(crate) fn scheduler(&self) -> &Scheduler<Event> {
        &self.scheduler
    }

    /// Whether the CPU is halted, waiting for an interrupt.
    pub(crate) fn halted(&self) -> bool {
        self.halted
    }

    /// Whether WAITCNT turns the Game Pak prefetch unit on.
    fn prefetch_enabled(&self) -> bool {
        self.waitcnt & 0x4000 != 0
    }

    /// Spends the cycles an access of `width` to `address` takes: the
    /// region's wait states, with the prefetch unit's help for code from
    /// the cartridge.
    #[inline]
    fn wait_for_access(&mut self, address: u32, width: Width, access: Access) {
        let dma = access.contains(Access::DMA);
        self.follows_dma = std::mem::replace(&mut self.last_access_dma, dma);
        let region = address >> 24;
        if access.contains(Access::CODE) {
            self.code_in_cartridge = (0x08..0x0E).contains(&region);
        }
        if (0x08..=0x0F).contains(&region) {
            self.wait_for_cartridge(address, width, access);
        } else {
            let sequential = access.contains(Access::SEQUENTIAL);
            self.tick(self.waits.cycles(address, width, sequential));
        }
    }

    /// [`Bus::wait_for_access`] for the cartridge's ROM and save memory.
    fn wait_for_cartridge(&mut self, address: u32, width: Width, access: Access) {
        if address >> 24 >= 0x0E {
            self.stop_prefetch();
            self.tick(self.waits.cycles(address, width, false));
            return;
        }
        // A new 128 KiB page, or the CPU's first access after a DMA, starts
        // a new burst.
        let after_dma = self.follows_dma && !access.contains(Access::DMA);
        let sequential =
            access.contains(Access::SEQUENTIAL) && address & 0x1_FFFF != 0 && !after_dma;
        if access.contains(Access::CODE) {
            self.fetch_from_cartridge(address, width, sequential);
        } else {
            self.stop_prefetch();
            self.tick(self.waits.cycles(address, width, sequential));
        }
    }

    /// The cycles of a code fetch from the cartridge, which the prefetch
    /// unit may already have read.
    fn fetch_from_cartridge(&mut self, address: u32, width: Width, sequential: bool) {
        let enabled = self.prefetch_enabled();
        match self.prefetch.look_up(address) {
            Lookup::Buffered => {
                self.prefetch.take(address, enabled);
                self.tick(1);
            }
            Lookup::InFlight(cycles) => {
                self.tick(cycles);
                self.prefetch.take(address, enabled);
            }
            Lookup::Miss => {
                self.stop_prefetch();
                let sequential = sequential && !std::mem::take(&mut self.prefetch_switched_off);
                self.tick(self.waits.cycles(address, width, sequential));
                if enabled {
                    let opcode = if width == Width::Word { 4 } else { 2 };
                    let duty = self.waits.cycles(address, width, true);
                    self.prefetch.start(address, opcode, duty);
                }
            }
        }
    }

    /// Stops the prefetch unit for a data access to the cartridge, waiting
    /// a cycle if its read cannot be cut short.
    fn stop_prefetch(&mut self) {
        if self.prefetch.stop(self.code_in_cartridge) {
            self.tick(1);
        }
    }

    /// Records an instruction fetch's value: the BIOS's last opcode for
    /// read protection, and the bus's open-bus value.
    fn note_fetch(&mut self, value: u32) {
        if self.executing_bios {
            self.bios_open_bus = value;
        }
        self.last_fetch = value;
    }
}

/// Whether `address` is in the SRAM/Flash region, which sits on an 8-bit
/// bus and sees unaligned addresses as they are.
fn is_save_region(address: u32) -> bool {
    matches!(address >> 24, 0x0E | 0x0F)
}

impl CpuBus for Bus {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        self.yield_to_dma(access);
        if is_save_region(address) {
            self.wait_for_access(address, Width::Word, access);
            return u32::from(self.cartridge.read_backup(address)) * 0x0101_0101;
        }
        let address = address & !3;
        self.wait_for_access(address, Width::Word, access);
        let code = access.contains(Access::CODE);
        if code {
            // Fetching from the BIOS is what unlocks reading it.
            self.executing_bios = address < 0x4000;
        }
        let value = if address < 0x4000 && !self.executing_bios {
            self.bios_open_bus
        } else {
            u32::from(self.read_16_raw(address)) | u32::from(self.read_16_raw(address | 2)) << 16
        };
        if code {
            self.note_fetch(value);
        }
        value
    }

    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        self.yield_to_dma(access);
        if is_save_region(address) {
            self.wait_for_access(address, Width::Half, access);
            return u16::from(self.cartridge.read_backup(address)) * 0x0101;
        }
        let address = address & !1;
        self.wait_for_access(address, Width::Half, access);
        let code = access.contains(Access::CODE);
        if code {
            self.executing_bios = address < 0x4000;
        }
        let value = self.read_16_raw(address);
        if code {
            // Thumb fetches leave the halfword in both halves of the bus.
            self.note_fetch(u32::from(value) * 0x0001_0001);
        }
        value
    }

    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        self.yield_to_dma(access);
        self.wait_for_access(address, Width::Byte, access);
        self.read_8_raw(address)
    }

    fn write_32(&mut self, address: u32, value: u32, access: Access) {
        self.yield_to_dma(access);
        if is_save_region(address) {
            // The byte lane at the (unaligned) address reaches the 8-bit bus.
            self.wait_for_access(address, Width::Word, access);
            let byte = value.rotate_right((address & 3) * 8) as u8;
            self.cartridge.write_backup(address, byte);
            return;
        }
        let address = address & !3;
        self.wait_for_access(address, Width::Word, access);
        self.write_16_raw(address, value as u16);
        self.write_16_raw(address | 2, (value >> 16) as u16);
    }

    fn write_16(&mut self, address: u32, value: u16, access: Access) {
        self.yield_to_dma(access);
        if is_save_region(address) {
            self.wait_for_access(address, Width::Half, access);
            let byte = (value >> ((address & 1) * 8)) as u8;
            self.cartridge.write_backup(address, byte);
            return;
        }
        let address = address & !1;
        self.wait_for_access(address, Width::Half, access);
        self.write_16_raw(address, value);
    }

    fn write_8(&mut self, address: u32, value: u8, access: Access) {
        self.yield_to_dma(access);
        self.wait_for_access(address, Width::Byte, access);
        self.write_8_raw(address, value);
    }

    /// An internal cycle. A DMA that has started runs now, and the CPU's
    /// internal cycles overlap it until the CPU needs the bus again.
    fn idle(&mut self) {
        if self.dma.next_pending().is_some() {
            self.idle_during_dma = self.run_dma();
        }
        if self.idle_during_dma == 0 {
            self.tick(1);
        } else {
            self.idle_during_dma -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eeprom_bus() -> Bus {
        let mut rom = vec![0; 0x400];
        rom[0x200..0x208].copy_from_slice(b"EEPROM_V");
        Bus::new(
            Cartridge::new(rom.into_boxed_slice(), None, 0),
            Box::default(),
        )
    }

    /// Sets DMA 3's registers, `control` last.
    fn dma3_registers(bus: &mut Bus, source: u32, destination: u32, count: u16, control: u16) {
        bus.write_io_16(0xD4, source as u16);
        bus.write_io_16(0xD6, (source >> 16) as u16);
        bus.write_io_16(0xD8, destination as u16);
        bus.write_io_16(0xDA, (destination >> 16) as u16);
        bus.write_io_16(0xDC, count);
        bus.write_io_16(0xDE, control);
    }

    /// Runs DMA 3 copying `count` halfwords from `source` to `destination`.
    fn dma3(bus: &mut Bus, source: u32, destination: u32, count: u16) {
        dma3_registers(bus, source, destination, count, 0x8000);
        bus.tick(dma::START_DELAY as u32);
        bus.run_dma();
    }

    /// Stores `bits`, one per halfword, at 0x02000000.
    fn stage(bus: &mut Bus, bits: &[u16]) {
        for (index, bit) in (0u32..).zip(bits) {
            bus.write_16(0x0200_0000 + index * 2, *bit, Access::NONSEQUENTIAL);
        }
    }

    #[test]
    fn eeprom_round_trips_a_block_through_dma() {
        let mut bus = eeprom_bus();
        let data: Vec<u16> = (0..64).map(|bit| u16::from(bit % 3 == 0)).collect();
        // Write block 5: 1 0, a 6-bit address, 64 data bits, 0.
        let address = [0, 0, 0, 1, 0, 1];
        let write: Vec<u16> = [1, 0]
            .iter()
            .chain(&address)
            .chain(&data)
            .chain(&[0])
            .copied()
            .collect();
        stage(&mut bus, &write);
        dma3(&mut bus, 0x0200_0000, 0x0D00_0000, 73);
        assert_eq!(bus.cartridge.backup_generation(), Some(1));
        // Read it back: 1 1, the address, 0; then 4 junk bits and the data.
        let read: Vec<u16> = [1, 1].iter().chain(&address).chain(&[0]).copied().collect();
        stage(&mut bus, &read);
        dma3(&mut bus, 0x0200_0000, 0x0D00_0000, 9);
        dma3(&mut bus, 0x0D00_0000, 0x0200_1000, 68);
        let received: Vec<u16> = (0..68)
            .map(|index| bus.read_16(0x0200_1000 + index * 2, Access::NONSEQUENTIAL) & 1)
            .collect();
        assert_eq!(received[4..], data[..]);
    }

    #[test]
    fn video_capture_copies_lines_2_to_161_then_stops() {
        let mut bus = blank_bus();
        // DMA 3, one halfword a line from EWRAM to VRAM: repeat, video
        // capture timing.
        dma3_registers(&mut bus, 0x0200_0000, 0x0600_0000, 1, 0xB200);
        // Runs through the start of `line`, returning the lines copied.
        let run_to_line = |bus: &mut Bus, line: u64| {
            while bus.cycles() < line * ppu::LINE_CYCLES {
                bus.idle_to_next_event();
                bus.run_dma();
            }
            (bus.dma.transfer(3).destination - 0x0600_0000) / 2
        };
        assert_eq!(run_to_line(&mut bus, 2), 0);
        assert_eq!(run_to_line(&mut bus, 3), 1);
        assert_eq!(run_to_line(&mut bus, 161), 159);
        assert!(bus.dma.enabled(3));
        assert_eq!(run_to_line(&mut bus, 162), 160);
        assert!(!bus.dma.enabled(3));
        // Off, it waits out the next frame.
        assert_eq!(run_to_line(&mut bus, 228 + 163), 160);
    }

    #[test]
    fn dma_from_the_bios_region_moves_the_channels_last_value_again() {
        let mut bus = eeprom_bus();
        bus.write_32(0x0200_0000, 0xCAFE_BABE, Access::NONSEQUENTIAL);
        // DMA 3, one word, from EWRAM.
        bus.write_32(0x0400_00D4, 0x0200_0000, Access::NONSEQUENTIAL);
        bus.write_32(0x0400_00D8, 0x0300_0000, Access::NONSEQUENTIAL);
        bus.write_32(0x0400_00DC, 0x8400_0001, Access::NONSEQUENTIAL);
        bus.tick(dma::START_DELAY as u32);
        bus.run_dma();
        // Two halfwords from the BIOS, which a DMA cannot read: each
        // repeats the latched word's half on its own lane.
        dma3(&mut bus, 0x0000_0000, 0x0300_0100, 2);
        assert_eq!(bus.read_32(0x0300_0100, Access::NONSEQUENTIAL), 0xCAFE_BABE);
    }

    /// Enables DMA 3 to copy one halfword from IWRAM to IWRAM, returning
    /// the cycle the enabling write happened.
    fn enable_iwram_dma(bus: &mut Bus) -> u64 {
        let n = Access::NONSEQUENTIAL;
        bus.write_32(0x0300_0000, 0x1234, n);
        bus.write_32(0x0400_00D4, 0x0300_0000, n);
        bus.write_32(0x0400_00D8, 0x0300_0100, n);
        bus.write_32(0x0400_00DC, 0x8000_0001, n);
        bus.cycles()
    }

    #[test]
    fn dma_starts_two_cycles_after_enabling_and_waits_for_the_cpus_next_access() {
        let mut bus = blank_bus();
        let n = Access::NONSEQUENTIAL;
        let enabled = enable_iwram_dma(&mut bus);
        // Two one-cycle accesses fit before the start...
        bus.read_16(0x0300_0004, n);
        bus.read_16(0x0300_0004, n);
        assert_eq!(bus.cycles() - enabled, 2);
        assert_eq!(bus.read_io_16(0x0DE), Some(0x8000));
        // ...and the next waits for the transfer: a cycle to start, a read,
        // a write and a cycle to finish.
        bus.read_16(0x0300_0004, n);
        assert_eq!(bus.cycles() - enabled, 2 + 4 + 1);
        assert_eq!(bus.read_16(0x0300_0100, n), 0x1234);
        assert_eq!(bus.read_io_16(0x0DE), Some(0));
    }

    #[test]
    fn an_unmapped_read_right_after_a_dma_sees_its_value() {
        let mut bus = blank_bus();
        enable_iwram_dma(&mut bus);
        bus.tick(2);
        assert_eq!(bus.read_16(0x0100_0000, Access::NONSEQUENTIAL), 0x1234);
        assert_eq!(bus.read_16(0x0100_0000, Access::NONSEQUENTIAL), 0);
    }

    #[test]
    fn internal_cycles_overlap_a_dma() {
        let mut bus = blank_bus();
        let enabled = enable_iwram_dma(&mut bus);
        bus.tick(2);
        // The transfer takes four cycles, which cover four internal cycles.
        for _ in 0..5 {
            bus.idle();
        }
        assert_eq!(bus.cycles() - enabled, 2 + 4 + 1);
    }

    #[test]
    fn sram_sees_one_byte_lane() {
        let mut rom = vec![0; 0x400];
        rom[0x200..0x206].copy_from_slice(b"SRAM_V");
        let mut bus = Bus::new(
            Cartridge::new(rom.into_boxed_slice(), None, 0),
            Box::default(),
        );
        bus.write_32(0x0E00_0002, 0xAABB_CCDD, Access::NONSEQUENTIAL);
        assert_eq!(bus.read_8(0x0E00_0002, Access::NONSEQUENTIAL), 0xBB);
        assert_eq!(bus.read_8(0x0E00_0003, Access::NONSEQUENTIAL), 0xFF);
        assert_eq!(bus.read_32(0x0E00_0002, Access::NONSEQUENTIAL), 0xBBBB_BBBB);
    }

    #[test]
    fn rom_patches_cover_every_mirror_and_width() {
        let rom: Vec<u8> = (0..0x400).map(|index| index as u8).collect();
        let mut bus = Bus::new(Cartridge::new(rom.into(), None, 0), Box::default());
        let patch = |address, value| RomPatch {
            address,
            value,
            compare: None,
        };
        bus.set_rom_patches(&[
            patch(0x0800_0100, 0xCD),
            patch(0x0800_0101, 0xAB),
            // Not the cartridge's ROM: dropped.
            patch(0x0200_0000, 0x11),
        ]);
        assert_eq!(bus.rom_patches.len(), 2);
        for mirror in [0x0800_0100, 0x0A00_0100, 0x0C00_0100] {
            assert_eq!(bus.read_16(mirror, Access::NONSEQUENTIAL), 0xABCD);
        }
        assert_eq!(bus.read_8(0x0800_0101, Access::NONSEQUENTIAL), 0xAB);
        assert_eq!(bus.read_32(0x0800_0100, Access::NONSEQUENTIAL), 0x0302_ABCD);
        assert_eq!(bus.read_16(0x0800_0102, Access::NONSEQUENTIAL), 0x0302);

        // States leave them out, but resets and loaded states keep them;
        // an empty list removes them.
        let mut bus = Bus::reset_from(&mut bus);
        let state = postcard::to_stdvec(&bus).expect("serializes");
        let mut loaded: Bus = postcard::from_bytes(&state).expect("deserializes");
        assert!(loaded.rom_patches.is_empty());
        loaded.take_media_from(&mut bus);
        assert_eq!(loaded.read_16(0x0800_0100, Access::NONSEQUENTIAL), 0xABCD);
        loaded.set_rom_patches(&[]);
        assert_eq!(loaded.read_16(0x0800_0100, Access::NONSEQUENTIAL), 0x0100);
    }

    #[test]
    fn cheat_io_writes_act_like_stores_but_never_halt() {
        let mut bus = Bus::new(
            Cartridge::new(vec![0; 0x400].into(), None, 0),
            Box::default(),
        );
        let cycles = bus.cycles();
        assert!(bus.poke_io_16(0x0400_0000, 0x0403));
        assert_eq!(bus.read_io_16(0x000), Some(0x0403));
        // IF: writing 1 clears the bit.
        bus.request_interrupt(0x0009);
        assert!(bus.poke_io_16(0x0400_0202, 0x0001));
        // POSTFLG and HALTCNT, and anything outside the I/O registers.
        for address in [0x0400_0300, 0x0400_0301, 0x0400_0400, 0x0200_0000] {
            assert!(!bus.poke_io_16(address, 0), "{address:08X}");
        }
        assert!(!bus.halted);
        assert_eq!(bus.cycles(), cycles);
        // Like a store's, the IF write lands a cycle later.
        bus.tick(1);
        assert_eq!(bus.read_io_16(0x202), Some(0x0008));
    }

    /// A bus with an empty cartridge.
    fn blank_bus() -> Bus {
        Bus::new(
            Cartridge::new(vec![0; 0x400].into(), None, 0),
            Box::default(),
        )
    }

    #[test]
    fn write_only_registers_read_as_open_bus_and_unused_halves_as_zero() {
        let mut rom = vec![0; 0x400];
        rom[..4].copy_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
        let mut bus = Bus::new(Cartridge::new(rom.into(), None, 0), Box::default());
        let n = Access::NONSEQUENTIAL;
        bus.read_32(0x0800_0000, Access::CODE);
        bus.write_16(0x0400_0010, 0x1234, n); // BG0HOFS
        assert_eq!(bus.read_16(0x0400_0010, n), 0xBEEF);
        assert_eq!(bus.read_16(0x0400_0012, n), 0xDEAD);
        assert_eq!(bus.read_32(0x0400_00A0, n), 0xDEAD_BEEF); // FIFO A
        assert_eq!(bus.read_16(0x0400_0066, n), 0);
        assert_eq!(bus.read_16(0x0400_020A, n), 0);
        bus.write_16(0x0400_0008, 0xFFFF, n); // BG0CNT
        assert_eq!(bus.read_16(0x0400_0008, n), 0xDFFF);
        // Debuggers see what was written.
        assert_eq!(bus.peek(0x0400_0010), Some(0x34));
    }

    #[test]
    fn prefetched_opcodes_take_a_cycle_and_data_reads_stop_the_unit() {
        let mut bus = blank_bus();
        let (code, sequential) = (Access::CODE, Access::CODE | Access::SEQUENTIAL);
        let elapsed = |bus: &mut Bus, read: &dyn Fn(&mut Bus)| {
            let start = bus.cycles();
            read(bus);
            bus.cycles() - start
        };
        // Prefetch on, cartridge at 4 and 2 wait states.
        bus.write_io_16(0x204, 0x4000);
        assert_eq!(
            elapsed(&mut bus, &|bus| _ = bus.read_16(0x0800_0000, code)),
            5
        );
        // Six free cycles read the next two halfwords.
        for _ in 0..6 {
            bus.idle();
        }
        let fetch = |address| move |bus: &mut Bus| _ = bus.read_16(address, sequential);
        assert_eq!(elapsed(&mut bus, &fetch(0x0800_0002)), 1);
        assert_eq!(elapsed(&mut bus, &fetch(0x0800_0004)), 1);
        // Those two cycles brought the third's read within a cycle of done.
        assert_eq!(elapsed(&mut bus, &fetch(0x0800_0006)), 1);
        // A data read from the cartridge stops the unit.
        bus.read_16(0x0800_0100, Access::NONSEQUENTIAL);
        bus.idle();
        assert_eq!(elapsed(&mut bus, &fetch(0x0800_0008)), 3);
    }

    #[test]
    fn interrupts_reach_the_cpu_three_cycles_after_the_request() {
        let mut bus = blank_bus();
        bus.write_io_16(0x200, 0x0008);
        bus.write_io_16(0x208, 1);
        bus.tick(1);
        bus.request_interrupt(0x0008);
        assert_eq!(bus.read_io_16(0x202), Some(0));
        bus.tick(1);
        assert_eq!(bus.read_io_16(0x202), Some(0x0008));
        assert!(!bus.irq_line());
        bus.tick(1);
        assert!(!bus.irq_line());
        bus.tick(1);
        assert!(bus.irq_line());
        // Acknowledging lowers the line just as late.
        bus.write_io_16(0x202, 0x0008);
        bus.tick(2);
        assert!(bus.irq_line());
        bus.tick(1);
        assert!(!bus.irq_line());
    }

    #[test]
    fn halt_wakes_two_cycles_after_the_request_and_a_cycle_later_runs() {
        let mut bus = blank_bus();
        bus.write_io_16(0x200, 0x0001);
        bus.halt();
        let start = bus.cycles();
        bus.request_interrupt(0x0001);
        while !bus.wake_if_requested() {
            bus.idle_to_next_event();
        }
        assert_eq!(bus.cycles() - start, 3);
    }

    #[test]
    fn a_timer_write_lands_a_cycle_later_and_counts_a_cycle_after_that() {
        let mut bus = blank_bus();
        let start = bus.cycles();
        // Reload 0xFFFE, enabled, divide by 1, with its interrupt.
        bus.write_32(0x0400_0100, 0x00C0_FFFE, Access::NONSEQUENTIAL);
        let written = bus.cycles();
        assert_eq!(written - start, 1);
        bus.write_io_16(0x200, 0x0008);
        bus.tick(1);
        assert_eq!(bus.read_io_16(0x102), Some(0x00C0));
        assert_eq!(bus.read_io_16(0x100), Some(0xFFFE));
        bus.tick(1);
        assert_eq!(bus.read_io_16(0x100), Some(0xFFFE));
        bus.tick(1);
        assert_eq!(bus.read_io_16(0x100), Some(0xFFFF));
        // It overflows at written + 4, and IF shows it a cycle later.
        bus.tick(1);
        assert_eq!(bus.read_io_16(0x100), Some(0xFFFE));
        assert_eq!(bus.read_io_16(0x202), Some(0));
        bus.tick(1);
        assert_eq!(bus.read_io_16(0x202), Some(0x0008));
    }
}
