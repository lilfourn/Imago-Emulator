//! The two CPUs and the hardware between them, run in lockstep.
//!
//! The machine advances in short slices: up to the next scheduled event,
//! and never more than [`SLICE_CYCLES`] bus cycles. In each slice the ARM9
//! runs until its clock reaches the slice's end, then the ARM7 does, then
//! the events due by then fire. A CPU therefore sees the other's writes at
//! most a slice late, which the games' IPC handshakes tolerate.

use emu_arm::{Cpu, Mode, Model, Processor, Registers, cp15, psr};
use serde::{Deserialize, Serialize};

use crate::bios::{self, Call, Effects};
use crate::boot::{self, ARM7_STACKS, ARM9_STACKS, Program};
use crate::bus::{Arm7Bus, Arm9Bus, Arm9TcmBus, CpuBus};
use crate::cart::Cart;
use crate::display::Moment;
use crate::dma::Timing;
use crate::hardware::{Event, Hardware};
use crate::interrupt::irq;
use crate::ipc::Side;

/// The longest slice, in bus cycles.
const SLICE_CYCLES: u64 = 64;

/// The ARM9's processor, an ARM946E-S.
pub(crate) type Arm9 = emu_arm::Arm9;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Machine {
    pub(crate) arm9: Arm9,
    pub(crate) arm7: Cpu,
    pub(crate) hw: Hardware,
    /// System calls are emulated in Rust rather than run from BIOS dumps.
    pub(crate) hle_bios: bool,
    /// How many times each CPU (ARM9, ARM7) called each BIOS function, for
    /// debugging.
    #[serde(skip)]
    pub(crate) system_calls: [[u32; 0x20]; 2],
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
        // CP15 as the BIOS leaves it: high vectors, DTCM and ITCM on.
        let cp15 = arm9.cp15_mut();
        cp15.set_control(
            cp15::control::RESET | cp15::control::DTCM_ENABLE | cp15::control::ITCM_ENABLE,
        );
        cp15.set_dtcm_region(boot::DTCM_REGION);
        cp15.set_itcm_region(boot::ITCM_REGION);
        arm9.jump(&mut Arm9Bus(&mut hw), header.arm9.entry);
        arm7.jump(&mut Arm7Bus(&mut hw), header.arm7.entry);
        Self {
            arm9,
            arm7,
            hw,
            hle_bios,
            system_calls: [[0; 0x20]; 2],
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
        // The geometry FIFO's interrupt is a level, and its DMA runs while
        // it is less than half full.
        if self.hw.gpu3d.irq_requested() {
            self.hw.arm9.interrupts.request(irq::GEOMETRY_FIFO);
        }
        if self.hw.gpu3d.fifo_wants_dma() {
            self.hw.arm9.dma.trigger(Timing::GeometryFifo);
        }
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
        // Halted by a BIOS call or by CP15's wait for interrupt.
        if hw.arm9.halted || self.arm9.halted() {
            if !hw.arm9.interrupts.pending() {
                hw.arm9.cycles = hw.arm9.cycles.max(target);
                return;
            }
            hw.arm9.halted = false;
            self.arm9.wake();
        }
        let entered_swi = self.arm9.reg(15) == bios::ARM9_SWI_ENTERED_PC
            && !self.arm9.thumb()
            && self.arm9.mode() == Mode::Supervisor;
        if self.hle_bios && entered_swi {
            // The BIOS keeps its interrupt flags at the top of DTCM.
            let dtcm = self.arm9.cp15().dtcm_region() & !0xFFF;
            let trap = Trap::read(&self.arm9, Side::Arm9, dtcm + 0x3FF8);
            let effects = {
                let mut bus = Arm9TcmBus {
                    cpu: &mut self.arm9,
                    bus: Arm9Bus(hw),
                };
                trap.run(&mut bus)
            };
            self.system_calls[0][usize::from(effects.0 & 0x1F)] += 1;
            let effects = effects.1;
            trap.finish(&mut self.arm9, &mut Arm9Bus(hw), effects);
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
            let trap = Trap::read(&self.arm7, Side::Arm7, 0x0380_FFF8);
            let (number, effects) = trap.run(&mut Arm7Bus(hw));
            self.system_calls[1][usize::from(number & 0x1F)] += 1;
            trap.finish(&mut self.arm7, &mut Arm7Bus(hw), effects);
            return;
        }
        self.arm7.set_irq_line(hw.arm7.interrupts.line());
        self.arm7.step(&mut Arm7Bus(hw));
    }
}

/// A system call a CPU just trapped into, with its arguments.
struct Trap {
    call: Call,
    return_address: u32,
    thumb: bool,
}

impl Trap {
    fn read<M: Model>(cpu: &Processor<M>, side: Side, intr_flags: u32) -> Self {
        Self {
            call: Call {
                side,
                args: [cpu.reg(0), cpu.reg(1), cpu.reg(2), cpu.reg(3)],
                intr_flags,
            },
            return_address: cpu.reg(14),
            thumb: cpu.saved_psr().is_some_and(|spsr| spsr & psr::T != 0),
        }
    }

    /// Runs the call against memory as the calling CPU sees it. Returns
    /// the function number and what it did.
    fn run<B: CpuBus>(&self, bus: &mut B) -> (u8, Effects) {
        let number = bios::number(bus, self.return_address, self.thumb);
        (number, bios::run(number, &self.call, bus))
    }

    /// Applies the call's effects to the CPU and returns from the SWI.
    fn finish<M: Model, B: CpuBus>(&self, cpu: &mut Processor<M>, bus: &mut B, effects: Effects) {
        let Effects {
            registers,
            halt,
            repeat,
            stall,
            soft_reset: _,
        } = effects;
        for (index, value) in registers {
            cpu.set_reg(index, value);
        }
        if repeat {
            // Return to the SWI itself, so the call runs again after waking.
            let length = if self.thumb { 2 } else { 4 };
            cpu.set_reg(14, cpu.reg(14).wrapping_sub(length));
        }
        if halt {
            bus.halt();
        }
        bus.stall(stall);
        cpu.return_from_exception(bus);
    }
}
