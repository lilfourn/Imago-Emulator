//! The two CPUs and the hardware between them, run in lockstep.
//!
//! The machine advances in short slices: up to the next scheduled event,
//! and never more than [`SLICE_CYCLES`] bus cycles. In each slice the ARM9
//! runs until its clock reaches the slice's end, then the ARM7 does, then
//! the events due by then fire. A CPU therefore sees the other's writes at
//! most a slice late, which the games' IPC handshakes tolerate. While both
//! CPUs wait for an interrupt, one slice runs to the next event: the short
//! ones would do nothing different.

use emu_arm::{Bus, Cpu, Mode, Model, Processor, Registers, cp15, psr};
use serde::{Deserialize, Serialize};

use crate::bios::{self, Call, Decompression, Effects, Next};
use crate::boot::{self, ARM7_STACKS, ARM9_STACKS, Program};
use crate::bus::{Arm7Bus, Arm9Bus, Arm9TcmBus, CpuBus};
use crate::cart::Cart;
use crate::display::Moment;
use crate::dma::Timing;
use crate::hardware::{CpuIo, Event, Hardware};
use crate::interrupt::irq;
use crate::ipc::Side;

/// The longest slice, in bus cycles.
const SLICE_CYCLES: u64 = 64;

/// Where the ARM9's BIOS starts, with the addresses its emulated system
/// calls trap at.
const ARM9_BIOS: u32 = 0xFFFF_0000;

/// Where the ARM7's BIOS, with its trap addresses, ends.
const ARM7_BIOS_END: u32 = 0x4000;

/// The ARM9's processor, an ARM946E-S.
pub(crate) type Arm9 = emu_arm::Arm9;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Machine {
    pub(crate) arm9: Arm9,
    pub(crate) arm7: Cpu,
    pub(crate) hw: Hardware,
    /// System calls are emulated in Rust rather than run from BIOS dumps.
    pub(crate) hle_bios: bool,
    /// Emulated BIOS calls waiting for a game function they called to
    /// return, per CPU (ARM9, ARM7), the innermost last.
    suspended: [Vec<Suspended>; 2],
    /// Per CPU (ARM9, ARM7), the address of the SWI whose VBlankIntrWait
    /// call halted it. The CPU runs that SWI again on waking.
    vblank_wait_swi: [Option<u32>; 2],
    /// How many times each CPU (ARM9, ARM7) called each BIOS function, for
    /// debugging.
    #[serde(skip)]
    pub(crate) system_calls: [[u32; 0x20]; 2],
    /// The end of a slice a debugger stopped part-way through, so the next
    /// run finishes the same slice. Not machine state: a loaded state
    /// starts a fresh slice.
    #[serde(skip)]
    slice_target: Option<u64>,
}

