//! The ARM946E-S (ARMv5TE), the DS's ARM9: the ARM7TDMI's interpreter plus
//! the ARMv5TE instructions, CP15, tightly coupled memory, and waiting for
//! interrupts.
//!
//! TCM hits never reach the [`Bus`], but each one still spends a cycle
//! through [`Bus::idle`] so the machine's clock moves while code runs from
//! TCM. Instruction fetches use the instruction TCM only; data accesses try
//! the instruction TCM first, then the data TCM. Caches are not emulated:
//! every access that misses TCM goes to the bus.
//!
//! With the protection unit on, CP15 checks every access first, TCM
//! included, as melonDS does. A refused access spends a cycle without
//! touching memory: a fetch returns BKPT, which the CPU takes as the
//! prefetch abort it is on this core if it gets to execute it, and a data
//! access returns 0 and leaves the instruction to take a data abort.

use serde::{Deserialize, Serialize};

use crate::cp15::Cp15;
use crate::model::{Model, hooks::Hooks};
use crate::protection::{EXECUTE, READ, WRITE};
use crate::tcm::{Block, DTCM_SIZE, ITCM_SIZE};
use crate::{Access, Bus, Processor};

/// What a refused ARM fetch returns: BKPT #0.
const REFUSED_ARM_FETCH: u32 = 0xE120_0070;
/// What a refused Thumb fetch returns: BKPT #0.
const REFUSED_THUMB_FETCH: u16 = 0xBE00;

/// The DS's ARM9, an ARM946E-S.
///
/// Beyond [`Cpu`](crate::Cpu)'s methods it has [`Arm9::cp15`] and
/// [`Arm9::cp15_mut`] for the system control coprocessor, [`Arm9::halted`]
/// and [`Arm9::wake`] for waiting for interrupts, and TCM access for
/// debuggers and direct boot.
pub type Arm9 = Processor<Arm946es>;

/// The ARM946E-S model: CP15, 32 KiB of instruction TCM, 16 KiB of data
/// TCM, and the wait-for-interrupt latch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arm946es {
    cp15: Cp15,
    itcm: Block,
    dtcm: Block,
    /// Waiting for an interrupt (MCR p15, 0, Rd, c7, c0, 4 or c7, c8, 2).
    halted: bool,
    /// The protection unit refused a data access in the current
    /// instruction, so its later accesses do nothing. Always clear between
    /// instructions, so not saved.
    #[serde(skip)]
    data_refused: bool,
}

impl Default for Arm946es {
    fn default() -> Self {
        Self {
            cp15: Cp15::default(),
            itcm: Block::new(ITCM_SIZE),
            dtcm: Block::new(DTCM_SIZE),
            halted: false,
            data_refused: false,
        }
    }
}

impl Arm946es {
    /// Whether a data access of kind `need` may go ahead: the protection
    /// unit allows it and has refused nothing earlier in the instruction.
    /// A refused access spends a cycle.
    #[inline(always)]
    fn data_allowed<B: Bus>(&mut self, bus: &mut B, address: u32, need: u8, user: bool) -> bool {
        if self.cp15.permits(address, need, user) && !self.data_refused {
            return true;
        }
        self.data_refused = true;
        bus.idle();
        false
    }

    /// Whether an instruction fetch may go ahead; a refused one spends a
    /// cycle.
    #[inline(always)]
    fn fetch_allowed<B: Bus>(&self, bus: &mut B, address: u32, user: bool) -> bool {
        if self.cp15.permits(address, EXECUTE, user) {
            return true;
        }
        bus.idle();
        false
    }

    /// The TCM block a data read at `address` comes from.
    #[inline(always)]
    fn read_block(&self, address: u32) -> Option<&Block> {
        if self.cp15.itcm().readable(address) {
            Some(&self.itcm)
        } else if self.cp15.dtcm().readable(address) {
            Some(&self.dtcm)
        } else {
            None
        }
    }

