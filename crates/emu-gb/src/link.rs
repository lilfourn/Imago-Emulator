//! Two Game Boys joined by a link cable (Pan Docs, "Serial Data Transfer").
//!
//! Both machines run on one thread in alternating slices of 32 clock cycles
//! (8 M-cycles at normal speed). Between slices, a transfer started by one
//! machine's internal clock is delivered to the other, which answers with
//! its own byte if it is waiting on the external clock. A byte takes 1,024
//! M-cycles to send at the normal rate and 32 at the CGB's fast rate, so the
//! slice delay never shows. See `docs/decisions/0004-linked-cores.md`.

use emu_core::debug::{
    CpuInfo, Graphics, GraphicsRequest, Instruction, IoRegister, Register, RunOutcome, Stop, Stops,
};
use emu_core::{
    Core, CoreInfo, Debugger, Frame, Input, LoadError, Media, MemoryRegion, RomPatch, ScreenSize,
    StateError, StereoSample,
};

use crate::{CYCLES_PER_FRAME, Gb, INFO, Machine};

/// Clock cycles each machine runs before the cable is serviced.
const SLICE_CYCLES: u64 = 32;

const SCREENS: [ScreenSize; 2] = [ScreenSize::new(160, 144), ScreenSize::new(160, 144)];

/// Two Game Boys and the cable between them, as one two-player core.
/// Screen and player 0 are the first Game Boy.
#[derive(Debug)]
pub struct Linked {
    players: [Gb; 2],
    screens: [Frame; 2],
    /// Where a debugger stopped the last run mid-frame, so the next one
    /// carries on with the same slices.
    resume: Option<Resume>,
}

/// A frame's end and the current slice's end, in player 0's clock cycles.
#[derive(Debug, Clone, Copy)]
struct Resume {
    end: u64,
    target: u64,
}

impl Linked {
    /// Powers on two Game Boys, one per entry of `media`.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] if `media` does not hold exactly two games or
    /// either cartridge is rejected.
    pub fn new(media: &[Media<'_>]) -> Result<Self, LoadError> {
        let [first, second] = media else {
            return Err(LoadError::Unsupported(
                "a link cable joins exactly two games",
            ));
        };
        Ok(Self {
            players: [Gb::new(first)?, Gb::new(second)?],
            screens: [Frame::new(SCREENS[0]), Frame::new(SCREENS[1])],
            resume: None,
        })
    }

    /// Runs both machines through a frame's time, each instruction by
    /// `step` (given the machine and its player number), until the frame
    /// ends or a step returns a stop. After a stop the next call carries
    /// on where this one left off.
    fn run(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        mut step: impl FnMut(&mut Gb, usize) -> Option<Stop>,
    ) -> RunOutcome {
        for (index, gb) in self.players.iter_mut().enumerate() {
            gb.set_input(&inputs.get(index).copied().unwrap_or_default());
        }
        let Resume { end, mut target } = self.resume.take().unwrap_or_else(|| {
            let now = self.players[0].cycles();
            let end = now + CYCLES_PER_FRAME;
            Resume {
                end,
                target: (now + SLICE_CYCLES).min(end),
            }
        });
        loop {
            for index in 0..self.players.len() {
                while self.players[index].cycles() < target {
                    if let Some(stop) = step(&mut self.players[index], index) {
                        self.resume = Some(Resume { end, target });
                        return RunOutcome {
                            frame_done: false,
                            stop: Some(stop),
                        };
                    }
                }
            }
            self.service_cable();
            if self.players[0].cycles() >= end {
                break;
            }
            target = (self.players[0].cycles() + SLICE_CYCLES).min(end);
        }
        for (screen, gb) in self.screens.iter_mut().zip(&self.players) {
            screen.clone_from(&gb.screens[0]);
        }

        // Both machines produce the same number of samples; play them mixed.
        let mut first = Vec::new();
        let mut second = Vec::new();
        self.players[0].machine.bus.apu.drain_samples(&mut first);
        self.players[1].machine.bus.apu.drain_samples(&mut second);
        let mix = |a: i16, b: i16| ((i32::from(a) + i32::from(b)) / 2) as i16;
        audio.extend(
            first
                .iter()
                .zip(&second)
                .map(|(a, b)| [mix(a[0], b[0]), mix(a[1], b[1])]),
        );
        RunOutcome {
            frame_done: true,
            stop: None,
        }
    }

    /// The machine CPU `cpu` (also its player number) is in.
    fn player(&self, cpu: usize) -> &Gb {
        &self.players[cpu.min(1)]
    }

    /// Hands each started transfer to the other machine.
    fn service_cable(&mut self) {
        let [a, b] = &mut self.players;
        deliver(a, b);
        deliver(b, a);
    }
}

/// Delivers a transfer `from` clocks to `to`, and `to`'s answer back.
fn deliver(from: &mut Gb, to: &mut Gb) {
    if let Some((byte, remaining)) = from.machine.bus.serial.take_outgoing() {
        let reply = to.machine.bus.serial.receive(byte, remaining);
        from.machine.bus.serial.set_incoming(reply);
    }
}

impl Core for Linked {
    fn info(&self) -> CoreInfo {
        CoreInfo {
            screens: &SCREENS,
            players: 2,
            ..INFO
        }
    }

    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>) {
        self.run(inputs, audio, |gb, _| {
            gb.step();
            None
        });
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        self.resume = None;
        for gb in &mut self.players {
            gb.reset();
        }
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        let machines = (&self.players[0].machine, &self.players[1].machine);
        let bytes = postcard::to_stdvec(&machines).expect("machine state always serializes");
        out.extend_from_slice(&bytes);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let (mut first, mut second): (Machine, Machine) = postcard::from_bytes(state)
            .map_err(|_| StateError::Malformed("not a linked Game Boy state"))?;
        first.bus.take_media_from(&mut self.players[0].machine.bus);
        second.bus.take_media_from(&mut self.players[1].machine.bus);
        self.players[0].machine = first;
        self.players[1].machine = second;
        self.resume = None;
        for (screen, gb) in self.screens.iter_mut().zip(&mut self.players) {
            gb.present();
            screen.clone_from(&gb.screens[0]);
        }
        Ok(())
    }

