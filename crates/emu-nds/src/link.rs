//! Two Nintendo DSes within wireless range of each other, for local
//! multiplayer: trades, battles and DS Download Play (GBATEK, "DS Wireless
//! Communications"). See `docs/decisions/0007-ds-local-wireless.md`.
//!
//! Both consoles run on one thread in alternating slices of bus time.
//! Their clocks started together and run in step, so a frame one sends at
//! a bus cycle reaches the other at that same cycle: after each console's
//! slice the air carries whatever it sent to the other, which takes it in
//! at the frame's own start time if it has not got there yet. With
//! [`SLICE_CYCLES`] well under a preamble's 96 µs, that is nearly always
//! so; the second console's frames can reach the first a slice late, which
//! moves only when the receiver starts writing the frame, not when it
//! finishes. While neither radio has power, each console runs a whole
//! frame at a time instead.

use emu_core::debug::{
    CpuInfo, Graphics, GraphicsRequest, Instruction, IoRegister, Register, RunOutcome,
    ScheduledEvent, Stops,
};
use emu_core::{
    Core, CoreInfo, Debugger, Frame, Input, LoadError, Media, MemoryRegion, ScreenSize, StateError,
    StereoSample,
};

use crate::debug::Checks;
use crate::display::FRAME_CYCLES;
use crate::machine::{Machine, Plain, Stepper};
use crate::{INFO, Nds, SCREEN};

/// Bus cycles each console runs before the other catches up while either
/// radio has power: about 15 µs.
const SLICE_CYCLES: u64 = 512;

/// Two consoles' screens: the first's top and bottom, then the second's.
const SCREENS: [ScreenSize; 4] = [SCREEN; 4];

/// Two DSes in wireless range, as one two-player core.
#[derive(Debug)]
pub struct Linked {
    players: [Nds; 2],
    screens: [Frame; 4],
    /// Where a debugger stopped the last run mid-frame, so the next one
    /// carries on with the same slices.
    resume: Option<Resume>,
}

/// The current slice's end, the frame's start, and which consoles have
/// finished the frame.
#[derive(Debug, Clone, Copy)]
struct Resume {
    target: u64,
    start: u64,
    done: [bool; 2],
}

