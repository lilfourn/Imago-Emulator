//! The two CPUs and the hardware between them, run in lockstep.
//!
//! The machine advances in short slices: up to the next scheduled event,
//! and never more than [`SLICE_CYCLES`] bus cycles. In each slice the ARM9
//! runs until its clock reaches the slice's end, then the ARM7 does, then
//! the events due by then fire. A CPU therefore sees the other's writes at
//! most a slice late, which the games' IPC handshakes tolerate.

use emu_arm::{Bus, Cpu, Mode, Registers, psr};
use serde::{Deserialize, Serialize};

use crate::bios::{self, Call, Effects};
use crate::boot::{self, ARM7_STACKS, ARM9_STACKS, Program};
use crate::bus::{Arm7Bus, Arm9Bus};
use crate::cart::Cart;
use crate::display::Moment;
use crate::hardware::{Event, Hardware};
use crate::interrupt::irq;
use crate::ipc::Side;

/// The longest slice, in bus cycles.
const SLICE_CYCLES: u64 = 64;

/// The ARM9's processor. Until the ARM946E-S model lands it runs on the
/// ARMv4T interpreter.
pub(crate) type Arm9 = Cpu;

/// What a system call needs from either CPU model.
pub(crate) trait Processor {
    fn reg(&self, index: usize) -> u32;
    fn set_reg(&mut self, index: usize, value: u32);
    fn saved_psr(&self) -> Option<u32>;
    fn return_from_exception<B: Bus>(&mut self, bus: &mut B);
}

impl Processor for Cpu {
    fn reg(&self, index: usize) -> u32 {
        Cpu::reg(self, index)
    }
    fn set_reg(&mut self, index: usize, value: u32) {
        Cpu::set_reg(self, index, value);
    }
    fn saved_psr(&self) -> Option<u32> {
        Cpu::saved_psr(self)
    }
    fn return_from_exception<B: Bus>(&mut self, bus: &mut B) {
        Cpu::return_from_exception(self, bus);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Machine {
    pub(crate) arm9: Arm9,
    pub(crate) arm7: Cpu,
    pub(crate) hw: Hardware,
    /// System calls are emulated in Rust rather than run from BIOS dumps.
    pub(crate) hle_bios: bool,
}

/// What happened while running, for the core to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Progress {
    /// VBlank started: a frame is complete.
    pub(crate) frame_done: bool,
}

/// The registers a CPU starts a program with: entry point, stacks for
/// Supervisor, IRQ and System mode, in System mode.
fn boot_registers(program: Program, stacks: (u32, u32, u32)) -> Registers {
    let (supervisor, irq_stack, system) = stacks;
    let mut registers = Registers {
        cpsr: Mode::System.bits(),
        ..Registers::default()
    };
    registers.r[12] = program.entry;
    registers.r[13] = system;
    registers.r[14] = program.entry;
    registers.usr[4] = program.entry;
    registers.usr[5] = system;
    registers.usr[6] = program.entry;
    registers.svc[0] = supervisor;
    registers.irq[0] = irq_stack;
    registers
}

impl Machine {
    /// A machine that has direct-booted the cartridge in `hw`.
    pub(crate) fn boot(mut hw: Hardware, hle_bios: bool) -> Self {
        let cart = std::mem::replace(&mut hw.cart, Cart::new(Box::default(), None));
        let user_settings = hw.spi.user_settings();
        let header = boot::load(&mut hw, cart.rom(), &user_settings, cart.chip_id());
        hw.cart = cart;
        hw.start_display();
        let mut arm9 = Arm9::from_registers(
            &boot_registers(header.arm9, ARM9_STACKS),
            [0, 0],
            emu_arm::Access::NONSEQUENTIAL,
        );
        let mut arm7 = Cpu::from_registers(
            &boot_registers(header.arm7, ARM7_STACKS),
            [0, 0],
            emu_arm::Access::NONSEQUENTIAL,
        );
        arm9.jump(&mut Arm9Bus(&mut hw), header.arm9.entry);
        arm7.jump(&mut Arm7Bus(&mut hw), header.arm7.entry);
        Self {
            arm9,
            arm7,
            hw,
            hle_bios,
        }
    }

    /// Bus cycles since power-on, as the scheduler counts them.
    pub(crate) fn now(&self) -> u64 {
        self.hw.scheduler.now()
    }

    /// Runs one slice. Returns what the events in it did.
    pub(crate) fn run_slice(
        &mut self,
        mut on_moment: impl FnMut(&mut Hardware, Moment),
    ) -> Progress {
        let now = self.hw.scheduler.now();
        let next = self.hw.scheduler.next_time().unwrap_or(u64::MAX);
        let target = next.clamp(now + 1, now + SLICE_CYCLES);
        while self.hw.arm9.cycles < target * 2 {
            self.step_arm9(target * 2);
        }
        while self.hw.arm7.cycles < target {
            self.step_arm7(target);
        }
        self.hw.scheduler.advance(target - now);
        let mut progress = Progress::default();
        while let Some(due) = self.hw.scheduler.pop_due() {
            match due.event {
                Event::HBlank => {
                    let moment = self.hw.hblank(due.time);
                    on_moment(&mut self.hw, moment);
                }
                Event::LineStart => {
                    let moment = self.hw.line_start(due.time);
                    progress.frame_done |=
                        moment == Moment::LineStart(crate::display::VISIBLE_LINES);
                    on_moment(&mut self.hw, moment);
                }
                Event::TimerOverflow(side, index) => {
                    self.timer_overflow(side, usize::from(index), due.time)
                }
                Event::SoundSample => self.hw.sound_sample(due.time),
                Event::CartStep => {
                    let event = self.hw.cart.rom_step();
                    self.hw.rom_event(event);
                }
                Event::CartSpiDone => self.hw.cart.finish_spi(),
                Event::SpiDone => {
                    if self.hw.spi.finish() {
                        self.hw.arm7.interrupts.request(irq::SPI);
                    }
                }
            }
        }
        progress
    }

