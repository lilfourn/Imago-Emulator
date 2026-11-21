//! The ARM946E-S system control coprocessor, CP15, as the DS has it
//! (GBATEK, "ARM CP15 System Control Coprocessor"; ARM946E-S Technical
//! Reference Manual, chapter 2).
//!
//! Registers are addressed as MRC/MCR do, by CRn, CRm and opcode 2 (opcode
//! 1 is always 0 on the ARM946E-S). This keeps the ID codes, the control
//! register, the protection unit, cache lockdown, TCM regions and the trace
//! process ID. While the protection unit is on, its regions and access
//! permissions decide which accesses abort (see `protection.rs`); the
//! cachable and bufferable bits are stored only. Cache commands (c7) and the
//! test registers (c15) have no state here; caches are not emulated.

use serde::{Deserialize, Serialize};

use crate::protection::Map;
use crate::tcm::Window;

/// Bits of the control register (c1, c0, 0).
pub mod control {
    /// Protection unit enable.
    pub const PROTECTION_UNIT: u32 = 1 << 0;
    /// Data cache enable.
    pub const DATA_CACHE: u32 = 1 << 2;
    /// Big-endian (the DS runs little-endian; stored, not emulated).
    pub const BIG_ENDIAN: u32 = 1 << 7;
    /// Instruction cache enable.
    pub const INSTRUCTION_CACHE: u32 = 1 << 12;
    /// Exception vectors at 0xFFFF0000 instead of 0.
    pub const HIGH_VECTORS: u32 = 1 << 13;
    /// Round-robin cache replacement.
    pub const ROUND_ROBIN: u32 = 1 << 14;
    /// ARMv4 behaviour for loads into R15: bit 0 does not select Thumb.
    pub const PRE_V5: u32 = 1 << 15;
    /// Data TCM enable.
    pub const DTCM_ENABLE: u32 = 1 << 16;
    /// Data TCM load mode: reads go to the bus, writes to the TCM.
    pub const DTCM_LOAD: u32 = 1 << 17;
    /// Instruction TCM enable.
    pub const ITCM_ENABLE: u32 = 1 << 18;
    /// Instruction TCM load mode: data reads go to the bus.
    pub const ITCM_LOAD: u32 = 1 << 19;
    /// Bits that read as 1 whatever is written (write buffer, 32-bit
    /// exceptions and addressing, late aborts).
    pub const FIXED: u32 = 0x78;
    /// Bits software can change.
    pub const WRITABLE: u32 = PROTECTION_UNIT
        | DATA_CACHE
        | BIG_ENDIAN
        | INSTRUCTION_CACHE
        | HIGH_VECTORS
        | ROUND_ROBIN
        | PRE_V5
        | DTCM_ENABLE
        | DTCM_LOAD
        | ITCM_ENABLE
        | ITCM_LOAD;
    /// The value after reset. The DS ties VINITHI high, so the vectors
    /// start at 0xFFFF0000 where its ARM9 BIOS is; both TCMs are off.
    pub const RESET: u32 = FIXED | HIGH_VECTORS;
}

/// Main ID: ARM, ARMv5TE, ARM946, revision 1.
pub const MAIN_ID: u32 = 0x4105_9461;
/// Cache type: separate 8 KiB instruction and 4 KiB data caches, 4-way,
/// 32-byte lines, write-back with format B lockdown.
pub const CACHE_TYPE: u32 = 0x0F0D_2112;
/// TCM sizes: 32 KiB instruction TCM, 16 KiB data TCM.
pub const TCM_SIZE: u32 = 0x0014_0180;

/// Writable bits of a protection region register: enable, size, base.
const REGION_MASK: u32 = 0xFFFF_F03F;
/// Writable bits of the data TCM region register: base and virtual size.
const DTCM_REGION_MASK: u32 = 0xFFFF_F03E;
/// The DS's instruction TCM base is fixed at 0; only its size changes.
const ITCM_REGION_MASK: u32 = 0x0000_003E;