/// How [`Machine::run_slice_with`] runs each CPU instruction, and whether
/// it stops after one: [`Plain`] for running games, or a debugger's
/// checks (`debug.rs`).
pub(crate) trait Stepper {
    /// Runs one ARM9 instruction on `bus`.
    fn arm9(&mut self, cpu: &mut Arm9, bus: &mut Arm9Bus<'_>);
    /// Runs one ARM7 instruction on `bus`.
    fn arm7(&mut self, cpu: &mut Cpu, bus: &mut Arm7Bus<'_>);
    /// Whether to stop after a step of `side`'s, whose next instruction is
    /// at `next`. `ran` says whether the step ran anything (an instruction
    /// or an emulated BIOS call) rather than DMA or waiting.
    fn stop(&mut self, side: Side, ran: bool, next: u32) -> bool;
}

/// Runs instructions straight on the bus and never stops.
pub(crate) struct Plain;

impl Stepper for Plain {
    #[inline]
    fn arm9(&mut self, cpu: &mut Arm9, bus: &mut Arm9Bus<'_>) {
        cpu.step(bus);
    }

    #[inline]
    fn arm7(&mut self, cpu: &mut Cpu, bus: &mut Arm7Bus<'_>) {
        cpu.step(bus);
    }

    #[inline]
    fn stop(&mut self, _: Side, _: bool, _: u32) -> bool {
        false
    }
}

/// What happened while running, for the core to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Progress {
    /// VBlank started: a frame is complete.
    pub(crate) frame_done: bool,
    /// The [`Stepper`] stopped the slice part-way through.
    pub(crate) stopped: bool,
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
            suspended: [Vec::new(), Vec::new()],
            vblank_wait_swi: [None; 2],
            system_calls: [[0; 0x20]; 2],
            slice_target: None,
        }
    }

    /// Bus cycles since power-on, as the scheduler counts them.
    pub(crate) fn now(&self) -> u64 {
        self.hw.scheduler.now()
    }

    /// Runs one slice, each instruction by `stepper` ([`Plain`] to just
    /// run them), which can stop it part-way; the next call then finishes
    /// the same slice. The caller stops calling once the clock reaches
    /// `limit` (see [`Machine::slice_end`]). Returns what the events in it
    /// did.
    pub(crate) fn run_slice_with<S: Stepper>(&mut self, stepper: &mut S, limit: u64) -> Progress {
        let now = self.hw.scheduler.now();
        let target = self
            .slice_target
            .take()
            .unwrap_or_else(|| self.slice_end(now, limit));
        let stopped = Progress {
            frame_done: false,
            stopped: true,
        };
        // The other CPU and the events may have changed what each CPU's
        // first instruction of the slice must check.
        self.hw.arm9.checked = false;
        self.hw.arm7.checked = false;
        while self.hw.arm9.cycles < target * 2 {
            let ran = self.step_arm9(target * 2, stepper);
            if stepper.stop(Side::Arm9, ran, self.arm9.next_instruction()) {
                self.slice_target = Some(target);
                return stopped;
            }
        }
        while self.hw.arm7.cycles < target {
            let ran = self.step_arm7(target, stepper);
            if stepper.stop(Side::Arm7, ran, self.arm7.next_instruction()) {
                self.slice_target = Some(target);
                return stopped;
            }
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
                    self.hw.hblank(due.time);
                }
                Event::LineStart => {
                    let moment = self.hw.line_start(due.time);
                    progress.frame_done |=
                        moment == Moment::LineStart(crate::display::VISIBLE_LINES);
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
                Event::Wifi => self.hw.run_wifi(due.time),
            }
        }
        progress
    }

    /// Where a fresh slice from `now` ends: at the next event, but at most
    /// [`SLICE_CYCLES`] on.
    ///
    /// While both CPUs are halted and nothing can wake them before the
    /// next event, the slices up to it would all just let the time pass,
    /// so one slice runs there at once. It still ends where a run of full
    /// slices would first reach `limit`, where the caller stops.
    fn slice_end(&self, now: u64, limit: u64) -> u64 {
        let next = self.hw.scheduler.next_time().unwrap_or(u64::MAX);
        if next <= now + SLICE_CYCLES || !self.idle() {
            return next.clamp(now + 1, now + SLICE_CYCLES);
        }
        let slices = limit.saturating_sub(now).div_ceil(SLICE_CYCLES).max(1);
        next.min(now.saturating_add(slices.saturating_mul(SLICE_CYCLES)))
    }

    /// Whether a slice would only let time pass: both CPUs are halted with
    /// no interrupt to wake them and no DMA to run, and the geometry FIFO
    /// checks that end every slice would not change that.
    fn idle(&self) -> bool {
        let Hardware {
            arm9, arm7, gpu3d, ..
        } = &self.hw;
        let quiet = |cpu: &CpuIo| !cpu.interrupts.pending() && cpu.dma.next_pending().is_none();
        let fifo_wakes =
            || gpu3d.irq_requested() && arm9.interrupts.enable & irq::GEOMETRY_FIFO != 0;
        let fifo_dma = || gpu3d.fifo_wants_dma() && arm9.dma.waiting_for(Timing::GeometryFifo);
        (arm9.halted || self.arm9.halted())
            && arm7.halted
            && quiet(arm9)
            && quiet(arm7)
            && !fifo_wakes()
            && !fifo_dma()
    }

    fn timer_overflow(&mut self, side: Side, index: usize, time: u64) {
        let timers = &mut self.hw.cpu_mut(side).timers;
        let overflowed = timers.overflow(index, time);
        let requested = timers.requesting(overflowed);
        self.hw.request_timer_interrupts(side, requested);
        self.hw.schedule_timer(side, index);
    }

    /// One ARM9 instruction (run by `stepper`), emulated BIOS call, DMA
    /// run or stretch of halt, up to `target` in ARM9 cycles. Returns
    /// whether it was an instruction or a BIOS call.
    fn step_arm9<S: Stepper>(&mut self, target: u64, stepper: &mut S) -> bool {
        let hw = &mut self.hw;
        // While `checked` holds, the checks below would find what they did
        // before the last instruction: no DMA to run and the same IRQ line.
        // CP15 can halt the CPU without touching the bus, so that is
        // checked here, and the BIOS area, where the emulated calls trap,
        // gets every check.
        if hw.arm9.checked && !self.arm9.halted() && self.arm9.reg(15) < ARM9_BIOS {
            stepper.arm9(&mut self.arm9, &mut Arm9Bus(hw));
            return true;
        }
        hw.arm9.checked = false;
        if hw.run_dma(Side::Arm9) {
            return false;
        }
        // Halted by a BIOS call or by CP15's wait for interrupt.
        if hw.arm9.halted || self.arm9.halted() {
            if !hw.arm9.interrupts.pending() {
                hw.arm9.cycles = hw.arm9.cycles.max(target);
                return false;
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
            let trap = Trap::read(
                &self.arm9,
                Side::Arm9,
                dtcm + 0x3FF8,
                self.vblank_wait_swi[0],
            );
            let effects = {
                let mut bus = Arm9TcmBus {
                    cpu: &mut self.arm9,
                    bus: Arm9Bus(hw),
                };
                trap.run(&mut bus)
            };
            self.system_calls[0][usize::from(effects.0 & 0x1F)] += 1;
            self.vblank_wait_swi[0] =
                trap.vblank_wait_after(effects.0, &effects.1, self.vblank_wait_swi[0]);
            let effects = effects.1;
            if effects.restart.is_some() {
                // The ARM9's SoftReset also resets CP15's control register.
                self.arm9.cp15_mut().set_control(bios::ARM9_RESET_CONTROL);
            }
            trap.finish(
                &mut self.arm9,
                &mut Arm9Bus(hw),
                effects,
                &mut self.suspended[0],
            );
            return true;
        }
        if self.hle_bios
            && reached(&self.arm9, bios::ARM9_CALLBACK_RETURN)
            && let Some(mut suspended) = self.suspended[0].pop()
        {
            let value = self.arm9.reg(0);
            let next = suspended.decompression.resume(
                &mut Arm9TcmBus {
                    cpu: &mut self.arm9,
                    bus: Arm9Bus(hw),
                },
                value,
            );
            let stub = bios::ARM9_CALLBACK_RETURN;
            suspended.proceed(
                &mut self.arm9,
                &mut Arm9Bus(hw),
                next,
                stub,
                &mut self.suspended[0],
            );
            return true;
        }
        self.arm9.set_irq_line(hw.arm9.interrupts.line());
        hw.arm9.checked = true;
        stepper.arm9(&mut self.arm9, &mut Arm9Bus(hw));
        true
    }

    /// One ARM7 instruction (run by `stepper`), emulated BIOS call, DMA
    /// run or stretch of halt, up to `target` in bus cycles. Returns
    /// whether it was an instruction or a BIOS call.
    fn step_arm7<S: Stepper>(&mut self, target: u64, stepper: &mut S) -> bool {
        let hw = &mut self.hw;
        // As on the ARM9.
        if hw.arm7.checked && self.arm7.reg(15) >= ARM7_BIOS_END {
            stepper.arm7(&mut self.arm7, &mut Arm7Bus(hw));
            return true;
        }
        hw.arm7.checked = false;
        if hw.run_dma(Side::Arm7) {
            return false;
        }
        if hw.arm7.halted {
            if !hw.arm7.interrupts.pending() {
                hw.arm7.cycles = hw.arm7.cycles.max(target);
                return false;
            }
            hw.arm7.halted = false;
        }
        let entered_swi = self.arm7.reg(15) == bios::ARM7_SWI_ENTERED_PC
            && !self.arm7.thumb()
            && self.arm7.mode() == Mode::Supervisor;
        if self.hle_bios && entered_swi {
            let trap = Trap::read(&self.arm7, Side::Arm7, 0x0380_FFF8, self.vblank_wait_swi[1]);
            let (number, effects) = trap.run(&mut Arm7Bus(hw));
            self.system_calls[1][usize::from(number & 0x1F)] += 1;
            self.vblank_wait_swi[1] =
                trap.vblank_wait_after(number, &effects, self.vblank_wait_swi[1]);
            trap.finish(
                &mut self.arm7,
                &mut Arm7Bus(hw),
                effects,
                &mut self.suspended[1],
            );
            return true;
        }
        if self.hle_bios
            && reached(&self.arm7, bios::ARM7_CALLBACK_RETURN)
            && let Some(mut suspended) = self.suspended[1].pop()
        {
            let next = suspended
                .decompression
                .resume(&mut Arm7Bus(hw), self.arm7.reg(0));
            let stub = bios::ARM7_CALLBACK_RETURN;
            suspended.proceed(
                &mut self.arm7,
                &mut Arm7Bus(hw),
                next,
                stub,
                &mut self.suspended[1],
            );
            return true;
        }
        self.arm7.set_irq_line(hw.arm7.interrupts.line());
        hw.arm7.checked = true;
        stepper.arm7(&mut self.arm7, &mut Arm7Bus(hw));
        true
    }
}

