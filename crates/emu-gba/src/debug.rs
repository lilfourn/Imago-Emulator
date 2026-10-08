//! The Game Boy Advance's [`Debugger`]: the ARM7TDMI, the memory map,
//! the I/O registers, the scheduler and the PPU's memory.
//!
//! Watchpoints see the CPU's own loads and stores. They do not see DMA
//! transfers, sound FIFO refills, or memory the HLE BIOS reads and writes
//! for a system call.

use emu_arm::{Architecture, Watched, describe_psr};
use emu_core::debug::{
    self, CpuInfo, DataAccess, Graphics, GraphicsRequest, Instruction, IoRegister, Register,
    RunOutcome, ScheduledEvent, Stop, Stops,
};
use emu_core::{Debugger, Input, StereoSample};

use crate::{Gba, graphics, io_names};

impl Gba {
    /// One step, as [`Gba::step`] runs it, with the CPU's data accesses
    /// checked against `stops`'s watchpoints and the instruction it moves
    /// on to against the rest. `cpu` is this console's CPU number in
    /// `stops`.
    pub(crate) fn step_checked(&mut self, cpu: usize, stops: &Stops) -> Option<Stop> {
        let mut hit = None;
        let ran = self.step_with(|arm, bus| {
            arm.step(&mut Watched::new(bus, |address, bytes, write, value| {
                if hit.is_none() {
                    let access = DataAccess {
                        address,
                        bytes,
                        write,
                        value,
                    };
                    hit = stops.watch(cpu, access);
                }
            }));
        });
        let next = self.machine.cpu.next_instruction();
        let reason = hit.or_else(|| ran.then(|| stops.after_instruction(cpu, next)).flatten());
        reason.map(|reason| Stop { cpu, reason })
    }
}

