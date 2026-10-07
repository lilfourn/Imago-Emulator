//! The DS's [`Debugger`]: both CPUs (0 is the ARM9, 1 the ARM7), each
//! with its own memory map and I/O registers, the scheduler they share,
//! and both 2D engines' memory.
//!
//! Watchpoints see each CPU's own loads and stores, the ARM9's to its
//! tightly coupled memory included. They do not see DMA, the sound
//! channels and capture units, display capture, or memory the HLE BIOS
//! reads and writes for a system call.

use emu_arm::{Architecture, Watched, describe_psr};
use emu_core::debug::{
    self, CpuInfo, DataAccess, Graphics, GraphicsRequest, Instruction, IoRegister, Register,
    RunOutcome, ScheduledEvent, Stop, Stops,
};
use emu_core::{Debugger, Input, StereoSample};

use crate::bus::{Arm7Bus, Arm9Bus};
use crate::ipc::Side;
use crate::machine::{Arm9, Stepper};
use crate::{Nds, graphics, io_names};

/// The ARM9's CPU number.
const ARM9: usize = 0;
/// The ARM7's CPU number.
const ARM7: usize = 1;

/// The CPU number `cpu` names; numbers past the ARM7 mean it too.
fn side(cpu: usize) -> Side {
    if cpu == ARM9 { Side::Arm9 } else { Side::Arm7 }
}

/// A [`Stepper`] that checks every instruction against a debugger's
/// stops and keeps the first that applies.
struct Checks<'a> {
    stops: &'a Stops,
    hit: Option<Stop>,
}

/// Records `access` by `cpu` in `hit` if a watchpoint catches it and
/// nothing stopped earlier.
fn watch(stops: &Stops, hit: &mut Option<Stop>, cpu: usize, access: DataAccess) {
    if hit.is_none() {
        *hit = stops.watch(cpu, access).map(|reason| Stop { cpu, reason });
    }
}

impl Stepper for Checks<'_> {
    fn arm9(&mut self, cpu: &mut Arm9, bus: &mut Arm9Bus<'_>) {
        let Self { stops, hit } = self;
        cpu.step(&mut Watched::new(bus, |address, bytes, write, value| {
            let access = DataAccess {
                address,
                bytes,
                write,
                value,
            };
            watch(stops, hit, ARM9, access);
        }));
    }

    fn arm7(&mut self, cpu: &mut emu_arm::Cpu, bus: &mut Arm7Bus<'_>) {
        let Self { stops, hit } = self;
        cpu.step(&mut Watched::new(bus, |address, bytes, write, value| {
            let access = DataAccess {
                address,
                bytes,
                write,
                value,
            };
            watch(stops, hit, ARM7, access);
        }));
    }

    fn stop(&mut self, side: Side, ran: bool, next: u32) -> bool {
        let cpu = match side {
            Side::Arm9 => ARM9,
            Side::Arm7 => ARM7,
        };
        if self.hit.is_none() && ran {
            self.hit = self
                .stops
                .after_instruction(cpu, next)
                .map(|reason| Stop { cpu, reason });
        }
        self.hit.is_some()
    }
}

