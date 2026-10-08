//! ARM interpreters shared by the GBA and DS cores.
//!
//! [`Processor`] implements the ARM and Thumb instruction sets, the barrel
//! shifter, banked registers for each CPU mode, exceptions, and the
//! two-stage prefetch that makes the PC read 8 (ARM) or 4 (Thumb) bytes
//! ahead. A [`Model`] type parameter specialises it at compile time:
//!
//! - [`Cpu`] is the ARM7TDMI (ARMv4T) of the GBA and the DS's ARM7.
//! - [`Arm9`] is the DS's ARM946E-S (ARMv5TE): the ARMv5TE instructions,
//!   the CP15 system control coprocessor with its protection unit, tightly
//!   coupled memory, and waiting for interrupts.
//!
//! References: ARM7TDMI and ARM946E-S Technical Reference Manuals, the ARM
//! Architecture Reference Manual (ARMv5TE), GBATEK "ARM CPU Reference", and
//! NanoBoyAdvance, whose bus timing the per-instruction tests in
//! `tests/single_step.rs` were recorded from.
//!
//! The CPU reaches memory through [`Bus`], which each system implements and
//! which also counts cycles: every access says whether it is sequential and
//! whether it is an instruction fetch, which is what GBA wait states depend
//! on. The ARM9 keeps the ARM7TDMI's access pattern (one fetch per
//! instruction, the same data accesses and internal cycles), which is not
//! the ARM946E-S's five-stage pipeline and caches but keeps time moving in
//! proportion; its new instructions spend the internal cycles GBATEK lists.

mod alu;
mod arm;
mod arm9;
mod armv5;
pub mod cp15;
mod describe;
mod disasm;
mod model;
mod multiplier;
mod protection;
mod tcm;
mod thumb;
mod watch;

use serde::{Deserialize, Serialize};

pub use arm9::{Arm9, Arm946es};
pub use cp15::Cp15;
pub use describe::describe_psr;
pub use disasm::{Architecture, Disassembly, disassemble_arm, disassemble_thumb};
pub use model::{Arm7tdmi, Model};
pub use watch::Watched;

/// How a memory access relates to the previous one, as a bit set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Access(u8);

impl Access {
    /// A random (non-sequential) access.
    pub const NONSEQUENTIAL: Self = Self(0);
    /// The address follows the previous access.
    pub const SEQUENTIAL: Self = Self(1);
    /// An instruction fetch.
    pub const CODE: Self = Self(2);
    /// A DMA access (set by systems, never by the CPU).
    pub const DMA: Self = Self(4);
    /// Part of a locked read-modify-write (SWP).
    pub const LOCK: Self = Self(8);

    /// The raw bits, matching NanoBoyAdvance's access mask.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The access with these raw bits.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Whether every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Access {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// What the CPU needs from the rest of the machine. Each call is one bus
/// cycle (plus any wait states the system adds).
///
/// Data accesses pass the address as the instruction computed it, which
/// may be misaligned; the bus ignores the low bits a word or halfword
/// access cannot use, as the hardware does. Instruction fetches are always
/// aligned.
pub trait Bus {
    /// Reads a word, ignoring address bits 0–1.
    fn read_32(&mut self, address: u32, access: Access) -> u32;
    /// Reads a halfword, ignoring address bit 0.
    fn read_16(&mut self, address: u32, access: Access) -> u16;
    /// Reads a byte.
    fn read_8(&mut self, address: u32, access: Access) -> u8;
    /// Writes a word, ignoring address bits 0–1.
    fn write_32(&mut self, address: u32, value: u32, access: Access);
    /// Writes a halfword, ignoring address bit 0.
    fn write_16(&mut self, address: u32, value: u16, access: Access);
    /// Writes a byte.
    fn write_8(&mut self, address: u32, value: u8, access: Access);
    /// Spends one internal cycle without a memory access.
    fn idle(&mut self);
    /// Notes a data access an ARM9 served from its tightly coupled memory,
    /// which never reaches the bus: `bytes` (1, 2 or 4) at `address`,
    /// whether it was a store, and the value. Only debuggers' watchpoints
    /// care (see [`Watched`]); by default it does nothing.
    fn tcm_data(&mut self, address: u32, bytes: u32, write: bool, value: u32) {
        let _ = (address, bytes, write, value);
    }
}

/// A processor mode, as encoded in CPSR bits 0–4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// Unprivileged.
    User,
    /// Fast interrupt.
    Fiq,
    /// Interrupt.
    Irq,
    /// Supervisor, entered by SWI and reset.
    Supervisor,
    /// Memory abort.
    Abort,
    /// Undefined instruction.
    Undefined,
    /// Privileged, sharing User's registers.
    System,
}

