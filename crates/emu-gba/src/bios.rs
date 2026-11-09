//! A high-level emulation of the GBA BIOS (GBATEK, "BIOS Functions").
//!
//! No Nintendo code is used. The 16 KiB BIOS region holds a small vector
//! table written here: the IRQ vector runs a six-instruction dispatcher that
//! calls the game's handler at 0x03007FFC, as every GBA program expects;
//! the SWI vector leads to a handler that just returns. System calls never
//! reach that code:
//! the machine intercepts the CPU as it enters the SWI vector and runs the
//! function here in Rust instead.
//!
//! A call runs to its end in one step of the emulation thread, where the
//! real BIOS spends CPU time with interrupts still served. So no function
//! here loops on a number the game supplies without a bound, and each
//! bound is one the real function has: CpuSet's 21-bit count, BitUnPack's
//! 16-bit length, a decompressor's 24-bit length (each turn of its loop
//! produces at least a byte). The two without one of their own stop at a
//! limit set here: the affine functions' count at [`AFFINE_SETS_MAX`],
//! and Huffman's bitstream at a word for each byte it unpacks to (see
//! [`huffman`]). Addresses wrap around the 32-bit bus, as the CPU's own
//! do.

use std::f64::consts::PI;

use emu_arm::{Access, Bus as _, Cpu, Mode, psr};

use crate::bus::Bus;

