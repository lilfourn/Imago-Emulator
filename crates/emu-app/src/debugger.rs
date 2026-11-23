//! The debugger's model: what the debugger window asks the emulation
//! thread to show and do, and the snapshot the thread sends back (UI-12).
//!
//! Everything here is plain data or a plain function over a core's
//! [`Debugger`]; the window that draws it lives in `debugger_window.rs`.
//! The thread builds a [`Snapshot`] after every frame while the window is
//! open, and after every command while paused.

use emu_core::Debugger;
use emu_core::debug::{
    Breakpoint, CpuInfo, Graphics, GraphicsRequest, Instruction, IoRegister, Register,
    ScheduledEvent, Stop, Stops, Watchpoint,
};

/// Instructions in the disassembly.
pub const CODE_LINES: usize = 24;
/// Of those, how many lead up to the PC when following it.
const LINES_BEFORE_PC: usize = 6;
/// Bytes on each row of the memory editor.
pub const BYTES_PER_ROW: u32 = 16;
/// Rows of the memory editor.
pub const MEMORY_ROWS: u32 = 16;

/// The panel under the code, registers and memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    /// The breakpoint and watchpoint lists.
    #[default]
    Breakpoints,
    /// Tiles, maps, sprites and palettes.
    Graphics,
    /// The I/O registers.
    Io,
    /// The scheduler's queue.
    Events,
}

/// What the debugger window shows. The thread gathers only this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct View {
    /// Index into the core's CPUs.
    pub cpu: usize,
    /// Where the disassembly starts, or `None` to follow the PC.
    pub code: Option<u32>,
    /// The memory editor's first address, a multiple of
    /// [`BYTES_PER_ROW`].
    pub memory: u32,
    /// The panel showing below.
    pub panel: Panel,
    /// What the graphics panel draws.
    pub graphics: GraphicsRequest,
}

/// A debugger request to the emulation thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Shows this in the debugger window after every frame, or stops when
    /// `None` (the window closed).
    View(Option<View>),
    /// Replaces the breakpoints and watchpoints.
    SetStops {
        /// Stop before these instructions.
        breakpoints: Vec<Breakpoint>,
        /// Stop after accesses these catch.
        watchpoints: Vec<Watchpoint>,
    },
    /// Pauses now (at the end of the frame, if running).
    Pause,
    /// Runs on until something stops it.
    Continue,
    /// Runs one instruction of this CPU, then pauses.
    Step(usize),
    /// Like [`Command::Step`], but runs a call through to its return.
    StepOver(usize),
    /// Runs to the end of the frame, then pauses.
    NextFrame,
    /// Writes one byte of this CPU's memory.
    Write {
        /// Index into the core's CPUs.
        cpu: usize,
        /// Where.
        address: u32,
        /// What.
        value: u8,
    },
}

/// Whether the emulation is running, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Running frames at the chosen speed.
    Running,
    /// Paused by the user.
    Paused,
    /// Stopped by a breakpoint, watchpoint or step.
    Stopped(Stop),
    /// Paused, but running towards a step or the end of a frame.
    Stepping,
}

/// Everything the debugger window shows, copied out of the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The core's CPUs.
    pub cpus: Vec<CpuInfo>,
    /// The CPU shown: the view's, if the core has it.
    pub cpu: usize,
    /// Its registers.
    pub registers: Vec<Register>,
    /// Its flags, mode and halt state.
    pub status: String,
    /// The address of its next instruction.
    pub pc: u32,
    /// The disassembly.
    pub code: Vec<Instruction>,
    /// The memory editor's first address.
    pub memory_start: u32,
    /// Its bytes; `None` where reading would have side effects.
    pub memory: Vec<Option<u8>>,
    /// The I/O registers, if that panel shows.
    pub io: Vec<IoRegister>,
    /// The scheduler's queue, if that panel shows.
    pub events: Vec<ScheduledEvent>,
    /// The graphics viewer's picture, if that panel shows.
    pub graphics: Option<Graphics>,
    /// Running, paused or stopped.
    pub state: RunState,
}

/// Copies what `view` asks for out of `debugger`.
pub fn snapshot(debugger: &dyn Debugger, view: &View, state: RunState) -> Snapshot {
    let cpus = debugger.cpus();
    let cpu = view.cpu.min(cpus.len().saturating_sub(1));
    let pc = debugger.pc(cpu);
    let code = match view.code {
        Some(start) => listing_from(debugger, cpu, start, CODE_LINES),
        None => listing(debugger, cpu, pc),
    };
    let memory = (0..BYTES_PER_ROW * MEMORY_ROWS)
        .map(|offset| debugger.read(cpu, view.memory.wrapping_add(offset)))
        .collect();
    let mut snapshot = Snapshot {
        cpus,
        cpu,
        registers: debugger.registers(cpu),
        status: debugger.status(cpu),
        pc,
        code,
        memory_start: view.memory,
        memory,
        io: Vec::new(),
        events: Vec::new(),
        graphics: None,
        state,
    };
    match view.panel {
        Panel::Breakpoints => {}
        Panel::Graphics => snapshot.graphics = debugger.graphics(cpu, &view.graphics),
        Panel::Io => snapshot.io = debugger.io_registers(cpu),
        Panel::Events => snapshot.events = debugger.events(cpu),
    }
    snapshot
}