    fn timer_overflow(&mut self, side: Side, index: usize, time: u64) {
        let cpu = self.hw.cpu_mut(side);
        let overflowed = cpu.timers.overflow(index, time);
        for timer in (0..4).filter(|timer| overflowed & (1 << timer) != 0) {
            if cpu.timers.interrupt_enabled(timer) {
                cpu.interrupts.request(irq::TIMER0 << timer);
            }
        }
        self.hw.schedule_timer(side, index);
    }

    /// One ARM9 instruction, DMA run or stretch of halt, up to `target` in
    /// ARM9 cycles.
    fn step_arm9(&mut self, target: u64) {
        let hw = &mut self.hw;
        if hw.run_dma(Side::Arm9) {
            return;
        }
        if hw.arm9.halted {
            if !hw.arm9.interrupts.pending() {
                hw.arm9.cycles = hw.arm9.cycles.max(target);
                return;
            }
            hw.arm9.halted = false;
        }
        let entered_swi = self.arm9.reg(15) == bios::ARM9_SWI_ENTERED_PC
            && !self.arm9.thumb()
            && self.arm9.mode() == Mode::Supervisor;
        if self.hle_bios && entered_swi {
            // The BIOS keeps its interrupt flags at the top of DTCM.
            let intr_flags = boot::DTCM_REGION & !0xFFF;
            system_call(
                &mut self.arm9,
                &mut Arm9Bus(hw),
                Side::Arm9,
                intr_flags + 0x3FF8,
            );
            return;
        }
        self.arm9.set_irq_line(hw.arm9.interrupts.line());
        self.arm9.step(&mut Arm9Bus(hw));
    }

    /// One ARM7 instruction, DMA run or stretch of halt, up to `target` in
    /// bus cycles.
    fn step_arm7(&mut self, target: u64) {
        let hw = &mut self.hw;
        if hw.run_dma(Side::Arm7) {
            return;
        }
        if hw.arm7.halted {
            if !hw.arm7.interrupts.pending() {
                hw.arm7.cycles = hw.arm7.cycles.max(target);
                return;
            }
            hw.arm7.halted = false;
        }
        let entered_swi = self.arm7.reg(15) == bios::ARM7_SWI_ENTERED_PC
            && !self.arm7.thumb()
            && self.arm7.mode() == Mode::Supervisor;
        if self.hle_bios && entered_swi {
            system_call(&mut self.arm7, &mut Arm7Bus(hw), Side::Arm7, 0x0380_FFF8);
            return;
        }
        self.arm7.set_irq_line(hw.arm7.interrupts.line());
        self.arm7.step(&mut Arm7Bus(hw));
    }
}

/// A bus that can halt and stall its CPU, for system calls.
pub(crate) trait CpuBus: Bus {
    fn halt(&mut self);
    fn stall(&mut self, cycles: u64);
}

impl CpuBus for Arm9Bus<'_> {
    fn halt(&mut self) {
        self.0.arm9.halted = true;
    }
    fn stall(&mut self, cycles: u64) {
        self.0.arm9.cycles += cycles;
    }
}

impl CpuBus for Arm7Bus<'_> {
    fn halt(&mut self) {
        self.0.arm7.halted = true;
    }
    fn stall(&mut self, cycles: u64) {
        self.0.arm7.cycles += cycles;
    }
}

/// Runs the system call `cpu` just entered and returns from it.
fn system_call<C: Processor, B: CpuBus>(cpu: &mut C, bus: &mut B, side: Side, intr_flags: u32) {
    let thumb = cpu.saved_psr().is_some_and(|spsr| spsr & psr::T != 0);
    let number = bios::number(bus, cpu.reg(14), thumb);
    let call = Call {
        side,
        args: [cpu.reg(0), cpu.reg(1), cpu.reg(2), cpu.reg(3)],
        intr_flags,
    };
    let Effects {
        registers,
        halt,
        repeat,
        stall,
        soft_reset: _,
    } = bios::run(number, &call, bus);
    for (index, value) in registers {
        cpu.set_reg(index, value);
    }
    if repeat {
        // Return to the SWI itself, so the call runs again after waking.
        let length = if thumb { 2 } else { 4 };
        cpu.set_reg(14, cpu.reg(14).wrapping_sub(length));
    }
    if halt {
        bus.halt();
    }
    bus.stall(stall);
    cpu.return_from_exception(bus);
}
