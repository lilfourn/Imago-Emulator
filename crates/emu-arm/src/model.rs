//! What sets one ARM core apart from another: its architecture version, the
//! memory inside the core, and its system control coprocessor.
//!
//! A model is a type parameter of [`Processor`](crate::Processor), so the
//! interpreter is compiled once per model: the ARM7TDMI's hooks are plain
//! calls to the [`Bus`] and its checks for ARMv5 behaviour are constants,
//! so it pays nothing for the ARM9's extras, the protection unit included.

use serde::{Deserialize, Serialize};

use crate::{Access, Bus};

/// An ARM core the interpreter can be: [`Arm7tdmi`] or
/// [`Arm946es`](crate::Arm946es). The trait is sealed; its hooks are
/// internal to this crate.
pub trait Model: hooks::Hooks {}

pub(crate) mod hooks {
    use std::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use crate::{Access, Bus};

    /// The hooks the interpreter calls. Memory hooks take the address as
    /// the instruction computed it, like [`Bus`], and `user`: whether the
    /// access has User mode's permissions (in User mode, or for LDRT and
    /// STRT) rather than privileged ones, for a protection unit to check.
    ///
    /// A fetch the protection unit refuses returns BKPT instead of touching
    /// memory: the ARM946E-S takes both as a prefetch abort, with the same
    /// mode, vector and return address, so the abort happens only if the
    /// instruction reaches execution. A refused data access does nothing,
    /// as does every later one until [`Hooks::data_refused`] is asked.
    pub trait Hooks: Debug + Clone + Default + Serialize + DeserializeOwned {
        /// ARMv5TE: CLZ, BLX, BKPT, the saturating and DSP multiply
        /// instructions, LDRD/STRD, coprocessor register transfers, and
        /// ARMv5's choices where ARMv4 behaviour was unpredictable.
        const V5TE: bool;
        /// A protection unit, which checks memory accesses by privilege.
        const PROTECTION_UNIT: bool;

        /// Returns the model's own state to its reset values.
        fn reset(&mut self);
        /// Fetches an ARM instruction.
        fn fetch_32<B: Bus>(
            &mut self,
            bus: &mut B,
            address: u32,
            access: Access,
            user: bool,
        ) -> u32;
        /// Fetches a Thumb instruction.
        fn fetch_16<B: Bus>(
            &mut self,
            bus: &mut B,
            address: u32,
            access: Access,
            user: bool,
        ) -> u16;
        /// Reads a data word.
        fn read_32<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, user: bool)
        -> u32;
        /// Reads a data halfword.
        fn read_16<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, user: bool)
        -> u16;
        /// Reads a data byte.
        fn read_8<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, user: bool) -> u8;
        /// Writes a data word.
        fn write_32<B: Bus>(
            &mut self,
            bus: &mut B,
            address: u32,
            value: u32,
            access: Access,
            user: bool,
        );
        /// Writes a data halfword.
        fn write_16<B: Bus>(
            &mut self,
            bus: &mut B,
            address: u32,
            value: u16,
            access: Access,
            user: bool,
        );
        /// Writes a data byte.
        fn write_8<B: Bus>(
            &mut self,
            bus: &mut B,
            address: u32,
            value: u8,
            access: Access,
            user: bool,
        );
        /// Whether the protection unit refused a data access since the
        /// last call, which clears it. The instruction then takes a data
        /// abort.
        fn data_refused(&mut self) -> bool;
        /// Where the exception vectors start.
        fn vector_base(&self) -> u32;
        /// Whether loads into R15 take the state (ARM or Thumb) from bit 0.
        fn loads_interwork(&self) -> bool;
        /// Whether the core is waiting for an interrupt.
        fn halted(&self) -> bool;
        /// Ends a wait for an interrupt.
        fn wake(&mut self);
        /// MRC: the value a coprocessor register transfer reads, or `None`
        /// if no coprocessor answers (an undefined instruction).
        fn coprocessor_read(&mut self, instruction: u32) -> Option<u32>;
        /// MCR: whether a coprocessor accepted the write.
        fn coprocessor_write(&mut self, instruction: u32, value: u32) -> bool;
    }
}

/// The ARM7TDMI (ARMv4T): the GBA's CPU and the DS's ARM7. Every memory
/// access goes to the bus, and it has no coprocessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Arm7tdmi;

impl Model for Arm7tdmi {}

impl hooks::Hooks for Arm7tdmi {
    const V5TE: bool = false;
    const PROTECTION_UNIT: bool = false;

    fn reset(&mut self) {}

    #[inline]
    fn fetch_32<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, _: bool) -> u32 {
        bus.read_32(address, access)
    }

    #[inline]
    fn fetch_16<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, _: bool) -> u16 {
        bus.read_16(address, access)
    }

    #[inline]
    fn read_32<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, _: bool) -> u32 {
        bus.read_32(address, access)
    }

    #[inline]
    fn read_16<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, _: bool) -> u16 {
        bus.read_16(address, access)
    }

    #[inline]
    fn read_8<B: Bus>(&mut self, bus: &mut B, address: u32, access: Access, _: bool) -> u8 {
        bus.read_8(address, access)
    }

    #[inline]
    fn write_32<B: Bus>(&mut self, bus: &mut B, address: u32, value: u32, access: Access, _: bool) {
        bus.write_32(address, value, access);
    }

    #[inline]
    fn write_16<B: Bus>(&mut self, bus: &mut B, address: u32, value: u16, access: Access, _: bool) {
        bus.write_16(address, value, access);
    }

    #[inline]
    fn write_8<B: Bus>(&mut self, bus: &mut B, address: u32, value: u8, access: Access, _: bool) {
        bus.write_8(address, value, access);
    }

    #[inline]
    fn data_refused(&mut self) -> bool {
        false
    }

    #[inline]
    fn vector_base(&self) -> u32 {
        0
    }

    #[inline]
    fn loads_interwork(&self) -> bool {
        false
    }

    #[inline]
    fn halted(&self) -> bool {
        false
    }

    #[inline]
    fn wake(&mut self) {}

    fn coprocessor_read(&mut self, _instruction: u32) -> Option<u32> {
        None
    }

    fn coprocessor_write(&mut self, _instruction: u32, _value: u32) -> bool {
        false
    }
}