/// The BIOS image: vector table and IRQ dispatcher.
pub(crate) fn image() -> Box<[u8]> {
    let mut image = vec![0u8; 0x4000];
    let mut put = |address: usize, instruction: u32| {
        image[address..address + 4].copy_from_slice(&instruction.to_le_bytes());
    };
    put(0x00, 0xEAFF_FFFE); // reset: B . (never used; boot skips the BIOS)
    put(0x04, 0xE1B0_F00E); // undefined: MOVS PC, LR
    put(0x08, 0xEA00_004C); // SWI: B 0x140 (calls are intercepted first)
    put(0x18, 0xEA00_0042); // IRQ: B 0x128
    put(0x1C, 0xE25E_F004); // FIQ: SUBS PC, LR, #4
    put(0x128, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    put(0x12C, 0xE3A0_0301); // MOV R0, #0x04000000
    put(0x130, 0xE28F_E000); // ADD LR, PC, #0
    put(0x134, 0xE510_F004); // LDR PC, [R0, #-4]  (handler at 0x03FFFFFC)
    put(0x138, 0xE8BD_500F); // LDMFD SP!, {R0-R3, R12, LR}
    put(0x13C, 0xE25E_F004); // SUBS PC, LR, #4
    // A SWI handler that reads the call number and returns. Calls are
    // handled in Rust before this runs, but its first instructions are what
    // the CPU prefetches after an IRQ returns, which BIOS reads then see.
    put(0x140, 0xE92D_5800); // STMFD SP!, {R11, R12, LR}
    put(0x144, 0xE55E_C002); // LDRB R12, [LR, #-2]
    put(0x148, 0xE8BD_5800); // LDMFD SP!, {R11, R12, LR}
    put(0x14C, 0xE1B0_F00E); // MOVS PC, LR
    image.into_boxed_slice()
}

/// What BIOS reads return from outside the BIOS just after boot (or
/// SoftReset, which ends the same way) and just after a system call: the
/// opcodes the real BIOS last fetched at those points (GBATEK, "BIOS
/// Memory Protection").
pub(crate) const OPEN_BUS_AFTER_BOOT: u32 = 0xE129_F000;
pub(crate) const OPEN_BUS_AFTER_SWI: u32 = 0xE3A0_2004;

/// The address the CPU's R15 holds once it has entered the SWI vector.
pub(crate) const SWI_ENTERED_PC: u32 = 0x08 + 8;

/// Cycles the real BIOS's SWI dispatcher (saving registers, finding the
/// function, restoring and returning) takes beyond this HLE's own entry
/// and return. The mGBA suite's BIOS timing tests measure 40 for Div,
/// Sqrt and ArcTan alike once each function's own cycles are counted.
const DISPATCH_CYCLES: u32 = 40;

/// Cycles the real IntrWait spends between the interrupt handler's return
/// and its own return, beyond this HLE's repeat of the call. The mGBA
/// suite's timer count-up tests, all 936 of them, put it at 35.
const INTR_WAIT_RETURN_CYCLES: u32 = 35;

/// Runs the system call the CPU just entered, then returns from it as the
/// BIOS's `MOVS PC, LR` would, taking as long as the real BIOS where that
/// is known.
pub(crate) fn call(cpu: &mut Cpu, bus: &mut Bus) {
    // The comment field of the SWI instruction picks the function: in
    // either state, the BIOS loads the byte two before the return address.
    let return_address = cpu.reg(14);
    let number = bus.read_8(return_address.wrapping_sub(2), Access::NONSEQUENTIAL);
    let r = |i| cpu.reg(i);
    let (r0, r1, r2, r3) = (r(0), r(1), r(2), r(3));
    // The function's own cycles, where they are modelled.
    let cycles = match number {
        0x00 => return soft_reset(cpu, bus),
        0x01 => {
            register_ram_reset(bus, r0);
            0
        }
        0x02 | 0x03 => {
            bus.halt();
            0
        }
        0x04 => {
            intr_wait(cpu, bus, r0 != 0, r1 as u16);
            return;
        }
        0x05 => return vblank_intr_wait(cpu, bus),
        0x06 => divide(cpu, r0 as i32, r1 as i32),
        0x07 => divide(cpu, r1 as i32, r0 as i32),
        0x08 => {
            cpu.set_reg(0, isqrt(r0));
            sqrt_cycles(r0)
        }
        0x09 => {
            let result = arc_tan(r0 as i32);
            cpu.set_reg(0, result.angle as i16 as u32);
            cpu.set_reg(1, result.a as u32);
            cpu.set_reg(3, result.b as u32);
            result.cycles
        }
        0x0A => {
            let (angle, a, cycles) = arc_tan2(r0 as i32, r1 as i32);
            cpu.set_reg(0, u32::from(angle));
            if let Some(a) = a {
                cpu.set_reg(1, a as u32);
            }
            cpu.set_reg(3, ARC_TAN2_R3);
            cycles
        }
        0x0B => {
            cpu_set(bus, r0, r1, r2);
            0
        }
        0x0C => {
            cpu_fast_set(bus, r0, r1, r2);
            0
        }
        0x0D => {
            cpu.set_reg(0, 0xBAAE_187F);
            0
        }
        0x0E => {
            bg_affine_set(bus, r0, r1, r2);
            0
        }
        0x0F => {
            obj_affine_set(bus, r0, r1, r2, r3);
            0
        }
        0x10 => {
            bit_unpack(bus, r0, r1, r2);
            0
        }
        0x11 | 0x12 => {
            lz77(bus, r0, r1, number == 0x12);
            0
        }
        0x13 => {
            huffman(bus, r0, r1);
            0
        }
        0x14 | 0x15 => {
            run_length(bus, r0, r1, number == 0x15);
            0
        }
        0x16 => {
            diff_unfilter(bus, r0, r1, false, false);
            0
        }
        0x17 => {
            diff_unfilter(bus, r0, r1, false, true);
            0
        }
        0x18 => {
            diff_unfilter(bus, r0, r1, true, true);
            0
        }
        0x19 => {
            let bias = if r0 != 0 { 0x200 } else { 0 };
            bus.write_16(0x0400_0088, bias, Access::NONSEQUENTIAL);
            0
        }
        0x1F => {
            cpu.set_reg(0, midi_key_to_frequency(bus, r0, r1 as u8, r2 as u8));
            0
        }
        // Sound driver, multiboot and the rest are not emulated.
        _ => 0,
    };
    bus.spend(DISPATCH_CYCLES + cycles);
    bus.set_bios_open_bus(OPEN_BUS_AFTER_SWI);
    cpu.return_from_exception(bus);
}

/// SoftReset (GBATEK, "BIOS Reset Functions"): clears the top 0x200 bytes
/// of IWRAM, sets the Supervisor, IRQ and System stacks and zeroes those
/// first two modes' R14 and saved PSRs, then restarts the game in System
/// mode, ARM state, with R0–R12 zeroed: at 0x02000000 if the byte at
/// 0x03007FFA is set, otherwise at 0x08000000. It never returns to the
/// caller, so the call's usual return is skipped.
fn soft_reset(cpu: &mut Cpu, bus: &mut Bus) {
    bus.forget_vblank_wait();
    let to_ram = bus.read_8(0x0300_7FFA, Access::NONSEQUENTIAL) != 0;
    for address in (0x0300_7E00..0x0300_8000).step_by(4) {
        bus.write_32(address, 0, Access::NONSEQUENTIAL);
    }
    for (mode, stack) in [(Mode::Supervisor, 0x0300_7FE0), (Mode::Irq, 0x0300_7FA0)] {
        cpu.set_cpsr(mode.bits() | psr::I | psr::F);
        cpu.set_reg(13, stack);
        cpu.set_reg(14, 0);
        cpu.set_saved_psr(0);
    }
    // System mode's R14 is left holding the entry point, as on the DS.
    let entry = if to_ram { 0x0200_0000 } else { 0x0800_0000 };
    cpu.set_cpsr(Mode::System.bits());
    for index in 0..13 {
        cpu.set_reg(index, 0);
    }
    cpu.set_reg(13, 0x0300_7F00);
    cpu.set_reg(14, entry);
    bus.set_bios_open_bus(OPEN_BUS_AFTER_BOOT);
    cpu.jump(bus, entry);
}

fn register_ram_reset(bus: &mut Bus, flags: u32) {
    let mut clear = |start: u32, length: u32| {
        for address in (start..start + length).step_by(4) {
            bus.write_32(address, 0, Access::NONSEQUENTIAL);
        }
    };
    if flags & 0x01 != 0 {
        clear(0x0200_0000, 0x4_0000);
    }
    if flags & 0x02 != 0 {
        // The top 0x200 bytes of IWRAM (stacks, IRQ vector) are kept.
        clear(0x0300_0000, 0x7E00);
    }
    if flags & 0x04 != 0 {
        clear(0x0500_0000, 0x400);
    }
    if flags & 0x08 != 0 {
        clear(0x0600_0000, 0x1_8000);
    }
    if flags & 0x10 != 0 {
        clear(0x0700_0000, 0x400);
    }
}

/// IntrWait: waits until one of `flags` is set in the BIOS interrupt flags
/// at 0x03007FF8 (which the game's IRQ handler sets), then clears it.
/// Returns whether the CPU halted to wait.
///
/// While waiting, the CPU halts and returns to the SWI instruction itself,
/// so the call runs again after each interrupt; R0 is zeroed so the repeat
/// does not discard flags the handler just set.
fn intr_wait(cpu: &mut Cpu, bus: &mut Bus, discard: bool, flags: u16) -> bool {
    const BIOS_FLAGS: u32 = 0x0300_7FF8;
    let mut current = bus.read_16(BIOS_FLAGS, Access::NONSEQUENTIAL);
    if discard {
        current &= !flags;
        bus.write_16(BIOS_FLAGS, current, Access::NONSEQUENTIAL);
    }
    bus.write_16(0x0400_0208, 1, Access::NONSEQUENTIAL);
    let waiting = current & flags == 0;
    if waiting {
        cpu.set_reg(0, 0);
        cpu.set_reg(14, swi_address(cpu));
        bus.halt();
    } else {
        bus.write_16(BIOS_FLAGS, current & !flags, Access::NONSEQUENTIAL);
        bus.spend(INTR_WAIT_RETURN_CYCLES);
    }
    bus.set_bios_open_bus(OPEN_BUS_AFTER_SWI);
    cpu.return_from_exception(bus);
    waiting
}

/// VBlankIntrWait: IntrWait for VBlank that always discards old flags. It
/// ignores R0, so the bus remembers the call that halted instead, and
/// that call's repeat keeps the flag the handler just set.
fn vblank_intr_wait(cpu: &mut Cpu, bus: &mut Bus) {
    let swi = swi_address(cpu);
    let repeat = bus.resumes_vblank_wait(swi);
    if intr_wait(cpu, bus, !repeat, 1) {
        bus.remember_vblank_wait(swi);
    }
}

/// The address of the SWI instruction the CPU is handling.
fn swi_address(cpu: &Cpu) -> u32 {
    let thumb = cpu.saved_psr().is_some_and(|spsr| spsr & (1 << 5) != 0);
    cpu.reg(14).wrapping_sub(if thumb { 2 } else { 4 })
}

/// Div: R0 = quotient, R1 = remainder, R3 = |quotient|. Returns the
/// cycles the BIOS's shift-and-subtract loop takes: 13 for each bit the
/// numerator's magnitude is wider than the denominator's (at least one),
/// plus 11 (mGBA's cycle model, which the mGBA suite's timing tests
/// confirm).
fn divide(cpu: &mut Cpu, numerator: i32, denominator: i32) -> u32 {
    let loops = denominator
        .unsigned_abs()
        .leading_zeros()
        .saturating_sub(numerator.unsigned_abs().leading_zeros())
        .max(1);
    if denominator == 0 {
        // The real BIOS loops forever; return something harmless instead.
        cpu.set_reg(0, if numerator < 0 { u32::MAX } else { 1 });
        cpu.set_reg(1, numerator as u32);
        cpu.set_reg(3, 1);
    } else {
        let quotient = numerator.wrapping_div(denominator);
        cpu.set_reg(0, quotient as u32);
        cpu.set_reg(1, numerator.wrapping_rem(denominator) as u32);
        cpu.set_reg(3, quotient.unsigned_abs());
    }
    11 + 13 * loops
}

/// The cycles the BIOS's Sqrt takes for `value`: it narrows a bound on
/// the root by repeated long division, and each step of each loop costs a
/// fixed number of cycles. This replays its loops (mGBA's cycle model,
/// MPL-2.0, `src/gba/bios.c`; the mGBA suite's timing tests confirm it).
fn sqrt_cycles(value: u32) -> u32 {
    if value == 0 {
        return 53;
    }
    let mut cycles = 15;
    let (mut upper, mut bound) = (value, 1u32);
    while bound < upper {
        upper >>= 1;
        bound <<= 1;
        cycles += 6;
    }
    loop {
        cycles += 6;
        upper = value;
        let mut accumulator = 0u32;
        let mut lower = bound;
        loop {
            cycles += 5;
            let old = lower;
            if lower <= upper >> 1 {
                lower <<= 1;
            }
            if old >= upper >> 1 {
                break;
            }
        }
        loop {
            cycles += 8;
            accumulator <<= 1;
            if upper >= lower {
                accumulator += 1;
                upper -= lower;
            }
            if lower == bound {
                break;
            }
            lower >>= 1;
        }
        let old = bound;
        bound = (bound + accumulator) >> 1;
        if bound >= old {
            return cycles;
        }
    }
}

/// The cycles a MUL takes with `multiplier` as Rs, beyond its first:
/// one per byte of significant multiplier bits.
fn multiply_cycles(multiplier: i32) -> u32 {
    let significant = |mask: u32| {
        let top = multiplier as u32 & mask;
        top == 0 || top == mask
    };
    if significant(0xFFFF_FF00) {
        1
    } else if significant(0xFFFF_0000) {
        2
    } else if significant(0xFF00_0000) {
        3
    } else {
        4
    }
}

fn isqrt(value: u32) -> u32 {
    let mut root = (f64::from(value)).sqrt() as u32;
    while u64::from(root) * u64::from(root) > u64::from(value) {
        root -= 1;
    }
    while u64::from(root + 1) * u64::from(root + 1) <= u64::from(value) {
        root += 1;
    }
    root
}

/// ArcTan's result and the intermediate values the BIOS leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArcTan {
    /// The angle, of which R0 gets the low 16 bits sign-extended.
    angle: i32,
    /// −tan² in 1.14 fixed point, left in R1.
    a: i32,
    /// The polynomial's last term, left in R3.
    b: i32,
    /// The cycles the BIOS takes: 37 plus each multiply's (mGBA's cycle
    /// model, which the mGBA suite's timing tests confirm).
    cycles: u32,
}

