//! The Game Boy's [`Debugger`]: one SM83, its whole memory map, the I/O
//! registers and the PPU's memory.
//!
//! Watchpoints see the CPU's own loads and stores, including the stack
//! pushes of an interrupt. They do not see OAM DMA or HDMA copies, which
//! the bus makes on its own.

use emu_core::debug::{
    self, CpuInfo, DataAccess, Graphics, GraphicsRequest, Instruction, IoRegister, Register,
    RunOutcome, Stop, StopReason, Stops,
};
use emu_core::{Debugger, Input, StereoSample};

use crate::bus::Bus;
use crate::cpu;
use crate::{Gb, Model, disasm, graphics, io_names};

impl Gb {
    /// One step, as [`Gb::step`] runs it, with the CPU's data accesses
    /// checked against `stops`'s watchpoints and the instruction it moves
    /// on to against the rest. `cpu` is this console's CPU number in
    /// `stops`. Returns whether the step finished a frame and why it
    /// stopped, if it did.
    pub(crate) fn step_checked(&mut self, cpu: usize, stops: &Stops) -> RunOutcome {
        let before = self.machine.cpu.regs.pc;
        let mut bus = Watched {
            bus: &mut self.machine.bus,
            stops,
            cpu,
            hit: None,
        };
        let opcode = self.machine.cpu.step(&mut bus);
        let hit = bus.hit;
        let frame_done = self.take_frame();
        let pc = self.machine.cpu.regs.pc;
        // An interrupt dispatch moves the PC without an opcode; a halted
        // CPU does neither.
        let moved = opcode.is_some() || pc != before;
        let reason = hit.or_else(|| {
            moved
                .then(|| stops.after_instruction(cpu, pc.into()))
                .flatten()
        });
        RunOutcome {
            frame_done,
            stop: reason.map(|reason| Stop { cpu, reason }),
        }
    }
}

/// The bus as the CPU sees it while a debugger watches: every data access
/// is checked against the watchpoints, and the first caught is kept.
struct Watched<'a> {
    bus: &'a mut Bus,
    stops: &'a Stops,
    cpu: usize,
    hit: Option<StopReason>,
}

impl Watched<'_> {
    fn check(&mut self, address: u16, write: bool, value: u8) {
        if self.hit.is_none() {
            let access = DataAccess {
                address: address.into(),
                bytes: 1,
                write,
                value: value.into(),
            };
            self.hit = self.stops.watch(self.cpu, access);
        }
    }
}

impl cpu::Bus for Watched<'_> {
    fn read(&mut self, address: u16) -> u8 {
        let value = self.bus.read(address);
        self.check(address, false, value);
        value
    }

    fn fetch(&mut self, address: u16) -> u8 {
        self.bus.fetch(address)
    }

    fn write(&mut self, address: u16, value: u8) {
        self.bus.write(address, value);
        self.check(address, true, value);
    }

    fn tick(&mut self) {
        self.bus.tick();
    }

    fn pending_interrupts(&self) -> u8 {
        self.bus.pending_interrupts()
    }

    fn acknowledge_interrupt(&mut self, mask: u8) {
        self.bus.acknowledge_interrupt(mask);
    }

    fn stop(&mut self) {
        self.bus.stop();
    }
}

/// `flags` (the F register) as letters, with `-` for each clear flag.
fn flag_letters(flags: u8) -> String {
    [(0x80, 'Z'), (0x40, 'N'), (0x20, 'H'), (0x10, 'C')]
        .iter()
        .map(|&(bit, letter)| if flags & bit != 0 { letter } else { '-' })
        .collect()
}

/// A 16-bit register.
fn pair(name: &'static str, value: u16) -> Register {
    Register {
        name,
        value: value.into(),
        bits: 16,
    }
}

impl Debugger for Gb {
    fn cpus(&self) -> Vec<CpuInfo> {
        vec![CpuInfo {
            name: "SM83",
            player: 0,
        }]
    }

    fn registers(&self, _cpu: usize) -> Vec<Register> {
        let regs = self.machine.cpu.regs;
        vec![
            pair("AF", regs.af()),
            pair("BC", regs.bc()),
            pair("DE", regs.de()),
            pair("HL", regs.hl()),
            pair("SP", regs.sp),
            pair("PC", regs.pc),
        ]
    }