    /// Each player has one cartridge: cartridge `i` is player `i`'s.
    fn backup_generation(&self, cartridge: usize) -> Option<u64> {
        self.players.get(cartridge)?.backup_generation(0)
    }

    fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
        if let Some(gb) = self.players.get(cartridge) {
            gb.save_backup(0, out);
        }
    }

    fn peek(&self, player: usize, address: u32) -> Option<u8> {
        // `Gb` has its own `peek` over the whole map; this is the RAM-only one.
        Core::peek(self.players.get(player)?, 0, address)
    }

    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool {
        self.players
            .get_mut(player)
            .is_some_and(|gb| Core::poke(gb, 0, address, value))
    }

    fn poke_bank(&mut self, player: usize, bank: u8, address: u32, value: u8) -> bool {
        self.players
            .get_mut(player)
            .is_some_and(|gb| gb.poke_bank(0, bank, address, value))
    }

    fn set_rom_patches(&mut self, player: usize, patches: &[RomPatch]) {
        if let Some(gb) = self.players.get_mut(player) {
            gb.set_rom_patches(0, patches);
        }
    }

    fn memory(&self, region: MemoryRegion) -> Option<&[u8]> {
        self.players[0].memory(region)
    }

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
    }
}

/// CPU `i` is player `i`'s; each is debugged as a lone Game Boy's.
impl Debugger for Linked {
    fn cpus(&self) -> Vec<CpuInfo> {
        (0..self.players.len())
            .map(|player| CpuInfo {
                name: "SM83",
                player,
            })
            .collect()
    }

    fn registers(&self, cpu: usize) -> Vec<Register> {
        Debugger::registers(self.player(cpu), 0)
    }

    fn status(&self, cpu: usize) -> String {
        self.player(cpu).status(0)
    }

    fn pc(&self, cpu: usize) -> u32 {
        self.player(cpu).pc(0)
    }

    fn read(&self, cpu: usize, address: u32) -> Option<u8> {
        Debugger::read(self.player(cpu), 0, address)
    }

    fn write(&mut self, cpu: usize, address: u32, value: u8) -> bool {
        Debugger::write(&mut self.players[cpu.min(1)], 0, address, value)
    }

    fn disassemble(&self, cpu: usize, address: u32) -> Instruction {
        self.player(cpu).disassemble(0, address)
    }

    fn io_registers(&self, cpu: usize) -> Vec<IoRegister> {
        self.player(cpu).io_registers(0)
    }

    fn graphics(&self, cpu: usize, request: &GraphicsRequest) -> Option<Graphics> {
        self.player(cpu).graphics(0, request)
    }