/// ArcTan: the BIOS's polynomial approximation, with `tan` in 1.14 fixed
/// point, returning an angle where 0x4000 is 90 degrees. Every step is the
/// BIOS's 32-bit arithmetic, overflow included, so inputs outside
/// −1.0..1.0 give the hardware's garbage too.
fn arc_tan(tan: i32) -> ArcTan {
    let square = tan.wrapping_mul(tan);
    let a = (square >> 14).wrapping_neg();
    let mut product = 0xA9_i32.wrapping_mul(a);
    let mut cycles = 37 + multiply_cycles(square) + multiply_cycles(product);
    let mut b = (product >> 14).wrapping_add(0x390);
    for constant in [0x91C, 0xFB6, 0x16AA, 0x2081, 0x3651, 0xA2F9] {
        product = b.wrapping_mul(a);
        cycles += multiply_cycles(product);
        b = (product >> 14).wrapping_add(constant);
    }
    ArcTan {
        angle: tan.wrapping_mul(b) >> 16,
        a,
        b,
        cycles,
    }
}

/// What ArcTan2 leaves in R3.
const ARC_TAN2_R3: u32 = 0x170;

/// ArcTan2: the angle of (x, y) in 0..0x10000, from 32-bit coordinates.
/// Also returns ArcTan's R1 value when it calls ArcTan, which it does
/// unless the point is on an axis, and the cycles taken (11 on an axis,
/// otherwise ArcTan's).
fn arc_tan2(x: i32, y: i32) -> (u16, Option<i32>, u32) {
    let ratio = |numerator: i32, denominator: i32| (numerator << 14).wrapping_div(denominator);
    let (angle, a, cycles) = if y == 0 {
        (if x >= 0 { 0 } else { 0x8000 }, None, 11)
    } else if x == 0 {
        (if y >= 0 { 0x4000 } else { 0xC000 }, None, 11)
    } else {
        let (base, flip, tan) = if y >= 0 {
            if x >= 0 && x >= y {
                (0, false, ratio(y, x))
            } else if x < 0 && x.wrapping_neg() >= y {
                (0x8000, false, ratio(y, x))
            } else {
                (0x4000, true, ratio(x, y))
            }
        } else if x <= 0 && x.wrapping_neg() > y.wrapping_neg() {
            (0x8000, false, ratio(y, x))
        } else if x > 0 && x >= y.wrapping_neg() {
            (0x1_0000, false, ratio(y, x))
        } else {
            (0xC000, true, ratio(x, y))
        };
        // The BIOS keeps ArcTan's result to 16 bits before adding.
        let result = arc_tan(tan);
        let angle = i32::from(result.angle as i16);
        let angle = if flip { base - angle } else { base + angle };
        (angle, Some(result.a), result.cycles)
    };
    (angle as u16, a, cycles)
}

/// CpuSet: copies or fills `control & 0x1FFFFF` halfwords or words.
///
/// The BIOS moves words with LDMIA and STMIA and halfwords with LDRH and
/// STRH, and never aligns the addresses it is given, so misaligned ones
/// behave as those instructions do: word accesses ignore the low bits
/// (except on SRAM's 8-bit bus, which sees the address as it is), and a
/// halfword load from an odd address keeps only the high byte.
fn cpu_set(bus: &mut Bus, mut source: u32, mut destination: u32, control: u32) {
    if reads_protected_memory(source) {
        return;
    }
    let count = control & 0x1F_FFFF;
    let fill = control & (1 << 24) != 0;
    let words = control & (1 << 26) != 0;
    let step = if words { 4 } else { 2 };
    let load = |bus: &mut Bus, address: u32, access: Access| {
        if words {
            bus.read_32(address, access)
        } else {
            u32::from(load_halfword(bus, address, access))
        }
    };
    let fill_value = load(bus, source, Access::NONSEQUENTIAL);
    for _ in 0..count {
        let value = if fill {
            fill_value
        } else {
            load(bus, source, Access::SEQUENTIAL)
        };
        if words {
            bus.write_32(destination, value, Access::SEQUENTIAL);
        } else {
            bus.write_16(destination, value as u16, Access::SEQUENTIAL);
        }
        source = source.wrapping_add(step);
        destination = destination.wrapping_add(step);
    }
}

