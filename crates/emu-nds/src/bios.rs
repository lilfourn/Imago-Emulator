//! A high-level emulation of the two DS BIOSes (GBATEK, "BIOS Functions",
//! DS columns).
//!
//! No Nintendo code is used. Each CPU gets a small vector table written
//! here. The ARM7's IRQ vector calls the handler at 0x0380FFFC, and the
//! ARM9's calls the one at the top of DTCM, which it finds through CP15.
//! System calls never run BIOS code: the machine catches the CPU entering
//! the SWI vector and runs [`run`] instead, then returns from the
//! exception.
//!
//! A call is a pure function of its register arguments and memory: it
//! touches memory through the [`Bus`] it is given (which, for the ARM9,
//! also reaches its TCM) and returns the register changes for the machine
//! to apply. The exceptions are the decompressors that read through the
//! game's callbacks (`callback.rs`): those return a game function for the
//! machine to run, and resume when it returns to [`ARM9_CALLBACK_RETURN`]
//! or [`ARM7_CALLBACK_RETURN`].
//!
//! A call runs to its end in one step of the emulation thread, where the
//! real BIOS spends CPU time with interrupts still served. So no function
//! here loops on a number the game supplies without a bound. CpuSet's is
//! its 21-bit count, and GetCRC16, whose length is a whole register,
//! stops at [`CRC16_LENGTH_MAX`]. BitUnPack, the Diff unfilters and the
//! decompressors are [`emu_arm::bios`]'s, the GBA's too, and that module
//! says what bounds each; the ReadByCallback functions do a unit's work
//! between two of the game's own calls. Addresses wrap around the 32-bit
//! bus, as the CPU's own do.

mod callback;
mod decompress;

use emu_arm::bios::{bit_unpack, diff_unfilter};
use emu_arm::{Access, Bus};

pub(crate) use callback::{Decompression, GuestCall, Next};

use crate::ipc::Side;
use crate::spi::firmware::crc16;
use callback::Format;
use decompress::{Decoder, Lz77, RunLength};