/// Whether the CPU's next instruction is at `address`, in either state. A
/// Thumb callback on the ARM7 may return with POP {PC}, which stays in
/// Thumb state on ARMv4.
fn reached<M: Model>(cpu: &Processor<M>, address: u32) -> bool {
    let pipeline = if cpu.thumb() { 4 } else { 8 };
    cpu.reg(15).wrapping_sub(pipeline) == address
}

/// A system call a CPU just trapped into, with its arguments.
struct Trap {
    call: Call,
    return_address: u32,
    /// The caller's CPSR, from SPSR_svc.
    caller_psr: u32,
}

impl Trap {
    /// The call the CPU just trapped into; `vblank_wait_swi` is the SWI
    /// whose VBlankIntrWait call halted the CPU, if one did.
    fn read<M: Model>(
        cpu: &Processor<M>,
        side: Side,
        intr_flags: u32,
        vblank_wait_swi: Option<u32>,
    ) -> Self {
        let mut trap = Self {
            call: Call {
                side,
                args: [cpu.reg(0), cpu.reg(1), cpu.reg(2), cpu.reg(3)],
                intr_flags,
                resumes: false,
            },
            return_address: cpu.reg(14),
            caller_psr: cpu.saved_psr().unwrap_or_default(),
        };
        trap.call.resumes = vblank_wait_swi == Some(trap.swi());
        trap
    }

    fn thumb(&self) -> bool {
        self.caller_psr & psr::T != 0
    }

    /// The address of the SWI instruction.
    fn swi(&self) -> u32 {
        let length = if self.thumb() { 2 } else { 4 };
        self.return_address.wrapping_sub(length)
    }

    /// The SWI whose VBlankIntrWait call halts the CPU once system call
    /// `number` has had `effects`, given `halted`, the one before: this SWI
    /// if its VBlankIntrWait halts, none once the halted call returns or
    /// SoftReset abandons it, and otherwise `halted`.
    fn vblank_wait_after(&self, number: u8, effects: &Effects, halted: Option<u32>) -> Option<u32> {
        match number {
            0x00 => None,
            0x05 if effects.repeat => Some(self.swi()),
            0x05 if self.call.resumes => None,
            _ => halted,
        }
    }

    /// Runs the call against memory as the calling CPU sees it. Returns
    /// the function number and what it did.
    fn run<B: CpuBus>(&self, bus: &mut B) -> (u8, Effects) {
        let number = bios::number(bus, self.return_address, self.thumb());
        (number, bios::run(number, &self.call, bus))
    }