/// Whether CpuSet and CpuFastSet refuse `source`: the BIOS will not copy
/// from addresses whose bits 25–27 are clear, which keeps its own code
/// (and the unmapped space after it) from being read out.
fn reads_protected_memory(source: u32) -> bool {
    source & 0x0E00_0000 == 0
}

/// Loads a halfword as LDRH does: from an odd address the ARM7TDMI
/// rotates the aligned halfword right by 8, which leaves its high byte.
fn load_halfword(bus: &mut Bus, address: u32, access: Access) -> u16 {
    let value = bus.read_16(address, access);
    if address & 1 == 0 { value } else { value >> 8 }
}

/// CpuFastSet: word copies or fills in blocks of eight words, moved with
/// LDMIA and STMIA like CpuSet's words (addresses are not aligned).
fn cpu_fast_set(bus: &mut Bus, mut source: u32, mut destination: u32, control: u32) {
    if reads_protected_memory(source) {
        return;
    }
    let count = (control & 0x1F_FFFF).div_ceil(8) * 8;
    let fill = control & (1 << 24) != 0;
    let fill_value = bus.read_32(source, Access::NONSEQUENTIAL);
    for _ in 0..count {
        let value = if fill {
            fill_value
        } else {
            let value = bus.read_32(source, Access::SEQUENTIAL);
            source = source.wrapping_add(4);
            value
        };
        bus.write_32(destination, value, Access::SEQUENTIAL);
        destination = destination.wrapping_add(4);
    }
}

fn read_i16(bus: &mut Bus, address: u32) -> f64 {
    f64::from(bus.read_16(address, Access::NONSEQUENTIAL) as i16)
}

/// The most sets BgAffineSet and ObjAffineSet build in one call: more than
/// all the memory a GBA can write holds, a set being 16 bytes or 8 and
/// the largest memory, EWRAM, 256 KiB. The real BIOS has no limit but its
/// 31-bit count, which is hours of CPU time.
const AFFINE_SETS_MAX: u32 = 0x1_0000;

/// How many sets an affine function builds for the count in R2. The real
/// BIOS counts R2 down as a signed number and stops once it is below
/// zero, so a count with bit 31 set builds nothing.
fn affine_sets(count: u32) -> u32 {
    if count > i32::MAX as u32 {
        0
    } else {
        count.min(AFFINE_SETS_MAX)
    }
}

/// BgAffineSet: builds background rotation/scaling parameters.
fn bg_affine_set(bus: &mut Bus, mut source: u32, mut destination: u32, count: u32) {
    for _ in 0..affine_sets(count) {
        let at = |offset: u32| source.wrapping_add(offset);
        let origin_x = f64::from(bus.read_32(source, Access::NONSEQUENTIAL) as i32) / 256.0;
        let origin_y = f64::from(bus.read_32(at(4), Access::NONSEQUENTIAL) as i32) / 256.0;
        let center_x = read_i16(bus, at(8));
        let center_y = read_i16(bus, at(10));
        let scale_x = read_i16(bus, at(12)) / 256.0;
        let scale_y = read_i16(bus, at(14)) / 256.0;
        let angle = f64::from(bus.read_16(at(16), Access::NONSEQUENTIAL) >> 8) / 128.0 * PI;
        let (sin, cos) = angle.sin_cos();
        let (pa, pb, pc, pd) = (cos * scale_x, -sin * scale_x, sin * scale_y, cos * scale_y);
        let x = origin_x - (pa * center_x + pb * center_y);
        let y = origin_y - (pc * center_x + pd * center_y);
        for (offset, value) in [(0, pa), (2, pb), (4, pc), (6, pd)] {
            bus.write_16(
                destination.wrapping_add(offset),
                (value * 256.0) as i16 as u16,
                Access::NONSEQUENTIAL,
            );
        }
        bus.write_32(
            destination.wrapping_add(8),
            (x * 256.0) as i32 as u32,
            Access::NONSEQUENTIAL,
        );
        bus.write_32(
            destination.wrapping_add(12),
            (y * 256.0) as i32 as u32,
            Access::NONSEQUENTIAL,
        );
        source = source.wrapping_add(20);
        destination = destination.wrapping_add(16);
    }
}

/// ObjAffineSet: builds object rotation/scaling parameters, writing each
/// of the four values `stride` bytes apart.
fn obj_affine_set(bus: &mut Bus, mut source: u32, mut destination: u32, count: u32, stride: u32) {
    for _ in 0..affine_sets(count) {
        let scale_x = read_i16(bus, source) / 256.0;
        let scale_y = read_i16(bus, source.wrapping_add(2)) / 256.0;
        let angle =
            f64::from(bus.read_16(source.wrapping_add(4), Access::NONSEQUENTIAL) >> 8) / 128.0 * PI;
        let (sin, cos) = angle.sin_cos();
        for value in [cos * scale_x, -sin * scale_x, sin * scale_y, cos * scale_y] {
            bus.write_16(
                destination,
                (value * 256.0) as i16 as u16,
                Access::NONSEQUENTIAL,
            );
            destination = destination.wrapping_add(stride);
        }
        source = source.wrapping_add(8);
    }
}

/// BitUnPack: widens packed pixels, adding an offset to non-zero (or all)
/// values.
fn bit_unpack(bus: &mut Bus, mut source: u32, mut destination: u32, info: u32) {
    let length = u32::from(bus.read_16(info, Access::NONSEQUENTIAL));
    let source_width = u32::from(bus.read_8(info.wrapping_add(2), Access::NONSEQUENTIAL));
    let destination_width = u32::from(bus.read_8(info.wrapping_add(3), Access::NONSEQUENTIAL));
    let offset_field = bus.read_32(info.wrapping_add(4), Access::NONSEQUENTIAL);
    let offset = offset_field & 0x7FFF_FFFF;
    let offset_zero = offset_field >> 31 != 0;
    if !matches!(source_width, 1 | 2 | 4 | 8)
        || !matches!(destination_width, 1 | 2 | 4 | 8 | 16 | 32)
    {
        return;
    }
    let mut out = 0u32;
    let mut out_bits = 0;
    for _ in 0..length {
        let byte = u32::from(bus.read_8(source, Access::NONSEQUENTIAL));
        source = source.wrapping_add(1);
        for shift in (0..8).step_by(source_width as usize) {
            let mut value = (byte >> shift) & ((1 << source_width) - 1);
            if value != 0 || offset_zero {
                value = value.wrapping_add(offset);
            }
            out |= value.checked_shl(out_bits).unwrap_or(0);
            out_bits += destination_width;
            if out_bits >= 32 {
                bus.write_32(destination, out, Access::NONSEQUENTIAL);
                destination = destination.wrapping_add(4);
                out = 0;
                out_bits = 0;
            }
        }
    }
}