    /// The TCM block a data write at `address` goes to.
    #[inline(always)]
    fn write_block(&mut self, address: u32) -> Option<&mut Block> {
        if self.cp15.itcm().contains(address) {
            Some(&mut self.itcm)
        } else if self.cp15.dtcm().contains(address) {
            Some(&mut self.dtcm)
        } else {
            None
        }
    }
}

/// CRn, CRm and opcode 2 of an MRC/MCR to CP15 with opcode 1 = 0, or
/// `None` for another coprocessor. Other opcode 1 values (the BIST
/// registers) read as 0 and ignore writes, which CRn = 15 does anyway.
fn cp15_register(instruction: u32) -> Option<(u32, u32, u32)> {
    if (instruction >> 8) & 0xF != 15 {
        return None;
    }
    let crn = if (instruction >> 21) & 7 == 0 {
        (instruction >> 16) & 0xF
    } else {
        15
    };
    Some((crn, instruction & 0xF, (instruction >> 5) & 7))
}

impl Model for Arm946es {}

impl Hooks for Arm946es {
    const V5TE: bool = true;
    const PROTECTION_UNIT: bool = true;

    /// CP15 returns to its reset values; TCM keeps its contents.
    fn reset(&mut self) {
        self.cp15 = Cp15::default();
        self.halted = false;
        self.data_refused = false;
    }

    // Every instruction is fetched through `fetch_32` or `fetch_16`, and
    // most data accesses are words (the stack, LDM and STM), but the
    // compiler does not inline these on its own. Forced, a frame of
    // Platinum runs about 3% fewer host instructions.

    #[inline(always)]
    fn fetch_32<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, user: bool) -> u32 {
        if !self.fetch_allowed(bus, address, user) {
            REFUSED_ARM_FETCH
        } else if self.cp15.itcm().contains(address) {
            bus.idle();
            self.itcm.read_32(address)
        } else {
            bus.read_32(address, access)
        }
    }

    #[inline(always)]
    fn fetch_16<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, user: bool) -> u16 {
        if !self.fetch_allowed(bus, address, user) {
            REFUSED_THUMB_FETCH
        } else if self.cp15.itcm().contains(address) {
            bus.idle();
            self.itcm.read_16(address)
        } else {
            bus.read_16(address, access)
        }
    }

    #[inline(always)]
    fn read_32<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, user: bool) -> u32 {
        if !self.data_allowed(bus, address, READ, user) {
            0
        } else if let Some(block) = self.read_block(address) {
            bus.idle();
            let value = block.read_32(address);
            bus.tcm_data(address, 4, false, value);
            value
        } else {
            bus.read_32(address, access)
        }
    }

    #[inline(always)]
    fn read_16<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, user: bool) -> u16 {
        if !self.data_allowed(bus, address, READ, user) {
            0
        } else if let Some(block) = self.read_block(address) {
            bus.idle();
            let value = block.read_16(address);
            bus.tcm_data(address, 2, false, value.into());
            value
        } else {
            bus.read_16(address, access)
        }
    }

    #[inline(always)]
    fn read_8<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, user: bool) -> u8 {
        if !self.data_allowed(bus, address, READ, user) {
            0
        } else if let Some(block) = self.read_block(address) {
            bus.idle();
            let value = block.read_8(address);
            bus.tcm_data(address, 1, false, value.into());
            value
        } else {
            bus.read_8(address, access)
        }
    }

    #[inline(always)]
    fn write_32<B: Bus>(
        &mut self,
        bus: &mut B,
        address: u32,
        value: u32,
        access: Access,
        user: bool,
    ) {
        if !self.data_allowed(bus, address, WRITE, user) {
            return;
        }
        if let Some(block) = self.write_block(address) {
            block.write_32(address, value);
            bus.idle();
            bus.tcm_data(address, 4, true, value);
        } else {
            bus.write_32(address, value, access);
        }
    }

    #[inline(always)]
    fn write_16<B: Bus>(
        &mut self,
        bus: &mut B,
        address: u32,
        value: u16,
        access: Access,
        user: bool,
    ) {
        if !self.data_allowed(bus, address, WRITE, user) {
            return;
        }
        if let Some(block) = self.write_block(address) {
            block.write_16(address, value);
            bus.idle();
            bus.tcm_data(address, 2, true, value.into());
        } else {
            bus.write_16(address, value, access);
        }
    }

    #[inline(always)]
    fn write_8<B: Bus>(
        &mut self,
        bus: &mut B,
        address: u32,
        value: u8,
        access: Access,
        user: bool,
    ) {
        if !self.data_allowed(bus, address, WRITE, user) {
            return;
        }
        if let Some(block) = self.write_block(address) {
            block.write_8(address, value);
            bus.idle();
            bus.tcm_data(address, 1, true, value.into());
        } else {
            bus.write_8(address, value, access);
        }
    }

    #[inline(always)]
    fn data_refused(&mut self) -> bool {
        std::mem::take(&mut self.data_refused)
    }

    #[inline(always)]
    fn vector_base(&self) -> u32 {
        self.cp15.vector_base()
    }

    #[inline(always)]
    fn loads_interwork(&self) -> bool {
        self.cp15.loads_interwork()
    }

    #[inline(always)]
    fn halted(&self) -> bool {
        self.halted
    }

    #[inline(always)]
    fn wake(&mut self) {
        self.halted = false;
    }

    fn coprocessor_read(&mut self, instruction: u32) -> Option<u32> {
        let (crn, crm, opcode2) = cp15_register(instruction)?;
        Some(self.cp15.read(crn, crm, opcode2))
    }

    fn coprocessor_write(&mut self, instruction: u32, value: u32) -> bool {
        let Some((crn, crm, opcode2)) = cp15_register(instruction) else {
            return false;
        };
        if matches!((crn, crm, opcode2), (7, 0, 4) | (7, 8, 2)) {
            self.halted = true;
        }
        self.cp15.write(crn, crm, opcode2, value);
        true
    }
}

