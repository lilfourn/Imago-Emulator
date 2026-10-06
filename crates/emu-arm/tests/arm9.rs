//! The ARM946E-S: ARMv5TE instructions, CP15, tightly coupled memory, high
//! vectors and waiting for interrupts. Each test assembles a few
//! instructions by hand into a flat RAM and steps through them.

use emu_arm::cp15::control;
use emu_arm::{Access, Arm9, Bus, Cpu, Mode, Model, Processor, Registers, psr};

/// A bus access, by address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    Fetch(u32),
    Read(u32),
    Write(u32),
}

/// 1 MiB of RAM that repeats across the address space, so the DS's main
/// RAM addresses and the high vectors land in it too. It logs every access
/// and counts idle cycles.
struct Ram {
    bytes: Vec<u8>,
    seen: Vec<Seen>,
    idles: u32,
}

impl Ram {
    fn new() -> Self {
        Self {
            bytes: vec![0; 0x10_0000],
            seen: Vec::new(),
            idles: 0,
        }
    }

    fn index(address: u32) -> usize {
        (address & 0xF_FFFF) as usize
    }

    fn peek_32(&self, address: u32) -> u32 {
        let i = Self::index(address & !3);
        u32::from_le_bytes(self.bytes[i..i + 4].try_into().unwrap())
    }