/// Writes decompressed bytes, either byte by byte (WRAM functions) or
/// a halfword at a time (VRAM functions, which cannot write single bytes).
struct Output {
    destination: u32,
    halfwords: bool,
    pending: Option<u8>,
}

impl Output {
    fn push(&mut self, bus: &mut Bus, byte: u8) {
        if !self.halfwords {
            bus.write_8(self.destination, byte, Access::NONSEQUENTIAL);
            self.destination = self.destination.wrapping_add(1);
            return;
        }
        match self.pending.take() {
            None => self.pending = Some(byte),
            Some(low) => {
                bus.write_16(
                    self.destination,
                    u16::from_le_bytes([low, byte]),
                    Access::NONSEQUENTIAL,
                );
                self.destination = self.destination.wrapping_add(2);
            }
        }
    }

    /// The byte written `distance` bytes back, for LZ77 back-references.
    fn earlier(&self, bus: &mut Bus, distance: u32) -> u8 {
        let written = self
            .destination
            .wrapping_add(u32::from(self.pending.is_some()));
        let address = written.wrapping_sub(distance);
        if self.halfwords && self.pending.is_some() && distance == 1 {
            return self.pending.unwrap_or(0);
        }
        bus.read_8(address, Access::NONSEQUENTIAL)
    }
}

/// LZ77UnComp: GBATEK's LZ77 format with a 4-byte header.
fn lz77(bus: &mut Bus, mut source: u32, destination: u32, halfwords: bool) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source = source.wrapping_add(4);
    let mut out = Output {
        destination,
        halfwords,
        pending: None,
    };
    while remaining > 0 {
        let flags = bus.read_8(source, Access::NONSEQUENTIAL);
        source = source.wrapping_add(1);
        for bit in (0..8).rev() {
            if remaining == 0 {
                break;
            }
            if flags & (1 << bit) == 0 {
                let byte = bus.read_8(source, Access::NONSEQUENTIAL);
                source = source.wrapping_add(1);
                out.push(bus, byte);
                remaining -= 1;
            } else {
                let high = u32::from(bus.read_8(source, Access::NONSEQUENTIAL));
                let low = u32::from(bus.read_8(source.wrapping_add(1), Access::NONSEQUENTIAL));
                source = source.wrapping_add(2);
                let length = (high >> 4) + 3;
                let distance = ((high & 0xF) << 8 | low) + 1;
                for _ in 0..length.min(remaining) {
                    let byte = out.earlier(bus, distance);
                    out.push(bus, byte);
                    remaining -= 1;
                }
            }
        }
    }
}

/// RLUnComp: runs of repeated bytes and literal stretches.
fn run_length(bus: &mut Bus, mut source: u32, destination: u32, halfwords: bool) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source = source.wrapping_add(4);
    let mut out = Output {
        destination,
        halfwords,
        pending: None,
    };
    while remaining > 0 {
        let flag = bus.read_8(source, Access::NONSEQUENTIAL);
        source = source.wrapping_add(1);
        if flag & 0x80 == 0 {
            for _ in 0..(u32::from(flag & 0x7F) + 1).min(remaining) {
                let byte = bus.read_8(source, Access::NONSEQUENTIAL);
                source = source.wrapping_add(1);
                out.push(bus, byte);
                remaining -= 1;
            }
        } else {
            let byte = bus.read_8(source, Access::NONSEQUENTIAL);
            source = source.wrapping_add(1);
            for _ in 0..(u32::from(flag & 0x7F) + 3).min(remaining) {
                out.push(bus, byte);
                remaining -= 1;
            }
        }
    }
}

/// Bitstream words HuffUnComp follows beyond one for each byte it has to
/// write: room for the longest codes a tree can hold (256 bits) in data
/// of a few bytes.
const HUFFMAN_SLACK_WORDS: u32 = 64;

/// HuffUnComp: GBATEK's Huffman format with 4- or 8-bit symbols, the two
/// sizes that fill the 32-bit words it writes; data that names another
/// size is left alone.
///
/// A node's children lie after it, so a walk only moves on through the
/// tree. One that leaves the tree (the size byte says where it ends) can
/// reach no symbol the data holds, and ends the call.
///
/// The bitstream followed is at most a word for each byte to write, and
/// [`HUFFMAN_SLACK_WORDS`] more. That is four times what the bytes take
/// unpacked, and a Huffman code is at worst as long as fixed-width
/// symbols; without it, a stream of a tree's longest codes for the
/// longest length would keep one call going for minutes.
fn huffman(bus: &mut Bus, mut source: u32, mut destination: u32) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let symbol_bits = header & 0xF;
    if !matches!(symbol_bits, 4 | 8) {
        return;
    }
    let mut remaining = header >> 8;
    let tree_size = u32::from(bus.read_8(source.wrapping_add(4), Access::NONSEQUENTIAL));
    let root = source.wrapping_add(5);
    // The nodes' bytes, from the root to the bitstream.
    let tree_length = tree_size * 2 + 1;
    source = root.wrapping_add(tree_length);
    let mut node = root;
    let mut node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
    let mut out = 0u32;
    let mut out_bits = 0;
    let mut words = remaining + HUFFMAN_SLACK_WORDS;
    while remaining > 0 && words > 0 {
        words -= 1;
        let bits = bus.read_32(source, Access::NONSEQUENTIAL);
        source = source.wrapping_add(4);
        for bit in (0..32).rev() {
            let right = bits & (1 << bit) != 0;
            let next = (node & !1).wrapping_add((node_value & 0x3F) * 2 + 2 + u32::from(right));
            if next.wrapping_sub(root) >= tree_length {
                return;
            }
            let terminal = node_value & if right { 0x40 } else { 0x80 } != 0;
            node = next;
            node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
            if terminal {
                out |= node_value << out_bits;
                out_bits += symbol_bits;
                node = root;
                node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
                if out_bits >= 32 {
                    bus.write_32(destination, out, Access::NONSEQUENTIAL);
                    destination = destination.wrapping_add(4);
                    out = 0;
                    out_bits = 0;
                    remaining = remaining.saturating_sub(4);
                    if remaining == 0 {
                        return;
                    }
                }
            }
        }
    }
}