    /// Applies the call's effects to the CPU and returns from the SWI. A
    /// SoftReset restarts the CPU instead, and a callback decompression
    /// calls its first game function and waits on `suspended`.
    fn finish<M: Model, B: CpuBus>(
        &self,
        cpu: &mut Processor<M>,
        bus: &mut B,
        effects: Effects,
        suspended: &mut Vec<Suspended>,
    ) {
        let Effects {
            registers,
            halt,
            repeat,
            stall,
            restart,
            callback,
        } = effects;
        for (index, value) in registers {
            cpu.set_reg(index, value);
        }
        let (stub, stacks) = match self.call.side {
            Side::Arm9 => (bios::ARM9_CALLBACK_RETURN, ARM9_STACKS),
            Side::Arm7 => (bios::ARM7_CALLBACK_RETURN, ARM7_STACKS),
        };
        if let Some(entry) = restart {
            suspended.clear();
            soft_reset(cpu, bus, entry, stacks);
            return;
        }
        if let Some((decompression, open)) = callback {
            let call = Suspended {
                decompression,
                return_address: self.return_address,
                caller_psr: self.caller_psr,
                system_lr: cpu.registers().usr[6],
                kept: [cpu.reg(2), cpu.reg(11), cpu.reg(12)],
            };
            call.proceed(cpu, bus, Next::Call(open), stub, suspended);
            return;
        }
        if repeat {
            // Return to the SWI itself, so the call runs again after waking.
            let length = if self.thumb() { 2 } else { 4 };
            cpu.set_reg(14, cpu.reg(14).wrapping_sub(length));
        }
        if halt {
            bus.halt();
        }
        bus.stall(stall);
        cpu.return_from_exception(bus);
    }
}

/// An emulated BIOS call waiting for a game function it called (a
/// decompression callback) to return to the callback stub
/// ([`bios::ARM9_CALLBACK_RETURN`] or [`bios::ARM7_CALLBACK_RETURN`]).
/// The function runs on the CPU like any other code, in the machine's
/// slices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Suspended {
    decompression: Decompression,
    /// Where the SWI returns to, and the caller's CPSR it restores.
    return_address: u32,
    caller_psr: u32,
    /// System mode's R14, which the callbacks' return address replaces.
    /// The real BIOS keeps it on the stack meanwhile.
    system_lr: u32,
    /// R2, R11 and R12, which the callbacks may use: SWIs return R2 and
    /// R4–R14 unchanged (GBATEK, "BIOS Functions").
    kept: [u32; 3],
}

impl Suspended {
    /// Calls the next game function the decompression needs, waiting on
    /// `suspended` again, or returns from the SWI once it is done.
    fn proceed<M: Model, B: Bus>(
        self,
        cpu: &mut Processor<M>,
        bus: &mut B,
        next: Next,
        stub: u32,
        suspended: &mut Vec<Self>,
    ) {
        match next {
            Next::Call(call) => {
                // The BIOS runs its functions, and so their callbacks, in
                // System mode, with IRQs enabled if the caller had them on.
                let thumb = if call.function & 1 != 0 { psr::T } else { 0 };
                cpu.set_cpsr(Mode::System.bits() | (self.caller_psr & psr::I) | thumb);
                for (index, value) in call.args.into_iter().enumerate() {
                    cpu.set_reg(index, value);
                }
                cpu.set_reg(14, stub);
                cpu.jump(bus, call.function & !1);
                suspended.push(self);
            }
            Next::Return(result) => {
                cpu.set_cpsr(Mode::System.bits());
                cpu.set_reg(14, self.system_lr);
                for (index, value) in [2, 11, 12].into_iter().zip(self.kept) {
                    cpu.set_reg(index, value);
                }
                cpu.set_cpsr(self.caller_psr);
                cpu.set_reg(0, result);
                cpu.jump(bus, self.return_address);
            }
        }
    }
}

/// SoftReset's registers (GBATEK, "BIOS Reset Functions"): the BIOS's
/// stacks for Supervisor, IRQ and System mode, with those two modes' R14
/// and saved PSRs zeroed; then System mode with R0–R12 zeroed, R14 =
/// `entry`, and a jump there (to Thumb code if bit 0 is set). GBATEK does
/// not say whether IRQs end up enabled; as at direct boot, they are. The
/// reset clears the IRQ handler's address, so callers turn IME off first.
fn soft_reset<M: Model, B: Bus>(
    cpu: &mut Processor<M>,
    bus: &mut B,
    entry: u32,
    stacks: (u32, u32, u32),
) {
    let (supervisor, irq_stack, system) = stacks;
    for (mode, stack) in [(Mode::Supervisor, supervisor), (Mode::Irq, irq_stack)] {
        cpu.set_cpsr(mode.bits() | psr::I | psr::F);
        cpu.set_reg(13, stack);
        cpu.set_reg(14, 0);
        cpu.set_saved_psr(0);
    }
    let thumb = if entry & 1 != 0 { psr::T } else { 0 };
    cpu.set_cpsr(Mode::System.bits() | thumb);
    for index in 0..13 {
        cpu.set_reg(index, 0);
    }
    cpu.set_reg(13, system);
    cpu.set_reg(14, entry);
    cpu.jump(bus, entry & !1);
}

#[cfg(test)]
mod tests {
    use emu_core::{Button, Core, Input, Media, StereoSample};

    use super::*;
    use crate::Nds;
    use crate::display::FRAME_CYCLES;

    /// Where the ARM9 program loads and starts.
    const ARM9_BASE: u32 = 0x0200_4000;