    fn poke_32(&mut self, address: u32, value: u32) {
        let i = Self::index(address & !3);
        self.bytes[i..i + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn peek_16(&self, address: u32) -> u16 {
        let i = Self::index(address & !1);
        u16::from_le_bytes([self.bytes[i], self.bytes[i + 1]])
    }

    fn poke_16(&mut self, address: u32, value: u16) {
        let i = Self::index(address & !1);
        self.bytes[i..i + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn load_arm(&mut self, address: u32, program: &[u32]) {
        for (offset, &word) in (0..).step_by(4).zip(program) {
            self.poke_32(address + offset, word);
        }
    }

    fn load_thumb(&mut self, address: u32, program: &[u16]) {
        for (offset, &half) in (0..).step_by(2).zip(program) {
            self.poke_16(address + offset, half);
        }
    }

    fn note_read(&mut self, address: u32, access: Access) {
        self.seen.push(if access.contains(Access::CODE) {
            Seen::Fetch(address)
        } else {
            Seen::Read(address)
        });
    }

    fn forget(&mut self) {
        self.seen.clear();
        self.idles = 0;
    }

    fn data_accesses(&self) -> Vec<Seen> {
        self.seen
            .iter()
            .copied()
            .filter(|seen| !matches!(seen, Seen::Fetch(_)))
            .collect()
    }

    fn fetched(&self, address: u32) -> bool {
        self.seen.contains(&Seen::Fetch(address))
    }
}

impl Bus for Ram {
    fn read_32(&mut self, address: u32, access: Access) -> u32 {
        self.note_read(address, access);
        self.peek_32(address)
    }
    fn read_16(&mut self, address: u32, access: Access) -> u16 {
        self.note_read(address, access);
        self.peek_16(address)
    }
    fn read_8(&mut self, address: u32, access: Access) -> u8 {
        self.note_read(address, access);
        self.bytes[Self::index(address)]
    }
    fn write_32(&mut self, address: u32, value: u32, _: Access) {
        self.seen.push(Seen::Write(address));
        self.poke_32(address, value);
    }
    fn write_16(&mut self, address: u32, value: u16, _: Access) {
        self.seen.push(Seen::Write(address));
        self.poke_16(address, value);
    }
    fn write_8(&mut self, address: u32, value: u8, _: Access) {
        self.seen.push(Seen::Write(address));
        self.bytes[Self::index(address)] = value;
    }
    fn idle(&mut self) {
        self.idles += 1;
    }
}

/// A CPU in System mode with IRQs enabled, about to run `address`.
fn start<M: Model>(ram: &mut Ram, address: u32, thumb: bool) -> Processor<M> {
    let registers = Registers {
        cpsr: Mode::System.bits() | if thumb { psr::T } else { 0 },
        ..Registers::default()
    };
    let mut cpu = Processor::from_registers(&registers, [0, 0], Access::NONSEQUENTIAL);
    cpu.jump(ram, address);
    cpu
}

/// An ARM9 about to run the ARM `program`, loaded at `address`.
fn arm9_running(ram: &mut Ram, address: u32, program: &[u32]) -> Arm9 {
    ram.load_arm(address, program);
    start(ram, address, false)
}

/// The address of the instruction that runs next.
fn next_pc<M: Model>(cpu: &Processor<M>) -> u32 {
    cpu.reg(15).wrapping_sub(if cpu.thumb() { 4 } else { 8 })
}

/// Main RAM, where the DS loads code, outside the TCMs' usual places.
const MAIN: u32 = 0x0200_1000;

#[test]
fn clz_counts_leading_zeros() {
    for (value, count) in [(0, 32), (1, 31), (0x8000_0000, 0), (0x00F0_0000, 8)] {
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[0xE16F_0F11]); // CLZ R0, R1
        cpu.set_reg(1, value);
        cpu.step(&mut ram);
        assert_eq!(cpu.reg(0), count, "CLZ of {value:#x}");
        assert_eq!(next_pc(&cpu), MAIN + 4);
    }
}

#[test]
fn saturating_arithmetic_saturates_and_sets_q() {
    const QADD: u32 = 0xE102_0051; // QADD R0, R1, R2: R0 = R1 + R2
    const QSUB: u32 = 0xE122_0051; // QSUB R0, R1, R2: R0 = R1 - R2
    const QDADD: u32 = 0xE142_0051; // QDADD R0, R1, R2: R0 = R1 + 2 × R2
    const QDSUB: u32 = 0xE162_0051; // QDSUB R0, R1, R2: R0 = R1 - 2 × R2
    let cases = [
        (QADD, 0x7FFF_FFFF, 1, 0x7FFF_FFFF, true),
        (QADD, 0x8000_0000, u32::MAX, 0x8000_0000, true),
        (QADD, 1, 2, 3, false),
        (QSUB, 0x8000_0000, 1, 0x8000_0000, true),
        (QSUB, 5, 7, (-2i32) as u32, false),
        // The doubling saturates, then the sum does not overflow.
        (QDADD, 0, 0x4000_0000, 0x7FFF_FFFF, true),
        (QDADD, 1, 0x1000, 0x2001, false),
        (QDSUB, 5, 2, 1, false),
        // 2 × -2^30 fits; subtracting it overflows.
        (QDSUB, 0, 0xC000_0000, 0x7FFF_FFFF, true),
    ];
    for (instruction, rm, rn, result, q) in cases {
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[instruction]);
        cpu.set_reg(1, rm);
        cpu.set_reg(2, rn);
        cpu.step(&mut ram);
        assert_eq!(cpu.reg(0), result, "{instruction:08x} {rm:#x}, {rn:#x}");
        assert_eq!(cpu.cpsr() & psr::Q != 0, q, "Q after {instruction:08x}");
    }
}

#[test]
fn q_is_sticky_until_msr_clears_it() {
    let mut ram = Ram::new();
    let program = [
        0xE102_0051, // QADD R0, R1, R2 (saturates)
        0xE102_3054, // QADD R3, R4, R2 (does not)
        0xE328_F000, // MSR CPSR_f, #0
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    cpu.set_reg(1, 0x7FFF_FFFF);
    cpu.set_reg(2, 1);
    cpu.step(&mut ram);
    cpu.step(&mut ram);
    assert_eq!(cpu.reg(3), 1);
    assert_ne!(cpu.cpsr() & psr::Q, 0);
    cpu.step(&mut ram);
    assert_eq!(cpu.cpsr() & psr::Q, 0);
}

#[test]
fn signed_halfword_multiplies() {
    const SMULBB: u32 = 0xE160_0281; // SMULBB R0, R1, R2
    const SMULTB: u32 = 0xE160_02A1;
    const SMULBT: u32 = 0xE160_02C1;
    const SMULTT: u32 = 0xE160_02E1;
    const SMLABB: u32 = 0xE100_3281; // SMLABB R0, R1, R2, R3
    const SMULWB: u32 = 0xE120_02A1; // SMULWB R0, R1, R2
    const SMULWT: u32 = 0xE120_02E1;
    const SMLAWB: u32 = 0xE120_3281; // SMLAWB R0, R1, R2, R3
    // R1's halves are -2 and 3, R2's 4 and -5.
    let (r1, r2) = (0xFFFE_0003, 0x0004_FFFB);
    let cases = [
        (SMULBB, r1, r2, 0, (-15i32) as u32, false),
        (SMULTB, r1, r2, 0, 10, false),
        (SMULBT, r1, r2, 0, 12, false),
        (SMULTT, r1, r2, 0, (-8i32) as u32, false),
        (SMLABB, 3, 0xFFFB, 100, 85, false),
        // The accumulation overflows: it wraps and sets Q.
        (SMLABB, 1, 1, 0x7FFF_FFFF, 0x8000_0000, true),
        // The W forms keep bits 16–47 of the 48-bit product.
        (SMULWB, 0x8000_0000, 2, 0, 0xFFFF_0000, false),
        (SMULWB, 1, 0xFFFF, 0, u32::MAX, false),
        (SMULWT, 0x0001_0000, 0xFFFD_0002, 0, (-3i32) as u32, false),
        (SMLAWB, 0x0002_0000, 3, 10, 16, false),
        (SMLAWB, 0x0002_0000, 3, 0x7FFF_FFFF, 0x8000_0005, true),
    ];
    for (instruction, rm, rs, rn, result, q) in cases {
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[instruction]);
        cpu.set_reg(1, rm);
        cpu.set_reg(2, rs);
        cpu.set_reg(3, rn);
        cpu.step(&mut ram);
        assert_eq!(
            cpu.reg(0),
            result,
            "{instruction:08x} {rm:#x} {rs:#x} {rn:#x}"
        );
        assert_eq!(cpu.cpsr() & psr::Q != 0, q, "Q after {instruction:08x}");
    }
}

#[test]
fn smlal_accumulates_64_bits() {
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xE144_0281]); // SMLALBB R0, R4, R1, R2
    cpu.set_reg(1, 0x1234_FFFE); // -2
    cpu.set_reg(2, 0xABCD_0003); // 3
    cpu.set_reg(0, 2);
    cpu.set_reg(4, 1);
    cpu.step(&mut ram);
    assert_eq!((cpu.reg(4), cpu.reg(0)), (0, 0xFFFF_FFFC));
    assert_eq!(cpu.cpsr() & (psr::Q | psr::N | psr::Z), 0);
}