/// The ARM9 BIOS (4 KiB at 0xFFFF0000).
pub(crate) fn arm9_image() -> Box<[u8]> {
    let mut image = vec![0u8; 0x1000];
    let mut put = |address: usize, instruction: u32| {
        image[address..address + 4].copy_from_slice(&instruction.to_le_bytes());
    };
    put(0x00, 0xEAFF_FFFE); // reset: B .
    put(0x04, 0xE1B0_F00E); // undefined: MOVS PC, LR
    put(0x08, 0xE1B0_F00E); // SWI: MOVS PC, LR (calls are intercepted first)
    put(0x0C, 0xE25E_F004); // prefetch abort: SUBS PC, LR, #4
    put(0x10, 0xE25E_F008); // data abort: SUBS PC, LR, #8
    put(0x18, 0xEA00_0038); // IRQ: B 0x100
    put(0x1C, 0xE25E_F004); // FIQ: SUBS PC, LR, #4
    put(0x100, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    put(0x104, 0xEE19_0F11); // MRC P15, 0, R0, C9, C1, 0  (DTCM region)
    put(0x108, 0xE1A0_0620); // MOV R0, R0, LSR #12
    put(0x10C, 0xE1A0_0600); // MOV R0, R0, LSL #12  (DTCM base)
    put(0x110, 0xE280_0901); // ADD R0, R0, #0x4000
    put(0x114, 0xE28F_E000); // ADD LR, PC, #0
    put(0x118, 0xE510_F004); // LDR PC, [R0, #-4]  (handler at DTCM + 0x3FFC)
    put(0x11C, 0xE8BD_500F); // LDMFD SP!, {R0-R3, R12, LR}
    put(0x120, 0xE25E_F004); // SUBS PC, LR, #4
    put(CALLBACK_RETURN, 0xEAFF_FFFE); // B .  (see ARM9_CALLBACK_RETURN)
    image.into_boxed_slice()
}

/// The ARM7 BIOS (16 KiB at 0).
pub(crate) fn arm7_image() -> Box<[u8]> {
    let mut image = vec![0u8; 0x4000];
    let mut put = |address: usize, instruction: u32| {
        image[address..address + 4].copy_from_slice(&instruction.to_le_bytes());
    };
    put(0x00, 0xEAFF_FFFE); // reset: B .
    put(0x04, 0xE1B0_F00E); // undefined: MOVS PC, LR
    put(0x08, 0xE1B0_F00E); // SWI: MOVS PC, LR (calls are intercepted first)
    put(0x0C, 0xE25E_F004); // prefetch abort: SUBS PC, LR, #4
    put(0x10, 0xE25E_F008); // data abort: SUBS PC, LR, #8
    put(0x18, 0xEA00_0038); // IRQ: B 0x100
    put(0x1C, 0xE25E_F004); // FIQ: SUBS PC, LR, #4
    put(0x100, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    put(0x104, 0xE3A0_0301); // MOV R0, #0x04000000
    put(0x108, 0xE28F_E000); // ADD LR, PC, #0
    put(0x10C, 0xE510_F004); // LDR PC, [R0, #-4]  (0x03FFFFFC mirrors 0x0380FFFC)
    put(0x110, 0xE8BD_500F); // LDMFD SP!, {R0-R3, R12, LR}
    put(0x114, 0xE25E_F004); // SUBS PC, LR, #4
    put(CALLBACK_RETURN, 0xEAFF_FFFE); // B .  (see ARM7_CALLBACK_RETURN)
    image.into_boxed_slice()
}

/// R15 once a CPU has entered its SWI vector (the vector plus 8).
pub(crate) const ARM9_SWI_ENTERED_PC: u32 = 0xFFFF_0008 + 8;
pub(crate) const ARM7_SWI_ENTERED_PC: u32 = 0x08 + 8;

/// Where a decompression callback returns to: the return address the
/// machine gives the game function. The machine catches the CPU arriving
/// there and resumes the BIOS call; the instruction there, a loop, never
/// runs.
const CALLBACK_RETURN: usize = 0x200;
pub(crate) const ARM9_CALLBACK_RETURN: u32 = 0xFFFF_0000 + CALLBACK_RETURN as u32;
pub(crate) const ARM7_CALLBACK_RETURN: u32 = CALLBACK_RETURN as u32;

/// The ARM9's CP15 control register after SoftReset: DTCM on, ITCM,
/// caches and protection unit off, high vectors (GBATEK).
pub(crate) const ARM9_RESET_CONTROL: u32 = 0x0001_2078;

/// A system call as the CPU made it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Call {
    pub(crate) side: Side,
    /// R0–R3.
    pub(crate) args: [u32; 4],
    /// Where the BIOS keeps its interrupt flags for IntrWait: DTCM + 0x3FF8
    /// on the ARM9, 0x0380FFF8 on the ARM7.
    pub(crate) intr_flags: u32,
    /// Whether this is the VBlankIntrWait call the CPU halted in, run
    /// again after an interrupt woke it.
    pub(crate) resumes: bool,
}

/// What a call does to the CPU, for the machine to apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Effects {
    /// Registers to set, by index.
    pub(crate) registers: Vec<(usize, u32)>,
    /// Halt until an interrupt.
    pub(crate) halt: bool,
    /// Run the SWI again after the interrupt that ends the halt (IntrWait).
    pub(crate) repeat: bool,
    /// Extra cycles the call takes (WaitByLoop), in the CPU's clock.
    pub(crate) stall: u64,
    /// SoftReset: restart the CPU at this address (Thumb if bit 0 is set).
    pub(crate) restart: Option<u32>,
    /// A decompression that reads through the game's callbacks, and the
    /// first game function it needs run.
    pub(crate) callback: Option<(Decompression, GuestCall)>,
}

/// The function number from the SWI instruction that trapped: bits 16–23
/// of an ARM SWI, the low byte of a Thumb one.
pub(crate) fn number<B: Bus>(bus: &mut B, return_address: u32, thumb: bool) -> u8 {
    if thumb {
        bus.read_16(return_address.wrapping_sub(2), Access::NONSEQUENTIAL) as u8
    } else {
        (bus.read_32(return_address.wrapping_sub(4), Access::NONSEQUENTIAL) >> 16) as u8
    }
}

