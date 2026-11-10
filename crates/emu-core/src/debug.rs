//! Optional debugging access to a core, for frontends' debuggers (PRD
//! UI-12).
//!
//! A core that can be debugged returns itself from
//! [`Core::debugger`](crate::Core::debugger). Everything crossing this
//! boundary is plain data: registers are numbers with names, instructions
//! are text, pictures of video memory are [`Frame`]s.
//!
//! # Stopping mid-frame
//!
//! A frontend calls [`Core::run_frame`](crate::Core::run_frame) while
//! nothing could stop the emulation, so breakpoints cost nothing until one
//! is set. While a breakpoint, watchpoint or step is pending it calls
//! [`Debugger::run_until`] instead, which runs the same frame but checks
//! the [`Stops`] after every instruction and can return in the middle of
//! it. Calling it again carries on from there.
//!
//! CPUs are numbered by their index in [`Debugger::cpus`]: one per
//! console, or the DS's ARM9 then ARM7, and in a linked pair player 0's
//! CPUs come before player 1's.

use crate::{Frame, Input, StereoSample};

pub mod draw;
pub mod gba_video;

/// A CPU a debugger can inspect and stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuInfo {
    /// What to call it, such as "SM83" or "ARM9".
    pub name: &'static str,
    /// The player whose console it is in.
    pub player: usize,
}

/// A register and its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Register {
    /// Its usual name, such as "A", "HL" or "R13".
    pub name: &'static str,
    /// Its value.
    pub value: u32,
    /// Its width in bits: 8, 16 or 32.
    pub bits: u8,
}

/// One instruction, decoded for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    /// Where it starts.
    pub address: u32,
    /// Its bytes, in memory order.
    pub bytes: Vec<u8>,
    /// Assembly text, such as `LD A, [HL]` or `MOV R0, #0x2A`.
    pub text: String,
    /// It calls a subroutine that returns to the next instruction (CALL,
    /// RST, BL, BLX), so stepping over it means running until then.
    pub call: bool,
}

/// An I/O register and its value, read without side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoRegister {
    /// Where the CPU finds it.
    pub address: u32,
    /// Its name in the hardware documentation, such as "LCDC" or "DISPCNT".
    pub name: &'static str,
    /// Its width in bits: 8, 16 or 32.
    pub bits: u8,
    /// Its value.
    pub value: u32,
}

/// Reads the I/O registers a core's `table` names, each `(address, name,
/// bits)`, as little-endian values from `read`; a register any of whose
/// bytes `read` cannot read is left out.
pub fn read_io_registers(
    table: &[(u32, &'static str, u8)],
    read: impl Fn(u32) -> Option<u8>,
) -> Vec<IoRegister> {
    table
        .iter()
        .filter_map(|&(address, name, bits)| {
            let bytes = u32::from(bits / 8);
            let value = (0..bytes).try_fold(0, |value, index| {
                let byte = read(address.wrapping_add(index))?;
                Some(value | u32::from(byte) << (8 * index))
            })?;
            Some(IoRegister {
                address,
                name,
                bits,
                value,
            })
        })
        .collect()
}

/// An event waiting in a core's scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledEvent {
    /// Cycles until it is due; negative if it is overdue.
    pub cycles: i64,
    /// What happens then, such as "HBlank" or "TimerOverflow(2)".
    pub name: String,
}

/// Stop when a CPU is about to run the instruction at an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Breakpoint {
    /// Index into [`Debugger::cpus`].
    pub cpu: usize,
    /// The instruction's address.
    pub address: u32,
}

/// Which data accesses a [`Watchpoint`] catches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WatchKind {
    /// Loads.
    Read,
    /// Stores.
    Write,
    /// Loads and stores.
    Access,
}

/// Stop after an instruction that reads or writes memory in a range.
///
/// Watchpoints see the data accesses a CPU makes itself, not its
/// instruction fetches. What else they see differs by core; each core's
/// [`Debugger`] documentation says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Watchpoint {
    /// Index into [`Debugger::cpus`].
    pub cpu: usize,
    /// First address watched.
    pub start: u32,
    /// Last address watched, inclusive.
    pub end: u32,
    /// Which accesses stop.
    pub kind: WatchKind,
}