#[test]
fn ldrd_and_strd_with_writeback() {
    let mut ram = Ram::new();
    let program = [
        0xE1E0_20D8, // LDRD R2, [R0, #8]!
        0xE041_21F0, // STRD R2, [R1], #-16
        0xE100_40D5, // LDRD R4, [R0, -R5]
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    for (address, value) in [(0x8000, 0x3333_3333), (0x8004, 0x4444_4444)] {
        ram.poke_32(address, value);
    }
    ram.poke_32(0x8008, 0x1111_1111);
    ram.poke_32(0x800C, 0x2222_2222);
    cpu.set_reg(0, 0x8000);
    cpu.set_reg(1, 0x9000);
    cpu.set_reg(5, 8);

    cpu.step(&mut ram);
    assert_eq!((cpu.reg(2), cpu.reg(3)), (0x1111_1111, 0x2222_2222));
    assert_eq!(cpu.reg(0), 0x8008);

    ram.forget();
    cpu.step(&mut ram);
    assert_eq!(ram.peek_32(0x9000), 0x1111_1111);
    assert_eq!(ram.peek_32(0x9004), 0x2222_2222);
    assert_eq!(cpu.reg(1), 0x8FF0);
    assert_eq!(
        ram.data_accesses(),
        [Seen::Write(0x9000), Seen::Write(0x9004)]
    );

    cpu.step(&mut ram);
    assert_eq!((cpu.reg(4), cpu.reg(5)), (0x3333_3333, 0x4444_4444));
    assert_eq!(cpu.reg(0), 0x8008);
}

#[test]
fn ldrd_with_an_odd_register_is_undefined() {
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xE1C0_10D0]); // LDRD R1, [R0]
    cpu.step(&mut ram);
    assert_eq!(cpu.mode(), Mode::Undefined);
    assert_eq!(next_pc(&cpu), 0xFFFF_0004);
}

#[test]
fn blx_immediate_calls_thumb_code() {
    // Forward with H set, and backward.
    for (instruction, target) in [(0xFB00_003E, MAIN + 0x102), (0xFAFF_FFBE, MAIN - 0x100)] {
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[instruction]);
        cpu.step(&mut ram);
        assert!(cpu.thumb());
        assert_eq!(cpu.reg(14), MAIN + 4);
        assert_eq!(next_pc(&cpu), target, "{instruction:08x}");
    }
}

#[test]
fn blx_register_switches_by_bit_0() {
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xE12F_FF30]); // BLX R0
    ram.load_thumb(0x0200_2000, &[0x4788]); // BLX R1
    cpu.set_reg(0, 0x0200_2001);
    cpu.step(&mut ram);
    assert!(cpu.thumb());
    assert_eq!(cpu.reg(14), MAIN + 4);
    assert_eq!(next_pc(&cpu), 0x0200_2000);

    // Thumb BLX R1 back to ARM code.
    cpu.set_reg(1, 0x0200_3000);
    cpu.step(&mut ram);
    assert!(!cpu.thumb());
    assert_eq!(cpu.reg(14), 0x0200_2003);
    assert_eq!(next_pc(&cpu), 0x0200_3000);
}