/// CP15's registers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Saved")]
pub struct Cp15 {
    control: u32,
    /// Cachable bits per protection region, for data and for instructions
    /// (c2, c0, 0–1).
    cachable: [u32; 2],
    /// Write-bufferable bits per data protection region (c3, c0, 0).
    bufferable: u32,
    /// Access permissions in the extended 4-bits-per-region form, for data
    /// and for instructions (c5, c0, 2–3; 0–1 are a 2-bit view of them).
    permissions: [u32; 2],
    /// Protection regions 0–7 (c6, c0–c7). The DS has one set for data
    /// and instructions; opcode 2 values 0 and 1 reach the same register.
    regions: [u32; 8],
    /// Cache lockdown for data and for instructions (c9, c0, 0–1).
    lockdown: [u32; 2],
    /// Data TCM base and virtual size (c9, c1, 0).
    dtcm_region: u32,
    /// Instruction TCM virtual size (c9, c1, 1).
    itcm_region: u32,
    /// Trace process ID (c13, c0, 1 and its mirror c13, c1, 1).
    trace_process_id: u32,
    /// Where the TCMs answer, derived from the registers above.
    itcm: Window,
    dtcm: Window,
    /// What each page allows, derived from the protection regions and
    /// permissions; current while the protection unit is on. Rebuilt
    /// rather than saved.
    #[serde(skip)]
    protection: Map,
}

/// CP15 as saved: every field but the protection map, in order.
#[derive(Deserialize)]
struct Saved {
    control: u32,
    cachable: [u32; 2],
    bufferable: u32,
    permissions: [u32; 2],
    regions: [u32; 8],
    lockdown: [u32; 2],
    dtcm_region: u32,
    itcm_region: u32,
    trace_process_id: u32,
    itcm: Window,
    dtcm: Window,
}

impl From<Saved> for Cp15 {
    fn from(saved: Saved) -> Self {
        let mut cp15 = Self {
            control: saved.control,
            cachable: saved.cachable,
            bufferable: saved.bufferable,
            permissions: saved.permissions,
            regions: saved.regions,
            lockdown: saved.lockdown,
            dtcm_region: saved.dtcm_region,
            itcm_region: saved.itcm_region,
            trace_process_id: saved.trace_process_id,
            itcm: saved.itcm,
            dtcm: saved.dtcm,
            protection: Map::default(),
        };
        cp15.map_protection();
        cp15
    }
}

impl Default for Cp15 {
    fn default() -> Self {
        let mut cp15 = Self {
            control: control::RESET,
            cachable: [0; 2],
            bufferable: 0,
            permissions: [0; 2],
            regions: [0; 8],
            lockdown: [0; 2],
            dtcm_region: 0,
            itcm_region: 0,
            trace_process_id: 0,
            itcm: Window::new(0, false, false),
            dtcm: Window::new(0, false, false),
            protection: Map::default(),
        };
        cp15.map_tcm();
        cp15
    }
}

impl Cp15 {
    /// MRC p15, 0, Rd, `crn`, `crm`, `opcode2`. Registers the ARM946E-S
    /// does not have read as 0.
    #[must_use]
    pub fn read(&self, crn: u32, crm: u32, opcode2: u32) -> u32 {
        match (crn, crm, opcode2) {
            (0, 0, 1) => CACHE_TYPE,
            (0, 0, 2) => TCM_SIZE,
            // The other ID registers repeat the main ID.
            (0, 0, _) => MAIN_ID,
            (1, 0, 0) => self.control,
            (2, 0, side @ 0..=1) => self.cachable[side as usize],
            (3, 0, 0) => self.bufferable,
            (5, 0, side @ 0..=1) => basic_permissions(self.permissions[side as usize]),
            (5, 0, side @ 2..=3) => self.permissions[side as usize - 2],
            (6, region @ 0..=7, 0..=1) => self.regions[region as usize],
            (9, 0, side @ 0..=1) => self.lockdown[side as usize],
            (9, 1, 0) => self.dtcm_region,
            (9, 1, 1) => self.itcm_region,
            (13, 0..=1, 1) => self.trace_process_id,
            _ => 0,
        }
    }

    /// MCR p15, 0, Rd, `crn`, `crm`, `opcode2`. Read-only registers,
    /// commands (c7) and registers the ARM946E-S does not have ignore the
    /// write; waiting for an interrupt is the CPU's business.
    pub fn write(&mut self, crn: u32, crm: u32, opcode2: u32, value: u32) {
        match (crn, crm, opcode2) {
            (1, 0, 0) => self.set_control(value),
            (2, 0, side @ 0..=1) => self.cachable[side as usize] = value & 0xFF,
            (3, 0, 0) => self.bufferable = value & 0xFF,
            (5, 0, side @ 0..=1) => {
                self.set_permissions(side as usize, extended_permissions(value));
            }
            (5, 0, side @ 2..=3) => self.set_permissions(side as usize - 2, value),
            (6, region @ 0..=7, 0..=1) => self.set_region(region as usize, value & REGION_MASK),
            (9, 0, side @ 0..=1) => self.lockdown[side as usize] = value,
            (9, 1, 0) => self.set_dtcm_region(value),
            (9, 1, 1) => self.set_itcm_region(value),
            (13, 0..=1, 1) => self.trace_process_id = value,
            _ => {}
        }
    }