impl Processor<Arm946es> {
    /// The system control coprocessor.
    #[must_use]
    pub fn cp15(&self) -> &Cp15 {
        &self.model.cp15
    }

    /// The system control coprocessor, for direct boot and debuggers. Set
    /// it up before [`Arm9::jump`], whose instruction fetches already
    /// depend on the TCM mapping.
    pub fn cp15_mut(&mut self) -> &mut Cp15 {
        &mut self.model.cp15
    }

    /// Whether the CPU is waiting for an interrupt. [`Arm9::step`] does not
    /// run instructions while it is: it spends one idle cycle, unless the
    /// IRQ line is high, which wakes the CPU (whatever CPSR.I says) and
    /// takes the IRQ if CPSR.I allows. A machine can instead skip ahead to
    /// its next event while this is true.
    #[must_use]
    pub fn halted(&self) -> bool {
        self.model.halted
    }

    /// Ends a wait for an interrupt; the next step runs the instruction
    /// after the one that halted.
    pub fn wake(&mut self) {
        self.model.halted = false;
    }

    /// The TCM byte a data read at `address` would return, or `None` when
    /// the read would go to the bus. No time passes.
    #[must_use]
    pub fn tcm_read_8(&self, address: u32) -> Option<u8> {
        self.model
            .read_block(address)
            .map(|block| block.read_8(address))
    }

    /// Writes a byte where a data write at `address` would put it, if that
    /// is TCM; returns whether it was. No time passes.
    pub fn tcm_write_8(&mut self, address: u32, value: u8) -> bool {
        self.model
            .write_block(address)
            .map(|block| block.write_8(address, value))
            .is_some()
    }

    /// The instruction TCM's contents.
    #[must_use]
    pub fn itcm(&self) -> &[u8] {
        self.model.itcm.bytes()
    }

    /// The data TCM's contents.
    #[must_use]
    pub fn dtcm(&self) -> &[u8] {
        self.model.dtcm.bytes()
    }
}