#[test]
fn thumb_bl_prefix_and_blx_suffix_call_arm_code() {
    let mut ram = Ram::new();
    ram.load_thumb(MAIN, &[0xF000, 0xE87E]); // BLX MAIN + 0x100
    let mut cpu: Arm9 = start(&mut ram, MAIN, true);
    cpu.step(&mut ram);
    cpu.step(&mut ram);
    assert!(!cpu.thumb());
    assert_eq!(cpu.reg(14), (MAIN + 4) | 1);
    assert_eq!(next_pc(&cpu), MAIN + 0x100);
}

#[test]
fn thumb_blx_suffix_with_an_odd_offset_is_undefined() {
    let mut ram = Ram::new();
    ram.load_thumb(MAIN, &[0xF000, 0xE87F]);
    let mut cpu: Arm9 = start(&mut ram, MAIN, true);
    cpu.step(&mut ram);
    cpu.step(&mut ram);
    assert_eq!(cpu.mode(), Mode::Undefined);
}

#[test]
fn loads_into_pc_take_the_state_from_bit_0() {
    for instruction in [0xE590_F000, 0xE890_8000] {
        // LDR PC, [R0] and LDMIA R0, {PC}
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[instruction]);
        ram.poke_32(0x8000, 0x0200_2001);
        cpu.set_reg(0, 0x8000);
        cpu.step(&mut ram);
        assert!(cpu.thumb(), "{instruction:08x}");
        assert_eq!(next_pc(&cpu), 0x0200_2000);
    }

    // POP {PC} with bit 0 clear returns to ARM code.
    let mut ram = Ram::new();
    ram.load_thumb(MAIN, &[0xBD00]);
    ram.poke_32(0x8000, 0x0200_3000);
    let mut cpu: Arm9 = start(&mut ram, MAIN, true);
    cpu.set_reg(13, 0x8000);
    cpu.step(&mut ram);
    assert!(!cpu.thumb());
    assert_eq!(next_pc(&cpu), 0x0200_3000);
    assert_eq!(cpu.reg(13), 0x8004);
}

#[test]
fn the_pre_v5_bit_keeps_loads_into_pc_in_their_state() {
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xE590_F000]); // LDR PC, [R0]
    cpu.cp15_mut().set_control(control::RESET | control::PRE_V5);
    ram.poke_32(0x8000, 0x0200_2001);
    cpu.set_reg(0, 0x8000);
    ram.forget();
    cpu.step(&mut ram);
    assert!(!cpu.thumb());
    assert!(ram.fetched(0x0200_2000));

    let mut ram = Ram::new();
    ram.load_thumb(MAIN, &[0xBD00]); // POP {PC}
    ram.poke_32(0x8000, 0x0200_3000);
    let mut cpu: Arm9 = start(&mut ram, MAIN, true);
    cpu.cp15_mut().set_control(control::RESET | control::PRE_V5);
    cpu.set_reg(13, 0x8000);
    cpu.step(&mut ram);
    assert!(cpu.thumb());
    assert_eq!(next_pc(&cpu), 0x0200_3000);
}

#[test]
fn bkpt_takes_a_prefetch_abort() {
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xE120_0070]); // BKPT #0
    let before = cpu.cpsr();
    cpu.step(&mut ram);
    assert_eq!(cpu.mode(), Mode::Abort);
    assert_eq!(cpu.reg(14), MAIN + 4);
    assert_eq!(next_pc(&cpu), 0xFFFF_000C);
    assert_eq!(cpu.saved_psr(), Some(before));
    assert_ne!(cpu.cpsr() & psr::I, 0);

    let mut ram = Ram::new();
    ram.load_thumb(MAIN, &[0xBE00]); // BKPT #0
    let mut cpu: Arm9 = start(&mut ram, MAIN, true);
    cpu.step(&mut ram);
    assert_eq!(cpu.mode(), Mode::Abort);
    assert!(!cpu.thumb());
    assert_eq!(cpu.reg(14), MAIN + 4);
    assert_ne!(cpu.saved_psr().unwrap_or(0) & psr::T, 0);
}