    fn status(&self, _cpu: usize) -> String {
        let cpu = &self.machine.cpu;
        let mut status = format!(
            "{}  IME {}",
            flag_letters(cpu.regs.f),
            if cpu.ime() { "on" } else { "off" }
        );
        if cpu.halted() {
            status.push_str("  halted");
        }
        match self.model() {
            Model::Dmg => {}
            Model::Cgb => status.push_str("  CGB"),
            Model::CgbDmgMode => status.push_str("  CGB in DMG mode"),
        }
        status
    }

    fn pc(&self, _cpu: usize) -> u32 {
        self.machine.cpu.regs.pc.into()
    }

    fn read(&self, _cpu: usize, address: u32) -> Option<u8> {
        u16::try_from(address)
            .ok()
            .map(|address| self.machine.bus.peek(address))
    }

    fn write(&mut self, _cpu: usize, address: u32, value: u8) -> bool {
        u16::try_from(address).is_ok_and(|address| self.machine.bus.poke_memory(address, value))
    }

    fn disassemble(&self, _cpu: usize, address: u32) -> Instruction {
        let address = address as u16;
        let bytes = [0, 1, 2].map(|offset| self.machine.bus.peek(address.wrapping_add(offset)));
        let decoded = disasm::disassemble(address, bytes);
        Instruction {
            address: address.into(),
            bytes: bytes[..usize::from(decoded.length)].to_vec(),
            text: decoded.text,
            call: decoded.call,
        }
    }

    fn io_registers(&self, cpu: usize) -> Vec<IoRegister> {
        debug::read_io_registers(io_names::IO_REGISTERS, |address| {
            Debugger::read(self, cpu, address)
        })
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
        self.run(inputs, audio, |gb| gb.step_checked(0, stops))
    }
}

#[cfg(test)]
mod tests {
    use emu_core::debug::{Breakpoint, WatchKind, Watchpoint};
    use emu_core::{Core, Media};

    use super::*;

    /// A 32 KiB ROM-only cartridge running `program` at 0x0100.
    fn gb(program: &[u8]) -> Gb {
        let mut rom = vec![0; 0x8000];
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        Gb::new(&Media::new(&rom)).expect("valid rom")
    }

    /// LD A, 0x42; LD (0xC000), A; INC A; NOP; JR -4 (back to INC A).
    const PROGRAM: [u8; 9] = [0x3E, 0x42, 0xEA, 0x00, 0xC0, 0x3C, 0x00, 0x18, 0xFC];

    #[test]
    fn a_frame_with_the_lcd_off_ends_however_often_it_stops() {
        // XOR A; LDH (0x40), A turns the LCD off; JR -2 loops at 0x0103.
        let mut gb = gb(&[0xAF, 0xE0, 0x40, 0x18, 0xFE]);
        let stops = Stops {
            breakpoints: vec![Breakpoint {
                cpu: 0,
                address: 0x0103,
            }],
            ..Stops::default()
        };
        // Turning the LCD off ends the first frame; time the one after.
        while !gb.run_until(&[], &mut Vec::new(), &stops).frame_done {}
        let start = gb.cycles();
        let mut runs = 0;
        while !gb.run_until(&[], &mut Vec::new(), &stops).frame_done {
            runs += 1;
            assert!(runs < 100_000, "the frame never ended");
        }
        let elapsed = gb.cycles() - start;
        assert!(
            (crate::CYCLES_PER_FRAME..crate::CYCLES_PER_FRAME + 16).contains(&elapsed),
            "{elapsed}"
        );
    }