/// An ARM CPU's registers as the debugger contract has them.
fn registers(named: Vec<(&'static str, u32)>) -> Vec<Register> {
    named
        .into_iter()
        .map(|(name, value)| Register {
            name,
            value,
            bits: 32,
        })
        .collect()
}

impl Debugger for Nds {
    fn cpus(&self) -> Vec<CpuInfo> {
        vec![
            CpuInfo {
                name: "ARM9",
                player: 0,
            },
            CpuInfo {
                name: "ARM7",
                player: 0,
            },
        ]
    }

    fn registers(&self, cpu: usize) -> Vec<Register> {
        registers(match side(cpu) {
            Side::Arm9 => self.machine.arm9.named_registers(),
            Side::Arm7 => self.machine.arm7.named_registers(),
        })
    }

    fn status(&self, cpu: usize) -> String {
        let machine = &self.machine;
        let (cpsr, halted) = match side(cpu) {
            Side::Arm9 => (
                machine.arm9.cpsr(),
                machine.hw.arm9.halted || machine.arm9.halted(),
            ),
            Side::Arm7 => (machine.arm7.cpsr(), machine.hw.arm7.halted),
        };
        let mut status = describe_psr(cpsr);
        if halted {
            status.push_str("  halted");
        }
        if machine.hle_bios {
            status.push_str("  HLE BIOS");
        }
        status
    }

    fn pc(&self, cpu: usize) -> u32 {
        match side(cpu) {
            Side::Arm9 => self.machine.arm9.next_instruction(),
            Side::Arm7 => self.machine.arm7.next_instruction(),
        }
    }

    fn read(&self, cpu: usize, address: u32) -> Option<u8> {
        let machine = &self.machine;
        match side(cpu) {
            Side::Arm9 => machine
                .arm9
                .tcm_read_8(address)
                .or_else(|| machine.hw.peek(true, address)),
            Side::Arm7 => machine.hw.peek(false, address),
        }
    }

    fn write(&mut self, cpu: usize, address: u32, value: u8) -> bool {
        let machine = &mut self.machine;
        match side(cpu) {
            Side::Arm9 => {
                machine.arm9.tcm_write_8(address, value) || machine.hw.poke(true, address, value)
            }
            Side::Arm7 => machine.hw.poke(false, address, value),
        }
    }

    fn disassemble(&self, cpu: usize, address: u32) -> Instruction {
        let (thumb, architecture) = match side(cpu) {
            Side::Arm9 => (self.machine.arm9.thumb(), Architecture::V5TE),
            Side::Arm7 => (self.machine.arm7.thumb(), Architecture::V4T),
        };
        let size = if thumb { 2 } else { 4 };
        let address = address & !(size - 1);
        let bytes: Vec<u8> = (0..4)
            .map(|offset| self.read(cpu, address.wrapping_add(offset)).unwrap_or(0))
            .collect();
        let decoded = if thumb {
            let opcode = u16::from_le_bytes([bytes[0], bytes[1]]);
            let next = u16::from_le_bytes([bytes[2], bytes[3]]);
            emu_arm::disassemble_thumb(address, opcode, next, architecture)
        } else {
            let opcode = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            emu_arm::disassemble_arm(address, opcode, architecture)
        };
        Instruction {
            address,
            bytes: bytes[..decoded.length as usize].to_vec(),
            text: decoded.text,
            call: decoded.call,
        }
    }

    fn io_registers(&self, cpu: usize) -> Vec<IoRegister> {
        let side = side(cpu);
        let table = match side {
            Side::Arm9 => io_names::ARM9_IO_REGISTERS,
            Side::Arm7 => io_names::ARM7_IO_REGISTERS,
        };
        // Straight to the I/O map: the ARM9's TCM could sit over it.
        debug::read_io_registers(table, |address| self.machine.hw.peek_io(side, address))
    }

    fn events(&self, _cpu: usize) -> Vec<ScheduledEvent> {
        let scheduler = &self.machine.hw.scheduler;
        let now = scheduler.now();
        scheduler
            .pending()
            .into_iter()
            .map(|due| ScheduledEvent {
                cycles: due.time as i64 - now as i64,
                name: format!("{:?}", due.event),
            })
            .collect()
    }

    fn graphics(&self, _cpu: usize, request: &GraphicsRequest) -> Option<Graphics> {
        graphics::draw(&self.machine.hw, request)
    }

    fn run_until(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> RunOutcome {
        let mut checks = Checks { stops, hit: None };
        let frame_done = self.run(inputs, audio, &mut checks);
        RunOutcome {
            frame_done,
            stop: checks.hit,
        }
    }
}

#[cfg(test)]
mod tests {
    use emu_core::debug::{Breakpoint, StopReason, WatchKind, Watchpoint};
    use emu_core::{Core, Media};

    use super::*;

    /// Where the ARM9's program runs.
    const ARM9_BASE: u32 = 0x0200_4000;
    /// Where the ARM7's program runs.
    const ARM7_BASE: u32 = 0x0238_0000;

    /// A cartridge whose CPUs run `arm9` and `arm7` (ARM words).
    fn nds(arm9: &[u32], arm7: &[u32]) -> Nds {
        let mut rom = vec![0; 0x400];
        let mut put = |offset: usize, value: u32| {
            rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        for (offset, value) in [(0x20, 0x200), (0x24, ARM9_BASE), (0x28, ARM9_BASE)] {
            put(offset, value);
        }
        for (offset, value) in [(0x2C, 0x100), (0x30, 0x300), (0x34, ARM7_BASE)] {
            put(offset, value);
        }
        put(0x38, ARM7_BASE);
        put(0x3C, 0x100);
        for (index, word) in arm9.iter().enumerate() {
            put(0x200 + index * 4, *word);
        }
        for (index, word) in arm7.iter().enumerate() {
            put(0x300 + index * 4, *word);
        }
        Nds::new(&Media::new(&rom)).expect("valid header")
    }

    /// MOV R0, #0x00800000 (DTCM); MOV R1, #5; STR R1, [R0]; ADD R1, R1,
    /// #1; B . - 4 (back to the ADD).
    const ARM9_PROGRAM: [u32; 5] = [
        0xE3A0_0502,
        0xE3A0_1005,
        0xE580_1000,
        0xE281_1001,
        0xEAFF_FFFD,
    ];
    /// MOV R0, #0x02000000; MOV R1, #7; STR R1, [R0, #4]; B .
    const ARM7_PROGRAM: [u32; 4] = [0xE3A0_0402, 0xE3A0_1007, 0xE580_1004, 0xEAFF_FFFE];

    fn stops_at(breakpoints: Vec<Breakpoint>, watchpoints: Vec<Watchpoint>) -> Stops {
        Stops {
            breakpoints,
            watchpoints,
            step: None,
        }
    }

    #[test]
    fn arm9_breakpoints_stop_mid_frame_and_resume() {
        let mut nds = nds(&ARM9_PROGRAM, &ARM7_PROGRAM);
        let at_add = Breakpoint {
            cpu: ARM9,
            address: ARM9_BASE + 12,
        };
        let stops = stops_at(vec![at_add], Vec::new());
        let outcome = nds.run_until(&[], &mut Vec::new(), &stops);
        let stop = Stop {
            cpu: ARM9,
            reason: StopReason::Breakpoint,
        };
        assert_eq!(
            outcome,
            RunOutcome {
                frame_done: false,
                stop: Some(stop),
            }
        );
        assert_eq!(nds.pc(ARM9), ARM9_BASE + 12);
        assert_eq!(nds.machine.arm9.reg(1), 5, "the ADD has not run");
        assert_eq!(nds.run_until(&[], &mut Vec::new(), &stops).stop, Some(stop));
        assert_eq!(nds.machine.arm9.reg(1), 6);
    }

    #[test]
    fn watchpoints_catch_the_arm7s_stores_and_the_arm9s_in_dtcm() {
        let mut nds = nds(&ARM9_PROGRAM, &ARM7_PROGRAM);
        let watch = |cpu, address| Watchpoint {
            cpu,
            start: address,
            end: address + 3,
            kind: WatchKind::Write,
        };
        let stops = stops_at(
            Vec::new(),
            vec![watch(ARM7, 0x0200_0004), watch(ARM9, 0x0080_0000)],
        );
        // The ARM9 runs first in each slice, so its DTCM store comes first.
        let dtcm = DataAccess {
            address: 0x0080_0000,
            bytes: 4,
            write: true,
            value: 5,
        };
        let stop = nds.run_until(&[], &mut Vec::new(), &stops).stop;
        assert_eq!(
            stop,
            Some(Stop {
                cpu: ARM9,
                reason: StopReason::Watchpoint(dtcm),
            })
        );
        let main_ram = DataAccess {
            address: 0x0200_0004,
            bytes: 4,
            write: true,
            value: 7,
        };
        let stop = nds.run_until(&[], &mut Vec::new(), &stops).stop;
        assert_eq!(
            stop,
            Some(Stop {
                cpu: ARM7,
                reason: StopReason::Watchpoint(main_ram),
            })
        );
        assert_eq!(nds.read(ARM9, 0x0080_0000), Some(5), "DTCM");
        assert_eq!(nds.read(ARM7, 0x0080_0000), None, "the ARM7 has no TCM");
    }

    #[test]
    fn stepping_the_arm7_leaves_the_emulation_as_it_would_have_been() {
        let mut plain = nds(&ARM9_PROGRAM, &ARM7_PROGRAM);
        let mut stepped = nds(&ARM9_PROGRAM, &ARM7_PROGRAM);
        let step = Stops {
            step: Some(ARM7),
            ..Stops::default()
        };
        for expected in [ARM7_BASE + 4, ARM7_BASE + 8] {
            let stop = stepped.run_until(&[], &mut Vec::new(), &step).stop;
            assert_eq!(stop.map(|stop| stop.reason), Some(StopReason::Step));
            assert_eq!(stepped.pc(ARM7), expected);
        }
        assert!(stepped.machine.arm9.reg(1) > 5, "the ARM9 ran ahead");
        stepped.run_frame(&[], &mut Vec::new());
        plain.run_frame(&[], &mut Vec::new());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        plain.save_state(&mut a);
        stepped.save_state(&mut b);
        assert!(a == b, "stepping changed the emulation");
    }

    #[test]
    fn debugger_memory_access_has_no_side_effects() {
        let mut nds = nds(&ARM9_PROGRAM, &ARM7_PROGRAM);
        assert!(nds.write(ARM9, 0x0200_1000, 0xAB));
        assert_eq!(nds.read(ARM7, 0x0240_1000), Some(0xAB), "a main RAM mirror");
        // A byte into engine A's palette, which CPU byte stores skip.
        assert!(nds.write(ARM9, 0x0500_0003, 0x7C));
        assert_eq!(nds.read(ARM9, 0x0500_0003), Some(0x7C));
        assert_eq!(nds.read(ARM9, 0x0500_0002), Some(0x00));
        assert!(nds.write(ARM9, 0x0080_0010, 0x55), "DTCM");
        assert!(!nds.write(ARM9, 0x0400_0000, 0), "I/O");
        assert!(!nds.write(ARM9, 0xFFFF_0000, 0), "BIOS");
        // POWCNT1 reads back; the IPC FIFO's receive port would pop a word.
        assert!(nds.read(ARM9, 0x0400_0304).is_some());
        assert_eq!(nds.read(ARM9, 0x0410_0000), None);
        let io = nds.io_registers(ARM7);
        assert!(io.iter().any(|register| register.name == "EXTKEYIN"));
        assert!(!nds.events(ARM9).is_empty());
        assert_eq!(nds.cpus().len(), 2);
        let instruction = nds.disassemble(ARM9, ARM9_BASE + 8);
        assert!(instruction.text.starts_with("STR"), "{}", instruction.text);
        assert!(nds.status(ARM7).contains("System"), "{}", nds.status(ARM7));
    }
}