#[test]
fn mrc_reads_the_id_registers() {
    let mut ram = Ram::new();
    let program = [
        0xEE10_0F10, // MRC p15, 0, R0, c0, c0, 0
        0xEE10_1F30, // MRC p15, 0, R1, c0, c0, 1
        0xEE10_2F50, // MRC p15, 0, R2, c0, c0, 2
        0xEE10_FF10, // MRC p15, 0, R15, c0, c0, 0: flags from bits 28–31
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    for _ in 0..program.len() {
        cpu.step(&mut ram);
    }
    assert_eq!(cpu.reg(0), 0x4105_9461);
    assert_eq!(cpu.reg(1), 0x0F0D_2112);
    assert_eq!(cpu.reg(2), 0x0014_0180);
    assert_eq!(cpu.cpsr() & 0xF000_0000, psr::Z);
    assert_eq!(next_pc(&cpu), MAIN + 16);
}

#[test]
fn mcr_and_mrc_round_trip() {
    let mut ram = Ram::new();
    let program = [
        0xEE09_1F11, // MCR p15, 0, R1, c9, c1, 0 (data TCM region)
        0xEE19_0F11, // MRC p15, 0, R0, c9, c1, 0
        0xEE06_2F32, // MCR p15, 0, R2, c6, c2, 1 (protection region 2)
        0xEE16_3F12, // MRC p15, 0, R3, c6, c2, 0
        0xEE01_4F10, // MCR p15, 0, R4, c1, c0, 0 (control)
        0xEE11_5F10, // MRC p15, 0, R5, c1, c0, 0
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    cpu.set_reg(1, 0x027C_000A);
    cpu.set_reg(2, 0x0800_0035);
    cpu.set_reg(4, 0x0005_707D);
    for _ in 0..program.len() {
        cpu.step(&mut ram);
    }
    assert_eq!(cpu.reg(0), 0x027C_000A);
    assert_eq!(cpu.reg(3), 0x0800_0035);
    assert_eq!(cpu.reg(5), 0x0005_707D);
    assert_eq!(cpu.cp15().dtcm_region(), 0x027C_000A);
    assert_eq!(cpu.cp15().control(), 0x0005_707D);
}

#[test]
fn other_coprocessor_instructions_are_undefined() {
    let cases = [
        (0xEE10_0E10, Mode::System), // MRC p14, 0, R0, c0, c0, 0
        (0xEE00_0F00, Mode::System), // CDP p15, 0, c0, c0, c0, 0
        (0xED90_0F00, Mode::System), // LDC p15, c0, [R0]
        (0xFE01_1F10, Mode::System), // MCR2 p15, 0, R1, c1, c0, 0
        (0xEE10_0F10, Mode::User),   // MRC p15 from User mode
    ];
    for (instruction, mode) in cases {
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[instruction]);
        cpu.set_cpsr(mode.bits());
        cpu.step(&mut ram);
        assert_eq!(cpu.mode(), Mode::Undefined, "{instruction:08x}");
        assert_eq!(cpu.reg(14), MAIN + 4);
        assert_eq!(next_pc(&cpu), 0xFFFF_0004);
    }
}

#[test]
fn pld_does_nothing() {
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xF5D0_F000]); // PLD [R0]
    ram.forget();
    cpu.step(&mut ram);
    assert_eq!(next_pc(&cpu), MAIN + 4);
    assert!(ram.data_accesses().is_empty());
}

/// Instruction TCM over the bottom 32 MiB, data TCM at 0x027C0000 with a
/// 32 KiB virtual size (so it repeats once), both on.
fn enable_tcm(cpu: &mut Arm9) {
    let cp15 = cpu.cp15_mut();
    cp15.set_itcm_region(0x0000_0020);
    cp15.set_dtcm_region(0x027C_000C);
    cp15.set_control(control::RESET | control::ITCM_ENABLE | control::DTCM_ENABLE);
}