impl Mode {
    /// The mode CPSR bits 0–4 select. Encodings that name no mode behave
    /// as User.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        match bits & 0x1F {
            0x11 => Self::Fiq,
            0x12 => Self::Irq,
            0x13 => Self::Supervisor,
            0x17 => Self::Abort,
            0x1B => Self::Undefined,
            0x1F => Self::System,
            _ => Self::User,
        }
    }

    /// The CPSR encoding.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::User => 0x10,
            Self::Fiq => 0x11,
            Self::Irq => 0x12,
            Self::Supervisor => 0x13,
            Self::Abort => 0x17,
            Self::Undefined => 0x1B,
            Self::System => 0x1F,
        }
    }

    /// Which bank of R13–R14 this mode uses: User/System, FIQ, IRQ,
    /// Supervisor, Abort, Undefined.
    const fn bank(self) -> usize {
        match self {
            Self::User | Self::System => 0,
            Self::Fiq => 1,
            Self::Irq => 2,
            Self::Supervisor => 3,
            Self::Abort => 4,
            Self::Undefined => 5,
        }
    }

    /// Index into the saved PSRs; User and System have none.
    const fn spsr_index(self) -> Option<usize> {
        match self {
            Self::User | Self::System => None,
            Self::Fiq => Some(0),
            Self::Supervisor => Some(1),
            Self::Abort => Some(2),
            Self::Irq => Some(3),
            Self::Undefined => Some(4),
        }
    }
}

/// CPSR flag and control bits.
pub mod psr {
    /// Negative.
    pub const N: u32 = 1 << 31;
    /// Zero.
    pub const Z: u32 = 1 << 30;
    /// Carry.
    pub const C: u32 = 1 << 29;
    /// Overflow.
    pub const V: u32 = 1 << 28;
    /// IRQs disabled.
    pub const I: u32 = 1 << 7;
    /// FIQs disabled.
    pub const F: u32 = 1 << 6;
    /// Thumb state.
    pub const T: u32 = 1 << 5;
    /// Sticky saturation (ARMv5TE): set by QADD and friends and by
    /// overflowing SMLA multiplies, cleared only by writing the CPSR.
    pub const Q: u32 = 1 << 27;
}

/// Exception vectors, as offsets from the model's vector base.
mod vector {
    pub const UNDEFINED: u32 = 0x04;
    pub const SWI: u32 = 0x08;
    pub const PREFETCH_ABORT: u32 = 0x0C;
    pub const DATA_ABORT: u32 = 0x10;
    pub const IRQ: u32 = 0x18;
}

/// For each condition (0–15), whether it passes with each value of the
/// flags NZCV (CPSR bits 31–28): bit `nzcv` of entry `condition`.
const CONDITIONS: [u16; 16] = {
    let mut table = [0; 16];
    let mut condition = 0;
    while condition < 16 {
        let mut nzcv = 0;
        while nzcv < 16 {
            if passes(condition, nzcv) {
                table[condition as usize] |= 1 << nzcv;
            }
            nzcv += 1;
        }
        condition += 1;
    }
    table
};