/// `count` instructions in a row from `start`.
fn listing_from(debugger: &dyn Debugger, cpu: usize, start: u32, count: usize) -> Vec<Instruction> {
    let mut address = start;
    (0..count)
        .map(|_| {
            let instruction = debugger.disassemble(cpu, address);
            address = next_address(&instruction);
            instruction
        })
        .collect()
}

/// Where the instruction after `instruction` starts.
fn next_address(instruction: &Instruction) -> u32 {
    let length = u32::try_from(instruction.bytes.len()).unwrap_or(1).max(1);
    instruction.address.wrapping_add(length)
}

/// [`CODE_LINES`] instructions around `pc`: a few leading up to it, then
/// it and those after.
///
/// Code has no marks where instructions start, so the lines before are
/// found by decoding forward from a little way back: from the furthest
/// start (up to 4 bytes per line back) whose instructions land exactly on
/// `pc` in at most [`LINES_BEFORE_PC`] lines. With fixed-size ARM
/// instructions the first try works; with the Game Boy's 1 to 3 bytes it
/// is a good guess.
pub fn listing(debugger: &dyn Debugger, cpu: usize, pc: u32) -> Vec<Instruction> {
    let reach = |start: u32| -> Option<Vec<Instruction>> {
        let mut lines = Vec::new();
        let mut address = start;
        while address != pc {
            if lines.len() == LINES_BEFORE_PC {
                return None;
            }
            let instruction = debugger.disassemble(cpu, address);
            let next = next_address(&instruction);
            // Overshooting `pc` makes the distance to it wrap round.
            if pc.wrapping_sub(next) > pc.wrapping_sub(address) {
                return None;
            }
            address = next;
            lines.push(instruction);
        }
        Some(lines)
    };
    let back = 4 * LINES_BEFORE_PC as u32;
    let mut lines = (1..=back)
        .rev()
        .find_map(|distance| reach(pc.wrapping_sub(distance)))
        .unwrap_or_default();
    let after = CODE_LINES - lines.len();
    lines.extend(listing_from(debugger, cpu, pc, after));
    lines
}

/// What the emulation thread runs towards while paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// One instruction of this CPU.
    Step(usize),
    /// The instruction after a call, as a breakpoint.
    StepOver(Breakpoint),
    /// The end of the frame.
    NextFrame,
}

impl Pending {
    /// What stepping over `debugger`'s next instruction on `cpu` runs
    /// towards: past it if it is a call, otherwise just through it.
    pub fn step_over(debugger: &dyn Debugger, cpu: usize) -> Self {
        let instruction = debugger.disassemble(cpu, debugger.pc(cpu));
        if instruction.call {
            Self::StepOver(Breakpoint {
                cpu,
                address: next_address(&instruction),
            })
        } else {
            Self::Step(cpu)
        }
    }

    /// `stops` with what this runs towards added.
    pub fn add_to(self, stops: &mut Stops) {
        match self {
            Self::Step(cpu) => stops.step = Some(cpu),
            Self::StepOver(breakpoint) => stops.breakpoints.push(breakpoint),
            Self::NextFrame => {}
        }
    }
}

/// One line describing why a run stopped, for the window and notices.
pub fn describe_stop(stop: &Stop, cpus: &[CpuInfo], pc: u32) -> String {
    use emu_core::debug::StopReason;

    let name = cpus.get(stop.cpu).map_or("CPU", |cpu| cpu.name);
    match stop.reason {
        StopReason::Breakpoint => format!("{name} at breakpoint {pc:08X}"),
        StopReason::Step => format!("{name} stepped to {pc:08X}"),
        StopReason::Watchpoint(access) => format!(
            "{name} {} {:0width$X} at {:08X}, before {pc:08X}",
            if access.write { "wrote" } else { "read" },
            access.value,
            access.address,
            width = access.bytes as usize * 2,
        ),
    }
}

#[cfg(test)]
mod tests {
    use emu_core::debug::{RunOutcome, StopReason};
    use emu_core::{Input, StereoSample};

    use super::*;

    /// A debugger over 256 bytes of "code" in which byte `n` at an
    /// address starts an instruction `n` bytes long (0 counts as 1), and
    /// 0xCA starts a 3-byte call.
    struct Fake {
        code: [u8; 256],
        pc: u32,
    }