#[test]
fn tcm_accesses_stay_off_the_bus_but_take_time() {
    let mut ram = Ram::new();
    let program = [
        0xE580_1000, // STR R1, [R0]
        0xE592_3000, // LDR R3, [R2]
        0xE584_1000, // STR R1, [R4]
        0xE596_5000, // LDR R5, [R6]
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    enable_tcm(&mut cpu);
    cpu.set_reg(0, 0x0100_0010); // an ITCM mirror
    cpu.set_reg(1, 0xCAFE_F00D);
    cpu.set_reg(2, 0x10);
    cpu.set_reg(4, 0x027C_4004); // the DTCM's mirror
    cpu.set_reg(6, 0x027C_0004);
    ram.forget();
    for _ in 0..program.len() {
        cpu.step(&mut ram);
    }
    assert_eq!(cpu.reg(3), 0xCAFE_F00D);
    assert_eq!(cpu.reg(5), 0xCAFE_F00D);
    assert!(ram.data_accesses().is_empty());
    // One cycle per TCM access, plus each LDR's internal cycle.
    assert_eq!(ram.idles, 6);
    assert_eq!(cpu.itcm()[0x10..0x14], [0x0D, 0xF0, 0xFE, 0xCA]);
    assert_eq!(cpu.dtcm()[4], 0x0D);
    assert_eq!(cpu.tcm_read_8(0x0180_0013), Some(0xCA));
    assert_eq!(ram.peek_32(0x10), 0);
}

#[test]
fn instructions_come_from_itcm_but_not_dtcm() {
    let mut ram = Ram::new();
    let mut cpu: Arm9 = start(&mut ram, MAIN, false);
    enable_tcm(&mut cpu);
    // MOV R0, #1 in ITCM; MOV R0, #2 in DTCM; MOV R0, #3 in RAM behind it.
    for (offset, byte) in (0..).zip(0xE3A0_0001u32.to_le_bytes()) {
        assert!(cpu.tcm_write_8(0x100 + offset, byte));
    }
    for (offset, byte) in (0..).zip(0xE3A0_0002u32.to_le_bytes()) {
        assert!(cpu.tcm_write_8(0x027C_0000 + offset, byte));
    }
    ram.poke_32(0x027C_0000, 0xE3A0_0003);

    ram.forget();
    cpu.jump(&mut ram, 0x100);
    cpu.step(&mut ram);
    assert_eq!(cpu.reg(0), 1);
    assert!(ram.seen.is_empty());
    assert!(ram.idles >= 3);

    cpu.jump(&mut ram, 0x027C_0000);
    cpu.step(&mut ram);
    assert_eq!(cpu.reg(0), 3);
    assert!(ram.fetched(0x027C_0000));
}

#[test]
fn tcm_load_mode_reads_the_bus_and_writes_the_tcm() {
    let mut ram = Ram::new();
    let program = [
        0xE580_1000, // STR R1, [R0]
        0xE590_2000, // LDR R2, [R0]
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    cpu.cp15_mut().set_itcm_region(0x0000_0020);
    cpu.cp15_mut()
        .set_control(control::RESET | control::ITCM_ENABLE | control::ITCM_LOAD);
    ram.poke_32(0x40, 0x1234);
    cpu.set_reg(0, 0x40);
    cpu.set_reg(1, 0x5678);
    ram.forget();
    cpu.step(&mut ram);
    cpu.step(&mut ram);
    assert_eq!(cpu.reg(2), 0x1234);
    assert_eq!(ram.data_accesses(), [Seen::Read(0x40)]);
    assert_eq!(cpu.itcm()[0x40..0x44], [0x78, 0x56, 0, 0]);
    assert_eq!(cpu.tcm_read_8(0x40), None);
}

#[test]
fn itcm_wins_where_the_tcms_overlap() {
    let mut ram = Ram::new();
    let mut cpu: Arm9 = start(&mut ram, MAIN, false);
    let cp15 = cpu.cp15_mut();
    cp15.set_itcm_region(0x0000_0020);
    cp15.set_dtcm_region(0x0000_000A);
    cp15.set_control(control::RESET | control::ITCM_ENABLE | control::DTCM_ENABLE);
    assert!(cpu.tcm_write_8(0x20, 0xAB));
    assert_eq!(cpu.itcm()[0x20], 0xAB);
    assert_eq!(cpu.dtcm()[0x20], 0);
}

#[test]
fn disabled_tcm_leaves_everything_to_the_bus() {
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xE580_1000]); // STR R1, [R0]
    cpu.set_reg(0, 0x10);
    ram.forget();
    cpu.step(&mut ram);
    assert_eq!(ram.data_accesses(), [Seen::Write(0x10)]);
    assert_eq!(cpu.tcm_read_8(0x10), None);
    assert!(!cpu.tcm_write_8(0x10, 1));
}

#[test]
fn reset_starts_at_the_high_vectors() {
    let mut ram = Ram::new();
    let mut cpu = Arm9::default();
    cpu.reset(&mut ram);
    assert_eq!(next_pc(&cpu), 0xFFFF_0000);
    assert_eq!(cpu.mode(), Mode::Supervisor);
    assert!(ram.fetched(0xFFFF_0000));
    assert_eq!(cpu.cp15().control(), 0x2078);
}