/// Runs system call `number`.
pub(crate) fn run<B: Bus>(number: u8, call: &Call, bus: &mut B) -> Effects {
    let [r0, r1, r2, _] = call.args;
    let arm7 = call.side == Side::Arm7;
    let set = |registers: &[(usize, u32)]| Effects {
        registers: registers.to_vec(),
        ..Effects::default()
    };
    let by_callback = |format, bus: &mut B| Effects {
        callback: Some(Decompression::start(format, call.args, bus)),
        ..Effects::default()
    };
    match number {
        0x00 => soft_reset(bus, call),
        0x03 => Effects {
            // Each iteration of the BIOS loop takes four cycles.
            stall: u64::from(r0) * 4,
            ..Effects::default()
        },
        0x04 => intr_wait(bus, call.intr_flags, r0 != 0, r1),
        // VBlankIntrWait ignores R0, so the machine tells its repeat apart.
        0x05 => intr_wait(bus, call.intr_flags, !call.resumes, 1),
        0x06 => Effects {
            halt: true,
            ..Effects::default()
        },
        0x07 if arm7 => Effects {
            halt: true,
            ..Effects::default()
        },
        0x08 if arm7 => {
            bus.write_16(
                0x0400_0504,
                if r0 != 0 { 0x200 } else { 0 },
                Access::NONSEQUENTIAL,
            );
            Effects::default()
        }
        0x09 => set(&divide(r0 as i32, r1 as i32)),
        0x0B => {
            cpu_set(bus, r0, r1, r2);
            Effects::default()
        }
        0x0C => {
            cpu_fast_set(bus, r0, r1, r2);
            Effects::default()
        }
        0x0D => set(&[(0, (r0).isqrt())]),
        0x0E => set(&crc16_call(bus, r0 as u16, r1, r2)),
        0x0F => set(&[(0, 0)]),
        0x10 => {
            bit_unpack(bus, r0, r1, r2);
            Effects::default()
        }
        0x11 => {
            decompress::read_normal(bus, r0, r1, Decoder::Lz77(Lz77::default()));
            Effects::default()
        }
        0x12 => by_callback(Format::Lz77, bus),
        0x13 => by_callback(Format::Huffman, bus),
        0x14 => {
            decompress::read_normal(bus, r0, r1, Decoder::RunLength(RunLength::default()));
            Effects::default()
        }
        0x15 => by_callback(Format::RunLength, bus),
        // Diff8bitUnFilterWrite8bit and Diff16bitUnFilter: the DS has no
        // function that writes bytes two at a time.
        0x16 if !arm7 => {
            diff_unfilter(bus, r0, r1, false, false);
            Effects::default()
        }
        0x18 if !arm7 => {
            diff_unfilter(bus, r0, r1, true, false);
            Effects::default()
        }
        0x1A if arm7 => set(&[(0, sine_table(r0))]),
        0x1B if arm7 => set(&[(0, pitch_table(r0))]),
        0x1C if arm7 => set(&[(0, volume_table(r0))]),
        // ARM9 CustomPost: write POSTFLG.
        0x1F if !arm7 => {
            bus.write_32(0x0400_0300, r0, Access::NONSEQUENTIAL);
            Effects::default()
        }
        // ARM7 CustomHalt: write HALTCNT.
        0x1F => {
            bus.write_8(0x0400_0301, r2 as u8, Access::NONSEQUENTIAL);
            Effects::default()
        }
        // GetBootProcs (ARM7 1Dh) is not emulated, and invalid numbers
        // (which jump to 0 on hardware) do nothing.
        _ => Effects::default(),
    }
}