/// Whether `condition` passes with flags `nzcv` (N in bit 3, V in bit 0).
const fn passes(condition: u32, nzcv: u32) -> bool {
    let (n, z, c, v) = (nzcv & 8 != 0, nzcv & 4 != 0, nzcv & 2 != 0, nzcv & 1 != 0);
    match condition {
        0x0 => z,
        0x1 => !z,
        0x2 => c,
        0x3 => !c,
        0x4 => n,
        0x5 => !n,
        0x6 => v,
        0x7 => !v,
        0x8 => c && !z,
        0x9 => !c || z,
        0xA => n == v,
        0xB => n != v,
        0xC => !z && n == v,
        0xD => z || n != v,
        0xE => true,
        _ => false,
    }
}

/// CPSR after reset: Supervisor mode, ARM state, IRQs and FIQs off.
const RESET_CPSR: u32 = Mode::Supervisor.bits() | psr::I | psr::F;

/// Every register: the current mode's view plus every bank.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Registers {
    /// R0–R15 as the current mode sees them.
    pub r: [u32; 16],
    /// R8–R14 of User and System mode.
    pub usr: [u32; 7],
    /// R8–R14 of FIQ mode.
    pub fiq: [u32; 7],
    /// R13–R14 of Supervisor mode.
    pub svc: [u32; 2],
    /// R13–R14 of Abort mode.
    pub abt: [u32; 2],
    /// R13–R14 of IRQ mode.
    pub irq: [u32; 2],
    /// R13–R14 of Undefined mode.
    pub und: [u32; 2],
    /// Current program status register.
    pub cpsr: u32,
    /// Saved PSRs: FIQ, Supervisor, Abort, IRQ, Undefined.
    pub spsr: [u32; 5],
}

/// An ARM7TDMI: the GBA's CPU and the DS's ARM7.
pub type Cpu = Processor<Arm7tdmi>;

/// An ARM processor of model `M`; use [`Cpu`] or [`Arm9`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Processor<M> {
    /// R0–R15 of the current mode.
    r: [u32; 16],
    cpsr: u32,
    /// R8–R12 for every mode but FIQ, and for FIQ.
    r8_12: [[u32; 5]; 2],
    /// R13–R14 per bank (see [`Mode::bank`]).
    r13_14: [[u32; 2]; 6],
    spsr: [u32; 5],
    /// The next two instructions: `pipeline[0]` executes next.
    pipeline: [u32; 2],
    /// Access type of the next instruction fetch.
    fetch_access: Access,
    /// The IRQ input line.
    irq_line: bool,
    /// The model's own state. The ARM7TDMI's is empty, so its saved state
    /// is unchanged by this field.
    model: M,
}

impl<M: Model> Default for Processor<M> {
    fn default() -> Self {
        Self {
            r: [0; 16],
            cpsr: RESET_CPSR,
            r8_12: [[0; 5]; 2],
            r13_14: [[0; 2]; 6],
            spsr: [0; 5],
            pipeline: [0; 2],
            fetch_access: Access::CODE,
            irq_line: false,
            model: M::default(),
        }
    }
}

impl<M: Model> Processor<M> {
    /// The current mode.
    #[must_use]
    pub fn mode(&self) -> Mode {
        Mode::from_bits(self.cpsr)
    }

    /// Whether the CPU is in Thumb state.
    #[must_use]
    pub fn thumb(&self) -> bool {
        self.cpsr & psr::T != 0
    }

    /// Register `index` (0–15) as the current mode sees it.
    #[must_use]
    pub fn reg(&self, index: usize) -> u32 {
        self.r[index]
    }

    /// The CPSR.
    #[must_use]
    pub fn cpsr(&self) -> u32 {
        self.cpsr
    }

    /// The prefetched instructions; `pipeline()[0]` executes next.
    #[must_use]
    pub fn pipeline(&self) -> [u32; 2] {
        self.pipeline
    }

    /// The access type of the next instruction fetch.
    #[must_use]
    pub fn fetch_access(&self) -> Access {
        self.fetch_access
    }