    /// A cartridge whose ARM9 runs `program` (ARM words) from 0x02004000
    /// and whose ARM7 waits in a loop.
    fn rom(program: &[u32]) -> Vec<u8> {
        let mut rom = vec![0; 0x410];
        let mut put = |offset: usize, value: u32| {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        for (offset, value) in [(0x20, 0x200), (0x24, ARM9_BASE), (0x28, ARM9_BASE)] {
            put(offset, value);
        }
        put(0x2C, 0x200);
        for (offset, value) in [(0x30, 0x400), (0x34, 0x0238_0000), (0x38, 0x0238_0000)] {
            put(offset, value);
        }
        put(0x3C, 4);
        for (index, word) in program.iter().enumerate() {
            put(0x200 + index * 4, *word);
        }
        put(0x400, 0xEAFF_FFFE); // B .
        rom
    }

    fn word(nds: &Nds, address: u32) -> u32 {
        u32::from_le_bytes([0, 1, 2, 3].map(|byte| nds.peek(0, address + byte).unwrap_or(0)))
    }

    #[test]
    fn callback_decompression_runs_the_games_functions() {
        let mut program = vec![0; 0x48];
        program[..15].copy_from_slice(&[
            0xE59F_0028, // LDR R0, =source
            0xE59F_1028, // LDR R1, =destination
            0xE3A0_2055, // MOV R2, #0x55
            0xE59F_3024, // LDR R3, =callbacks
            0xE3A0_E0AB, // MOV LR, #0xAB
            0xE3A0_C0CD, // MOV R12, #0xCD
            0xEF12_0000, // SWI 12h (LZ77UnCompReadByCallbackWrite16bit)
            0xE3A0_4402, // MOV R4, #0x02000000
            0xE884_5005, // STMIA R4, {R0, R2, R12, LR}
            0xEAFF_FFFE, // B .
            0,
            0,
            ARM9_BASE + 0x100,
            0x0210_0000,
            ARM9_BASE + 0xC0,
        ]);
        // Open (ARM): LDR R0, [R0]; BX LR.
        program[0x20..0x22].copy_from_slice(&[0xE590_0000, 0xE12F_FF1E]);
        // Get_8bit (Thumb): LDRB R0, [R0]; BX LR.
        program[0x22] = 0x4770_7800;
        // Close (ARM), using the scratch registers: MOV R2, #0; MOV R12,
        // #0; MOV R0, #0; BX LR.
        program[0x23..0x27].copy_from_slice(&[0xE3A0_2000, 0xE3A0_C000, 0xE3A0_0000, 0xE12F_FF1E]);
        // The callback structure: Open, Close, Get_8bit (Thumb), unused.
        program[0x30..0x33].copy_from_slice(&[
            ARM9_BASE + 0x80,
            ARM9_BASE + 0x8C,
            ARM9_BASE + 0x89,
        ]);
        // LZ77, 8 bytes: "A", "B", then 6 from distance 2.
        program[0x40..0x43].copy_from_slice(&[0x0000_0810, 0x3042_4120, 0x0000_0001]);

        let rom = rom(&program);
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        // Stop while a callback runs, and finish from a save state.
        for _ in 0..1000 {
            if !nds.machine.suspended[0].is_empty() {
                break;
            }
            nds.machine.run_slice_with(&mut Plain, u64::MAX);
        }
        assert!(!nds.machine.suspended[0].is_empty(), "stopped mid-call");
        let mut state = Vec::new();
        nds.save_state(&mut state);
        let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
        nds.load_state(&state).expect("loads");
        nds.run_frame(&[], &mut Vec::new());
        assert_eq!(word(&nds, 0x0210_0000), u32::from_le_bytes(*b"ABAB"));
        assert_eq!(word(&nds, 0x0210_0004), u32::from_le_bytes(*b"ABAB"));
        assert_eq!(word(&nds, 0x0200_0000), 8, "the SWI returns the length");
        let kept = [4, 8, 12].map(|offset| word(&nds, 0x0200_0000 + offset));
        assert_eq!(kept, [0x55, 0xCD, 0xAB], "R2, R12 and LR are kept");
        assert!(nds.machine.suspended[0].is_empty());
    }

    #[test]
    fn soft_reset_restarts_the_arm9_at_its_entry_point() {
        let program = [
            0xE3A0_0402, // MOV R0, #0x02000000
            0xE590_1000, // LDR R1, [R0]
            0xE281_1001, // ADD R1, R1, #1
            0xE580_1000, // STR R1, [R0]   (count the starts)
            0xE351_0001, // CMP R1, #1
            0x1AFF_FFFE, // BNE .          (stop after the second)
            0xE59F_200C, // LDR R2, =0x00803FFC
            0xE582_2000, // STR R2, [R2]   (the IRQ handler's address)
            0xE3A0_5055, // MOV R5, #0x55
            0xEF00_0000, // SWI 0 (SoftReset)
            0xEAFF_FFFE, // B .
            0x0080_3FFC,
        ];
        let mut nds = Nds::new(&Media::new(&rom(&program))).expect("valid header");
        nds.run_frame(&[], &mut Vec::new());
        assert_eq!(word(&nds, 0x0200_0000), 2, "started twice");
        let arm9 = &nds.machine.arm9;
        assert_eq!(&arm9.dtcm()[0x3FFC..0x4000], &[0; 4], "BIOS area cleared");
        assert_eq!(arm9.reg(15), ARM9_BASE + 0x14 + 8, "waiting at BNE");
        let registers = arm9.registers();
        assert_eq!(registers.cpsr & 0x3F, Mode::System.bits());
        assert_eq!(registers.r[5], 0);
        assert_eq!(registers.r[13], ARM9_STACKS.2);
        assert_eq!(registers.r[14], ARM9_BASE);
        assert_eq!(registers.svc, [ARM9_STACKS.0, 0]);
        assert_eq!(registers.irq, [ARM9_STACKS.1, 0]);
        assert_eq!(registers.spsr[1], 0, "SPSR_svc");
        assert_eq!(arm9.cp15().control(), bios::ARM9_RESET_CONTROL);
    }

    /// An ARM9 program that waits for VBlank with VBlankIntrWait in a
    /// loop, counting frames in R5, with an IRQ handler that acknowledges
    /// the IRQ and sets the BIOS flag at the top of DTCM, as libnds's does.
    fn vblank_wait_program() -> Vec<u32> {
        let mut program = vec![0; 0x4C];
        program[..17].copy_from_slice(&[
            0xEE19_3F11, // MRC P15, 0, R3, C9, C1, 0  (DTCM region)
            0xE1A0_3623, // MOV R3, R3, LSR #12
            0xE1A0_3603, // MOV R3, R3, LSL #12
            0xE283_3901, // ADD R3, R3, #0x4000
            0xE3A0_4402, // MOV R4, #0x02000000
            0xE284_4C41, // ADD R4, R4, #0x4100
            0xE503_4004, // STR R4, [R3, #-4]  (the IRQ handler)
            0xE3A0_0301, // MOV R0, #0x04000000
            0xE3A0_1008, // MOV R1, #8
            0xE1C0_10B4, // STRH R1, [R0, #4]  (DISPSTAT: VBlank IRQ)
            0xE280_2C02, // ADD R2, R0, #0x200
            0xE3A0_1001, // MOV R1, #1
            0xE582_1010, // STR R1, [R2, #0x10]  (IE: VBlank)
            0xE582_1008, // STR R1, [R2, #8]  (IME)
            0xEF05_0000, // loop: SWI 05h (VBlankIntrWait)
            0xE285_5001, // ADD R5, R5, #1
            0xEAFF_FFFC, // B loop
        ]);
        program[0x40..0x4C].copy_from_slice(&[
            0xE3A0_0301, // MOV R0, #0x04000000
            0xE280_0C02, // ADD R0, R0, #0x200
            0xE3A0_1001, // MOV R1, #1
            0xE580_1014, // STR R1, [R0, #0x14]  (IF)
            0xEE19_2F11, // MRC P15, 0, R2, C9, C1, 0
            0xE1A0_2622, // MOV R2, R2, LSR #12
            0xE1A0_2602, // MOV R2, R2, LSL #12
            0xE282_2901, // ADD R2, R2, #0x4000
            0xE512_3008, // LDR R3, [R2, #-8]
            0xE383_3001, // ORR R3, R3, #1
            0xE502_3008, // STR R3, [R2, #-8]
            0xE12F_FF1E, // BX LR
        ]);
        program
    }

    #[test]
    fn vblank_intr_wait_returns_once_per_frame() {
        let mut nds = Nds::new(&Media::new(&rom(&vblank_wait_program()))).expect("valid header");
        for _ in 0..10 {
            nds.run_frame(&[], &mut Vec::new());
        }
        let frames_waited = nds.machine.arm9.reg(5);
        assert!((9..=10).contains(&frames_waited), "{frames_waited}");
    }

    /// A cartridge whose CPUs take turns, each halted with IME off until
    /// it is woken. At VBlank the ARM9 wakes from CP15's wait for
    /// interrupt, counts a while, lets the VBlank IRQ in by turning IME on
    /// for a moment, and wakes the ARM7 with an IPCSYNC IRQ. The ARM7,
    /// halted through HALTCNT in its own WRAM, wakes for that and for its
    /// timer 1, counts a little and wakes the ARM9. Each CPU adds its timer 0 count at every waking to
    /// R8, and the IRQ handler adds the ARM9's to the word at 0x02000000,
    /// so those show when everything happened.
    fn ping_pong_rom() -> Vec<u8> {
        let mut arm9 = vec![0; 0x4B];
        arm9[..38].copy_from_slice(&[
            0xEE19_3F11, // MRC P15, 0, R3, C9, C1, 0  (DTCM region)
            0xE1A0_3623, // MOV R3, R3, LSR #12
            0xE1A0_3603, // MOV R3, R3, LSL #12
            0xE283_3901, // ADD R3, R3, #0x4000
            0xE3A0_4402, // MOV R4, #0x02000000
            0xE284_4C41, // ADD R4, R4, #0x4100
            0xE503_4004, // STR R4, [R3, #-4]  (the IRQ handler)
            0xE3A0_0301, // MOV R0, #0x04000000
            0xE280_2C02, // ADD R2, R0, #0x200
            0xE3A0_1801, // MOV R1, #0x10000
            0xE381_1001, // ORR R1, R1, #1
            0xE582_1010, // STR R1, [R2, #0x10]  (IE: IPCSYNC and VBlank)
            0xE3A0_1008, // MOV R1, #8
            0xE1C0_10B4, // STRH R1, [R0, #4]  (DISPSTAT: VBlank IRQ)
            0xE280_3E18, // ADD R3, R0, #0x180
            0xE3A0_1901, // MOV R1, #0x4000
            0xE1C3_10B0, // STRH R1, [R3]  (IPCSYNC: IRQ from the ARM7)
            0xE280_5C01, // ADD R5, R0, #0x100
            0xE3A0_1080, // MOV R1, #0x80
            0xE1C5_10B2, // STRH R1, [R5, #2]  (TM0CNT_H: start)
            0xEE07_0F90, // loop: MCR P15, 0, R0, C7, C0, 4  (wait for IRQ)
            0xE1D5_40B0, // LDRH R4, [R5]  (TM0CNT_L)
            0xE088_8004, // ADD R8, R8, R4
            0xE592_6014, // LDR R6, [R2, #0x14]  (IF)
            0xE316_0801, // TST R6, #0x10000
            0x13A0_1801, // MOVNE R1, #0x10000
            0x1582_1014, // STRNE R1, [R2, #0x14]  (the ARM7's: acknowledge)
            0x1AFF_FFF7, // BNE loop
            0xE3A0_7040, // MOV R7, #0x40
            0xE257_7001, // count: SUBS R7, R7, #1
            0x1AFF_FFFD, // BNE count
            0xE3A0_1001, // MOV R1, #1
            0xE582_1008, // STR R1, [R2, #8]  (IME on: VBlank's IRQ comes)
            0xE3A0_1000, // MOV R1, #0
            0xE582_1008, // STR R1, [R2, #8]  (IME off)
            0xE3A0_1A06, // MOV R1, #0x6000
            0xE1C3_10B0, // STRH R1, [R3]  (IPCSYNC: IRQ to the ARM7)
            0xEAFF_FFED, // B loop
        ]);
        arm9[0x40..].copy_from_slice(&[
            0xE3A0_0301, // MOV R0, #0x04000000
            0xE280_1C01, // ADD R1, R0, #0x100
            0xE1D1_10B0, // LDRH R1, [R1]  (TM0CNT_L)
            0xE3A0_2402, // MOV R2, #0x02000000
            0xE592_3000, // LDR R3, [R2]
            0xE083_3001, // ADD R3, R3, R1
            0xE582_3000, // STR R3, [R2]
            0xE280_0C02, // ADD R0, R0, #0x200
            0xE3A0_1001, // MOV R1, #1
            0xE580_1014, // STR R1, [R0, #0x14]  (IF: acknowledge VBlank)
            0xE12F_FF1E, // BX LR
        ]);
        let arm7 = [
            0xE3A0_0301, // MOV R0, #0x04000000
            0xE280_2C02, // ADD R2, R0, #0x200
            0xE3A0_1801, // MOV R1, #0x10000
            0xE381_1010, // ORR R1, R1, #0x10
            0xE582_1010, // STR R1, [R2, #0x10]  (IE: IPCSYNC and timer 1)
            0xE280_3E18, // ADD R3, R0, #0x180
            0xE3A0_1901, // MOV R1, #0x4000
            0xE1C3_10B0, // STRH R1, [R3]  (IPCSYNC: IRQ from the ARM9)
            0xE280_5C01, // ADD R5, R0, #0x100
            0xE3A0_1080, // MOV R1, #0x80
            0xE1C5_10B2, // STRH R1, [R5, #2]  (TM0CNT_H: start)
            0xE3A0_1CF0, // MOV R1, #0xF000
            0xE381_18C0, // ORR R1, R1, #0xC00000
            0xE585_1004, // STR R1, [R5, #4]  (timer 1: every 4096, IRQ)
            0xE3A0_1080, // loop: MOV R1, #0x80
            0xE5C0_1301, // STRB R1, [R0, #0x301]  (HALTCNT: halt)
            0xE1D5_40B0, // LDRH R4, [R5]  (TM0CNT_L)
            0xE088_8004, // ADD R8, R8, R4
            0xE592_6014, // LDR R6, [R2, #0x14]  (IF)
            0xE582_6014, // STR R6, [R2, #0x14]  (acknowledge)
            0xE3A0_7020, // MOV R7, #0x20
            0xE257_7001, // count: SUBS R7, R7, #1
            0x1AFF_FFFD, // BNE count
            0xE3A0_1A06, // MOV R1, #0x6000
            0xE1C3_10B0, // STRH R1, [R3]  (IPCSYNC: IRQ to the ARM9)
            0xEAFF_FFF3, // B loop
        ];
        let mut rom = rom(&arm9);
        rom.resize(0x400 + arm7.len() * 4, 0);
        let mut put = |offset: usize, value: u32| {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(0x34, 0x0380_0000);
        put(0x38, 0x0380_0000);
        put(0x3C, arm7.len() as u32 * 4);
        for (index, word) in arm7.into_iter().enumerate() {
            put(0x400 + index * 4, word);
        }
        rom
    }

    /// Stops after every step, so each instruction runs in a fresh call of
    /// [`Machine::run_slice_with`], which checks everything before it.
    struct CheckEverything;

    impl Stepper for CheckEverything {
        fn arm9(&mut self, cpu: &mut Arm9, bus: &mut Arm9Bus<'_>) {
            cpu.step(bus);
        }

        fn arm7(&mut self, cpu: &mut Cpu, bus: &mut Arm7Bus<'_>) {
            cpu.step(bus);
        }

        fn stop(&mut self, _: Side, _: bool, _: u32) -> bool {
            true
        }
    }

    /// Runs a frame as [`Core::run_frame`] does, but the plain way: every
    /// instruction gets every check, and no slice is longer than
    /// [`SLICE_CYCLES`], idle or not.
    fn run_frame_plainly(nds: &mut Nds, input: Input, audio: &mut Vec<StereoSample>) {
        nds.set_input(&input);
        let start = nds.machine.now();
        while nds.machine.now() - start < FRAME_CYCLES {
            // A limit just ahead keeps idle slices short too.
            let limit = nds.machine.now() + 1;
            if nds
                .machine
                .run_slice_with(&mut CheckEverything, limit)
                .frame_done
            {
                break;
            }
        }
        nds.machine.hw.wifi.take_transmissions();
        nds.machine.hw.sound.drain_samples(audio);
        nds.present();
    }

    /// Runs `rom` for `frames` frames with `input`, both as games run and
    /// [`run_frame_plainly`], and checks that the screens, the sound and
    /// (every 10 frames) the save states match.
    fn assert_runs_as_plainly(rom: &[u8], frames: usize, input: impl Fn(usize) -> Input) {
        let mut fast = Nds::new(&Media::new(rom)).expect("valid header");
        let mut plain = Nds::new(&Media::new(rom)).expect("valid header");
        for frame in 0..frames {
            let (mut fast_audio, mut plain_audio) = (Vec::new(), Vec::new());
            fast.run_frame(&[input(frame)], &mut fast_audio);
            run_frame_plainly(&mut plain, input(frame), &mut plain_audio);
            assert!(fast.screens() == plain.screens(), "frame {frame}: screens");
            assert!(fast_audio == plain_audio, "frame {frame}: sound");
            if frame % 10 == 9 {
                let (mut fast_state, mut plain_state) = (Vec::new(), Vec::new());
                fast.save_state(&mut fast_state);
                plain.save_state(&mut plain_state);
                assert!(fast_state == plain_state, "frame {frame}: state");
            }
        }
    }

    #[test]
    fn idle_slices_run_to_the_next_event() {
        let mut nds = Nds::new(&Media::new(&ping_pong_rom())).expect("valid header");
        for _ in 0..3 {
            nds.run_frame(&[], &mut Vec::new());
        }
        assert_ne!(word(&nds, 0x0200_0000), 0, "the ARM9 took VBlank's IRQ");
        assert_ne!(nds.machine.arm7.reg(8), 0, "the ARM9 woke the ARM7");
        assert!(nds.machine.arm9.reg(8) > 0x1_0000, "and the ARM7 the ARM9");
        let mut longest = 0;
        loop {
            let before = nds.machine.now();
            let progress = nds.machine.run_slice_with(&mut Plain, u64::MAX);
            longest = longest.max(nds.machine.now() - before);
            if progress.frame_done {
                break;
            }
        }
        assert!(longest > SLICE_CYCLES, "{longest}");
    }

    #[test]
    fn a_slice_is_idle_only_with_nothing_to_wake_or_run() {
        let rom = rom(&[0xEAFF_FFFE]);
        let idle_after = |change: &dyn Fn(&mut Hardware)| {
            let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
            nds.machine.hw.arm9.halted = true;
            nds.machine.hw.arm7.halted = true;
            change(&mut nds.machine.hw);
            nds.machine.idle()
        };
        assert!(idle_after(&|_| {}));
        assert!(!idle_after(&|hw| hw.arm9.halted = false));
        assert!(!idle_after(&|hw| hw.arm7.halted = false));
        // An interrupt to wake a CPU, whatever IME says.
        assert!(!idle_after(&|hw| {
            hw.arm9.interrupts.enable = irq::VBLANK;
            hw.arm9.interrupts.request(irq::VBLANK);
        }));
        assert!(idle_after(&|hw| hw.arm7.interrupts.request(irq::VBLANK)));
        // A DMA channel to run.
        assert!(!idle_after(&|hw| hw.arm7_write_32(0x0400_00B8, 0x8000_0001)));
        // The geometry FIFO, empty, would raise an enabled interrupt or
        // start its DMA at the end of the slice.
        let fifo_irq = |hw: &mut Hardware| hw.arm9_write_32(0x0400_0600, 2 << 30);
        assert!(idle_after(&fifo_irq));
        assert!(!idle_after(&|hw| {
            fifo_irq(hw);
            hw.arm9.interrupts.enable = irq::GEOMETRY_FIFO;
        }));
        assert!(!idle_after(&|hw| hw.arm9_write_32(0x0400_00B8, 0xB800_0001)));
    }

    #[test]
    fn halted_cpus_and_skipped_checks_change_nothing() {
        assert_runs_as_plainly(&ping_pong_rom(), 30, |_| Input::default());
        // The emulated BIOS's VBlankIntrWait, with the ARM7 busy.
        assert_runs_as_plainly(&rom(&vblank_wait_program()), 30, |_| Input::default());
    }

    /// rockwrestler (if fetched) opening its IPC menu and running its
    /// IPCFIFO IRQ test, where both CPUs exchange words and interrupts.
    #[test]
    fn rockwrestler_runs_as_plainly() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-roms/rockwrestler/rockwrestler.nds"
        );
        let Ok(rom) = std::fs::read(path) else {
            eprintln!("skipping: run scripts/fetch-test-roms.sh to download rockwrestler");
            return;
        };
        let presses = [
            Button::Down,
            Button::Down,
            Button::A,
            Button::Down,
            Button::Down,
            Button::A,
        ];
        // 60 frames to start, then each press held for 10 frames and
        // released for 10.
        let input = |frame: usize| {
            let step = frame.saturating_sub(60);
            match presses.get(step / 20) {
                Some(&button) if frame >= 60 && step % 20 < 10 => Input {
                    buttons: [button].into_iter().collect(),
                    ..Input::default()
                },
                _ => Input::default(),
            }
        };
        assert_runs_as_plainly(&rom, 300, input);
    }
}