    impl Debugger for Fake {
        fn cpus(&self) -> Vec<CpuInfo> {
            vec![CpuInfo {
                name: "FAKE",
                player: 0,
            }]
        }
        fn registers(&self, _: usize) -> Vec<Register> {
            Vec::new()
        }
        fn status(&self, _: usize) -> String {
            String::new()
        }
        fn pc(&self, _: usize) -> u32 {
            self.pc
        }
        fn read(&self, _: usize, address: u32) -> Option<u8> {
            self.code.get(address as usize).copied()
        }
        fn write(&mut self, _: usize, _: u32, _: u8) -> bool {
            false
        }
        fn disassemble(&self, _: usize, address: u32) -> Instruction {
            let byte = self.code[address as usize % 256];
            let length = if byte == 0xCA {
                3
            } else {
                usize::from(byte.clamp(1, 4))
            };
            Instruction {
                address,
                bytes: vec![byte; length],
                text: format!("OP{length}"),
                call: byte == 0xCA,
            }
        }
        fn run_until(&mut self, _: &[Input], _: &mut Vec<StereoSample>, _: &Stops) -> RunOutcome {
            RunOutcome::default()
        }
        fn io_registers(&self, _: usize) -> Vec<IoRegister> {
            vec![IoRegister {
                address: 0x40,
                name: "FAKEIO",
                bits: 8,
                value: 0,
            }]
        }
        fn events(&self, _: usize) -> Vec<ScheduledEvent> {
            vec![ScheduledEvent {
                cycles: 1,
                name: "Tick".to_owned(),
            }]
        }
    }

    #[test]
    fn fixed_size_code_lists_whole_instructions_before_the_pc() {
        let fake = Fake {
            code: [4; 256],
            pc: 0x80,
        };
        let lines = listing(&fake, 0, 0x80);
        assert_eq!(lines.len(), CODE_LINES);
        assert_eq!(lines[0].address, 0x80 - 4 * LINES_BEFORE_PC as u32);
        assert_eq!(lines[LINES_BEFORE_PC].address, 0x80);
        assert!(
            lines
                .windows(2)
                .all(|pair| pair[1].address == pair[0].address + 4)
        );
    }

    #[test]
    fn variable_size_code_lands_on_the_pc() {
        let mut code = [1; 256];
        // 3-byte instructions from 0x40 to the PC at 0x52.
        for start in (0x40..0x52).step_by(3) {
            code[start] = 3;
        }
        let fake = Fake { code, pc: 0x52 };
        let lines = listing(&fake, 0, 0x52);
        let pc_line = lines.iter().position(|line| line.address == 0x52);
        assert_eq!(pc_line, Some(LINES_BEFORE_PC));
        assert!(
            lines[..LINES_BEFORE_PC]
                .iter()
                .all(|line| line.bytes.len() == 3)
        );
    }

    #[test]
    fn stepping_over_a_call_stops_after_it() {
        let mut code = [1; 256];
        code[0x10] = 0xCA;
        let fake = Fake { code, pc: 0x10 };
        let pending = Pending::step_over(&fake, 0);
        assert_eq!(
            pending,
            Pending::StepOver(Breakpoint {
                cpu: 0,
                address: 0x13
            })
        );
        let mut stops = Stops::default();
        pending.add_to(&mut stops);
        assert_eq!(stops.breakpoints.len(), 1);
        let plain = Fake { code, pc: 0x11 };
        assert_eq!(Pending::step_over(&plain, 0), Pending::Step(0));
    }

    #[test]
    fn snapshots_gather_only_the_panel_shown() {
        let fake = Fake {
            code: [2; 256],
            pc: 0x20,
        };
        let view = View {
            memory: 0xF8,
            panel: Panel::Io,
            ..View::default()
        };
        let snapshot = snapshot(&fake, &view, RunState::Paused);
        assert_eq!(snapshot.memory[0], Some(2));
        assert_eq!(snapshot.memory[8], None, "past the end");
        assert_eq!(snapshot.io.len(), 1);
        assert!(snapshot.events.is_empty(), "another panel's");
        assert_eq!(snapshot.code[LINES_BEFORE_PC].address, 0x20);
    }

    #[test]
    fn stops_read_as_one_line() {
        let cpus = [CpuInfo {
            name: "ARM9",
            player: 0,
        }];
        let watch = Stop {
            cpu: 0,
            reason: StopReason::Watchpoint(emu_core::debug::DataAccess {
                address: 0x0200_0000,
                bytes: 2,
                write: true,
                value: 0x1234,
            }),
        };
        assert_eq!(
            describe_stop(&watch, &cpus, 0x0200_4008),
            "ARM9 wrote 1234 at 02000000, before 02004008"
        );
    }
}