    /// The current mode's saved PSR, or `None` in User and System mode.
    #[must_use]
    pub fn saved_psr(&self) -> Option<u32> {
        self.spsr()
    }

    /// Sets the current mode's saved PSR; User and System mode have none.
    /// For high-level emulation of code that resets the modes.
    pub fn set_saved_psr(&mut self, value: u32) {
        self.set_spsr(value);
    }

    /// Returns from an exception as `MOVS PC, LR` does: CPSR from SPSR, R15
    /// from R14, and the pipeline refilled. For high-level emulation of
    /// code that would end that way (a BIOS call).
    pub fn return_from_exception<B: Bus>(&mut self, bus: &mut B) {
        let target = self.r[14];
        self.restore_cpsr();
        self.r[15] = target & if self.thumb() { !1 } else { !3 };
        self.reload_pipeline(bus);
    }

    /// Sets the IRQ input. While it is high and CPSR.I is clear, the CPU
    /// takes the IRQ exception before the next instruction. A high line
    /// also ends an ARM9's wait for an interrupt, whatever CPSR.I says.
    pub fn set_irq_line(&mut self, high: bool) {
        self.irq_line = high;
    }

    /// A snapshot of every register.
    #[must_use]
    pub fn registers(&self) -> Registers {
        // Copy the banks with the current mode's live registers stored, so
        // every bank is current.
        let (mut r8_12, mut r13_14) = (self.r8_12, self.r13_14);
        let mode = self.mode();
        r8_12[usize::from(mode == Mode::Fiq)].copy_from_slice(&self.r[8..13]);
        r13_14[mode.bank()] = [self.r[13], self.r[14]];
        let r8_14 = |low: [u32; 5], high: [u32; 2]| {
            [low[0], low[1], low[2], low[3], low[4], high[0], high[1]]
        };
        Registers {
            r: self.r,
            usr: r8_14(r8_12[0], r13_14[0]),
            fiq: r8_14(r8_12[1], r13_14[1]),
            svc: r13_14[3],
            abt: r13_14[4],
            irq: r13_14[2],
            und: r13_14[5],
            cpsr: self.cpsr,
            spsr: self.spsr,
        }
    }

    /// A CPU with these registers, prefetched instructions and next fetch
    /// access, as the per-instruction tests set it up.
    #[must_use]
    pub fn from_registers(registers: &Registers, pipeline: [u32; 2], fetch_access: Access) -> Self {
        let mut cpu = Self {
            r: registers.r,
            cpsr: registers.cpsr,
            r8_12: [[0; 5]; 2],
            r13_14: [[0; 2]; 6],
            spsr: registers.spsr,
            pipeline,
            fetch_access,
            irq_line: false,
            model: M::default(),
        };
        let (usr, fiq) = (registers.usr, registers.fiq);
        cpu.r8_12[0] = [usr[0], usr[1], usr[2], usr[3], usr[4]];
        cpu.r8_12[1] = [fiq[0], fiq[1], fiq[2], fiq[3], fiq[4]];
        cpu.r13_14[0] = [usr[5], usr[6]];
        cpu.r13_14[1] = [fiq[5], fiq[6]];
        cpu.r13_14[2] = registers.irq;
        cpu.r13_14[3] = registers.svc;
        cpu.r13_14[4] = registers.abt;
        cpu.r13_14[5] = registers.und;
        // The current mode's banks hold whatever `r` says.
        cpu.store_bank(cpu.mode());
        cpu
    }

    /// Resets to Supervisor mode at the reset vector (address 0, or
    /// 0xFFFF0000 for an ARM9 whose CP15 resets to high vectors) and fills
    /// the pipeline. An ARM9's TCM keeps its contents.
    pub fn reset<B: Bus>(&mut self, bus: &mut B) {
        self.r = [0; 16];
        self.cpsr = RESET_CPSR;
        self.r8_12 = [[0; 5]; 2];
        self.r13_14 = [[0; 2]; 6];
        self.spsr = [0; 5];
        self.pipeline = [0; 2];
        self.fetch_access = Access::CODE;
        self.irq_line = false;
        self.model.reset();
        self.r[15] = self.model.vector_base();
        self.reload_pipeline(bus);
    }