/// Diff8bitUnFilter and Diff16bitUnFilter: running sums.
fn diff_unfilter(
    bus: &mut Bus,
    mut source: u32,
    mut destination: u32,
    sixteen: bool,
    halfwords: bool,
) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source = source.wrapping_add(4);
    if sixteen {
        let mut sum = 0u16;
        while remaining >= 2 {
            sum = sum.wrapping_add(bus.read_16(source, Access::NONSEQUENTIAL));
            bus.write_16(destination, sum, Access::NONSEQUENTIAL);
            source = source.wrapping_add(2);
            destination = destination.wrapping_add(2);
            remaining -= 2;
        }
        return;
    }
    let mut out = Output {
        destination,
        halfwords,
        pending: None,
    };
    let mut sum = 0u8;
    while remaining > 0 {
        sum = sum.wrapping_add(bus.read_8(source, Access::NONSEQUENTIAL));
        out.push(bus, sum);
        source = source.wrapping_add(1);
        remaining -= 1;
    }
}

/// MidiKey2Freq: the sample rate that plays a wave at a MIDI key.
fn midi_key_to_frequency(bus: &mut Bus, wave: u32, key: u8, fine: u8) -> u32 {
    let base = f64::from(bus.read_32(wave.wrapping_add(4), Access::NONSEQUENTIAL));
    let exponent = (180.0 - f64::from(key) - f64::from(fine) / 256.0) / 12.0;
    (base / 2f64.powf(exponent)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ram_bus() -> Bus {
        let cartridge = emu_gba_cart::Cartridge::new(vec![0; 0x400].into(), None, 0);
        Bus::new(cartridge, image())
    }

    #[test]
    fn cpu_set_moves_misaligned_halfwords_as_ldrh_does() {
        let mut bus = ram_bus();
        bus.write_32(0x0200_0000, 0xDEAD_BEEF, Access::NONSEQUENTIAL);
        bus.write_32(0x0200_0004, 0x3333_3333, Access::NONSEQUENTIAL);
        cpu_set(&mut bus, 0x0200_0001, 0x0300_0000, 2);
        assert_eq!(bus.read_32(0x0300_0000, Access::NONSEQUENTIAL), 0x00DE_00BE);
    }

    #[test]
    fn cpu_set_and_cpu_fast_set_will_not_read_the_bios() {
        let mut bus = ram_bus();
        cpu_set(&mut bus, 0, 0x0300_0000, 1 << 26 | 4);
        cpu_fast_set(&mut bus, 0x10, 0x0300_0010, 8);
        for address in [0x0300_0000, 0x0300_0010] {
            assert_eq!(bus.read_32(address, Access::NONSEQUENTIAL), 0);
        }
    }

    /// Cycle counts the mGBA suite's BIOS timing tests imply, less the
    /// dispatcher's.
    #[test]
    fn math_functions_take_the_bioss_time() {
        let mut cpu = Cpu::default();
        assert_eq!(divide(&mut cpu, 0x1234_5678, 0xFF), 284);
        assert_eq!(divide(&mut cpu, 0xFF, 0x1234_5678), 24);
        assert_eq!(sqrt_cycles(0), 53);
        assert_eq!(sqrt_cycles(0xFF), 168);
        assert_eq!(sqrt_cycles(0x1234_5678), 1084);
        assert_eq!(arc_tan(0xFF).cycles, 53);
    }

    #[test]
    fn integer_square_roots() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(15), 3);
        assert_eq!(isqrt(16), 4);
        assert_eq!(isqrt(u32::MAX), 65_535);
    }

    #[test]
    fn arc_tan2_quadrants() {
        assert_eq!(arc_tan2(1, 0), (0, None, 11));
        assert_eq!(arc_tan2(0, 1), (0x4000, None, 11));
        assert_eq!(arc_tan2(-1, 0), (0x8000, None, 11));
        assert_eq!(arc_tan2(0, -1), (0xC000, None, 11));
        let (diagonal, _, _) = arc_tan2(100, 100);
        assert!((0x1FF0..=0x2010).contains(&diagonal), "{diagonal:x}");
    }

    /// Hardware results from the mGBA suite's BIOS math tests: R0, R1, R3.
    #[test]
    fn arc_tan_leaves_the_bioss_registers() {
        let registers = |tan: u32| {
            let result = arc_tan(tan as i32);
            (result.angle as i16 as u32, result.a as u32, result.b as u32)
        };
        assert_eq!(registers(0x0000), (0, 0, 0xA2F9));
        assert_eq!(registers(0x7FFF), (0x16D8, 0xFFFF_0004, 0x2_2DB6));
        assert_eq!(registers(0xC001), (0xFFFF_D550, 0x1_BFFA, 0x2_71BD));
        assert_eq!(registers(0xFFFF), (0xFFFF_A2FE, 8, 0xA2FF));
        assert_eq!(registers(0xFFFF_0000), (0x5D07, 0, 0xA2F9));
        assert_eq!(registers(0xFFFF_8001), (0xFFFF_E927, 0xFFFF_0004, 0x2_2DB6));
    }

    #[test]
    fn arc_tan2_takes_32_bit_coordinates() {
        let call = |x: u32, y: u32| {
            let (angle, a, _) = arc_tan2(x as i32, y as i32);
            (angle, a.map(|a| a as u32))
        };
        assert_eq!(call(0xFFFF_0000, 1), (0x8000, Some(0)));
        assert_eq!(call(0xC000, 0x4000), (0x0D1B, Some(0xFFFF_F8E4)));
        assert_eq!(call(0xFFFF_0001, 0xFFFF_0000), (0xA001, Some(0xFFFF_C002)));
        assert_eq!(call(0xFFFF_8000, 0x1_0000), (0x52E4, Some(0xFFFF_F000)));
        assert_eq!(call(0x4000, 0), (0, None));
    }

    /// A Huffman stream at `source` in EWRAM for `length` bytes of
    /// `symbol_bits`-bit symbols, with the codes 1 = "0", 2 = "10" and
    /// 3 = "11": the header, the size byte (8 bytes / 2 − 1), the root
    /// with symbol 1 as child 0 and a node as child 1, that node's two
    /// symbols, padding to a word, then `bits`.
    fn put_huffman(bus: &mut Bus, source: u32, symbol_bits: u32, length: u32, bits: u32) {
        let header = length << 8 | 0x20 | symbol_bits;
        bus.write_32(source, header, Access::NONSEQUENTIAL);
        for (offset, byte) in [0x03, 0x80, 0x01, 0xC0, 0x02, 0x03, 0, 0]
            .into_iter()
            .enumerate()
        {
            bus.write_8(source + 4 + offset as u32, byte, Access::NONSEQUENTIAL);
        }
        bus.write_32(source + 12, bits, Access::NONSEQUENTIAL);
    }

    /// 1, 2, 3, 1, 2, 3, ...: "0 10 11 0 10 11 ...".
    const HUFFMAN_BITS: u32 = 0b0101_1010_1101_0110_1011_0101_1010_1101;

    #[test]
    fn huffman_decodes_four_and_eight_bit_symbols() {
        // Four-bit symbols pack two to a byte, the first in the low nibble.
        for (symbol_bits, expected) in [(8, 0x0103_0201), (4, 0x2132_1321)] {
            let mut bus = ram_bus();
            put_huffman(&mut bus, 0x0200_0000, symbol_bits, 4, HUFFMAN_BITS);
            huffman(&mut bus, 0x0200_0000, 0x0200_0100);
            assert_eq!(
                bus.read_32(0x0200_0100, Access::NONSEQUENTIAL),
                expected,
                "{symbol_bits}-bit symbols"
            );
            assert_eq!(bus.read_32(0x0200_0104, Access::NONSEQUENTIAL), 0);
        }
    }

    /// Symbols of another size never fill a word exactly, so the function
    /// used to wait for ever for its count of bits to equal 32.
    #[test]
    fn huffman_leaves_other_symbol_sizes_alone() {
        for symbol_bits in [0, 1, 2, 3, 5, 6, 7, 9, 15] {
            let mut bus = ram_bus();
            put_huffman(&mut bus, 0x0200_0000, symbol_bits, 0xFF_FFFF, HUFFMAN_BITS);
            huffman(&mut bus, 0x0200_0000, 0x0200_0100);
            assert_eq!(bus.read_32(0x0200_0100, Access::NONSEQUENTIAL), 0);
        }
    }

    /// A tree none of whose nodes marks a child as a symbol: the walk
    /// runs off its end, where it used to go on through all of memory.
    #[test]
    fn huffman_stops_when_a_walk_leaves_the_tree() {
        let mut bus = ram_bus();
        put_huffman(&mut bus, 0x0200_0000, 8, 0xFF_FFFF, HUFFMAN_BITS);
        for offset in 5..12 {
            bus.write_8(0x0200_0000 + offset, 0, Access::NONSEQUENTIAL);
        }
        let before = bus.cycles();
        huffman(&mut bus, 0x0200_0000, 0x0200_0100);
        assert_eq!(bus.read_32(0x0200_0100, Access::NONSEQUENTIAL), 0);
        // Three nodes fit in the tree, so the walk is over in a few reads.
        assert!(bus.cycles() - before < 100);
    }

    /// A stream of the longest code a deep tree holds, for the longest
    /// length there is: the call ends once it has followed a word for
    /// each byte it was to write, where it used to follow two hundred
    /// times as many.
    #[test]
    fn huffman_follows_a_bitstream_no_longer_than_its_data_allows() {
        let mut bus = ram_bus();
        let source = 0x0200_0000;
        let length = 0x400;
        // A full tree: a chain of 255 nodes, each the 1 child of the one
        // before, the last with two symbols.
        bus.write_32(source, length << 8 | 0x28, Access::NONSEQUENTIAL);
        bus.write_8(source + 4, 0xFF, Access::NONSEQUENTIAL);
        bus.write_8(source + 4 + 509, 0xC0, Access::NONSEQUENTIAL);
        bus.write_8(source + 4 + 511, 0x5A, Access::NONSEQUENTIAL);
        for word in 0..0x1000 {
            bus.write_32(source + 516 + word * 4, 0xFFFF_FFFF, Access::NONSEQUENTIAL);
        }
        huffman(&mut bus, source, 0x0300_0000);
        // 255 bits a symbol, four symbols a word written.
        let words = (length + HUFFMAN_SLACK_WORDS) * 32 / (255 * 4);
        assert!(words < length / 4, "the stream runs out before the length");
        let written =
            |bus: &mut Bus, word: u32| bus.read_32(0x0300_0000 + word * 4, Access::NONSEQUENTIAL);
        assert_eq!(written(&mut bus, words - 1), 0x5A5A_5A5A);
        assert_eq!(written(&mut bus, words), 0);
    }

    /// The call a game makes, through the machine: data whose header
    /// names 5-bit symbols, which never returned.
    #[test]
    fn a_huffman_call_with_an_odd_symbol_size_returns() {
        let gba = run_to_first_swi(&[
            0xE28F_0008, // ADD R0, PC, #8  (the data below)
            0xE3A0_1402, // MOV R1, #0x02000000
            0xEF13_0000, // SWI 0x13  (HuffUnComp)
            0xEAFF_FFFE, // B .
            0x00FF_FF25, // header: Huffman, 5-bit symbols, 65,535 bytes
            0x0101_C001, // tree: size 1, the root, its two symbols
            0xFFFF_FFFF, // bitstream
        ]);
        assert_eq!(gba.cpu().mode(), Mode::System, "back in the game");
    }

    #[test]
    fn affine_functions_count_as_the_bios_does_and_stop_at_the_limit() {
        assert_eq!(affine_sets(0), 0);
        assert_eq!(affine_sets(32), 32);
        assert_eq!(affine_sets(0x7FFF_FFFF), AFFINE_SETS_MAX);
        assert_eq!(affine_sets(0x8000_0000), 0);
        assert_eq!(affine_sets(0xFFFF_FFFF), 0);

        // Scale 1, no rotation.
        let mut bus = ram_bus();
        bus.write_16(0x0200_0000, 0x100, Access::NONSEQUENTIAL);
        bus.write_16(0x0200_0002, 0x100, Access::NONSEQUENTIAL);
        obj_affine_set(&mut bus, 0x0200_0000, 0x0300_0000, 1, 2);
        assert_eq!(bus.read_32(0x0300_0000, Access::NONSEQUENTIAL), 0x0000_0100);
        assert_eq!(bus.read_32(0x0300_0004, Access::NONSEQUENTIAL), 0x0100_0000);

        // A negative count builds nothing; the largest ends. Both used to
        // run for billions of sets.
        let mut bus = ram_bus();
        bg_affine_set(&mut bus, 0x0200_0000, 0x0300_0000, 0xFFFF_FFFF);
        obj_affine_set(&mut bus, 0x0200_0000, 0x0300_0000, 0x8000_0000, 2);
        assert_eq!(bus.read_32(0x0300_0000, Access::NONSEQUENTIAL), 0);
        let before = bus.cycles();
        obj_affine_set(&mut bus, 0x0200_0000, 0x0300_0000, 0x7FFF_FFFF, 2);
        bg_affine_set(&mut bus, 0x0200_0000, 0x0300_0000, 0x7FFF_FFFF);
        assert!(bus.cycles() - before < u64::from(AFFINE_SETS_MAX) * 2 * 64);
    }

    /// Addresses run off the top of the bus and come round to its start,
    /// where they used to overflow.
    #[test]
    fn addresses_wrap_at_the_top_of_the_bus() {
        let mut bus = ram_bus();
        // Run-length data for five bytes: "z" five times.
        bus.write_32(0x0200_0000, 0x0000_0530, Access::NONSEQUENTIAL);
        bus.write_16(0x0200_0004, 0x7A82, Access::NONSEQUENTIAL);
        run_length(&mut bus, 0x0200_0000, 0xFFFF_FFFE, false);
        run_length(&mut bus, 0x0200_0000, 0xFFFF_FFFE, true);
        // LZ77 data for five bytes: "a", then four from distance 1.
        bus.write_32(0x0200_0010, 0x0000_0510, Access::NONSEQUENTIAL);
        bus.write_32(0x0200_0014, 0x0010_6140, Access::NONSEQUENTIAL);
        lz77(&mut bus, 0x0200_0010, 0xFFFF_FFFE, false);
        lz77(&mut bus, 0x0200_0010, 0xFFFF_FFFE, true);
        diff_unfilter(&mut bus, 0x0200_0000, 0xFFFF_FFFE, false, false);
        diff_unfilter(&mut bus, 0x0200_0000, 0xFFFF_FFFE, true, true);
        // The sources and the structures the functions read.
        bg_affine_set(&mut bus, 0xFFFF_FFF0, 0xFFFF_FFF8, 2);
        obj_affine_set(&mut bus, 0xFFFF_FFFC, 0xFFFF_FFFC, 2, 2);
        bit_unpack(&mut bus, 0xFFFF_FFFF, 0xFFFF_FFFC, 0xFFFF_FFFC);
        huffman(&mut bus, 0xFFFF_FFFC, 0xFFFF_FFFC);
        midi_key_to_frequency(&mut bus, 0xFFFF_FFFC, 60, 0);
    }

    /// Boots a cartridge running `program` (ARM words from 0x08000000) and
    /// runs it until its first system call has been handled.
    fn run_to_first_swi(program: &[u32]) -> crate::Gba {
        let mut rom = vec![0u8; 0x200];
        for (index, word) in program.iter().enumerate() {
            rom[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        rom[0xB2] = 0x96;
        let mut gba = crate::Gba::new(&emu_core::Media::new(&rom)).expect("valid rom");
        for _ in 0..100 {
            let cpu = gba.cpu();
            let entering = cpu.reg(15) == SWI_ENTERED_PC && cpu.mode() == Mode::Supervisor;
            gba.step();
            if entering {
                return gba;
            }
        }
        panic!("the program made no system call");
    }

    /// The registers SoftReset leaves, about to run from `entry`.
    fn assert_soft_reset_registers(cpu: &Cpu, entry: u32) {
        assert_eq!((cpu.mode(), cpu.thumb()), (Mode::System, false));
        assert_eq!(cpu.reg(15), entry + 8);
        let registers = cpu.registers();
        assert_eq!(&registers.r[..13], &[0; 13]);
        assert_eq!((registers.r[13], registers.r[14]), (0x0300_7F00, entry));
        assert_eq!(registers.svc, [0x0300_7FE0, 0]);
        assert_eq!(registers.irq, [0x0300_7FA0, 0]);
        // Saved PSRs: Supervisor's and IRQ's.
        assert_eq!((registers.spsr[1], registers.spsr[3]), (0, 0));
    }

    #[test]
    fn soft_reset_from_arm_restarts_where_the_flag_says() {
        let mut gba = run_to_first_swi(&[
            0xE3A0_0403, // MOV R0, #0x03000000
            0xE280_0C7F, // ADD R0, R0, #0x7F00
            0xE3A0_1001, // MOV R1, #1
            0xE5C0_10FA, // STRB R1, [R0, #0xFA]  (restart in EWRAM)
            0xE3A0_D402, // MOV SP, #0x02000000
            0xEF00_0000, // SWI 0x00  (SoftReset)
        ]);
        assert_soft_reset_registers(gba.cpu(), 0x0200_0000);
        let flag = gba.machine.bus.read_8(0x0300_7FFA, Access::NONSEQUENTIAL);
        assert_eq!(flag, 0);
    }

    #[test]
    fn soft_reset_from_thumb_restarts_the_rom_in_arm_state() {
        let gba = run_to_first_swi(&[
            0xE3A0_C00C, // MOV R12, #12
            0xE3A0_D402, // MOV SP, #0x02000000
            0xE28F_0001, // ADD R0, PC, #1
            0xE12F_FF10, // BX R0
            0x46C0_DF00, // (Thumb) SWI 0x00  (SoftReset); NOP
        ]);
        assert_soft_reset_registers(gba.cpu(), 0x0800_0000);
    }

    #[test]
    fn vblank_intr_wait_returns_once_per_frame() {
        use emu_core::{Core, Media};
        let mut rom = vec![0u8; 0x200];
        let mut put = |address: usize, program: &[u32]| {
            for (index, word) in program.iter().enumerate() {
                let at = address + index * 4;
                rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
        };
        put(
            0x000,
            &[
                0xE3A0_0301, // MOV R0, #0x04000000
                0xE3A0_1008, // MOV R1, #8
                0xE1C0_10B4, // STRH R1, [R0, #4]  (DISPSTAT: VBlank IRQ)
                0xE280_2C02, // ADD R2, R0, #0x200
                0xE3A0_1001, // MOV R1, #1
                0xE1C2_10B0, // STRH R1, [R2]  (IE: VBlank)
                0xE1C2_10B8, // STRH R1, [R2, #8]  (IME)
                0xE3A0_3403, // MOV R3, #0x03000000
                0xE283_3C7F, // ADD R3, R3, #0x7F00
                0xE283_30FC, // ADD R3, R3, #0xFC
                0xE3A0_4408, // MOV R4, #0x08000000
                0xE284_4C01, // ADD R4, R4, #0x100
                0xE583_4000, // STR R4, [R3]  (the IRQ handler)
                0xEF05_0000, // loop: SWI 0x05  (VBlankIntrWait)
                0xE285_5001, // ADD R5, R5, #1
                0xEAFF_FFFC, // B loop
            ],
        );
        // The handler acknowledges the IRQ and sets the BIOS flag, as every
        // game's does.
        put(
            0x100,
            &[
                0xE3A0_0301, // MOV R0, #0x04000000
                0xE280_0C02, // ADD R0, R0, #0x200
                0xE3A0_1001, // MOV R1, #1
                0xE1C0_10B2, // STRH R1, [R0, #2]  (IF)
                0xE3A0_2403, // MOV R2, #0x03000000
                0xE282_2C7F, // ADD R2, R2, #0x7F00
                0xE282_20F8, // ADD R2, R2, #0xF8
                0xE1D2_30B0, // LDRH R3, [R2]
                0xE383_3001, // ORR R3, R3, #1
                0xE1C2_30B0, // STRH R3, [R2]
                0xE12F_FF1E, // BX LR
            ],
        );
        rom[0xB2] = 0x96;
        let mut gba = crate::Gba::new(&Media::new(&rom)).expect("valid rom");
        for _ in 0..10 {
            gba.run_frame(&[], &mut Vec::new());
        }
        let frames_waited = gba.cpu().reg(5);
        assert!((9..=10).contains(&frames_waited), "{frames_waited}");
    }
}