    /// The control register (c1, c0, 0); see [`control`].
    #[must_use]
    pub fn control(&self) -> u32 {
        self.control
    }

    /// Writes the control register; fixed bits keep their values.
    pub fn set_control(&mut self, value: u32) {
        let was_protecting = self.protecting();
        self.control = (value & control::WRITABLE) | control::FIXED;
        self.map_tcm();
        if !was_protecting {
            self.map_protection();
        }
    }

    /// The data TCM region register (c9, c1, 0).
    #[must_use]
    pub fn dtcm_region(&self) -> u32 {
        self.dtcm_region
    }

    /// Moves the data TCM: base in bits 12–31, virtual size 512 << N in
    /// bits 1–5.
    pub fn set_dtcm_region(&mut self, value: u32) {
        self.dtcm_region = value & DTCM_REGION_MASK;
        self.map_tcm();
    }

    /// The instruction TCM region register (c9, c1, 1).
    #[must_use]
    pub fn itcm_region(&self) -> u32 {
        self.itcm_region
    }

    /// Sets the instruction TCM's virtual size (512 << N in bits 1–5); its
    /// base is fixed at 0 on the DS.
    pub fn set_itcm_region(&mut self, value: u32) {
        self.itcm_region = value & ITCM_REGION_MASK;
        self.map_tcm();
    }

    /// Protection region `index` (0–7): enable in bit 0, size 2 << N in
    /// bits 1–5, base in bits 12–31.
    #[must_use]
    pub fn protection_region(&self, index: usize) -> u32 {
        self.regions[index]
    }

    /// Where the exception vectors start.
    #[must_use]
    pub fn vector_base(&self) -> u32 {
        if self.control & control::HIGH_VECTORS != 0 {
            0xFFFF_0000
        } else {
            0
        }
    }

    /// Whether loads into R15 take the state from bit 0 (ARMv5), which
    /// [`control::PRE_V5`] turns off.
    #[must_use]
    pub fn loads_interwork(&self) -> bool {
        self.control & control::PRE_V5 == 0
    }

    /// Whether the protection unit lets an access of kind `need`
    /// (`protection::READ`, `WRITE` or `EXECUTE`) through, with User mode's
    /// permissions if `user` is set. Everything passes while it is off.
    #[inline]
    pub(crate) fn permits(&self, address: u32, need: u8, user: bool) -> bool {
        !self.protecting() || self.protection.allows(address, need, user)
    }

    fn protecting(&self) -> bool {
        self.control & control::PROTECTION_UNIT != 0
    }

    fn set_permissions(&mut self, side: usize, value: u32) {
        if self.permissions[side] != value {
            self.permissions[side] = value;
            self.map_protection();
        }
    }

    fn set_region(&mut self, index: usize, value: u32) {
        if self.regions[index] != value {
            self.regions[index] = value;
            self.map_protection();
        }
    }

    /// Rebuilds the protection map if the unit is on; turning it on
    /// rebuilds it too, so changes meanwhile wait until then.
    fn map_protection(&mut self) {
        if self.protecting() {
            let [data, code] = self.permissions;
            self.protection.rebuild(&self.regions, data, code);
        }
    }

    pub(crate) fn itcm(&self) -> Window {
        self.itcm
    }

    pub(crate) fn dtcm(&self) -> Window {
        self.dtcm
    }

    fn map_tcm(&mut self) {
        let on = |bit: u32| self.control & bit != 0;
        self.itcm = Window::new(
            self.itcm_region,
            on(control::ITCM_ENABLE),
            on(control::ITCM_LOAD),
        );
        self.dtcm = Window::new(
            self.dtcm_region,
            on(control::DTCM_ENABLE),
            on(control::DTCM_LOAD),
        );
    }
}

