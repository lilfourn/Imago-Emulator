//! The two CPUs and the hardware between them, run in lockstep.
//!
//! The machine advances in short slices: up to the next scheduled event,
//! and never more than [`SLICE_CYCLES`] bus cycles. In each slice the ARM9
//! runs until its clock reaches the slice's end, then the ARM7 does, then
//! the events due by then fire. A CPU therefore sees the other's writes at
//! most a slice late, which the games' IPC handshakes tolerate.

use emu_arm::{Bus, Cpu, Mode, Model, Processor, Registers, cp15, psr};
use serde::{Deserialize, Serialize};

use crate::bios::{self, Call, Decompression, Effects, Next};
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
    /// Emulated BIOS calls waiting for a game function they called to
    /// return, per CPU (ARM9, ARM7), the innermost last.
    suspended: [Vec<Suspended>; 2],
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
            suspended: [Vec::new(), Vec::new()],
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
            return;
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
            trap.finish(
                &mut self.arm7,
                &mut Arm7Bus(hw),
                effects,
                &mut self.suspended[1],
            );
            return;
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
            return;
        }
        self.arm7.set_irq_line(hw.arm7.interrupts.line());
        self.arm7.step(&mut Arm7Bus(hw));
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
    fn read<M: Model>(cpu: &Processor<M>, side: Side, intr_flags: u32) -> Self {
        Self {
            call: Call {
                side,
                args: [cpu.reg(0), cpu.reg(1), cpu.reg(2), cpu.reg(3)],
                intr_flags,
            },
            return_address: cpu.reg(14),
            caller_psr: cpu.saved_psr().unwrap_or_default(),
        }
    }

    fn thumb(&self) -> bool {
        self.caller_psr & psr::T != 0
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
    use emu_core::{Core, Media};

    use super::*;
    use crate::Nds;

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
            nds.machine.run_slice(|_, _| {});
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
}