/// SoftReset (GBATEK, "BIOS Reset Functions"): clears the BIOS's 0x200
/// bytes at the top of DTCM or ARM7 WRAM (the stacks, then the interrupt
/// flags and the IRQ handler's address in the last 8 bytes) and restarts
/// at the entry point in the header copy at 0x027FFE00. The machine sets
/// the registers. Only the calling CPU resets.
fn soft_reset<B: Bus>(bus: &mut B, call: &Call) -> Effects {
    let start = call.intr_flags.wrapping_sub(0x1F8);
    for offset in (0..0x200).step_by(4) {
        bus.write_32(start.wrapping_add(offset), 0, Access::NONSEQUENTIAL);
    }
    let entry = match call.side {
        Side::Arm9 => 0x027F_FE24,
        Side::Arm7 => 0x027F_FE34,
    };
    Effects {
        restart: Some(bus.read_32(entry, Access::NONSEQUENTIAL)),
        ..Effects::default()
    }
}

/// IntrWait: returns once one of `mask`'s interrupts has been flagged by
/// the game's handler at `flags_address`. While waiting, the CPU halts and
/// the call runs again after each interrupt; R0 is zeroed so the repeat
/// does not discard flags the handler just set.
fn intr_wait<B: Bus>(bus: &mut B, flags_address: u32, discard: bool, mask: u32) -> Effects {
    let mut current = bus.read_32(flags_address, Access::NONSEQUENTIAL);
    if discard {
        current &= !mask;
        bus.write_32(flags_address, current, Access::NONSEQUENTIAL);
    }
    bus.write_32(0x0400_0208, 1, Access::NONSEQUENTIAL);
    if current & mask != 0 {
        bus.write_32(flags_address, current & !mask, Access::NONSEQUENTIAL);
        return Effects::default();
    }
    Effects {
        registers: vec![(0, 0)],
        halt: true,
        repeat: true,
        ..Effects::default()
    }
}

/// Div: R0 / R1 into R0, the remainder into R1 and |quotient| into R3.
fn divide(numerator: i32, denominator: i32) -> [(usize, u32); 3] {
    if denominator == 0 {
        // The real BIOS loops forever; return something harmless instead.
        let quotient: i32 = if numerator < 0 { -1 } else { 1 };
        return [(0, quotient as u32), (1, numerator as u32), (3, 1)];
    }
    let quotient = numerator.wrapping_div(denominator);
    [
        (0, quotient as u32),
        (1, numerator.wrapping_rem(denominator) as u32),
        (3, quotient.unsigned_abs()),
    ]
}

/// The most bytes one GetCRC16 call sums: 32 MiB, the largest stretch of
/// memory a DS has to sum, a GBA cartridge's ROM. The real BIOS has no
/// limit but its 32-bit length, which is minutes of CPU time.
const CRC16_LENGTH_MAX: u32 = 0x200_0000;

/// GetCRC16 over `length` bytes (whole halfwords, [`CRC16_LENGTH_MAX`] at
/// most) at `address`. R3 gets the last halfword read.
fn crc16_call<B: Bus>(bus: &mut B, initial: u16, address: u32, length: u32) -> [(usize, u32); 2] {
    let mut crc = initial;
    let mut last = 0;
    for offset in (0..length.min(CRC16_LENGTH_MAX) & !1).step_by(2) {
        last = bus.read_16(address.wrapping_add(offset), Access::SEQUENTIAL);
        crc = crc16(crc, &last.to_le_bytes());
    }
    [(0, u32::from(crc)), (3, u32::from(last))]
}