impl Linked {
    /// Powers on two DSes, one per entry of `media`. If both firmwares
    /// give the same MAC address (as two synthesised ones do), the second
    /// console's is changed so the two can tell each other apart.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] if `media` does not hold exactly two games or
    /// either cartridge is rejected.
    pub fn new(media: &[Media<'_>]) -> Result<Self, LoadError> {
        let [first, second] = media else {
            return Err(LoadError::Unsupported(
                "local wireless joins exactly two DSes",
            ));
        };
        let mut players = [Nds::new(first)?, Nds::new(second)?];
        let mac = players[0].machine.hw.spi.mac_address();
        let second_spi = &mut players[1].machine.hw.spi;
        if second_spi.mac_address() == mac {
            let mut other = mac;
            other[5] = other[5].wrapping_add(1);
            second_spi.set_mac_address(other);
        }
        let mut linked = Self {
            players,
            screens: std::array::from_fn(|_| Frame::new(SCREEN)),
            resume: None,
        };
        linked.present();
        Ok(linked)
    }

    fn present(&mut self) {
        for (pair, nds) in self.screens.chunks_exact_mut(2).zip(&self.players) {
            for (screen, own) in pair.iter_mut().zip(&nds.screens) {
                screen.clone_from(own);
            }
        }
    }

    /// Hands the frames console `from` sent to the other.
    fn carry(&mut self, from: usize) {
        let frames = self.players[from].machine.hw.wifi.take_transmissions();
        let to = &mut self.players[1 - from];
        if frames.is_empty() || to.machine.hw.spi.powered_off() {
            return;
        }
        for frame in frames {
            to.machine.hw.wifi.receive(frame);
        }
        to.machine.hw.sync_wifi();
    }

    /// Whether either console's radio has power, so frames may fly.
    fn radios_on(&self) -> bool {
        self.players.iter().any(|nds| nds.machine.hw.wifi.powered())
    }

    /// Runs both consoles to the end of a frame (VBlank, which they reach
    /// together), each instruction by its console's stepper, until the
    /// frame ends or a stepper stops it. Returns whether the frame ended;
    /// after a stop the next call carries on where this one left off.
    fn run<S: Stepper>(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        steppers: &mut [S; 2],
    ) -> bool {
        for (index, nds) in self.players.iter_mut().enumerate() {
            nds.set_input(&inputs.get(index).copied().unwrap_or_default());
        }
        let Resume {
            mut target,
            start,
            mut done,
        } = self.resume.take().unwrap_or_else(|| {
            let start = self.players[0].machine.now();
            Resume {
                target: start,
                start,
                done: self
                    .players
                    .each_ref()
                    .map(|nds| nds.machine.hw.spi.powered_off()),
            }
        });
        while !done.iter().all(|&done| done) {
            let caught_up = self
                .players
                .iter()
                .zip(done)
                .all(|(nds, done)| done || nds.machine.now() >= target);
            if caught_up {
                let step = if self.radios_on() {
                    SLICE_CYCLES
                } else {
                    FRAME_CYCLES
                };
                target = (target + step).min(start + FRAME_CYCLES);
            }
            for index in 0..2 {
                let machine = &mut self.players[index].machine;
                while !done[index] && machine.now() < target {
                    let progress = machine.run_slice_with(&mut steppers[index], |_, _| {});
                    if progress.stopped {
                        self.resume = Some(Resume {
                            target,
                            start,
                            done,
                        });
                        return false;
                    }
                    // A frame ends at VBlank, or after a frame's time if
                    // that never comes.
                    done[index] |= progress.frame_done || machine.now() - start >= FRAME_CYCLES;
                }
                self.carry(index);
            }
        }
        self.finish_frame(audio);
        true
    }

    /// Shows each console's frame and plays their sound mixed.
    fn finish_frame(&mut self, audio: &mut Vec<StereoSample>) {
        let mut sounds: [Vec<StereoSample>; 2] = [Vec::new(), Vec::new()];
        for (nds, sound) in self.players.iter_mut().zip(&mut sounds) {
            if nds.machine.hw.spi.powered_off() {
                for screen in &mut nds.screens {
                    screen.pixels_mut().fill(0xFF00_0000);
                }
            } else {
                nds.machine.hw.sound.drain_samples(sound);
                nds.present();
            }
        }
        self.present();
        let [first, second] = &sounds;
        let mix = |a: i16, b: i16| ((i32::from(a) + i32::from(b)) / 2) as i16;
        audio.extend((0..first.len().max(second.len())).map(|index| {
            let a = first.get(index).copied().unwrap_or_default();
            let b = second.get(index).copied().unwrap_or_default();
            [mix(a[0], b[0]), mix(a[1], b[1])]
        }));
    }

    /// The console CPU `cpu` belongs to: 0 and 1 are the first console's
    /// ARM9 and ARM7, 2 and 3 the second's.
    fn player(&self, cpu: usize) -> &Nds {
        &self.players[(cpu / 2).min(1)]
    }

    /// Player `player`'s console (0 or 1), for debugging tools.
    #[must_use]
    pub fn console(&self, player: usize) -> &Nds {
        &self.players[player.min(1)]
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
        self.run(inputs, audio, &mut [Plain, Plain]);
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn set_render_scale(&mut self, scale: u32) {
        for nds in &mut self.players {
            nds.set_render_scale(scale);
        }
        self.present();
    }

    fn reset(&mut self) {
        self.resume = None;
        for nds in &mut self.players {
            nds.reset();
        }
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        let machines = (&self.players[0].machine, &self.players[1].machine);
        let bytes = postcard::to_stdvec(&machines).expect("machine state always serializes");
        out.extend_from_slice(&bytes);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let (first, second): (Machine, Machine) = postcard::from_bytes(state)
            .map_err(|_| StateError::Malformed("not a linked Nintendo DS state"))?;
        self.players[0].check_state(&first)?;
        self.players[1].check_state(&second)?;
        for (nds, machine) in self.players.iter_mut().zip([first, second]) {
            nds.install(machine);
        }
        self.resume = None;
        self.present();
        Ok(())
    }

    /// Each player has one game card: cartridge `i` is player `i`'s.
    fn backup_generation(&self, cartridge: usize) -> Option<u64> {
        self.players.get(cartridge)?.backup_generation(0)
    }

    fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
        if let Some(nds) = self.players.get(cartridge) {
            nds.save_backup(0, out);
        }
    }

    fn peek(&self, player: usize, address: u32) -> Option<u8> {
        self.players.get(player)?.peek(0, address)
    }

    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool {
        self.players
            .get_mut(player)
            .is_some_and(|nds| nds.poke(0, address, value))
    }

    fn memory(&self, region: MemoryRegion) -> Option<&[u8]> {
        self.players[0].memory(region)
    }

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
    }
}

/// CPUs 0 and 1 are player 0's ARM9 and ARM7, 2 and 3 player 1's; each is
/// debugged as a lone console's.
impl Debugger for Linked {
    fn cpus(&self) -> Vec<CpuInfo> {
        let mut cpus = Vec::new();
        for (player, nds) in self.players.iter().enumerate() {
            for cpu in nds.cpus() {
                cpus.push(CpuInfo { player, ..cpu });
            }
        }
        cpus
    }

    fn registers(&self, cpu: usize) -> Vec<Register> {
        self.player(cpu).registers(cpu % 2)
    }

    fn status(&self, cpu: usize) -> String {
        self.player(cpu).status(cpu % 2)
    }

    fn pc(&self, cpu: usize) -> u32 {
        self.player(cpu).pc(cpu % 2)
    }

    fn read(&self, cpu: usize, address: u32) -> Option<u8> {
        self.player(cpu).read(cpu % 2, address)
    }

    fn write(&mut self, cpu: usize, address: u32, value: u8) -> bool {
        Debugger::write(&mut self.players[(cpu / 2).min(1)], cpu % 2, address, value)
    }

    fn disassemble(&self, cpu: usize, address: u32) -> Instruction {
        self.player(cpu).disassemble(cpu % 2, address)
    }

    fn io_registers(&self, cpu: usize) -> Vec<IoRegister> {
        self.player(cpu).io_registers(cpu % 2)
    }

    fn events(&self, cpu: usize) -> Vec<ScheduledEvent> {
        self.player(cpu).events(cpu % 2)
    }

    fn graphics(&self, cpu: usize, request: &GraphicsRequest) -> Option<Graphics> {
        self.player(cpu).graphics(cpu % 2, request)
    }

    fn run_until(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> RunOutcome {
        let mut checks = [Checks::new(stops, 0), Checks::new(stops, 2)];
        let frame_done = self.run(inputs, audio, &mut checks);
        RunOutcome {
            frame_done,
            stop: checks.iter().find_map(Checks::hit),
        }
    }
}

#[cfg(test)]
mod tests;