#[test]
fn reset_keeps_tcm_contents() {
    let mut ram = Ram::new();
    let mut cpu: Arm9 = start(&mut ram, MAIN, false);
    enable_tcm(&mut cpu);
    assert!(cpu.tcm_write_8(0x027C_0000, 0x5A));
    cpu.reset(&mut ram);
    assert_eq!(cpu.cp15().control(), control::RESET);
    assert_eq!(cpu.dtcm()[0], 0x5A);
}

#[test]
fn the_v_bit_picks_the_vector_base() {
    for (value, vector) in [(control::RESET, 0xFFFF_0008), (control::FIXED, 0x0000_0008)] {
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[0xEF00_0000]); // SWI 0
        cpu.cp15_mut().set_control(value);
        cpu.step(&mut ram);
        assert_eq!(cpu.mode(), Mode::Supervisor);
        assert_eq!(next_pc(&cpu), vector);
    }
}

#[test]
fn wait_for_interrupt_halts_until_an_irq() {
    let mut ram = Ram::new();
    let program = [
        0xEE07_0F90, // MCR p15, 0, R0, c7, c0, 4
        0xE3A0_1001, // MOV R1, #1
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    cpu.step(&mut ram);
    assert!(cpu.halted());

    ram.forget();
    for _ in 0..3 {
        cpu.step(&mut ram);
    }
    assert!(cpu.halted());
    assert_eq!(cpu.reg(1), 0);
    assert!(ram.seen.is_empty());
    assert_eq!(ram.idles, 3);

    cpu.set_irq_line(true);
    cpu.step(&mut ram);
    assert!(!cpu.halted());
    assert_eq!(cpu.mode(), Mode::Irq);
    assert_eq!(next_pc(&cpu), 0xFFFF_0018);
    // SUBS PC, LR, #4 returns to the MOV.
    assert_eq!(cpu.reg(14), MAIN + 8);
}

#[test]
fn a_masked_irq_wakes_without_being_taken() {
    let mut ram = Ram::new();
    let program = [
        0xEE07_0F58, // MCR p15, 0, R0, c7, c8, 2
        0xE3A0_1001, // MOV R1, #1
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    cpu.set_cpsr(Mode::System.bits() | psr::I);
    cpu.step(&mut ram);
    assert!(cpu.halted());
    cpu.set_irq_line(true);
    cpu.step(&mut ram);
    assert!(!cpu.halted());
    assert_eq!(cpu.mode(), Mode::System);
    assert_eq!(cpu.reg(1), 1);
}

#[test]
fn wake_resumes_after_the_halt() {
    let mut ram = Ram::new();
    let program = [
        0xEE07_0F90, // MCR p15, 0, R0, c7, c0, 4
        0xE3A0_1001, // MOV R1, #1
    ];
    let mut cpu = arm9_running(&mut ram, MAIN, &program);
    cpu.step(&mut ram);
    cpu.wake();
    cpu.step(&mut ram);
    assert_eq!(cpu.reg(1), 1);
}

#[test]
fn block_transfers_follow_armv5_rules() {
    // (instruction, R0, R1, expected R0, expected R1)
    let cases = [
        // LDMIA R0!, {R0, R1}: the base is not last, so writeback wins.
        (0xE8B0_0003, 0x8000, 0, 0x8008, 0xBBBB),
        // LDMIA R1!, {R0, R1}: the base is last of several; its load stays.
        (0xE8B1_0003, 0, 0x8000, 0xAAAA, 0xBBBB),
        // LDMIA R0!, {R0}: the base alone; writeback wins.
        (0xE8B0_0001, 0x8000, 0, 0x8004, 0),
        // LDMIA R0!, {}: nothing transferred, the base moves by 0x40.
        (0xE8B0_0000, 0x8000, 0, 0x8040, 0),
        // STMDB R0!, {}: likewise, downwards.
        (0xE920_0000, 0x8000, 0, 0x7FC0, 0),
    ];
    for (instruction, r0, r1, expected_r0, expected_r1) in cases {
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[instruction]);
        ram.poke_32(0x8000, 0xAAAA);
        ram.poke_32(0x8004, 0xBBBB);
        cpu.set_reg(0, r0);
        cpu.set_reg(1, r1);
        cpu.step(&mut ram);
        assert_eq!(
            (cpu.reg(0), cpu.reg(1)),
            (expected_r0, expected_r1),
            "{instruction:08x}"
        );
        assert_eq!(next_pc(&cpu), MAIN + 4, "{instruction:08x}");
    }

    // STMIA R1!, {R0, R1} stores the old base even though it is not first.
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xE8A1_0003]);
    cpu.set_reg(0, 7);
    cpu.set_reg(1, 0x8000);
    ram.forget();
    cpu.step(&mut ram);
    assert_eq!(ram.peek_32(0x8004), 0x8000);
    assert_eq!(cpu.reg(1), 0x8008);
    assert_eq!(
        ram.data_accesses(),
        [Seen::Write(0x8000), Seen::Write(0x8004)]
    );

    // Thumb LDMIA R0!, {R0, R1} keeps the loaded base, unlike ARM's LDM.
    let mut ram = Ram::new();
    ram.load_thumb(MAIN, &[0xC803]);
    ram.poke_32(0x8000, 0xAAAA);
    ram.poke_32(0x8004, 0xBBBB);
    let mut cpu: Arm9 = start(&mut ram, MAIN, true);
    cpu.set_reg(0, 0x8000);
    cpu.step(&mut ram);
    assert_eq!((cpu.reg(0), cpu.reg(1)), (0xAAAA, 0xBBBB));
}