    fn run_until(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> RunOutcome {
        self.run(inputs, audio, |gb, cpu| gb.step_checked(cpu, stops).stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32 KiB ROM-only cartridge running `program` at 0x0100.
    fn rom(program: &[u8]) -> Vec<u8> {
        let mut rom = vec![0; 0x8000];
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        rom[0x14D] = rom[0x134..0x14D]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
        rom
    }

    /// Waits a while, then sends 0x42 on the internal clock and stores the
    /// byte received at 0xC000.
    const MASTER: [u8; 27] = [
        0x06, 0x00, // LD B, 0
        0x05, // DEC B
        0x20, 0xFD, // JR NZ, -3: wait for the other side to get ready
        0x3E, 0x42, // LD A, 0x42
        0xE0, 0x01, // LDH (SB), A
        0x3E, 0x81, // LD A, 0x81
        0xE0, 0x02, // LDH (SC), A: start, internal clock
        0xF0, 0x02, // LDH A, (SC)
        0xCB, 0x7F, // BIT 7, A
        0x20, 0xFA, // JR NZ, -6: until the transfer finishes
        0xF0, 0x01, // LDH A, (SB)
        0xEA, 0x00, 0xC0, // LD (0xC000), A
        0x18, 0xFE, // JR -2
        0x00,
    ];

    /// Waits on the external clock with 0x99 and stores the byte received
    /// at 0xC000.
    const SLAVE: [u8; 21] = [
        0x3E, 0x99, // LD A, 0x99
        0xE0, 0x01, // LDH (SB), A
        0x3E, 0x80, // LD A, 0x80
        0xE0, 0x02, // LDH (SC), A: start, external clock
        0xF0, 0x02, // LDH A, (SC)
        0xCB, 0x7F, // BIT 7, A
        0x20, 0xFA, // JR NZ, -6
        0xF0, 0x01, // LDH A, (SB)
        0xEA, 0x00, 0xC0, // LD (0xC000), A
        0x18, 0xFE, // JR -2
    ];

    #[test]
    fn a_byte_crosses_the_cable_both_ways() {
        let master = rom(&MASTER);
        let slave = rom(&SLAVE);
        let mut linked =
            Linked::new(&[Media::new(&master), Media::new(&slave)]).expect("valid roms");
        for _ in 0..3 {
            linked.run_frame(&[], &mut Vec::new());
        }
        assert_eq!(linked.players[0].peek(0xC000), 0x99);
        assert_eq!(linked.players[1].peek(0xC000), 0x42);
    }

    #[test]
    fn without_a_partner_the_master_reads_ff() {
        let master = rom(&MASTER);
        let mut gb = Gb::new(&Media::new(&master)).expect("valid rom");
        for _ in 0..3 {
            gb.run_frame(&[], &mut Vec::new());
        }
        assert_eq!(gb.peek(0xC000), 0xFF);
    }

    #[test]
    fn linked_states_round_trip() {
        let master = rom(&MASTER);
        let slave = rom(&SLAVE);
        let mut linked =
            Linked::new(&[Media::new(&master), Media::new(&slave)]).expect("valid roms");
        linked.run_frame(&[], &mut Vec::new());
        let mut state = Vec::new();
        linked.save_state(&mut state);
        let saved = linked.players[0].cycles();
        linked.run_frame(&[], &mut Vec::new());
        linked.load_state(&state).expect("own state");
        assert_eq!(linked.players[0].cycles(), saved);
        assert_eq!(linked.info().players, 2);
    }

    #[test]
    fn a_breakpoint_stops_one_player_and_the_frame_resumes_in_step() {
        use emu_core::debug::{Breakpoint, StopReason};

        let master = rom(&MASTER);
        let slave = rom(&SLAVE);
        let media = [Media::new(&master), Media::new(&slave)];
        let mut plain = Linked::new(&media).expect("valid roms");
        let mut debugged = Linked::new(&media).expect("valid roms");
        // Player 1's LDH A, (SC), polled until the transfer ends.
        let stops = Stops {
            breakpoints: vec![Breakpoint {
                cpu: 1,
                address: 0x0108,
            }],
            ..Stops::default()
        };
        let outcome = debugged.run_until(&[], &mut Vec::new(), &stops);
        assert_eq!(
            outcome.stop,
            Some(Stop {
                cpu: 1,
                reason: StopReason::Breakpoint
            })
        );
        assert_eq!(debugged.pc(1), 0x0108);
        // Without stops the rest of the frame runs on the same slices, so
        // both pairs end the frame in the same state.
        debugged.run_frame(&[], &mut Vec::new());
        plain.run_frame(&[], &mut Vec::new());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        plain.save_state(&mut a);
        debugged.save_state(&mut b);
        assert!(a == b, "the stop changed the emulation");
    }

    #[test]
    fn rom_patches_reach_one_player_and_outlast_linked_states() {
        let master = rom(&MASTER);
        let mut linked =
            Linked::new(&[Media::new(&master), Media::new(&master)]).expect("valid roms");
        let mut state = Vec::new();
        linked.save_state(&mut state);
        let patch = RomPatch {
            address: 0x0101,
            value: 0x07,
            compare: Some(0x00),
        };
        linked.set_rom_patches(1, &[patch]);
        linked.load_state(&state).expect("own state");
        assert_eq!(linked.players[0].peek(0x0101), 0x00);
        assert_eq!(linked.players[1].peek(0x0101), 0x07);
    }
}