    #[test]
    fn breakpoints_stop_before_the_instruction_and_resume_past_it() {
        let mut gb = gb(&PROGRAM);
        let stops = Stops {
            breakpoints: vec![Breakpoint {
                cpu: 0,
                address: 0x0105,
            }],
            ..Stops::default()
        };
        let outcome = gb.run_until(&[], &mut Vec::new(), &stops);
        let stop = Stop {
            cpu: 0,
            reason: StopReason::Breakpoint,
        };
        assert_eq!(outcome.stop, Some(stop));
        assert!(!outcome.frame_done);
        assert_eq!(gb.pc(0), 0x0105);
        assert_eq!(gb.registers().a, 0x42, "INC A has not run");
        // Resuming runs INC A, NOP and JR, then stops at INC A again.
        assert_eq!(gb.run_until(&[], &mut Vec::new(), &stops).stop, Some(stop));
        assert_eq!(gb.registers().a, 0x43);
    }

    #[test]
    fn watchpoints_stop_after_the_store() {
        let mut gb = gb(&PROGRAM);
        let stops = Stops {
            watchpoints: vec![Watchpoint {
                cpu: 0,
                start: 0xC000,
                end: 0xC000,
                kind: WatchKind::Write,
            }],
            ..Stops::default()
        };
        let outcome = gb.run_until(&[], &mut Vec::new(), &stops);
        let access = DataAccess {
            address: 0xC000,
            bytes: 1,
            write: true,
            value: 0x42,
        };
        assert_eq!(
            outcome.stop.map(|stop| stop.reason),
            Some(StopReason::Watchpoint(access))
        );
        assert_eq!(gb.pc(0), 0x0105, "after the store");
        // Instruction fetches never trip a read watchpoint.
        let fetches = Stops {
            watchpoints: vec![Watchpoint {
                cpu: 0,
                start: 0x0100,
                end: 0x0200,
                kind: WatchKind::Read,
            }],
            ..Stops::default()
        };
        assert_eq!(gb.run_until(&[], &mut Vec::new(), &fetches).stop, None);
    }

    #[test]
    fn a_step_runs_one_instruction_and_frames_still_finish() {
        let mut gb = gb(&PROGRAM);
        let step = Stops {
            step: Some(0),
            ..Stops::default()
        };
        let outcome = gb.run_until(&[], &mut Vec::new(), &step);
        assert_eq!(outcome.stop.map(|stop| stop.reason), Some(StopReason::Step));
        assert_eq!(gb.pc(0), 0x0102);
        // With nothing to stop at, a run finishes the frame.
        let outcome = gb.run_until(&[], &mut Vec::new(), &Stops::default());
        assert_eq!(
            outcome,
            RunOutcome {
                frame_done: true,
                stop: None,
            }
        );
    }

    #[test]
    fn debugger_writes_reach_ram_and_vram_but_not_rom() {
        let mut gb = gb(&PROGRAM);
        assert!(Debugger::write(&mut gb, 0, 0xC123, 0x5A));
        assert!(Debugger::write(&mut gb, 0, 0x8000, 0x77));
        assert!(!Debugger::write(&mut gb, 0, 0x0100, 0x00));
        assert!(!Debugger::write(&mut gb, 0, 0xFF40, 0x00), "I/O");
        assert_eq!(Debugger::read(&gb, 0, 0xC123), Some(0x5A));
        assert_eq!(Debugger::read(&gb, 0, 0x8000), Some(0x77));
        assert_eq!(Debugger::read(&gb, 0, 0x0100), Some(0x3E), "ROM reads");
        assert_eq!(Debugger::read(&gb, 0, 0x1_0000), None);
        assert_eq!(
            gb.memory(emu_core::MemoryRegion::VideoRam).map(|v| v[0]),
            Some(0x77)
        );
    }

    #[test]
    fn disassembly_and_registers_describe_the_cpu() {
        let gb = gb(&PROGRAM);
        let instruction = gb.disassemble(0, 0x0102);
        assert_eq!(instruction.bytes, [0xEA, 0x00, 0xC0]);
        assert!(instruction.text.contains("$C000"), "{}", instruction.text);
        let registers = Debugger::registers(&gb, 0);
        assert_eq!(registers[5].name, "PC");
        assert_eq!(registers[5].value, 0x0100);
        assert!(gb.status(0).starts_with("Z-HC"), "{}", gb.status(0));
        let io = gb.io_registers(0);
        assert!(
            io.iter()
                .any(|register| register.name == "LCDC" && register.value == 0x91)
        );
    }
}