#[test]
fn misaligned_halfword_loads_ignore_bit_0() {
    // LDRH R0, [R1] and LDRSH R0, [R1]
    for (instruction, result) in [(0xE1D1_00B0, 0x8180), (0xE1D1_00F0, 0xFFFF_8180)] {
        let mut ram = Ram::new();
        let mut cpu = arm9_running(&mut ram, MAIN, &[instruction]);
        ram.poke_16(0x8000, 0x8180);
        cpu.set_reg(1, 0x8001);
        cpu.step(&mut ram);
        assert_eq!(cpu.reg(0), result, "{instruction:08x}");
    }
}

#[test]
fn the_arm7_keeps_armv4_behaviour() {
    let mut ram = Ram::new();
    let program = [
        0xFB00_003E, // BLX on ARMv5; condition "never" on ARMv4
        0xE590_F000, // LDR PC, [R0]
    ];
    ram.load_arm(0x1000, &program);
    ram.poke_32(0x8000, 0x2001);
    let mut cpu: Cpu = start(&mut ram, 0x1000, false);
    cpu.set_reg(0, 0x8000);
    cpu.step(&mut ram);
    assert_eq!(next_pc(&cpu), 0x1004);
    cpu.step(&mut ram);
    assert!(!cpu.thumb());

    // The Thumb BLX suffix is undefined.
    let mut ram = Ram::new();
    ram.load_thumb(0x1000, &[0xE800]);
    let mut cpu: Cpu = start(&mut ram, 0x1000, true);
    cpu.step(&mut ram);
    assert_eq!(cpu.mode(), Mode::Undefined);
    assert_eq!(next_pc(&cpu), 0x0004);
}

#[test]
fn registers_round_trip_on_the_arm9() {
    let mut ram = Ram::new();
    let mut cpu: Arm9 = start(&mut ram, MAIN, false);
    cpu.set_cpsr(Mode::Irq.bits() | psr::Q);
    cpu.set_reg(13, 0x0080_3FA0);
    let registers = cpu.registers();
    let copy = Arm9::from_registers(&registers, cpu.pipeline(), cpu.fetch_access());
    assert_eq!(copy.registers(), registers);
}

#[test]
fn user_bank_ldm_writes_the_base_back_to_the_current_mode() {
    // LDMIA R13!, {R14}^ in IRQ mode: R14 comes from memory into the User
    // bank, and the incremented base goes to R13_irq.
    let mut ram = Ram::new();
    let mut cpu = arm9_running(&mut ram, MAIN, &[0xE8FD_4000]);
    ram.poke_32(MAIN + 0x100, 0xC0FF_EEAB);
    cpu.set_reg(13, 0xBAD0);
    cpu.set_reg(14, 0);
    cpu.set_cpsr(Mode::Irq.bits());
    cpu.set_reg(13, MAIN + 0x100);
    cpu.set_reg(14, 0);
    cpu.step(&mut ram);
    assert_eq!(cpu.reg(13), MAIN + 0x104, "R13_irq written back");
    assert_eq!(cpu.reg(14), 0, "R14_irq untouched");
    cpu.set_cpsr(Mode::System.bits());
    assert_eq!(cpu.reg(14), 0xC0FF_EEAB, "loaded into the User bank");
    assert_eq!(cpu.reg(13), 0xBAD0, "User R13 untouched");
}
