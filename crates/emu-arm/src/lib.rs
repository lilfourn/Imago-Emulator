//! ARM interpreter shared by the GBA and DS cores.
//!
//! Status: not started. This is Phase 3, Milestone 7 in `docs/ROADMAP.md`.
//!
//! Planned contents:
//!
//! - ARMv4T for the ARM7TDMI (GBA, DS ARM7): ARM and Thumb decoding, the
//!   barrel shifter, banked registers for each CPU mode, exceptions, and the
//!   pipeline's effect on the visible PC.
//! - ARMv5TE additions for the DS ARM946E-S: CLZ, saturating QADD/QSUB, DSP
//!   multiplies, BLX, LDRD/STRD, and a CP15 hook for the protection unit,
//!   TCM and cache control.
//!
//! The CPU reaches memory through a bus trait that each system implements,
//! so one interpreter serves the GBA, the DS ARM7 and the DS ARM9. Sharing
//! stops at the CPU: the GBA and DS cores share no system code.