impl Watchpoint {
    /// Whether an access by `cpu` to the `bytes` bytes from `address`
    /// stops here.
    #[must_use]
    pub fn catches(&self, cpu: usize, address: u32, bytes: u32, write: bool) -> bool {
        let kind = match self.kind {
            WatchKind::Read => !write,
            WatchKind::Write => write,
            WatchKind::Access => true,
        };
        let last = address.wrapping_add(bytes.max(1) - 1);
        kind && cpu == self.cpu && address <= self.end && last >= self.start
    }
}

/// Everything that can stop [`Debugger::run_until`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stops {
    /// Stop when a CPU reaches one of these.
    pub breakpoints: Vec<Breakpoint>,
    /// Stop after an instruction whose data access one of these catches.
    pub watchpoints: Vec<Watchpoint>,
    /// Stop once this CPU has run one instruction.
    pub step: Option<usize>,
}

impl Stops {
    /// Whether nothing would stop a run, so [`Core::run_frame`](crate::Core::run_frame)
    /// can run instead.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.breakpoints.is_empty() && self.watchpoints.is_empty() && self.step.is_none()
    }

    /// Why `cpu` stops after running an instruction, now that its next one
    /// is at `next`: a finished step, or a breakpoint there. Cores call it
    /// after each instruction, so a run that starts on a breakpoint runs
    /// that instruction instead of stopping again at once.
    #[must_use]
    pub fn after_instruction(&self, cpu: usize, next: u32) -> Option<StopReason> {
        if self.step == Some(cpu) {
            Some(StopReason::Step)
        } else if self
            .breakpoints
            .contains(&Breakpoint { cpu, address: next })
        {
            Some(StopReason::Breakpoint)
        } else {
            None
        }
    }

    /// Why a data access by `cpu` stops, if a watchpoint catches it.
    #[must_use]
    pub fn watch(&self, cpu: usize, access: DataAccess) -> Option<StopReason> {
        self.watchpoints
            .iter()
            .any(|watch| watch.catches(cpu, access.address, access.bytes, access.write))
            .then_some(StopReason::Watchpoint(access))
    }
}

/// A data access a CPU made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataAccess {
    /// Where, as the instruction computed it.
    pub address: u32,
    /// How many bytes: 1, 2 or 4.
    pub bytes: u32,
    /// A store rather than a load.
    pub write: bool,
    /// The value loaded or stored.
    pub value: u32,
}

/// Why a run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The CPU reached a breakpoint; it has not run that instruction yet.
    Breakpoint,
    /// The CPU ran the one instruction it was asked to.
    Step,
    /// The CPU's last instruction made this access, which a watchpoint
    /// caught.
    Watchpoint(DataAccess),
}

/// Where and why a run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stop {
    /// Index into [`Debugger::cpus`].
    pub cpu: usize,
    /// Why.
    pub reason: StopReason,
}

/// How far [`Debugger::run_until`] got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunOutcome {
    /// The frame finished: the screens show it, its audio is out, and the
    /// next call starts a new one.
    pub frame_done: bool,
    /// Where the run stopped, if it did. The instruction that finished
    /// the frame can also stop it.
    pub stop: Option<Stop>,
}

/// What a graphics viewer draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GraphicsView {
    /// Tile data, as a grid of tiles.
    #[default]
    Tiles,
    /// A background layer's whole map, as the game set it up.
    Map,
    /// Every sprite in OAM, one per cell, with their attributes.
    Sprites,
    /// Palette RAM, one swatch per colour.
    Palettes,
}

/// A graphics viewer's choices. Each field says which views and systems
/// use it; the others ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct GraphicsRequest {
    /// What to draw.
    pub view: GraphicsView,
    /// DS: 0 for engine A, 1 for engine B.
    pub engine: u8,
    /// Map: the background layer, 0–3 (Game Boy: 0 for the map at 0x9800,
    /// 1 for the one at 0x9C00).
    pub layer: u8,
    /// Tiles (GBA, DS): where the tiles start, as the CPU (the DS's ARM9)
    /// addresses video memory. The Game Boy always shows both VRAM banks.
    pub base: u32,
    /// Tiles (GBA, DS): 8 bits per pixel rather than 4.
    pub eight_bit: bool,
    /// Tiles: which palette colours them (Game Boy: 0–2 for BGP, OBP0 and
    /// OBP1, or 0–15 for the Game Boy Color's background then object
    /// palettes; GBA and DS: a 16-colour palette, 0–15 for backgrounds or
    /// 16–31 for objects).
    pub palette: u8,
}