/// CpuSet: copies or fills `control & 0x1FFFFF` halfwords or words.
fn cpu_set<B: Bus>(bus: &mut B, source: u32, destination: u32, control: u32) {
    let count = control & 0x1F_FFFF;
    let fill = control & (1 << 24) != 0;
    let words = control & (1 << 26) != 0;
    let step = if words { 4 } else { 2 };
    let (mut source, mut destination) = if words {
        (source & !3, destination & !3)
    } else {
        (source & !1, destination & !1)
    };
    let fill_value = if words {
        bus.read_32(source, Access::NONSEQUENTIAL)
    } else {
        u32::from(bus.read_16(source, Access::NONSEQUENTIAL))
    };
    for _ in 0..count {
        let value = if fill {
            fill_value
        } else if words {
            bus.read_32(source, Access::SEQUENTIAL)
        } else {
            u32::from(bus.read_16(source, Access::SEQUENTIAL))
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

/// CpuFastSet: word copies or fills in blocks of eight words.
fn cpu_fast_set<B: Bus>(bus: &mut B, source: u32, destination: u32, control: u32) {
    let count = (control & 0x1F_FFFF).div_ceil(8) * 8;
    let fill = control & (1 << 24) != 0;
    let (mut source, mut destination) = (source & !3, destination & !3);
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

/// GetSineTable: sin(index × 90° / 64) in 1.15 fixed point, index 0–63.
fn sine_table(index: u32) -> u32 {
    let angle = f64::from(index & 0x3F) * std::f64::consts::FRAC_PI_2 / 64.0;
    (angle.sin() * 32768.0).round() as u32
}

/// GetPitchTable: (2^(index / 768) − 1) × 0x10000, index 0–767, which the
/// sound driver uses to turn semitone fractions into timer values.
fn pitch_table(index: u32) -> u32 {
    let fraction = f64::from(index.min(767)) / 768.0;
    ((fraction.exp2() - 1.0) * 65536.0) as u32
}

/// GetVolumeTable: the 7-bit volume for attenuation `index`, 0–723, in
/// tenths of a decibel above −72.3 dB. The sound driver divides the result
/// by 16, 4 or 2 below −24, −12 and −6 dB, so each of those ranges is
/// scaled up to fill 7 bits. Derived from how the driver uses it, not from
/// the BIOS's own table, so rounding may differ by one.
fn volume_table(index: u32) -> u32 {
    let index = index.min(723);
    let shift = match index {
        0..483 => 3,
        483..603 => 2,
        603..663 => 1,
        _ => 0,
    };
    let decibels = (f64::from(index) - 723.0) / 10.0;
    let level = 127.0 * 10f64.powf(decibels / 20.0) * f64::from(1u32 << shift);
    (level.round() as u32).min(127)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 KiB of flat memory for exercising calls.
    pub(super) struct Ram(pub(super) Vec<u8>);

    impl Ram {
        pub(super) fn new() -> Self {
            Self(vec![0; 0x1_0000])
        }
    }

    impl Bus for Ram {
        fn read_32(&mut self, address: u32, _: Access) -> u32 {
            let i = (address & 0xFFFC) as usize;
            u32::from_le_bytes([self.0[i], self.0[i + 1], self.0[i + 2], self.0[i + 3]])
        }
        fn read_16(&mut self, address: u32, _: Access) -> u16 {
            let i = (address & 0xFFFE) as usize;
            u16::from_le_bytes([self.0[i], self.0[i + 1]])
        }
        fn read_8(&mut self, address: u32, _: Access) -> u8 {
            self.0[(address & 0xFFFF) as usize]
        }
        fn write_32(&mut self, address: u32, value: u32, _: Access) {
            let i = (address & 0xFFFC) as usize;
            self.0[i..i + 4].copy_from_slice(&value.to_le_bytes());
        }
        fn write_16(&mut self, address: u32, value: u16, _: Access) {
            let i = (address & 0xFFFE) as usize;
            self.0[i..i + 2].copy_from_slice(&value.to_le_bytes());
        }
        fn write_8(&mut self, address: u32, value: u8, _: Access) {
            self.0[(address & 0xFFFF) as usize] = value;
        }
        fn idle(&mut self) {}
    }

    fn call(side: Side, args: [u32; 4]) -> Call {
        Call {
            side,
            args,
            intr_flags: 0xFFF8,
            resumes: false,
        }
    }

    #[test]
    fn get_crc16_reads_halfwords_and_returns_the_last() {
        let mut ram = Ram::new();
        ram.0[0x100..0x10A].copy_from_slice(b"123456789\0");
        let effects = run(0x0E, &call(Side::Arm7, [0, 0x100, 9, 0]), &mut ram);
        // Nine bytes round down to four halfwords: "12345678", whose
        // CRC-16/ARC is 3C9Dh.
        assert_eq!(effects.registers, [(0, 0x3C9D), (3, 0x3837)]);
    }

    /// A bus that counts its reads and holds nothing, for calls that
    /// would read more than a test's RAM.
    #[derive(Default)]
    struct Counting {
        reads: u64,
    }

    impl Bus for Counting {
        fn read_32(&mut self, _: u32, _: Access) -> u32 {
            self.reads += 1;
            0
        }
        fn read_16(&mut self, _: u32, _: Access) -> u16 {
            self.reads += 1;
            0
        }
        fn read_8(&mut self, _: u32, _: Access) -> u8 {
            self.reads += 1;
            0
        }
        fn write_32(&mut self, _: u32, _: u32, _: Access) {}
        fn write_16(&mut self, _: u32, _: u16, _: Access) {}
        fn write_8(&mut self, _: u32, _: u8, _: Access) {}
        fn idle(&mut self) {}
    }

    /// A length of 4 GiB used to read two billion halfwords in one step.
    #[test]
    fn get_crc16_stops_at_the_longest_memory_there_is() {
        let mut bus = Counting::default();
        run(
            0x0E,
            &call(Side::Arm9, [0, 0x0200_0000, 0xFFFF_FFFF, 0]),
            &mut bus,
        );
        // 32 MiB in halfwords. The SWI's own number is not read here:
        // `run` is given it.
        assert_eq!(bus.reads, 0x100_0000);
        let mut bus = Counting::default();
        run(
            0x0E,
            &call(Side::Arm7, [0, 0x0200_0000, 0x1000, 0]),
            &mut bus,
        );
        assert_eq!(bus.reads, 0x800, "a length within the limit is read whole");
    }

    /// Addresses run off the top of the bus and come round to its start,
    /// where they used to overflow. The test's RAM repeats every 64 KiB,
    /// so the top of the bus is the top of the RAM.
    #[test]
    fn addresses_wrap_at_the_top_of_the_bus() {
        // BitUnPack: its structure in the last word (two bytes, 4 bits to
        // 8) with the offset in the first; the two bytes of pixels start
        // at the last byte; the result goes over the structure.
        let mut ram = Ram::new();
        ram.0[0xFFFC..].copy_from_slice(&[2, 0, 4, 8]);
        run(
            0x10,
            &call(Side::Arm9, [0xFFFF_FFFF, 0xFFFF_FFFC, 0xFFFF_FFFC, 0]),
            &mut ram,
        );
        // The pixels 8 and 0 of the byte 0x08, then 0 and 0.
        assert_eq!(&ram.0[0xFFFC..], &[8, 0, 0, 0]);

        // Diff8bitUnFilter: the header in the last word, three bytes of 1
        // from the start, summed into the last two bytes and the first.
        let mut ram = Ram::new();
        ram.0[0xFFFC..].copy_from_slice(&[0x81, 3, 0, 0]);
        ram.0[..3].copy_from_slice(&[1, 1, 1]);
        run(
            0x16,
            &call(Side::Arm9, [0xFFFF_FFFC, 0xFFFF_FFFE, 0, 0]),
            &mut ram,
        );
        assert_eq!((ram.0[0xFFFE], ram.0[0xFFFF], ram.0[0]), (1, 2, 3));

        // Diff16bitUnFilter: two halfwords of 1, summed into the last
        // halfword and the first.
        let mut ram = Ram::new();
        ram.0[0xFFFC..].copy_from_slice(&[0x82, 4, 0, 0]);
        ram.0[..4].copy_from_slice(&[1, 0, 1, 0]);
        run(
            0x18,
            &call(Side::Arm9, [0xFFFF_FFFC, 0xFFFF_FFFE, 0, 0]),
            &mut ram,
        );
        assert_eq!((ram.0[0xFFFE], ram.0[0]), (1, 2));
    }

    #[test]
    fn intr_wait_halts_until_the_flag_is_set() {
        let mut ram = Ram::new();
        let effects = run(0x05, &call(Side::Arm7, [0; 4]), &mut ram);
        assert!(effects.halt && effects.repeat);
        ram.write_32(0xFFF8, 1, Access::NONSEQUENTIAL);
        let effects = run(0x04, &call(Side::Arm7, [0, 1, 0, 0]), &mut ram);
        assert!(!effects.halt);
        assert_eq!(
            ram.read_32(0xFFF8, Access::NONSEQUENTIAL),
            0,
            "flag consumed"
        );
    }

    #[test]
    fn vblank_intr_wait_keeps_the_flag_when_it_resumes() {
        let mut ram = Ram::new();
        ram.write_32(0xFFF8, 1, Access::NONSEQUENTIAL);
        // A new call discards the old flag and waits for the next VBlank.
        let effects = run(0x05, &call(Side::Arm7, [0; 4]), &mut ram);
        assert!(effects.halt && effects.repeat);
        // Woken by an interrupt whose handler set the flag again.
        ram.write_32(0xFFF8, 1, Access::NONSEQUENTIAL);
        let resumed = Call {
            resumes: true,
            ..call(Side::Arm7, [0; 4])
        };
        let effects = run(0x05, &resumed, &mut ram);
        assert!(!effects.halt);
        assert_eq!(
            ram.read_32(0xFFF8, Access::NONSEQUENTIAL),
            0,
            "flag consumed"
        );
    }

    #[test]
    fn lz77_expands_back_references() {
        let mut ram = Ram::new();
        // Header: type 0x10, 6 bytes. Flags 0b0100_0000: literal, then a
        // reference of length 5 at distance 1.
        let packed = [0x10, 6, 0, 0, 0b0100_0000, b'A', 0x20, 0x00];
        ram.0[0x100..0x108].copy_from_slice(&packed);
        run(0x11, &call(Side::Arm9, [0x100, 0x200, 0, 0]), &mut ram);
        assert_eq!(&ram.0[0x200..0x206], b"AAAAAA");
    }

    #[test]
    fn run_length_writes_a_byte_at_a_time() {
        let mut ram = Ram::new();
        // Header: type 0x30, 5 bytes. "x" five times, to an odd address.
        ram.0[0x100..0x106].copy_from_slice(&[0x30, 5, 0, 0, 0x82, b'x']);
        run(0x14, &call(Side::Arm7, [0x100, 0x201, 0, 0]), &mut ram);
        assert_eq!(&ram.0[0x200..0x207], b"\0xxxxx\0");
    }

    #[test]
    fn callback_decompressors_start_by_calling_open() {
        let mut ram = Ram::new();
        // The callback structure at 0x300 lists Open at 0x1001 (Thumb).
        ram.write_32(0x300, 0x1001, Access::NONSEQUENTIAL);
        for number in [0x12, 0x13, 0x15] {
            let args = [0x100, 0x200, 0x55, 0x300];
            let effects = run(number, &call(Side::Arm9, args), &mut ram);
            let (_, open) = effects.callback.expect("a callback decompressor");
            assert_eq!(open.function, 0x1001);
            assert_eq!(open.args, [0x100, 0x200, 0x55]);
        }
    }

    #[test]
    fn soft_reset_clears_the_bios_area_and_finds_the_entry_point() {
        let mut ram = Ram::new();
        ram.0[0x3C00..0x4000].fill(0xAA);
        // The ARM7's entry point in the header copy (0x027FFE34, which
        // this RAM mirrors at 0xFE34).
        ram.write_32(0xFE34, 0x0238_0001, Access::NONSEQUENTIAL);
        let call = Call {
            side: Side::Arm7,
            args: [0; 4],
            intr_flags: 0x3FF8,
            resumes: false,
        };
        let effects = run(0x00, &call, &mut ram);
        assert_eq!(effects.restart, Some(0x0238_0001));
        assert!(ram.0[0x3E00..0x4000].iter().all(|byte| *byte == 0));
        assert_eq!(ram.0[0x3DFF], 0xAA, "below the area");
    }

    #[test]
    fn divide_returns_quotient_remainder_and_magnitude() {
        assert_eq!(
            divide(-7, 2),
            [(0, (-3i32) as u32), (1, (-1i32) as u32), (3, 3)]
        );
    }

    #[test]
    fn sound_tables_span_their_ranges() {
        assert_eq!(pitch_table(0), 0);
        assert!(pitch_table(767) > 0xFF00);
        assert_eq!(sine_table(0), 0);
        assert_eq!(volume_table(723), 127);
        assert_eq!(volume_table(663), 64);
        assert!(volume_table(0) < volume_table(180));
    }
}