    /// Jumps to `address` in the current state (ARM or Thumb) and fills
    /// the pipeline. For boot code that skips the BIOS.
    pub fn jump<B: Bus>(&mut self, bus: &mut B, address: u32) {
        self.r[15] = address;
        self.reload_pipeline(bus);
    }

    /// Sets register `index` (0–14) of the current mode.
    pub fn set_reg(&mut self, index: usize, value: u32) {
        self.r[index] = value;
    }

    /// Replaces the CPSR, switching register banks if the mode changes.
    pub fn set_cpsr(&mut self, value: u32) {
        let old = self.mode();
        let new = Mode::from_bits(value);
        if old != new {
            self.store_bank(old);
            self.load_bank(new);
        }
        self.cpsr = value;
    }

    /// Runs one instruction, or takes a pending IRQ. An ARM9 waiting for
    /// an interrupt runs nothing: it spends one idle cycle, or wakes if the
    /// IRQ line is high.
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        if self.model.halted() {
            if !self.irq_line {
                bus.idle();
                return;
            }
            self.model.wake();
        }
        if self.irq_line && self.cpsr & psr::I == 0 {
            self.take_irq(bus);
            return;
        }
        let instruction = self.pipeline[0];
        self.pipeline[0] = self.pipeline[1];
        let user = self.user_permissions();
        if self.thumb() {
            self.r[15] &= !1;
            let access = self.fetch_access | Access::CODE;
            self.pipeline[1] = u32::from(self.model.fetch_16(bus, self.r[15], access, user));
            self.execute_thumb(bus, instruction as u16);
        } else {
            self.r[15] &= !3;
            let access = self.fetch_access | Access::CODE;
            self.pipeline[1] = self.model.fetch_32(bus, self.r[15], access, user);
            let condition = instruction >> 28;
            if self.condition_passed(condition) {
                self.execute_arm(bus, instruction);
            } else if M::V5TE && condition == 0xF {
                self.execute_unconditional(bus, instruction);
            } else {
                self.advance();
            }
        }
    }

    /// Moves past the current instruction without a jump: the next fetch
    /// follows sequentially.
    fn advance(&mut self) {
        self.r[15] = self.r[15].wrapping_add(if self.thumb() { 2 } else { 4 });
        self.fetch_access = Access::SEQUENTIAL;
    }

    /// Like [`Processor::advance`], but the bus was used for data, so the
    /// next fetch is non-sequential.
    fn advance_after_data(&mut self) {
        self.advance();
        self.fetch_access = Access::NONSEQUENTIAL;
    }

    /// Refills the pipeline from R15 after a jump: one non-sequential and
    /// one sequential fetch, leaving R15 two instructions ahead. R15 is not
    /// aligned here (a jump to an odd address keeps its low bits until the
    /// next instruction starts), matching the hardware's PC register.
    fn reload_pipeline<B: Bus>(&mut self, bus: &mut B) {
        let sequential = Access::CODE | Access::SEQUENTIAL;
        let user = self.user_permissions();
        if self.thumb() {
            let first = self
                .model
                .fetch_16(bus, self.r[15] & !1, Access::CODE, user);
            self.pipeline[0] = u32::from(first);
            self.r[15] = self.r[15].wrapping_add(2);
            let second = self.model.fetch_16(bus, self.r[15] & !1, sequential, user);
            self.pipeline[1] = u32::from(second);
            self.r[15] = self.r[15].wrapping_add(2);
        } else {
            self.pipeline[0] = self
                .model
                .fetch_32(bus, self.r[15] & !3, Access::CODE, user);
            self.r[15] = self.r[15].wrapping_add(4);
            self.pipeline[1] = self.model.fetch_32(bus, self.r[15] & !3, sequential, user);
            self.r[15] = self.r[15].wrapping_add(4);
        }
        self.fetch_access = Access::SEQUENTIAL;
    }

    /// Whether a load into R15 (LDR, LDM, Thumb POP) takes the state from
    /// bit 0: ARMv5, unless CP15 asks for ARMv4 behaviour.
    fn loads_interwork(&self) -> bool {
        M::V5TE && self.model.loads_interwork()
    }

    /// After a load into R15 on ARMv5: bit 0 selects Thumb.
    fn set_state_from_pc(&mut self) {
        let pc = self.r[15];
        self.set_flag(psr::T, pc & 1 != 0);
        self.r[15] = pc & !1;
    }

    /// Whether accesses have User mode's permissions rather than
    /// privileged ones, for a protection unit to check.
    fn user_permissions(&self) -> bool {
        M::PROTECTION_UNIT && self.mode() == Mode::User
    }

    /// Reads a data word through the model (TCM or the bus). Data accesses
    /// the protection unit refuses read 0 and write nothing; the
    /// instruction then calls [`Processor::abort_if_refused`].
    fn read_32<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access) -> u32 {
        let user = self.user_permissions();
        self.model.read_32(bus, address, access, user)
    }

    /// Reads a data halfword through the model.
    fn read_16<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access) -> u16 {
        let user = self.user_permissions();
        self.model.read_16(bus, address, access, user)
    }

    /// Reads a data byte through the model.
    fn read_8<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access) -> u8 {
        let user = self.user_permissions();
        self.model.read_8(bus, address, access, user)
    }

    /// Writes a data word through the model.
    fn write_32<B: Bus>(&mut self, bus: &mut B, address: u32, value: u32, access: Access) {
        let user = self.user_permissions();
        self.model.write_32(bus, address, value, access, user);
    }

    /// Writes a data halfword through the model.
    fn write_16<B: Bus>(&mut self, bus: &mut B, address: u32, value: u16, access: Access) {
        let user = self.user_permissions();
        self.model.write_16(bus, address, value, access, user);
    }

    /// Writes a data byte through the model.
    fn write_8<B: Bus>(&mut self, bus: &mut B, address: u32, value: u8, access: Access) {
        let user = self.user_permissions();
        self.model.write_8(bus, address, value, access, user);
    }

    /// After an instruction's data accesses: if the protection unit
    /// refused one, takes the data abort and returns true. The instruction
    /// must then stop, loading no register and writing no base back
    /// (ARMv5's base-restored abort model).
    fn abort_if_refused<B: Bus>(&mut self, bus: &mut B) -> bool {
        let refused = self.model.data_refused();
        if refused {
            self.data_abort(bus);
        }
        refused
    }

    /// Whether condition `condition` (0–15) passes with the current flags.
    fn condition_passed(&self, condition: u32) -> bool {
        CONDITIONS[condition as usize] >> (self.cpsr >> 28) & 1 != 0
    }

    /// Saves R8–R14 of `mode` from the live registers into its banks.
    fn store_bank(&mut self, mode: Mode) {
        let fiq = usize::from(mode == Mode::Fiq);
        self.r8_12[fiq].copy_from_slice(&self.r[8..13]);
        self.r13_14[mode.bank()] = [self.r[13], self.r[14]];
    }

    /// Loads R8–R14 of `mode` from its banks into the live registers.
    fn load_bank(&mut self, mode: Mode) {
        let fiq = usize::from(mode == Mode::Fiq);
        self.r[8..13].copy_from_slice(&self.r8_12[fiq]);
        [self.r[13], self.r[14]] = self.r13_14[mode.bank()];
    }

    /// The current mode's SPSR, if it has one.
    fn spsr(&self) -> Option<u32> {
        self.mode().spsr_index().map(|index| self.spsr[index])
    }

    fn set_spsr(&mut self, value: u32) {
        if let Some(index) = self.mode().spsr_index() {
            self.spsr[index] = value;
        }
    }

    /// Restores CPSR from SPSR (exception return). In User or System mode
    /// there is no SPSR and CPSR is left alone.
    fn restore_cpsr(&mut self) {
        if let Some(spsr) = self.spsr() {
            self.set_cpsr(spsr);
        }
    }

    /// Enters an exception: saves CPSR, switches mode in ARM state with
    /// IRQs off, sets LR, and jumps to `vector` from the model's base.
    fn enter_exception<B: Bus>(
        &mut self,
        bus: &mut B,
        mode: Mode,
        vector: u32,
        return_address: u32,
    ) {
        let old_cpsr = self.cpsr;
        self.set_cpsr((old_cpsr & !0x3F) | mode.bits() | psr::I);
        self.set_spsr(old_cpsr);
        self.r[14] = return_address;
        self.r[15] = self.model.vector_base() | vector;
        self.reload_pipeline(bus);
    }

    fn take_irq<B: Bus>(&mut self, bus: &mut B) {
        // The cycle that decides to take the interrupt still fetches the
        // next instruction, which the exception then throws away with the
        // rest of the pipeline: entry costs 2S + 1N, as the ARM7TDMI's
        // datasheet gives.
        let user = self.user_permissions();
        let access = self.fetch_access | Access::CODE;
        if self.thumb() {
            self.model.fetch_16(bus, self.r[15] & !1, access, user);
        } else {
            self.model.fetch_32(bus, self.r[15] & !3, access, user);
        }
        // LR points one instruction past the next one, so handlers return
        // with SUBS PC, LR, #4.
        let return_address = if self.thumb() {
            self.r[15]
        } else {
            self.r[15].wrapping_sub(4)
        };
        self.enter_exception(bus, Mode::Irq, vector::IRQ, return_address);
    }

    fn software_interrupt<B: Bus>(&mut self, bus: &mut B) {
        let return_address = self.r[15].wrapping_sub(if self.thumb() { 2 } else { 4 });
        self.enter_exception(bus, Mode::Supervisor, vector::SWI, return_address);
    }

    fn undefined<B: Bus>(&mut self, bus: &mut B) {
        let return_address = self.r[15].wrapping_sub(if self.thumb() { 2 } else { 4 });
        bus.idle();
        self.enter_exception(bus, Mode::Undefined, vector::UNDEFINED, return_address);
    }

    /// A data abort, for an instruction whose data access the protection
    /// unit refused. Data instructions move R15 past themselves before
    /// their accesses, so the instruction is 12 (ARM) or 6 (Thumb) bytes
    /// behind it. LR is its address plus 8 in either state, so `SUBS PC,
    /// LR, #8` retries it.
    fn data_abort<B: Bus>(&mut self, bus: &mut B) {
        let return_address = if self.thumb() {
            self.r[15].wrapping_add(2)
        } else {
            self.r[15].wrapping_sub(4)
        };
        self.enter_exception(bus, Mode::Abort, vector::DATA_ABORT, return_address);
    }

    /// BKPT (ARMv5): a prefetch abort. LR is the breakpoint's address plus
    /// 4 in either state, so `SUBS PC, LR, #4` retries it. A fetch the
    /// protection unit refuses becomes BKPT, so this is its abort too.
    fn breakpoint<B: Bus>(&mut self, bus: &mut B) {
        let return_address = if self.thumb() {
            self.r[15]
        } else {
            self.r[15].wrapping_sub(4)
        };
        self.enter_exception(bus, Mode::Abort, vector::PREFETCH_ABORT, return_address);
    }

    fn set_nz(&mut self, value: u32) {
        self.cpsr = (self.cpsr & !(psr::N | psr::Z))
            | (value & psr::N)
            | if value == 0 { psr::Z } else { 0 };
    }

    fn set_flag(&mut self, flag: u32, on: bool) {
        if on {
            self.cpsr |= flag;
        } else {
            self.cpsr &= !flag;
        }
    }

    fn carry(&self) -> bool {
        self.cpsr & psr::C != 0
    }

    /// Sets C after a flag-setting multiply of `multiplicand` (Rm) by
    /// `multiplier_operand` (Rs) plus `accumulator`, as the ARM7TDMI's
    /// multiplier leaves it. ARMv5 leaves C alone.
    fn set_multiply_carry(
        &mut self,
        kind: multiplier::Kind,
        multiplicand: u32,
        multiplier_operand: u32,
        accumulator: u64,
    ) {
        if !M::V5TE {
            let output = multiplier::multiply(kind, multiplicand, multiplier_operand, accumulator);
            self.set_flag(psr::C, output.carry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_switches_bank_r13_and_r14() {
        let mut cpu = Cpu::default();
        cpu.set_cpsr(Mode::User.bits());
        cpu.set_reg(13, 0x100);
        cpu.set_cpsr(Mode::Irq.bits());
        cpu.set_reg(13, 0x200);
        cpu.set_cpsr(Mode::System.bits());
        assert_eq!(cpu.reg(13), 0x100);
        cpu.set_cpsr(Mode::Irq.bits());
        assert_eq!(cpu.reg(13), 0x200);
    }

    #[test]
    fn fiq_banks_r8_to_r12() {
        let mut cpu = Cpu::default();
        cpu.set_cpsr(Mode::User.bits());
        cpu.set_reg(8, 8);
        cpu.set_cpsr(Mode::Fiq.bits());
        assert_eq!(cpu.reg(8), 0);
        cpu.set_reg(8, 88);
        cpu.set_cpsr(Mode::Supervisor.bits());
        assert_eq!(cpu.reg(8), 8);
        assert_eq!(cpu.registers().fiq[0], 88);
    }

    /// 64 KiB of RAM holding `MOV R0, R0` everywhere, logging every
    /// access's address and type.
    struct LoggingBus(Vec<(u32, Access)>);

    impl Bus for LoggingBus {
        fn read_32(&mut self, address: u32, access: Access) -> u32 {
            self.0.push((address, access));
            0xE1A0_0000
        }
        fn read_16(&mut self, address: u32, access: Access) -> u16 {
            self.0.push((address, access));
            0x46C0 // MOV R8, R8
        }
        fn read_8(&mut self, address: u32, access: Access) -> u8 {
            self.0.push((address, access));
            0
        }
        fn write_32(&mut self, address: u32, _: u32, access: Access) {
            self.0.push((address, access));
        }
        fn write_16(&mut self, address: u32, _: u16, access: Access) {
            self.0.push((address, access));
        }
        fn write_8(&mut self, address: u32, _: u8, access: Access) {
            self.0.push((address, access));
        }
        fn idle(&mut self) {}
    }

    #[test]
    fn taking_an_irq_fetches_once_before_the_vector() {
        let mut bus = LoggingBus(Vec::new());
        let mut cpu = Cpu::default();
        cpu.set_cpsr(Mode::System.bits());
        cpu.jump(&mut bus, 0x1000);
        cpu.step(&mut bus);
        bus.0.clear();

        cpu.set_irq_line(true);
        cpu.step(&mut bus);
        let code = Access::CODE;
        let sequential = Access::CODE | Access::SEQUENTIAL;
        // The discarded fetch of 0x100C, then the vector's two fetches.
        assert_eq!(
            bus.0,
            [(0x100C, sequential), (0x18, code), (0x1C, sequential)]
        );
        assert_eq!(cpu.mode(), Mode::Irq);
        // SUBS PC, LR, #4 returns to the instruction at 0x1004.
        assert_eq!(cpu.reg(14), 0x1008);
    }

    #[test]
    fn registers_round_trip() {
        let mut cpu = Cpu::default();
        cpu.set_cpsr(Mode::Abort.bits() | psr::C);
        for index in 0..15 {
            cpu.set_reg(index, index as u32 * 3);
        }
        let registers = cpu.registers();
        let copy = Cpu::from_registers(&registers, [1, 2], Access::SEQUENTIAL);
        assert_eq!(copy.registers(), registers);
    }
}