/// An ARM CPU's registers as the debugger contract has them.
pub(crate) fn registers(named: Vec<(&'static str, u32)>) -> Vec<Register> {
    named
        .into_iter()
        .map(|(name, value)| Register {
            name,
            value,
            bits: 32,
        })
        .collect()
}

impl Debugger for Gba {
    fn cpus(&self) -> Vec<CpuInfo> {
        vec![CpuInfo {
            name: "ARM7TDMI",
            player: 0,
        }]
    }

    fn registers(&self, _cpu: usize) -> Vec<Register> {
        registers(self.machine.cpu.named_registers())
    }

    fn status(&self, _cpu: usize) -> String {
        let mut status = describe_psr(self.machine.cpu.cpsr());
        if self.machine.bus.halted() {
            status.push_str("  halted");
        }
        if self.hle_bios {
            status.push_str("  HLE BIOS");
        }
        status
    }

    fn pc(&self, _cpu: usize) -> u32 {
        self.machine.cpu.next_instruction()
    }

    fn read(&self, _cpu: usize, address: u32) -> Option<u8> {
        self.machine.bus.peek(address)
    }

    fn write(&mut self, _cpu: usize, address: u32, value: u8) -> bool {
        self.machine.bus.poke_memory(address, value)
    }

    fn disassemble(&self, cpu: usize, address: u32) -> Instruction {
        disassemble(
            |address| Debugger::read(self, cpu, address),
            self.machine.cpu.thumb(),
            Architecture::V4T,
            address,
        )
    }

    fn io_registers(&self, cpu: usize) -> Vec<IoRegister> {
        debug::read_io_registers(io_names::IO_REGISTERS, |address| {
            Debugger::read(self, cpu, address)
        })
    }

    fn events(&self, _cpu: usize) -> Vec<ScheduledEvent> {
        let scheduler = self.machine.bus.scheduler();
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
        graphics::draw(&self.machine.bus.ppu, request)
    }

    fn run_until(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> RunOutcome {
        self.run(inputs, audio, |gba| gba.step_checked(0, stops))
    }
}

/// The ARM (or, if `thumb`, Thumb) instruction at `address`, reading
/// memory through `read` (unreadable bytes read 0).
fn disassemble(
    read: impl Fn(u32) -> Option<u8>,
    thumb: bool,
    architecture: Architecture,
    address: u32,
) -> Instruction {
    let size = if thumb { 2 } else { 4 };
    let address = address & !(size - 1);
    let byte = |offset: u32| read(address.wrapping_add(offset)).unwrap_or(0);
    let bytes: Vec<u8> = (0..4).map(byte).collect();
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

#[cfg(test)]
mod tests {
    use emu_core::debug::{Breakpoint, StopReason, WatchKind, Watchpoint};
    use emu_core::{Core, Media};

    use super::*;

    /// A console running `program` (ARM) from 0x08000000.
    fn gba(program: &[u32]) -> Gba {
        let mut rom = vec![0; 0x200];
        for (index, word) in program.iter().enumerate() {
            rom[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        rom[0xB2] = 0x96;
        Gba::new(&Media::new(&rom)).expect("valid rom")
    }

    /// MOV R0, #0x03000000; MOV R1, #42; STR R1, [R0]; ADD R1, R1, #1;
    /// B . - 4 (back to the ADD).
    const PROGRAM: [u32; 5] = [
        0xE3A0_0403,
        0xE3A0_102A,
        0xE580_1000,
        0xE281_1001,
        0xEAFF_FFFD,
    ];

    #[test]
    fn breakpoints_stop_before_the_instruction() {
        let mut gba = gba(&PROGRAM);
        let stops = Stops {
            breakpoints: vec![Breakpoint {
                cpu: 0,
                address: 0x0800_000C,
            }],
            ..Stops::default()
        };
        let stop = Stop {
            cpu: 0,
            reason: StopReason::Breakpoint,
        };
        assert_eq!(gba.run_until(&[], &mut Vec::new(), &stops).stop, Some(stop));
        assert_eq!(gba.pc(0), 0x0800_000C);
        assert_eq!(gba.cpu().reg(1), 42, "the ADD has not run");
        assert_eq!(gba.run_until(&[], &mut Vec::new(), &stops).stop, Some(stop));
        assert_eq!(gba.cpu().reg(1), 43, "once round the loop");
    }

    #[test]
    fn watchpoints_stop_after_the_store_with_what_it_stored() {
        let mut gba = gba(&PROGRAM);
        let stops = Stops {
            watchpoints: vec![Watchpoint {
                cpu: 0,
                start: 0x0300_0000,
                end: 0x0300_0003,
                kind: WatchKind::Access,
            }],
            ..Stops::default()
        };
        let outcome = gba.run_until(&[], &mut Vec::new(), &stops);
        let access = DataAccess {
            address: 0x0300_0000,
            bytes: 4,
            write: true,
            value: 42,
        };
        assert_eq!(
            outcome.stop.map(|stop| stop.reason),
            Some(StopReason::Watchpoint(access))
        );
        assert_eq!(gba.pc(0), 0x0800_000C);
        // The loop touches no memory, so the frame finishes.
        let outcome = gba.run_until(&[], &mut Vec::new(), &stops);
        assert!(outcome.frame_done && outcome.stop.is_none());
    }

    #[test]
    fn steps_run_one_instruction() {
        let mut gba = gba(&PROGRAM);
        let step = Stops {
            step: Some(0),
            ..Stops::default()
        };
        for expected in [0x0800_0004, 0x0800_0008, 0x0800_000C] {
            let outcome = gba.run_until(&[], &mut Vec::new(), &step);
            assert_eq!(outcome.stop.map(|stop| stop.reason), Some(StopReason::Step));
            assert_eq!(gba.pc(0), expected);
        }
        let instruction = gba.disassemble(0, 0x0800_0008);
        assert_eq!(instruction.bytes, [0x00, 0x10, 0x80, 0xE5]);
        assert!(instruction.text.starts_with("STR"), "{}", instruction.text);
    }

    #[test]
    fn reads_see_everything_and_writes_only_memory() {
        let mut gba = gba(&PROGRAM);
        assert_eq!(Debugger::read(&gba, 0, 0x0800_0000), Some(0x03), "ROM");
        assert_eq!(Debugger::read(&gba, 0, 0x0400_0130), Some(0xFF), "KEYINPUT");
        assert!(Debugger::write(&mut gba, 0, 0x0600_0001, 0xAB));
        assert_eq!(Debugger::read(&gba, 0, 0x0600_0001), Some(0xAB));
        assert_eq!(
            Debugger::read(&gba, 0, 0x0600_0000),
            Some(0x00),
            "one byte only"
        );
        assert!(Debugger::write(&mut gba, 0, 0x0200_0010, 0x11));
        assert_eq!(gba.peek(0, 0x0200_0010), Some(0x11));
        assert!(!Debugger::write(&mut gba, 0, 0x0800_0000, 0));
        assert!(!Debugger::write(&mut gba, 0, 0x0400_0000, 0), "I/O");
        assert!(!gba.events(0).is_empty(), "the PPU's next line event");
        let io = gba.io_registers(0);
        assert!(io.iter().any(|register| register.name == "KEYINPUT"));
        assert_eq!(gba.registers(0)[15].name, "PC");
        assert!(gba.status(0).contains("System"), "{}", gba.status(0));
    }
}