/// The 2-bit-per-region view (c5, c0, 0–1) of extended permissions.
fn basic_permissions(extended: u32) -> u32 {
    (0..8).fold(0, |basic, region| {
        basic | ((extended >> (region * 4)) & 3) << (region * 2)
    })
}

/// Extended permissions from a 2-bit-per-region write; the upper two bits
/// of each region's field become 0.
fn extended_permissions(basic: u32) -> u32 {
    (0..8).fold(0, |extended, region| {
        extended | ((basic >> (region * 2)) & 3) << (region * 4)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_keeps_fixed_bits() {
        let mut cp15 = Cp15::default();
        assert_eq!(cp15.control(), 0x2078);
        cp15.write(1, 0, 0, 0xFFFF_FFFF);
        assert_eq!(cp15.read(1, 0, 0), 0x000F_F0FD);
        cp15.write(1, 0, 0, 0);
        assert_eq!(cp15.read(1, 0, 0), 0x78);
        assert_eq!(cp15.vector_base(), 0);
    }

    #[test]
    fn protection_regions_are_unified() {
        let mut cp15 = Cp15::default();
        cp15.write(6, 3, 1, 0x0800_0035);
        assert_eq!(cp15.read(6, 3, 0), 0x0800_0035);
        assert_eq!(cp15.protection_region(3), 0x0800_0035);
        // Bits 6–11 are reserved.
        cp15.write(6, 0, 0, 0xFFFF_FFFF);
        assert_eq!(cp15.read(6, 0, 0), 0xFFFF_F03F);
    }

    #[test]
    fn basic_permissions_view_extended_ones() {
        let mut cp15 = Cp15::default();
        cp15.write(5, 0, 2, 0x6543_2106);
        // Regions 7 to 0 hold 6, 5, 4, 3, 2, 1, 0, 6; the view keeps their low bits.
        assert_eq!(cp15.read(5, 0, 0), 0b10_01_00_11_10_01_00_10);
        cp15.write(5, 0, 1, 0xFFFF);
        assert_eq!(cp15.read(5, 0, 3), 0x3333_3333);
    }

    #[test]
    fn protection_follows_the_registers_and_survives_loading() {
        use crate::protection::READ;
        let mut cp15 = Cp15::default();
        // 4 MiB at 0x02000000, read-write for privileged code only.
        cp15.write(6, 0, 0, 0x0200_002B);
        cp15.write(5, 0, 2, 1);
        assert!(cp15.permits(0x0300_0000, READ, true), "off: no checks");
        cp15.set_control(control::RESET | control::PROTECTION_UNIT);
        assert!(cp15.permits(0x023F_FFFC, READ, false));
        assert!(!cp15.permits(0x023F_FFFC, READ, true));
        assert!(!cp15.permits(0x0240_0000, READ, false));

        let saved = Saved {
            control: cp15.control,
            cachable: cp15.cachable,
            bufferable: cp15.bufferable,
            permissions: cp15.permissions,
            regions: cp15.regions,
            lockdown: cp15.lockdown,
            dtcm_region: cp15.dtcm_region,
            itcm_region: cp15.itcm_region,
            trace_process_id: cp15.trace_process_id,
            itcm: cp15.itcm,
            dtcm: cp15.dtcm,
        };
        assert_eq!(Cp15::from(saved), cp15);
    }

    #[test]
    fn tcm_regions() {
        let mut cp15 = Cp15::default();
        cp15.write(9, 1, 0, 0x027C_000B);
        assert_eq!(cp15.read(9, 1, 0), 0x027C_000A);
        // The instruction TCM's base cannot move.
        cp15.write(9, 1, 1, 0x0100_0020);
        assert_eq!(cp15.read(9, 1, 1), 0x0000_0020);
        cp15.set_control(control::DTCM_ENABLE | control::ITCM_ENABLE);
        assert!(cp15.dtcm().readable(0x027C_3FFC));
        assert!(cp15.itcm().readable(0x01FF_FFFC));
    }

    #[test]
    fn trace_process_id_is_mirrored() {
        let mut cp15 = Cp15::default();
        cp15.write(13, 1, 1, 0x1234_5678);
        assert_eq!(cp15.read(13, 0, 1), 0x1234_5678);
        // The DS has no fast context switch; its process ID reads 0.
        cp15.write(13, 0, 0, 0xFE00_0000);
        assert_eq!(cp15.read(13, 0, 0), 0);
    }
}