/// A picture of video memory, with notes about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graphics {
    /// The picture.
    pub image: Frame,
    /// Lines of text: what the picture shows, and per-entry details such as
    /// each sprite's attributes.
    pub notes: Vec<String>,
}

/// Debugging access to a core. See the [module documentation](self).
pub trait Debugger {
    /// The CPUs, in the order the other methods number them.
    fn cpus(&self) -> Vec<CpuInfo>;

    /// `cpu`'s registers, in the order a debugger shows them.
    fn registers(&self, cpu: usize) -> Vec<Register>;

    /// `cpu`'s flags, mode and whether it is halted, as one line.
    fn status(&self, cpu: usize) -> String;

    /// The address of the next instruction `cpu` runs.
    fn pc(&self, cpu: usize) -> u32;

    /// The byte at `address` as `cpu` sees it, without side effects:
    /// memory and ROM, and I/O registers whose reads change nothing. `None`
    /// where reading would have side effects, or nothing answers.
    fn read(&self, cpu: usize, address: u32) -> Option<u8>;

    /// Writes the byte at `address` as `cpu` sees it, without side effects
    /// or time passing. Returns whether it was memory that could take it:
    /// RAM, video memory, palettes and OAM; never ROM or I/O registers.
    fn write(&mut self, cpu: usize, address: u32, value: u8) -> bool;

    /// The instruction at `address`, decoded in `cpu`'s current state (ARM
    /// or Thumb). Addresses round down to the state's instruction size.
    fn disassemble(&self, cpu: usize, address: u32) -> Instruction;

    /// `cpu`'s I/O registers, read without side effects.
    fn io_registers(&self, cpu: usize) -> Vec<IoRegister> {
        let _ = cpu;
        Vec::new()
    }

    /// The events waiting in the scheduler of `cpu`'s console, soonest
    /// first. Empty for cores without a scheduler.
    fn events(&self, cpu: usize) -> Vec<ScheduledEvent> {
        let _ = cpu;
        Vec::new()
    }

    /// A picture of the video memory of `cpu`'s console, or `None` if the
    /// core cannot draw that view.
    fn graphics(&self, cpu: usize, request: &GraphicsRequest) -> Option<Graphics> {
        let _ = (cpu, request);
        None
    }

    /// Runs the rest of the frame as [`Core::run_frame`](crate::Core::run_frame)
    /// does, but checks `stops` after every instruction and returns at the
    /// first that applies. Until the frame finishes, the screens show the
    /// last finished one, and the next call (or `run_frame`) carries on
    /// from where this one stopped.
    fn run_until(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> RunOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchpoints_catch_overlapping_accesses_of_their_kind() {
        let watch = Watchpoint {
            cpu: 0,
            start: 0x100,
            end: 0x103,
            kind: WatchKind::Write,
        };
        assert!(watch.catches(0, 0x0FE, 4, true), "a word over the start");
        assert!(watch.catches(0, 0x103, 1, true));
        assert!(!watch.catches(0, 0x104, 1, true));
        assert!(!watch.catches(0, 0x0FC, 4, true));
        assert!(!watch.catches(0, 0x100, 1, false), "a load");
        assert!(!watch.catches(1, 0x100, 1, true), "another CPU");
    }

    #[test]
    fn io_registers_read_little_endian_and_skip_unreadable_ones() {
        let table = [(0x10, "WORD", 32), (0x20, "HALF", 16), (0x30, "GONE", 8)];
        let read = |address: u32| (address < 0x30).then_some(address as u8);
        let registers = read_io_registers(&table, read);
        let values: Vec<(&str, u32)> = registers.iter().map(|r| (r.name, r.value)).collect();
        assert_eq!(values, [("WORD", 0x1312_1110), ("HALF", 0x2120)]);
    }

    #[test]
    fn steps_stop_before_breakpoints_are_checked() {
        let mut stops = Stops {
            breakpoints: vec![Breakpoint {
                cpu: 1,
                address: 0x0200_0000,
            }],
            ..Stops::default()
        };
        assert!(!stops.is_empty());
        assert_eq!(
            stops.after_instruction(1, 0x0200_0000),
            Some(StopReason::Breakpoint)
        );
        assert_eq!(stops.after_instruction(0, 0x0200_0000), None);
        stops.step = Some(0);
        assert_eq!(stops.after_instruction(0, 0x1234), Some(StopReason::Step));
    }
}
